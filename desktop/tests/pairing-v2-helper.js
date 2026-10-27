const assert = require('node:assert/strict');
const { client, tick, pairedV2 } = require('./client-helper');
const peer = (over = {}) => ({ device_id: 'dev-peer', name: 'GAME', online: true, waiting: true, addr: '192.168.1.23:47820', ...over });
const displays = [{ edid_id: 'display-a', model_name: '显示器 1', ddc: { state: 'available', input: 15 } }, { edid_id: 'display-b', model_name: '显示器 2', ddc: { state: 'available', input: 7 } }];
const state = (peers = [peer()], id = 'host-win') => ({ state: 'idle', enabled: true, lan: { enabled: true, self: { device_id: id, addr: '192.168.1.22:47820' }, peers } });
const click = (c, action) => { const el = c.win.document.querySelector(`[data-pair-action="${action}"]`); assert.ok(el, action); el.click(); };
const body = c => c.win.document.querySelector('#guide-content').textContent;
const emit = async (c, kind, data) => { await c.subscriptions.get(kind)(data); await tick(); };
async function setup(t, options = {}) {
  const c = await client(t, { state: state(), displays, ...options });
  const timers = new Map();
  const nativeTimeout = c.win.setTimeout.bind(c.win);
  c.win.setTimeout = (fn, delay, ...args) => {
    if ([10000, 5000].includes(delay)) { const id = nativeTimeout(() => {}, 1000000); timers.set(delay, { id, fn: () => fn(...args) }); return id; }
    return nativeTimeout(fn, delay, ...args);
  };
  c.fire = async delay => { const timer = timers.get(delay); assert.ok(timer, `timer ${delay}`); c.win.clearTimeout(timer.id); timers.delete(delay); timer.fn(); await tick(); };
  c.win.navigate('guide');
  c.start = async () => { click(c, 'start'); await tick(); };
  return c;
}
async function waiting(c) { await c.start(); await c.fire(10000); assert.equal(c.win.KVMFlowPairing.activeMode(), 'wait'); }
const profile = (pending = false, rev = 1) => ({ device_id: 'dev-peer', profile_rev: rev, monitors: displays.map((d, i) => ({ fingerprint: d.edid_id, name: d.model_name, input: pending && i === 1 ? { state: 'pending' } : { state: 'value', value: i + 17 } })) });
async function ready(c, pending = false) {
  await c.start(); c.setConfig(pairedV2());
  await emit(c, 'lan.pair', { phase: 'committed', device_id: 'dev-peer', initiator: true, config_merged: true });
  await emit(c, 'lan.profile', profile(pending));
}
module.exports = { setup, waiting, ready, state, peer, displays, click, body, emit, profile, tick };
