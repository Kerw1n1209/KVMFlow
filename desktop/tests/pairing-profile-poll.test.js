const assert = require('node:assert/strict');
const test = require('node:test');
const V = require('./pairing-v2-helper');

// Profile refreshes during confirmation go to the peer and can drop; the 2 s
// poll retries them, so only a run of failures ends the flow.
async function confirming(t) {
  let failing = false;
  const c = await V.setup(t, { request: async method => {
    if (method === 'lan.profile.refresh' && failing) throw Object.assign(new Error('timeout'), { code: 'E_LAN_TIMEOUT' });
  } });
  let poll = null;
  const nativeInterval = c.win.setInterval.bind(c.win);
  c.win.setInterval = (fn, delay) => (delay === 2000 ? (poll = fn, 0) : nativeInterval(fn, delay));
  await V.ready(c);
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'confirm');
  assert.ok(poll, 'profile poll started');
  return { c, fail: on => { failing = on; }, poll: async () => { poll(); await V.tick(); } };
}

test('one failed profile poll keeps the confirmation open', async t => {
  const { c, fail, poll } = await confirming(t);
  fail(true); await poll();
  fail(false); await poll();
  fail(true); await poll(); await poll(); await poll(); await poll();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'confirm');
});

test('a run of failed profile polls ends the flow', async t => {
  const { c, fail, poll } = await confirming(t);
  fail(true);
  for (let i = 0; i < 5; i += 1) await poll();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'error');
});

test('a failed refresh after learned inputs outside pairing opens no error', async t => {
  const c = await V.setup(t, { request: async method => {
    if (method === 'lan.profile.refresh') throw Object.assign(new Error('timeout'), { code: 'E_LAN_TIMEOUT' });
  } });
  await V.emit(c, 'lan.profile', { phase: 'inputs_learned', device_id: 'dev-peer' });
  assert.ok(c.requests.some(r => r.method === 'lan.profile.refresh'));
  assert.notEqual(c.win.KVMFlowPairing.activeMode(), 'error');
});
