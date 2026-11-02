const assert = require('node:assert/strict');
const test = require('node:test');
const { client, tick } = require('./client-helper');
const { state, peer, displays, emit } = require('./pairing-v2-helper');
const calls = (c, method) => c.requests.filter(r => r.method === method);
const $ = (c, selector) => c.win.document.querySelector(selector);
const fullScreen = c => c.win.document.documentElement.classList.contains('onboarding-active');
const sidebar = c => $(c, '#pair-requests');
const request = (over = {}) => ({ phase: 'await_confirm', request: true, device_id: 'dev-peer', initiator: false, local_confirmed: false,
  session_id: 'req-1', event_seq: 1, sas: '123 456', peer_name: 'GAME', ...over });
const withLan = extra => { const s = state(); s.lan = { ...s.lan, pair_requests: true, ...extra }; return s; };
async function settle() { for (let i = 0; i < 4; i += 1) await tick(); }

test('a request while no pairing screen is open waits in the sidebar', async t => {
  const c = await client(t, { state: withLan(), displays });
  assert.equal(sidebar(c).hidden, true);
  await emit(c, 'lan.pair', request());
  assert.equal(sidebar(c).hidden, false);
  assert.match(sidebar(c).textContent, /配对请求.*GAME.*想与这台电脑配对.*拒绝.*接受/s);
  assert.equal(calls(c, 'lan.pair.confirm').length, 0, 'nothing is confirmed until the user accepts');
  assert.ok(!fullScreen(c));
});

test('accepting a request pairs full screen', async t => {
  const c = await client(t, { state: withLan(), displays });
  await emit(c, 'lan.pair', request());
  $(c, '[data-request-action="accept"]').click(); await settle();
  assert.equal(sidebar(c).hidden, true);
  assert.ok(fullScreen(c));
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'connecting');
  assert.equal(calls(c, 'lan.pair.confirm').length, 1);
  assert.match($(c, '#guide-content').textContent, /与 GAME 配对/);
});

test('declining a request tells the sidecar and clears the sidebar', async t => {
  const c = await client(t, { state: withLan(), displays });
  await emit(c, 'lan.pair', request());
  $(c, '[data-request-action="decline"]').click(); await settle();
  assert.equal(sidebar(c).hidden, true);
  assert.deepEqual(JSON.parse(JSON.stringify(calls(c, 'lan.pair.decline')[0].params)), { device_id: 'dev-peer' });
  assert.equal(calls(c, 'lan.pair.confirm').length, 0);
  // The sidecar's own cancellation of the declined request changes nothing.
  await emit(c, 'lan.pair', request({ phase: 'cancelled', code: 'E_LAN_PAIR_DECLINED', event_seq: 2 }));
  assert.equal(sidebar(c).hidden, true);
  assert.ok(!fullScreen(c));
});

test('a request that times out or is withdrawn leaves the sidebar', async t => {
  const c = await client(t, { state: withLan(), displays });
  await emit(c, 'lan.pair', request());
  await emit(c, 'lan.pair', request({ phase: 'failed', code: 'E_LAN_PAIR_TIMEOUT', event_seq: 2 }));
  assert.equal(sidebar(c).hidden, true);
  // A late event of the ended session does not bring it back.
  await emit(c, 'lan.pair', request({ event_seq: 3 }));
  assert.equal(sidebar(c).hidden, true);
});

test('an unanswered request survives a window reload through lan state', async t => {
  const pending = { device_id: 'dev-peer', peer_name: 'GAME', sas: '123 456', session_id: 'req-1', expires_in_ms: 90000 };
  const c = await client(t, { state: withLan({ pair_request: pending }), displays });
  await settle();
  assert.equal(sidebar(c).hidden, false);
  assert.match(sidebar(c).textContent, /GAME/);
});

test('opening the add page accepts a waiting request', async t => {
  const c = await client(t, { state: withLan(), displays });
  await emit(c, 'lan.pair', request());
  c.win.navigate('add'); await settle();
  assert.equal(sidebar(c).hidden, true);
  assert.equal(calls(c, 'lan.pair.window').at(-1).params.open, true);
  assert.equal(calls(c, 'lan.pair.confirm').length, 1);
  assert.ok(fullScreen(c));
});

test('a request that arrives on the add page is accepted like any inbound attempt', async t => {
  const c = await client(t, { state: withLan(), displays });
  c.win.navigate('add'); await settle();
  await emit(c, 'lan.pair', request()); await settle();
  assert.equal(sidebar(c).hidden, true);
  assert.equal(calls(c, 'lan.pair.confirm').length, 1);
  assert.ok(fullScreen(c));
});

test('the add page lists computers that take requests, waiting ones first', async t => {
  const peers = [
    peer({ device_id: 'dev-new', name: 'NEW', waiting: false, requests: true, addr: '192.168.1.30:47820' }),
    peer({ device_id: 'dev-old', name: 'OLD', waiting: false, addr: '192.168.1.31:47820' }),
    peer({ device_id: 'dev-off', name: 'OFF', waiting: false, requests: false, addr: '192.168.1.32:47820' }),
    peer({ device_id: 'dev-wait', name: 'WAIT', waiting: true, requests: false, addr: '192.168.1.33:47820' }),
  ];
  const c = await client(t, { state: { ...withLan(), lan: { ...withLan().lan, peers } }, displays });
  c.win.navigate('add'); await settle();
  const rows = [...c.win.document.querySelectorAll('#add-content [data-v2-peer]')].map(el => el.dataset.v2Peer);
  assert.deepEqual(rows, ['dev-wait', 'dev-new']);
});

test('dialing a computer off its add page asks the user there to accept', async t => {
  const peers = [peer({ waiting: false, requests: true })];
  const c = await client(t, { state: { ...withLan(), lan: { ...withLan().lan, peers } }, displays });
  c.win.navigate('add'); await settle();
  $(c, '#add-content [data-v2-peer="dev-peer"]').click(); await settle();
  assert.equal(calls(c, 'lan.pair.start')[0].params.addr, '192.168.1.23:47820');
  await emit(c, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer', initiator: true, event_seq: 1, sas: '123 456', local_confirmed: false });
  await settle();
  await emit(c, 'lan.pair', { phase: 'await_confirm', device_id: 'dev-peer', initiator: true, event_seq: 2, sas: '123 456', local_confirmed: true, peer_confirmed: false });
  assert.match($(c, '#guide-content').textContent, /请在 GAME 上点「接受」/);
});

for (const [name, over, copy] of [
  ['needs an update', {}, /需要升级/],
  ['declined', { requests: true }, /拒绝了配对请求/],
  ['turned requests off', { requests: false }, /关闭了配对请求/],
]) {
  test(`a refused dial explains that the other computer ${name}`, async t => {
    // Listed while waiting; its pairing screen closed before the dial arrived.
    const c = await client(t, { state: { ...withLan(), lan: { ...withLan().lan, peers: [peer(over)] } }, displays });
    c.win.navigate('add'); await settle();
    $(c, '#add-content [data-v2-peer="dev-peer"]').click(); await settle();
    await emit(c, 'lan.peers', { peers: [peer({ ...over, waiting: false })] });
    await emit(c, 'lan.pair', { phase: 'failed', code: 'E_LAN_PAIR_WINDOW', event_seq: 1 }); await settle();
    assert.match($(c, '#guide-content').textContent, copy);
  });
}

test('settings turn pairing requests on and off', async t => {
  let fail = false;
  const c = await client(t, { state: withLan(), displays, request: async (method, params) => {
    if (method !== 'lan.pair.requests') return undefined;
    if (fail) throw { code: 'E_CONFIG_WRITE' };
    return { enabled: params.enabled };
  } });
  c.win.navigate('settings'); await settle();
  const toggle = $(c, '#pair-requests-toggle');
  assert.equal(toggle.checked, true);
  assert.equal(toggle.disabled, false);
  toggle.checked = false; toggle.dispatchEvent(new c.win.Event('change', { bubbles: true })); await settle();
  assert.equal(calls(c, 'lan.pair.requests').at(-1).params.enabled, false);
  assert.equal(toggle.checked, false);
  fail = true;
  toggle.checked = true; toggle.dispatchEvent(new c.win.Event('change', { bubbles: true })); await settle();
  assert.equal(toggle.checked, false, 'a failed save reverts the switch');
  assert.match($(c, '#pair-requests-status').textContent, /没有保存/);
});

test('the setting stays disabled until the sidecar reports it', async t => {
  const c = await client(t, { state: state(), displays });
  c.win.navigate('settings'); await settle();
  assert.equal($(c, '#pair-requests-toggle').disabled, true);
});
