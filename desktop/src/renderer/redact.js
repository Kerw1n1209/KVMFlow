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

  function replaceKnown(text, mapping) {
    if (mapping.has(text)) return mapping.get(text);
    let result = text;
    for (const [original, placeholder] of mapping) {
      if (original.length >= MIN_SUBSTRING) result = result.split(original).join(placeholder);
    }
    return result;
  }

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
    const visit = text => {
      let result = HOME_PATHS.reduce((current, pattern) => current.replace(pattern, '~'), text);
      result = result.replace(EDID_SERIAL, (_, prefix, serial) => `${prefix}-${serials.get(bareSerial(serial))}`);
      result = replaceKnown(result, serialPlaceholders);
      return replaceKnown(result, namePlaceholders);
    };
    return { ...walk(payload, visit), redacted: true };
  }

  window.KVMFlowRedact = { redactDiagnostics };
})();
