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
  await window.kvmflow.updates.install({ allowUnrestored: true });
  assert.equal(calls.at(-1)[1].allowUnrestored, true);
  await window.kvmflow.updates.setBeta(true);
  assert.equal(calls.at(-1)[0], 'update_set_beta');
  assert.equal(calls.at(-1)[1].enabled, true);
  await window.kvmflow.updates.setBeta('true');
  assert.equal(calls.at(-1)[1].enabled, false);
  await window.kvmflow.updates.openDownload();
  assert.equal(calls.at(-1)[0], 'update_open_download');
  await window.kvmflow.confirmLocalInput('G73：15 → 17');
  assert.equal(calls.at(-1)[0], 'confirm_local_input');
  assert.equal(calls.at(-1)[1].changes, 'G73：15 → 17');
});

test('native hidden event reaches pairing lifecycle listeners', async () => {
  const events = new Map();
  const window = { __TAURI__: { core: { invoke: async () => ({ platform: 'windows' }) },
    event: { listen: async (kind, callback) => { events.set(kind, callback); return () => {}; } } } };
  vm.runInNewContext(fs.readFileSync(path.join(root, 'src/renderer/bridge.js'), 'utf8'), { window });
  await window.kvmflow.ready;
  let hidden = 0;
  window.kvmflow.onNotification('client.hidden', () => { hidden += 1; });
  events.get('client-hidden')({ payload: {} });
  assert.equal(hidden, 1);
});

test('compatibility inventory reuses display responses without probing and opens only its native command', async () => {
  const calls = [];
  const displays = [{ edid_id: 'private-edid', model_name: 'U2723QE' }];
  const window = { __TAURI__: {
    core: { invoke: async (command, args) => {
      calls.push([command, args]);
      return command === 'runtime_request' ? { displays } : { platform: 'windows' };
    } },
    event: { listen: async () => () => {} },
  } };
  vm.runInNewContext(fs.readFileSync(path.join(root, 'src/renderer/bridge.js'), 'utf8'), { window, structuredClone });
  await window.kvmflow.ready;
  assert.equal(window.kvmflow.cachedDisplays.length, 0);
  await window.kvmflow.request('display.list');
  assert.deepEqual(window.kvmflow.cachedDisplays, displays);
  window.kvmflow.cachedDisplays[0].model_name = 'mutated';
  assert.equal(window.kvmflow.cachedDisplays[0].model_name, 'U2723QE');
  assert.equal(calls.filter(([command]) => command === 'runtime_request').length, 1);
  await window.kvmflow.openCompatibilityIssue('https://github.com/Kerw1n1209/KVMFlow/issues/new?template=compatibility.yml');
  assert.equal(calls.at(-1)[0], 'compatibility_open_issue');
});
