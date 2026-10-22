const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const { execFileSync } = require('node:child_process');
const { compareVersions } = require('./release-metadata.cjs');

const CHANNEL = 'beta-channel';
function github(args) {
  return execFileSync('gh', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
}
function validateManifest(manifest) {
  compareVersions(manifest.version, manifest.version);
  for (const platform of ['darwin-aarch64', 'windows-x86_64']) {
    const entry = manifest.platforms?.[platform];
    if (!entry?.signature?.trim() || !entry.url?.startsWith('https://')) throw Error('Incomplete update manifest');
  }
  return manifest;
}

// Called only after the versioned release is public, under a shared workflow
// concurrency group. The immutable versioned assets remain the download target.
function updateBetaChannel({ candidate, repository, sha, gh = github }) {
  if (!/^[\w.-]+\/[\w.-]+$/.test(repository) || !/^[a-f0-9]{40}$/.test(sha)) throw Error('Invalid release context');
  const base = `repos/${repository}/releases`;
  const api = (route, fields) => {
    const args = ['api', `${base}${route}`];
    if (fields) {
      args.push('--method', route ? 'PATCH' : 'POST');
      for (const [key, value] of Object.entries(fields)) {
        args.push(typeof value === 'boolean' ? '-F' : '-f', `${key}=${value}`);
      }
    }
    return JSON.parse(gh(args));
  };
  const lookup = route => {
    try { return api(route); }
    catch (error) {
      // Authentication, rate limits and transport failures must never be treated
      // as an empty channel. Only an actual GitHub 404 permits bootstrapping.
      if (/\(HTTP 404\)/.test(String(error.stderr))) return null;
      throw error;
    }
  };
  let channel = lookup(`/tags/${CHANNEL}`);
  let newest = validateManifest(candidate);
  // GitHub concurrency retains only one pending job. Scan all published
  // versions so even a superseded job's newer beta is never lost.
  const releases = JSON.parse(gh(['api', base, '--paginate', '--slurp'])).flat();
  let latestRelease = null;
  for (const release of releases) {
    if (release.draft || !release.tag_name.startsWith('v')) continue;
    const version = release.tag_name.slice(1);
    try { compareVersions(version, version); } catch { continue; }
    if (!latestRelease || compareVersions(version, latestRelease.tag_name.slice(1)) > 0) latestRelease = release;
  }
  for (const release of [channel, latestRelease]) {
    if (!release) continue;
    if (!release.assets.some(asset => asset.name === 'latest.json')) {
      // An interrupted --clobber can remove the old asset before upload.
      // Rebuild it from the published versioned manifests on retry.
      if (release === channel) continue;
      throw Error(`Release ${release.tag_name} is missing latest.json`);
    }
    const current = validateManifest(JSON.parse(gh(['release', 'download', release.tag_name, '--repo', repository, '--pattern', 'latest.json', '--output', '-'])));
    if (release !== channel && current.version !== release.tag_name.slice(1)) throw Error('Release tag and manifest version differ');
    if (compareVersions(current.version, newest.version) > 0) newest = current;
  }
  const properties = { prerelease: true, make_latest: 'false' };
  if (!channel) {
    channel = api('', { ...properties, tag_name: CHANNEL, target_commitish: sha, name: 'KVMFlow beta channel', body: 'Latest stable or beta update manifest.', draft: true });
  } else {
    api(`/${channel.id}`, properties);
  }
  // gh replaces the single channel asset; clients can fall back to stable
  // endpoints during the brief delete/upload window.
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'kvmflow-beta-'));
  try {
    const file = path.join(directory, 'latest.json');
    fs.writeFileSync(file, JSON.stringify(newest, null, 2) + '\n');
    gh(['release', 'upload', CHANNEL, file, '--repo', repository, '--clobber']);
    // On first creation the draft becomes visible only after it has a manifest.
    api(`/${channel.id}`, { ...properties, draft: false });
    const published = api(`/tags/${CHANNEL}`);
    if (published.draft || !published.prerelease) throw Error('Beta channel must be a public prerelease');
    const uploaded = JSON.parse(gh(['release', 'download', CHANNEL, '--repo', repository, '--pattern', 'latest.json', '--output', '-']));
    if (JSON.stringify(uploaded) !== JSON.stringify(newest)) throw Error('Beta channel manifest read-back mismatch');
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
  return newest.version;
}

if (require.main === module) {
  const candidate = JSON.parse(fs.readFileSync(process.argv[2] || 'release-assets/latest.json', 'utf8'));
  console.log('Beta channel:', updateBetaChannel({ candidate, repository: process.env.GITHUB_REPOSITORY, sha: process.env.GITHUB_SHA }));
}
module.exports = { updateBetaChannel };
