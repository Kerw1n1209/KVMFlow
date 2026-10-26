const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const test = require('node:test');
const { JSDOM } = require('jsdom');
const { initializeI18n } = require('./i18n-helper');
const root = path.resolve(__dirname, '../src/renderer');
const tick = () => new Promise(resolve => setImmediate(resolve));
const receipt = { ok: true, id: 'fb_01JABCDEF0123456789ABCDEFG' };
async function setup(t, options = {}) {
  const dom = new JSDOM(fs.readFileSync(path.join(root, 'index.html'), 'utf8'), { url: 'http://localhost', runScripts: 'outside-only' });
  t.after(() => dom.window.close());
  const win = dom.window, requests = [], opened = [], copied = [], sent = [], handlers = new Map();
  let diagnosticCalls = 0, privacyCalls = 0;
  win.structuredClone = structuredClone; win.TextEncoder = TextEncoder;
  win.HTMLDialogElement.prototype.showModal = function () { this.setAttribute('open', ''); };
  win.HTMLDialogElement.prototype.close = function () { this.removeAttribute('open'); this.dispatchEvent(new win.Event('close')); };
  Object.defineProperty(win.navigator, 'clipboard', { value: { writeText: async text => { if (options.copyFailed) throw new Error('clipboard unavailable'); copied.push(text); } } });
  win.kvmflow = {
    info: Promise.resolve({ platform: 'windows', version: '0.4.0', arch: 'x86_64', osVersion: '11.0.26100', computerName: 'PRIVATE-PC' }), cachedDisplays: options.displays || [],
    hostInfo: async () => ({ platform: 'windows', version: '0.4.0', arch: 'x86_64', osVersion: '11.0.26100', receiveBeta: options.receiveBeta === true }),
    request: async method => {
      requests.push(method);
      if (options.requestFailed) throw new Error('/Users/private');
      if (method === 'display.list') { if (options.probeFailed) throw new Error('probe failed'); return { displays: options.liveDisplays || options.displays || [] }; }
      return {};
    },
    openCompatibilityIssue: async url => { if (options.openFailed) throw new Error('cannot launch'); opened.push(url); },
    openPrivacy: async () => { privacyCalls++; },
    sendFeedback: async payload => { sent.push(structuredClone(payload)); if (options.send) return options.send(payload); if (options.sendError) throw { code: options.sendError }; return receipt; },
    collectRedactedDiagnostics: async () => {
      diagnosticCalls++;
      if (options.collect) return options.collect();
      return win.KVMFlowRedact.redactDiagnostics({ config: { local_device: { host_label: 'PRIVATE-PC' }, monitors: [{ fingerprint: 'DEL-A261-0000BEEF' }] }, path: '/Users/private/report', event: 'PRIVATE-PC 192.168.1.5' });
    },
    onNotification: (kind, cb) => handlers.set(kind, cb),
  };
  await initializeI18n(win, 'en');
  for (const file of ['redact.js', 'compat-report.js', 'compatibility.js']) vm.runInContext(fs.readFileSync(path.join(root, file), 'utf8'), dom.getInternalVMContext());
  const byId = id => win.document.getElementById(id);
  const click = async id => { byId(id).click(); await tick(); };
  const fill = (id, value) => { byId(id).value = value; byId(id).dispatchEvent(new win.Event('input', { bubbles: true })); };
  const choose = (kind = 'works') => { const radio = win.document.querySelector(`[name="feedback-kind"][value="${kind}"]`); radio.checked = true; radio.dispatchEvent(new win.Event('input', { bubbles: true })); };
  return { win, click, fill, choose, byId, opened, copied, sent, requests, handlers, dialog: byId('compatibility-dialog'), preview: () => byId('compatibility-report').textContent, diagnosticCalls: () => diagnosticCalls, privacyCalls: () => privacyCalls };
}

test('entry defaults to no diagnostics and previews the exact structured object sent', async t => {
  const c = await setup(t); await c.click('submit-device');
  assert.equal(c.dialog.open, true); assert.equal(c.byId('feedback-diagnostics').checked, false); assert.equal(c.byId('feedback-send').disabled, true);
  assert.deepEqual(c.requests, ['config.get', 'usb.snapshot', 'state.get', 'display.list']); assert.equal(c.diagnosticCalls(), 0); assert.deepEqual(c.sent, []);
  c.choose('problem'); c.fill('feedback-message', 'Display stayed black\nAfter switching'); c.fill('feedback-contact', 'user@example.com');
  const preview = JSON.parse(c.preview());
  await c.click('feedback-send');
  assert.deepEqual(c.sent[0], preview); assert.equal(c.sent[0].diagnostics, null); assert.equal(c.byId('feedback-success').classList.contains('hidden'), false);
  assert.equal(c.byId('feedback-id').textContent, receipt.id); await c.click('feedback-id'); assert.equal(c.copied[0], receipt.id);
});

test('diagnostics are collected only on explicit opt-in and the redacted text is visible and sent unchanged', async t => {
  const c = await setup(t); await c.click('submit-device'); c.choose(); await c.click('feedback-diagnostics');
  assert.equal(c.diagnosticCalls(), 1);
  const preview = JSON.parse(c.preview()); assert.equal(typeof preview.diagnostics, 'string');
  for (const secret of ['PRIVATE-PC', '0000BEEF', '/Users/private', '192.168.1.5']) assert.equal(preview.diagnostics.includes(secret), false, secret);
  await c.click('feedback-send'); assert.deepEqual(c.sent[0], preview);
});

test('unchecking diagnostics removes them and stale collection cannot add them back', async t => {
  let resolve;
  const c = await setup(t, { collect: () => new Promise(done => { resolve = done; }) });
  await c.click('submit-device'); c.choose(); await c.click('feedback-diagnostics');
  assert.equal(c.byId('feedback-send').disabled, true); await c.click('feedback-diagnostics');
  resolve({ redacted: true, event: 'late' }); await tick();
  assert.equal(JSON.parse(c.preview()).diagnostics, null); assert.equal(c.byId('feedback-send').disabled, false);
  await c.click('feedback-send'); assert.equal(c.sent[0].diagnostics, null);
});

test('oversized or failed diagnostics block sending until excluded', async t => {
  for (const collect of [async () => ({ text: '中'.repeat(17000) }), async () => { throw new Error('/Users/private'); }]) {
    const c = await setup(t, { collect }); await c.click('submit-device'); c.choose(); await c.click('feedback-diagnostics');
    assert.equal(c.byId('feedback-send').disabled, true); assert.match(c.byId('feedback-status').textContent, /too large|Could not collect/);
    await c.click('feedback-diagnostics'); assert.equal(c.byId('feedback-send').disabled, false); assert.deepEqual(c.sent, []);
  }
});

test('network, rate-limit, 5xx and validation failures preserve every input for retry and copying', async t => {
  for (const code of ['network', 'rate_limited', 'unavailable', 'too_large', 'invalid_feedback']) {
    const c = await setup(t, { sendError: code }); await c.click('submit-device'); c.choose('problem'); c.fill('feedback-message', 'Keep this draft'); c.fill('feedback-contact', 'contact@example.com');
    const preview = c.preview(); await c.click('feedback-send');
    assert.equal(c.dialog.open, true); assert.equal(c.preview(), preview); assert.equal(c.byId('feedback-message').value, 'Keep this draft');
    assert.equal(c.byId('copy-compatibility').classList.contains('hidden'), false); assert.equal(c.byId('feedback-send').disabled, false);
    await c.click('copy-compatibility'); assert.equal(c.copied[0], preview); await c.click('cancel-compatibility'); await c.click('submit-device'); assert.equal(c.preview(), preview);
  }
});

test('sending locks editing and prevents duplicate requests or accidental dismissal', async t => {
  let resolve;
  const c = await setup(t, { send: () => new Promise(done => { resolve = done; }) }); await c.click('submit-device'); c.choose();
  c.byId('feedback-send').click(); await tick();
  assert.equal(c.byId('feedback-message').disabled, true); assert.equal(c.byId('cancel-compatibility').disabled, true);
  await c.click('feedback-send'); assert.equal(c.sent.length, 1);
  const escape = new c.win.Event('cancel', { cancelable: true }); c.dialog.dispatchEvent(escape); assert.equal(escape.defaultPrevented, true);
  resolve(receipt); await tick(); assert.equal(c.byId('feedback-success').classList.contains('hidden'), false);
});

const manyDisplays = () => Array.from({ length: 8 }, (_, i) => ({ edid_id: `display-${i}`, manufacturer: 'DEL', model_name: '中'.repeat(64), ddc: { state: 'available', input: 15 } }));
test('public GitHub fallback uses structured report and description, excluding contact and private diagnostics', async t => {
  const c = await setup(t, { displays: manyDisplays() }); await c.click('submit-device'); c.choose(); c.fill('feedback-contact', 'private-contact@example.com'); c.fill('feedback-message', 'Public experience'); await c.click('feedback-diagnostics');
  await c.click('open-compatibility');
  const url = new URL(c.opened[0]); assert.equal(url.searchParams.has('report'), false); assert.equal(url.searchParams.get('template'), 'compatibility.yml');
  assert.match(c.copied[0], /Public experience/); assert.match(c.copied[0], /Everything works/); assert.doesNotMatch(c.copied[0], /private-contact|diagnostics|serial-1/); assert.match(c.byId('feedback-status').textContent, /Paste it/);
});

test('clipboard failure blocks long public fallback and privacy uses a dedicated native action', async t => {
  const c = await setup(t, { displays: manyDisplays(), copyFailed: true }); await c.click('submit-device'); await c.click('open-compatibility');
  assert.deepEqual(c.opened, []); assert.equal(c.dialog.open, true); assert.match(c.byId('feedback-status').textContent, /Copy failed/);
  await c.click('feedback-privacy'); assert.equal(c.privacyCalls(), 1); assert.deepEqual(c.sent, []);
});

test('hidden lifecycle and cancel preserve the draft without sending or opening GitHub', async t => {
  const c = await setup(t); await c.click('submit-device'); c.fill('feedback-message', 'Saved draft'); c.handlers.get('client.hidden')();
  assert.equal(c.dialog.open, false); await c.click('submit-device'); assert.equal(c.byId('feedback-message').value, 'Saved draft');
  await c.click('cancel-compatibility'); assert.equal(c.win.document.activeElement.id, 'submit-device'); assert.deepEqual(c.sent, []); assert.deepEqual(c.opened, []);
});

test('collection and receipt failures display local messages and never raw server paths', async t => {
  const failed = await setup(t, { requestFailed: true }); await failed.click('submit-device');
  assert.equal(failed.dialog.open, false); assert.match(failed.byId('compatibility-status').textContent, /Could not prepare/); assert.doesNotMatch(failed.win.document.body.textContent, /\/Users\/private/);
  const c = await setup(t, { send: async () => ({ ok: true, id: '/Users/private' }) }); await c.click('submit-device'); c.choose(); await c.click('feedback-send');
  assert.match(c.byId('feedback-status').textContent, /Unable to send/); assert.doesNotMatch(c.win.document.body.textContent, /\/Users\/private/);
});


test('each open refreshes display evidence and saved channel while preserving the complete draft', async t => {
  const display = { edid_id: 'display-one', manufacturer: 'DEL', model_name: 'U2723QE', ddc: { state: 'available', input: 15 } };
  const options = { liveDisplays: [display], receiveBeta: false };
  const c = await setup(t, options); await c.click('submit-device'); c.choose('problem');
  c.fill('feedback-message', 'Keep my description'); c.fill('feedback-contact', 'person@example.com'); await c.click('feedback-diagnostics');
  assert.equal(JSON.parse(c.preview()).report.monitors[0].model, 'U2723QE');
  await c.click('cancel-compatibility');
  options.liveDisplays = [{ ...display, model_name: 'U3223QE', ddc: { state: 'available', input: 17 } }]; options.receiveBeta = true;
  await c.click('submit-device');
  const payload = JSON.parse(c.preview());
  assert.equal(payload.report.monitors[0].model, 'U3223QE'); assert.equal(payload.report.monitors[0].vcp60, 17); assert.equal(payload.report.app.channel, 'beta');
  assert.equal(payload.kind, 'problem'); assert.equal(payload.message, 'Keep my description'); assert.equal(payload.contact, 'person@example.com');
  assert.equal(c.byId('feedback-diagnostics').checked, true); assert.equal(typeof payload.diagnostics, 'string');
  assert.equal(c.requests.filter(method => method === 'display.list').length, 2);
});

test('display probe failure falls back to existing evidence and still opens the dialog', async t => {
  const c = await setup(t, { probeFailed: true, displays: [{ edid_id: 'cached-unique-identifier', manufacturer: 'DEL', model_name: 'Cached monitor' }] });
  await c.click('submit-device'); assert.equal(c.dialog.open, true);
  assert.equal(JSON.parse(c.preview()).report.monitors[0].model, 'Cached monitor');
  const empty = await setup(t, { probeFailed: true }); await empty.click('submit-device');
  assert.equal(empty.dialog.open, true); assert.deepEqual(JSON.parse(empty.preview()).report.monitors, []);
});

test('reopening after a receipt also preserves all entered fields and the diagnostics choice', async t => {
  const c = await setup(t); await c.click('submit-device'); c.choose('problem');
  c.fill('feedback-message', 'Keep this text'); c.fill('feedback-contact', 'person@example.com');
  await c.click('feedback-diagnostics'); await c.click('feedback-send'); await c.click('cancel-compatibility');
  await c.click('submit-device');
  const p = JSON.parse(c.preview());
  assert.equal(p.kind, 'problem'); assert.equal(p.message, 'Keep this text'); assert.equal(p.contact, 'person@example.com');
  assert.equal(c.byId('feedback-diagnostics').checked, true); assert.equal(typeof p.diagnostics, 'string');
  assert.equal(c.requests.filter(method => method === 'display.list').length, 2);
});
