// LAN keyboard and mouse sharing: the status page additions and settings.
// lan-pairing.js reads this module's status during setup.
(() => {
  const byId = id => document.getElementById(id);
  const t = (id, params) => window.KVMFlowI18n.t(id, params);
  const esc = value => String(value ?? '').replace(/[&<>"']/g, char => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
  })[char]);
  const Hotkey = window.KVMFlowHotkey;

  let status = null, platform = '', recording = null;
  const OTHER_HOTKEY = { hotkey: 'home_hotkey', home_hotkey: 'hotkey' };
  const hotkeyOf = field => status?.[field] || (field === 'hotkey' ? Hotkey.DEFAULT : Hotkey.HOME);
  const listeners = new Set();
  window.kvmflow.info?.then?.(info => { platform = info?.platform || ''; render(); }, () => {});

  const needsPermissions = () => platform === 'macos';
  const missingPermission = () => (status?.permissions?.accessibility === false ? 'accessibility' : 'input_monitoring');
  const keys = hotkey => Hotkey.labels(hotkey, platform);
  const caps = hotkey => keys(hotkey).map(key => `<kbd>${esc(key)}</kbd>`).join('');
  const computerName = id => model.computers.find(computer => computer.id === id)?.name || '';

  function apply(next) {
    if (!next) return;
    status = next;
    navState();
    renderStatus();
    render();
    for (const listener of listeners) listener(status);
  }
  const refresh = () => window.kvmflow.request('km.get', {}).then(apply, () => {});
  const configure = params => window.kvmflow.request('km.configure', params).then(next => { apply(next); return next; });
  const requestPermission = permission => window.kvmflow.request('km.permissions.request', { permission }).then(apply);

  function alertModel() {
    if (!status?.enabled) return null;
    const peer = computerName(status.peer_device_id);
    if (status.error === 'E_KM_PERMISSION') {
      const permission = missingPermission();
      return { title: t('km.paused.title'), copy: t(permission === 'accessibility' ? 'km.paused.accessibility' : 'km.paused.input'), permission };
    }
    if (status.error) {
      const copy = { E_KM_ENGINE_RUNNING: 'km.paused.running', E_KM_PORT: 'km.paused.port', E_KM_TWO_COMPUTERS: 'km.paused.two' }[status.error] || 'km.paused.engine';
      return { title: t('km.paused.title'), copy: t(copy) };
    }
    if (!status.peer_online) return { title: t('km.offline.title', { p0: peer }), copy: t('km.offline.copy') };
    return null;
  }

  function renderStatus() {
    const paired = Boolean(status?.peer_device_id && status?.configured);
    const row = byId('km-toggle-row');
    if (row) row.hidden = !paired;
    const toggle = byId('km-toggle');
    if (toggle) toggle.checked = Boolean(status?.enabled);
    const state = byId('app-state');
    if (state) state.hidden = Boolean(status?.enabled);

    const alert = byId('km-alert'), notice = alertModel();
    if (alert) {
      alert.hidden = !notice;
      alert.innerHTML = notice ? `<div><strong>${esc(notice.title)}</strong><p class="sub">${esc(notice.copy)}</p></div>${notice.permission ? `<button type="button" class="secondary" data-km-permission="${notice.permission}">${esc(t('km.perm.allow'))}</button>` : ''}` : '';
    }
    const hint = byId('km-hint');
    if (hint) {
      const visible = Boolean(status?.enabled && status.peer_online && status.hotkey && !status.error);
      hint.hidden = !visible;
      const target = status?.input_here ? computerName(status?.peer_device_id) : model.computers.find(computer => computer.local)?.name || '';
      hint.innerHTML = visible ? `<strong>${esc(t('km.hint', { p0: target }))}</strong><span class="km-keys">${caps(status.hotkey)}</span>` : '';
    }
  }

  const CHECK = '<svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="M20 6 9 17l-5-5"/></svg>';
  function permissionRow(id, title, copy, granted) {
    return `<div class="response-row"><div class="response-copy"><strong>${esc(t(title))}</strong><span>${esc(t(copy))}</span></div>${granted ? `<span class="km-granted"><span class="ob-box on" aria-hidden="true">${CHECK}</span>${esc(t('km.perm.granted'))}</span>` : `<button type="button" class="secondary" data-km-permission="${id}">${esc(t('km.perm.allow'))}</button>`}</div>`;
  }

  function renderSettings() {
    const paired = Boolean(status?.peer_device_id);
    for (const section of document.querySelectorAll('[data-km-section]')) section.hidden = !paired;
    const permissions = byId('km-permissions-section');
    if (permissions) permissions.hidden = !paired || !needsPermissions();
    if (!paired) return;
    const method = byId('km-method');
    method.value = status.enabled ? 'hotkey' : 'switch';
    method.querySelector('option[value="switch"]').disabled = !window.KVMFlowPairingConfig?.hasTrigger?.();
    const server = byId('km-server');
    const computers = model.computers;
    const options = computers.map(computer => `<option value="${esc(computer.id)}">${esc(computer.name)}</option>`).join('');
    if (server.dataset.options !== options) { server.innerHTML = options; server.dataset.options = options; }
    server.value = status.server_device_id || computers.find(computer => computer.local)?.id || '';
    server.disabled = !status.enabled;
    renderInputRows();
    for (const button of document.querySelectorAll('[data-km-hotkey]')) renderHotkeyField(button);
    if (permissions && needsPermissions()) {
      byId('km-permissions').innerHTML = permissionRow('accessibility', 'km.perm.accessibility', 'km.perm.accessibility.copy', status.permissions?.accessibility)
        + permissionRow('input_monitoring', 'km.perm.input', 'km.perm.input.copy', status.permissions?.input_monitoring);
    }
  }

  // Command and Ctrl only differ between a Mac and a PC. The keyboard is on
  // the computer named as the server, so that names the direction.
  function renderInputRows() {
    byId('km-clipboard').checked = status.clipboard !== false;
    byId('km-invert').checked = status.invert_scroll === true;
    byId('km-awake').checked = status.keep_awake === true;
    for (const id of ['km-clipboard', 'km-invert', 'km-awake', 'km-swap']) byId(id).disabled = !status.enabled;
    const here = status.server_device_id === model.computers.find(computer => computer.local)?.id;
    const keyboard = here ? platform : status.peer_platform;
    const mixed = Boolean(platform && status.peer_platform && platform !== status.peer_platform);
    byId('km-swap-row').hidden = !mixed;
    if (!mixed) return;
    const key = keyboard === 'macos' ? 'mac' : 'pc';
    byId('km-swap-copy').textContent = t(`km.settings.swap.${key}.copy`);
    const swap = byId('km-swap');
    const mapped = swap.querySelector('option[value="on"]');
    mapped.dataset.i18n = `km.settings.swap.${key}`;
    mapped.textContent = t(mapped.dataset.i18n);
    swap.value = status.swap_command_control === false ? 'off' : 'on';
  }

  function renderHotkeyField(button) {
    const field = button.dataset.kmHotkey;
    const active = recording?.field === field;
    button.innerHTML = active
      ? `<span class="km-field-wait">${esc(t(recording.taken ? 'km.hotkey.taken' : 'km.hotkey.recording'))}</span>`
      : caps(hotkeyOf(field));
    button.disabled = !status.enabled;
    button.classList.toggle('recording', active);
    button.setAttribute('aria-label', `${t(`km.settings.${field}`)}: ${keys(hotkeyOf(field)).join(' + ')}`);
  }

  function render() { renderStatus(); renderSettings(); }

  function recordHotkey(field) {
    recording?.stop();
    const other = hotkeyOf(OTHER_HOTKEY[field]);
    const run = recording = { field, taken: false };
    run.stop = Hotkey.record(hotkey => {
      recording = null;
      if (hotkey) void configure({ [field]: hotkey }).catch(() => render());
      else render();
    }, { isTaken: hotkey => hotkey === other, onTaken: () => { run.taken = true; render(); } });
    render();
  }

  byId('km-toggle')?.addEventListener('change', event => {
    void configure({ enabled: event.target.checked }).catch(() => render());
  });
  byId('km-method')?.addEventListener('change', event => {
    void configure({ enabled: event.target.value === 'hotkey' }).catch(() => render());
  });
  byId('km-server')?.addEventListener('change', event => {
    void configure({ server_device_id: event.target.value }).catch(() => render());
  });
  for (const [id, name] of [['km-clipboard', 'clipboard'], ['km-invert', 'invert_scroll'], ['km-awake', 'keep_awake']]) {
    byId(id)?.addEventListener('change', event => {
      void configure({ [name]: event.target.checked }).catch(() => render());
    });
  }
  byId('km-swap')?.addEventListener('change', event => {
    void configure({ swap_command_control: event.target.value === 'on' }).catch(() => render());
  });
  document.addEventListener('click', event => {
    const field = event.target.closest('[data-km-hotkey]');
    if (field) recordHotkey(field.dataset.kmHotkey);
    const control = event.target.closest('[data-km-permission]');
    if (control) void requestPermission(control.dataset.kmPermission).catch(() => {});
  });
  window.kvmflow.onNotification?.('km.changed', apply);
  window.addEventListener('language-changed', render);
  void refresh();

  window.KVMFlowKm = {
    status: () => status,
    refresh, configure, render,
    onChange: listener => { listeners.add(listener); },
    keys, needsPermissions,
    platformName: () => ({ macos: 'macOS', windows: 'Windows' })[platform] || '',
  };
})();
