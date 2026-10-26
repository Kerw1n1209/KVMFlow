#!/usr/bin/env node
// Generate the redistributable third-party license bundle for the sidecar
// LAN dependency graph (S9). Reads the actual cargo registry cache; every
// transitive crate of the locked workspace graph gets its license files
// copied under third-party-licenses/<crate>@<version>/.
//
// Usage: node tools/generate-third-party-licenses.js [output-dir]
// The output directory defaults to third-party-licenses/ beside this script.

'use strict';

const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const { spawnSync } = require('node:child_process');

const root = path.resolve(__dirname, '..');
const sidecar = path.join(root, 'sidecar');
const outDir = process.argv[2]
  ? path.resolve(process.argv[2])
  : path.join(root, 'third-party-licenses');

function fail(message) {
  console.error(`generate-third-party-licenses: ${message}`);
  process.exit(1);
}

// The full workspace graph, including the vendored patched snow.
const metadata = spawnSync(
  'cargo',
  ['metadata', '--format-version', '1', '--locked'],
  { cwd: sidecar, encoding: 'utf8', maxBuffer: 256 * 1024 * 1024 },
);
if (metadata.status !== 0) fail(`cargo metadata failed: ${metadata.stderr}`);
const graph = JSON.parse(metadata.stdout);

const own = new Set(['kvmflow-core', 'kvmflow-sidecar']);
// Development-only, never linked into the shipped binaries.
const devOnly = new Set(['snow-upstream']);
// All registry source directories (sparse/regular caches), searched in order.
const registries = (() => {
  const base = path.join(os.homedir(), '.cargo/registry/src');
  if (!fs.existsSync(base)) fail('no cargo registry cache found');
  return fs
    .readdirSync(base, { withFileTypes: true })
    .filter((e) => e.isDirectory())
    .map((e) => path.join(base, e.name));
})();
function crateSourceDir(name, version) {
  for (const registry of registries) {
    const dir = path.join(registry, `${name}-${version}`);
    if (fs.existsSync(dir)) return dir;
  }
  return null;
}

const LICENSE_FILES = [
  'LICENSE', 'LICENSE.md', 'LICENSE.txt', 'LICENSE-MIT', 'LICENSE-APACHE',
  'COPYRIGHT', 'COPYING', 'NOTICE', 'README.md',
];

const crates = graph.packages
  .filter((p) => !own.has(p.name) && !devOnly.has(p.name))
  .sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version));

fs.rmSync(outDir, { recursive: true, force: true });
fs.mkdirSync(outDir, { recursive: true });

const index = [];
let missing = 0;
for (const crate of crates) {
  // Vendored crates live in the workspace itself.
  const vendored = path.join(sidecar, 'vendor', crate.name);
  const cached = crateSourceDir(crate.name, crate.version);
  const source = cached ?? (fs.existsSync(vendored) ? vendored : null);
  if (!source) {
    missing += 1;
    index.push({ ...crate, status: 'source-unavailable' });
    continue;
  }
  const target = path.join(outDir, `${crate.name}@${crate.version}`);
  fs.mkdirSync(target, { recursive: true });
  const copied = [];
  for (const name of LICENSE_FILES) {
    const from = path.join(source, name);
    if (fs.existsSync(from) && fs.statSync(from).isFile()) {
      // READMEs only matter when they carry licensing notes; keep them but
      // mark license files first in the manifest.
      fs.copyFileSync(from, path.join(target, name));
      copied.push(name);
    }
  }
  if (copied.length === 0) {
    index.push({ ...crate, status: 'no-license-file' });
    missing += 1;
    continue;
  }
  index.push({ ...crate, status: 'ok', files: copied });
}

const manifest = {
  generated_at: new Date().toISOString(),
  workspace: 'sidecar (locked graph via cargo metadata --locked)',
  note: 'Complete license material for every transitive crate in the macOS and Windows sidecar binaries. m1ddc (MIT) is covered separately in THIRD_PARTY_NOTICES.md.',
  crates: index.map((c) => ({
    name: c.name,
    version: c.version,
    license: c.license || 'UNKNOWN',
    repository: c.repository || null,
    files: c.files || null,
    status: c.status,
  })),
};

// Bundled renderer fonts are not cargo crates; carry their OFL texts in the
// same redistributable bundle so release packaging has one license root.
const fontSource = path.join(root, 'desktop/src/renderer/assets/fonts');
const fontTarget = path.join(outDir, 'fonts');
fs.mkdirSync(fontTarget, { recursive: true });
const fontFiles = ['OFL-NotoSerifSC.txt', 'OFL-SourceSerif4.txt', 'README.md'];
for (const name of fontFiles) {
  const from = path.join(fontSource, name);
  if (!fs.existsSync(from)) fail(`missing font license file: ${from}`);
  fs.copyFileSync(from, path.join(fontTarget, name));
}
manifest.fonts = {
  source: 'desktop/src/renderer/assets/fonts',
  files: fontFiles,
};
fs.writeFileSync(path.join(outDir, 'manifest.json'), JSON.stringify(manifest, null, 2));

const ok = index.filter((c) => c.status === 'ok').length;
console.log(`third-party licenses: ${ok}/${crates.length} crates`);
for (const crate of index.filter((c) => c.status !== 'ok')) {
  console.error(`  ${crate.status}: ${crate.name}@${crate.version} (${crate.license || '?'})`);
}
if (missing > 0) fail(`${missing} crates lacked license files`);
