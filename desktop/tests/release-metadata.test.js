const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { readVersion, readReleaseNotes, validateRef, prepareRelease } = require('../scripts/release-metadata.cjs');
const releaseNotes = '设置页可直接下载更新。\n\n- 更新说明默认展开。';
const prepareFixtureRelease = dir => prepareRelease(dir, '0.2.3', 'Kerw1n1209/KVMFlow', new Date('2026-10-09T00:00:00Z'), releaseNotes);
function fixture(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kvmflow-release-test-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const dmg = Buffer.alloc(512); dmg.write('koly');
  fs.writeFileSync(path.join(dir, 'KVMFlow-0.2.3-arm64.dmg'), dmg);
  fs.writeFileSync(path.join(dir, 'KVMFlow Setup 0.2.3.exe'), 'MZinstaller');
  fs.writeFileSync(path.join(dir, 'KVMFlow-0.2.3-macos-arm64.app.tar.gz'), Buffer.from([0x1f,0x8b,0x08]));
  for (const name of ['KVMFlow Setup 0.2.3.exe','KVMFlow-0.2.3-macos-arm64.app.tar.gz']) fs.writeFileSync(path.join(dir, `${name}.sig`), Buffer.from('untrusted comment: fixture\ntrusted comment: timestamp:1\tversion:0.2.3\n').toString('base64'));
  return dir;
}
test('three release versions agree', () => { assert.match(readVersion(), /^\d+\.\d+\.\d+/); });
test('only matching tags publish, main manual runs are build-only', () => {
  assert.equal(validateRef('0.2.3', 'refs/tags/v0.2.3').publish, true);
  assert.equal(validateRef('0.2.3', 'refs/heads/main').publish, false);
  assert.throws(() => validateRef('0.2.3','refs/tags/v0.2.2'));
  assert.throws(() => validateRef('0.2.3','refs/heads/untrusted'));
});
test('both platforms and checksum manifest are required', t => {
  const dir = fixture(t); const files = prepareFixtureRelease(dir);
  assert.equal(files.length,7);
  const manifest = JSON.parse(fs.readFileSync(path.join(dir,'latest.json')));
  assert.equal(manifest.notes, releaseNotes);
  assert.deepEqual(Object.keys(manifest.platforms),['darwin-aarch64','windows-x86_64']);
  assert.match(manifest.platforms['windows-x86_64'].url,/KVMFlow\.Setup\.0\.2\.3\.exe$/);
  assert.ok(files.includes('KVMFlow.Setup.0.2.3.exe'));
  assert.ok(!fs.existsSync(path.join(dir,'KVMFlow Setup 0.2.3.exe')));
  assert.match(fs.readFileSync(path.join(dir,'SHA256SUMS.txt'),'utf8'),/  KVMFlow\.Setup\.0\.2\.3\.exe\n/);
  assert.equal(fs.readFileSync(path.join(dir,'SHA256SUMS.txt'),'utf8').trim().split('\n').length,6);
  fs.unlinkSync(path.join(dir,'KVMFlow.Setup.0.2.3.exe'));
  assert.throws(()=>prepareFixtureRelease(dir),/missing/);
});
test('wrong signed versions cannot produce an update manifest', t => {
  const dir = fixture(t);
  fs.writeFileSync(path.join(dir,'KVMFlow Setup 0.2.3.exe.sig'),Buffer.from('trusted comment: timestamp:1\tversion:0.2.2\n').toString('base64'));
  assert.throws(()=>prepareFixtureRelease(dir),/signed version/);
});
test('release notes are versioned and missing, empty or unsafe versions are rejected', t => {
  const dir = fixture(t);
  const releases = path.join(dir, 'desktop/releases');
  fs.mkdirSync(releases, { recursive: true });
  fs.writeFileSync(path.join(releases, '0.2.3.md'), releaseNotes + '\n');
  assert.equal(readReleaseNotes('0.2.3', dir), releaseNotes);
  assert.throws(() => readReleaseNotes('0.2.4', dir), /ENOENT/);
  assert.throws(() => readReleaseNotes('../0.2.3', dir), /Invalid/);
  fs.writeFileSync(path.join(releases, '0.2.3.md'), ' \n');
  assert.throws(() => readReleaseNotes('0.2.3', dir), /empty/);
});
