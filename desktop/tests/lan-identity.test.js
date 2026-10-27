const assert = require('node:assert/strict');
const test = require('node:test');
const { client, tick, v2, mixedState } = require('./client-helper');
const identity = 'dev-01J00000000000000000000001';
const state = { state: 'idle', enabled: true, mixed: mixedState(), lan: { enabled: true, self: { device_id: identity, addr: '192.168.1.22' }, peers: [] } };

test('local settings writes use runtime identity even when the saved UI ID differs', async t => {
  const c = await client(t, { state, request: async method => method === 'lan.identity' ? { device_id: identity } : undefined });
  c.win.navigate('settings'); await c.win.saveSettings();
  const saved = c.requests.find(r => r.method === 'config.set').params.config;
  assert.equal(saved.local_device.device_id, identity);
  assert.ok(saved.switch_group.devices.some(d => d.device_id === identity));
  assert.equal(saved.switch_group.devices.filter(d => d.device_id === v2.local_device.device_id).length, 0);
});

test('legacy drafts recover local details with runtime identity and never arm switching', async t => {
  const c = await client(t, { config: null, state, storage: {
    'kvmflow-computer-group-draft-v1': JSON.stringify({ localId: 'mac', nextId: 'win', computers: [
      { id: 'mac', name: 'My PC', local: true, port: 1, sources: [15] }, { id: 'win', name: 'Other', local: false, port: 2, sources: [17] },
    ] }),
  } });
  const model = JSON.parse(c.win.localStorage.getItem('kvmflow-local-prototype-v2'));
  assert.equal(model.localId, identity);
  assert.equal(model.computers.find(c => c.local).id, identity);
  assert.equal(model.computers.find(c => !c.local).name, 'Other');
  assert.equal(c.requests.filter(r => r.method === 'config.set').length, 0);
});

test('missing runtime identity prevents configuration writes', async t => {
  const c = await client(t, { request: async method => method === 'lan.identity' ? {} : undefined });
  c.win.navigate('settings'); await c.win.saveSettings();
  assert.equal(c.requests.filter(r => r.method === 'config.set').length, 0);
  assert.match(c.win.document.querySelector('#event-list').textContent, /身份/);
});

test('settings discovery offers IP connection and reuses its existing pairing dialog', async t => {
  const c = await client(t, { state }); c.win.KVMFlowPairing.openDialog();
  const form = c.win.document.querySelector('.pair-ip-form'); assert.ok(form);
  form.elements.addr.value = '192.168.1.23'; form.dispatchEvent(new c.win.Event('submit', { bubbles: true, cancelable: true })); await tick();
  assert.equal(c.requests.find(r => r.method === 'lan.connect.manual').params.addr, '192.168.1.23');
});

for (const language of ['zh-CN', 'en']) test(`${language} preserves identity conflict guidance on role-free setup`, async t => {
  const c = await client(t, { language, state }); c.win.KVMFlowPairing.startPrimaryAdd();
  await c.subscriptions.get('lan.sync')({ issues: [{ code: 'E_LAN_ID_CONFLICT' }] }); await tick();
  assert.match(c.win.document.querySelector('#guide-content [role="alert"]').textContent, language === 'en' ? /same device identity/ : /相同的设备身份/);
});

test('unpaired waiting hints permit dialing without inventing an authenticated online session', async t => {
  const { setup, peer } = require('./pairing-v2-helper');
  const discovered = peer({ online: false, addr: null, pair_addr: '192.168.1.23:47820' });
  const c = await setup(t, { config: null, state: { ...state, lan: { ...state.lan, peers: [discovered] } } }); await c.start();
  const row = c.win.KVMFlowPairingView.discoveryList({ peers: [discovered] })[0];
  assert.equal(row.online, false); assert.equal(row.available, true);
  assert.equal(c.requests.find(r => r.method === 'lan.pair.start').params.addr, discovered.pair_addr);
});
