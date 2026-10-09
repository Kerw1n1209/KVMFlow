const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const root = path.resolve(__dirname, '../..');
const semver = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?$/;

function readVersion(base = root) {
  const version = JSON.parse(fs.readFileSync(path.join(base, 'desktop/package.json'))).version;
  const config = JSON.parse(fs.readFileSync(path.join(base, 'desktop/src-tauri/tauri.conf.json')));
  const cargo = fs.readFileSync(path.join(base, 'desktop/src-tauri/Cargo.toml'), 'utf8').match(/^version = "([^"]+)"/m)?.[1];
  if (!semver.test(version) || config.version !== version || cargo !== version) throw Error('The three desktop version numbers must be valid and identical');
  return version;
}
function validateRef(version, ref = '') {
  if (!ref || ref === 'refs/heads/main') return { version, tag: `v${version}`, publish: false };
  if (ref !== `refs/tags/v${version}`) throw Error('Release tag must exactly match the desktop version; manual runs must use main');
  return { version, tag: `v${version}`, publish: true };
}
function signature(file, version) {
  const encoded = fs.readFileSync(file, 'utf8').trim();
  const decoded = Buffer.from(encoded, 'base64').toString('utf8');
  if (!decoded.includes(`\tversion:${version}\t`) && !decoded.includes(`\tversion:${version}\n`)) throw Error('Updater signature is missing the correct signed version: ' + path.basename(file));
  return encoded;
}
function prepareRelease(directory, version, repository, date = new Date()) {
  if (!semver.test(version) || !/^[\w.-]+\/[\w.-]+$/.test(repository)) throw Error('Invalid release version or repository');
  const macInstaller = `KVMFlow-${version}-arm64.dmg`;
  const macUpdate = `KVMFlow-${version}-macos-arm64.app.tar.gz`;
  const winInstaller = `KVMFlow Setup ${version}.exe`;
  const files = [macInstaller, macUpdate, `${macUpdate}.sig`, winInstaller, `${winInstaller}.sig`];
  for (const name of files) {
    const file = path.join(directory, name);
    if (!fs.existsSync(file) || !fs.statSync(file).isFile() || !fs.statSync(file).size) throw Error('Release asset missing or empty: ' + name);
  }
  const dmg = fs.readFileSync(path.join(directory, macInstaller));
  if (dmg.subarray(-512, -508).toString() !== 'koly') throw Error('Invalid DMG installer');
  if (fs.readFileSync(path.join(directory, winInstaller)).subarray(0, 2).toString() !== 'MZ') throw Error('Invalid Windows installer');
  if (fs.readFileSync(path.join(directory, macUpdate)).subarray(0, 2).toString('hex') !== '1f8b') throw Error('Invalid macOS updater archive');
  const base = `https://github.com/${repository}/releases/download/v${version}/`;
  const manifest = {
    version, notes: `KVMFlow ${version}\n版本说明：https://github.com/${repository}/releases/tag/v${version}`,
    pub_date: date.toISOString(), platforms: {
      'darwin-aarch64': { url: base + encodeURIComponent(macUpdate), signature: signature(path.join(directory, `${macUpdate}.sig`), version) },
      'windows-x86_64': { url: base + encodeURIComponent(winInstaller), signature: signature(path.join(directory, `${winInstaller}.sig`), version) },
    },
  };
  fs.writeFileSync(path.join(directory, 'latest.json'), JSON.stringify(manifest, null, 2) + '\n');
  files.push('latest.json');
  const sums = files.map(name => `${crypto.createHash('sha256').update(fs.readFileSync(path.join(directory, name))).digest('hex')}  ${name}`).join('\n');
  fs.writeFileSync(path.join(directory, 'SHA256SUMS.txt'), sums + '\n');
  return [...files, 'SHA256SUMS.txt'];
}
if (require.main === module) {
  const version = readVersion();
  if (process.argv[2] === 'validate') {
    const result = validateRef(version, process.env.GITHUB_REF);
    console.log(JSON.stringify(result));
    if (process.env.GITHUB_OUTPUT) fs.appendFileSync(process.env.GITHUB_OUTPUT, Object.entries(result).map(([key,value]) => `${key}=${value}`).join('\n') + '\n');
  } else if (process.argv[2] === 'prepare') {
    console.log('Release assets prepared:', prepareRelease(path.resolve(process.argv[3] || 'release-assets'), version, process.env.GITHUB_REPOSITORY || 'Kerw1n1209/KVMFlow').join(', '));
  } else throw Error('usage: node release-metadata.cjs validate|prepare [asset-directory]');
}
module.exports = { readVersion, validateRef, prepareRelease };
