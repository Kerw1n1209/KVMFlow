const assert = require('node:assert/strict');
const test = require('node:test');
const { client, tick, v2, mixedState, pairedV2 } = require('./client-helper');
const peer = { device_id: 'dev-peer', name: 'Studio Mac', online: false, pair_addr: '192.168.1.23:47820' };
const state = { state: 'idle', enabled: true, mixed: mixedState(), lan: { enabled: true, peers: [peer], self: { device_id: v2.local_device.device_id }, setup: {} } };
const click = (c, selector) => { const el = c.win.document.querySelector(selector); assert.ok(el, selector); assert.notEqual(el.disabled, true); el.click(); };
async function primary(c) { c.win.navigate('guide'); c.win.KVMFlowPairing.startPrimaryAdd(); c.win.KVMFlowPairing.pairWith(peer.device_id); await tick(); }
async function fail(c) { await c.subscriptions.get('lan.pair')({ phase: 'failed', device_id: peer.device_id, code: 'E_LAN_PAIR_TIMEOUT' }); await tick(); }

test('retry after a backend pairing failure redials the selected computer', async t => {
  const c = await client(t, { state }); await primary(c); await fail(c);
  click(c, '#pairing-body [data-pair-action="restart"]'); await tick();
  assert.equal(c.requests.filter(r => r.method === 'lan.pair.start').length, 2);
  assert.equal(c.requests.filter(r => r.method === 'lan.pair.start').at(-1).params.addr, peer.pair_addr);
});

test('retry after a rejected start request retires the stale session and can redial', async t => {
  let attempts = 0;
  const c = await client(t, { state, request: async method => { if (method === 'lan.pair.start' && ++attempts === 1) throw Object.assign(new Error('offline'), { code: 'E_LAN_IO' }); } });
  await primary(c); click(c, '#pairing-body [data-pair-action="restart"]'); await tick();
  assert.equal(c.requests.filter(r => r.method === 'lan.pair.start').length, 2);
  assert.ok(c.requests.some(r => r.method === 'lan.pair.cancel'));
});

for (const action of ['close', 'cancel']) test(`failed dialog ${action} returns to a usable discovery page`, async t => {
  const c = await client(t, { state }); await primary(c); await fail(c);
  // The failure dialog exposes Close, while the dialog's native cancel event is Escape.
  if (action === 'close') click(c, '#pairing-body [data-pair-action="close"]');
  else c.win.document.querySelector('#pairing-dialog').dispatchEvent(new c.win.Event('cancel', { cancelable: true }));
  await tick(); assert.equal(c.win.document.querySelector('#pairing-dialog').open, false);
  c.win.KVMFlowPairing.pairWith('dev-peer'); await tick();
  assert.equal(c.requests.filter(r => r.method === 'lan.pair.start').length, 2);
});


// These regressions now exercise the role-free flow instead of the removed role/manual pages.
const V = require('./pairing-v2-helper');
const count = (c, method) => c.requests.filter(r => r.method === method).length;

test('a failed waiting connection retries by reopening discovery and capture', async t => {
  const c = await V.setup(t, { state: V.state([]) }); await V.waiting(c);
  await V.emit(c, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer', initiator: false });
  await V.emit(c, 'lan.pair', { phase: 'failed', device_id: 'dev-peer', code: 'E_LAN_PAIR_TIMEOUT' });
  V.click(c, 'v2-retry'); await tick(); await c.fire(10000);
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'wait');
  assert.equal(count(c, 'lan.setup.secondary'), 2);
  assert.equal(count(c, 'wizard.begin'), 2);
});

test('continuing setup after a rejected attempt starts cleanly without role or manual input pages', async t => {
  let reject = true;
  const c = await V.setup(t, { request: async method => { if (method === 'lan.pair.start' && reject) throw new Error('offline'); } });
  await c.start(); assert.equal(c.win.KVMFlowPairing.activeMode(), 'error');
  c.win.navigate('settings'); c.win.navigate('guide'); reject = false; await c.start();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'connecting');
  assert.equal(count(c, 'lan.pair.start'), 2);
});

test('navigation and delayed state never restore removed role or manual steps', async t => {
  const c = await V.setup(t, { config: null, state: V.state([]) });
  c.win.navigate('settings'); c.win.navigate('guide');
  c.win.KVMFlowPairingStateHook({ lan: V.state([]).lan }); c.win.renderGuide();
  assert.equal(c.win.document.querySelectorAll('#steps .step').length, 3);
  assert.match(V.body(c), /开始配置/);
  assert.doesNotMatch(V.body(c), /主机|副机|手动填写|配对码/);
});

test('malformed post-pair config preserves the local model and exposes identity guidance', async t => {
  let config = structuredClone(v2);
  const c = await V.setup(t, { request: async method => method === 'config.get' ? config : undefined });
  config = structuredClone(config); config.switch_group.devices = config.switch_group.devices.filter(d => d.device_id !== config.local_device.device_id);
  await assert.rejects(c.win.KVMFlowPairingConfig.refresh(), { code: 'E_LAN_TOPOLOGY_MISSING_SELF' });
  const model = JSON.parse(c.win.localStorage.getItem('kvmflow-local-prototype-v2'));
  assert.ok(model.computers.some(computer => computer.local && computer.id === model.localId));
  await V.emit(c, 'lan.sync', { issues: [{ code: 'E_LAN_TOPOLOGY_MISSING_SELF' }] });
  assert.match(c.win.document.querySelector('#guide-content [role="alert"]').textContent, /重新添加/);
});

test('a delayed owner profile retries reads automatically and cannot enable Next without topology', async t => {
  const c = await V.setup(t); await c.start();
  await V.emit(c, 'lan.pair', { phase: 'committed', device_id: 'dev-peer', config_merged: false });
  await V.emit(c, 'lan.profile', V.profile(false));
  assert.equal(c.win.document.querySelector('[data-pair-action="v2-next"]').disabled, true);
  c.setConfig(pairedV2()); await V.emit(c, 'lan.profile', V.profile(false, 2));
  assert.equal(c.win.document.querySelector('[data-pair-action="v2-next"]').disabled, false);
  V.click(c, 'v2-next'); await tick();
  const confirm = c.requests.findIndex(r => r.method === 'config.set' && r.params.config.switch_group?.sync?.ports_confirmed);
  assert.ok(confirm >= 0 && c.requests.findIndex(r => r.method === 'lan.test.start') > confirm);
});

test('a failed topology save exposes E2 and retry returns to the saved confirmation', async t => {
  let reject = false;
  const c = await V.setup(t, { request: async method => { if (method === 'config.set' && reject) throw new Error('disk'); } });
  await V.ready(c); reject = true; V.click(c, 'v2-next'); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'error'); assert.equal(count(c, 'lan.test.start'), 0);
  reject = false; V.click(c, 'v2-retry'); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'confirm');
  V.click(c, 'v2-next'); await tick(); assert.equal(count(c, 'lan.test.start'), 1);
});

test('waiting computer with mutual trust but no topology cannot report pairing completion', async t => {
  const c = await V.setup(t, { state: V.state([]) }); await V.waiting(c);
  await V.emit(c, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer', initiator: false });
  await V.emit(c, 'lan.pair', { phase: 'committed', device_id: 'dev-peer', config_merged: false });
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'error');
  assert.doesNotMatch(V.body(c), /两台电脑已配对完成/);
  c.setConfig(pairedV2()); V.click(c, 'v2-retry'); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'done');
});

for (const leave of [false, true]) test(`confirmation save is single-flight and respects navigation: leave=${leave}`, async t => {
  let finish, block = false;
  const saving = new Promise(resolve => { finish = resolve; });
  const c = await V.setup(t, { request: async method => method === 'config.set' && block ? saving : undefined });
  await V.ready(c); block = true;
  const writes = count(c, 'config.set');
  const next = c.win.document.querySelector('[data-pair-action="v2-next"]'); next.click(); next.click(); await tick();
  assert.equal(count(c, 'config.set') - writes, 1); assert.equal(count(c, 'lan.test.start'), 0);
  if (leave) c.win.navigate('settings');
  finish({}); await tick(); await tick();
  assert.equal(count(c, 'lan.test.start'), leave ? 0 : 1);
  assert.equal(c.win.KVMFlowPairing.activeMode(), leave ? null : 'try');
});
