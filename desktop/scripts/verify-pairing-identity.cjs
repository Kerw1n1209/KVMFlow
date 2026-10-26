// Synthetic renderer verification only. No native runtime or hardware calls.
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const output = path.resolve(__dirname, '../../docs/design/iteration-1/pairing/identity');
const waitLine = (process, stream, pattern) => new Promise((resolve, reject) => {
  let text = '';
  process.once('error', reject);
  process.once('exit', code => reject(new Error(`Process exited: ${code}: ${text}`)));
  stream.on('data', data => { text += data; const match = text.match(pattern); if (match) resolve(match[1]); });
});
(async () => {
  fs.mkdirSync(output, { recursive: true });
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'kvmflow-pairing-'));
  const preview = spawn(process.execPath, [path.join(__dirname, 'preview-pairing.js')], { env: { ...process.env, KVMFLOW_PAIRING_PREVIEW_PORT: '0' } });
  const chrome = spawn(process.env.CHROME_BIN || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', ['--headless=new', '--no-sandbox', '--disable-gpu', '--remote-debugging-port=0', `--user-data-dir=${profile}`, 'about:blank']);
  const timeout = setTimeout(() => { preview.kill(); chrome.kill(); process.exitCode = 1; }, 55000);
  let ws, command;
  try {
    const [port, debuggerUrl] = await Promise.all([
      waitLine(preview, preview.stdout, /127\.0\.0\.1:(\d+)\//),
      waitLine(chrome, chrome.stderr, /DevTools listening on (ws:\/\/[^\s]+)/),
    ]);
    ws = new WebSocket(debuggerUrl);
    await new Promise((resolve, reject) => { ws.addEventListener('open', resolve, { once: true }); ws.addEventListener('error', reject, { once: true }); });
    let id = 0; const pending = new Map();
    ws.addEventListener('message', event => { const reply = JSON.parse(event.data); const request = pending.get(reply.id); if (!request) return; pending.delete(reply.id); reply.error ? request.reject(new Error(reply.error.message)) : request.resolve(reply.result); });
    command = (method, params = {}, sessionId) => new Promise((resolve, reject) => { const next = ++id; pending.set(next, { resolve, reject }); ws.send(JSON.stringify({ id: next, method, params, ...(sessionId ? { sessionId } : {}) })); });
    const { targetId } = await command('Target.createTarget', { url: 'about:blank' });
    const { sessionId } = await command('Target.attachToTarget', { targetId, flatten: true });
    const evaluate = async expression => { const r = await command('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true }, sessionId); if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails)); return r.result.value; };
    const waitFor = async expression => { for (let attempt = 0; attempt < 100; attempt++) { if (await evaluate(expression)) return; await new Promise(resolve => setTimeout(resolve, 50)); } throw new Error(`Timed out: ${expression}`); };
    const capture = async name => {
      await evaluate('document.fonts.ready');
      const metrics = await evaluate(`(() => { const main=document.querySelector('main'); return { width: innerWidth, content:main.scrollWidth, available:main.clientWidth, form:!!document.querySelector('.pair-ip-form') }; })()`);
      assert.ok(metrics.content <= metrics.available + 1, `Horizontal overflow: ${JSON.stringify(metrics)}`);
      const { data } = await command('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false }, sessionId);
      fs.writeFileSync(path.join(output, name + '.png'), Buffer.from(data, 'base64'));
      console.log(JSON.stringify({ name, ...metrics }));
    };
    for (const [language, width] of [['zh-CN', 1020], ['en', 840]]) {
      await command('Emulation.setDeviceMetricsOverride', { width, height: 800, deviceScaleFactor: 1, mobile: false }, sessionId);
      await command('Page.navigate', { url: `http://127.0.0.1:${port}/?scenario=main-1-wait&lang=${language}` }, sessionId);
      await waitFor(`window.KVMFlowPairing?.activeMode()==='add' && !!document.querySelector('.pair-ip')`);
      const suffix = `${language}-${width}`;
      await capture(`discovery-${suffix}`);
      await evaluate(`document.querySelector('.pair-ip').open=true; document.querySelector('.pair-ip input').value='192.168.1.23';`);
      await capture(`manual-${suffix}`);
      await evaluate(`window.__previewPairing.emit('lan.sync',{issues:[{code:'E_LAN_ID_CONFLICT'}]})`);
      await waitFor(`!!document.querySelector('#guide-content [role="alert"]')`);
      await capture(`conflict-${suffix}`);
      await evaluate(`window.KVMFlowPairing.startRoleChoice(); document.querySelector('[data-pair-role="secondary"]').click(); document.querySelector('[data-pair-action="role-next"]').click();`);
      await waitFor(`window.KVMFlowPairing.activeMode()==='secondary-wait'`);
      const secondary = await evaluate(`document.querySelector('#guide-content').textContent`);
      assert.match(secondary, /192\.168\.1\.23/);
      await capture(`secondary-${suffix}`);
    }
  } finally {
    clearTimeout(timeout); preview.kill();
    if (command && ws?.readyState === WebSocket.OPEN) await Promise.race([command('Browser.close').catch(() => {}), new Promise(resolve => setTimeout(resolve, 1000))]);
    ws?.close(); if (chrome.exitCode === null) chrome.kill();
    await new Promise(resolve => chrome.exitCode !== null ? resolve() : chrome.once('exit', resolve));
    await fs.promises.rm(profile, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
