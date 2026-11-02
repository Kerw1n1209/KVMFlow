// Capture every onboarding page using only the synthetic preview bridge.
// Start preview-pairing.js on 4192, then run with CHROME_BIN set.
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const scenarios = {
  'v2-welcome': 'welcome', 'v2-theme': 'theme', 'v2-local': 'local', 'v2-local-e4': 'local', 'v2-local-empty': 'local',
  'v2-connect': 'connect', 'v2-connect-empty': 'connect', 'v2-connect-ip': 'connect', 'v2-pairing': 'connecting', 'v2-sas': 'connecting',
  'v2-pair-fail': 'error', 'v2-paired': 'confirm', 'v2-try': 'try', 'v2-try-hint': 'try-hint', 'v2-try-ok': 'try-ok',
  'v2-test-error': 'test-error', 'v2-done': 'done', 'v2-error': 'error',
};
// Page buttons in Chinese, without per-row controls (Pair, Skip this monitor), theme cards and the "?" hint.
const expected = {
  'v2-welcome': ['开始'], 'v2-theme': ['← 返回', '下一步'], 'v2-local': ['← 返回', '下一步'], 'v2-local-e4': ['← 返回', '下一步', '重新检测'],
  'v2-local-empty': ['← 返回', '下一步', '重新检测'], 'v2-connect': ['按 IP 地址连接', '连接', '跳过'], 'v2-connect-empty': ['按 IP 地址连接', '连接', '跳过'],
  'v2-connect-ip': ['按 IP 地址连接', '连接', '跳过'], 'v2-pairing': ['← 返回'], 'v2-sas': ['← 返回', '不一致', '一致'], 'v2-pair-fail': ['← 返回', '重试'],
  'v2-paired': [], 'v2-try': ['跳过'], 'v2-try-hint': ['跳过'], 'v2-try-ok': [], 'v2-test-error': ['发送反馈', '重试'], 'v2-done': ['完成'], 'v2-error': ['发送反馈', '重试'],
};
const nextDisabled = { 'v2-local': false, 'v2-local-e4': true, 'v2-local-empty': true };
const runs = [['zh-CN', 'light'], ['en', 'light'], ['zh-CN', 'dark']];
const sizes = [[1020, 700], [840, 700], [1020, 580], [840, 580]];
(async () => {
  const output = process.env.KVMFLOW_PAIRING_SCREENSHOTS || fs.mkdtempSync(path.join(os.tmpdir(), 'kvmflow-pairing-v2-'));
  fs.mkdirSync(output, { recursive: true });
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'kvmflow-pairing-browser-'));
  const chrome = spawn(process.env.CHROME_BIN || 'google-chrome', ['--headless=new', '--no-sandbox', '--disable-gpu', '--remote-debugging-port=0', `--user-data-dir=${profile}`, 'about:blank']);
  let socket, send;
  const deadline = setTimeout(() => { console.error('Pairing screenshot verification timed out'); process.exitCode = 1; chrome.kill(); }, 300000);
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
    // Entrance animations finish before measuring; looping indicators never do.
    const settled = `new Promise((resolve, reject) => { const start = Date.now(); const poll = () => { if (document.getAnimations().filter(a => a.effect?.getTiming().iterations !== Infinity).every(a => a.playState === 'finished')) resolve(true); else if (Date.now() - start > 8000) reject(new Error('Animations did not settle')); else setTimeout(poll, 25); }; poll(); })`;
    let captures = 0;
    for (const [locale, theme] of runs) for (const [scenario, mode] of Object.entries(scenarios)) {
      await send('Emulation.setDeviceMetricsOverride', { width: 1020, height: 700, deviceScaleFactor: 1, mobile: false }, sessionId);
      await send('Page.navigate', { url: `${process.env.KVMFLOW_PAIRING_PREVIEW_URL || 'http://127.0.0.1:4192'}/?scenario=${scenario}&lang=${locale}&theme=${theme}` }, sessionId);
      await evaluate(`new Promise((resolve, reject) => { const start = Date.now(); const poll = () => { if (window.__previewPairing?.ready) { if (window.__previewPairing.error) reject(new Error(window.__previewPairing.error)); else resolve(true); } else if (Date.now() - start > 8000) reject(new Error('Preview did not become ready')); else setTimeout(poll, 25); }; poll(); })`);
      await evaluate('document.fonts.ready.then(() => true)');
      assert.equal(await evaluate('window.KVMFlowPairing.activeMode()'), mode, scenario);
      assert.equal(await evaluate('document.documentElement.dataset.theme'), theme, `${scenario} theme`);
      if (locale === 'zh-CN') {
        const labels = await evaluate(`[...document.querySelectorAll('#guide-content .ob-view button:not([data-v2-peer]):not([data-fp]):not([data-ob-theme]):not(.ob-help)')].map(node => node.textContent.trim())`);
        assert.deepEqual(labels.slice().sort(), expected[scenario].slice().sort(), `${scenario} final Chinese buttons`);
      }
      if (scenario in nextDisabled) assert.equal(await evaluate(`document.querySelector('[data-pair-action="local-next"]').disabled`), nextDisabled[scenario], `${scenario} Next`);
      if (scenario === 'v2-local-empty') assert.equal(await evaluate(`document.querySelectorAll('[data-pair-action="read-skip"]').length`), 0);
      if (scenario === 'v2-sas') assert.equal(await evaluate(`document.querySelectorAll('.ob-sas > *').length > 0`), true, 'SAS digits');
      for (const [width, height] of sizes) {
        await send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false }, sessionId);
        await evaluate(settled);
        const geometry = await evaluate(`new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(() => { const root = document.querySelector('#guide-content'); const controls = [...root.querySelectorAll('button, input')].filter(node => node.offsetParent && !node.closest('[inert]')).map(node => { const r = node.getBoundingClientRect(); return { label: node.textContent.trim() || node.id || node.type, left: r.left, right: r.right, top: r.top, bottom: r.bottom }; }); resolve({ width: root.clientWidth, contentWidth: root.scrollWidth, height: root.clientHeight, contentHeight: root.scrollHeight, steps: !document.querySelector('#steps') || document.querySelector('#steps').offsetParent === null, chrome: document.querySelector('.nav')?.offsetParent ?? null, dialogOpen: document.querySelector('#pairing-dialog').open, controls }); })))`);
        assert.equal(geometry.steps, true, `${scenario} hides the classic steps`);
        assert.equal(geometry.chrome, null, `${scenario} hides the sidebar`);
        assert.equal(geometry.dialogOpen, false, scenario);
        assert.ok(geometry.contentWidth <= geometry.width + 1, `${scenario} horizontal overflow at ${width}×${height}`);
        assert.ok(geometry.contentHeight <= geometry.height + 1, `${scenario} vertical overflow at ${width}×${height}`);
        for (const control of geometry.controls) {
          assert.ok(control.left >= 0 && control.right <= width + 1, `${scenario} clipped control ${control.label} at ${width}×${height}`);
          assert.ok(control.top >= 0 && control.bottom <= height + 1, `${scenario} unreachable control ${control.label} at ${width}×${height}`);
        }
        const png = await send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false }, sessionId);
        fs.writeFileSync(path.join(output, `${scenario}-${locale}-${theme}-${width}x${height}.png`), Buffer.from(png.data, 'base64'));
        captures += 1;
      }
      console.log(`${locale} ${theme} ${scenario}: 4 sizes checked`);
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
