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

test('structured v1 report excludes every identifier and preserves only allowed hardware fields', () => {
  const data = fixture(), before = structuredClone(data);
  const { build, renderReport, submission } = load();
  const report = build(data), publicText = renderReport(report, id => messages.en[id]);
  const text = JSON.stringify(report) + publicText + decodeURIComponent(submission(publicText).url.replaceAll('+', ' '));
  for (const secret of ['DEL-A261-0000BEEF', '0000BEEF', 'USB-PRIVATE-SERIAL', 'PERIPHERAL-SERIAL', 'USB-PRIVATE-KEY', 'RAW-EDID-PRIVATE',
    'local-private-device', 'remote-private-device', '0ac24f98-45b9-4eea-8ad8-d5f7cbf0c24e', 'f2312345-1234-4321-8abc-123456789abc',
    '/Users/', 'C:\\Users\\', 'secret-user', '192.168.1.22', 'fd12:3456::42', 'a'.repeat(64), 'Elliott-MacBook', 'GAMING-PRIVATE-PC']) {
    assert.equal(text.includes(secret), false, `${secret} leaked`);
  }
  assert.deepEqual(JSON.parse(JSON.stringify(report)), {
    app: { version: '0.4.0-beta.1', channel: 'beta' }, os: { platform: 'macos', version: '15.7.1', arch: 'aarch64' }, mode: 'normal',
    monitors: [{ vendor: 'DEL', model: 'DELL U2723QE', ddc: true, vcp60: 15, inputs: [{ computer: 1, value: 15, source: 'read' }, { computer: 2, value: 17, source: 'manual' }] }],
    usb: { vendor: 'Terminus Technology', product: 'USB Hub', vid: '1A40', pid: '0101' }, lastSwitch: { accepted: true, readback: 17 },
  });
  assert.match(publicText, /Picture switch: Needs user confirmation/);
  assert.doesNotMatch(text, /Keyboard|9999:8888/);
  assert.deepEqual(data, before);
});

test('private values embedded in allowed hardware strings become null or empty strings', () => {
  for (const secret of ['0000BEEF', '0000beef', 'DEL-A261-0000BEEF', 'GAMING-PRIVATE-PC', '/Users/elliott/private', 'C:\\Users\\elliott\\file',
    '/Volumes/PrivateDisk/report', '/etc/private', '10.0.1.2', '172.16.0.2', '192.168.1.7', 'fd12:3456::42', 'f2312345-1234-4321-8abc-123456789abc', 'a'.repeat(64), 'secret-user', 'remote-private-device', 'owner@example.com']) {
    const data = fixture(); data.displays[0].model_name = `Monitor ${secret}`; data.usb[0].vendor = `Vendor ${secret}`; data.usb[0].product = `Hub ${secret}`;
    const report = load().build(data);
    assert.equal(report.monitors[0].model, null, secret); assert.equal(report.usb.vendor, ''); assert.equal(report.usb.product, '');
    assert.equal(JSON.stringify(report).includes(secret), false);
  }
});

test('unknown evidence is nullable; multi-monitor readback never becomes a global success claim', () => {
  const data = fixture(); data.state = {}; data.displays = []; data.usb = [];
  const report = load().build(data);
  assert.equal(report.monitors[0].model, null); assert.equal(report.monitors[0].ddc, null); assert.equal(report.monitors[0].vcp60, null);
  assert.equal(report.lastSwitch, null); assert.equal(report.usb.vendor, '');
  const multiple = fixture(); multiple.state.lastMultiDeviceReport.per_monitor.push({ commanded: false, readback: 15 });
  assert.equal(load().build(multiple).lastSwitch, null);
  assert.throws(() => load().build({ host: { platform: 'private-hostname' } }));
});

test('report obeys monitor, input, string and integer bounds and only exposes the v1 schema', () => {
  const data = fixture(); data.displays = Array.from({ length: 10 }, (_, i) => ({ edid_id: `display-${i}`, manufacturer: '中'.repeat(65), model_name: '🙂'.repeat(65), ddc: { state: 'available', input: i ? 65536 : 65535 } }));
  data.state.mixed = { enabled: true, execution: [{ step: { step: 'detach', fingerprint: 'PRIVATE' }, ok: true }] };
  const report = load().build(data);
  assert.equal(report.monitors.length, 8); assert.equal(Array.from(report.monitors[0].vendor).length, 64); assert.equal(Array.from(report.monitors[0].model).length, 64);
  assert.equal(report.monitors[0].vcp60, 65535); assert.equal(report.monitors[1].vcp60, null); assert.equal(report.mode, 'mixed');
  assert.deepEqual(Object.keys(report).sort(), ['app', 'lastSwitch', 'mode', 'monitors', 'os', 'usb']);
  data.config.switch_group.devices.push({ port_index: 3, monitors: [{ fingerprint: 'DEL-A261-0000BEEF', local_input: 18, source: 'manual' }] });
  data.displays = fixture().displays;
  assert.equal(load().build(data).monitors[0].inputs.length, 2);
});

test('GitHub text is derived from the structured report and both languages cover every key', () => {
  for (const locale of ['en', 'zh-CN']) {
    const api = load(locale), report = api.build(fixture());
    const rendered = api.renderReport(report, id => messages[locale][id]);
    assert.doesNotMatch(rendered, /compat\./); assert.ok(rendered.includes(report.monitors[0].model));
    for (const key of Object.keys(messages.en).filter(key => /^(compat|feedback)\./.test(key))) assert.ok(messages[locale][key], key);
  }
});

test('form URL contains the exact text and falls back at the encoded boundary', () => {
  const { submission, ISSUE_URL, MAX_URL_LENGTH } = load();
  const report = '显示器 & ? # +\nUSB Hub';
  const result = submission(report), url = new URL(result.url);
  assert.equal(result.copyFirst, false); assert.equal(url.searchParams.get('report'), report); assert.equal(url.searchParams.get('template'), 'compatibility.yml');
  let size = 0; while (!submission('a'.repeat(size + 1)).copyFirst) size++;
  assert.equal(submission('a'.repeat(size)).url.length, MAX_URL_LENGTH); assert.equal(submission('a'.repeat(size + 1)).copyFirst, true);
  assert.equal(submission('设备'.repeat(500)).url, `${ISSUE_URL}?template=compatibility.yml`);
  assert.match(fs.readFileSync(path.join(__dirname, '../../.github/ISSUE_TEMPLATE/compatibility.yml'), 'utf8'), /id: report/);
});
