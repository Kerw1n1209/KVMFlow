const first = '11111111-2222-4333-8444-555555555555';
const second = 'aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee';
const peerName = "Alice's ThinkPad";
const ipv6 = 'fe80::a123:b456:c789:d012%en0';
const displayPath = 'DISPLAY\\DEL40A9\\5&1234abcd&0&UID4352';
const usbPath = 'USB\\VID_1234&PID_5678\\6&9876abcd&0&2';
const backend = () => ({
  lan: {
    self: { device_id: first },
    peers: [{ name: peerName, device_id: second, connection_id: first, initiator: second,
      last_error: `${peerName}: dial [${ipv6}]:47820 and 2001:db8::42 failed` }],
    sync: { [second]: { device_id: second, status: 'synced' } },
  },
  events: [{ kind: 'network.error', fields: { message: `peer ${peerName} peer_${second.toUpperCase()} from 203.0.113.9`, instance: displayPath, usb: usbPath } }],
});
const secrets = [first, second, second.toUpperCase(), peerName, ipv6, '2001:db8::42', '203.0.113.9', displayPath, usbPath];
module.exports = { backend, secrets, first, second };
