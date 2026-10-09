const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { readVersion, validateRef, prepareRelease } = require('../scripts/release-metadata.cjs');
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
  const dir = fixture(t); const files = prepareRelease(dir,'0.2.3','Kerw1n1209/KVMFlow');
  assert.equal(files.length,7);
  const manifest = JSON.parse(fs.readFileSync(path.join(dir,'latest.json')));
  assert.deepEqual(Object.keys(manifest.platforms),['darwin-aarch64','windows-x86_64']);
  assert.match(manifest.platforms['windows-x86_64'].url,/KVMFlow%20Setup%200\.2\.3\.exe$/);
  assert.equal(fs.readFileSync(path.join(dir,'SHA256SUMS.txt'),'utf8').trim().split('\n').length,6);
  fs.unlinkSync(path.join(dir,'KVMFlow Setup 0.2.3.exe'));
  assert.throws(()=>prepareRelease(dir,'0.2.3','Kerw1n1209/KVMFlow'),/missing/);
});
test('wrong signed versions cannot produce an update manifest', t => {
  const dir = fixture(t);
  fs.writeFileSync(path.join(dir,'KVMFlow Setup 0.2.3.exe.sig'),Buffer.from('trusted comment: timestamp:1\tversion:0.2.2\n').toString('base64'));
  assert.throws(()=>prepareRelease(dir,'0.2.3','Kerw1n1209/KVMFlow'),/signed version/);
});
