const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const root = path.resolve(__dirname, '../..');
const semver = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;

function parseVersion(version) {
  const match = typeof version === 'string' && version.match(semver);
  if (!match || match[4]?.split('.').some(id => /^0\d+$/.test(id))) throw Error('Invalid release version');
  return { core: match.slice(1, 4).map(BigInt), pre: match[4]?.split('.') || [] };
}
function compareVersions(left, right) {
  const a = parseVersion(left), b = parseVersion(right);
  const compare = (x, y) => x === y ? 0 : x > y ? 1 : -1;
  for (let i = 0; i < 3; i++) {
    const result = compare(a.core[i], b.core[i]);
    if (result) return result;
  }
  if (!a.pre.length || !b.pre.length) return compare(!a.pre.length, !b.pre.length);
  for (let i = 0; i < Math.max(a.pre.length, b.pre.length); i++) {
    if (a.pre[i] === undefined) return -1;
    if (b.pre[i] === undefined) return 1;
    if (a.pre[i] === b.pre[i]) continue;
    const numericA = /^\d+$/.test(a.pre[i]), numericB = /^\d+$/.test(b.pre[i]);
    if (numericA && numericB) return compare(BigInt(a.pre[i]), BigInt(b.pre[i]));
    if (numericA !== numericB) return numericA ? -1 : 1;
    return compare(a.pre[i], b.pre[i]);
  }
  return 0;
}

function readVersion(base = root) {
  const version = JSON.parse(fs.readFileSync(path.join(base, 'desktop/package.json'))).version;
  const config = JSON.parse(fs.readFileSync(path.join(base, 'desktop/src-tauri/tauri.conf.json')));
  const cargo = fs.readFileSync(path.join(base, 'desktop/src-tauri/Cargo.toml'), 'utf8').match(/^version = "([^"]+)"/m)?.[1];
  parseVersion(version);
  if (config.version !== version || cargo !== version) throw Error('The three desktop version numbers must be valid and identical');
  return version;
}
function validateRef(version, ref = '') {
  parseVersion(version);
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
function readReleaseNotes(version, base = root) {
  parseVersion(version);
  const notes = fs.readFileSync(path.join(base, 'desktop/releases', `${version}.md`), 'utf8').trim();
  if (!notes) throw Error('Release notes must not be empty');
  return notes;
}
function prepareRelease(directory, version, repository, date = new Date(), notes = readReleaseNotes(version)) {
  parseVersion(version);
  if (!/^[\w.-]+\/[\w.-]+$/.test(repository)) throw Error('Invalid release version or repository');
  if (typeof notes !== 'string' || !notes.trim()) throw Error('Release notes must not be empty');
  const macInstaller = `KVMFlow-${version}-arm64.dmg`;
  const macUpdate = `KVMFlow-${version}-macos-arm64.app.tar.gz`;
  // GitHub normalizes spaces in asset names to dots. Normalize before hashing
  // and building URLs so downloaded filenames, checksums and URLs agree.
  const winInstaller = `KVMFlow.Setup.${version}.exe`;
  const winSource = `KVMFlow Setup ${version}.exe`;
  for (const suffix of ['', '.sig']) {
    const source = path.join(directory, winSource + suffix);
    const destination = path.join(directory, winInstaller + suffix);
    if (fs.existsSync(source)) {
      if (fs.existsSync(destination)) throw Error('Duplicate Windows release asset');
      fs.renameSync(source, destination);
    }
  }
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
    version, notes: notes.trim(),
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
    readReleaseNotes(version);
    const result = validateRef(version, process.env.GITHUB_REF);
    console.log(JSON.stringify(result));
    if (process.env.GITHUB_OUTPUT) fs.appendFileSync(process.env.GITHUB_OUTPUT, Object.entries(result).map(([key,value]) => `${key}=${value}`).join('\n') + '\n');
  } else if (process.argv[2] === 'prepare') {
    console.log('Release assets prepared:', prepareRelease(path.resolve(process.argv[3] || 'release-assets'), version, process.env.GITHUB_REPOSITORY || 'Kerw1n1209/KVMFlow').join(', '));
  } else throw Error('usage: node release-metadata.cjs validate|prepare [asset-directory]');
}
module.exports = { readVersion, readReleaseNotes, validateRef, prepareRelease, compareVersions };
