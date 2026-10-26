// Diagnostics leave this machine. Replace computer names, device serials and
// home directories with stable placeholders so cross-references still line up,
// while keeping the hardware evidence (VID:PID, EDID vendor/product, inputs).
(() => {
  const EDID_SERIAL = /\b([A-Z0-9]{3}-[0-9A-F]{4})-(S:[^\s"',;]+|[0-9A-F]{8})(?![0-9A-Za-z])/g;
  const HOME_PATHS = [/\/(?:Users|home)\/[^/\s"']+/g, /[A-Za-z]:\\Users\\[^\\"']+/gi];
  const SERIAL_KEYS = new Set(['serial', 'serial_string']);
  // Short values are only replaced as whole strings; as substrings they would
  // corrupt unrelated text.
  const MIN_SUBSTRING = 3;

  const byPort = (items, key) => [...(Array.isArray(items) ? items : [])]
    .sort((a, b) => Number(a?.[key]) - Number(b?.[key]));

  const UUID = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/gi;
  const UUID_EXACT = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
  const DEVICE_ID_KEY = /(?:^|_)device_id$|^(?:connection_id|initiator|group_id)$/;
  const IPV4 = /\b(?:\d{1,3}\.){3}\d{1,3}\b/g;
  const IPV6_CANDIDATE = /(^|[^0-9A-Za-z])((?:[0-9a-f]{0,4}:){2,}[0-9a-f:.]*(?:%[0-9a-z_.-]+)?)/gi;
  const INSTANCE_PATH = /(?:\\\\[?.]\\)?\b(?:DISPLAY|USB|HID|PCI|SWD)(?:\\|#)[^\s"'<>()[\],;]+/gi;

  function isIpv6(value) {
    const address = value.split('%')[0];
    if (address.split('::').length > 2) return false;
    const groups = address.split(':').filter(Boolean);
    let count = groups.length;
    if (groups.at(-1)?.includes('.')) {
      const ipv4 = groups.pop().split('.');
      if (ipv4.length !== 4 || !ipv4.every(part => /^\d{1,3}$/.test(part) && Number(part) <= 255)) return false;
      count++;
    }
    if (!groups.every(group => /^[0-9a-f]{1,4}$/i.test(group))) return false;
    return address.includes('::') ? count < 8 : count === 8;
  }

  function collectSerials(value, serials) {
    if (Array.isArray(value)) { value.forEach(item => collectSerials(item, serials)); return; }
    if (!value || typeof value !== 'object') return;
    for (const [key, item] of Object.entries(value)) {
      if (SERIAL_KEYS.has(key) && typeof item === 'string' && item) serials.add(item);
      else collectSerials(item, serials);
    }
  }

  function collectLanAddresses(value, addresses) {
    if (Array.isArray(value)) { value.forEach(item => collectLanAddresses(item, addresses)); return; }
    if (!value || typeof value !== 'object') return;
    for (const [key, item] of Object.entries(value)) {
      if (LAN_ADDRESS_KEYS.has(key)) {
        if (typeof item === 'string') addresses.add(item);
        else if (Array.isArray(item)) item.forEach(entry => { if (typeof entry === 'string') addresses.add(entry); });
      } else {
        collectLanAddresses(item, addresses);
      }
    }
  }

  function replaceKnown(text, mapping) {
    if (mapping.has(text)) return mapping.get(text);
    let result = text;
    for (const [original, placeholder] of mapping) {
      if (original.length >= MIN_SUBSTRING) result = result.split(original).join(placeholder);
    }
    return result;
  }

  // LAN keys that carry public keys: keep an 8-hex-char fingerprint so the
  // diagnostics stay correlatable across a support thread without shipping
  // the full pin material.
  const PUBLIC_KEY_KEYS = new Set(['public_key']);
  const LAN_ADDRESS_KEYS = new Set(['addr', 'addresses']);

  function redactDiagnostics(payload, { computerName } = {}) {
    const names = new Map();
    const addName = name => {
      if (typeof name === 'string' && name.trim() && !names.has(name)) names.set(name, `Computer ${names.size + 1}`);
    };
    byPort(payload?.ui?.computers, 'port').forEach(computer => addName(computer?.name));
    byPort(payload?.config?.switch_group?.devices, 'port_index').forEach(device => addName(device?.name));
    [payload?.config?.local_device?.host_label, payload?.config?.host_label, computerName].forEach(addName);

    const identifiers = new Map();
    const idKey = id => UUID_EXACT.test(id) ? id.toLowerCase() : id;
    const addId = id => {
      if (typeof id === 'string' && id && !identifiers.has(idKey(id))) identifiers.set(idKey(id), `device-${identifiers.size + 1}`);
    };
    const collectContext = (value, key = '') => {
      if (typeof value === 'string') {
        if (DEVICE_ID_KEY.test(key)) addId(value);
        for (const [uuid] of value.matchAll(UUID)) addId(uuid);
      } else if (Array.isArray(value)) value.forEach(item => collectContext(item, key));
      else if (value && typeof value === 'object') {
        if (key === 'lan') (Array.isArray(value.peers) ? value.peers : []).forEach(peer => addName(peer?.name));
        for (const [k, item] of Object.entries(value)) {
          for (const [uuid] of k.matchAll(UUID)) addId(uuid);
          collectContext(item, k);
        }
      }
    };
    collectContext(payload);

    const serials = new Map();
    const addSerial = serial => { if (!serials.has(serial)) serials.set(serial, `serial-${serials.size + 1}`); };
    const found = new Set();
    collectSerials(payload, found);
    found.forEach(addSerial);
    // Register EDID serials before replacing, so a bare copy that appears
    // ahead of its fingerprint is still caught.
    const bareSerial = serial => (serial.startsWith('S:') ? serial.slice(2) : serial);
    for (const [, , serial] of JSON.stringify(payload ?? null).matchAll(EDID_SERIAL)) addSerial(bareSerial(serial));

    // Longest first, so a value that contains another is replaced whole.
    const sortedByLength = mapping => new Map([...mapping].sort((a, b) => b[0].length - a[0].length));
    const serialPlaceholders = sortedByLength(serials);
    const namePlaceholders = sortedByLength(names);
    const idPlaceholders = sortedByLength(identifiers);
    const instancePaths = new Map();
    const lanAddresses = new Set();
    collectLanAddresses(payload, lanAddresses);
    const lanAddressPlaceholders = sortedByLength(new Map([...lanAddresses].map((addr, index) => [addr, `lan-addr-${index + 1}`])));
    // Bare IP literals anywhere in a string (e.g. inside error
    // messages or event text) map to the same placeholders, with a stable
    // per-IP suffix so distinct addresses stay distinguishable.
    const ipIndex = new Map();
    const bareIpPlaceholder = ip => {
      const base = [...lanAddresses].find(addr => addr === ip || addr.startsWith(`${ip}:`) || addr.startsWith(`[${ip}]`));
      const basePlaceholder = base ? lanAddressPlaceholders.get(base) : null;
      if (basePlaceholder) return basePlaceholder;
      if (!ipIndex.has(ip)) ipIndex.set(ip, `lan-ip-${ipIndex.size + 1}`);
      return ipIndex.get(ip);
    };
    const visit = (key, text) => {
      let result = text.replace(INSTANCE_PATH, path => {
        if (!instancePaths.has(path)) instancePaths.set(path, `device-path-${instancePaths.size + 1}`);
        return instancePaths.get(path);
      });
      result = HOME_PATHS.reduce((current, pattern) => current.replace(pattern, '~'), result);
      result = result.replace(UUID, uuid => identifiers.get(idKey(uuid)));
      result = replaceKnown(result, idPlaceholders);
      result = result.replace(EDID_SERIAL, (_, prefix, serial) => `${prefix}-${serials.get(bareSerial(serial))}`);
      result = replaceKnown(result, serialPlaceholders);
      result = replaceKnown(result, lanAddressPlaceholders);
      // Bare IPs run AFTER the full addr replacements, so an intact
      // "ip:port" was already mapped; what remains are bare literals
      // (including the halves of unknown-port addresses).
      result = result.replace(IPV6_CANDIDATE, (match, prefix, candidate) => {
        const address = candidate.replace(/\.+$/, '');
        if (isIpv6(address)) return prefix + bareIpPlaceholder(address) + candidate.slice(address.length);
        // Also hide an unbracketed IPv6 endpoint whose final group is a port.
        const at = address.lastIndexOf(':');
        if (/^\d{1,5}$/.test(address.slice(at + 1)) && isIpv6(address.slice(0, at))) return prefix + bareIpPlaceholder(address);
        return match;
      });
      result = result.replace(IPV4, ip => bareIpPlaceholder(ip));
      result = replaceKnown(result, namePlaceholders);
      // Public pins become fingerprints after the generic replacements so
      // the (already anonymized) name map cannot interact with them.
      if (PUBLIC_KEY_KEYS.has(key) && typeof result === 'string' && /^[0-9a-fA-F]{16,}$/.test(result)) {
        return `fp:${result.slice(0, 8)}`;
      }
      return result;
    };
    const visitKeyed = (value, key) => {
      if (Array.isArray(value)) return value.map(item => visitKeyed(item, key));
      if (value && typeof value === 'object') {
        return Object.fromEntries(Object.entries(value).map(([k, item]) => [visit(null, k), visitKeyed(item, k)]));
      }
      return typeof value === 'string' ? visit(key, value) : value;
    };
    return { ...visitKeyed(payload), redacted: true };
  }

  window.KVMFlowRedact = { redactDiagnostics };
})();
