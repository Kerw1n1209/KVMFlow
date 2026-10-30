const assert = require('node:assert/strict');
const test = require('node:test');
const { client, tick, v2 } = require('./client-helper');

// A runtime stand-in that tags each config version like config.get/config.set.
function tagged() {
  const runtime = { live: structuredClone(v2), etag: 'e1', version: 1 };
  runtime.change = edit => { edit(runtime.live); runtime.etag = `e${++runtime.version}`; };
  runtime.request = async (method, params) => {
    if (method === 'config.get') {
      const config = structuredClone(runtime.live);
      return params?.with_etag ? { config, etag: runtime.etag } : config;
    }
    if (method === 'config.set') {
      if (params.expected_etag && params.expected_etag !== runtime.etag) {
        throw Object.assign(new Error('Settings changed on another computer. Save again.'), { code: 'E_CONFIG_STALE' });
      }
      runtime.change(live => Object.assign(live, structuredClone(params.config)));
      return { saved: true, etag: runtime.etag };
    }
  };
  return runtime;
}
const rename = name => live => { live.switch_group.devices.find(d => d.device_id === 'host-mac').name = name; };
const sets = c => c.requests.filter(r => r.method === 'config.set');
const gets = c => c.requests.filter(r => r.method === 'config.get');

test('a synced peer rename is reloaded and kept by the next settings save', async t => {
  const runtime = tagged();
  const c = await client(t, { request: runtime.request });
  runtime.change(rename('Mac Studio'));
  await c.subscriptions.get('config.changed')({ etag: runtime.etag }); await tick();
  c.win.navigate('settings'); await c.win.saveSettings();
  const saved = sets(c).at(-1).params;
  assert.equal(saved.expected_etag, 'e2');
  assert.equal(saved.config.switch_group.devices.find(d => d.device_id === 'host-mac').name, 'Mac Studio');
});

test('a save built from an older view reloads and saves on top of the change', async t => {
  const runtime = tagged();
  const c = await client(t, { request: runtime.request });
  runtime.change(rename('Mac Studio')); // the notification has not arrived yet
  c.win.navigate('settings'); await c.win.saveSettings();
  assert.deepEqual(sets(c).map(r => r.params.expected_etag), ['e1', 'e2']);
  assert.equal(runtime.live.switch_group.devices.find(d => d.device_id === 'host-mac').name, 'Mac Studio');
});

test('the notification for its own save does not reload again', async t => {
  const runtime = tagged();
  const c = await client(t, { request: runtime.request });
  c.win.navigate('settings'); await c.win.saveSettings();
  const before = gets(c).length;
  await c.subscriptions.get('config.changed')({ etag: runtime.etag }); await tick();
  assert.equal(gets(c).length, before);
});
