const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const test = require('node:test');
const { JSDOM } = require('jsdom');
const { initializeI18n } = require('./i18n-helper');
const root = path.resolve(__dirname, '../src/renderer');
const tick = () => new Promise(resolve => setImmediate(resolve));
async function setup(t, options = {}) {
  const dom = new JSDOM(fs.readFileSync(path.join(root, 'index.html'), 'utf8'), { url: 'http://localhost', runScripts: 'outside-only' });
  t.after(() => dom.window.close());
  const win = dom.window, requests = [], opened = [], copied = [], handlers = new Map();
  win.HTMLDialogElement.prototype.showModal = function () { this.setAttribute('open', ''); };
  win.HTMLDialogElement.prototype.close = function () { this.removeAttribute('open'); this.dispatchEvent(new win.Event('close')); };
  Object.defineProperty(win.navigator, 'clipboard', { value: { writeText: async text => {
    if (options.copyFailed) throw new Error('clipboard unavailable'); copied.push(text);
  } } });
  win.kvmflow = {
    info: Promise.resolve({ platform: 'windows', version: '0.4.0', arch: 'x86_64', osVersion: '11.0.26100' }), cachedDisplays: options.displays || [],
    request: async method => { requests.push(method); if (options.requestFailed) throw new Error('/Users/private'); return {}; },
    openCompatibilityIssue: async url => { if (options.openFailed) throw new Error('cannot launch'); opened.push(url); },
    onNotification: (kind, cb) => handlers.set(kind, cb),
  };
  await initializeI18n(win, 'en');
  for (const file of ['compat-report.js', 'compatibility.js']) vm.runInContext(fs.readFileSync(path.join(root, file), 'utf8'), dom.getInternalVMContext());
  const click = async id => { win.document.getElementById(id).click(); await tick(); };
  return { win, click, opened, copied, requests, handlers, dialog: win.document.getElementById('compatibility-dialog'), preview: () => win.document.getElementById('compatibility-report').textContent };
}

test('entry previews every byte before browser or clipboard access, without DDC or diagnostics', async t => {
  const c = await setup(t); await c.click('submit-device');
  assert.equal(c.dialog.open, true); assert.ok(c.preview().includes('Windows'));
  assert.deepEqual(c.requests, ['config.get', 'usb.snapshot', 'state.get']);
  assert.deepEqual(c.opened, []); assert.deepEqual(c.copied, []);
  await c.click('copy-compatibility'); assert.equal(c.copied[0], c.preview());
  await c.click('open-compatibility');
  assert.equal(new URL(c.opened[0]).searchParams.get('report'), c.preview()); assert.equal(c.dialog.open, false);
});

test('cancel and hidden lifecycle do not open GitHub or copy data', async t => {
  const c = await setup(t); await c.click('submit-device'); await c.click('cancel-compatibility');
  assert.equal(c.dialog.open, false); assert.equal(c.win.document.activeElement.id, 'submit-device');
  await c.click('submit-device'); c.handlers.get('client.hidden')();
  assert.equal(c.dialog.open, false); assert.deepEqual(c.opened, []); assert.deepEqual(c.copied, []);
});

const manyDisplays = () => Array.from({ length: 12 }, (_, i) => ({ edid_id: `display-${i}`, manufacturer: 'DEL', model_name: 'U2723QE', ddc: { state: 'available', input: 15 } }));
test('long URL copies the complete report and opens a blank form with an explicit paste prompt', async t => {
  const c = await setup(t, { displays: manyDisplays() }); await c.click('submit-device'); await c.click('open-compatibility');
  assert.equal(c.copied[0], c.preview());
  const url = new URL(c.opened[0]); assert.equal(url.searchParams.has('report'), false); assert.equal(url.searchParams.get('template'), 'compatibility.yml');
  assert.match(c.win.document.getElementById('compatibility-status').textContent, /Paste it/);
});

test('clipboard failure blocks long-URL launch and preserves full preview for retry', async t => {
  const c = await setup(t, { displays: manyDisplays(), copyFailed: true }); await c.click('submit-device'); const preview = c.preview(); await c.click('open-compatibility');
  assert.deepEqual(c.opened, []); assert.equal(c.dialog.open, true); assert.equal(c.preview(), preview);
  assert.match(c.win.document.getElementById('copy-compatibility').textContent, /Copy failed/);
});

test('browser and collection failures stay local and never leak raw errors', async t => {
  const c = await setup(t, { openFailed: true }); await c.click('submit-device'); await c.click('open-compatibility');
  assert.equal(c.dialog.open, true); assert.match(c.win.document.getElementById('open-compatibility').textContent, /Open failed/);
  const failed = await setup(t, { requestFailed: true }); await failed.click('submit-device');
  assert.equal(failed.dialog.open, false); assert.match(failed.win.document.getElementById('compatibility-status').textContent, /Could not prepare/);
  assert.doesNotMatch(failed.win.document.body.textContent, /\/Users\/private/);
});
