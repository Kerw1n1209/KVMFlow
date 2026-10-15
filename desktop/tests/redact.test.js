const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../src/renderer/redact.js'), 'utf8');
const load = () => {
  const context = { window: {} };
  vm.runInNewContext(source, context);
  return context.window.KVMFlowRedact.redactDiagnostics;
};

const payload = () => ({
  exported_at: '2026-10-15T00:00:00.000Z',
  ui: {
    computers: [
      { id: 'host-mac', name: 'Elliott的MacBook', port: 1, local: true },
      { id: 'host-win', name: 'GAMING-RIG', port: 2, local: false },
    ],
    displays: ['DELL U2723'],
    events: [{ title: '电脑配置已保存', copy: 'GAMING-RIG 已加入切换组' }],
  },
  config: {
    local_device: { device_id: 'host-mac', host_label: 'Elliott的MacBook', monitors: [
      { fingerprint: 'DEL-A0B1-0012ABCD', label: 'DELL U2723', local_input: 15 },
      { fingerprint: 'GSM-5B7F-S:304NTAB1C234', label: 'LG', local_input: 17 },
    ] },
    switch_group: { devices: [{ device_id: 'host-win', name: 'GAMING-RIG', monitors: [
      { fingerprint: 'DEL-A0B1-0012ABCD', local_input: 16 },
    ] }] },
    trigger: { anchor: { vid_pid: '1a40:0101' }, members: [{ vid_pid: '3837:303c', serial: 'KB-9F31-77' }] },
  },
  sidecar: {
    logPath: '/Users/elliott/Library/Logs/KVMFlow/session.jsonl',
    configPath: 'C:\\Users\\Elliott Wu\\AppData\\Roaming\\KVMFlow\\config.json',
    events: [
      { kind: 'usb.snapshot', fields: { devices: [{ key: '3837:303c:KB-9F31-77', vid_pid: '3837:303c', serial: 'KB-9F31-77', product: 'Keyboard' }] } },
      { kind: 'display.list', fields: { displays: [{ edid_id: 'DEL-A0B1-0012ABCD', serial_string: '0012ABCD', model_name: 'DELL U2723' }] } },
      { kind: 'ddc.set', fields: { evidence: { argv: ['/Users/elliott/Applications/KVMFlow.app/Contents/Resources/m1ddc', 'set', 'input', '16'] } } },
    ],
  },
});

test('personal identifiers are replaced with stable placeholders', () => {
  const redact = load();
  const original = payload();
  const result = redact(original, { computerName: 'Elliott的MacBook' });
  const text = JSON.stringify(result);
  for (const secret of ['Elliott', 'GAMING-RIG', 'KB-9F31-77', '0012ABCD', '304NTAB1C234', '/Users/elliott', 'Elliott Wu']) {
    assert.equal(text.includes(secret), false, `${secret} leaked: ${text}`);
  }
  assert.equal(result.redacted, true);
  // Stable mapping keeps cross-references readable.
  assert.equal(result.ui.computers[0].name, 'Computer 1');
  assert.equal(result.config.local_device.host_label, 'Computer 1');
  assert.equal(result.ui.computers[1].name, 'Computer 2');
  assert.equal(result.config.switch_group.devices[0].name, 'Computer 2');
  assert.equal(result.ui.events[0].copy, 'Computer 2 已加入切换组');
  const fingerprint = result.config.local_device.monitors[0].fingerprint;
  assert.match(fingerprint, /^DEL-A0B1-serial-\d+$/);
  assert.equal(result.config.switch_group.devices[0].monitors[0].fingerprint, fingerprint);
  assert.equal(result.sidecar.events[1].fields.displays[0].edid_id, fingerprint);
  assert.match(result.config.local_device.monitors[1].fingerprint, /^GSM-5B7F-serial-\d+$/);
  const serial = result.config.trigger.members[0].serial;
  assert.match(serial, /^serial-\d+$/);
  assert.equal(result.sidecar.events[0].fields.devices[0].key, `3837:303c:${serial}`);
  assert.equal(result.sidecar.logPath, '~/Library/Logs/KVMFlow/session.jsonl');
  assert.equal(result.sidecar.configPath, '~\\AppData\\Roaming\\KVMFlow\\config.json');
  assert.equal(result.sidecar.events[2].fields.evidence.argv[0], '~/Applications/KVMFlow.app/Contents/Resources/m1ddc');
  // Hardware evidence that is not personal stays intact.
  assert.equal(result.config.trigger.members[0].vid_pid, '3837:303c');
  assert.equal(result.config.local_device.monitors[0].local_input, 15);
  assert.equal(result.ui.displays[0], 'DELL U2723');
  // The caller's object is not modified.
  assert.equal(original.ui.computers[0].name, 'Elliott的MacBook');
});

test('a bare EDID serial seen before its fingerprint is still replaced', () => {
  const redact = load();
  const result = redact({ sidecar: { note: 'panel 0012ABCD replied', displays: [{ edid_id: 'DEL-A0B1-0012ABCD' }] } }, {});
  assert.equal(JSON.stringify(result).includes('0012ABCD'), false);
});

test('empty and unusual values do not break redaction', () => {
  const redact = load();
  const result = redact({ ui: { computers: [{ name: '' }] }, config: null, sidecar: { events: [{ fields: { serial: '' } }] } }, {});
  assert.equal(result.ui.computers[0].name, '');
  assert.equal(result.config, null);
  assert.equal(result.sidecar.events[0].fields.serial, '');
});
