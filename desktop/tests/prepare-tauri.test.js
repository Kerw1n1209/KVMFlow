const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const desktop = path.resolve(__dirname, '..');

function fixture(t, { nativeIcons = false } = {}) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'kvmflow-prepare-test-')));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const cwd = path.join(root, 'desktop');
  fs.mkdirSync(path.join(cwd, 'scripts'), { recursive: true });
  for (const name of ['prepare-tauri.js', 'sync-branding.js']) {
    fs.copyFileSync(path.join(desktop, 'scripts', name), path.join(cwd, 'scripts', name));
  }
  fs.copyFileSync(path.join(desktop, 'package.json'), path.join(cwd, 'package.json'));
  fs.mkdirSync(path.join(root, 'shared/branding'), { recursive: true });
  fs.copyFileSync(path.join(desktop, '../shared/branding/logo.png'), path.join(root, 'shared/branding/logo.png'));
  const icons = ['build/icon.icns', 'build/icon.ico', 'src/renderer/assets/trayTemplate.png'];
  if (nativeIcons) {
    for (const icon of icons) {
      fs.mkdirSync(path.dirname(path.join(cwd, icon)), { recursive: true });
      fs.writeFileSync(path.join(cwd, icon), `existing ${icon}`);
    }
  }
  // Keep the required bundle resource available without compiling or using hardware.
  const helper = path.join(root, 'm1ddc');
  fs.writeFileSync(helper, 'fixture helper');
  const bin = path.join(root, 'bin');
  fs.mkdirSync(bin);
  const toolLog = path.join(root, 'tools.log');
  for (const tool of ['swift', 'iconutil']) {
    fs.writeFileSync(path.join(bin, tool), `#!/bin/sh\necho ${tool} >> "$KVMFLOW_TOOL_LOG"\nexit 91\n`, { mode: 0o755 });
  }
  return {
    root, cwd, icons, toolLog,
    env: { ...process.env, PATH: `${bin}${path.delimiter}${process.env.PATH}`, KVMFLOW_M1DDC: helper, KVMFLOW_TOOL_LOG: toolLog },
  };
}

for (const nativeIcons of [false, true]) {
  test(`Rust pretest avoids native icon tools with ${nativeIcons ? 'existing' : 'missing'} generated icons`, t => {
    const f = fixture(t, { nativeIcons });
    for (let run = 0; run < 2; run++) {
      // Exercise the actual npm hook, so changing package.json can regress this test.
      const result = spawnSync(process.env.npm_execpath ? process.execPath : 'npm',
        process.env.npm_execpath ? [process.env.npm_execpath, 'run', 'pretest:rust'] : ['run', 'pretest:rust'], {
        cwd: f.cwd, env: f.env, encoding: 'utf8',
      });
      assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
      assert.deepEqual(fs.readFileSync(path.join(f.cwd, 'src/renderer/assets/appIcon.png')),
        fs.readFileSync(path.join(f.root, 'shared/branding/logo.png')));
      for (const icon of f.icons) {
        const file = path.join(f.cwd, icon);
        if (nativeIcons) assert.equal(fs.readFileSync(file, 'utf8'), `existing ${icon}`);
        else assert.equal(fs.existsSync(file), false);
      }
      assert.equal(fs.existsSync(f.toolLog), false, 'native icon tools must not run for Rust tests');
      if (process.platform === 'darwin') {
        assert.equal(fs.readFileSync(path.join(f.cwd, 'resources/sidecar/m1ddc'), 'utf8'), 'fixture helper');
      }
    }
  });
}

test('clean helper builds use a writable repository-local module cache', {
  skip: process.platform !== 'darwin',
}, t => {
  const f = fixture(t);
  delete f.env.KVMFLOW_M1DDC;
  delete f.env.CLANG_MODULE_CACHE_PATH;
  const buildDir = path.join(f.root, 'probes/mac/experiments/m1ddc-stock');
  fs.mkdirSync(buildDir, { recursive: true });
  fs.writeFileSync(path.join(buildDir, 'build.sh'), `#!/bin/sh
set -eu
mkdir -p ../../.build/m1ddc-stock
echo "$CLANG_MODULE_CACHE_PATH" > ../../.build/cache-path
echo 'built helper' > ../../.build/m1ddc-stock/m1ddc-selfbuilt
`, { mode: 0o755 });
  const result = spawnSync(process.execPath, ['scripts/prepare-tauri.js', '--rust-tests'], {
    cwd: f.cwd, env: f.env, encoding: 'utf8',
  });
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
  assert.equal(fs.readFileSync(path.join(f.root, 'probes/mac/.build/cache-path'), 'utf8').trim(),
    path.join(f.root, 'probes/mac/.build/clang-module-cache'));
  assert.equal(fs.readFileSync(path.join(f.cwd, 'resources/sidecar/m1ddc'), 'utf8'), 'built helper\n');
});

test('default build preparation still requires native icon tools and propagates failure', {
  skip: process.platform !== 'darwin',
}, t => {
  const f = fixture(t);
  const result = spawnSync(process.execPath, ['scripts/prepare-tauri.js'], {
    cwd: f.cwd, env: f.env, encoding: 'utf8',
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Command failed: swift/);
  assert.equal(fs.readFileSync(f.toolLog, 'utf8'), 'swift\n');
});
