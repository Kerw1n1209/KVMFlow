const assert = require('node:assert/strict');
const test = require('node:test');
const { client, tick, v2, mixedState } = require('./client-helper');
const peer = (over = {}) => ({ device_id: 'dev-peer', name: 'GAME', online: true, addr: '192.168.1.23:47820', ...over });
const state = (peers = [peer()]) => ({ state: 'idle', enabled: true, mixed: mixedState(), lan: { enabled: true, self: { device_id: 'host-win' }, peers } });
const click = (win, selector) => { const el = win.document.querySelector(selector); assert.ok(el, selector); el.click(); };
const body = win => win.document.querySelector('#pairing-body').textContent;
const deferred = () => { let resolve, reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; };
const emit = async (c, kind, data) => { await c.subscriptions.get(kind)(data); await tick(); };
async function dial(c, id = 'dev-peer') { c.win.KVMFlowPairing.pairWith(id); await tick(); }

// P1-1: names remain text on every screen, including the inbound SAS boundary.

test('backend error detail is displayed as text without creating controls', async t => {
  const c = await client(t, { state: state() }); await dial(c);
  await emit(c, 'lan.pair', { phase: 'failed', code: 'E_UNKNOWN', message: '<button data-pair-action=confirm>fake</button>' });
  assert.equal(c.win.document.querySelectorAll('.pair-error button').length, 0);
  assert.match(body(c.win), /<button data-pair-action=confirm>/);
});

// P1-2: first-run pairing must offer only real local data to the runtime.
test('fresh waiting computer persists its identity, observed inputs and captured trigger before dialing', async t => {
  const V = require('./pairing-v2-helper');
  let persisted = null;
  const c = await V.setup(t, { config: null, computerName: 'Local', state: V.state([], 'aaa'),
    displays: [{ edid_id: 'real-display', model_name: 'DELL', ddc: { state: 'available', input: 17 } }],
    request: async (method, params) => {
      if (method === 'config.set') { persisted = structuredClone(params.config); return { saved: true }; }
      if (method === 'config.get' && persisted) return persisted;
    } });
  await V.waiting(c);
  await emit(c, 'wizard.candidates', { disappeared: [{ vid_pid: '1234:5678', product: 'USB Hub' }, { vid_pid: '2345:6789', product: 'Keyboard' }] });
  await emit(c, 'lan.peers', { peers: [V.peer()] });
  assert.equal(persisted.local_device.device_id, 'aaa');
  assert.equal(persisted.local_device.monitors[0].fingerprint, 'real-display');
  assert.equal(persisted.local_device.monitors[0].local_input, 17);
  assert.equal(persisted.local_device.monitors[0].source, 'learned_active_read');
  assert.equal(persisted.trigger.anchor.vid_pid, '1234:5678');
  assert.equal(persisted.switch_group, null, 'no synthetic remote devices or pins');
  assert.ok(c.requests.findIndex(r => r.method === 'config.set') < c.requests.findIndex(r => r.method === 'lan.pair.start'));
});

test('fresh initiator cannot dial if saving local setup fails', async t => {
  const V = require('./pairing-v2-helper');
  const c = await V.setup(t, { config: null,
    request: async method => { if (method === 'config.set') throw Object.assign(new Error('disk'), { code: 'E_CONFIG_IO' }); } });
  await c.start();
  assert.equal(c.requests.filter(r => r.method === 'config.set').length, 1);
  assert.equal(c.requests.filter(r => r.method === 'lan.pair.start').length, 0);
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'error');
});

// P1-3: read the committed snapshot before any subsequent settings write.
test('pair completion refreshes configuration before saving settings and preserves new devices', async t => {
  let live = structuredClone(v2);
  const c = await client(t, { state: state(), request: async (method, params) => {
    if (method === 'config.get') return structuredClone(live);
    if (method === 'config.set') { live = structuredClone(params.config); return { saved: true }; }
  } });
  await dial(c);
  live.switch_group.devices.push({ device_id: 'dev-peer', name: 'GAME', port_index: 4, monitors: [{ fingerprint: 'display-a', local_input: 19 }, { fingerprint: 'display-b', local_input: 20 }] });
  await emit(c, 'lan.pair', { phase: 'committed', device_id: 'dev-peer', config_merged: true });
  c.win.navigate('settings'); await c.win.saveSettings();
  const saved = c.requests.filter(r => r.method === 'config.set').at(-1);
  assert.ok(saved.params.config.switch_group.devices.some(d => d.device_id === 'dev-peer'));
  assert.ok(c.requests.filter(r => r.method === 'config.get').length >= 2);
});

test('owner profile update refreshes input values before the next settings save', async t => {
  let live = structuredClone(v2);
  const c = await client(t, { state: state(), request: async method => method === 'config.get' ? structuredClone(live) : undefined });
  live.switch_group.devices.find(d => d.device_id === 'host-mac').monitors[0].local_input = 23;
  await emit(c, 'lan.profile', { device_id: 'host-mac', profile_rev: 5, monitors: [] });
  c.win.navigate('settings'); await c.win.saveSettings();
  const saved = c.requests.filter(r => r.method === 'config.set').at(-1).params.config;
  assert.equal(saved.switch_group.devices.find(d => d.device_id === 'host-mac').monitors.find(m => m.fingerprint === 'display-a').local_input, 23);
});

// P1-4: RPC completions and notifications belong to one session, never a global last-writer-wins slot.
test('late confirm reply cannot replace committed state and duplicate commit is idempotent', async t => {
  const reply = deferred();
  const c = await client(t, { state: state(), request: async method => method === 'lan.pair.confirm' ? reply.promise : undefined }); await dial(c);
  await emit(c, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer', sas: '123 456' });
  click(c.win, '[data-pair-action="confirm"]');
  const committed = { phase: 'committed', device_id: 'dev-peer', config_merged: false };
  await emit(c, 'lan.pair', committed);
  const gets = c.requests.filter(r => r.method === 'config.get').length;
  await emit(c, 'lan.pair', committed);
  assert.equal(c.requests.filter(r => r.method === 'config.get').length, gets, 'duplicate commit must not refresh again');
  reply.resolve({ phase: 'await_peer_confirm', sas: '123 456' }); await tick();
  assert.match(body(c.win), /配置待完成/);
  await emit(c, 'lan.pair', { phase: 'peer_committed' });
  assert.match(body(c.win), /配置待完成/, 'peer ack must preserve the pin-only commit result');
});

test('late start reply cannot replace SAS and repeated connect or confirm cannot issue a second request', async t => {
  const start = deferred(), confirm = deferred();
  const c = await client(t, { state: state(), request: async method => method === 'lan.pair.start' ? start.promise : method === 'lan.pair.confirm' ? confirm.promise : undefined });
  await dial(c); c.win.KVMFlowPairing.pairWith('dev-peer');
  await emit(c, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer', sas: '123 456' });
  start.resolve({ phase: 'connecting' }); await tick();
  assert.match(body(c.win), /123 456/);
  click(c.win, '[data-pair-action="confirm"]');
  c.win.document.querySelector('[data-pair-action="confirm"]')?.click();
  assert.equal(c.requests.filter(r => r.method === 'lan.pair.start').length, 1);
  assert.equal(c.requests.filter(r => r.method === 'lan.pair.confirm').length, 1);
  confirm.resolve({ phase: 'await_peer_confirm', sas: '123 456' }); await tick();
});

test('old peer and retired session notifications cannot overwrite a new SAS or profile', async t => {
  const c = await client(t, { state: state([peer(), peer({ device_id: 'dev-other', name: 'OTHER' })]) });
  await dial(c);
  await emit(c, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer', session_id: 'old', event_seq: 1, sas: '111 111' });
  click(c.win, '[data-pair-action="cancel"]'); await tick();
  await dial(c, 'dev-other');
  await emit(c, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-other', session_id: 'new', event_seq: 2, sas: '222 222' });
  await emit(c, 'lan.pair', { phase: 'failed', device_id: 'dev-peer', session_id: 'old', event_seq: 3, code: 'E_LAN_CLOSED' });
  await emit(c, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-other', session_id: 'new', event_seq: 1, sas: '333 333' });
  await emit(c, 'lan.profile', { device_id: 'dev-peer', profile_rev: 4, monitors: [] });
  assert.match(body(c.win), /222 222/);
  assert.equal(c.win.document.querySelector('#pair-peer-name'), null);
});

test('same-peer retry ignores the retired session and a late resolve reply cannot undo commit', async t => {
  const resolve = deferred();
  const c = await client(t, { state: state(), request: async method => method === 'lan.pair.resolve' ? resolve.promise : undefined });
  await dial(c);
  await emit(c, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer', session_id: 'first', event_seq: 1, sas: '111 111' });
  click(c.win, '[data-pair-action="cancel"]'); await tick(); await dial(c);
  await emit(c, 'lan.pair', { phase: 'await_resolve', device_id: 'dev-peer', initiator: true, session_id: 'second', event_seq: 2, issues: [
    { severity: 'choice', detail: { kind: 'mixed', basis: {}, joiner: {}, resolved: false } },
  ] });
  click(c.win, '[data-pair-action="resolve"]');
  await emit(c, 'lan.pair', { phase: 'committed', device_id: 'dev-peer', session_id: 'second', event_seq: 3, config_merged: true });
  resolve.resolve({ phase: 'ready_to_commit', issues: [] }); await tick();
  await emit(c, 'lan.pair', { phase: 'failed', device_id: 'dev-peer', session_id: 'first', event_seq: 9, code: 'E_LAN_CLOSED' });
  assert.match(body(c.win), /配对完成/);
});

test('record reconciliation events never replace a live pairing session', async t => {
  const c = await client(t, { state: state() });
  await dial(c);
  await emit(c, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer', session_id: 'live', event_seq: 1, sas: '123 456', local_confirmed: false, peer_confirmed: false });
  await emit(c, 'lan.pair', { phase: 'peer_committed', session_id: 'old-record', event_seq: 1, local_confirmed: true, peer_confirmed: true });
  await emit(c, 'lan.pair', { phase: 'failed', code: 'E_LAN_PAIR_ONESIDED', session_id: 'old-record', event_seq: 2 });
  assert.match(body(c.win), /123 456/, 'the live SAS stays on screen');
  await emit(c, 'lan.pair', { phase: 'await_peer_confirm', device_id: 'dev-peer', session_id: 'live', event_seq: 2, sas: '123 456', local_confirmed: true, peer_confirmed: false });
  assert.match(body(c.win), /本机已确认/);
});

// P1-5 / P2-7: every exit owns admission cleanup; retry deliberately opens a new window.
test('Esc and native dialog close cancel pairing and close admission even outside the wizard', async t => {
  const c = await client(t, { state: state() }); await dial(c);
  const dialog = c.win.document.querySelector('#pairing-dialog');
  dialog.dispatchEvent(new c.win.Event('cancel', { cancelable: true }));
  dialog.close(); await tick();
  assert.ok(c.requests.some(r => r.method === 'lan.pair.cancel'));
  assert.equal(c.requests.filter(r => r.method === 'lan.pair.window').at(-1).params.open, false);
  c.win.KVMFlowPairing.openDialog(); await tick(); dialog.close(); await tick();
  assert.equal(c.requests.filter(r => r.method === 'lan.pair.window').at(-1).params.open, false);
});

test('leaving the primary guide cancels its active session and closes its dialog', async t => {
  const c = await client(t, { state: state() }); c.win.navigate('guide'); c.win.KVMFlowPairing.startPrimaryAdd(); await dial(c);
  c.win.navigate('settings'); await tick();
  assert.equal(c.win.document.querySelector('#pairing-dialog').open, false);
  assert.ok(c.requests.some(r => r.method === 'lan.pair.cancel'));
  assert.equal(c.requests.filter(r => r.method === 'lan.pair.window').at(-1).params.open, false);
});

test('failed admission close is retried and a failed dial does not leave admission open', async t => {
  let failedClose = false;
  const c = await client(t, { state: state(), request: async (method, params) => {
    if (method === 'lan.pair.window' && !params.open && !failedClose) { failedClose = true; throw new Error('transport'); }
    if (method === 'lan.pair.start') throw Object.assign(new Error('offline'), { code: 'E_LAN_OFFLINE' });
  } });
  await dial(c); c.win.KVMFlowPairing.closeDialog(); await tick(); await tick();
  assert.equal(c.requests.filter(r => r.method === 'lan.pair.window' && !r.params.open).length, 2);
});

const aliasIssue = { severity: 'choice', detail: { kind: 'monitor_alias', group_fingerprint: 'display-a', local_fingerprint: 'peer-display', default_same: true, resolved: false } };
const replacementIssue = { severity: 'choice', detail: { kind: 'replace_device', old_device_id: 'old-peer', new_device_id: 'dev-peer', default_replace: true, resolved: false } };
const mixedIssue = { severity: 'choice', detail: { kind: 'mixed', basis: {}, joiner: {}, default: 'basis', resolved: false } };
async function choices(c, issues, initiator = true) {
  await dial(c); await emit(c, 'lan.pair', { phase: 'await_resolve', device_id: 'dev-peer', initiator, issues });
}
// P1-6 / P2-8: choices must produce the exact documented wire answers.
test('mixed default selection sends basis without requiring a redundant click', async t => {
  const c = await client(t, { state: state() }); await choices(c, [mixedIssue]);
  click(c.win, '[data-pair-action="resolve"]'); await tick();
  assert.equal(c.requests.find(r => r.method === 'lan.pair.resolve').params.mixed, 'basis');
});

test('monitor alias choice submits local-to-group fingerprints and can be declined safely', async t => {
  const c = await client(t, { state: state() }); await choices(c, [aliasIssue]);
  assert.match(body(c.win), /同一台显示器/);
  click(c.win, '[data-pair-action="resolve"]'); await tick();
  const params = c.requests.find(r => r.method === 'lan.pair.resolve').params;
  assert.deepEqual(JSON.parse(JSON.stringify(params.monitor_aliases)), { 'peer-display': 'display-a' });
  assert.equal(params.replacement, undefined);
});

test('replacement choice submits an explicit keep-old answer', async t => {
  const c = await client(t, { state: state() }); await choices(c, [replacementIssue]);
  click(c.win, '[data-pair-replacement="false"]');
  click(c.win, '[data-pair-action="resolve"]'); await tick();
  assert.deepEqual(JSON.parse(JSON.stringify(c.requests.find(r => r.method === 'lan.pair.resolve').params.replacement)), { old_device_id: 'old-peer', replace: false });
});

test('choosing different monitors sends an explicit false answer and keeps pairing', async t => {
  const c = await client(t, { state: state() }); await choices(c, [aliasIssue]);
  click(c.win, '[data-pair-alias="different"]'); await tick();
  const selected = c.win.document.querySelector('[data-pair-alias="different"]');
  assert.ok(selected.classList.contains('selected'));
  assert.equal(c.win.document.querySelector('[data-pair-alias="same"]').classList.contains('selected'), false);
  click(c.win, '[data-pair-action="resolve"]'); await tick();
  const params = c.requests.find(r => r.method === 'lan.pair.resolve').params;
  assert.deepEqual(JSON.parse(JSON.stringify(params.monitor_aliases)), { 'peer-display': false });
  assert.equal(c.requests.filter(r => r.method === 'lan.pair.cancel').length, 0);
});

test('pin-only completion shows the unmatched blocker and pending configuration state', async t => {
  const c = await client(t, { state: state() }); await dial(c);
  await emit(c, 'lan.pair', {
    phase: 'committed', device_id: 'dev-peer', config_merged: false, peer_confirmed_commit: true,
    local_confirmed: true, peer_confirmed: true,
    issues: [{ severity: 'blocking', detail: { kind: 'monitor_unmatched', basis: ['display-a'], joiner: ['peer-display'] } }],
  });
  assert.match(body(c.win), /配置待完成/);
  assert.match(body(c.win), /没有一一对应/);
});

test('session events use the explicit flags and the start reply sequence is zero', async t => {
  const c = await client(t, { state: state(), request: async method => method === 'lan.pair.start'
    ? { phase: 'connecting', session_id: 's1', event_seq: 0, local_confirmed: false, peer_confirmed: false }
    : undefined });
  await dial(c);
  await emit(c, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer', session_id: 's1', event_seq: 1, sas: '111 111', local_confirmed: false, peer_confirmed: false });
  assert.match(body(c.win), /等待本机确认/);
  await emit(c, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer', session_id: 's1', event_seq: 1, sas: '111 111', local_confirmed: true, peer_confirmed: false });
  assert.match(body(c.win), /等待本机确认/, 'a duplicate sequence must not update the dialog');
  await emit(c, 'lan.pair', { phase: 'await_peer_confirm', device_id: 'dev-peer', session_id: 's1', event_seq: 2, sas: '111 111', local_confirmed: true, peer_confirmed: false });
  assert.match(body(c.win), /本机已确认/);
  const flags = c.win.KVMFlowPairingView.confirmedFlags({ phase: 'await_peer_confirm' });
  assert.deepEqual(JSON.parse(JSON.stringify(flags)), { local: false, peer: false });
});

test('declining a display correspondence cancels without sending an invented alias answer', async t => {
  const c = await client(t, { state: state() }); await choices(c, [aliasIssue]);
  click(c.win, '[data-pair-action="cancel"]'); await tick();
  assert.equal(c.requests.filter(r => r.method === 'lan.pair.resolve').length, 0);
  assert.ok(c.requests.some(r => r.method === 'lan.pair.cancel'));
});

test('settings pairing fallback uses raw peers instead of filtering the selected group device', async t => {
  const c = await client(t, { state: state([peer({ device_id: 'host-mac', pairing_available: true })]) });
  c.win.navigate('settings');
  click(c.win, '[data-mixed-peer="host-mac"] button'); await tick();
  assert.ok(c.requests.some(r => r.method === 'lan.pair.start' && r.params.addr === '192.168.1.23:47820'));
});

test('an already pinned group member reconnects without starting another pairing', async t => {
  const config = structuredClone(v2);
  config.switch_group.sync = { members: [{ device_id: 'host-mac', public_key: 'a'.repeat(64), removed: false }] };
  const c = await client(t, { config, state: state([peer({ device_id: 'host-mac', pairing_available: true })]) });
  c.win.navigate('settings'); click(c.win, '[data-mixed-peer="host-mac"] button'); await tick();
  assert.ok(c.requests.some(r => r.method === 'lan.connect.manual'));
  assert.equal(c.requests.filter(r => r.method === 'lan.pair.start').length, 0);
});

test('background owner refresh preserves the selected editor and unsaved name', async t => {
  const c = await client(t, { state: state() }); c.win.navigate('settings'); c.win.editComputer('host-mac');
  c.win.document.querySelector('#edit-name').value = '重命名草稿';
  await emit(c, 'lan.profile', { device_id: 'host-mac', profile_rev: 8, monitors: [] });
  assert.equal(c.win.document.querySelector('#edit-name').value, '重命名草稿');
  await c.win.saveSettings();
  assert.equal(c.requests.filter(r => r.method === 'config.set').at(-1).params.config.switch_group.devices.find(d => d.device_id === 'host-mac').name, '重命名草稿');
});

test('settings save queued behind a refresh rebases its edits onto the latest topology', async t => {
  const read = deferred(); let defer = false;
  const c = await client(t, { state: state(), request: async method => method === 'config.get' && defer ? read.promise : undefined });
  c.win.navigate('settings'); c.win.editComputer('host-mac'); c.win.document.querySelector('#edit-name').value = '保存中的新名字';
  defer = true;
  const refresh = c.win.KVMFlowPairingConfig.refresh(); await tick();
  const saving = c.win.saveSettings(); await tick();
  const updated = structuredClone(v2);
  updated.switch_group.devices.push({ device_id: 'dev-peer', name: 'GAME', port_index: 4, monitors: [{ fingerprint: 'display-a', local_input: 19 }, { fingerprint: 'display-b', local_input: 20 }] });
  read.resolve(updated); await refresh; await saving;
  const config = c.requests.filter(r => r.method === 'config.set').at(-1).params.config;
  assert.equal(config.switch_group.devices.find(d => d.device_id === 'host-mac').name, '保存中的新名字');
  assert.ok(config.switch_group.devices.some(d => d.device_id === 'dev-peer'));
});

test('a removed editor target cannot transfer its unsaved fields onto the local computer', async t => {
  let live = structuredClone(v2);
  const c = await client(t, { state: state(), request: async method => method === 'config.get' ? structuredClone(live) : undefined });
  c.win.navigate('settings'); c.win.editComputer('host-mac'); c.win.document.querySelector('#edit-name').value = 'Removed draft';
  live.switch_group.devices = live.switch_group.devices.filter(d => d.device_id !== 'host-mac');
  await emit(c, 'lan.profile', { device_id: 'dev-peer', profile_rev: 9, monitors: [] });
  assert.equal(c.win.document.querySelector('#edit-name').value, 'Windows');
});

test('events from an earlier configuration do not survive into a fresh setup', async t => {
  const stale = { localId: 'draft-local', nextId: 'draft-remote', usbConfirmed: true, displays: [],
    computers: [{ id: 'draft-local', name: 'MAC', port: 1, local: true, sources: [] }],
    events: [{ title: '配对完成', copy: '旧配置', time: '10:00' }] };
  const c = await client(t, { config: null, state: state([]), storage: { 'kvmflow-local-prototype-v2': JSON.stringify(stale) } });
  assert.doesNotMatch(c.win.document.querySelector('#event-list').textContent, /旧配置/);
  c.win.log('本次事件', '新会话');
  assert.match(c.win.document.querySelector('#event-list').textContent, /新会话/);
});
