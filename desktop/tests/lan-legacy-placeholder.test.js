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
  click(c, '[data-pair-peer="dev-peer"]'); await tick();
  assert.equal(c.requests.filter(r => r.method === 'lan.pair.start').length, 2);
});

for (const action of ['wait-again', 'manual']) test(`failed secondary ${action} remains actionable`, async t => {
  const c = await client(t, { state }); c.win.navigate('guide'); c.win.KVMFlowPairing.startRoleChoice();
  click(c, '[data-pair-role="secondary"]'); click(c, '[data-pair-action="role-next"]'); await tick(); await fail(c);
  click(c, `#guide-content [data-pair-action="${action}"]`); await tick();
  if (action === 'wait-again') {
    assert.equal(c.win.KVMFlowPairing.activeMode(), 'secondary-wait');
    assert.equal(c.requests.filter(r => r.method === 'lan.setup.secondary').length, 2);
    assert.equal(c.requests.filter(r => r.method === 'lan.pair.window').at(-1).params.open, true);
  } else {
    assert.equal(c.win.KVMFlowPairing.activeMode(), null);
    assert.ok(c.win.document.querySelector('#target-name'));
  }
});

test('continuing initialization after a rejected pairing attempt clears the blocked attempt', async t => {
  let reject = true;
  const c = await client(t, { state, request: async method => { if (method === 'lan.pair.start' && reject) throw Object.assign(new Error('offline'), { code: 'E_LAN_IO' }); } });
  await primary(c); click(c, '#pairing-body [data-pair-action="close"]'); await tick();
  c.win.KVMFlowPairing.startRoleChoice(); click(c, '[data-pair-role="primary"]'); click(c, '[data-pair-action="role-next"]'); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(), null);
  reject = false; c.win.KVMFlowPairing.startPrimaryAdd(); click(c, '[data-pair-peer="dev-peer"]'); await tick();
  assert.equal(c.requests.filter(r => r.method === 'lan.pair.start').length, 2);
});

test('welcome never reveals primary steps while navigation and delayed state race', async t => {
  const c = await client(t, { config: null, state: { ...state, mixed: null } });
  c.win.navigate('guide'); c.win.KVMFlowPairing.startRoleChoice();
  const chrome = () => assert.equal(c.win.document.querySelector('#steps').classList.contains('hidden'), true);
  chrome(); c.win.navigate('settings'); chrome(); c.win.navigate('guide'); chrome();
  c.win.KVMFlowPairingStateHook({ lan: { ...state.lan, setup: {} } }); await tick(); chrome();
  c.win.renderGuide(); chrome();
  assert.match(c.win.document.querySelector('#guide-content').textContent, /欢迎使用 KVMFlow/);
});

test('malformed post-pair config is rejected without removing the UI local computer', async t => {
  let config = structuredClone(v2);
  const c = await client(t, { state, request: async method => method === 'config.get' ? config : undefined });
  config = structuredClone(config); config.switch_group.devices = config.switch_group.devices.filter(d => d.device_id !== config.local_device.device_id);
  await assert.rejects(c.win.KVMFlowPairingConfig.refresh(), { code: 'E_LAN_TOPOLOGY_MISSING_SELF' });
  const model = JSON.parse(c.win.localStorage.getItem('kvmflow-local-prototype-v2'));
  assert.ok(model.computers.some(computer => computer.local && computer.id === model.localId));
  c.win.KVMFlowPairing.startPrimaryAdd();
  await c.subscriptions.get('lan.sync')({ issues: [{ code: 'E_LAN_TOPOLOGY_MISSING_SELF' }] });
  assert.match(c.win.document.querySelector('#guide-content [role="alert"]').textContent, /重新添加/);
});


const readyReport = {device_id:'dev-peer',profile_rev:2,monitors:[{fingerprint:'display-a',name:'Screen',builtin:false,input:{state:'value',value:21,source:'learned_active_read'}}]};
async function confirmPage(c, merged = false) {
  await primary(c);
  await c.subscriptions.get('lan.pair')({phase:'committed',device_id:'dev-peer',config_merged:merged});
  click(c, '#pairing-body [data-pair-action="finish"]'); await tick();
}

test('inputs alone cannot enter Try; retry recovers a missed topology-ready event', async t => {
  const c=await client(t,{state}); await confirmPage(c);
  await c.subscriptions.get('lan.profile')(readyReport); await tick();
  assert.equal(c.win.document.querySelector('[data-pair-action="done"]').disabled,true);
  assert.doesNotMatch(c.win.document.querySelector('#guide-content').textContent,/配对完成/);
  c.setConfig(pairedV2());
  click(c,'[data-pair-action="retry-setup"]'); await tick();
  assert.ok(c.requests.some(r=>r.method==='lan.profile.refresh'));
  click(c,'[data-pair-action="done"]'); await tick();
  const confirm=c.requests.findIndex(r=>r.method==='config.set' && r.params.config.switch_group.sync.ports_confirmed);
  const testStart=c.requests.findIndex(r=>r.method==='lan.test.start');
  assert.ok(confirm>=0 && testStart>confirm,'saved confirmation precedes the test request');
  assert.equal(c.win.KVMFlowPairing.activeMode(),'try');
});

test('failed topology save stays on confirmation and Retry makes the action usable again', async t => {
  let reject=true;
  const c=await client(t,{state,config:pairedV2(),request:async method=>{if(method==='config.set' && reject) throw Object.assign(new Error('disk'),{code:'E_CONFIG_IO'});}});
  await confirmPage(c,true); await c.subscriptions.get('lan.profile')(readyReport); await tick();
  click(c,'[data-pair-action="done"]'); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(),'confirm');
  assert.equal(c.requests.filter(r=>r.method==='lan.test.start').length,0);
  assert.ok(c.win.document.querySelector('[role="alert"]'));
  reject=false; click(c,'[data-pair-action="retry-setup"]'); await tick();
  click(c,'[data-pair-action="done"]'); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(),'try');
});

test('confirmation without any owner report exposes a working retry',async t=>{
  const c=await client(t,{state}); await confirmPage(c);
  click(c,'[data-pair-action="retry-setup"]'); await tick();
  assert.ok(c.requests.some(r=>r.method==='lan.profile.refresh'));
  assert.equal(c.win.document.querySelector('[data-pair-action="done"]').disabled,true);
});

test('a secondary with only mutual trust stays waiting until its topology is saved',async t=>{
  const c=await client(t,{state}); c.win.navigate('guide'); c.win.KVMFlowPairing.startRoleChoice();
  click(c,'[data-pair-role="secondary"]'); click(c,'[data-pair-action="role-next"]'); await tick();
  await c.subscriptions.get('lan.pair')({phase:'await_confirm',device_id:'dev-peer',sas:'123456'});
  await c.subscriptions.get('lan.pair')({phase:'committed',device_id:'dev-peer',config_merged:false}); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(),'secondary-wait');
  assert.ok(c.win.document.querySelector('[data-pair-action="retry-setup"]'));
  assert.equal(c.win.document.querySelector('[data-pair-action="close-window"]'),null);
  c.setConfig(pairedV2()); click(c,'[data-pair-action="retry-setup"]'); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(),'secondary-done');
});


for (const leave of [false,true]) test(`confirmation save is single-flight and respects navigation: leave=${leave}`,async t=>{
  let finish;
  const saving=new Promise(resolve=>{finish=resolve;});
  const c=await client(t,{state,config:pairedV2(),request:async method=>method==='config.set'?saving:undefined});
  await confirmPage(c,true); await c.subscriptions.get('lan.profile')(readyReport); await tick();
  const done=c.win.document.querySelector('[data-pair-action="done"]');
  done.click(); done.click(); await tick();
  assert.equal(c.requests.filter(r=>r.method==='config.set').length,1);
  assert.equal(c.requests.filter(r=>r.method==='lan.test.start').length,0);
  if(leave) c.win.navigate('settings');
  finish({}); await tick(); await tick();
  assert.equal(c.requests.filter(r=>r.method==='lan.test.start').length,leave?0:1);
  assert.equal(c.win.KVMFlowPairing.activeMode(),leave?null:'try');
});
