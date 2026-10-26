const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');
const { JSDOM } = require('jsdom');
const { initializeI18n } = require('./i18n-helper');
const root = path.resolve(__dirname, '..');
const read = relative => fs.readFileSync(path.join(root, relative), 'utf8');

const v2 = {
  schema_version: 2,
  local_device: { device_id: 'host-win', host_label: 'Windows', monitors: [
    { fingerprint: 'display-b', label: '副屏', local_input: 8 },
    { fingerprint: 'display-a', label: '主屏', local_input: 16 },
  ] },
  switch_group: { group_id: 'preserved-group', revision: 4, devices: [
    { device_id: 'host-mac', name: 'Mac', port_index: 1, monitors: [
      { fingerprint: 'display-a', local_input: 15 }, { fingerprint: 'display-b', local_input: 7 },
    ] },
    { device_id: 'host-win', name: 'Windows', port_index: 2, monitors: [
      { fingerprint: 'display-b', local_input: 8 }, { fingerprint: 'display-a', local_input: 16 },
    ] },
    { device_id: 'host-linux', name: '第三台', port_index: 3, monitors: [
      { fingerprint: 'display-a', local_input: 17 }, { fingerprint: 'display-b', local_input: 9 },
    ] },
  ] },
  trigger: { anchor: { vid_pid: '1a40:0101' }, members: [{ vid_pid: '3837:303c' }], debounce: {
    t_stable_ms: 0, t_absent_ms: 0, t_cooldown_ms: 1000, quorum_peripherals: 1,
  } },
  advanced: { ddc_retry: { attempts: 3, delay_ms: 200 }, usb_poll_ms: 300, arrival_correction_enabled: false },
};

async function client(t, options = {}) {
  const { failSave = false, displays = [], storage = {} } = options;
  let config = options.config === undefined ? v2 : options.config;
  const state = options.state || { state: 'idle', enabled: true, mixed: config?.switch_group ? mixedState(config) : null };
  const computerName = Object.hasOwn(options, 'computerName') ? options.computerName : config?.local_device?.host_label;
  const dom = new JSDOM(read('src/renderer/index.html'), { url: 'http://tauri.localhost', runScripts: 'outside-only' });
  t.after(() => dom.window.close());
  const win = dom.window, requests = [], subscriptions = new Map(), fanout = new Map();
  for (const [key, value] of Object.entries(storage)) win.localStorage.setItem(key, value);
  let exported = null;
  const navigateListeners = new Set();
  win.structuredClone = structuredClone;
  win.TextEncoder = TextEncoder;
  // jsdom does not implement modal dialogs; keep their open/close lifecycle real.
  win.HTMLDialogElement.prototype.showModal = function () { this.setAttribute('open', ''); };
  win.HTMLDialogElement.prototype.close = function () { this.removeAttribute('open'); this.dispatchEvent(new win.Event('close')); };
  win.kvmflow = {
    ready: Promise.resolve(), info: Promise.resolve({ platform: 'windows', version: '0.2.0', computerName, initialPage: options.initialPage || (config ? 'status' : 'wizard') }),
    clientReady: async () => {},
    request: async (method, params) => {
      requests.push({ method, params });
      if (options.request) {
        const result = await options.request(method, params);
        if (result !== undefined) return result;
      }
      if (method === 'config.get') return structuredClone(config);
      if (method === 'lan.identity') return { device_id: state?.lan?.self?.device_id || config?.local_device?.device_id || 'dev-test-local' };
      if (method === 'state.get') return structuredClone(state);
      if (method === 'display.list') return { displays };
      if (method === 'config.set') {
        if (failSave) throw new Error('保存失败');
        config = structuredClone(params.config);
      }
      if (method === 'diagnostics.collect') return { events: [], backend: 'test' };
      return {};
    },
    // Mirror the real bridge: several modules subscribe to the same kind.
    onNotification: (kind, cb) => {
      if (!fanout.has(kind)) fanout.set(kind, new Set());
      fanout.get(kind).add(cb);
      subscriptions.set(kind, async (data) => {
        await Promise.all([...fanout.get(kind)].map(handler => handler(data)));
      });
      return () => fanout.get(kind)?.delete(cb);
    },
    onClientNavigate: cb => { navigateListeners.add(cb); return () => navigateListeners.delete(cb); },
    startup: { get: async () => false, set: async enabled => enabled },
    confirmDelete: async () => true,
    exportDiagnostics: async payload => { exported = payload; return true; },
  };
  await initializeI18n(win, options.language || 'zh-CN');
  vm.runInContext(read('src/renderer/shell.js'), dom.getInternalVMContext());
  vm.runInContext(read('src/renderer/redact.js'), dom.getInternalVMContext());
  vm.runInContext(read('src/renderer/compat-report.js'), dom.getInternalVMContext());
  vm.runInContext(read('src/renderer/compatibility.js'), dom.getInternalVMContext());
  vm.runInContext(read('src/renderer/mixed-view.js'), dom.getInternalVMContext());
  vm.runInContext(read('src/renderer/mixed.js'), dom.getInternalVMContext());
  vm.runInContext(read('src/renderer/lan-pairing-view.js'), dom.getInternalVMContext());
  vm.runInContext(read('src/renderer/lan-pairing.js'), dom.getInternalVMContext());
  vm.runInContext(read('src/renderer/app.js'), dom.getInternalVMContext());
  await new Promise(resolve => setTimeout(resolve, 10));
  return { win, requests, subscriptions, setConfig: next => { config = structuredClone(next); }, export: () => exported, navigate: page => navigateListeners.forEach(cb => cb(page)) };
}

function mixedState(config = v2) {
  const monitors = config.local_device.monitors;
  const assignment = config.switch_group.mixed?.assignment || monitors.map((m, i) => ({fingerprint:m.fingerprint, device_id: config.switch_group.devices[i % 2].device_id}));
  return {enabled:false, revision:1, assignment, validation:{ok:true,reasons:[]}, keyboard_holder_device_id:config.local_device.device_id, holder_confidence:'known',
    transition:{phase:'idle',outcome:'ok',started_ms:1,reasons:[]},
    destinations:monitors.map(m => ({fingerprint:m.fingerprint,builtin:false,available:true,connected:true,current_device_id:config.local_device.device_id,target_device_id:config.local_device.device_id,evidence:'input_observation_only',fixed:false})),
    pending_remote:[],warnings:[],execution:[],topology_error:null};
}

const tick = () => new Promise(resolve => setTimeout(resolve, 10));
const automaticConfig = () => {
  const config = structuredClone(v2);
  config.local_device.monitors.forEach(monitor => { monitor.source = 'learned_active_read'; });
  return config;
};

const pairedV2 = () => {
  const config = structuredClone(v2);
  config.switch_group.devices.push({ device_id: 'dev-peer', name: 'DESKTOP-GAME', port_index: 4,
    monitors: [{fingerprint: 'display-a', local_input: 21}, {fingerprint: 'display-b', local_input: 22}] });
  return config;
};
module.exports = { client, v2, tick, automaticConfig, mixedState, pairedV2 };
