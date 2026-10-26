// Public reports are built from an explicit allowlist, never from diagnostics.
(() => {
  const ISSUE_URL = 'https://github.com/Kerw1n1209/KVMFlow/issues/new';
  const MAX_URL_LENGTH = 2000; // Conservative browser/Windows launch budget, after encoding.
  const array = value => Array.isArray(value) ? value : [];
  const input = value => Number.isInteger(value) && value >= 0 && value <= 255;

  function buildReport(data, t = id => id) {
    const { host = {}, config = {}, displays = [], usb = [], state = {} } = data;
    const unknown = t('compat.unknown');
    // Hardware names are arbitrary strings too. Gather known identifiers before
    // admitting any text, including bare EDID serial tails embedded in names.
    const secrets = new Set();
    const sensitiveKeys = /^(?:serial|serial_string|fingerprint|edid_id|uuid|device_id|group_id|public_key|key|computerName|host_label|username|computer_name|addr|addresses|path|logPath|configPath)$/i;
    const collect = (value, key = '') => {
      if (typeof value === 'string') {
        if (sensitiveKeys.test(key) && value) secrets.add(value);
        for (const match of value.matchAll(/\b[A-Z0-9]{3}-[0-9A-F]{4}-(?:S:([^\s"',;]+)|([0-9A-F]{8}))(?![0-9A-Za-z])/g)) secrets.add(match[1] || match[2]);
      } else if (Array.isArray(value)) value.forEach(item => collect(item, key));
      else if (value && typeof value === 'object') Object.entries(value).forEach(([k, v]) => collect(v, k));
    };
    collect(data);
    array(config.switch_group?.devices).forEach(device => { if (device.name) secrets.add(device.name); });
    const unsafe = /(?:[a-z]:[\\/]|\/(?:Users|home|private|tmp|var)\/|https?:|[\\/]|\b(?:\d{1,3}\.){3}\d{1,3}\b|\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b|\b[A-Z0-9]{3}-[0-9A-F]{4}-|\b[0-9a-f]{32,}\b|[0-9a-f]*:[0-9a-f:]*:[0-9a-f:]*|[\r\n\x00-\x1f])/i;
    const text = value => {
      if (typeof value !== 'string' || !value.trim() || unsafe.test(value) || [...secrets].some(secret => value.includes(secret))) return unknown;
      return value.trim().slice(0, 96).replace(/[\\`*_{}\[\]<>|#]/g, '\\$&');
    };
    const yesNo = value => value === true ? t('compat.yes') : value === false ? t('compat.no') : unknown;
    const source = value => ({ learned_active_read: t('compat.read'), manual: t('compat.manual'), manual_entry: t('compat.manual') })[value] || unknown;
    const valueText = value => input(value) ? `${value} (0x${value.toString(16).toUpperCase().padStart(2, '0')})` : unknown;
    const line = (key, value) => `- ${t(`compat.${key}`)}: ${value}`;
    const lines = ['## KVMFlow', line('os', ({ macos: 'macOS', windows: 'Windows', linux: 'Linux' })[host.platform] || unknown),
      line('os.version', /^[\d.]+$/.test(host.osVersion || '') ? host.osVersion : unknown),
      line('arch', ['aarch64', 'x86_64', 'x86', 'arm'].includes(host.arch) ? host.arch : unknown),
      line('version', /^\d+\.\d+\.\d+(?:-[\w.]+)?$/.test(host.version || '') ? host.version : unknown)];
    const configured = array(config.local_device?.monitors);
    const inventory = array(displays);
    const monitors = [...inventory, ...configured.filter(m => !inventory.some(d => d.edid_id === m.fingerprint)).map(m => ({ edid_id: m.fingerprint }))];
    if (!monitors.length) lines.push('', `## ${t('compat.monitors')}`, unknown);
    monitors.forEach((display, index) => {
      const fp = display.edid_id;
      const outcome = fp ? array((state.lastMultiDeviceReport || state.lastReport)?.per_monitor).find(m => m.edid_id === fp) : null;
      const ddc = display.ddc?.state;
      lines.push('', `## ${t('compat.monitor')} ${index + 1}`, line('vendor', text(display.manufacturer)), line('model', text(display.model_name)),
        line('ddc', ddc === 'available' ? t('compat.yes') : ddc === 'unavailable' ? t('compat.no') : unknown),
        line('read.60', ddc === 'available' ? valueText(display.ddc.input) : unknown),
        line('write.60', outcome ? yesNo(outcome.commanded) : unknown));
      const mappings = array(config.switch_group?.devices).map((device, i) => ({ ...array(device.monitors).find(m => m.fingerprint === fp), port: i + 1 }));
      if (!mappings.some(m => input(m.local_input))) mappings.push({ ...configured.find(m => m.fingerprint === fp), port: 1 });
      mappings.filter(m => input(m.local_input)).forEach(m => lines.push(line('input', `${t('compat.computer')} ${m.port}: ${valueText(m.local_input)} (${source(m.source)})`)));
      if (outcome) lines.push(line('requested', valueText(outcome.requested)), line('readback', valueText(outcome.readback)), line('picture', t('compat.unconfirmed')));
    });
    lines.push('', `## ${t('compat.usb')}`);
    const vidPid = config.trigger?.anchor?.vid_pid;
    const devices = array(usb).filter(device => device.vid_pid === vidPid);
    if (/^[0-9a-f]{4}:[0-9a-f]{4}$/i.test(vidPid || '')) {
      lines.push(line('vidpid', vidPid.toUpperCase()));
      if (!devices.length) lines.push(line('vendor', unknown), line('product', unknown));
      devices.forEach(device => lines.push(line('vendor', text(device.vendor)), line('product', text(device.product))));
    } else lines.push(unknown);
    const execution = array(state.mixed?.execution);
    if (execution.length) {
      lines.push('', `## ${t('compat.mixed')}`, line('steps', execution.map(entry => yesNo(entry.ok)).join(', ')));
      const detach = execution.filter(entry => entry.step?.step === 'detach');
      if (detach.length) lines.push(line('detach', detach.map(entry => yesNo(entry.ok)).join(', ')));
    }
    lines.push('', `## ${t('compat.notes')}`, line('purchase', ''), line('experience', ''));
    return lines.join('\n');
  }

  function submission(report) {
    const params = new URLSearchParams({ template: 'compatibility.yml', title: '[Compatibility] My devices', report });
    const url = `${ISSUE_URL}?${params}`;
    return url.length <= MAX_URL_LENGTH ? { url, copyFirst: false } : { url: `${ISSUE_URL}?template=compatibility.yml`, copyFirst: true };
  }
  window.KVMFlowCompatReport = { buildReport, submission, ISSUE_URL, MAX_URL_LENGTH };
})();
