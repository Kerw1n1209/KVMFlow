// The public report follows feedback-api v1 exactly. No diagnostics are used
// as a report source; arbitrary hardware strings pass a separate privacy gate.
(() => {
  const ISSUE_URL = 'https://github.com/Kerw1n1209/KVMFlow/issues/new';
  const MAX_URL_LENGTH = 2000;
  const array = value => Array.isArray(value) ? value : [];
  const input = value => Number.isInteger(value) && value >= 0 && value <= 65535;
  const codepoints = (value, max) => Array.from(value).slice(0, max).join('');

  function buildReport(data) {
    const { host = {}, config = {}, displays = [], usb = [], state = {} } = data;
    if (!['macos', 'windows'].includes(host.platform)) throw new Error('Unsupported feedback platform');
    const secrets = new Set();
    const sensitiveKeys = /^(?:serial|serial_string|fingerprint|edid_id|uuid|device_id|group_id|public_key|key|computerName|host_label|username|computer_name|addr|addresses|path|logPath|configPath)$/i;
    const collect = (value, key = '') => {
      if (typeof value === 'string') {
        if (sensitiveKeys.test(key) && value) secrets.add(value.toLowerCase());
        for (const match of value.matchAll(/\b[A-Z0-9]{3}-[0-9A-F]{4}-(?:S:([^\s"',;]+)|([0-9A-F]{8}))(?![0-9A-Za-z])/gi)) secrets.add((match[1] || match[2]).toLowerCase());
      } else if (Array.isArray(value)) value.forEach(item => collect(item, key));
      else if (value && typeof value === 'object') Object.entries(value).forEach(([k, v]) => collect(v, k));
    };
    collect(data);
    array(config.switch_group?.devices).forEach(device => { if (device.name) secrets.add(device.name.toLowerCase()); });
    const unsafe = /(?:https?:|[\\/@]|\b(?:\d{1,3}\.){3}\d{1,3}\b|\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b|\b[A-Z0-9]{3}-[0-9A-F]{4}-|\b[0-9a-f]{32,}\b|[0-9a-f]*:[0-9a-f:]*:[0-9a-f:]*|[\x00-\x1f\x7f])/i;
    const text = value => {
      if (typeof value !== 'string' || !value.trim() || unsafe.test(value) || [...secrets].some(secret => value.toLowerCase().includes(secret))) return null;
      return codepoints(value.trim(), 64);
    };
    const source = value => value === 'learned_active_read' ? 'read' : ['manual', 'manual_entry'].includes(value) ? 'manual' : null;
    const configured = array(config.local_device?.monitors);
    const inventory = array(displays);
    const allMonitors = [...inventory, ...configured.filter(m => !inventory.some(d => d.edid_id === m.fingerprint)).map(m => ({ edid_id: m.fingerprint }))];
    const monitors = allMonitors.slice(0, 8).map(display => {
      const fp = display.edid_id;
      let mappings = array(config.switch_group?.devices).map((device, i) => ({ ...array(device.monitors).find(m => fp && m.fingerprint === fp), computer: device.port_index ?? i + 1 }));
      if (!mappings.some(m => input(m.local_input))) mappings = [{ ...configured.find(m => fp && m.fingerprint === fp), computer: 1 }];
      return {
        vendor: text(display.manufacturer), model: text(display.model_name),
        ddc: display.ddc?.state === 'available' ? true : display.ddc?.state === 'unavailable' ? false : null,
        vcp60: display.ddc?.state === 'available' && input(display.ddc.input) ? display.ddc.input : null,
        inputs: mappings.filter(m => [1, 2].includes(m.computer) && input(m.local_input) && source(m.source))
          .slice(0, 4).map(m => ({ computer: m.computer, value: m.local_input, source: source(m.source) })),
      };
    });
    const vidPid = config.trigger?.anchor?.vid_pid;
    const device = array(usb).find(device => device.vid_pid === vidPid);
    const report = state.lastMultiDeviceReport || state.lastReport;
    const outcomes = array(report?.per_monitor);
    const accepted = typeof report?.commanded === 'boolean' ? report.commanded
      : outcomes.length && outcomes.every(m => typeof m.commanded === 'boolean') ? outcomes.every(m => m.commanded) : null;
    // v1 has a single readback, not per-monitor outcomes. Do not attribute one
    // panel's readback to multiple panels or infer a picture-switch result.
    const readback = outcomes.length === 1 && input(outcomes[0].readback) ? outcomes[0].readback : null;
    return {
      app: { version: /^\d+\.\d+\.\d+(?:-[\w.]+)?$/.test(host.version || '') ? host.version : '0.0.0', channel: host.version?.includes('-') ? 'beta' : 'stable' },
      os: { platform: host.platform, version: /^[\d.]+$/.test(host.osVersion || '') ? host.osVersion : 'unknown', arch: ['aarch64', 'x86_64', 'x86', 'arm'].includes(host.arch) ? host.arch : 'unknown' },
      mode: state.mixed?.enabled === true ? 'mixed' : 'normal', monitors,
      usb: /^[0-9a-f]{4}:[0-9a-f]{4}$/i.test(vidPid || '') ? { vendor: text(device?.vendor) || '', product: text(device?.product) || '', vid: vidPid.slice(0, 4).toUpperCase(), pid: vidPid.slice(5).toUpperCase() } : null,
      lastSwitch: typeof accepted === 'boolean' && readback !== null ? { accepted, readback } : null,
    };
  }

  function renderReport(report, t) {
    const unknown = t('compat.unknown');
    const text = value => value == null || value === '' ? unknown : String(value).replace(/[\\`*_{}\[\]<>|#]/g, '\\$&');
    const bool = value => value == null ? unknown : t(value ? 'compat.yes' : 'compat.no');
    const line = (key, value) => `- ${t(`compat.${key}`)}: ${value}`;
    const lines = ['## KVMFlow', line('version', text(report.app.version)), line('channel', report.app.channel), line('os', text(report.os.platform)), line('os.version', text(report.os.version)), line('arch', text(report.os.arch)), line('mode', report.mode)];
    report.monitors.forEach((m, i) => {
      lines.push('', `## ${t('compat.monitor')} ${i + 1}`, line('vendor', text(m.vendor)), line('model', text(m.model)), line('ddc', bool(m.ddc)), line('read.60', text(m.vcp60)));
      m.inputs.forEach(mapping => lines.push(line('input', `${t('compat.computer')} ${mapping.computer}: ${mapping.value} (${t(mapping.source === 'read' ? 'compat.read' : 'compat.manual')})`)));
    });
    if (report.usb) lines.push('', `## ${t('compat.usb')}`, line('vendor', text(report.usb.vendor)), line('product', text(report.usb.product)), line('vidpid', `${report.usb.vid}:${report.usb.pid}`));
    if (report.lastSwitch) lines.push('', line('write.60', bool(report.lastSwitch.accepted)), line('readback', text(report.lastSwitch.readback)), line('picture', t('compat.unconfirmed')));
    lines.push('', `## ${t('compat.notes')}`, line('purchase', ''), line('experience', ''));
    return lines.join('\n');
  }

  function submission(report) {
    const params = new URLSearchParams({ template: 'compatibility.yml', title: '[Compatibility] My devices', report });
    const url = `${ISSUE_URL}?${params}`;
    return url.length <= MAX_URL_LENGTH ? { url, copyFirst: false } : { url: `${ISSUE_URL}?template=compatibility.yml`, copyFirst: true };
  }
  window.KVMFlowCompatReport = { buildReport, renderReport, submission, ISSUE_URL, MAX_URL_LENGTH };
})();
