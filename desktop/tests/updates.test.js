const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const { JSDOM } = require('jsdom');
const { initializeI18n } = require('./i18n-helper');
const flush = () => new Promise(resolve => setImmediate(resolve));

async function client(t, overrides = {}, info = {}) {
  const root = path.resolve(__dirname, '..');
  const dom = new JSDOM(fs.readFileSync(path.join(root, 'src/renderer/index.html'), 'utf8'), { runScripts: 'outside-only' });
  t.after(() => dom.window.close());
  const calls = [];
  let progress;
  dom.window.kvmflow = {
    ready: Promise.resolve(),
    info: Promise.resolve({ version: '0.2.0', ...info }),
    updates: {
      check: async () => { calls.push('check'); return { version: '0.2.1', notes: '<script>bad()</script>\n修复切换' }; },
      download: async () => { calls.push('download'); },
      install: async () => { calls.push('install'); },
      onProgress: cb => { progress = cb; },
      ...overrides,
    },
  };
  await initializeI18n(dom.window);
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

test('settings button downloads and installs the available update, with notes expanded', async t => {
  const c = await client(t);
  const button = c.doc.getElementById('check-update');
  assert.equal(button.disabled, false);
  assert.equal(button.textContent, '下载更新');
  assert.equal(button.classList.contains('primary'), true);
  assert.equal(c.doc.getElementById('update-notes').open, true);
  await c.click('check-update');
  assert.deepEqual(c.calls, ['check', 'download']);
  assert.equal(button.textContent, '重启并更新');
  await c.click('check-update');
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
  assert.match(c.doc.getElementById('update-detail').textContent, /已是最新版本/);
});

test('failed verification offers retry without installation, download progress prevents duplicate clicks', async t => {
  let rejectDownload;
  let attempts = 0;
  const c = await client(t, { download: async () => {
    attempts++;
    await new Promise((_, reject) => { rejectDownload = reject; });
  } });
  await c.click('update-action');
  assert.equal(c.doc.getElementById('check-update').disabled, true);
  c.progress({ percent: 42 });
  assert.match(c.doc.getElementById('update-status').textContent, /42%/);
  await c.click('update-action');
  await c.click('check-update');
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

test('update progress and retry messages change language without another network check', async t => {
  let rejectDownload;
  const c = await client(t, { download: async () => new Promise((_, reject) => { rejectDownload = reject; }) });
  await c.click('update-action');
  c.progress({ percent: 42 });
  c.win.KVMFlowI18n.setPreference('en', { persist: false });
  assert.equal(c.doc.querySelector('#update-status').textContent, 'Downloading the update, 42%.');
  assert.equal(c.doc.querySelector('#update-action').textContent, 'Downloading');
  assert.equal(c.doc.querySelector('#update-version').textContent, 'Current version 0.2.0');
  assert.equal(c.doc.querySelector('#update-notes summary').textContent, 'Release notes');
  assert.equal(c.doc.querySelector('#update-notes p').textContent, '<script>bad()</script>\n修复切换');
  assert.equal(c.calls.filter(call => call === 'check').length, 1);
  rejectDownload(new Error('offline'));
  await flush();
  assert.match(c.doc.querySelector('#update-detail').textContent, /Download or verification failed/);
  c.win.KVMFlowI18n.setPreference('zh-CN', { persist: false });
  assert.match(c.doc.querySelector('#update-detail').textContent, /下载或验证失败/);
});

test('update restore failure uses a stable error code in either interface language', async t => {
  const c = await client(t, { install: async () => { throw new Error('E_UPDATE_RESTORE: 自动切换未恢复'); } });
  await c.click('update-action');
  c.win.KVMFlowI18n.setPreference('en', { persist: false });
  await c.click('update-action');
  assert.match(c.doc.querySelector('#update-detail').textContent, /Display restoration is unconfirmed/);
});

test('display recovery failure offers retry, manual download and explicit risky install', async t => {
  const installs = [];
  const c = await client(t, { install: async options => { installs.push(options); throw new Error('E_UPDATE_RESTORE: failed'); } });
  await c.click('update-action');
  await c.click('update-action');
  assert.equal(c.doc.getElementById('update-recovery-actions').classList.contains('hidden'), false);
  assert.equal(c.doc.getElementById('update-manual-download').href, 'https://github.com/Kerw1n1209/KVMFlow/releases');
  assert.match(c.doc.getElementById('update-retry-recovery').textContent, /重试恢复/);
  await c.click('update-retry-recovery');
  assert.equal(installs.length, 2);
  assert.equal(installs[1]?.allowUnrestored, undefined);
  await c.click('update-install-anyway');
  assert.equal(installs[2].allowUnrestored, true);
  assert.equal(c.calls.filter(call => call === 'download').length, 1);
  c.win.KVMFlowI18n.setPreference('en', { persist: false });
  assert.match(c.doc.getElementById('update-retry-recovery').textContent, /Retry recovery/);
  assert.match(c.doc.getElementById('update-manual-download').textContent, /Download manually/);
});

test('declining risky installation keeps recovery retry and working manual download available', async t => {
  let opened = 0; let installs = 0;
  const c = await client(t, {
    install: async options => { installs++; throw new Error(options?.allowUnrestored ? 'E_UPDATE_CANCELLED: declined' : 'E_UPDATE_RESTORE: failed'); },
    openDownload: async () => { opened++; },
  });
  await c.click('update-action'); await c.click('update-action');
  await c.click('update-install-anyway');
  assert.equal(installs, 2);
  assert.equal(c.doc.getElementById('update-recovery-actions').classList.contains('hidden'), false);
  assert.equal(c.doc.getElementById('update-retry-recovery').disabled, false);
  await c.click('update-manual-download');
  assert.equal(opened, 1);
  assert.match(c.doc.getElementById('update-detail').textContent, /已取消安装/);
});

test('manual download stays available when a confirmed risky installer later fails', async t => {
  let opened = 0;
  const c = await client(t, {
    install: async options => { throw new Error(options?.allowUnrestored ? 'installer permission denied' : 'E_UPDATE_RESTORE: failed'); },
    openDownload: async () => { opened++; },
  });
  await c.click('update-action'); await c.click('update-action');
  await c.click('update-install-anyway');
  assert.equal(c.doc.getElementById('update-recovery-actions').classList.contains('hidden'), false);
  await c.click('update-manual-download');
  assert.equal(opened, 1);
});

test('beta toggle defaults off, restores saved preference and uses one translated label', async t => {
  const c = await client(t);
  assert.equal(c.doc.getElementById('receive-beta').checked, false);
  assert.equal(c.doc.getElementById('receive-beta').disabled, false);
  const row = c.doc.getElementById('receive-beta').closest('.response-row');
  assert.equal(row.textContent, '接收测试版');
  c.win.KVMFlowI18n.setPreference('en', { persist: false });
  assert.equal(row.textContent, 'Receive beta updates');
  const restored = await client(t, {}, { receiveBeta: true });
  assert.equal(restored.doc.getElementById('receive-beta').checked, true);
});
test('changing channel saves before rechecking and drops an already downloaded beta', async t => {
  let beta = true;
  const sequence = [];
  const c = await client(t, {
    check: async () => { sequence.push(`check:${beta}`); return beta ? { version: '0.4.0-beta.1', downloaded: true } : null; },
    setBeta: async enabled => { sequence.push(`save:${enabled}`); beta = enabled; return enabled; },
  }, { receiveBeta: true });
  assert.equal(c.doc.getElementById('update-action').textContent, '重启并更新');
  await c.click('receive-beta');
  assert.deepEqual(sequence, ['check:true', 'save:false', 'check:false']);
  assert.equal(c.doc.getElementById('update-toolbar').classList.contains('hidden'), true);
  await c.click('check-update');
  assert.equal(c.calls.includes('install'), false);
});
test('failed channel persistence restores the toggle and retains the verified update', async t => {
  const c = await client(t, { setBeta: async () => { throw Error('disk full'); } });
  await c.click('update-action');
  await c.click('receive-beta');
  assert.equal(c.doc.getElementById('receive-beta').checked, false);
  assert.equal(c.doc.getElementById('update-action').textContent, '重启并更新');
  assert.match(c.doc.getElementById('update-detail').textContent, /渠道未保存/);
  await c.click('update-action');
  assert.equal(c.calls.includes('install'), true);
});
test('channel changes are disabled while a check or download is running', async t => {
  let finish;
  const c = await client(t, { download: async () => new Promise(resolve => { finish = resolve; }) });
  await c.click('update-action');
  assert.equal(c.doc.getElementById('receive-beta').disabled, true);
  finish();
  await flush();
  assert.equal(c.doc.getElementById('receive-beta').disabled, false);
});
