// Status page and settings section for mixed mode. The runtime is the only
// source of truth: this file keeps the latest `mixed` object it reported and
// asks for changes through mixed.set / mixed.assign.
(() => {
  const t = (id, params) => window.KVMFlowI18n.t(id, params);
  const View = window.KVMFlowMixedView;
  const byId = (id) => document.getElementById(id);
  const esc = (value) => escapeHtml(value);
  const renderedHtml = new Map();
  const html = (id, content) => { const element = byId(id); if (renderedHtml.get(id) !== content) { element.innerHTML = content; renderedHtml.set(id,content); } };
  let mixed = null;
  let runtimeGeneration = 0;
  const runtimeFields = new Map();
  let runtimeSnapshot = {};
  let config = null;
  let lan = null;
  let transition = null;
  let settingsError = null;
  let settingsErrorOperation = null;
  let lastEvent = null;
  let assignBusy = false;
  const seenWarnings = new Set();
  const warningKey = warning => `${warning?.code}:${warning?.fingerprint || warning?.reason?.fingerprint || ''}:${warning?.device_id || ''}`;
  const dismissedKey = () => `kvmflow-mixed-notices:${config?.switch_group?.group_id || 'local'}`;
  const dismissed = () => { try { return new Set(JSON.parse(localStorage.getItem(dismissedKey()) || '[]')); } catch { return new Set(); } };

  const clock = () => new Date().toLocaleTimeString(window.KVMFlowI18n.locale, { hour: '2-digit', minute: '2-digit' });
  const context = () => ({ mixed, config, lan, computers: model.computers });
  const recordEvent = (text) => { lastEvent = { time: clock(), message: window.KVMFlowI18n.describe(text) }; };

  // ---- status page -------------------------------------------------------

  function screenHtml(screen) {
    const classes = ['desk-screen', screen.state, screen.builtin ? 'builtin' : '', screen.assumed ? 'assumed' : ''].filter(Boolean).join(' ');
    const body = screen.state === 'offline' || screen.state === 'closed' || screen.state === 'builtin-unavailable'
      ? `<span class="desk-screen-name">${esc(screen.showing || screen.name)}</span><span class="desk-screen-note">${t(screen.state === 'closed' ? 'mixed.screen.closed' : screen.state === 'builtin-unavailable' ? 'mixed.screen.builtin.unavailable' : 'mixed.screen.offline')}</span>`
      : screen.state === 'detached'
        ? `<span class="desk-screen-name">${esc(screen.showing || '')}</span><span class="desk-screen-note">${t('mixed.screen.detached')}</span>`
        : `<span class="desk-screen-name">${esc(screen.showing || t('mixed.unknown'))}</span>${screen.assumed && screen.showing ? `<span class="desk-screen-note">${t(['ddc_commanded','commanded_unconfirmed'].includes(screen.evidence) ? 'mixed.screen.commanded' : 'mixed.screen.target')}</span>` : ''}`;
    const title = screen.assumed && !screen.builtin && screen.showing ? ` title="${esc(t('mixed.screen.assumed'))}"` : '';
    const tag = screen.fixed && !screen.builtin ? `<span class="settings-tag">${t('mixed.fixed')}</span>` : '';
    return `<div class="desk-screen-cell"><div class="${classes}"${title}>${body}</div><span class="desk-stand" aria-hidden="true"></span>
      <div class="desk-screen-label"><span>${esc(screen.name)}</span>${tag}</div></div>`;
  }

  function computerHtml(computer) {
    const lan = computer.lan ? `<p class="desk-lan ${computer.lan}"><span class="lan-dot" aria-hidden="true"></span>${t(computer.lan === 'connected' ? 'mixed.lan.connected' : 'mixed.lan.disconnected')}</p>` : '';
    return `<div class="desk-computer${computer.holder ? ' holder' : ''}" data-computer="${esc(computer.id)}">
      <div class="desk-computer-head"><strong>${esc(computer.name)}</strong>${computer.local ? `<span class="settings-tag">${t('this.computer')}</span>` : ''}</div>
      <p class="hint">${t('port.value', { p0: computer.port })}</p>${lan}
      ${computer.holder ? `<span class="holder-tag">${t(computer.inferred ? 'mixed.keyboard.here.inferred' : 'mixed.keyboard.here')}</span>` : ''}</div>`;
  }

  function renderStatus() {
    const root = byId('mixed-status');
    if (!root) return;
    const ctx = context();
    const bar = View.modeBar(ctx, transition);
    if (!bar) { root.hidden = true; return; }
    root.hidden = false;
    const blocked = bar.blocked.length > 0;
    byId('mode-bar').className = `mode-bar ${bar.mode}`;
    html('mode-bar', `<div class="mode-copy"><div class="mode-title"><strong>${bar.title}</strong>${bar.tag ? `<span class="settings-tag">${bar.tag}</span>` : ''}</div>
      <p class="hint" role="status">${esc(bar.description)}</p></div>
      <button type="button" class="secondary" id="mode-action" data-mixed-action="${bar.action}" ${bar.busy ? 'disabled' : ''}
        ${blocked ? `title="${esc(bar.blocked[0])}" aria-describedby="mode-blocked"` : ''}>${t(bar.action === 'enable' ? 'mixed.enable' : 'mixed.disable')}</button>` + (blocked ? `<p id="mode-blocked" class="mode-blocked hint">${esc(bar.blocked[0])}</p>` : ''));
    const screens = View.statusScreens(ctx);
    byId('screen-row').classList.toggle('many', screens.length >= 3);
    html('screen-row', screens.map(screenHtml).join(''));
    html('computer-row', View.statusComputers(ctx).map(computerHtml).join(''));
    const next = View.nextSwitch(ctx);
    byId('next-switch').hidden = !next;
    byId('next-switch').textContent = next ? t('mixed.next.prefix', { p0: next }) : '';
    const dismissedKeys = dismissed();
    const banners = View.banners(ctx).filter(entry => !entry.dismissible || !dismissedKeys.has(entry.key));
    html('status-banners', banners.map(entry => `<div class="status-banner" role="status"><span>${esc(entry.text)}</span>${entry.dismissible ? `<button type="button" class="dismiss-notice" data-mixed-dismiss="${esc(entry.key)}" aria-label="${t('mixed.dismiss')}">×</button>` : ''}</div>`).join(''));
  }

  function renderLast() {
    const title = byId('last-title'), copy = byId('last-copy');
    if (!title || !copy) return false;
    const report = window.__kvmflowLastSwitchReport;
    if (!lastEvent && !report) return false;
    title.textContent = t('mixed.last.title');
    copy.textContent = lastEvent ? `${lastEvent.time}  ${window.KVMFlowI18n.format(lastEvent.message)}` : reportText(report);
    return true;
  }

  function reportText(report) {
    const outcomes = Array.isArray(report?.per_monitor) ? report.per_monitor : [];
    const name = model.computers.find(computer => computer.id === report?.target_device_id)?.name || t('computer');
    const ok = outcomes.length > 0 && outcomes.every(entry => entry.commanded === true || entry.ok === true);
    return mixed?.enabled && !outcomes.length ? t('mixed.last.keys.only', {p0:name})
      : ok ? t('mixed.last.switched', {p0:name}) : t('mixed.last.switch.failed');
  }

  // ---- settings section ----------------------------------------------------

  function screenChoiceHtml(screen, ctx, locked) {
    const device = screen.assigned ? ctx.computers.find((computer) => computer.id === screen.assigned) : null;
    const absent = screen.destination?.available === false;
    const unknown = screen.assigned && !device ? `<option value="${esc(screen.assigned)}" disabled selected>${t('mixed.deleted.computer')}</option>` : '';
    const options = [...ctx.computers].sort((a, b) => a.port - b.port)
      .map((computer) => `<option value="${esc(computer.id)}" ${computer.id === screen.assigned ? 'selected' : ''}>${esc(computer.name)}</option>`).join('');
    const unassigned = screen.assigned ? '' : `<option value="" disabled selected>${t('mixed.unassigned')}</option>`;
    return `<div class="assign-cell${absent ? ' absent' : ''}"><div class="assign-screen${device ? '' : ' empty'}">${esc(device?.name || t('mixed.unassigned'))}</div>
      <span class="desk-stand" aria-hidden="true"></span><label class="assign-label" for="assign-${screen.index}">${esc(screen.name)}${absent ? ` · ${t('mixed.screen.absent')}` : ''}</label>
      <select id="assign-${screen.index}" class="assign-select" data-fingerprint="${esc(screen.fingerprint)}" ${assignBusy || locked ? 'disabled' : ''}>${unassigned}${unknown}${options}</select></div>`;
  }

  function renderSettings() {
    const root = byId('mixed-settings');
    if (!root) return;
    const ctx = context();
    if (!mixed) { root.hidden = true; return; }
    root.hidden = false;
    const blocked = View.blockReasons(ctx);
    const screens = View.externalScreens(ctx);
    const builtins = View.builtinScreens(ctx);
    byId('mixed-screens').classList.toggle('many', screens.length + builtins.length >= 3);
    const single = (mixed.validation?.reasons || []).some(reason => reason.code === 'not_enough_screens');
    const busy = assignBusy || ['enabling', 'disabling'].includes(View.effectiveTransition(mixed, transition));
    const toggle = byId('mixed-toggle');
    toggle.checked = mixed.enabled === true;
    toggle.disabled = busy || (!mixed.enabled && blocked.length > 0);
    byId('mixed-toggle-label').textContent = t(mixed.enabled ? 'on' : 'off');
    byId('mixed-description').textContent = busy ? t(View.effectiveTransition(mixed, transition) === 'disabling' ? 'mixed.restoring' : 'mixed.saving') : !mixed.enabled && blocked.length ? blocked[0]
      : t(mixed.enabled ? 'mixed.settings.on' : 'mixed.settings.off');
    const externalHtml = single
      ? screens.map((screen) => `<div class="assign-cell unavailable"><div class="assign-screen empty">${t('mixed.follows.keyboard')}</div><span class="desk-stand" aria-hidden="true"></span>
          <span class="assign-label">${esc(screen.name)}</span><span class="assign-fixed">${t('mixed.cannot.assign')}</span></div>`).join('')
      : screens.map((screen) => screenChoiceHtml(screen, ctx, busy)).join('');
    const builtinHtml = builtins.map((screen) => `<div class="assign-cell builtin">
          <div class="assign-screen">${esc(ctx.computers.find((computer) => computer.local)?.name || t('this.computer'))}</div><span class="desk-stand" aria-hidden="true"></span>
          <span class="assign-label">${esc(t('mixed.builtin.label'))}</span><span class="assign-fixed">${t(screen.destination.lid_closed ? 'mixed.screen.closed' : screen.destination.unavailable_reason === 'builtin_closed_or_unavailable' ? 'mixed.screen.builtin.unavailable' : screen.destination.available === false ? 'mixed.screen.offline' : 'mixed.builtin.always.local')}</span></div>`).join('');
    html('mixed-screens', externalHtml + builtinHtml);
    const lines = [...(blocked.length > 1 ? blocked.slice(1) : []), ...View.notes(ctx)];
    if (settingsError) lines.push(settingsError);
    const sync = View.sync(ctx);
    byId('mixed-notes').innerHTML = lines.map((text) => `<p class="mixed-note" role="status">${esc(text)}</p>`).join('')
      + (sync ? `<p class="mixed-sync ${sync.tone}"><span class="lan-dot" aria-hidden="true"></span>${esc(sync.text)}</p>` : '');
    byId('mixed-screens-block').hidden = screens.length + builtins.length === 0;
  }

  function render() {
    renderStatus();
    renderSettings();
    renderLast();
    renderPeers();
    window.dispatchEvent(new CustomEvent('mixed-rendered'));
  }

  function renderPeers() {
    for (const element of document.querySelectorAll('[data-mixed-peer]')) {
      const peer = Array.isArray(lan?.peers) ? lan.peers.find(peer => peer.device_id === element.dataset.mixedPeer) : null;
      element.textContent = typeof peer?.online === 'boolean' ? t(peer.online ? 'mixed.lan.connected' : 'mixed.lan.disconnected') : '';
      if (peer?.pairing_available === true) {
        const button = document.createElement('button');
        button.type = 'button'; button.className = 'settings-text-button';
        button.textContent = t('mixed.lan.connect');
        button.addEventListener('click', event => {
          event.stopPropagation();
          // The native host may forward the pairing request; without that
          // capability the in-app pairing flow takes over.
          if (typeof window.kvmflow.lan?.openPairing === 'function') {
            void Promise.resolve(window.kvmflow.lan.openPairing(peer.device_id)).catch(error => {
              log(t('mixed.log.warning'),errorText(error));
            });
          } else {
            window.KVMFlowPairing?.pairWith(peer.device_id);
          }
        });
        element.append(' ',button);
      }
    }
  }

  // ---- runtime data ----------------------------------------------------------

  const compare = (a,b) => a === b ? 0 : a > b ? 1 : -1;
  const compareVersion = (a,b) => compare(Number(a.revision),Number(b.revision))
    || compare(Number(a.section_meta?.updated_at_ms || 0),Number(b.section_meta?.updated_at_ms || 0))
    || compare(a.section_meta?.updated_by || '',b.section_meta?.updated_by || '');

  function setMixed(next, sequenced = false) {
    const before = mixed;
    if (before && next && !sequenced) {
      const version = compareVersion(next,before);
      if (version < 0) return false;
      if (version === 0 && Number(next.transition?.started_ms) < Number(before.transition?.started_ms)) return false;
    }
    mixed = next || null;
    const activeWarnings = new Set((mixed?.warnings || []).map(warningKey));
    for (const key of seenWarnings) if (!activeWarnings.has(key)) seenWarnings.delete(key);
    const dismissedKeys = dismissed();
    let dismissedChanged = false;
    for (const warning of before?.warnings || []) {
      if (!activeWarnings.has(warningKey(warning))) {
        const bannerKey = warning.code === 'unassigned_monitor' ? `unassigned_monitor:${warning.fingerprint}` : warning.code;
        dismissedChanged = dismissedKeys.delete(bannerKey) || dismissedChanged;
      }
    }
    if (dismissedChanged) {
      try { localStorage.setItem(dismissedKey(), JSON.stringify([...dismissedKeys])); } catch { /* Keep current runtime state usable. */ }
    }
    if (mixed && before && mixed.enabled && before.enabled && before.keyboard_holder_device_id
      && mixed.keyboard_holder_device_id && before.keyboard_holder_device_id !== mixed.keyboard_holder_device_id) {
      const name = model.computers.find((computer) => computer.id === mixed.keyboard_holder_device_id)?.name || t('computer');
      recordEvent(t('mixed.last.holder', { p0: name }));
    }
    return true;
  }

  function clearCompletedRestoreError() {
    if (settingsErrorOperation === 'close' && mixed?.enabled === false
      && mixed.transition?.phase === 'idle' && mixed.transition?.outcome === 'ok'
      && !mixed.pending_remote?.length && !mixed.execution?.some(entry => entry.ok === false)) {
      settingsError = null; settingsErrorOperation = null;
      if (transition === 'failed') transition = null;
    }
  }

  function acceptGeneration(state) {
    const generation = state.runtime_generation ?? state.mixed?.runtime_generation ?? 0;
    if (!Number.isSafeInteger(generation) || generation < runtimeGeneration) return false;
    if (generation > runtimeGeneration) {
      runtimeGeneration = generation;
      runtimeFields.clear();
      runtimeSnapshot = {};
      setMixed(null);
      lan = null;
      lastEvent = null;
    }
    return true;
  }

  function setState(state) {
    if (!state || !acceptGeneration(state)) return false;
    const sequence = state.runtime_seq ?? state.mixed?.runtime_seq;
    const sequenced = Number.isSafeInteger(sequence) && sequence >= 0;
    let accepted = false;
    for (const [field,value] of Object.entries(state)) {
      if (['runtime_seq','runtime_generation','saved','applied'].includes(field)) continue;
      const latest = runtimeFields.get(field);
      if (latest != null && (!sequenced || sequence < latest)) continue;
      if (field === 'mixed' && !setMixed(value,sequenced)) continue;
      if (field === 'lan') lan = value || null;
      runtimeFields.set(field,sequenced ? sequence : null);
      runtimeSnapshot[field] = value;
      accepted = true;
    }
    if (!accepted) return false;
    clearCompletedRestoreError();
    render();
    // Consumers must use the merged snapshot: a message can contain both
    // accepted fields and fields already superseded by another partial result.
    return runtimeSnapshot;
  }

  const errorText = (error) => String(error?.message || t('please.try.again'));
  // E_CONFIG_INVALID carries the validation object as JSON text.
  const reasonOf = (error) => {
    try {
      const reasons = JSON.parse(error?.message || '')?.reasons;
      return Array.isArray(reasons) ? reasons : null;
    } catch { return null; }
  };

  async function setEnabled(enabled) {
    if (!mixed || assignBusy || ['enabling', 'disabling'].includes(View.effectiveTransition(mixed, transition))) return;
    const ctx = context();
    if (enabled && View.blockReasons(ctx).length) { window.navigate('settings', { section: 'mixed' }); return; }
    transition = enabled ? 'enabling' : 'disabling';
    settingsError = null;
    render();
    try {
      const response = await window.kvmflow.request('mixed.set', { enabled });
      transition = null;
      if (response?.mixed ? !setState(response) : !setState(await window.kvmflow.request('state.get'))) { render(); return; }
      const pending = (mixed?.pending_remote?.length || 0) + (mixed?.execution || []).filter(entry => entry.ok === false).length;
      const partial = pending > 0 || ['partial', 'failed'].includes(mixed?.transition?.outcome);
      recordEvent(enabled ? t(partial ? 'mixed.last.enabled.partial' : 'mixed.last.enabled') : partial ? t('mixed.last.disabled.pending') : t('mixed.last.disabled'));
      renderLast();
      log(enabled ? t('mixed.log.enabled') : t('mixed.log.disabled'), t('settings.saved.on.this.computer'));
    } catch (error) {
      transition = null;
      settingsErrorOperation = enabled ? 'enable' : 'close';
      settingsError = reasonOf(error) ? t('mixed.block.generic') : errorText(error);
      log(enabled ? t('mixed.log.enable.failed') : t('mixed.log.disable.failed'), errorText(error));
      clearCompletedRestoreError();
      if (!enabled) {
        // Disabling must never be blocked; refresh to learn what actually happened.
        try { setState(await window.kvmflow.request('state.get')); } catch { /* keep the last state */ }
      }
    }
    render();
  }

  async function assign(fingerprint, deviceId) {
    if (!model.computers.some(computer => computer.id === deviceId)) {
      renderedHtml.delete('mixed-screens'); render(); return;
    }
    if (!mixed || assignBusy || ['enabling', 'disabling'].includes(View.effectiveTransition(mixed, transition))) return;
    assignBusy = true;
    settingsError = null;
    settingsErrorOperation = 'assignment';
    render();
    try {
      const response = await window.kvmflow.request('mixed.assign', { assignment: View.assignmentAfterChoice(context(), fingerprint, deviceId) });
      if (response?.mixed && !setState(response)) return;
      log(t('mixed.log.assigned'), t('settings.saved.on.this.computer'));
    } catch (error) {
      settingsError = reasonOf(error) ? t('mixed.assign.rejected') : errorText(error);
      log(t('mixed.log.assign.failed'), errorText(error));
    } finally {
      assignBusy = false;
      render();
    }
  }

  document.addEventListener('click', (event) => {
    const dismiss = event.target.closest('[data-mixed-dismiss]');
    if (dismiss) {
      const keys = dismissed(); keys.add(dismiss.dataset.mixedDismiss);
      try { localStorage.setItem(dismissedKey(), JSON.stringify([...keys].slice(-100))); } catch { /* Still hide this occurrence. */ }
      dismiss.closest('.status-banner').remove(); return;
    }
    const control = event.target.closest('[data-mixed-action]');
    if (!control || control.disabled) return;
    void setEnabled(control.dataset.mixedAction === 'enable');
  });
  byId('mixed-toggle')?.addEventListener('change', (event) => {
    const checked = event.currentTarget.checked;
    event.currentTarget.checked = !checked;
    void setEnabled(checked);
  });
  byId('mixed-screens')?.addEventListener('change', (event) => {
    const select = event.target.closest('.assign-select');
    if (select) void assign(select.dataset.fingerprint, select.value);
  });

  window.kvmflow.onNotification?.('mixed.changed', (data) => {
    if (data?.mixed) setState(data);
  });
  window.kvmflow.onNotification?.('mixed.pending_remote', (data) => {
    if (mixed && data?.group_id === config?.switch_group?.group_id && data.enabled === mixed.enabled
      && compareVersion(data,mixed) === 0 && Array.isArray(data.pending_remote)) {
      setState({ runtime_generation:data.runtime_generation, runtime_seq:data.runtime_seq,
        mixed:{ ...mixed, runtime_generation:data.runtime_generation ?? 0, runtime_seq:data.runtime_seq, pending_remote:data.pending_remote } });
    }
    render();
  });
  window.kvmflow.onNotification?.('mixed.warning', (data) => {
    const key = warningKey(data);
    if (seenWarnings.has(key)) return;
    seenWarnings.add(key);
    log(t('mixed.log.warning'), String(data?.error?.message || data?.error || data?.code || ''));
  });
  window.addEventListener('language-changed', render);

  window.KVMFlowMixed = {
    setConfig: (value) => {
      if (config?.switch_group?.group_id && config.switch_group.group_id !== value?.switch_group?.group_id) setMixed(null);
      config = value || null; render();
    },
    setState,
    markError: (error) => {
      if (!acceptGeneration(error || {})) return false;
      runtimeSnapshot.state = 'error';
      return true;
    },
    // The runtime's latest mixed object, for config.set round trips.
    groupMixed: () => mixed ? { enabled: mixed.enabled, revision: mixed.revision, assignment: mixed.assignment } : null,
    onSwitchReport: (report) => {
      recordEvent(reportText(report));
      renderLast();
      render();
    },
    renderLast, render, renderPeers, setEnabled,
    peer: (id) => Array.isArray(lan?.peers) ? lan.peers.find(peer => peer.device_id === id) : null,
    get mixed() { return mixed; },
  };
})();
