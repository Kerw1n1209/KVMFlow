const assert = require('node:assert/strict');
const test = require('node:test');
const { client, v2, tick, mixedState, pairedV2 } = require('./client-helper');

const state = (mixed, lan) => ({ state: 'idle', enabled: true, mixed, ...(lan ? { lan } : {}) });
const text = (win, selector) => win.document.querySelector(selector).textContent;
const peers = (list) => ({ enabled: true, listen_port: 47820, peers: list, issues: [], sync: {}, setup: { secondary: false } });
const discovered = (over = {}) => ({
  device_id: 'dev-peer', name: 'DESKTOP-GAME', online: true, addr: '192.168.1.23:47820',
  last_seen_ms: 1, initiator: null, app_version: '0.3.0', connection_id: null, key_fp: 'abcd', last_error: null,
  ...over,
});
const choosePrimary = async (win) => {
  if (win.KVMFlowPairing.activeMode() !== 'role') return;
  win.document.querySelector('[data-pair-role="primary"]').click();
  win.document.querySelector('[data-pair-action="role-next"]').click();
  await tick();
};

// Profile pages exercise an actual selected/committed peer, rather than
// accepting an unrelated owner's broadcast as proof of a pairing.
async function pairedForProfile(win, subscriptions) {
  if (win.KVMFlowPairing.activeMode() !== 'add') return;
  win.KVMFlowPairing.pairWith('dev-peer'); await tick();
  await subscriptions.get('lan.pair')({ phase: 'committed', device_id: 'dev-peer', config_merged: true });
  win.KVMFlowPairing.closeDialog(); await tick();
}

test('first run opens the role page; the host enters step 1 and can step back', async t => {
  const { win } = await client(t, { config: null });
  assert.equal(win.KVMFlowPairing.activeMode(), 'role');
  assert.match(text(win, '#guide-content'), /欢迎使用 KVMFlow/);
  assert.match(text(win, '#guide-content'), /这台电脑是/);
  assert.equal(win.document.querySelector('#steps').classList.contains('hidden'), true);
  assert.equal(win.document.querySelector('#guide-count').textContent, '');

  win.document.querySelector('[data-pair-role="primary"]').click();
  win.document.querySelector('[data-pair-action="role-next"]').click();
  await tick();
  assert.equal(win.KVMFlowPairing.activeMode(), null);
  assert.match(text(win, '#guide-content'), /确认 USB Switch/);
  assert.equal(win.document.querySelector('#steps').classList.contains('hidden'), false);
  assert.match(win.document.querySelector('.step.active').textContent, /1/);
  const back = win.document.querySelector('[data-pair-action="role-back"]');
  assert.ok(back);
  assert.match(back.textContent, /上一步|返回/);

  back.click();
  assert.equal(win.KVMFlowPairing.activeMode(), 'role');
  assert.match(text(win, '#guide-content'), /这台电脑是/);
});

test('choosing the host is remembered for the rest of the session', async t => {
  const { win } = await client(t, { config: null });
  await choosePrimary(win);
  win.navigate('status');
  win.navigate('guide');
  assert.equal(win.KVMFlowPairing.activeMode(), null);
  assert.match(text(win, '#guide-content'), /确认 USB Switch/);
  assert.ok(win.document.querySelector('[data-pair-action="role-back"]'));
});

test('a persisted secondary resumes the wait page instead of the role page', async t => {
  const lan = { enabled: true, listen_port: 47820,
    self: { device_id: 'dev-mac', key_fp: '1f22', addr: '192.168.1.23:47820' }, peers: [], issues: [], sync: {},
    setup: { secondary: true } };
  const { win, requests } = await client(t, { config: null, state: { state: 'unconfigured', enabled: true, lan } });
  assert.equal(win.KVMFlowPairing.activeMode(), 'secondary-wait');
  assert.doesNotMatch(text(win, '#guide-content'), /这台电脑是/);
  assert.match(text(win, '#guide-content'), /等待主力机连接/);
  assert.equal(requests.filter(entry => entry.method === 'lan.setup.secondary').length, 0);
});

test('completed setup opens step 1 from the sidebar without the role page', async t => {
  const { win } = await client(t);
  win.navigate('guide');
  assert.equal(win.KVMFlowPairing.activeMode(), null);
  assert.match(text(win, '#guide-content'), /确认 USB Switch/);
  assert.doesNotMatch(text(win, '#guide-content'), /这台电脑是/);
});

test('guide step 3 offers LAN pairing first and step 1 has no secondary shortcut', async t => {
  const { win } = await client(t);
  win.navigate('guide');
  win.nextStep(); win.nextStep();
  const guide = text(win, '#guide-content');
  assert.match(guide, /添加另一台电脑/);
  assert.match(guide, /手动填写|另一台电脑的输入值/);
  assert.equal(win.KVMFlowPairing.activeMode(), null);
  win.previousStep(); win.previousStep();
  assert.doesNotMatch(text(win, '#guide-content'), /这是副机/);
});

test('discovery list shows discovered peers, skips members and offline peers are disabled', async t => {
  const { win } = await client(t, { state: state(mixedState(), peers([
    discovered(), discovered({ device_id: 'host-mac', name: 'Mac' }), discovered({ device_id: 'dev-off', online: false, addr: null }),
  ])) });
  win.navigate('guide');
  win.nextStep(); win.nextStep();
  win.document.querySelector('[data-pair-action="add-via-lan"]').click();
  await tick();
  assert.equal(win.KVMFlowPairing.activeMode(), 'add');
  const cards = [...win.document.querySelectorAll('.pair-card')];
  assert.equal(cards.length, 2); // member host-mac filtered out
  assert.match(cards[0].textContent, /DESKTOP-GAME/);
  assert.equal(cards[1].querySelector('button').disabled, true);
  assert.match(text(win, '.pair-card'), /等待连接的副机/);
});

test('empty and disabled LAN states render distinct copy without inventing peers', async t => {
  for (const [lan, pattern] of [
    [peers([]), /没有发现局域网内的电脑/],
    [{ enabled: false, peers: [] }, /局域网功能未启用/],
  ]) {
    const { win } = await client(t, { state: state(mixedState(), lan) });
    win.navigate('guide');
    win.nextStep(); win.nextStep();
    win.document.querySelector('[data-pair-action="add-via-lan"]').click();
    await tick();
    assert.match(text(win, '#guide-content'), pattern);
    assert.match(text(win, '#guide-content'), /正在搜索局域网内的电脑…|局域网功能未启用/);
    assert.equal(win.document.querySelectorAll('.pair-card button:not(:disabled)').length, 0);
  }
});

test('pairing runs the full phase machine: connect, SAS, mismatch paths', async t => {
  const requests = [];
  const { win, subscriptions } = await client(t, { state: state(mixedState(), peers([discovered()])), request: async (method, params) => {
    requests.push({ method, params });
    if (method === 'lan.pair.start') return { phase: 'connecting', addr: '192.168.1.23:47820' };
    if (method === 'lan.pair.confirm') return params.sas_ok
      ? { phase: 'await_peer_confirm', sas: '482 913' } : { phase: 'cancelled' };
  } });
  win.navigate('guide');
  win.nextStep(); win.nextStep();
  win.document.querySelector('[data-pair-action="add-via-lan"]').click();
  await tick();
  win.document.querySelector('.pair-card [data-pair-peer]').click();
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
  assert.equal(win.KVMFlowPairing.activeMode(), 'confirm');
});

test('authenticated hello name replaces the mDNS label on the SAS screen', async t => {
  const { win, subscriptions } = await client(t, { state: state(mixedState(), peers([discovered({ name: 'spoofed-mdns-name' })])) });
  win.navigate('guide');
  win.nextStep(); win.nextStep();
  win.document.querySelector('[data-pair-action="add-via-lan"]').click();
  await tick();
  win.document.querySelector('.pair-card [data-pair-peer]').click();
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
  win.navigate('guide');
  win.nextStep(); win.nextStep();
  win.document.querySelector('[data-pair-action="add-via-lan"]').click();
  await tick();
  win.document.querySelector('.pair-card [data-pair-peer]').click();
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
  win.navigate('guide');
  win.nextStep(); win.nextStep();
  win.document.querySelector('[data-pair-action="add-via-lan"]').click();
  await tick();
  win.document.querySelector('.pair-card [data-pair-peer]').click();
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

test('primary confirmation page lists the secondary profile with pending rows and blocks 完成', async t => {
  const { win, subscriptions } = await client(t, { state: state(mixedState(), peers([discovered()])) });
  win.navigate('guide');
  win.nextStep(); win.nextStep();
  win.document.querySelector('[data-pair-action="add-via-lan"]').click();
  await tick();
  await pairedForProfile(win, subscriptions);
  subscriptions.get('lan.profile')({ device_id: 'dev-peer', profile_rev: 2, monitors: [
    { fingerprint: 'display-a', name: 'DELL U2723', builtin: false, input: { state: 'value', value: 17, source: 'learned_active_read' } },
    { fingerprint: 'display-b', name: 'AOC Q27G2', builtin: false, input: { state: 'pending' } },
  ] });
  await tick();
  assert.equal(win.KVMFlowPairing.activeMode(), 'confirm');
  const guide = text(win, '#guide-content');
  assert.match(guide, /确认.*显示器/);
  assert.match(guide, /DELL U2723/);
  assert.match(guide, /✓ 已读取/);
  assert.match(guide, /用显示器上的按键把输入切到 DESKTOP-GAME 一次/);
  assert.match(guide, /待读取/);
  assert.match(guide, /还有 1 台显示器待读取/);
  assert.equal(win.document.querySelector('[data-pair-action="done"]').disabled, true);
  assert.match(guide, /混屏分配可以稍后在设置里完成/);
});

test('three pending monitors and a single read one all render; learned inputs unlock 完成', async t => {
  const { win, subscriptions } = await client(t, { state: state(mixedState(), peers([discovered()])) });
  win.navigate('guide');
  win.nextStep(); win.nextStep();
  win.document.querySelector('[data-pair-action="add-via-lan"]').click();
  await tick();
  await pairedForProfile(win, subscriptions);
  subscriptions.get('lan.profile')({ device_id: 'dev-peer', profile_rev: 2, monitors: [
    { fingerprint: 's1', name: '', builtin: false, input: { state: 'value', value: 1 } },
    ...[2, 3, 4].map(n => ({ fingerprint: `s${n}`, name: '', builtin: false, input: { state: 'pending' } })),
  ] });
  await tick();
  assert.equal(win.document.querySelectorAll('.pair-monitor').length, 4);
  assert.match(text(win, '#pair-pending-count'), /还有 3 台显示器待读取/);
  assert.match(text(win, '#guide-content'), /显示器 2/); // default names
  await pairedForProfile(win, subscriptions);
  subscriptions.get('lan.profile')({ device_id: 'dev-peer', profile_rev: 3, monitors: [
    { fingerprint: 's1', name: '', builtin: false, input: { state: 'value', value: 1 } },
    { fingerprint: 's2', name: '', builtin: false, input: { state: 'value', value: 2 } },
    { fingerprint: 's3', name: '', builtin: false, input: { state: 'value', value: 3 } },
    { fingerprint: 's4', name: '', builtin: false, input: { state: 'pending' } },
  ] });
  await tick();
  assert.equal(win.document.querySelector('[data-pair-action="done"]').disabled, true);
  await pairedForProfile(win, subscriptions);
  subscriptions.get('lan.profile')({ device_id: 'dev-peer', profile_rev: 4, monitors: [
    ...[1, 2, 3, 4].map(n => ({ fingerprint: `s${n}`, name: '', builtin: false, input: { state: 'value', value: n } })),
  ] });
  await tick();
  assert.equal(win.document.querySelector('[data-pair-action="done"]').disabled, false);
});

test('single-monitor groups still pair; the try step summarizes two switch reports', async t => {
  const single = pairedV2();
  single.local_device.monitors.splice(1);
  single.switch_group.devices.forEach(d => d.monitors = d.monitors.filter(m => m.fingerprint === 'display-b'));
  single.switch_group.devices = single.switch_group.devices.filter(d => d.monitors.length || d.device_id === 'host-win');
  const mixed = mixedState(single);
  const { win, subscriptions, requests } = await client(t, { config: single, state: state(mixed, peers([discovered()])), request: async (method) => {
    if (method === 'lan.test.start') return { test_id: 'run-single', duration_ms: 120000 };
    return undefined;
  } });
  win.navigate('guide');
  win.nextStep(); win.nextStep();
  win.document.querySelector('[data-pair-action="add-via-lan"]').click();
  await tick();
  await pairedForProfile(win, subscriptions);
  subscriptions.get('lan.profile')({ device_id: 'dev-peer', profile_rev: 1, monitors: [
    { fingerprint: 'display-b', name: '仅有一台', builtin: false, input: { state: 'value', value: 8 } },
  ] });
  await tick();
  win.document.querySelector('[data-pair-action="done"]').click();
  await tick();
  assert.equal(win.KVMFlowPairing.activeMode(), 'try');
  assert.ok(requests.some(r => r.method === 'lan.test.start'));
  assert.match(text(win, '#guide-content'), /等待按键/);
  subscriptions.get('switch.report')({ test_id: 'run-single', press_id: 'press-1', target_device_id: 'dev-peer', per_monitor: [{ edid_id: 'display-b', commanded: true }] });
  await tick();
  assert.match(text(win, '#guide-content'), /指令已发送/);
  subscriptions.get('switch.report')({ test_id: 'run-single', press_id: 'press-2', target_device_id: 'host-win', per_monitor: [{ edid_id: 'display-b', commanded: false }] });
  await tick();
  assert.match(text(win, '#guide-content'), /未通过/);
  assert.match(text(win, '#guide-content'), /副屏 没有收到指令|输入值可能不对/);
  assert.match(win.document.querySelector('#event-list').textContent, /试切未通过/);
  assert.ok(requests.some(r => r.method === 'lan.test.stop'));
  win.document.querySelector('[data-pair-action="skip"]').click();
  await tick();
  assert.equal(win.KVMFlowPairing.activeMode(), null);
});

test('secondary role choice opens the wait page, pairing window and SAS confirm', async t => {
  const requests = [];
  const { win, subscriptions } = await client(t, { config: null, state: { state: 'unconfigured', enabled: true }, request: async (method, params) => {
    requests.push({ method, params });
    if (method === 'lan.setup.secondary') return { secondary: true, phase: 'waiting_for_primary' };
    if (method === 'lan.pair.confirm') return { phase: 'await_peer_confirm', sas: '482 913', session_id: 'inbound', event_seq: 2, local_confirmed: true, peer_confirmed: false };
  } });
  win.navigate('guide');
  assert.equal(win.KVMFlowPairing.activeMode(), 'role');
  assert.match(text(win, '#guide-content'), /这台电脑是/);
  assert.match(text(win, '#guide-content'), /这是主机/);
  assert.match(text(win, '#guide-content'), /这是副机/);
  assert.equal(win.document.querySelector('[data-pair-action="role-next"]').disabled, true);
  win.document.querySelector('[data-pair-role="secondary"]').click();
  await tick();
  assert.match(text(win, '#guide-content'), /由另一台电脑设置/);
  win.document.querySelector('[data-pair-action="role-next"]').click();
  await tick();
  assert.equal(win.KVMFlowPairing.activeMode(), 'secondary-wait');
  assert.ok(requests.some(r => r.method === 'lan.setup.secondary'));
  assert.ok(requests.some(r => r.method === 'lan.pair.window' && r.params.open === true));
  assert.match(text(win, '#guide-content'), /等待主力机连接/);
  assert.match(text(win, '#guide-content'), /正在搜索|等待主力机/);
  assert.match(text(win, '#guide-content'), /改为手动设置/);
  // inbound request shows the SAS panel
  subscriptions.get('lan.pair')({ phase: 'await_confirm', device_id: 'dev-main', initiator: false, sas: '482 913', session_id: 'inbound', event_seq: 1, local_confirmed: false, peer_confirmed: false });
  await tick();
  assert.match(text(win, '#guide-content'), /482 913/);
  win.document.querySelector('[data-pair-action="confirm"]').click();
  await tick();
  assert.match(text(win, '#guide-content'), /本机已确认/);
  assert.match(text(win, '#guide-content'), /等待对方确认/);
  // committed → done page; leaving closes the pairing window
  subscriptions.get('lan.pair')({ phase: 'committed', device_id: 'dev-main', config_merged: true, peer_confirmed_commit: true, session_id: 'inbound', event_seq: 3, local_confirmed: true, peer_confirmed: true });
  await tick();
  assert.equal(win.KVMFlowPairing.activeMode(), 'secondary-done');
  assert.match(text(win, '#guide-content'), /已由/);
  assert.match(text(win, '#guide-content'), /设置完成/);
  win.document.querySelector('[data-pair-action="close-window"]').click();
  await tick();
  // Committed mirrors the runtime by closing admission explicitly; leaving
  // the page afterwards must not send another close.
  const windowCalls = requests.filter(r => r.method === 'lan.pair.window');
  assert.deepEqual(windowCalls.map(call => call.params.open), [true, false]);
  assert.equal(win.KVMFlowPairing.activeMode(), null);
});

test('secondary done page lists monitors whose local input is still unknown', async t => {
  const config = structuredClone(v2);
  config.local_device.monitors[1].source = 'unknown';
  const { win, subscriptions } = await client(t, { config, state: state(mixedState()) });
  win.navigate('guide');
  win.KVMFlowPairing.startRoleChoice();
  win.document.querySelector('[data-pair-role="secondary"]').click();
  await tick();
  win.document.querySelector('[data-pair-action="role-next"]').click();
  await tick();
  subscriptions.get('lan.pair')({ phase: 'committed', device_id: 'dev-main', config_merged: true, peer_confirmed_commit: true, session_id: 'inbound', event_seq: 3, local_confirmed: true, peer_confirmed: true });
  await tick();
  assert.match(text(win, '#guide-content'), /还没读到本机的输入值/);
});

test('navigating away from the secondary wait page closes the pairing window', async t => {
  const requests = [];
  const { win } = await client(t, { config: null, state: { state: 'unconfigured', enabled: true }, request: async (method, params) => {
    requests.push({ method, params });
    if (method === 'lan.setup.secondary') return { secondary: true, phase: 'waiting_for_primary' };
  } });
  win.navigate('guide');
  assert.equal(win.KVMFlowPairing.activeMode(), 'role');
  win.document.querySelector('[data-pair-role="secondary"]').click();
  await tick();
  win.document.querySelector('[data-pair-action="role-next"]').click();
  await tick();
  win.navigate('status');
  await tick();
  const windowCalls = requests.filter(r => r.method === 'lan.pair.window');
  assert.deepEqual(windowCalls.map(call => call.params.open), [true, false]);
});

test('manual fallback returns to the classic wizard step and keeps entered data path', async t => {
  const { win } = await client(t, { state: state(mixedState(), peers([discovered()])) });
  win.navigate('guide');
  win.nextStep(); win.nextStep();
  win.document.querySelector('[data-pair-action="add-via-lan"]').click();
  await tick();
  assert.equal(win.KVMFlowPairing.activeMode(), 'add');
  win.document.querySelector('[data-pair-action="manual"]').click();
  await tick();
  assert.equal(win.KVMFlowPairing.activeMode(), null);
  assert.match(text(win, '#guide-content'), /另一台电脑的输入值/); // classic step 3
});

test('peer names and addresses never inject markup into the pairing UI', async t => {
  const evil = discovered({ device_id: 'dev-x', name: '<img src=x onerror=alert(1)>', addr: '10.0.0.4:47820' });
  const { win } = await client(t, { state: state(mixedState(), peers([evil])) });
  win.navigate('guide');
  win.nextStep(); win.nextStep();
  win.document.querySelector('[data-pair-action="add-via-lan"]').click();
  await tick();
  assert.equal(win.document.querySelectorAll('.pair-card img').length, 0);
  assert.match(text(win, '.pair-card'), /<img src=x/);
});

test('state notifications update the discovery list live', async t => {
  const { win, subscriptions } = await client(t, { state: state(mixedState(), peers([])) });
  win.navigate('guide');
  win.nextStep(); win.nextStep();
  win.document.querySelector('[data-pair-action="add-via-lan"]').click();
  await tick();
  assert.match(text(win, '#guide-content'), /没有发现局域网内的电脑/);
  subscriptions.get('state')(state(mixedState(), peers([discovered()])));
  await tick();
  assert.match(text(win, '#guide-content'), /DESKTOP-GAME/);
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

test('read monitor cards drop the raw input row and pending count shares the 完成 row', async t => {
  const { win, subscriptions } = await client(t, { state: state(mixedState(), peers([discovered()])) });
  win.navigate('guide');
  win.nextStep(); win.nextStep();
  win.document.querySelector('[data-pair-action="add-via-lan"]').click();
  await tick();
  await pairedForProfile(win, subscriptions);
  subscriptions.get('lan.profile')({ device_id: 'dev-peer', profile_rev: 2, monitors: [
    { fingerprint: 'display-a', name: 'DELL U2723', builtin: false, input: { state: 'value', value: 17 } },
    { fingerprint: 'display-b', name: 'AOC Q27G2', builtin: false, input: { state: 'pending' } },
  ] });
  await tick();
  const read = win.document.querySelector('.pair-monitor:not(.pending)');
  assert.doesNotMatch(read.textContent, /输入值/);
  assert.match(read.textContent, /已读取/);
  const footer = win.document.querySelector('.wizard-footer');
  assert.ok(footer.querySelector('#pair-pending-count'));
  assert.ok(footer.querySelector('[data-pair-action="done"]'));
});

test('finishing the primary confirmation moves the wizard to step 4', async t => {
  const { win, subscriptions } = await client(t, { config: pairedV2(), state: state(mixedState(), peers([discovered()])) });
  win.navigate('guide');
  win.nextStep(); win.nextStep();
  win.document.querySelector('[data-pair-action="add-via-lan"]').click();
  await tick();
  await pairedForProfile(win, subscriptions);
  subscriptions.get('lan.profile')({ device_id: 'dev-peer', profile_rev: 2, monitors: [
    { fingerprint: 'display-a', name: 'DELL U2723', builtin: false, input: { state: 'value', value: 17 } },
  ] });
  await tick();
  win.document.querySelector('[data-pair-action="done"]').click();
  await tick();
  assert.equal(win.KVMFlowPairing.activeMode(), 'try');
  assert.match(text(win, '#guide-count'), /4/);
  assert.match(win.document.querySelector('.step.active').textContent, /4/);
});

test('secondary wait page shows the local host address without the pairing port', async t => {
  const lan = { enabled: true, listen_port: 47820,
    self: { device_id: 'dev-mac', key_fp: '1f22', addr: '192.168.1.23:47820' }, peers: [], issues: [], sync: {}, setup: { secondary: true } };
  const { win } = await client(t, { config: null, state: { state: 'unconfigured', enabled: true, lan },
    request: async (method) => { if (method === 'lan.setup.secondary') return { secondary: true, phase: 'waiting_for_primary' }; } });
  win.navigate('guide');
  assert.equal(win.KVMFlowPairing.activeMode(), 'secondary-wait');
  const self = text(win, '.pair-self');
  assert.match(self, /192\.168\.1\.23/);
  assert.doesNotMatch(self, /47820/);
});
