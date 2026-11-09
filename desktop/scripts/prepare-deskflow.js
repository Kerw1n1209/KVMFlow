// Fetches the official Deskflow release that provides the keyboard and mouse
// sharing engine. The engine ships unmodified as a separate process; the
// corresponding source and licences travel with it to meet GPL-2.0.
//
// The macOS app keeps its upstream Developer ID signature, so it is packed
// into one archive instead of being copied file by file: Tauri resource
// copying would drop framework symlinks and invalidate that signature.
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const { execFileSync } = require('node:child_process');

const root = path.resolve(__dirname, '../..');
const version = '1.27.0';
const revision = '081f6478e654a112168879e67a41ce7d7e4ae3af';
const assets = {
  'darwin-arm64': { name: `deskflow-${version}-macos-arm64.dmg`, sha256: 'dd21e79e566314394214906d3338c505d337af536fc580c022ee3fadffad0c97' },
  'win32-x64': { name: `deskflow-${version}-win-x64-portable.zip`, sha256: '8d19004cc3625623489d96580c1b58fd13ee78185689907fd4478595f93fab9a' },
};
const sourceSha256 = 'd2a7b6686deda74095f5a8fe88826d4cdd5554f5e0a5362eec3e654098aa4191';

const argument = (name, fallback) => {
  const at = process.argv.indexOf(name);
  return at < 0 ? fallback : process.argv[at + 1];
};
const target = argument('--target', `${process.platform}-${process.arch}`);
const cache = path.resolve(argument('--cache', process.env.KVMFLOW_DESKFLOW_CACHE || path.join(root, 'desktop/resources/sidecar/.deskflow-cache')));
const destination = path.resolve(argument('--dest', path.join(root, 'desktop/resources/sidecar/deskflow')));

const digest = (file) => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');

function download(name, url, expected) {
  const file = path.join(cache, name);
  if (fs.existsSync(file) && digest(file) === expected) return file;
  const pending = `${file}.download`;
  execFileSync('curl', ['-fL', '--retry', '2', '--connect-timeout', '20', '--max-time', '600', url, '-o', pending], { stdio: 'inherit' });
  if (digest(pending) !== expected) {
    fs.rmSync(pending, { force: true });
    throw new Error(`Checksum mismatch for ${name}`);
  }
  fs.renameSync(pending, file);
  return file;
}

function findFile(directory, name) {
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const full = path.join(directory, entry.name);
    if (entry.isFile() && entry.name === name) return full;
    if (entry.isDirectory() && !entry.isSymbolicLink()) {
      const found = findFile(full, name);
      if (found) return found;
    }
  }
  return null;
}

function main() {
  const asset = assets[target];
  if (!asset) throw new Error(`Deskflow is packaged for ${Object.keys(assets).join(', ')}; not ${target}`);
  fs.mkdirSync(cache, { recursive: true });
  const archive = download(asset.name, `https://github.com/deskflow/deskflow/releases/download/v${version}/${asset.name}`, asset.sha256);
  const source = download(`deskflow-${version}-source.tar.gz`, `https://codeload.github.com/deskflow/deskflow/tar.gz/${revision}`, sourceSha256);

  const staging = fs.mkdtempSync(path.join(os.tmpdir(), 'kvmflow-deskflow-'));
  const content = path.join(staging, 'content');
  fs.mkdirSync(content);
  let mounted = false;
  try {
    let runtimeDir;
    if (target.startsWith('darwin')) {
      if (process.platform !== 'darwin') throw new Error('The macOS engine can only be prepared on macOS');
      execFileSync('hdiutil', ['attach', archive, '-readonly', '-nobrowse', '-mountpoint', content], { input: 'Y\n', stdio: ['pipe', 'ignore', 'inherit'] });
      mounted = true;
      const app = fs.readdirSync(content).find((name) => name.endsWith('.app'));
      if (!app) throw new Error('The Deskflow disk image contains no application');
      runtimeDir = path.join(staging, 'runtime');
      fs.mkdirSync(runtimeDir);
      fs.cpSync(path.join(content, app), path.join(runtimeDir, 'Deskflow.app'), { recursive: true, verbatimSymlinks: true });
    } else {
      execFileSync('tar', ['-xf', archive, '-C', content]);
      const top = fs.readdirSync(content).find((name) => name.startsWith('deskflow-'));
      runtimeDir = path.join(content, top);
    }
    const binaryName = target.startsWith('win32') ? 'deskflow-core.exe' : 'deskflow-core';
    const binary = findFile(runtimeDir, binaryName);
    if (!binary) throw new Error(`The Deskflow release contains no ${binaryName}`);

    fs.rmSync(destination, { recursive: true, force: true });
    fs.mkdirSync(path.join(destination, 'licenses'), { recursive: true });
    const runtimeArchive = path.join(destination, 'runtime.tar.gz');
    // BSD tar otherwise adds AppleDouble ._* entries, which break the bundle seal on extraction.
    execFileSync('tar', ['-czf', runtimeArchive, '-C', runtimeDir, '.'], { env: { ...process.env, COPYFILE_DISABLE: '1' } });

    fs.copyFileSync(source, path.join(destination, `deskflow-${version}-source.tar.gz`));
    execFileSync('tar', ['-xzf', source, '-C', staging]);
    const sourceDir = path.join(staging, `deskflow-${revision}`);
    fs.copyFileSync(path.join(sourceDir, 'LICENSE'), path.join(destination, 'licenses/DESKFLOW-LICENSE'));
    if (fs.existsSync(path.join(sourceDir, 'LICENSES'))) fs.cpSync(path.join(sourceDir, 'LICENSES'), path.join(destination, 'licenses'), { recursive: true });

    const manifest = {
      version,
      target,
      binary: path.relative(runtimeDir, binary).split(path.sep).join('/'),
      binarySha256: digest(binary),
      runtimeSha256: digest(runtimeArchive),
      upstreamAsset: asset.name,
      upstreamSha256: asset.sha256,
      sourceRevision: revision,
      sourceSha256,
    };
    fs.writeFileSync(path.join(destination, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`);
    const bytes = fs.statSync(runtimeArchive).size;
    console.log(`Prepared Deskflow ${version} for ${target}: ${manifest.binary}, runtime ${(bytes / 1048576).toFixed(1)} MB`);
  } finally {
    if (mounted) execFileSync('hdiutil', ['detach', content], { stdio: 'ignore' });
    fs.rmSync(staging, { recursive: true, force: true });
  }
}

if (require.main === module) main();
module.exports = { assets, version, revision };
