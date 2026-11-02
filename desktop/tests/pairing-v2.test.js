const assert = require('node:assert/strict');
const test = require('node:test');
const { v2, pairedV2 } = require('./client-helper');
const { setup, waiting, ready, state, peer, displays, click, body, emit, profile, tick } = require('./pairing-v2-helper');
const calls = (c, method) => c.requests.filter(r => r.method === method);
const deferred = () => { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; };
const candidates = { disappeared: [{ vid_pid: '1234:5678', product: 'USB Hub' }, { vid_pid: '2345:6789', product: 'Keyboard' }] };
const $ = (c, selector) => c.win.document.querySelector(selector);
const action = (c, name) => $(c, `[data-pair-action="${name}"]`);

test('first run opens full screen on welcome, then appearance, then this computer', async t => {
  for (const config of [null, v2]) {
    const c = await setup(t, { config });
    assert.ok(c.win.document.documentElement.classList.contains('onboarding-active'));
    click(c, 'ob-welcome');
    assert.match(body(c), /KVMFlow.*一套键鼠，两台电脑/s);
    click(c, 'ob-theme');
    assert.match(body(c), /选择外观.*跟随系统.*浅色.*深色/s);
    await c.start();
    assert.equal(c.win.KVMFlowPairing.activeMode(), 'local');
    assert.match(body(c), /检查这台电脑.*请确认键盘和鼠标已连接到 USB 切换器。.*显示器 1.*可切换/s);
    assert.doesNotMatch(body(c), /主机|副机|配对码|手动填写|输入值/);
    assert.equal(action(c, 'local-next').disabled, false);
    c.win.navigate('settings');
    assert.ok(!c.win.document.documentElement.classList.contains('onboarding-active'));
    c.win.navigate('guide');
    assert.match(body(c), /一套键鼠/);
  }
});

test('appearance cards apply and remember the theme', async t => {
  const c = await setup(t);
  $(c, '[data-ob-theme="dark"]').click();
  assert.equal(c.win.document.documentElement.dataset.theme, 'dark');
  assert.equal(c.win.localStorage.getItem('kvmflow-theme-v1'), 'dark');
  assert.equal($(c, '[data-ob-theme="dark"]').getAttribute('aria-checked'), 'true');
  assert.equal($(c, '[data-ob-theme="system"]').getAttribute('aria-checked'), 'false');
  $(c, '[data-ob-theme="light"]').click();
  assert.equal(c.win.document.documentElement.dataset.theme, 'light');
});

test('the language toggle switches every page', async t => {
  const c = await setup(t);
  $(c, '[data-ob-lang="en"]').click(); await tick();
  assert.match(body(c), /Choose a look/);
  $(c, '[data-ob-lang="zh-CN"]').click(); await tick();
  assert.match(body(c), /选择外观/);
});

test('Next saves observed displays, then waits on the connect page until the user pairs', async t => {
  const c = await setup(t, { config: null, computerName: 'LOCAL' }); await c.connect();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'connect');
  const saved = calls(c, 'config.set')[0].params.config;
  assert.equal(saved.local_device.device_id, 'host-win');
  // The app's immediate timing, not the sidecar's 5 s / 10 s defaults: an adopted
  // trigger keeps this computer's debounce.
  assert.deepEqual(JSON.parse(JSON.stringify(saved.trigger)), { anchor: { vid_pid: '' }, members: [], debounce: { t_stable_ms: 0, t_absent_ms: 0, t_cooldown_ms: 1000, quorum_peripherals: 1 } });
  assert.equal(saved.local_device.monitors[0].local_input, 15);
  assert.ok(c.requests.indexOf(calls(c, 'config.set')[0]) < c.requests.indexOf(calls(c, 'lan.setup.secondary')[0]));
  assert.equal(calls(c, 'lan.pair.window').at(-1).params.open, true);
  assert.match(body(c), /连接另一台电脑.*附近的电脑.*GAME.*192\.168\.1\.23.*配对/s);
  assert.equal(calls(c, 'lan.pair.start').length, 0, 'nobody dials without a click');
  await emit(c, 'lan.peers', { peers: [peer()] });
  assert.equal(calls(c, 'lan.pair.start').length, 0);
  await c.pair();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'connecting');
  assert.equal(calls(c, 'lan.pair.start')[0].params.addr, '192.168.1.23:47820');
  assert.equal($(c, '#pairing-dialog').open, false);
  assert.match(body(c), /与 GAME 配对.*本机.*GAME.*正在配对…/s);
});

test('the connect page lists only waiting computers and opens A2 with USB capture at once', async t => {
  const c = await setup(t, { state: state([peer({ waiting: undefined }), peer({ device_id: 'offline', online: false, addr: null })]) }); await c.connect();
  assert.equal(c.win.document.querySelectorAll('[data-v2-peer]').length, 0);
  assert.match(body(c), /正在搜索….*另一台电脑还没打开？.*按 USB 切换器.*切换显示器输入.*在另一台电脑上打开 KVMFlow.*192\.168\.1\.22/s);
  assert.equal(calls(c, 'lan.setup.secondary').length, 1);
  assert.equal(calls(c, 'wizard.begin').length, 1);
  await emit(c, 'wizard.candidates', { disappeared: [{ vid_pid: '2345:6789', product: 'Keyboard' }] });
  assert.equal(calls(c, 'wizard.end').length, 0);
  await emit(c, 'wizard.candidates', candidates);
  assert.equal(calls(c, 'wizard.end').length, 1);
  assert.equal(calls(c, 'config.set').at(-1).params.config.trigger.anchor.vid_pid, '1234:5678');
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'connect');
});

test('each waiting computer has its own Pair button; departures update the list', async t => {
  const c = await setup(t, { state: state([peer(), peer({ device_id: 'dev-two', name: 'TWO', addr: '192.168.1.24:47820' }), peer({ device_id: 'old', waiting: undefined })]) });
  await c.connect();
  assert.equal(c.win.document.querySelectorAll('[data-v2-peer]').length, 2);
  await emit(c, 'lan.peers', { peers: [peer({ device_id: 'dev-two', name: 'TWO', addr: '192.168.1.24:47820' })] });
  assert.equal(c.win.document.querySelectorAll('[data-v2-peer]').length, 1);
  await emit(c, 'lan.peers', { peers: [] });
  assert.match(body(c), /正在搜索…/);
  assert.ok($(c, '[data-ob-ip]'), 'IP connection stays available');
  await emit(c, 'lan.status', state([peer({ device_id: 'dev-two', name: 'TWO', addr: '192.168.1.24:47820' })]).lan);
  await c.pair('dev-two');
  assert.equal(calls(c, 'lan.pair.start').length, 1);
  assert.equal(calls(c, 'lan.pair.start')[0].params.addr, '192.168.1.24:47820');
});

test('both initiator and responder confirm automatically once, including peer-first confirmation', async t => {
  for (const responder of [false, true]) {
    const c = await setup(t, { state: state(responder ? [] : [peer()]) });
    if (responder) await waiting(c); else await c.dial();
    const event = { phase: responder ? 'peer_confirmed' : 'await_confirm', device_id: 'dev-peer', initiator: !responder, session_id: 's1', event_seq: 1, sas: '123 456', peer_confirmed: responder };
    await emit(c, 'lan.pair', event); await emit(c, 'lan.pair', { ...event, event_seq: 2 });
    assert.equal(calls(c, 'lan.pair.confirm').length, 1);
    assert.equal(calls(c, 'lan.pair.confirm')[0].params.sas_ok, true);
    assert.doesNotMatch(body(c), /123|456|一致/);
  }
});

test('manual code check shows the digits and confirms only on the user answer', async t => {
  for (const answer of ['confirm', 'mismatch']) {
    const c = await setup(t); await c.connect();
    const box = $(c, '[data-ob-manual]'); box.checked = true; box.dispatchEvent(new c.win.Event('change', { bubbles: true }));
    await c.pair();
    await emit(c, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer', initiator: true, session_id: 's1', event_seq: 1, sas: '482 913' });
    assert.equal(calls(c, 'lan.pair.confirm').length, 0);
    assert.equal(c.win.document.querySelector('.ob-sas').textContent, '482913');
    assert.match(body(c), /两台电脑显示的数字一致吗？.*不一致.*一致/s);
    click(c, answer); await tick();
    assert.equal(calls(c, 'lan.pair.confirm').length, 1);
    assert.equal(calls(c, 'lan.pair.confirm')[0].params.sas_ok, answer === 'confirm');
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
    else await c.dial();
    await emit(c, 'lan.pair', { phase: 'await_resolve', device_id: 'dev-peer', initiator: !responder, issues });
    assert.equal(calls(c, 'lan.pair.resolve').length, responder ? 0 : 1);
    if (!responder) assert.deepEqual(JSON.parse(JSON.stringify(calls(c, 'lan.pair.resolve')[0].params)), { mixed: 'joiner', monitor_aliases: { l: false }, replacement: { old_device_id: 'old', replace: false } });
    assert.equal(c.win.document.querySelectorAll('.pair-option').length, 0);
  }
});

test('IP connection validates the address, keeps its draft and returns to the connect page on Back', async t => {
  const c = await setup(t, { state: state([]) }); await waiting(c);
  $(c, '[data-ob-ip]').click();
  assert.ok($(c, '.ob-ipwrap').classList.contains('open'));
  const form = $(c, '.v2-ip-form'); form.elements.addr.value = '192.168.1';
  form.dispatchEvent(new c.win.Event('submit', { bubbles: true, cancelable: true })); await tick();
  assert.equal($(c, '[data-region="ip-error"]').textContent, 'IP 地址无效');
  assert.equal(calls(c, 'lan.connect.manual').length, 0);
  form.elements.addr.value = '192.168.1.50';
  await emit(c, 'lan.peers', { peers: [peer()] });
  assert.equal($(c, '.v2-ip-form').elements.addr.value, '192.168.1.50');
  assert.ok($(c, '.ob-ipwrap').classList.contains('open'), 'discovery keeps the drawer open');
  c.win.KVMFlowI18n.setPreference('en'); await tick();
  assert.equal($(c, '.v2-ip-form').elements.addr.value, '192.168.1.50', 'language keeps the draft');
  c.win.KVMFlowI18n.setPreference('zh-CN'); await tick();
  $(c, '.v2-ip-form').dispatchEvent(new c.win.Event('submit', { bubbles: true, cancelable: true })); await tick();
  assert.equal(calls(c, 'lan.connect.manual').at(-1).params.pair, true);
  assert.equal(calls(c, 'lan.connect.manual').at(-1).params.addr, '192.168.1.50');
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'connecting');
  click(c, 'v2-cancel'); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'connect');
  assert.equal(calls(c, 'lan.pair.window').at(-1).params.open, true);
});

test('E2 retries pairing with a new session and ignores retired events', async t => {
  const c = await setup(t); await c.dial();
  await emit(c, 'lan.pair', { phase: 'failed', device_id: 'dev-peer', session_id: 'old', event_seq: 1, code: 'E_LAN_CLOSED' });
  assert.match(body(c), /与 GAME 配对.*配对失败.*连接已中断。请检查防火墙是否允许 KVMFlow 访问网络。/s);
  click(c, 'v2-retry'); await tick();
  assert.equal(calls(c, 'lan.pair.start').length, 2);
  await emit(c, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer', session_id: 'new', event_seq: 1 });
  await emit(c, 'lan.pair', { phase: 'failed', device_id: 'dev-peer', session_id: 'old', event_seq: 99 });
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'connecting');
});

test('E4 skips the chosen unreadable display, retries reads and never invents input values', async t => {
  let available = false;
  const c = await setup(t, { request: async method => method === 'display.list' ? { displays: displays.map(d => ({ ...d, ddc: available ? d.ddc : { state: 'unavailable' } })) } : undefined });
  await c.start(); assert.equal(c.win.KVMFlowPairing.activeMode(), 'local');
  assert.equal(action(c, 'local-next').disabled, true);
  assert.match(body(c), /无法读取.*扩展坞/s);
  click(c, 'read-skip', '[data-fp="display-a"]'); await tick();
  // The skipped row slides out before the list is redrawn.
  assert.ok($(c, '[data-ob-row="display-a"]').classList.contains('ob-gone'));
  await new Promise(resolve => setTimeout(resolve, 150));
  assert.equal(c.win.document.querySelectorAll('[data-ob-row]').length, 1);
  assert.ok($(c, '[data-ob-row="display-b"]'));
  assert.equal(calls(c, 'lan.pair.start').length, 0);
  available = true; click(c, 'read-retry'); await tick();
  assert.equal(action(c, 'local-next').disabled, false);
  await c.next();
  const saved = calls(c, 'config.set').at(-1).params.config;
  assert.equal(saved.local_device.monitors.length, 1);
  assert.equal(saved.local_device.monitors[0].fingerprint, 'display-b');
  assert.doesNotMatch(body(c), /0x0F|输入值/);
});

test('skipping every display leaves an empty list that cannot continue', async t => {
  const c = await setup(t, { displays: displays.map(d => ({ ...d, ddc: { state: 'unavailable' } })) });
  await c.start();
  click(c, 'read-skip', '[data-fp="display-a"]'); await tick();
  click(c, 'read-skip', '[data-fp="display-b"]'); await tick();
  await new Promise(resolve => setTimeout(resolve, 150));
  assert.match(body(c), /没有可切换的显示器/);
  assert.match(body(c), /请把显示器直连电脑，不要经过扩展坞/);
  assert.equal(action(c, 'local-next').disabled, true);
  assert.equal(action(c, 'read-retry').disabled, false);
  click(c, 'read-retry'); await tick();
  assert.match(body(c), /没有可切换的显示器/, 'skipped displays stay skipped');
});

test('B2 waits for the selected owner, pending inputs and committed topology before testing', async t => {
  const c = await setup(t); await c.dial();
  await emit(c, 'lan.pair', { phase: 'committed', device_id: 'dev-peer', config_merged: false });
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'confirm');
  assert.match(body(c), /已配对/);
  assert.equal(c.pending(900), false);
  await emit(c, 'lan.profile', { ...profile(false), device_id: 'foreign' });
  assert.match(body(c), /读取另一台电脑的显示器/);
  await emit(c, 'lan.profile', profile(true));
  assert.match(body(c), /请用显示器按键切换到本机。/);
  assert.equal(c.pending(900), false);
  await emit(c, 'lan.profile', profile(false, 2));
  assert.equal(c.pending(900), false);
  c.setConfig(pairedV2()); await emit(c, 'lan.setup', { phase: 'topology_ready' });
  assert.equal(c.pending(900), true);
  await emit(c, 'lan.profile', profile(true, 1));
  await c.advance();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'try');
  assert.doesNotMatch(body(c), /输入值|0x|电脑名称|端口/);
});

test('B3 without a trigger identifies the switch first, then hints and starts the bounded test', async t => {
  const c = await setup(t, { request: async method => method === 'lan.test.start' ? { test_id: 'test-1' } : undefined }); await ready(c);
  const config = pairedV2(); config.trigger = null; c.setConfig(config);
  const begins = calls(c, 'wizard.begin').length, ends = calls(c, 'wizard.end').length;
  await c.advance();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'try');
  // The sidecar refuses a test without a trigger; identify from this press first.
  assert.equal(calls(c, 'lan.test.start').length, 0);
  assert.equal(calls(c, 'wizard.begin').length, begins + 1);
  await emit(c, 'wizard.candidates', candidates);
  assert.equal(calls(c, 'wizard.end').length, ends + 1);
  // The identifying press could not move the monitors: hint at once.
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'try-hint');
  assert.equal(calls(c, 'lan.test.start')[0].params.duration_ms, 120000);
  assert.match(body(c), /测试切换.*如果显示器未切换，请手动切换到 GAME。/s);
  c.win.navigate('settings'); await tick();
  assert.equal(calls(c, 'lan.test.stop').length, 1);
});

test('B3 skip and navigation both stop the test once; command acceptance cannot finish the flow', async t => {
  for (const leave of ['skip', 'navigate']) {
    const c = await setup(t, { request: async method => method === 'lan.test.start' ? { test_id: 'new' } : undefined }); await ready(c); await c.advance();
    await emit(c, 'switch.report', { test_id: 'old', press_id: 'p', reporting_device_id: 'host-win', target_device_id: 'dev-peer', per_monitor: [{ commanded: true }] });
    await emit(c, 'switch.report', { test_id: 'new', press_id: 'p', reporting_device_id: 'host-win', target_device_id: 'dev-peer', per_monitor: [{ commanded: true }] });
    assert.equal(c.win.KVMFlowPairing.activeMode(), 'try');
    assert.doesNotMatch(body(c), /成功|画面已到达/);
    if (leave === 'skip') { click(c, 'v2-skip'); await tick(); assert.equal(c.win.KVMFlowPairing.activeMode(), 'done'); }
    else c.win.navigate('settings');
    await tick(); c.win.navigate('status'); await tick();
    assert.equal(calls(c, 'lan.test.stop').length, 1);
  }
});

test('the other computer reporting the arrival shows a successful switch, then done', async t => {
  const c = await setup(t, { request: async method => method === 'lan.test.start' ? { test_id: 'new' } : undefined }); await ready(c); await c.advance();
  const arrival = { test_id: 'new', press_id: 'host-win:1', reporting_device_id: 'dev-peer', target_device_id: 'dev-peer', test_kind: 'picture_arrived' };
  await emit(c, 'lan.test', { test_id: 'new', events: [{ device_id: 'foreign', kind: 'test.event', body: { ...arrival, reporting_device_id: 'foreign' } }] });
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'try', 'only the peer can report the arrival');
  await emit(c, 'lan.test', { test_id: 'new', events: [{ device_id: 'dev-peer', kind: 'test.event', body: arrival }] });
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'try-ok');
  assert.match(body(c), /切换成功/);
  await c.fire(1100);
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'done');
  assert.equal(calls(c, 'lan.test.stop').length, 1);
});

test('this computer failing to switch a monitor during the test shows which one', async t => {
  const c = await setup(t, { request: async method => method === 'lan.test.start' ? { test_id: 'new' } : undefined }); await ready(c); await c.advance();
  await emit(c, 'switch.report', { test_id: 'new', press_id: 'host-win:1', reporting_device_id: 'host-win', target_device_id: 'dev-peer', test_kind: 'commands_sent', per_monitor: [{ edid_id: 'display-a', commanded: true }, { edid_id: 'display-b', commanded: false }] });
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'test-error');
  assert.match(body(c), /显示器未切换/);
});

test('waiting computer completes A3 and shows E3 only for a peer test targeting this computer', async t => {
  const c = await setup(t, { state: state([]) }); await waiting(c);
  await emit(c, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer', initiator: false });
  c.setConfig(pairedV2());
  await emit(c, 'lan.pair', { phase: 'committed', device_id: 'dev-peer', initiator: false, config_merged: true });
  assert.match(body(c), /已配对/);
  await c.advance();
  assert.match(body(c), /两台电脑已配对完成/);
  const report = { test_id: 'remote', press_id: 'p', reporting_device_id: 'dev-peer', target_device_id: 'host-win', per_monitor: [{ edid_id: 'display-b', commanded: false }] };
  await emit(c, 'switch.report', { ...report, reporting_device_id: 'foreign' }); assert.equal(c.win.KVMFlowPairing.activeMode(), 'done');
  await emit(c, 'lan.test', { test_id: 'remote', events: [{ device_id: 'dev-peer', kind: 'switch.report', body: report }] });
  assert.match(body(c), /显示器未切换.*未响应切换指令。请在显示器菜单中开启 DDC\/CI。/s);
  click(c, 'test-retry'); await tick(); assert.equal(c.win.KVMFlowPairing.activeMode(), 'try');
  assert.equal(calls(c, 'lan.test.start').length, 1);
});

test('the waiting computer keeps the name the dialing computer sent, never its device id', async t => {
  const c = await setup(t, { state: state([peer({ name: '' })]) }); await waiting(c);
  await emit(c, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer', initiator: false, peer_name: 'PC B', session_id: 's1', event_seq: 1 });
  await emit(c, 'lan.pair', { phase: 'await_peer_confirm', device_id: 'dev-peer', initiator: false, session_id: 's1', event_seq: 2 });
  assert.match(body(c), /与 PC B 配对/);
  assert.doesNotMatch(body(c), /dev-peer/);
});
test('leaving every active page cancels pairing, ends capture and closes admission; late setup stays closed', async t => {
  const late = deferred();
  const c = await setup(t, { state: state([]), request: async method => method === 'lan.setup.secondary' ? late.promise : undefined });
  await c.connect(); await emit(c, 'client.hidden', {});
  late.resolve({ secondary: true }); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(), null);
  assert.equal(calls(c, 'lan.pair.window').at(-1).params.open, false);
  assert.equal(calls(c, 'wizard.end').length, calls(c, 'wizard.begin').length, 'every capture ended');
  const d = await setup(t); await d.dial(); d.win.navigate('settings'); await tick();
  assert.equal(calls(d, 'lan.pair.cancel').length, 1);
  assert.equal(calls(d, 'lan.pair.window').at(-1).params.open, false);
  await emit(d, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer' });
  assert.equal(calls(d, 'lan.pair.confirm').length, 0);
});

test('exit leaves setup from any page and cleans up', async t => {
  const c = await setup(t); await c.connect();
  click(c, 'ob-exit'); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(), null);
  assert.equal(c.win.document.querySelector('#status-page').classList.contains('hidden'), false);
  assert.equal(calls(c, 'lan.pair.window').at(-1).params.open, false);
  assert.equal(calls(c, 'wizard.end').length, calls(c, 'wizard.begin').length);
});

test('names are escaped on the connect and pair pages', async t => {
  const payload = '<button data-pair-action=v2-finish>fake</button>';
  const c = await setup(t, { state: state([peer({ name: payload })]) }); await c.connect();
  assert.equal(c.win.document.querySelectorAll('.ob-devs b button').length, 0);
  assert.match(body(c), /<button/);
  c.win.KVMFlowI18n.setPreference('en'); await tick(); assert.match(body(c), /Connect another computer/);
  await c.pair();
  assert.equal(c.win.document.querySelectorAll('.ob-pc b button, h1 button').length, 0);
  assert.equal(calls(c, 'lan.pair.start').length, 1);
  assert.equal(c.win.document.querySelectorAll('[onclick]').length, 0);
});

test('final Chinese copy is exact on the done page, including button labels', async t => {
  const c = await setup(t); await c.start();
  assert.equal(action(c, 'local-next').textContent.trim(), '下一步');
  const d = await setup(t, { state: state([]) }); await waiting(d);
  await emit(d, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer', initiator: false });
  d.setConfig(pairedV2());
  await emit(d, 'lan.pair', { phase: 'committed', device_id: 'dev-peer', initiator: false, config_merged: true });
  await d.advance();
  assert.equal(d.win.document.querySelector('#guide-content h1').textContent, '两台电脑已配对完成');
  assert.equal(action(d, 'v2-finish').textContent, '完成');
});

test('true default choices require no input and are sent once', async t => {
  const c = await setup(t); await c.dial();
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
  const config = pairedV2(); config.trigger = {};
  const begins = calls(c, 'wizard.begin').length;
  c.setConfig(config); await c.advance();
  assert.equal(calls(c, 'wizard.begin').length, begins + 1);
  await emit(c, 'wizard.candidates', candidates);
  const saved = calls(c, 'config.set').at(-1).params.config;
  assert.ok(saved.switch_group.devices.some(d => d.device_id === 'dev-peer'));
  assert.equal(saved.trigger.anchor.vid_pid, '1234:5678');
});

test('leaving during connect cleanup never opens a new window', async t => {
  const close = deferred(); let blockClose = false;
  const c = await setup(t, { state: state([]), request: async (method, params) => method === 'lan.pair.window' && !params.open && blockClose ? close.promise : undefined });
  await c.start(); blockClose = true; click(c, 'local-next'); await tick();
  c.win.navigate('settings'); close.resolve({ open: false }); await tick(); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(), null);
  assert.equal(calls(c, 'lan.setup.secondary').length, 0);
});

test('no detected monitors stays on an actionable empty list with the required explanation', async t => {
  const c = await setup(t, { displays: [] }); await c.start();
  assert.match(body(c), /没有可切换的显示器/);
  assert.match(body(c), /请把显示器直连电脑/);
  assert.equal(action(c, 'read-retry').disabled, false);
  assert.equal(action(c, 'local-next').disabled, true);
  assert.equal(calls(c, 'lan.pair.start').length, 0);
});

test('runtime state refresh cannot replace the sidebar pairing lifecycle', async t => {
  const c = await setup(t, { state: state([]) }); await waiting(c);
  await emit(c, 'state', { ...state([]), runtime_seq: 999 });
  assert.equal(c.win.document.querySelector('#nav-state').textContent, '等待另一台电脑');
});

test('trigger_adopted before B3 refreshes config and avoids redundant USB capture', async t => {
  const c = await setup(t); await ready(c);
  const adopted = pairedV2(); c.setConfig(adopted);
  const reads = calls(c, 'config.get').length;
  await emit(c, 'lan.setup', { phase: 'trigger_adopted', source: 'dev-peer' });
  assert.ok(calls(c, 'config.get').length > reads);
  const begins = calls(c, 'wizard.begin').length;
  await c.advance();
  assert.equal(calls(c, 'wizard.begin').length, begins);
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'try');
});

test('unmatched then adopted during B3 stops capture once without exposing missing identifiers', async t => {
  const c = await setup(t); await ready(c);
  const config = pairedV2(); config.trigger = null; c.setConfig(config);
  await emit(c, 'lan.setup', { phase: 'trigger_unmatched', source: 'dev-peer', missing: ['1234:5678'] });
  const begins = calls(c, 'wizard.begin').length, ends = calls(c, 'wizard.end').length;
  await c.advance();
  assert.equal(calls(c, 'wizard.begin').length, begins + 1);
  c.setConfig(pairedV2());
  await emit(c, 'lan.setup', { phase: 'trigger_adopted', source: 'dev-peer' });
  await emit(c, 'lan.setup', { phase: 'trigger_adopted', source: 'dev-peer' });
  assert.equal(calls(c, 'wizard.end').length, ends + 1);
  assert.equal(calls(c, 'lan.test.start').length, 1, 'the adopted switch starts the test');
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'try');
  assert.doesNotMatch(body(c), /1234:5678|missing|unmatched|adopted/);
  const writes = calls(c, 'config.set').length;
  await emit(c, 'wizard.candidates', candidates);
  assert.equal(calls(c, 'config.set').length, writes, 'late capture event cannot overwrite the adopted trigger');
});

test('a first profile without an identified USB switch saves the empty trigger, never null', async t => {
  const c = await setup(t, { config: null, state: state([]) });
  await c.connect();
  const saved = c.requests.filter(r => r.method === 'config.set').at(-1);
  assert.ok(saved, 'local profile saved before discovery');
  // The sidecar's TriggerConfig is a struct: null fails the whole save.
  assert.deepEqual(JSON.parse(JSON.stringify(saved.params.config.trigger)), { anchor: { vid_pid: '' }, members: [], debounce: { t_stable_ms: 0, t_absent_ms: 0, t_cooldown_ms: 1000, quorum_peripherals: 1 } });
});

test('a failed setup step records its error code in the diagnostics events', async t => {
  const c = await setup(t, { config: null, state: state([]), request: async method => {
    if (method === 'config.set') throw Object.assign(new Error('schema v2 config parse'), { code: 'E_CONFIG_INVALID' });
  } });
  await c.connect();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'error');
  const events = JSON.parse(c.win.localStorage.getItem('kvmflow-local-prototype-v2')).events;
  assert.ok(events.some(e => /E_CONFIG_INVALID/.test(e.copy)), JSON.stringify(events));
});

for (const [code, title, copy] of [
  ['E_CONFIG_INVALID', /无法保存本机配置/, /本机配置未能保存/],
  ['E_LAN_PAIR_TIMEOUT', /配对失败/, /等待超时/],
  ['E_LAN_IO', /配对失败/, /防火墙/],
  ['E_SOMETHING_NEW', /配对失败/, /E_SOMETHING_NEW/],
]) test(`the setup error page explains ${code} instead of one generic message`, async t => {
  const c = await setup(t, { config: null, state: state([]), request: async method => {
    if (method === 'config.set') throw Object.assign(new Error('failed'), { code });
  } });
  await c.connect();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'error');
  const page = body(c);
  assert.match(page, title); assert.match(page, copy);
  if (code !== 'E_LAN_IO') assert.doesNotMatch(page, /防火墙/);
});

test('an uncoded setup failure is not reported as a network problem', async t => {
  const c = await setup(t, { config: null, state: state([]), request: async method => {
    if (method === 'config.set') throw new Error('boom');
  } });
  await c.connect();
  assert.doesNotMatch(body(c), /防火墙/);
  assert.match(body(c), /E_UNKNOWN/);
});

test('a switch press before the connect page is learned once and kept', async t => {
  // Field report: users press the USB switch right after Start.
  const c = await setup(t, { state: state([]) }); await c.start();
  assert.equal(calls(c, 'wizard.begin').length, 1, 'capture starts with Start');
  await emit(c, 'wizard.candidates', candidates);
  await c.next();
  assert.equal(calls(c, 'config.set').at(-1).params.config.trigger.anchor.vid_pid, '1234:5678');
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'connect');
  assert.equal(calls(c, 'wizard.begin').length, 1, 'an identified switch is not identified again');
});

test('a switch identified before the first profile save is kept and saved with it', async t => {
  const list = deferred();
  const c = await setup(t, { config: null, state: state([]), request: async method => method === 'display.list' ? list.promise : undefined });
  click(c, 'start'); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'detect');
  assert.match(body(c), /读取中/);
  await emit(c, 'wizard.candidates', candidates);
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'detect', 'no profile yet is not a failure');
  list.resolve({ displays }); await tick(); await tick();
  await c.next();
  assert.equal(calls(c, 'config.set').at(-1).params.config.trigger.anchor.vid_pid, '1234:5678');
});

test('the dialing computer discards what it captured since Start', async t => {
  const c = await setup(t); await c.dial();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'connecting');
  assert.equal(calls(c, 'wizard.end').length, calls(c, 'wizard.begin').length);
});

test('settings entry points rerun setup from this computer', async t => {
  const c = await setup(t);
  c.win.navigate('settings');
  c.win.KVMFlowPairing.startPrimaryAdd(); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'local');
  assert.match(body(c), /检查这台电脑/);
});

test('setup can skip pairing while connecting and after a failure', async t => {
  const c = await setup(t, { config: null });
  await c.dial();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'connecting');
  assert.match(body(c), /跳过/);
  click(c, 'ob-exit', '.ob-ghost'); await tick(); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(), null);
  assert.ok(!$(c, '#status-page').classList.contains('hidden'));
  assert.ok(calls(c, 'lan.pair.cancel').length || calls(c, 'lan.pair.window').at(-1)?.params.open === false, 'the pairing attempt ends');

  const failed = await setup(t, { config: null, state: state([]), request: async method => {
    if (method === 'config.set') throw Object.assign(new Error('failed'), { code: 'E_CONFIG_INVALID' });
  } });
  await failed.connect();
  assert.equal(failed.win.KVMFlowPairing.activeMode(), 'error');
  click(failed, 'ob-exit', '.ob-ghost'); await tick(); await tick();
  assert.equal(failed.win.KVMFlowPairing.activeMode(), null);
});

test('after skipping pairing the app reads as unpaired, not as unfinished setup', async t => {
  const c = await setup(t, { config: null, state: state([]) });
  await c.connect();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'connect');
  click(c, 'ob-exit', '.ob-ghost'); await tick(); await tick();
  const status = $(c, '#status-page').textContent;
  assert.match(status, /还没有配对其他电脑.*添加电脑/s);
  assert.doesNotMatch(status, /尚未完成初始化|开始初始化|等待 USB Switch/);
  assert.equal($(c, '#nav-state').textContent, '未配对');
  $(c, '[data-action="beginSetup"]').click(); await tick();
  assert.ok(!$(c, '#add-page').classList.contains('hidden'));
});

test('the add page leaves pairing with back, so it offers no skip', async t => {
  const c = await setup(t, { config: v2, state: state() });
  c.win.navigate('add'); await tick(); await tick();
  await c.pair(); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'connecting');
  assert.equal($(c, '.ob-ghost[data-pair-action="ob-exit"]'), null);
  assert.ok($(c, '[data-pair-action="v2-cancel"]'));
});

test('a failed pairing in setup can be skipped too', async t => {
  const c = await setup(t, { config: null });
  await c.dial();
  await emit(c, 'lan.pair', { phase: 'failed', device_id: 'dev-peer', code: 'E_LAN_PAIR_TIMEOUT' }); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'error');
  assert.ok($(c, '[data-pair-action="v2-retry"]'));
  click(c, 'ob-exit', '.ob-ghost'); await tick(); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(), null);
  assert.equal($(c, '#nav-state').textContent, '未配对');
});
