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

  function walk(value, visit) {
    if (Array.isArray(value)) return value.map(item => walk(item, visit));
    if (value && typeof value === 'object') {
      return Object.fromEntries(Object.entries(value).map(([key, item]) => [visit(key), walk(item, visit)]));
    }
    return typeof value === 'string' ? visit(value) : value;
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
    const lanAddresses = new Set();
    collectLanAddresses(payload, lanAddresses);
    const lanAddressPlaceholders = sortedByLength(new Map([...lanAddresses].map((addr, index) => [addr, `lan-addr-${index + 1}`])));
    // Bare private IPv4 literals anywhere in a string (e.g. inside error
    // messages or event text) map to the same placeholders, with a stable
    // per-IP suffix so distinct addresses stay distinguishable.
    const ipIndex = new Map();
    const bareIpPlaceholder = ip => {
      const base = [...lanAddresses].find(addr => addr.startsWith(`${ip}:`));
      const basePlaceholder = base ? lanAddressPlaceholders.get(base) : null;
      if (basePlaceholder) return basePlaceholder;
      if (!ipIndex.has(ip)) ipIndex.set(ip, `lan-ip-${ipIndex.size + 1}`);
      return ipIndex.get(ip);
    };
    const PRIVATE_IPV4 = /\b(10\.\d{1,3}\.\d{1,3}\.\d{1,3}|192\.168\.\d{1,3}\.\d{1,3}|172\.(1[6-9]|2\d|3[01])\.\d{1,3}\.\d{1,3}|127\.0\.0\.1|169\.254\.\d{1,3}\.\d{1,3})\b/g;
    const visit = (key, text) => {
      let result = HOME_PATHS.reduce((current, pattern) => current.replace(pattern, '~'), text);
      result = result.replace(EDID_SERIAL, (_, prefix, serial) => `${prefix}-${serials.get(bareSerial(serial))}`);
      result = replaceKnown(result, serialPlaceholders);
      result = replaceKnown(result, lanAddressPlaceholders);
      // Bare IPs run AFTER the full addr replacements, so an intact
      // "ip:port" was already mapped; what remains are bare literals
      // (including the halves of unknown-port addresses).
      result = result.replace(PRIVATE_IPV4, ip => bareIpPlaceholder(ip));
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
        return Object.fromEntries(Object.entries(value).map(([k, item]) => [k, visitKeyed(item, k)]));
      }
      return typeof value === 'string' ? visit(key, value) : value;
    };
    return { ...visitKeyed(payload), redacted: true };
  }

  window.KVMFlowRedact = { redactDiagnostics };
})();
