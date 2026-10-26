#!/usr/bin/env node
// S9 platform-integration verifier. Checks release-relevant static files:
//   node tools/verify-lan-release.js [--licenses]
// --licenses additionally regenerates and validates the third-party license
// bundle (requires the cargo registry cache; offline otherwise).

'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const root = path.resolve(__dirname, '..');
let failures = 0;

function check(name, ok, detail) {
  if (ok) {
    console.log(`ok    ${name}`);
  } else {
    failures += 1;
    console.error(`FAIL  ${name}${detail ? `: ${detail}` : ''}`);
  }
}

// macOS local-network permission keys (S9). Both live in the bundled
// Info.plist consumed by tauri.macos.conf.json.
const plistPath = path.join(root, 'desktop/src-tauri/Info.plist');
const plist = fs.existsSync(plistPath) ? fs.readFileSync(plistPath, 'utf8') : '';
check('Info.plist exists', fs.existsSync(plistPath));
check(
  'Info.plist declares NSLocalNetworkUsageDescription',
  /<key>NSLocalNetworkUsageDescription<\/key>/.test(plist)
    && /<string>[^<]+<\/string>/.test(plist.split('NSLocalNetworkUsageDescription</key>')[1] || ''),
);
check(
  'Info.plist advertises the _kvmflow._tcp Bonjour service',
  /<key>NSBonjourServices<\/key>/.test(plist)
    && plist.includes('<string>_kvmflow._tcp</string>'),
);
check(
  'tauri.macos.conf.json still bundles the custom Info.plist',
  fs.existsSync(path.join(root, 'desktop/src-tauri/tauri.macos.conf.json'))
    && fs.readFileSync(path.join(root, 'desktop/src-tauri/tauri.macos.conf.json'), 'utf8')
      .includes('"infoPlist": "Info.plist"'),
);

// mDNS service name must match the advertised Bonjour service.
const discovery = fs.readFileSync(
  path.join(root, 'sidecar/crates/sidecar/src/lan/discovery.rs'),
  'utf8',
);
check(
  'lan discovery browses the same _kvmflow._tcp service',
  discovery.includes('_kvmflow._tcp'),
);

// Third-party notices must no longer claim the license bundle is pending.
const notices = fs.readFileSync(path.join(root, 'THIRD_PARTY_NOTICES.md'), 'utf8');
check(
  'THIRD_PARTY_NOTICES references the generated license bundle',
  notices.includes('generate-third-party-licenses'),
);

const fontDir = path.join(root, 'desktop/src/renderer/assets/fonts');
const fontFiles = [
  'NotoSerifSC-Variable.woff2',
  'KVMFlowSerif-Variable.woff2',
  'OFL-NotoSerifSC.txt',
  'OFL-SourceSerif4.txt',
];
check(
  'bundled renderer fonts and OFL texts are present',
  fontFiles.every((name) => fs.existsSync(path.join(fontDir, name))),
);
check(
  'THIRD_PARTY_NOTICES documents the bundled fonts',
  notices.includes('Noto Serif SC') && notices.includes('Source Serif 4'),
);

if (process.argv.includes('--licenses')) {
  const result = spawnSync(
    process.execPath,
    [path.join(root, 'tools/generate-third-party-licenses.js')],
    { encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 },
  );
  check('license bundle generates without gaps', result.status === 0, result.stdout + result.stderr);
  check(
    'license bundle includes the font OFL texts',
    fs.existsSync(path.join(root, 'third-party-licenses/fonts/OFL-NotoSerifSC.txt'))
      && fs.existsSync(path.join(root, 'third-party-licenses/fonts/OFL-SourceSerif4.txt')),
  );
}

if (failures > 0) {
  console.error(`\n${failures} check(s) failed`);
  process.exit(1);
}
console.log('\nall checks passed');
