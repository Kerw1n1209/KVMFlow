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

test('lan state is included and redacted without any private key material', () => {
  const redact = load();
  // Mirrors diagnostics.collect's `lan` section (sidecar lan.status) plus
  // the config's `lan` block. The Noise private key lives in
  // lan/identity.key (or DPAPI) and is never part of any diagnostics
  // payload; only the key fingerprint (a 4-byte hash) appears.
  const payload = () => ({
    sidecar: {
      lan: {
        enabled: true,
        listen_port: 47820,
        self: { device_id: 'dev-ABC234DEF', key_fp: '1f22c9e5' },
        peers: [{ device_id: 'dev-XYZ987HIJ', name: 'Elliott的MacBook', online: true, addr: '192.168.1.23:47820', key_fp: '0a0b0c0d' }],
        issues: [
          { code: 'E_LAN_NOT_MEMBER', message: 'Elliott的MacBook 连接未授权' },
          { code: 'E_LAN_IO', message: 'dial 192.168.1.23:47820 and 10.0.0.7 failed' },
        ],
        sync: { 'dev-XYZ987HIJ': { status: 'synced' } },
      },
    },
    config: {
      local_device: { device_id: 'dev-ABC234DEF', host_label: 'Elliott的MacBook' },
      lan: { enabled: true, listen_port: 47820, public_key: 'd3adbeefcafebabe0123456789abcdef' },
      switch_group: { sync: { members: [
        { device_id: 'dev-ABC234DEF', public_key: 'aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899', removed: false },
      ] } },
    },
  });
  const result = redact(payload(), { computerName: 'Elliott的MacBook' });
  const text = JSON.stringify(result);
  // LAN status stays diagnosable through stable IDs, ports and short fingerprints.
  assert.equal(result.sidecar.lan.enabled, true);
  assert.equal(result.sidecar.lan.listen_port, 47820);
  assert.equal(result.sidecar.lan.peers[0].online, true);
  assert.equal(result.sidecar.lan.self.key_fp, '1f22c9e5');
  assert.match(result.sidecar.lan.peers[0].device_id, /^device-\d+$/);
  assert.equal(result.sidecar.lan.peers[0].device_id, Object.keys(result.sidecar.lan.sync)[0]);
  // LAN addresses (IPs) are replaced with stable placeholders (P2-7), and
  // bare private IPs embedded in free text (messages, events) map to the
  // same placeholder as the full address (P3).
  assert.match(result.sidecar.lan.peers[0].addr, /^lan-addr-\d+$/);
  assert.equal(text.includes('192.168.1.23'), false);
  // Full public pins become 8-hex fingerprints; short fingerprint fields
  // stay as they are.
  assert.equal(result.config.lan.public_key, 'fp:d3adbeef');
  assert.equal(
    result.config.switch_group.sync.members[0].public_key,
    'fp:aabbccdd',
    'member pins are fingerprinted'
  );
  // Personal names are replaced everywhere, including lan issues.
  assert.equal(text.includes('Elliott'), false);
  assert.equal(result.sidecar.lan.peers[0].name, 'Computer 1');
  assert.match(result.sidecar.lan.issues[0].message, /^Computer 1 /);
  // The bare IPs in the second message are gone; the one matching the peer
  // address reuses its placeholder, the other gets a stable lan-ip-N.
  const ioMessage = result.sidecar.lan.issues[1].message;
  assert.equal(ioMessage.includes('192.168.1.23'), false, ioMessage);
  assert.equal(ioMessage.includes('10.0.0.7'), false, ioMessage);
  assert.match(ioMessage, /lan-(addr|ip)-\d+/);
  // No private-key material of any shape appears: the payload never
  // contains the identity file's contents (header + 32 raw bytes).
  assert.equal(text.includes('KVMFLOW-LAN-KEY'), false);
});


test('unpaired peer names, UUID values and keys, IPs and instance paths are redacted together', () => {
  const { backend, secrets } = require('./fixtures/diagnostics-private');
  const original = { sidecar: backend() };
  const result = load()(original);
  const text = JSON.stringify(result);
  for (const secret of secrets) assert.equal(text.includes(secret), false, `${secret} leaked`);
  const lan = result.sidecar.lan;
  assert.match(lan.self.device_id, /^device-\d+$/);
  assert.match(lan.peers[0].device_id, /^device-\d+$/);
  assert.notEqual(lan.self.device_id, lan.peers[0].device_id);
  assert.equal(lan.peers[0].connection_id, lan.self.device_id);
  assert.equal(lan.peers[0].initiator, lan.peers[0].device_id);
  assert.equal(Object.keys(lan.sync)[0], lan.peers[0].device_id);
  assert.match(lan.peers[0].name, /^Computer \d+$/);
  assert.ok(result.sidecar.events[0].fields.message.includes(lan.peers[0].device_id));
  assert.deepEqual(original, { sidecar: backend() });
});

test('IPv6 forms are masked without mistaking VID:PID or USB serial keys for addresses', () => {
  const ips = ['::1', '::', '2001:db8:85a3:0:0:8a2e:370:7334', '2001:db8::1', '::ffff:203.0.113.9', 'fe80::1234%en0'];
  const result = load()({ errors: ips.map(ip => `dial [${ip}]:47820 failed`), hardware: ['3837:303c', '3837:303c:serial-1', '15 (0x0F)'] });
  const text = JSON.stringify(result);
  for (const ip of ips.filter(ip => ip !== '::')) assert.equal(text.includes(ip), false, ip);
  assert.deepEqual(Array.from(result.hardware), ['3837:303c', '3837:303c:serial-1', '15 (0x0F)']);
  assert.ok(result.errors.every(error => error.includes('lan-ip-')));
});
