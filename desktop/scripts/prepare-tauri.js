// Only macOS needs a helper executable. USB/DDC runtime code is linked into
// Tauri; the existing, verified m1ddc implementation remains a subprocess.
const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');
const { syncBranding } = require('./sync-branding');

const root = path.resolve(__dirname, '../..');
// Unit tests use the checked-in PNGs (Tauri also falls back to PNG without icns).
// Native icon export is a packaging concern and needs macOS tools/services that
// may be unavailable in a sandbox. Keep it enabled for dev and release builds.
syncBranding({ nativeIcons: !process.argv.includes('--rust-tests') });
if (process.platform === 'darwin') {
  const destination = path.join(root, 'desktop/resources/sidecar/m1ddc');
  const probe = path.join(root, 'probes/mac/.build/m1ddc-stock/m1ddc-selfbuilt');
  const override = process.env.KVMFLOW_M1DDC;
  let source = override || (fs.existsSync(destination) ? destination : probe);
  if (!fs.existsSync(source) && !override) {
    execFileSync('./build.sh', [], {
      cwd: path.join(root, 'probes/mac/experiments/m1ddc-stock'),
      stdio: 'inherit',
      env: {
        ...process.env,
        CLANG_MODULE_CACHE_PATH: process.env.CLANG_MODULE_CACHE_PATH || path.join(root, 'probes/mac/.build/clang-module-cache'),
      },
    });
    source = probe;
  }
  if (!fs.existsSync(source)) throw new Error('m1ddc is missing; macOS cannot control displays');
  fs.mkdirSync(path.dirname(destination), { recursive: true });
  if (path.resolve(source) !== destination) fs.copyFileSync(source, destination);
  fs.chmodSync(destination, 0o755);
}
