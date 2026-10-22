const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const { spawnSync } = require('node:child_process');

const root = path.resolve(__dirname, '../..');

// S9 platform integration: the verifier script checks the macOS local-network
// permission keys, the Bonjour service name and the third-party notices
// references. Run the real script so drift between script and release files
// fails here, not at release time.
test('verify-lan-release validates the LAN release prerequisites', () => {
  const result = spawnSync(
    process.execPath,
    [path.join(root, 'tools/verify-lan-release.js')],
    { encoding: 'utf8' },
  );
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
  assert.match(result.stdout, /all checks passed/);
});

test('Info.plist carries the local network usage description and Bonjour service', () => {
  const plist = fs.readFileSync(path.join(root, 'desktop/src-tauri/Info.plist'), 'utf8');
  assert.match(plist, /<key>NSLocalNetworkUsageDescription<\/key>/);
  assert.match(plist, /<key>NSBonjourServices<\/key>/);
  assert.match(plist, /<string>_kvmflow\._tcp<\/string>/);
  // The usage description must be a non-empty localized sentence.
  const section = plist.split('NSLocalNetworkUsageDescription</key>')[1] ?? '';
  const text = /<string>([^<]+)<\/string>/.exec(section)?.[1] ?? '';
  assert.ok(text.trim().length >= 10, `usage description too short: ${text}`);
});
