// Capture every role-free setup screen using only the synthetic preview bridge.
// Start preview-pairing.js on 4192, then run with CHROME_BIN set.
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const scenarios = { 'v2-a1': 'start', 'v2-a2': 'wait', 'v2-a3': 'done', 'v2-b1': 'connecting', 'v2-b1a': 'select', 'v2-b2': 'confirm', 'v2-b2a': 'confirm', 'v2-b3': 'try', 'v2-b3a': 'try-hint', 'v2-e1': 'ip', 'v2-e2': 'error', 'v2-e3': 'test-error', 'v2-e4': 'read-error' };
const sizes = [[1020, 700], [840, 700], [1020, 580], [840, 580]];
(async () => {
  const output = process.env.KVMFLOW_PAIRING_SCREENSHOTS || fs.mkdtempSync(path.join(os.tmpdir(), 'kvmflow-pairing-v2-'));
  fs.mkdirSync(output, { recursive: true });
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'kvmflow-pairing-browser-'));
  const chrome = spawn(process.env.CHROME_BIN || 'google-chrome', ['--headless=new', '--no-sandbox', '--disable-gpu', '--remote-debugging-port=0', `--user-data-dir=${profile}`, 'about:blank']);
  let socket, send;
  const deadline = setTimeout(() => { console.error('Pairing screenshot verification timed out'); process.exitCode = 1; chrome.kill(); }, 120000);
  try {
    const url = await new Promise((resolve, reject) => {
      let log = '';
      chrome.once('error', reject);
      chrome.once('exit', (code, signal) => reject(new Error(`Chrome exited before connecting: ${code ?? signal}`)));
      chrome.stderr.on('data', data => { log += data; const match = log.match(/DevTools listening on (ws:\/\/[^\s]+)/); if (match) resolve(match[1]); });
    });
    socket = new WebSocket(url);
    await new Promise((resolve, reject) => { socket.addEventListener('open', resolve, { once: true }); socket.addEventListener('error', reject, { once: true }); });
    let seq = 0; const pending = new Map();
    socket.addEventListener('message', event => { const message = JSON.parse(event.data), call = pending.get(message.id); if (call) { pending.delete(message.id); message.error ? call.reject(new Error(message.error.message)) : call.resolve(message.result); } });
    send = (method, params = {}, sessionId) => new Promise((resolve, reject) => { const id = ++seq; pending.set(id, { resolve, reject }); socket.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) })); });
    const { targetId } = await send('Target.createTarget', { url: 'about:blank' });
    const { sessionId } = await send('Target.attachToTarget', { targetId, flatten: true });
    await send('Page.enable', {}, sessionId);
    const evaluate = async expression => {
      const result = await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true }, sessionId);
      if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
      return result.result.value;
    };
    let captures = 0;
    for (const locale of ['zh-CN', 'en']) for (const [scenario, mode] of Object.entries(scenarios)) {
      await send('Emulation.setDeviceMetricsOverride', { width: 1020, height: 700, deviceScaleFactor: 1, mobile: false }, sessionId);
      await send('Page.navigate', { url: `${process.env.KVMFLOW_PAIRING_PREVIEW_URL || 'http://127.0.0.1:4192'}/?scenario=${scenario}&lang=${locale}` }, sessionId);
      await evaluate(`new Promise((resolve, reject) => { const start = Date.now(); const poll = () => { if (window.__previewPairing?.ready) { if (window.__previewPairing.error) reject(new Error(window.__previewPairing.error)); else resolve(true); } else if (Date.now() - start > 8000) reject(new Error('Preview did not become ready')); else setTimeout(poll, 25); }; poll(); })`);
      await evaluate('document.fonts.ready.then(() => true)');
      assert.equal(await evaluate('window.KVMFlowPairing.activeMode()'), mode, scenario);
      const expected = {
        'v2-a1': ['开始'], 'v2-a2': ['按 IP 地址连接'], 'v2-a3': ['完成'], 'v2-b1': ['取消'], 'v2-b1a': ['连接'],
        'v2-b2': ['下一步'], 'v2-b2a': ['下一步'], 'v2-b3': ['跳过'], 'v2-b3a': [], 'v2-e1': ['连接', '重新搜索'],
        'v2-e2': ['重试'], 'v2-e3': ['发送反馈', '重试'], 'v2-e4': ['跳过此显示器', '重试'],
      };
      if (locale === 'zh-CN') {
        const labels = await evaluate(`[...document.querySelectorAll('#guide-content button:not([data-v2-peer])')].map(node => node.textContent)`);
        assert.deepEqual(labels.slice().sort(), expected[scenario].slice().sort(), `${scenario} final Chinese buttons`);
      }
      for (const [width, height] of sizes) {
        await send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false }, sessionId);
        const geometry = await evaluate(`new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(() => { const main = document.querySelector('main'); main.scrollTop = main.scrollHeight; const controls = [...document.querySelectorAll('#guide-content button, #guide-content input')].map(node => { const r = node.getBoundingClientRect(); return { label: node.textContent || node.id, left: r.left, right: r.right, bottom: r.bottom }; }); resolve({ width: main.clientWidth, contentWidth: main.scrollWidth, steps: document.querySelectorAll('#steps .step').length, dialogOpen: document.querySelector('#pairing-dialog').open, controls }); })))`);
        assert.equal(geometry.steps, 3, scenario);
        assert.equal(geometry.dialogOpen, false, scenario);
        assert.ok(geometry.contentWidth <= geometry.width + 1, `${scenario} horizontal overflow at ${width}×${height}`);
        for (const control of geometry.controls) { assert.ok(control.left >= 0 && control.right <= width + 1, `${scenario} clipped control ${control.label}`); assert.ok(control.bottom <= height + 1, `${scenario} unreachable control ${control.label}`); }
        if (scenario === 'v2-b2a') assert.equal(await evaluate(`document.querySelector('[data-pair-action="v2-next"]').disabled`), true);
        if (scenario === 'v2-b2') assert.equal(await evaluate(`document.querySelector('[data-pair-action="v2-next"]').disabled`), false);
        if (scenario === 'v2-b3a') assert.equal(geometry.controls.length, 0);
        await evaluate('document.querySelector("main").scrollTop = 0');
        const png = await send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false }, sessionId);
        fs.writeFileSync(path.join(output, `${scenario}-${locale}-${width}x${height}.png`), Buffer.from(png.data, 'base64'));
        captures += 1;
      }
      console.log(`${locale} ${scenario}: 4 sizes checked`);
    }
    console.log(JSON.stringify({ captures, output }));
  } finally {
    clearTimeout(deadline);
    if (send && socket?.readyState === WebSocket.OPEN) await Promise.race([send('Browser.close').catch(() => {}), new Promise(resolve => setTimeout(resolve, 2000))]);
    socket?.close();
    if (chrome.exitCode === null && chrome.signalCode === null) chrome.kill();
    if (chrome.exitCode === null && chrome.signalCode === null) await new Promise(resolve => chrome.once('exit', resolve));
    await fs.promises.rm(profile, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
