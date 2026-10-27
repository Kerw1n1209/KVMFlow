// Compatibility entry point: role-free setup replaces the old identity/role screens.
// All verification uses the synthetic bridge and writes screenshots to a temporary directory.
const path = require('node:path');
const { spawn } = require('node:child_process');
(async () => {
  const preview = spawn(process.execPath, [path.join(__dirname, 'preview-pairing.js')], { env: { ...process.env, KVMFLOW_PAIRING_PREVIEW_PORT: '0' } });
  try {
    const port = await new Promise((resolve, reject) => {
      let output = '';
      preview.once('error', reject);
      preview.once('exit', (code, signal) => reject(new Error(`Preview exited: ${code ?? signal}`)));
      preview.stdout.on('data', data => { output += data; const match = output.match(/127\.0\.0\.1:(\d+)\//); if (match) resolve(match[1]); });
      preview.stderr.on('data', data => process.stderr.write(data));
    });
    const verifier = spawn(process.execPath, [path.join(__dirname, 'verify-pairing-v2.cjs')], { stdio: 'inherit', env: { ...process.env, KVMFLOW_PAIRING_PREVIEW_URL: `http://127.0.0.1:${port}` } });
    const result = await new Promise((resolve, reject) => { verifier.once('error', reject); verifier.once('exit', (code, signal) => resolve({ code, signal })); });
    if (result.code !== 0) throw new Error(`Pairing verification failed: ${result.code ?? result.signal}`);
  } finally {
    if (preview.exitCode === null && preview.signalCode === null) {
      preview.kill();
      await new Promise(resolve => preview.once('exit', resolve));
    }
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
