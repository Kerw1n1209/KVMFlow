const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const { execFileSync } = require('node:child_process');
const root = path.resolve(__dirname, '..');
const platform = process.argv[2];
const version = JSON.parse(fs.readFileSync(path.join(root, 'package.json'))).version;
const config = JSON.parse(fs.readFileSync(path.join(root, 'src-tauri/tauri.conf.json')));
if (config.version !== version) throw new Error('package.json and tauri.conf.json versions must match');
const cargoVersion = fs.readFileSync(path.join(root, 'src-tauri/Cargo.toml'), 'utf8').match(/^version = "([^"]+)"/m)?.[1];
if (cargoVersion !== version) throw new Error('package.json and Cargo.toml versions must match');
if (!['mac', 'win'].includes(platform)) throw new Error('usage: node scripts/build-tauri.js <mac|win>');
if (platform === 'mac' && (process.platform !== 'darwin' || process.arch !== 'arm64')) {
  throw new Error('macOS arm64 installers must be built on an Apple Silicon Mac');
}
// Tauri documents NSIS cross-builds from macOS/Linux through cargo-xwin as a
// supported-with-caveats path; MSI is Windows-only and is never produced here.
const crossWindows = platform === 'win' && process.platform !== 'win32';
const target = platform === 'mac' ? 'aarch64-apple-darwin' : 'x86_64-pc-windows-msvc';
const cli = require.resolve('@tauri-apps/cli/tauri.js');
const args = [cli, 'build', '--target', target, '--bundles', platform === 'mac' ? 'app,dmg' : 'nsis'];
if (crossWindows) args.push('--runner', 'cargo-xwin');
const llvmBins = ['/opt/homebrew/opt/llvm/bin', '/opt/homebrew/opt/lld/bin', '/usr/local/opt/llvm/bin', '/usr/local/opt/lld/bin']
  .filter((dir) => fs.existsSync(dir));
const env = { ...process.env };
if (crossWindows) env.PATH = [...llvmBins, process.env.PATH].join(path.delimiter);
if (!env.TAURI_SIGNING_PRIVATE_KEY) {
  const keyFile = env.KVMFLOW_UPDATE_KEY_FILE || path.join(os.homedir(), '.kvmflow-updater.key');
  if (!fs.existsSync(keyFile)) throw new Error('Missing updater signing key. Set TAURI_SIGNING_PRIVATE_KEY or KVMFLOW_UPDATE_KEY_FILE; see docs/auto-updates.md.');
  env.TAURI_SIGNING_PRIVATE_KEY = fs.readFileSync(keyFile, 'utf8').trim();
}
env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD ??= '';
execFileSync(process.execPath, args, { cwd: root, stdio: 'inherit', env });
const bundle = path.join(root, 'src-tauri/target', target, 'release/bundle');
const source = platform === 'mac'
  ? path.join(bundle, 'dmg', `KVMFlow_${version}_aarch64.dmg`)
  : path.join(bundle, 'nsis', `KVMFlow_${version}_x64-setup.exe`);
const output = path.join(root, 'dist', platform === 'mac' ? `KVMFlow-${version}-arm64.dmg` : `KVMFlow Setup ${version}.exe`);
if (!fs.existsSync(source)) throw new Error(`Installer missing: ${source}`);
fs.mkdirSync(path.dirname(output), { recursive: true });
fs.copyFileSync(source, output);
const updateSource = platform === 'mac' ? path.join(bundle, 'macos', 'KVMFlow.app.tar.gz') : source;
const updateOutput = platform === 'mac'
  ? path.join(root, 'dist', `KVMFlow-${version}-macos-arm64.app.tar.gz`) : output;
for (const artifact of [updateSource, `${updateSource}.sig`]) {
  if (!fs.existsSync(artifact)) throw new Error(`Signed updater artifact missing: ${artifact}`);
}
if (updateSource !== source) fs.copyFileSync(updateSource, updateOutput);
fs.copyFileSync(`${updateSource}.sig`, `${updateOutput}.sig`);
execFileSync('cargo', ['run', '--quiet', '--manifest-path', path.join(root, 'tools/update-verifier/Cargo.toml'),
  '--', path.join(root, 'src-tauri/tauri.conf.json'), updateOutput, `${updateOutput}.sig`],
{ cwd: root, stdio: 'inherit' });
console.log(`Installer ready: ${output}`);
console.log(`Signed update ready: ${updateOutput}`);
