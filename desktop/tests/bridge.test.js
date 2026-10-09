const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const test = require('node:test');
const root = path.resolve(__dirname, '..');

test('bridge installs listeners before commands and unsubscribes independently', async () => {
  const events = new Map(), calls = [];
  const window = { __TAURI__: {
    core: { invoke: async (command, params) => { calls.push([command, params]); return { platform: 'windows', version: '0.2.0' }; } },
    event: { listen: async (kind, cb) => { events.set(kind, cb); return () => events.delete(kind); } },
  } };
  vm.runInNewContext(fs.readFileSync(path.join(root, 'src/renderer/bridge.js'), 'utf8'), { window });
  await window.kvmflow.ready;
  const received = [];
  const stop = window.kvmflow.onNotification('state', data => received.push(data.state));
  events.get('runtime-notification')({ payload: { kind: 'state', data: { state: 'idle' } } });
  stop();
  events.get('runtime-notification')({ payload: { kind: 'state', data: { state: 'pushing' } } });
  assert.deepEqual(received, ['idle']);
  await window.kvmflow.request('config.get');
  assert.equal(calls[1][0], 'runtime_request');
  events.get('client-navigate')({ payload: 'settings' });
  window.kvmflow.onClientNavigate(page => received.push(page));
  assert.deepEqual(received, ['idle', 'settings']);
  const progress = [];
  const unsubscribe = window.kvmflow.updates.onProgress(data => progress.push(data.percent));
  events.get('update-progress')({ payload: { percent: 50 } });
  unsubscribe();
  events.get('update-progress')({ payload: { percent: 100 } });
  assert.deepEqual(progress, [50]);
  await window.kvmflow.updates.check();
  await window.kvmflow.updates.download();
  await window.kvmflow.updates.install();
  assert.deepEqual(calls.slice(-3).map(([command]) => command), ['update_check', 'update_download', 'update_install']);
  await window.kvmflow.confirmLocalInput('G73：15 → 17');
  assert.equal(calls.at(-1)[0], 'confirm_local_input');
  assert.equal(calls.at(-1)[1].changes, 'G73：15 → 17');
});
