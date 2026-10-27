const assert = require('node:assert/strict');
const test = require('node:test');
const { client, tick, mixedState } = require('./client-helper');

const state = (mixed, lan) => ({ state: 'idle', enabled: true, mixed, ...(lan ? { lan } : {}) });
const text = (win, selector) => win.document.querySelector(selector).textContent;
const peers = (list) => ({ enabled: true, listen_port: 47820, peers: list, issues: [], sync: {}, setup: { secondary: false } });
const discovered = (over = {}) => ({
  device_id: 'dev-peer', name: 'DESKTOP-GAME', online: true, addr: '192.168.1.23:47820',
  last_seen_ms: 1, initiator: null, app_version: '0.3.0', connection_id: null, key_fp: 'abcd', last_error: null,
  ...over,
});

test('pairing runs the full phase machine: connect, SAS, mismatch paths', async t => {
  const requests = [];
  const { win, subscriptions } = await client(t, { state: state(mixedState(), peers([discovered()])), request: async (method, params) => {
    requests.push({ method, params });
    if (method === 'lan.pair.start') return { phase: 'connecting', addr: '192.168.1.23:47820' };
    if (method === 'lan.pair.confirm') return params.sas_ok
      ? { phase: 'await_peer_confirm', sas: '482 913' } : { phase: 'cancelled' };
  } });
  win.KVMFlowPairing.pairWith('dev-peer');
  await tick();
  const start = requests.find(r => r.method === 'lan.pair.start');
  assert.equal(start.params.addr, '192.168.1.23:47820');
  assert.ok(requests.some(r => r.method === 'lan.pair.window' && r.params.open === true));
  // await_confirm with SAS
  subscriptions.get('lan.pair')({ phase: 'await_confirm', device_id: 'dev-peer', initiator: true, sas: '482 913' });
  await tick();
  assert.match(text(win, '#pairing-body'), /482 913/);
  assert.match(text(win, '#pairing-body'), /等待本机确认/);
  assert.match(text(win, '#pairing-body'), /等待对方确认/);
  // local confirmed, waiting for peer — no confirm buttons
  subscriptions.get('lan.pair')({ phase: 'await_peer_confirm', device_id: 'dev-peer', initiator: true, sas: '482 913', local_confirmed: true, peer_confirmed: false });
  await tick();
  assert.match(text(win, '#pairing-body'), /本机已确认/);
  assert.equal(win.document.querySelector('[data-pair-action="confirm"]'), null);
  // peer confirms too → exchange and issues
  subscriptions.get('lan.pair')({ phase: 'await_resolve', device_id: 'dev-peer', initiator: true, sas: '482 913', local_confirmed: true, peer_confirmed: true,
    issues: [{ severity: 'blocking', detail: { kind: 'input_pending', device_id: 'dev-peer', fingerprint: 'DEL-1' } }], proposed_snapshot: {} });
  await tick();
  assert.match(text(win, '#pairing-body'), /1 台副机的显示器未读到输入值/);
  // ready_to_commit on the initiator renders as 提交中 until committed
  subscriptions.get('lan.pair')({ phase: 'ready_to_commit', device_id: 'dev-peer', initiator: true, local_confirmed: true, peer_confirmed: true, issues: [], proposed_snapshot: {} });
  await tick();
  assert.match(text(win, '#pairing-body'), /正在完成配对/);
  assert.equal(win.document.querySelector('#pairing-body button:not(:disabled)'), null);
  // committed with pending peer ack keeps the waiting note
  subscriptions.get('lan.pair')({ phase: 'committed', device_id: 'dev-peer', config_merged: true, peer_confirmed_commit: false, local_confirmed: true, peer_confirmed: true });
  await tick();
  assert.match(text(win, '#pairing-body'), /配对完成/);
  assert.match(text(win, '#pairing-body'), /正在等待对方电脑确认写入结果/);
  subscriptions.get('lan.pair')({ phase: 'peer_committed' });
  await tick();
  assert.doesNotMatch(text(win, '#pairing-body'), /正在等待对方电脑确认写入结果/);
  // mismatch cancels cleanly
  win.document.querySelector('[data-pair-action="finish"]').click();
  await tick();
  assert.equal(win.document.querySelector('#pairing-dialog').open, false);
});

test('authenticated hello name replaces the mDNS label on the SAS screen', async t => {
  const { win, subscriptions } = await client(t, { state: state(mixedState(), peers([discovered({ name: 'spoofed-mdns-name' })])) });
  win.KVMFlowPairing.pairWith('dev-peer');
  await tick();
  subscriptions.get('lan.pair')({ phase: 'await_confirm', device_id: 'dev-peer', initiator: true, sas: '482 913',
    session_id: 'auth-name', event_seq: 1, local_confirmed: false, peer_confirmed: false, peer_name: 'MacBook Pro' });
  await tick();
  const sas = text(win, '#pairing-body');
  assert.match(sas, /MacBook Pro/);
  assert.doesNotMatch(sas, /spoofed-mdns-name/);
});

test('SAS mismatch reports cancellation and both request errors surface codes', async t => {
  const { win, subscriptions } = await client(t, { state: state(mixedState(), peers([discovered()])) });
  win.KVMFlowPairing.pairWith('dev-peer');
  await tick();
  subscriptions.get('lan.pair')({ phase: 'await_confirm', device_id: 'dev-peer', initiator: true, sas: '482 913' });
  await tick();
  win.document.querySelector('[data-pair-action="mismatch"]').click();
  await tick();
  subscriptions.get('lan.pair')({ phase: 'cancelled' });
  await tick();
  assert.match(text(win, '#pairing-body'), /已取消配对/);
  assert.match(text(win, '#pairing-body'), /两台电脑的配置都没有变化/);
  // failed with rejection code
  subscriptions.get('lan.pair')({ phase: 'failed', code: 'E_LAN_PAIR_REJECTED', message: 'peer aborted', device_id: 'dev-peer' });
  await tick();
  assert.match(text(win, '#pairing-body'), /对方取消了这次配对/);
  assert.match(win.document.querySelector('#event-list').textContent, /配对未完成/);
  assert.doesNotMatch(text(win, '#pairing-body'), /E_LAN_PAIR_REJECTED/); // code is diagnostics-only
  // unknown codes fall back to a generic sentence with the code visible
  subscriptions.get('lan.pair')({ phase: 'failed', code: 'E_LAN_WEIRD', message: 'strange' });
  await tick();
  assert.match(text(win, '#pairing-body'), /配对失败（E_LAN_WEIRD）/);
});

test('every documented pairing error code maps to readable copy in both languages', async t => {
  const { win } = await client(t);
  const codes = ['E_LAN_PAIR_REJECTED', 'E_LAN_PAIR_CANCELLED', 'E_LAN_PAIR_ABORTED', 'E_LAN_PAIR_TIMEOUT',
    'E_LAN_TIMEOUT', 'E_LAN_PAIR_ONESIDED', 'E_LAN_PAIR_COMMIT', 'E_LAN_PAIR_RECORD', 'E_LAN_PAIR_INTERRUPTED',
    'E_LAN_PAIR_NOT_CONFIRMED', 'E_LAN_PAIR_WINDOW', 'E_LAN_CLOSED', 'E_LAN_BUSY', 'E_LAN_ADDRESS',
    'E_LAN_DISABLED', 'E_LAN_NOISE', 'E_LAN_GROUP_CONFLICT', 'E_LAN_PROTO'];
  const View = win.KVMFlowPairingView;
  for (const code of codes) {
    const zh = win.KVMFlowI18n.t(View.errorCopy(code).id, { p0: code, p1: '' });
    assert.ok(zh.trim() && zh !== View.errorCopy(code).id, code);
    assert.doesNotMatch(zh, /E_LAN/, `${code} leaked raw code`);
  }
  win.KVMFlowI18n.setPreference('en');
  for (const code of codes) {
    const en = win.KVMFlowI18n.t(View.errorCopy(code).id, { p0: code, p1: '' });
    assert.ok(en.trim() && !/[\u3400-\u9fff]/.test(en), code);
  }
});

test('mixed conflict choice offers both assignments and resolves with the selection', async t => {
  const requests = [];
  const { win, subscriptions } = await client(t, { state: state(mixedState(), peers([discovered()])), request: async (method, params) => {
    if (method === 'lan.pair.start') return { phase: 'connecting' };
    if (method === 'lan.pair.resolve') { requests.push({ method, params }); return { phase: 'ready_to_commit', issues: [] }; }
  } });
  win.KVMFlowPairing.pairWith('dev-peer');
  await tick();
  subscriptions.get('lan.pair')({ phase: 'await_confirm', device_id: 'dev-peer', initiator: true, sas: '1' });
  subscriptions.get('lan.pair')({ phase: 'await_resolve', device_id: 'dev-peer', initiator: true, sas: '1', issues: [
    { severity: 'choice', detail: { kind: 'mixed', default: 'basis', resolved: false,
      basis: { enabled: true, revision: 1, assignment: [{ fingerprint: 'display-a', device_id: 'host-win' }, { fingerprint: 'display-b', device_id: 'host-mac' }] },
      joiner: { enabled: true, revision: 1, assignment: [{ fingerprint: 'display-a', device_id: 'host-mac' }, { fingerprint: 'display-b', device_id: 'host-win' }] } } },
  ], proposed_snapshot: {} });
  await tick();
  const options = [...win.document.querySelectorAll('.pair-option')];
  assert.equal(options.length, 2);
  assert.match(options[0].textContent, /Windows/); // this computer is host-win in the fixture
  assert.ok(options[0].classList.contains('selected')); // basis defaults selected
  options[1].click();
  await tick();
  const fresh = [...win.document.querySelectorAll('.pair-option')];
  assert.ok(fresh[1].classList.contains('selected'));
  win.document.querySelector('[data-pair-action="resolve"]').click();
  await tick();
  assert.equal(requests.at(-1).params.mixed, 'joiner');
  assert.match(text(win, '#pairing-body'), /正在完成配对/);
});

test('peer names and addresses never inject markup into the pairing UI', async t => {
  const evil = discovered({ device_id: 'dev-x', name: '<img src=x onerror=alert(1)>', addr: '10.0.0.4:47820' });
  const { win } = await client(t, { state: state(mixedState(), peers([evil])) });
  win.KVMFlowPairing.openDialog();
  await tick();
  assert.equal(win.document.querySelectorAll('.pair-peer img').length, 0);
  assert.match(text(win, '.pair-peer'), /<img src=x/);
});

test('pair-select marks the chosen peer, hides the pairing port and shows the search hint', async t => {
  const { win } = await client(t, { state: state(mixedState(), peers([discovered({ platform: 'windows' })])) });
  win.KVMFlowPairing.openDialog();
  await tick();
  assert.match(text(win, '#pairing-body'), /正在搜索局域网内的电脑/);
  win.document.querySelector('.pair-peer[data-pair-peer="dev-peer"]').click();
  await tick();
  const row = win.document.querySelector('.pair-peer.selected');
  assert.ok(row);
  assert.equal(row.getAttribute('aria-pressed'), 'true');
  assert.match(row.textContent, /已选择/);
  assert.match(text(win, '.pair-peer .hint'), /Windows/);
  assert.match(text(win, '.pair-peer .hint'), /192\.168\.1\.23/);
  assert.doesNotMatch(text(win, '.pair-peer .hint'), /47820/);
});
