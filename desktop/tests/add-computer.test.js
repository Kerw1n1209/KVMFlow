const assert = require('node:assert/strict');
const test = require('node:test');
const { client, tick } = require('./client-helper');
const { state, peer, displays, emit } = require('./pairing-v2-helper');
const calls = (c, method) => c.requests.filter(r => r.method === method);
const $ = (c, selector) => c.win.document.querySelector(selector);
const page = c => $(c, '#add-content').textContent;
const fullScreen = c => c.win.document.documentElement.classList.contains('onboarding-active');
async function open(t, options = {}) {
  const c = await client(t, { state: state(), displays, ...options });
  c.win.navigate('add'); await tick(); await tick();
  return c;
}

test('the sidebar adds computers instead of rerunning setup', async t => {
  const c = await client(t, { state: state() });
  assert.ok($(c, '.nav-item[data-page="add"]'));
  assert.equal($(c, '.nav-item[data-page="guide"]'), null);
  assert.match($(c, '.nav-item[data-page="add"]').textContent, /添加电脑/);
});

test('the add page makes this computer findable and lists waiting computers', async t => {
  const c = await open(t, { state: state([peer({ platform: 'windows' }), peer({ device_id: 'dev-idle', name: 'IDLE', waiting: undefined })]) });
  assert.ok(!fullScreen(c), 'the sidebar stays visible');
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'connect');
  assert.equal(calls(c, 'lan.setup.secondary').length, 1);
  assert.equal(calls(c, 'lan.pair.window').at(-1).params.open, true);
  assert.equal(calls(c, 'wizard.begin').length, 0, 'no USB capture outside setup');
  assert.equal(c.win.document.querySelectorAll('#add-content [data-v2-peer]').length, 1);
  assert.match(page(c), /附近的电脑.*GAME.*Windows · 192\.168\.1\.23.*连接.*按 IP 地址连接.*这台电脑.*192\.168\.1\.22.*手动核对配对码/s);
  await emit(c, 'lan.peers', { peers: [] });
  assert.match(page(c), /正在搜索….*点「添加电脑」/s);
});

test('connecting from the add page pairs full screen and Back returns to the add page', async t => {
  const c = await open(t);
  $(c, '#add-content [data-v2-peer="dev-peer"]').click(); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'connecting');
  assert.equal(calls(c, 'lan.pair.start')[0].params.addr, '192.168.1.23:47820');
  assert.ok(fullScreen(c));
  assert.match($(c, '#guide-content').textContent, /与 GAME 配对/);
  assert.equal($(c, '#guide-content .ob-pg i'), null, 'no setup step dots');
  $(c, '#guide-content [data-pair-action="v2-cancel"]').click(); await tick(); await tick();
  assert.ok(!fullScreen(c));
  assert.ok(!$(c, '#add-page').classList.contains('hidden'));
  assert.equal(calls(c, 'lan.pair.cancel').length, 1);
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'connect');
  assert.equal(calls(c, 'lan.pair.window').at(-1).params.open, true);
});

test('an incoming pairing on the add page continues full screen', async t => {
  const c = await open(t);
  await emit(c, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer', initiator: false, session_id: 's1', event_seq: 1, sas: '123 456' });
  assert.ok(fullScreen(c));
  assert.equal(calls(c, 'lan.pair.confirm').length, 1);
});

test('IP connection on the add page validates and dials the address', async t => {
  const c = await open(t);
  const form = $(c, '#add-content .add-ip'), input = form.elements.addr;
  input.value = '300.1.1.1'; form.requestSubmit(); await tick();
  assert.equal($(c, '#add-iperr').textContent, 'IP 地址无效');
  assert.equal(calls(c, 'lan.connect.manual').length, 0);
  input.value = '192.168.1.50'; form.requestSubmit(); await tick();
  assert.deepEqual(JSON.parse(JSON.stringify(calls(c, 'lan.connect.manual')[0].params)), { addr: '192.168.1.50', pair: true });
  assert.ok(fullScreen(c));
});

test('refresh re-announces discovery', async t => {
  const c = await open(t);
  $(c, '[data-pair-action="add-refresh"]').click(); await tick();
  assert.equal(calls(c, 'lan.discover.start').length, 1);
  assert.ok($(c, '.add-refresh').classList.contains('spin'));
});

test('leaving the add page closes admission', async t => {
  const c = await open(t);
  c.win.navigate('settings'); await tick();
  assert.equal(calls(c, 'lan.pair.window').at(-1).params.open, false);
  assert.equal(c.win.KVMFlowPairing.activeMode(), null);
});

test('a computer without its monitors checked goes through the check, then back to the add page', async t => {
  const c = await open(t, { config: null });
  assert.match(page(c), /先检查这台电脑的显示器/);
  assert.equal(calls(c, 'lan.setup.secondary').length, 0);
  $(c, '[data-pair-action="add-local"]').click(); await tick(); await tick();
  assert.ok(fullScreen(c));
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'local');
  $(c, '[data-pair-action="local-next"]').click(); await tick(); await tick();
  assert.ok(!fullScreen(c));
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'connect');
  assert.equal(calls(c, 'config.set').at(-1).params.config.local_device.monitors.length, 2);
  assert.equal(calls(c, 'lan.setup.secondary').length, 1);
  assert.match(page(c), /GAME/);
});

test('a failure while getting ready shows a retry on the add page', async t => {
  let fail = true;
  const c = await open(t, { request: async method => { if (method === 'lan.setup.secondary' && fail) throw { code: 'E_LAN_BIND', message: 'bind' }; } });
  assert.match(page(c), /重试/);
  assert.equal(calls(c, 'lan.pair.window').at(-1)?.params.open ?? false, false);
  fail = false;
  $(c, '[data-pair-action="add-retry"]').click(); await tick(); await tick();
  assert.match(page(c), /GAME/);
});

test('setup prompts and the settings add button open the add page', async t => {
  const c = await client(t, { state: state(), displays, config: null, initialPage: 'status' });
  $(c, '[data-action="beginSetup"]').click(); await tick();
  assert.ok(!$(c, '#add-page').classList.contains('hidden'));
  c.win.navigate('settings');
  $(c, '#add-computer').click(); await tick();
  assert.ok(!$(c, '#add-page').classList.contains('hidden'));
});

test('pairing from the settings computer list runs through the add page', async t => {
  const c = await client(t, { state: state() });
  c.win.KVMFlowPairing.addWith('dev-peer'); await tick(); await tick();
  assert.equal(calls(c, 'lan.setup.secondary').length, 1);
  assert.equal(calls(c, 'lan.pair.start')[0].params.addr, '192.168.1.23:47820');
  assert.ok(fullScreen(c));
  assert.equal($(c, '#pairing-dialog').open, false);
});

test('diagnostics can show the welcome flow again', async t => {
  const c = await client(t, { state: state() });
  c.win.navigate('diagnostics');
  $(c, '#show-welcome').click(); await tick();
  assert.ok(fullScreen(c));
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'welcome');
  assert.match($(c, '#guide-content').textContent, /KVMFlow/);
});
