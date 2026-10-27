const assert = require('node:assert/strict');
const test = require('node:test');
const { v2 } = require('./client-helper');
const { setup, waiting, ready, state, peer, displays, click, body, emit, profile, tick } = require('./pairing-v2-helper');
const calls = (c, method) => c.requests.filter(r => r.method === method);
const deferred = () => { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; };
const candidates = { disappeared: [{ vid_pid: '1234:5678', product: 'USB Hub' }, { vid_pid: '2345:6789', product: 'Keyboard' }] };

test('A1 uses the same three-step setup for fresh, configured and persisted waiting computers', async t => {
  for (const config of [null, v2]) {
    const c = await setup(t, { config, state: { ...state([]), lan: { ...state([]).lan, setup: { secondary: true } } } });
    assert.match(body(c), /开始配置.*请确认键盘和鼠标已连接到 USB 切换器。/s);
    assert.equal(c.win.document.querySelectorAll('#steps .step').length, 3);
    assert.equal(c.win.document.querySelector('#guide-count').textContent, '');
    assert.doesNotMatch(body(c), /主机|副机|配对码|手动填写|输入值/);
    c.win.navigate('settings'); c.win.navigate('guide');
    assert.match(body(c), /开始配置/);
  }
});

test('one waiting peer dials automatically after persisting observed displays without a trigger', async t => {
  const c = await setup(t, { config: null, computerName: 'LOCAL' }); await c.start();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'connecting');
  const saved = calls(c, 'config.set')[0].params.config;
  assert.equal(saved.local_device.device_id, 'host-win');
  assert.equal(saved.trigger, null);
  assert.equal(saved.local_device.monitors[0].local_input, 15);
  assert.ok(c.requests.indexOf(calls(c, 'config.set')[0]) < c.requests.indexOf(calls(c, 'lan.pair.start')[0]));
  assert.equal(c.win.document.querySelector('#pairing-dialog').open, false);
  assert.match(body(c), /连接另一台电脑.*GAME.*连接中/s);
});

test('zero waiting peers, including old peers without waiting, wait for ten seconds then open A2 and USB capture', async t => {
  const c = await setup(t, { state: state([peer({ waiting: undefined })]) }); await c.start();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'search');
  assert.equal(calls(c, 'lan.setup.secondary').length, 0);
  await c.fire(10000);
  assert.equal(calls(c, 'lan.setup.secondary').length, 1);
  assert.equal(calls(c, 'wizard.begin').length, 1);
  assert.equal(calls(c, 'lan.pair.start').length, 0);
  assert.match(body(c), /切换到另一台电脑.*按 USB 切换器.*切换显示器输入.*在另一台电脑上打开 KVMFlow.*192.168.1.22/s);
  await emit(c, 'wizard.candidates', { disappeared: [{ vid_pid: '2345:6789', product: 'Keyboard' }] });
  assert.equal(calls(c, 'wizard.end').length, 0);
  await emit(c, 'wizard.candidates', candidates);
  assert.equal(calls(c, 'wizard.end').length, 1);
  assert.equal(calls(c, 'config.set').at(-1).params.config.trigger.anchor.vid_pid, '1234:5678');
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'wait');
});

test('multiple waiting peers require explicit selection and revalidate disappearing selections', async t => {
  const c = await setup(t, { state: state([peer(), peer({ device_id: 'dev-two', name: 'TWO' }), peer({ device_id: 'old', waiting: undefined }), peer({ device_id: 'offline', online: false, addr: null })]) });
  await c.start(); assert.equal(calls(c, 'lan.pair.start').length, 0);
  assert.equal(c.win.document.querySelectorAll('[data-v2-peer]').length, 2);
  assert.equal(c.win.document.querySelector('[data-pair-action="v2-connect"]').disabled, true);
  c.win.document.querySelector('[data-v2-peer="dev-two"]').click();
  await emit(c, 'lan.peers', { peers: [peer()] });
  assert.equal(c.win.document.querySelector('[data-pair-action="v2-connect"]').disabled, true);
  c.win.document.querySelector('[data-v2-peer="dev-peer"]').click(); click(c, 'v2-connect'); await tick();
  assert.equal(calls(c, 'lan.pair.start').length, 1);
});

test('simultaneously waiting computers use device id order to dial exactly one direction', async t => {
  for (const [id, expected] of [['aaa', 1], ['zzz', 0]]) {
    const c = await setup(t, { config: null, state: state([], id) }); await waiting(c);
    await emit(c, 'lan.peers', { peers: [peer()] });
    assert.equal(calls(c, 'lan.pair.start').length, expected);
  }
});

test('both initiator and responder confirm automatically once, including peer-first confirmation', async t => {
  for (const responder of [false, true]) {
    const c = await setup(t, { state: state(responder ? [] : [peer()]) });
    if (responder) await waiting(c); else await c.start();
    const event = { phase: responder ? 'peer_confirmed' : 'await_confirm', device_id: 'dev-peer', initiator: !responder, session_id: 's1', event_seq: 1, sas: '123 456', peer_confirmed: responder };
    await emit(c, 'lan.pair', event); await emit(c, 'lan.pair', { ...event, event_seq: 2 });
    assert.equal(calls(c, 'lan.pair.confirm').length, 1);
    assert.equal(calls(c, 'lan.pair.confirm')[0].params.sas_ok, true);
    assert.doesNotMatch(body(c), /123 456|一致|确认配对/);
  }
});

test('all default choices are submitted automatically, preserving false defaults; responder never resolves', async t => {
  const issues = [
    { severity: 'choice', detail: { kind: 'mixed', default: 'joiner' } },
    { severity: 'choice', detail: { kind: 'monitor_alias', default_same: false, local_fingerprint: 'l', group_fingerprint: 'g' } },
    { severity: 'choice', detail: { kind: 'replace_device', default_replace: false, old_device_id: 'old' } },
  ];
  for (const responder of [false, true]) {
    const c = await setup(t, { state: state(responder ? [] : [peer()]) });
    if (responder) { await waiting(c); await emit(c, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer', initiator: false }); }
    else await c.start();
    await emit(c, 'lan.pair', { phase: 'await_resolve', device_id: 'dev-peer', initiator: !responder, issues });
    assert.equal(calls(c, 'lan.pair.resolve').length, responder ? 0 : 1);
    if (!responder) assert.deepEqual(JSON.parse(JSON.stringify(calls(c, 'lan.pair.resolve')[0].params)), { mixed: 'joiner', monitor_aliases: { l: false }, replacement: { old_device_id: 'old', replace: false } });
    assert.equal(c.win.document.querySelectorAll('.pair-option').length, 0);
  }
});

test('E1 is the IP-only fallback; manual connection and search again leave the form', async t => {
  const c = await setup(t, { state: state([]) }); await waiting(c); click(c, 'ip'); await tick();
  assert.match(body(c), /未找到另一台电脑.*IP 地址显示在另一台电脑的等待页面。/s);
  const form = c.win.document.querySelector('.v2-ip-form'); form.elements.addr.value = '192.168.1.50';
  await emit(c, 'lan.peers', { peers: [peer()] });
  assert.equal(form.elements.addr.value, '192.168.1.50');
  form.dispatchEvent(new c.win.Event('submit', { bubbles: true, cancelable: true })); await tick();
  assert.equal(calls(c, 'lan.connect.manual').at(-1).params.pair, true);
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'connecting');
  click(c, 'v2-cancel'); await tick(); await c.start();
  // A new search now sees the newly discovered peer and dials immediately.
  assert.equal(calls(c, 'lan.pair.start').length, 1);
  const d = await setup(t, { state: state([]) }); await waiting(d); click(d, 'ip'); await tick(); click(d, 'search'); await tick();
  assert.equal(d.win.KVMFlowPairing.activeMode(), 'search');
  assert.equal(d.win.document.querySelector('.v2-ip-form'), null);
});

test('E2 retries pairing with a new session and ignores retired events', async t => {
  const c = await setup(t); await c.start();
  await emit(c, 'lan.pair', { phase: 'failed', device_id: 'dev-peer', session_id: 'old', event_seq: 1, code: 'E_LAN_CLOSED' });
  assert.match(body(c), /配对失败.*连接已中断。请检查防火墙是否允许 KVMFlow 访问网络。/s);
  click(c, 'v2-retry'); await tick();
  assert.equal(calls(c, 'lan.pair.start').length, 2);
  await emit(c, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer', session_id: 'new', event_seq: 1 });
  await emit(c, 'lan.pair', { phase: 'failed', device_id: 'dev-peer', session_id: 'old', event_seq: 99 });
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'connecting');
});

test('E4 retries current reads, skips one failed display at a time and never invents input values', async t => {
  let available = false;
  const c = await setup(t, { request: async method => method === 'display.list' ? { displays: displays.map(d => ({ ...d, ddc: available ? d.ddc : { state: 'unavailable' } })) } : undefined });
  await c.start(); assert.equal(c.win.KVMFlowPairing.activeMode(), 'read-error');
  assert.equal(calls(c, 'lan.pair.start').length, 0);
  click(c, 'read-skip'); await tick(); assert.equal(c.win.KVMFlowPairing.activeMode(), 'read-error');
  available = true; click(c, 'read-retry'); await tick();
  const saved = calls(c, 'config.set').at(-1).params.config;
  assert.equal(saved.local_device.monitors.length, 1);
  assert.equal(saved.local_device.monitors[0].fingerprint, 'display-b');
  assert.doesNotMatch(body(c), /0x0F|输入值/);
});

test('B2 waits for the selected owner, pending inputs and committed topology before enabling Next', async t => {
  const c = await setup(t); await c.start();
  await emit(c, 'lan.pair', { phase: 'committed', device_id: 'dev-peer', config_merged: false });
  assert.equal(c.win.document.querySelector('[data-pair-action="v2-next"]').disabled, true);
  await emit(c, 'lan.profile', { ...profile(false), device_id: 'foreign' });
  assert.match(body(c), /等待.*显示器/);
  await emit(c, 'lan.profile', profile(true));
  assert.match(body(c), /请用显示器按键切换到本机。/);
  assert.equal(c.win.document.querySelector('[data-pair-action="v2-next"]').disabled, true);
  await emit(c, 'lan.profile', profile(false, 2));
  assert.equal(c.win.document.querySelector('[data-pair-action="v2-next"]').disabled, true);
  c.setConfig(require('./client-helper').pairedV2()); await emit(c, 'lan.setup', { phase: 'topology_ready' });
  assert.equal(c.win.document.querySelector('[data-pair-action="v2-next"]').disabled, false);
  await emit(c, 'lan.profile', profile(true, 1));
  assert.equal(c.win.document.querySelector('[data-pair-action="v2-next"]').disabled, false);
  assert.doesNotMatch(body(c), /输入值|0x|电脑名称|端口/);
});

test('B3 starts a bounded test, detects a missing trigger and shows B3a only after a switch press', async t => {
  const c = await setup(t, { request: async method => method === 'lan.test.start' ? { test_id: 'test-1' } : undefined }); await ready(c);
  const config = require('./client-helper').pairedV2(); config.trigger = null; c.setConfig(config);
  click(c, 'v2-next'); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'try');
  assert.equal(calls(c, 'lan.test.start')[0].params.duration_ms, 120000);
  assert.equal(calls(c, 'wizard.begin').length, 1);
  await emit(c, 'wizard.candidates', candidates);
  assert.equal(calls(c, 'wizard.end').length, 1);
  await c.fire(5000);
  assert.match(body(c), /如果显示器未切换，请手动切换到 GAME。/);
  assert.equal(c.win.document.querySelectorAll('#guide-content button').length, 0);
  c.win.navigate('settings'); await tick();
  assert.equal(calls(c, 'lan.test.stop').length, 1);
});

test('B3 skip and navigation both stop the test once; command acceptance cannot finish the flow', async t => {
  for (const leave of ['skip', 'navigate']) {
    const c = await setup(t, { request: async method => method === 'lan.test.start' ? { test_id: 'new' } : undefined }); await ready(c); click(c, 'v2-next'); await tick();
    await emit(c, 'switch.report', { test_id: 'old', press_id: 'p', reporting_device_id: 'host-win', target_device_id: 'dev-peer', per_monitor: [{ commanded: true }] });
    await emit(c, 'switch.report', { test_id: 'new', press_id: 'p', reporting_device_id: 'host-win', target_device_id: 'dev-peer', per_monitor: [{ commanded: true }] });
    assert.equal(c.win.KVMFlowPairing.activeMode(), 'try');
    assert.doesNotMatch(body(c), /成功|画面已到达/);
    if (leave === 'skip') click(c, 'v2-finish'); else c.win.navigate('settings');
    await tick(); c.win.navigate('status'); await tick();
    assert.equal(calls(c, 'lan.test.stop').length, 1);
  }
});

test('waiting computer completes A3 and shows E3 only for a peer test targeting this computer', async t => {
  const c = await setup(t, { state: state([]) }); await waiting(c);
  await emit(c, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer', initiator: false });
  c.setConfig(require('./client-helper').pairedV2());
  await emit(c, 'lan.pair', { phase: 'committed', device_id: 'dev-peer', initiator: false, config_merged: true });
  assert.match(body(c), /两台电脑已配对完成/);
  const report = { test_id: 'remote', press_id: 'p', reporting_device_id: 'dev-peer', target_device_id: 'host-win', per_monitor: [{ edid_id: 'display-b', commanded: false }] };
  await emit(c, 'switch.report', { ...report, reporting_device_id: 'foreign' }); assert.equal(c.win.KVMFlowPairing.activeMode(), 'done');
  await emit(c, 'lan.test', { test_id: 'remote', events: [{ device_id: 'dev-peer', kind: 'switch.report', body: report }] });
  assert.match(body(c), /显示器未切换.*未响应切换指令。请在显示器菜单中开启 DDC\/CI。/s);
  click(c, 'test-retry'); await tick(); assert.equal(c.win.KVMFlowPairing.activeMode(), 'try');
  assert.equal(calls(c, 'lan.test.start').length, 1);
});

test('leaving every active page cancels pairing, ends capture and closes admission; late setup stays closed', async t => {
  const late = deferred();
  const c = await setup(t, { state: state([]), request: async method => method === 'lan.setup.secondary' ? late.promise : undefined });
  await c.start(); await c.fire(10000); await emit(c, 'client.hidden', {});
  late.resolve({ secondary: true }); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(), null);
  assert.equal(calls(c, 'lan.pair.window').at(-1).params.open, false);
  assert.equal(calls(c, 'wizard.begin').length, 0);
  const d = await setup(t); await d.start(); d.win.navigate('settings'); await tick();
  assert.equal(calls(d, 'lan.pair.cancel').length, 1);
  assert.equal(calls(d, 'lan.pair.window').at(-1).params.open, false);
  await emit(d, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer' });
  assert.equal(calls(d, 'lan.pair.confirm').length, 0);
});

test('names are escaped in B1 and B2, language changes preserve in-progress setup and IP drafts', async t => {
  const payload = '<button data-pair-action=v2-finish>fake</button>';
  const c = await setup(t, { state: state([peer({ name: payload })]) }); await c.start();
  assert.equal(c.win.document.querySelectorAll('#guide-content strong button').length, 0);
  assert.match(body(c), /<button/);
  c.win.KVMFlowI18n.setPreference('en'); assert.match(body(c), /Connect another computer/);
  assert.equal(calls(c, 'lan.pair.start').length, 1);
  assert.equal(c.win.document.querySelectorAll('[onclick]').length, 0);
});

test('final Chinese copy is exact on A3 and B2, including button labels', async t => {
  const c = await setup(t); await ready(c);
  assert.equal(c.win.document.querySelector('[data-pair-action="v2-next"]').textContent, '下一步');
  const d = await setup(t, { state: state([]) }); await waiting(d);
  await emit(d, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer', initiator: false });
  d.setConfig(require('./client-helper').pairedV2());
  await emit(d, 'lan.pair', { phase: 'committed', device_id: 'dev-peer', initiator: false, config_merged: true });
  assert.equal(d.win.document.querySelector('#guide-content h1').textContent, '两台电脑已配对完成');
  assert.equal(d.win.document.querySelector('[data-pair-action="v2-finish"]').textContent, '完成');
});

test('true default choices require no input and are sent once', async t => {
  const c = await setup(t); await c.start();
  const event = { phase: 'await_resolve', device_id: 'dev-peer', initiator: true, issues: [
    { severity: 'choice', detail: { kind: 'monitor_alias', default_same: true, local_fingerprint: 'l', group_fingerprint: 'g' } },
    { severity: 'choice', detail: { kind: 'replace_device', default_replace: true, old_device_id: 'old' } },
  ] };
  await emit(c, 'lan.pair', event); await emit(c, 'lan.pair', event);
  assert.equal(calls(c, 'lan.pair.resolve').length, 1);
  assert.equal(calls(c, 'lan.pair.resolve')[0].params.monitor_aliases.l, 'g');
  assert.equal(calls(c, 'lan.pair.resolve')[0].params.replacement.replace, true);
});

test('an empty trigger object also starts B3 capture; saved topology and trigger are preserved', async t => {
  const c = await setup(t); await ready(c);
  const config = require('./client-helper').pairedV2(); config.trigger = {};
  c.setConfig(config); click(c, 'v2-next'); await tick();
  assert.equal(calls(c, 'wizard.begin').length, 1);
  await emit(c, 'wizard.candidates', candidates);
  const saved = calls(c, 'config.set').at(-1).params.config;
  assert.ok(saved.switch_group.devices.some(d => d.device_id === 'dev-peer'));
  assert.equal(saved.trigger.anchor.vid_pid, '1234:5678');
});

test('leaving during search cleanup never schedules a new window or resumes discovery', async t => {
  const close = deferred(); let blockClose = false;
  const c = await setup(t, { state: state([]), request: async (method, params) => method === 'lan.pair.window' && !params.open && blockClose ? close.promise : undefined });
  blockClose = true; click(c, 'start'); await tick(); c.win.navigate('settings'); close.resolve({ open: false }); await tick(); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(), null);
  assert.equal(calls(c, 'lan.setup.secondary').length, 0);
});

test('no detected monitors stays on an actionable E4 with the required explanation', async t => {
  const c = await setup(t, { displays: [] }); await c.start();
  assert.match(body(c), /无法读取显示器.*请在显示器菜单中开启 DDC\/CI。/s);
  assert.equal(c.win.document.querySelector('[data-pair-action="read-retry"]').disabled, false);
  assert.equal(calls(c, 'lan.pair.start').length, 0);
});

test('runtime state refresh cannot replace the sidebar pairing lifecycle', async t => {
  const c = await setup(t, { state: state([]) }); await waiting(c);
  await emit(c, 'state', { ...state([]), runtime_seq: 999 });
  assert.equal(c.win.document.querySelector('#nav-state').textContent, '等待另一台电脑');
});

test('lan.status waiting updates discover a peer during the search window', async t => {
  const c = await setup(t, { state: state([]) }); await c.start();
  await emit(c, 'lan.status', state([peer()]).lan);
  assert.equal(calls(c, 'lan.pair.start').length, 1);
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'connecting');
});

test('trigger_adopted before B3 refreshes config and avoids redundant USB capture', async t => {
  const c = await setup(t); await ready(c);
  const adopted = require('./client-helper').pairedV2(); c.setConfig(adopted);
  const reads = calls(c, 'config.get').length;
  await emit(c, 'lan.setup', { phase: 'trigger_adopted', source: 'dev-peer' });
  assert.ok(calls(c, 'config.get').length > reads);
  click(c, 'v2-next'); await tick();
  assert.equal(calls(c, 'wizard.begin').length, 0);
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'try');
});

test('unmatched then adopted during B3 stops capture once without exposing missing identifiers', async t => {
  const c = await setup(t); await ready(c);
  const config = require('./client-helper').pairedV2(); config.trigger = null; c.setConfig(config);
  await emit(c, 'lan.setup', { phase: 'trigger_unmatched', source: 'dev-peer', missing: ['1234:5678'] });
  click(c, 'v2-next'); await tick();
  assert.equal(calls(c, 'wizard.begin').length, 1);
  c.setConfig(require('./client-helper').pairedV2());
  await emit(c, 'lan.setup', { phase: 'trigger_adopted', source: 'dev-peer' });
  await emit(c, 'lan.setup', { phase: 'trigger_adopted', source: 'dev-peer' });
  assert.equal(calls(c, 'wizard.end').length, 1);
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'try');
  assert.doesNotMatch(body(c), /1234:5678|missing|unmatched|adopted/);
  const writes = calls(c, 'config.set').length;
  await emit(c, 'wizard.candidates', candidates);
  assert.equal(calls(c, 'config.set').length, writes, 'late capture event cannot overwrite the adopted trigger');
});

test('when every waiting peer leaves B1a, discovery resumes and remains actionable', async t => {
  const c = await setup(t, { state: state([peer(), peer({ device_id: 'second' })]) }); await c.start();
  await emit(c, 'lan.peers', { peers: [] });
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'search');
  await c.fire(10000);
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'wait');
  assert.ok(c.win.document.querySelector('[data-pair-action="ip"]'));
});
