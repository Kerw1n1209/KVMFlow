const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const { JSDOM } = require('jsdom');
const flush = () => new Promise(resolve => setImmediate(resolve));

async function client(t, overrides = {}) {
  const root = path.resolve(__dirname, '..');
  const dom = new JSDOM(fs.readFileSync(path.join(root, 'src/renderer/index.html'), 'utf8'), { runScripts: 'outside-only' });
  t.after(() => dom.window.close());
  const calls = [];
  let progress;
  dom.window.kvmflow = {
    ready: Promise.resolve(),
    info: Promise.resolve({ version: '0.2.0' }),
    updates: {
      check: async () => { calls.push('check'); return { version: '0.2.1', notes: '<script>bad()</script>\n修复切换' }; },
      download: async () => { calls.push('download'); },
      install: async () => { calls.push('install'); },
      onProgress: cb => { progress = cb; },
      ...overrides,
    },
  };
  dom.window.eval(fs.readFileSync(path.join(root, 'src/renderer/updates.js'), 'utf8'));
  await flush();
  const doc = dom.window.document;
  return { win: dom.window, doc, calls, progress: data => progress(data), click: async id => { doc.getElementById(id).click(); await flush(); } };
}

test('new version appears globally, verifies download before offering restart, and escapes notes', async t => {
  const c = await client(t);
  assert.equal(c.doc.getElementById('update-toolbar').classList.contains('hidden'), false);
  assert.equal(c.doc.getElementById('update-action').textContent, '更新至 0.2.1');
  assert.equal(c.doc.querySelector('#update-notes script'), null);
  assert.match(c.doc.querySelector('#update-notes p').textContent, /<script>/);
  await c.click('update-action');
  assert.deepEqual(c.calls, ['check', 'download']);
  assert.equal(c.doc.getElementById('update-action').textContent, '重启并更新');
  await c.click('update-action');
  assert.deepEqual(c.calls, ['check', 'download', 'install']);
});

test('background errors stay quiet and a manual check can recover', async t => {
  let attempts = 0;
  const c = await client(t, { check: async () => {
    if (++attempts === 1) throw new Error('offline');
    return null;
  } });
  assert.equal(c.doc.getElementById('update-toolbar').classList.contains('hidden'), true);
  await c.click('check-update');
  assert.match(c.doc.getElementById('update-detail').textContent, /当前已是最新版本/);
});

test('failed verification offers retry without installation, download progress prevents duplicate clicks', async t => {
  let rejectDownload;
  let attempts = 0;
  const c = await client(t, { download: async () => {
    attempts++;
    await new Promise((_, reject) => { rejectDownload = reject; });
  } });
  await c.click('update-action');
  c.progress({ percent: 42 });
  assert.match(c.doc.getElementById('update-status').textContent, /42%/);
  await c.click('update-action');
  assert.equal(attempts, 1);
  rejectDownload(new Error('invalid signature'));
  await flush();
  assert.match(c.doc.getElementById('update-detail').textContent, /下载或验证失败/);
  assert.equal(c.calls.includes('install'), false);
  await c.click('update-action');
  assert.equal(attempts, 2);
  rejectDownload(new Error('offline'));
  await flush();
});

test('unsaved settings block restart and installation errors retry installation without redownloading', async t => {
  let installs = 0;
  const c = await client(t, { install: async () => {
    if (++installs === 1) throw new Error('permission denied');
  } });
  await c.click('update-action');
  c.win.hasUnsavedComputerEditor = () => true;
  await c.click('update-action');
  assert.equal(installs, 0);
  assert.match(c.doc.getElementById('update-status').textContent, /尚未保存/);
  c.win.hasUnsavedComputerEditor = () => false;
  c.win.dispatchEvent(new c.win.Event('settings-saved'));
  await c.click('update-action');
  assert.match(c.doc.getElementById('update-status').textContent, /无法安装/);
  await c.click('update-action');
  assert.equal(installs, 2);
  assert.equal(c.calls.filter(call => call === 'download').length, 1);
});

test('independently saved timing settings do not require saving the computer group before updating', async t => {
  const c = await client(t);
  await c.click('update-action');
  c.doc.querySelector('#timing-absent').dispatchEvent(new c.win.Event('input', { bubbles: true }));
  await c.click('update-action');
  assert.equal(c.calls.includes('install'), true);
});

test('a newer structural edit remains unsaved when an older save finishes', async t => {
  const c = await client(t);
  await c.click('update-action');
  c.win.dispatchEvent(new c.win.CustomEvent('computer-settings-structure-changed', {
    detail: { revision: 1 },
  }));
  c.win.dispatchEvent(new c.win.CustomEvent('settings-saved', {
    detail: { structureRevision: 0 },
  }));
  await c.click('update-action');
  assert.equal(c.calls.includes('install'), false);
  assert.match(c.doc.getElementById('update-status').textContent, /尚未保存/);
});
