const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const { updateBetaChannel } = require('../scripts/update-beta-channel.cjs');
const { compareVersions } = require('../scripts/release-metadata.cjs');
const repository = 'Kerw1n1209/KVMFlow';
const manifest = version => ({ version, notes: version, platforms: Object.fromEntries(['darwin-aarch64', 'windows-x86_64'].map(platform => [platform, { signature: 'signed', url: `https://github.com/${repository}/releases/download/v${version}/${platform}` }])) });
function github({ current, releases = [], draft = false, missingManifest = false, lookupError, uploadError } = {}) {
  let channel = current || draft ? { id: 10, tag_name: 'beta-channel', prerelease: true, draft, assets: current && !missingManifest ? [{ name: 'latest.json' }] : [] } : null;
  let uploaded = current && manifest(current);
  const calls = [];
  const gh = args => {
    calls.push(args);
    if (args[0] === 'api') {
      const fields = Object.fromEntries(args.filter((arg, i) => ['-f', '-F'].includes(args[i - 1])).map(field => { const i = field.indexOf('='); return [field.slice(0, i), field.slice(i + 1)]; }));
      if (args.includes('--paginate')) return JSON.stringify([releases.map(version => ({ tag_name: `v${version}`, draft: false, assets: [{ name: 'latest.json' }] }))]);
      if (args.includes('--method')) {
        assert.equal(fields.prerelease, 'true');
        assert.equal(fields.make_latest, 'false');
        if (!channel) channel = { id: 10, tag_name: fields.tag_name, draft: true, assets: [] };
        channel.prerelease = true;
        if ('draft' in fields) channel.draft = fields.draft === 'true';
        return JSON.stringify(channel);
      }
      if (lookupError) throw Object.assign(new Error('GitHub lookup failed'), { stderr: lookupError });
      if (!channel) throw Object.assign(new Error('not found'), { stderr: 'gh: Not Found (HTTP 404)' });
      return JSON.stringify(channel);
    }
    if (args[1] === 'download') return JSON.stringify(args[2] === 'beta-channel' ? uploaded : manifest(args[2].slice(1)));
    if (args[1] === 'upload') {
      if (uploadError) throw new Error('upload failed');
      uploaded = JSON.parse(fs.readFileSync(args[3], 'utf8'));
      channel.assets = [{ name: 'latest.json' }];
      return '';
    }
    throw Error(`Unexpected command: ${args}`);
  };
  return { gh, calls, state: () => ({ channel, uploaded }) };
}
function run(mock, version) {
  return updateBetaChannel({ candidate: manifest(version), repository, sha: 'a'.repeat(40), gh: mock.gh });
}

test('SemVer orders stable after beta and compares numeric beta identifiers', () => {
  const ordered = ['0.3.9', '0.4.0-beta.1', '0.4.0-beta.2', '0.4.0-beta.10', '0.4.0', '0.4.1-beta.1'];
  for (let i = 1; i < ordered.length; i++) {
    assert.equal(compareVersions(ordered[i - 1], ordered[i]), -1);
    assert.equal(compareVersions(ordered[i], ordered[i - 1]), 1);
  }
  assert.equal(compareVersions('0.4.0+build.1', '0.4.0+build.2'), 0);
  assert.equal(compareVersions('0.4.0-beta.1', '0.4.0-beta.1'), 0);
  assert.equal(compareVersions('0.4.0-alpha', '0.4.0-beta'), -1);
  assert.equal(compareVersions('0.4.0-beta', '0.4.0-beta.1'), -1);
  for (const version of ['0.4.0-beta.01', '0.4.0-beta..1', '0.4.0-', '00.4.0', '../0.4.0']) assert.throws(() => compareVersions(version, '0.4.0'), /Invalid/);
});
test('a stable release advances the beta channel past its prerelease', () => {
  const mock = github({ current: '0.4.0-beta.10', releases: ['0.4.0'] });
  assert.equal(run(mock, '0.4.0'), '0.4.0');
  assert.deepEqual(mock.state().uploaded, manifest('0.4.0'));
  assert.equal(mock.state().channel.prerelease, true);
});
test('a beta release advances only the rolling channel, leaving versioned releases alone', () => {
  const mock = github({ current: '0.3.0', releases: ['0.3.0', '0.4.0-beta.1'] });
  assert.equal(run(mock, '0.4.0-beta.1'), '0.4.0-beta.1');
  assert.ok(mock.calls.filter(args => args.includes('--method')).every(args => /releases\/10$/.test(args[1])));
});
test('an older release completing later cannot roll back the channel', () => {
  const mock = github({ current: '0.5.0-beta.2', releases: ['0.4.0'] });
  assert.equal(run(mock, '0.4.0'), '0.5.0-beta.2');
});
test('missing channel bootstraps as a draft prerelease, then publishes after upload', () => {
  const mock = github();
  assert.equal(run(mock, '0.4.0-beta.1'), '0.4.0-beta.1');
  const create = mock.calls.findIndex(args => args.includes('POST'));
  const upload = mock.calls.findIndex(args => args[1] === 'upload');
  const publish = mock.calls.findIndex(args => args.includes('draft=false'));
  assert.ok(create < upload && upload < publish);
  assert.equal(mock.state().channel.draft, false);
});
test('bootstrap and skipped concurrency jobs select the highest published stable or beta', () => {
  for (const versions of [['0.5.0', '0.4.0-beta.1'], ['0.5.0-beta.10', '0.5.0-beta.2']]) {
    const mock = github({ releases: versions });
    assert.equal(run(mock, '0.4.0-beta.1'), versions[0]);
  }
});
test('a draft with no manifest resumes initial channel creation', () => {
  const mock = github({ draft: true });
  assert.equal(run(mock, '0.4.0-beta.1'), '0.4.0-beta.1');
  assert.equal(mock.calls.some(args => args.includes('POST')), false);
});
test('authentication errors never create a new release and upload errors never publish an empty draft', () => {
  const mock = github({ lookupError: 'gh: forbidden (HTTP 403)' });
  assert.throws(() => run(mock, '0.4.0-beta.1'), /lookup failed/);
  assert.equal(mock.calls.length, 1);
  const failed = github({ uploadError: true });
  assert.throws(() => run(failed, '0.4.0-beta.1'), /upload failed/);
  assert.equal(failed.state().channel.draft, true);
});
test('invalid candidates fail before any channel mutation', () => {
  const mock = github();
  assert.throws(() => updateBetaChannel({ candidate: { version: '0.4.0-beta.1' }, repository, sha: 'a'.repeat(40), gh: mock.gh }), /Incomplete/);
  assert.equal(mock.calls.some(args => args.includes('--method')), false);
});

test('an interrupted replacement repairs a public channel from the highest published version', () => {
  const mock = github({ current: '0.5.0-beta.2', missingManifest: true, releases: ['0.5.0-beta.2', '0.4.0'] });
  assert.equal(run(mock, '0.4.0'), '0.5.0-beta.2');
  assert.equal(mock.state().uploaded.version, '0.5.0-beta.2');
});
