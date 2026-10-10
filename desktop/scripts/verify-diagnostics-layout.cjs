// Run in CI against the actual renderer HTML/CSS without building the app.
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const renderer = path.resolve(__dirname, '../src/renderer');
const config = JSON.parse(fs.readFileSync(path.join(renderer, 'i18n/messages.json'), 'utf8'));
const html = fs.readFileSync(path.join(renderer, 'index.html'), 'utf8')
  .replace(/<script\b[^>]*>[\s\S]*?<\/script>/g, '')
  .replace(/<link rel="stylesheet" href="([^"/]+\.css)">/g,
    (_, file) => `<style>${fs.readFileSync(path.join(renderer, file), 'utf8')}</style>`);

(async () => {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'kvmflow-layout-'));
  const chrome = spawn(process.env.CHROME_BIN || 'google-chrome', [
    '--headless=new', '--no-sandbox', '--disable-gpu', '--remote-debugging-port=0',
    `--user-data-dir=${profile}`, 'about:blank',
  ]);
  let ws, command;
  const timeout = setTimeout(() => { chrome.kill(); process.exitCode = 1; }, 45000);
  try {
    const url = await new Promise((resolve, reject) => {
      let output = '';
      chrome.once('error', reject);
      chrome.once('exit', code => reject(new Error(`Chrome exited before connecting: ${code}`)));
      chrome.stderr.on('data', data => {
        output += data;
        const match = output.match(/DevTools listening on (ws:\/\/[^\s]+)/);
        if (match) resolve(match[1]);
      });
    });
    ws = new WebSocket(url);
    await new Promise((resolve, reject) => { ws.addEventListener('open', resolve, { once: true }); ws.addEventListener('error', reject, { once: true }); });
    let nextId = 0;
    const pending = new Map();
    ws.addEventListener('message', event => {
      const result = JSON.parse(event.data);
      const wait = pending.get(result.id);
      if (!wait) return;
      pending.delete(result.id);
      result.error ? wait.reject(new Error(result.error.message)) : wait.resolve(result.result);
    });
    command = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
      const id = ++nextId;
      pending.set(id, { resolve, reject });
      ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
    const { targetId } = await command('Target.createTarget', { url: 'about:blank' });
    const { sessionId } = await command('Target.attachToTarget', { targetId, flatten: true });
    await command('Emulation.setDeviceMetricsOverride', { width: 1058, height: 810, deviceScaleFactor: 1, mobile: false }, sessionId);
    const { frameTree } = await command('Page.getFrameTree', {}, sessionId);
    await command('Page.setDocumentContent', { frameId: frameTree.frame.id, html }, sessionId);
    for (const locale of ['zh-CN', 'en']) {
      for (const count of [0, 1, 3, 8]) {
        const expression = `(() => {
          const messages = ${JSON.stringify(config.messages[locale])};
          document.querySelectorAll('[data-i18n]').forEach(node => { node.textContent = messages[node.dataset.i18n]; });
          document.querySelectorAll('.page > section').forEach(node => node.classList.toggle('hidden', node.id !== 'diagnostics-page'));
          const list = document.getElementById('event-list');
          list.replaceChildren();
          for (let i = 0; i < ${count}; i++) {
            const event = document.createElement('div'); event.className = 'event';
            const copy = document.createElement('div');
            const title = document.createElement('strong'); title.textContent = messages['computer.group.saved'];
            const detail = document.createElement('p'); detail.className = 'hint'; detail.textContent = messages['this.computer.group.will.be.used.for.automatic.switching.after.setup.is.'];
            const time = document.createElement('span'); time.className = 'time'; time.textContent = '21:34';
            copy.append(title, detail); event.append(copy, time); list.append(event);
          }
          if (!${count}) { const empty = document.createElement('p'); empty.className = 'hint'; empty.textContent = messages['no.switch.records.yet']; list.append(empty); }
          const main = document.querySelector('main'); main.scrollTop = main.scrollHeight;
          const style = getComputedStyle(list);
          return { viewport: innerHeight, maxHeight: style.maxHeight, overflow: style.overflowY, listHeight: list.clientHeight, listContent: list.scrollHeight, pageHeight: main.clientHeight, pageContent: main.scrollHeight, width: main.clientWidth, contentWidth: main.scrollWidth, rows: list.querySelectorAll('.event').length, recoveryBottom: document.querySelector('.recovery').getBoundingClientRect().bottom };
        })()`;
        const evaluated = await command('Runtime.evaluate', { expression, returnByValue: true }, sessionId);
        if (evaluated.exceptionDetails) throw new Error(JSON.stringify(evaluated.exceptionDetails));
        const result = evaluated.result.value;
        console.log(JSON.stringify({ locale, count, ...result }));
        assert.equal(result.viewport, 810);
        assert.equal(result.rows, count);
        assert.equal(result.maxHeight, 'none');
        assert.equal(result.overflow, 'visible');
        assert.ok(result.listContent <= result.listHeight + 1, 'Events must not have a nested scroller');
        assert.ok(result.contentWidth <= result.width + 1, 'Diagnostics must not scroll horizontally');
        if (count <= 3) assert.ok(result.pageContent <= result.pageHeight + 1, 'Short event lists must fit without extra page scrolling');
        if (count === 8) assert.ok(result.pageContent > result.pageHeight, 'Long lists need real page scrolling');
        assert.ok(result.recoveryBottom <= result.viewport + 1, 'Recovery instructions must remain reachable');
      }
    }
  } finally {
    clearTimeout(timeout);
    if (command && ws?.readyState === WebSocket.OPEN) {
      await Promise.race([command('Browser.close').catch(() => {}), new Promise(resolve => setTimeout(resolve, 3000))]);
    }
    if (ws) ws.close();
    if (chrome.exitCode === null) chrome.kill();
    await new Promise(resolve => chrome.exitCode !== null ? resolve() : chrome.once('exit', resolve));
    try { await fs.promises.rm(profile, { recursive: true, force: true, maxRetries: 15, retryDelay: 200 }); }
    catch (error) {
      if (!['ENOTEMPTY', 'EBUSY'].includes(error.code)) throw error;
      console.warn('Browser profile still in use; the ephemeral CI runner will clean it up.');
    }
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
