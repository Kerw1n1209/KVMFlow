const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const test = require('node:test');
const messages = require('../src/renderer/i18n/messages.json').messages;
const source = fs.readFileSync(path.join(__dirname, '../src/renderer/compat-report.js'), 'utf8');
const load = locale => {
  const window = {};
  vm.runInNewContext(source, { window, URLSearchParams });
  return { ...window.KVMFlowCompatReport, build: data => window.KVMFlowCompatReport.buildReport(data, id => messages[locale || 'en'][id]) };
};
const fixture = () => ({
  host: { platform: 'macos', arch: 'aarch64', osVersion: '15.7.1', version: '0.4.0-beta.1', computerName: 'Elliott-MacBook' },
  config: {
    local_device: { device_id: 'local-private-device', host_label: 'Elliott-MacBook', monitors: [{ fingerprint: 'DEL-A261-0000BEEF', local_input: 15, source: 'learned_active_read' }] },
    trigger: { anchor: { vid_pid: '1a40:0101', serial: 'USB-PRIVATE-SERIAL' }, members: [{ vid_pid: '9999:8888', serial: 'PERIPHERAL-SERIAL' }] },
    switch_group: { group_id: '0ac24f98-45b9-4eea-8ad8-d5f7cbf0c24e', devices: [
      { device_id: 'local-private-device', name: 'Elliott-MacBook', monitors: [{ fingerprint: 'DEL-A261-0000BEEF', local_input: 15, source: 'learned_active_read' }] },
      { device_id: 'remote-private-device', name: 'GAMING-PRIVATE-PC', monitors: [{ fingerprint: 'DEL-A261-0000BEEF', local_input: 17, source: 'manual' }] },
    ] },
  },
  displays: [{ manufacturer: 'DEL', model_name: 'DELL U2723QE', edid_id: 'DEL-A261-0000BEEF', serial_string: '0000BEEF', edid: 'RAW-EDID-PRIVATE', ddc: { state: 'available', input: 15 } }],
  usb: [{ vendor: 'Terminus Technology', product: 'USB Hub', vid_pid: '1a40:0101', serial: 'USB-PRIVATE-SERIAL', key: 'USB-PRIVATE-KEY' }, { vendor: 'Peripheral', product: 'Keyboard', vid_pid: '9999:8888' }],
  state: { lastMultiDeviceReport: { target_device_id: 'remote-private-device', per_monitor: [{ edid_id: 'DEL-A261-0000BEEF', requested: 17, commanded: true, readback: 17, last_error: '/Users/elliott/private 192.168.1.22' }] },
    lan: { public_key: 'a'.repeat(64), addr: '192.168.1.22:47820', peer: 'GAMING-PRIVATE-PC' } },
  extra: { uuid: 'f2312345-1234-4321-8abc-123456789abc', username: 'secret-user', path: 'C:\\Users\\secret-user\\AppData\\file', addr: 'fd12:3456::42' },
});

test('allowlisted report excludes all personal data, raw diagnostics and fingerprint tails', () => {
  const data = fixture(), before = structuredClone(data);
  const { build, submission } = load();
  const report = build(data);
  const decoded = decodeURIComponent(submission(report).url.replaceAll('+', ' '));
  for (const secret of ['DEL-A261-0000BEEF', '0000BEEF', 'USB-PRIVATE-SERIAL', 'PERIPHERAL-SERIAL', 'USB-PRIVATE-KEY', 'RAW-EDID-PRIVATE',
    'local-private-device', 'remote-private-device', '0ac24f98-45b9-4eea-8ad8-d5f7cbf0c24e', 'f2312345-1234-4321-8abc-123456789abc',
    '/Users/', 'C:\\Users\\', 'secret-user', '192.168.1.22', 'fd12:3456::42', 'a'.repeat(64), 'Elliott-MacBook', 'GAMING-PRIVATE-PC']) {
    assert.equal(report.includes(secret), false, `${secret} leaked in report`);
    assert.equal(decoded.includes(secret), false, `${secret} leaked in URL`);
  }
  assert.match(report, /DEL/); assert.match(report, /DELL U2723QE/); assert.match(report, /1A40:0101/);
  assert.match(report, /Terminus Technology/); assert.doesNotMatch(report, /Keyboard|9999:8888/);
  assert.match(report, /15 \(0x0F\) \(Read\)/); assert.match(report, /17 \(0x11\) \(User entered\)/);
  assert.match(report, /command accepted: Yes/); assert.match(report, /Picture switch: Needs user confirmation/);
  assert.deepEqual(data, before);
});

test('sensitive data embedded in allowed hardware names is rejected', () => {
  const { build } = load();
  for (const secret of ['0000BEEF', 'DEL-A261-0000BEEF', 'GAMING-PRIVATE-PC', '/Users/elliott/private', 'C:\\Users\\elliott\\file',
    '/Volumes/PrivateDisk/report', '/etc/private', '10.0.1.2', '172.16.0.2', '192.168.1.7', 'fd12:3456::42', 'f2312345-1234-4321-8abc-123456789abc', 'a'.repeat(64), 'secret-user', 'remote-private-device']) {
    const data = fixture();
    data.displays[0].model_name = `Monitor ${secret}`;
    data.usb[0].vendor = `Vendor ${secret}`;
    data.usb[0].product = `Hub ${secret}`;
    const report = build(data);
    assert.equal(report.includes(secret), false, secret);
    assert.doesNotMatch(report, /Vendor Vendor|Product Hub/);
  }
});

test('unknown evidence stays unknown; no phantom switch, mixed or detach result', () => {
  const data = fixture(); data.state = {}; data.displays = []; data.usb = [];
  const report = load().build(data);
  assert.match(report, /Model: Unknown/); assert.match(report, /DDC available: Unknown/);
  assert.match(report, /command accepted: Unknown/); assert.match(report, /Vendor: Unknown/);
  assert.doesNotMatch(report, /Last requested|Picture switch|Mixed mode|Detach/);
  assert.match(load().build({}), /Unknown/);
});

test('mixed and detach fields require execution evidence and expose only booleans', () => {
  const data = fixture();
  data.state.mixed = { enabled: true, execution: [{ step: { step: 'ddc_push', fingerprint: 'PRIVATE' }, ok: false }, { step: { step: 'detach', fingerprint: 'PRIVATE' }, ok: true, error: '/Users/private' }] };
  const report = load().build(data);
  assert.match(report, /Steps completed: No, Yes/); assert.match(report, /Detach steps completed: Yes/);
  assert.doesNotMatch(report, /PRIVATE|\/Users/);
  data.state.mixed.execution = [];
  assert.doesNotMatch(load().build(data), /Mixed mode|Detach/);
});

test('all generated fields and actions have Chinese and English copy', () => {
  for (const locale of ['en', 'zh-CN']) {
    const report = load(locale).build(fixture());
    assert.doesNotMatch(report, /compat\./);
    for (const key of Object.keys(messages.en).filter(key => key.startsWith('compat.'))) assert.ok(messages[locale][key], key);
  }
});

test('form URL contains the exact preview and falls back at the encoded boundary', () => {
  const { submission, ISSUE_URL, MAX_URL_LENGTH } = load();
  const report = '显示器 & ? # +\nUSB Hub';
  const result = submission(report), url = new URL(result.url);
  assert.equal(result.copyFirst, false); assert.equal(url.searchParams.get('report'), report);
  assert.equal(url.searchParams.get('template'), 'compatibility.yml');
  let size = 0;
  while (!submission('a'.repeat(size + 1)).copyFirst) size++;
  assert.equal(submission('a'.repeat(size)).url.length, MAX_URL_LENGTH);
  assert.equal(submission('a'.repeat(size + 1)).copyFirst, true);
  assert.equal(submission('设备'.repeat(500)).url, `${ISSUE_URL}?template=compatibility.yml`);
  assert.match(fs.readFileSync(path.join(__dirname, '../../.github/ISSUE_TEMPLATE/compatibility.yml'), 'utf8'), /id: report/);
});
