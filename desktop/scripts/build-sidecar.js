#!/usr/bin/env node
// Optional standalone JSONL server for diagnostics and protocol work.
// Tauri links the runtime as a library; desktop packaging does not use this.

const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');

const target = process.argv[2];
if (!['mac', 'win'].includes(target)) {
  throw new Error('usage: node scripts/build-sidecar.js <mac|win>');
}

const desktopRoot = path.join(__dirname, '..');
const repoRoot = path.join(desktopRoot, '..');
const sidecarRoot = path.join(repoRoot, 'sidecar');
const resources = path.join(desktopRoot, 'resources', 'sidecar');
const isWindows = target === 'win';
const crossTarget = isWindows && process.platform !== 'win32' ? 'x86_64-pc-windows-gnu' : null;
const outputName = isWindows ? 'kvmflow-sidecar.exe' : 'kvmflow-sidecar';
const cargoArgs = ['build', '--release', '-p', 'kvmflow-sidecar'];
if (crossTarget) cargoArgs.push('--target', crossTarget);

execFileSync('cargo', cargoArgs, { cwd: sidecarRoot, stdio: 'inherit' });
const source = path.join(sidecarRoot, 'target', crossTarget || 'release', 'release', outputName);
const resolvedSource = crossTarget
  ? source
  : path.join(sidecarRoot, 'target', 'release', outputName);
if (!fs.existsSync(resolvedSource)) throw new Error(`sidecar build output missing: ${resolvedSource}`);

// m1ddc is not rebuilt here; keep the already staged copy (or the probe build)
// across the clean so macOS packages still ship it.
const m1ddcSources = [
  path.join(resources, 'm1ddc'),
  path.join(repoRoot, 'probes', 'mac', '.build', 'm1ddc-stock', 'm1ddc-selfbuilt'),
];
const m1ddcSource = isWindows ? null : m1ddcSources.find((candidate) => fs.existsSync(candidate));
if (!isWindows && !m1ddcSource) throw new Error('m1ddc binary missing; macOS package would not control displays');
const m1ddcBytes = m1ddcSource ? fs.readFileSync(m1ddcSource) : null;

// Clear the staging dir so a package never carries the other platform's binaries.
fs.rmSync(resources, { recursive: true, force: true });
fs.mkdirSync(resources, { recursive: true });
if (m1ddcBytes) {
  fs.writeFileSync(path.join(resources, 'm1ddc'), m1ddcBytes, { mode: 0o755 });
}
const destination = path.join(resources, outputName);
fs.copyFileSync(resolvedSource, destination);
if (!isWindows) fs.chmodSync(destination, 0o755);
console.log(`Prepared ${target} sidecar: ${destination}`);
