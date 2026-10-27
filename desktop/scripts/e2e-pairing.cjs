#!/usr/bin/env node
// Two simulated sidecars, each driven by the real renderer (jsdom), walk the
// role-free setup the way a person does: start on one computer, press the USB
// switch, start on the other, then test. Catches renderer <-> sidecar contract
// bugs before a release. It is not hardware acceptance.
//
//   npm run e2e:pairing          (both orders)
//   node scripts/e2e-pairing.cjs [--early-press] [--no-build] [--keep]
//
// --early-press presses the switch while the first computer is still
// searching, before it starts identifying the switch.
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const readline = require('node:readline');
const vm = require('node:vm');
const { spawn, execFileSync } = require('node:child_process');
const { JSDOM } = require('jsdom');

const desktop = path.resolve(__dirname, '..');
const repo = path.resolve(desktop, '..');
const renderer = path.join(desktop, 'src/renderer');
const args = new Set(process.argv.slice(2));
const earlyPress = args.has('--early-press');
const exe = path.join(repo, 'sidecar/target/debug', process.platform === 'win32' ? 'kvmflow-sidecar.exe' : 'kvmflow-sidecar');
const messages = JSON.parse(fs.readFileSync(path.join(renderer, 'i18n/messages.json'), 'utf8'));
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const started = Date.now();
const stamp = () => `${((Date.now() - started) / 1000).toFixed(1).padStart(5)}s`;
const say = (who, text) => console.log(`${stamp()} ${who.padEnd(3)} ${text}`);

class Sidecar {
  constructor(name, dir, discovery, scenario = 'idle') {
    this.name = name; this.next = 1; this.pending = new Map(); this.listeners = new Set();
    fs.mkdirSync(dir, { recursive: true });
    this.child = spawn(exe, ['--backend', 'simulated', '--scenario', scenario, '--config', path.join(dir, 'config.json'),
      '--log-dir', path.join(dir, 'logs'), '--lan', '--lan-port', '0'], {
      env: { ...process.env, KVMFLOW_LAN_ALLOW_LOOPBACK: '1', KVMFLOW_LAN_DISCOVERY_DIR: discovery },
      stdio: ['pipe', 'pipe', fs.openSync(path.join(dir, 'stderr.log'), 'w')],
    });
    this.child.on('exit', code => { if (!this.closing) say(name, `sidecar exited (${code})`); });
    readline.createInterface({ input: this.child.stdout }).on('line', line => {
      let message; try { message = JSON.parse(line); } catch { return; }
      if (message.notification) { for (const cb of this.listeners) cb(message.notification, message.data); return; }
      const waiter = this.pending.get(message.id);
      if (!waiter) return;
      this.pending.delete(message.id);
      if (message.ok) waiter.resolve(message.result ?? null); else waiter.reject(message.error);
    });
  }
  request(method, params = {}) {
    const id = this.next++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.child.stdin.write(`${JSON.stringify({ v: 1, id, method, params })}\n`);
    });
  }
  close() { this.closing = true; this.child.kill(); }
}

async function rendererFor(name, sidecar, computerName) {
  const dom = new JSDOM(fs.readFileSync(path.join(renderer, 'index.html'), 'utf8'), { url: 'http://tauri.localhost', runScripts: 'outside-only' });
  const win = dom.window;
  const listeners = new Map();
  win.structuredClone = structuredClone;
  win.TextEncoder = TextEncoder;
  win.HTMLDialogElement.prototype.showModal = function () { this.setAttribute('open', ''); };
  win.HTMLDialogElement.prototype.close = function () { this.removeAttribute('open'); this.dispatchEvent(new win.Event('close')); };
  win.console.error = (...parts) => say(name, `console.error ${parts.map(String).join(' ')}`);
  const info = { version: '0.0.0-beta.0', platform: 'macos', arch: 'aarch64', osVersion: '15', computerName, initialPage: 'wizard', languagePreference: 'zh-CN', receiveBeta: true };
  const invoke = async (command, payload = {}) => {
    if (command === 'host_info') return info;
    if (command === 'runtime_request') {
      try { return await sidecar.request(payload.method, payload.params); }
      catch (error) { throw { code: error?.code || 'E_BACKEND', message: error?.message || String(error) }; }
    }
    if (command === 'confirm_delete' || command === 'confirm_local_input') return true;
    return null;
  };
  const emit = (event, payload) => { for (const cb of listeners.get(event) || []) cb({ payload }); };
  win.__TAURI__ = { core: { invoke }, event: { listen: async (event, cb) => {
    if (!listeners.has(event)) listeners.set(event, new Set());
    listeners.get(event).add(cb);
    return () => listeners.get(event).delete(cb);
  } } };
  sidecar.listeners.add((kind, data) => {
    if (/^(lan\.(pair|setup|profile|test|topology)|wizard\.|switch\.report|config)/.test(kind)) say(name, `<- ${kind} ${JSON.stringify(data).slice(0, 300)}`);
  });
  sidecar.listeners.add((kind, data) => emit('runtime-notification', { kind, data: { ...data, runtime_generation: 1 }, runtime_generation: 1 }));
  win.KVMFLOW_I18N_CONFIG = structuredClone(messages);
  const run = file => vm.runInContext(fs.readFileSync(path.join(renderer, file), 'utf8'), dom.getInternalVMContext(), { filename: file });
  run('i18n.js');
  await win.KVMFlowI18n.ready;
  win.KVMFlowI18n.setPreference('zh-CN', { persist: false, notify: false });
  run('bridge.js');
  await win.kvmflow.ready;
  for (const file of ['shell.js', 'redact.js', 'compat-report.js', 'compatibility.js', 'mixed-view.js', 'mixed.js', 'lan-pairing-view.js', 'lan-pairing.js', 'app.js']) run(file);
  await sleep(50);
  emit('client-navigate', 'wizard');
  await sleep(50);
  const ui = {
    name, win, sidecar,
    mode: () => win.KVMFlowPairing.activeMode(),
    text: () => win.document.querySelector('#guide-content')?.textContent.replace(/\s+/g, ' ').trim(),
    events: () => [...win.document.querySelectorAll('#event-list .event')].map(row => row.textContent.replace(/\s+/g, ' ').trim()),
    click(action) {
      const el = win.document.querySelector(`#guide-content [data-pair-action="${action}"]`);
      if (!el) throw new Error(`${name}: no "${action}" button in mode ${ui.mode()}: ${ui.text()}`);
      if (el.disabled) throw new Error(`${name}: "${action}" is disabled in mode ${ui.mode()}: ${ui.text()}`);
      say(name, `click ${action}`);
      el.click();
    },
    async until(label, predicate, timeout = 30000) {
      const deadline = Date.now() + timeout;
      while (Date.now() < deadline) {
        if (predicate()) return;
        await sleep(100);
      }
      throw new Error(`${name}: timed out waiting for ${label}; mode ${ui.mode()}: ${ui.text()}\n  events: ${ui.events().slice(0, 4).join(' | ')}`);
    },
  };
  let last = null;
  setInterval(() => {
    const mode = ui.mode();
    if (mode !== last) { say(name, `mode ${last ?? '-'} -> ${mode}: ${ui.text()?.slice(0, 90)}`); last = mode; }
  }, 50).unref();
  return ui;
}

// One physical switch: the devices are on exactly one computer at a time.
async function press(to, from) {
  say('USB', `switch -> ${to.name}`);
  await from.sidecar.request('sim.usb', { group: false });
  await to.sidecar.request('sim.usb', { group: true });
}

// Monitors are shared and show one computer at a time. Each simulated
// backend reads only while the picture is its own, as on real hardware.
async function showMonitors(owner, other) {
  say('DDC', `monitors -> ${owner.name}`);
  await other.sidecar.request('sim.display', { here: false });
  await owner.sidecar.request('sim.display', { here: true });
}

async function main() {
  if (!args.has('--no-build')) execFileSync('cargo', ['build', '-q', '-p', 'kvmflow-sidecar'], { cwd: path.join(repo, 'sidecar'), stdio: 'inherit' });
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'kvmflow-e2e-'));
  const discovery = path.join(root, 'discovery');
  const sa = new Sidecar('A', path.join(root, 'a'), discovery);
  const sb = new Sidecar('B', path.join(root, 'b'), discovery, 'idle_peer');
  let failed = false;
  try {
    // The switch starts on A.
    await sb.request('sim.usb', { group: false });
    const a = await rendererFor('A', sa, 'Mac A');
    const b = await rendererFor('B', sb, 'PC B');

    a.click('start');
    await a.until('A searching', () => a.mode() === 'search');
    if (earlyPress) { await sleep(2000); await press(b, a); }
    await a.until('A waiting', () => a.mode() === 'wait', 20000);
    if (!earlyPress) { await sleep(1000); await press(b, a); }
    await sleep(500);
    await showMonitors(b, a);
    await sleep(1500);

    b.click('start');
    await b.until('B connected', () => b.mode() === 'confirm', 40000);
    await b.until('B next enabled', () => b.win.document.querySelector('[data-pair-action="v2-next"]')?.disabled === false, 30000);
    b.click('v2-next');
    await b.until('B testing', () => ['try', 'try-hint'].includes(b.mode()), 10000);
    await sleep(1500);
    const reports = { A: [], B: [] };
    for (const side of [a, b]) side.sidecar.listeners.add((kind, data) => { if (kind === 'switch.report') reports[side.name].push(data); });
    await press(a, b);
    // B sends the monitors to A once the switch has stayed away (T_absent).
    await b.until('B switch report', () => reports.B.length > 0, 30000);
    const bad = reports.B[0].per_monitor.filter(row => row.commanded !== true);
    if (bad.length) throw new Error(`B did not command ${bad.map(row => row.edid_id).join(', ')}`);
    await showMonitors(a, b);
    await sleep(1000);
    for (const side of [a, b]) if (side.mode() === 'test-error') throw new Error(`${side.name} shows E3: ${side.text()}`);
    if (a.mode() !== 'done') throw new Error(`A is not done: ${a.text()}`);
    // And back: A sends the monitors to B.
    await sleep(6000);
    await press(b, a);
    await a.until('A switch report', () => reports.A.length > 0, 30000);
    const back = reports.A[0].per_monitor.filter(row => row.commanded !== true);
    if (back.length) throw new Error(`A did not command ${back.map(row => row.edid_id).join(', ')}`);
    const wrote = reports.A[0].per_monitor.map(row => row.requested ?? row.value).join('/');
    say('A', `sent monitors to B (${wrote})`);

    const [ca, cb] = await Promise.all([sa.request('config.get'), sb.request('config.get')]);
    for (const [who, config] of [['A', ca], ['B', cb]]) {
      const devices = config?.switch_group?.devices?.length ?? 0;
      say(who, `config: trigger=${config?.trigger?.anchor?.vid_pid || '(none)'} debounce=${JSON.stringify(config?.trigger?.debounce)} devices=${devices}`);
      if (!config?.trigger?.anchor?.vid_pid) throw new Error(`${who} has no trigger`);
      if (devices !== 2) throw new Error(`${who} has ${devices} computers in its group`);
    }
    say('OK', earlyPress ? 'setup completed (early press)' : 'setup completed');
  } catch (error) {
    failed = true;
    console.error(`\nFAILED: ${error.message}`);
    console.error(`logs: ${root}`);
  } finally {
    sa.close(); sb.close();
    if (!failed && !args.has('--keep')) fs.rmSync(root, { recursive: true, force: true });
  }
  process.exit(failed ? 1 : 0);
}

main();
