// Connect the local UI to the shared Rust runtime through Tauri commands.
(async () => {
  const t = (id, params) => window.KVMFlowI18n.t(id, params);
  const S = window.kvmflow;
  await S.ready;
  const prototypeStoreKey = 'kvmflow-local-prototype-v2';
  const computerGroupDraftKey = 'kvmflow-computer-group-draft-v1';
  const timingDraftKey = 'kvmflow-timing-draft-v1';
  let configWrites = Promise.resolve();
  const serializeConfigWrite = (write) => {
    const result = configWrites.then(write);
    configWrites = result.catch(() => {});
    return result;
  };
  let sidecarConfig = null;
  let computerName = t("this.computer");
  let computerNameIsFallback = true;
  let learnedTrigger = null;
  let calibrationActive = false;
  let calibrationSnapshot = null;
  let displayDetectionSnapshot = null;
  let calibrationTimer = null;
  let stopCandidateListener = null;
  let displayFingerprints = [];
  let readableDisplayCount = 0;
  let displayDetectionActive = false;
  let timingMigrationPending = false;
  let recalibrationNeedsPersist = false;
  let arrivalCorrectionEnabled = false;
  let runtimeState = 'unconfigured';
  let lastSwitchReport = null;
  const immediateTiming = { t_stable_ms: 0, t_absent_ms: 0, t_cooldown_ms: 1000, quorum_peripherals: 1 };
  let debounceSettings = { ...immediateTiming };
  const safe = (value, fallback) => value == null || Number.isNaN(Number(value)) ? fallback : Number(value);
  const byId = (id) => document.getElementById(id);
  // Keep the saved experimental setting intact while its UI is unavailable.
  byId('arrival-correction')?.closest('.response-row')?.classList.add('hidden');
  const ensureStartupSettingsControl = () => {
    if (byId('startup-at-login')) return byId('startup-at-login');
    const list = byId('startup-list');
    if (!list) return null;
    const row = document.createElement('div');
    row.className = 'response-row startup-row';
    row.innerHTML = `<div class="response-copy"><strong>${t("launch.at.login")}</strong></div><label class="toggle-control" for="startup-at-login"><input id="startup-at-login" type="checkbox" checked><span class="toggle-track" aria-hidden="true"></span><span class="toggle-label">${t("on")}</span></label>`;
    list.appendChild(row);
    return byId('startup-at-login');
  };
  const startupControl = ensureStartupSettingsControl();
  const current = () => model.computers.find((computer) => computer.id === model.localId);
  const target = () => model.computers.find((computer) => computer.id === model.nextId) || model.computers.find((computer) => !computer.local) || current();
  const storePrototypeModel = () => localStorage.setItem(prototypeStoreKey, JSON.stringify(model));
  const availableDisplays = (response) => (Array.isArray(response?.displays) ? response.displays : []).filter((display) => !display.builtin);
  const validTrigger = (trigger) => Boolean(trigger?.anchor?.vid_pid && Array.isArray(trigger?.members) && trigger.members.length);
  const isLegacyTiming = (timing) => Number(timing?.t_stable_ms) === 5000 && Number(timing?.t_absent_ms) === 10000 && Number(timing?.t_cooldown_ms) === 15000;
  const setTimingSelect = (id, value) => {
    const select = byId(id);
    if (!select) return;
    const normalized = String(value);
    if (![...select.options].some((option) => option.value === normalized)) {
      const option = document.createElement('option');
      option.value = normalized;
      option.textContent = t("value.seconds.custom", {
  p0: value / 1000
});
      select.appendChild(option);
    }
    select.value = normalized;
  };
  const renderTimingSettings = () => {
    setTimingSelect('timing-absent', debounceSettings.t_absent_ms);
    setTimingSelect('timing-stable', debounceSettings.t_stable_ms);
    setTimingSelect('timing-cooldown', debounceSettings.t_cooldown_ms);
    if (byId('arrival-correction')) {
      const control = byId('arrival-correction');
      const unavailable = model.computers.length > 2;
      if (unavailable) arrivalCorrectionEnabled = false;
      control.disabled = unavailable;
      control.checked = !unavailable && arrivalCorrectionEnabled;
      const label = control.closest('.toggle-control')?.querySelector('.toggle-label');
      const description = byId('host-mode-description');
      if (label) label.textContent = unavailable ? t("unavailable") : arrivalCorrectionEnabled ? t("on") : t("off");
      if (description) description.textContent = unavailable
        ? t("there.are.value.computers.host.mode.supports.only.two.computers", {
  p0: model.computers.length
})
        : t("when.enabled.kvmflow.on.this.computer.can.manage.switching.for.the.other");
    }
  };

  function applySidecarConfig(config, displayResponse) {
    sidecarConfig = config || null;
    if (!config) {
      model = { ...structuredClone(defaults), events: model.events };
      // Drafts contain user-entered computer details, not proof of hardware setup.
      try {
        const draft = JSON.parse(localStorage.getItem(computerGroupDraftKey));
        if (Array.isArray(draft?.computers) && draft.computers.length &&
            new Set(draft.computers.map(c => c.id)).size === draft.computers.length &&
            draft.computers.every((c, i) => typeof c.id === 'string' && typeof c.name === 'string' &&
              c.port === i + 1 && Array.isArray(c.sources) &&
              c.local === (c.id === draft.localId)) &&
            draft.computers.some(c => c.id === draft.localId)) {
          model = { ...model, localId: draft.localId, nextId: draft.nextId, computers: draft.computers };
        }
      } catch { /* Ignore a damaged UI draft; never use it to arm the runtime. */ }
      editingId = model.localId;
    }
    const displays = availableDisplays(displayResponse);
    const readableDisplays = displays.filter((display) => display.ddc?.state === 'available');
    readableDisplayCount = readableDisplays.length;
    const groupDevices = Array.isArray(config?.switch_group?.devices) ? [...config.switch_group.devices].sort((a, b) => Number(a.port_index) - Number(b.port_index)) : [];
    const localDeviceId = config?.local_device?.device_id;
    if (groupDevices.length && localDeviceId) {
      model.localId = localDeviceId;
      model.computers = groupDevices.map(device => ({
        id: device.device_id,
        name: device.name || t("computer"),
        local: device.device_id === localDeviceId,
        port: device.port_index,
        sources: (device.monitors || []).map(monitor => monitor.local_input),
      }));
      const at = groupDevices.findIndex(device => device.device_id === localDeviceId);
      model.nextId = groupDevices[(at + 1) % groupDevices.length].device_id;
      editingId = localDeviceId;
    }
    const localComputer = current();
    const remoteComputer = target();
    const legacyMonitors = Array.isArray(config?.monitors) ? config.monitors : [];
    const localProfileMonitors = Array.isArray(config?.local_device?.monitors) ? config.local_device.monitors : [];
    const configuredMonitors = legacyMonitors.length ? legacyMonitors : localProfileMonitors;
    // Preserve saved monitor identity and order when USB/video enumeration
    // changes. A fresh setup learns from readable displays instead.
    const localMonitors = configuredMonitors.length ? configuredMonitors : readableDisplays;
    const localGroupIndex = groupDevices.findIndex((device) => device.device_id === localDeviceId);
    const localGroupDevice = localGroupIndex >= 0 ? groupDevices[localGroupIndex] : null;
    const remoteGroupDevice = localGroupIndex >= 0 && groupDevices.length > 1 ? groupDevices[(localGroupIndex + 1) % groupDevices.length] : null;
    // The persisted profile may originate on another host or an older build.
    // Never use its display name as evidence of this machine's identity.
    localComputer.name = computerName;
    if (computerNameIsFallback) localComputer.nameKey = "this.computer";
    else delete localComputer.nameKey;
    localComputer.sources = localMonitors.map((monitor, index) => safe(monitor.here_input ?? monitor.local_input ?? monitor.ddc?.input, localComputer.sources[index]));
    model.localInputSources = localMonitors.map(monitor => monitor.source || monitor.here_input_source ||
      (monitor.ddc?.state === 'available' ? 'learned_active_read' : 'unknown'));
    remoteComputer.name = remoteGroupDevice?.name || remoteComputer.name;
    if (remoteGroupDevice?.name) delete remoteComputer.nameKey;
    remoteComputer.sources = localMonitors.map((monitor, index) => {
      const fingerprint = monitor.edid_id || monitor.fingerprint;
      const groupMonitor = remoteGroupDevice?.monitors?.find((entry) => entry.fingerprint === fingerprint);
      return safe(groupMonitor?.local_input ?? legacyMonitors[index]?.away_input ?? localProfileMonitors[index]?.legacy_remote_preset?.value, remoteComputer.sources[index]);
    });
    if (localGroupDevice) localComputer.port = Number(localGroupDevice.port_index) || localComputer.port;
    if (remoteGroupDevice) remoteComputer.port = Number(remoteGroupDevice.port_index) || remoteComputer.port;
    displayFingerprints = localMonitors.map((monitor, index) => monitor.edid_id || monitor.fingerprint || `local-display-${index + 1}`);
    model.displays = localMonitors.map((monitor, index) => monitor.label || monitor.model_name || t("monitor.value", {
  p0: index + 1
}));
    for (const computer of model.computers) {
      if (computer.local) continue;
      const device = groupDevices.find(entry => entry.device_id === computer.id);
      if (device) computer.sources = displayFingerprints.map(fingerprint =>
        device.monitors.find(monitor => monitor.fingerprint === fingerprint)?.local_input ?? null);
    }
    learnedTrigger = validTrigger(config?.trigger) ? config.trigger : null;
    const storedTiming = config?.trigger?.debounce;
    arrivalCorrectionEnabled = model.computers.length <= 2 && config?.advanced?.arrival_correction_enabled === true;
    timingMigrationPending = Boolean(learnedTrigger && isLegacyTiming(storedTiming));
    debounceSettings = storedTiming && !isLegacyTiming(storedTiming) ? { ...immediateTiming, ...storedTiming, quorum_peripherals: 1 } : { ...immediateTiming };
    if (!config) {
      try {
        const timing = JSON.parse(localStorage.getItem(timingDraftKey));
        for (const key of ['t_absent_ms', 't_stable_ms', 't_cooldown_ms']) {
          if (Number.isFinite(timing?.[key]) && timing[key] >= 0) debounceSettings[key] = timing[key];
        }
      } catch { /* Keep safe defaults when a saved preference cannot be read. */ }
    }
    if (learnedTrigger) learnedTrigger = { ...learnedTrigger, debounce: { ...debounceSettings } };
    if (sidecarConfig?.trigger) sidecarConfig = { ...sidecarConfig, trigger: { ...sidecarConfig.trigger, debounce: { ...debounceSettings } } };
    model.usbConfirmed = validTrigger(config?.trigger);
    renderTimingSettings();
    storePrototypeModel();
  }

  function localConfigFromModel() {
    const localComputer = current();
    const remoteComputer = target();
    const previous = sidecarConfig || {};
    const monitorCount = Math.max(model.displays.length, localComputer.sources.length, remoteComputer.sources.length);
    if (!monitorCount) throw new Error(t("detect.the.monitors.before.saving.settings"));
    const previousMonitors = Array.isArray(previous.monitors) ? previous.monitors : (Array.isArray(previous.local_device?.monitors) ? previous.local_device.monitors : []);
    const monitors = Array.from({ length: monitorCount }, (_, index) => {
      const before = previousMonitors[index] || {};
      return { ...before, fingerprint: displayFingerprints[index] || before.edid_id || before.fingerprint || `local-display-${index + 1}`, label: model.displays[index] || t("monitor.value", {
  p0: index + 1
}), local_input: safe(localComputer.sources[index], before.local_input ?? 0), source: model.localInputSources?.[index] || before.here_input_source || before.source || 'unknown' };
    });
    const localDeviceId = previous.local_device?.device_id || localComputer.id;
    const inputFor = (computer, index) => computer.local ? monitors[index].local_input : computer.sources[index];
    for (const computer of model.computers) {
      const missing = monitors.findIndex((_, index) => {
        const value = inputFor(computer, index);
        return value == null || value === '' || !Number.isFinite(Number(value));
      });
      if (missing >= 0) throw new Error(t("enter.the.input.value.for.value.on.value.before.saving", {
  p0: computer.name,
  p1: monitors[missing].label
}));
    }
    return {
      ...(previous.schema_version === 2 ? previous : {}),
      schema_version: 2,
      local_device: { ...previous.local_device, device_id: localDeviceId, host_label: localComputer.name, monitors },
      switch_group: {
        ...previous.switch_group,
        group_id: previous.switch_group?.group_id || 'local-switch-group',
        revision: Math.max(1, Number(previous.switch_group?.revision || 0) + 1),
        devices: model.computers.map((computer) => ({
          ...previous.switch_group?.devices?.find(device => device.device_id === computer.id),
          device_id: computer.local ? localDeviceId : computer.id,
          name: computer.name,
          port_index: Number(computer.port),
          monitors: monitors.map((monitor, index) => ({ fingerprint: monitor.fingerprint, label: monitor.label, local_input: Number(inputFor(computer, index)) })),
        })),
      },
      trigger: { ...(learnedTrigger || previous.trigger || { anchor: { vid_pid: '' }, members: [] }), debounce: { ...debounceSettings } },
      advanced: {
        ...previous.advanced,
        ddc_retry: previous.advanced?.ddc_retry || { attempts: 2, delay_ms: 100 },
        usb_poll_ms: previous.advanced?.usb_poll_ms || 250,
        arrival_correction_enabled: arrivalCorrectionEnabled,
      },
    };
  }

  async function persistManualMapping(computerGroupOnly = false) {
    return serializeConfigWrite(async () => {
      if (computerGroupOnly && !sidecarConfig) {
        localStorage.setItem(computerGroupDraftKey, JSON.stringify({
          localId: model.localId, nextId: model.nextId,
          computers: [...model.computers].sort((a, b) => a.port - b.port),
        }));
        return;
      }
      const next = localConfigFromModel();
      if (computerGroupOnly) {
        next.trigger = structuredClone(sidecarConfig.trigger);
        next.advanced = structuredClone(sidecarConfig.advanced);
      }
      if ('host_label' in next) next.host_label = current().name;
      await S.request('config.set', { config: next });
      sidecarConfig = next;
      recalibrationNeedsPersist = false;
      localStorage.removeItem(computerGroupDraftKey);
      if (!computerGroupOnly && next.trigger?.anchor?.vid_pid && next.trigger?.members?.length) await S.request('usb.watch.start');
    });
  }

  const redrawAll = () => { navState(); renderStatus(); renderDiagnostics(); if (!byId('guide-page').classList.contains('hidden')) renderGuide(); if (!byId('settings-page').classList.contains('hidden')) { renderComputers(); renderTimingSettings(); } };
  const originalConfirmUsb = window.confirmUsb;
  const calibrationUi = (title, detail, buttonLabel, disabled = false) => {
    calibrationSnapshot = [title, detail, buttonLabel].map(value => window.KVMFlowI18n.describe(value));
    calibrationSnapshot.push(disabled);
    const callout = document.querySelector('#guide-content .callout');
    const button = document.querySelector('#guide-content .wizard-footer .primary');
    if (callout) callout.innerHTML = `<strong>${escapeHtml(title)}</strong>${detail ? `<p class="hint">${escapeHtml(detail)}</p>` : ''}`;
    if (button) { button.textContent = buttonLabel; button.disabled = disabled; }
  };
  const clearCalibrationHooks = () => {
    if (calibrationTimer) clearTimeout(calibrationTimer);
    calibrationTimer = null;
    if (stopCandidateListener) stopCandidateListener();
    stopCandidateListener = null;
    calibrationActive = false;
  };
  const endCalibration = async () => {
    clearCalibrationHooks();
    try { await S.request('wizard.end'); } catch { /* sidecar may already be stopping */ }
  };
  const failCalibration = async (title, detail) => {
    await endCalibration();
    model.usbConfirmed = false;
    storePrototypeModel();
    navState();
    calibrationUi(title, detail, t("identify.again"));
    log(title, detail);
  };
  const displayDetectionUi = (title, detail, buttonLabel, disabled = false) => {
    if (step !== 1) return;
    displayDetectionSnapshot = [title, detail, buttonLabel].map(value => window.KVMFlowI18n.describe(value));
    displayDetectionSnapshot.push(disabled);
    byId('guide-content').innerHTML = `
      <div class="wizard-head"><h1>${t("remember.this.computer.s.input.values")}</h1><p class="sub">${t("first.check.that.the.monitors.work.with.this.computer")}</p></div>
      <div class="callout"><strong>${escapeHtml(title)}</strong>${detail ? `<p class="hint">${escapeHtml(detail)}</p>` : ''}</div>
      <div class="wizard-footer"><button class="secondary" data-action="previousStep">${t("back")}</button><button class="primary" data-action="retryDisplayDetection" ${disabled ? 'disabled' : ''}>${buttonLabel}</button></div>`;
  };
  const updateFinalStepUi = () => {
    const status = byId('test-status');
    const copy = byId('test-status-copy');
    const action = byId('test-action-hint');
    if (!status || !copy || !action) return;
    const outcomes = Array.isArray(lastSwitchReport?.per_monitor) ? lastSwitchReport.per_monitor : [];
    const ok = outcomes.length > 0 && outcomes.every((entry) => entry.commanded === true || entry.ok === true);
    if (lastSwitchReport) {
      status.querySelector('strong').textContent = ok ? t("switch.commands.sent") : t("switch.failed");
      copy.textContent = ok ? t("check.the.monitor.s.picture") : t("open.diagnostics.to.see.why.switching.failed");
      action.textContent = ok ? t("command.accepted") : t("you.can.press.usb.switch.again");
      return;
    }
    if (runtimeState === 'pushing') {
      status.querySelector('strong').textContent = t("switching.monitors");
      copy.textContent = t("usb.switch.departure.detected.sending.monitor.commands");
      action.textContent = t("working");
      return;
    }
    if (runtimeState === 'cooldown') {
      status.querySelector('strong').textContent = t("command.accepted");
      copy.textContent = t("monitor.commands.sent.check.whether.the.picture.has.switched");
      action.textContent = t("command.accepted");
      return;
    }
    status.querySelector('strong').textContent = t("waiting.for.usb.switch");
    copy.textContent = t("press.the.physical.button.kvmflow.will.update.the.status.from.the.actual");
    action.textContent = t("waiting.for.usb.switch");
  };
  const originalRenderGuide = window.renderGuide;
  if (typeof originalRenderGuide === 'function') {
    window.renderGuide = (...args) => {
      displayDetectionSnapshot = null;
      const result = originalRenderGuide(...args);
      updateFinalStepUi();
      if (step === 0 && model.usbConfirmed) {
        const footer = byId('guide-content')?.querySelector('.wizard-footer');
        if (footer && !byId('recalibrate-usb')) {
          const button = document.createElement('button');
          button.id = 'recalibrate-usb';
          button.type = 'button';
          button.className = 'secondary';
          button.textContent = t("identify.usb.switch.again");
          button.addEventListener('click', () => { void window.recalibrateUsb(); });
          footer.insertBefore(button, footer.querySelector('.primary'));
        }
      }
      return result;
    };
  }
  const detectDisplaysForStepTwo = async () => {
    if (displayDetectionActive || step !== 1) return;
    displayDetectionActive = true;
    displayDetectionUi(t("detecting.monitors"), t("keep.the.monitors.displaying.this.computer"), t("detecting"), true);
    try {
      const displays = await S.request('display.list');
      applySidecarConfig(sidecarConfig, displays);
      if (readableDisplayCount === 0) {
        displayDetectionUi(t("no.controllable.monitors.detected"), t("switch.the.monitor.to.this.computer.enable.ddc.ci.and.try.again"), t("detect.again"));
        log(t("monitor.temporarily.unavailable"), t("no.external.monitor.could.be.read.through.ddc.ci"));
        return;
      }
      renderGuide();
      log(t("monitors.detected"), t("read.value.controllable.monitors", {
  p0: readableDisplayCount
}));
    } catch (error) {
      displayDetectionUi(t("monitor.detection.failed"), String(error?.message || t("check.the.connections.and.try.again")), t("detect.again"));
    } finally {
      displayDetectionActive = false;
    }
  };
  window.retryDisplayDetection = () => { void detectDisplaysForStepTwo(); };
  window.confirmUsb = async () => {
    if (calibrationActive) return;
    if (model.usbConfirmed && validTrigger(learnedTrigger || sidecarConfig?.trigger)) {
      originalConfirmUsb();
      await detectDisplaysForStepTwo();
      return;
    }
    calibrationActive = true;
    let sawNonHubUsbChange = false;
    model.usbConfirmed = false;
    storePrototypeModel();
    calibrationUi(t("preparing.identification"), t("do.not.switch.yet"), t("listening"), true);
    stopCandidateListener = S.onNotification('wizard.candidates', async (data) => {
      if (!calibrationActive) return;
      const devices = Array.isArray(data?.disappeared)
        ? data.disappeared.filter((device) => typeof device?.vid_pid === 'string' && device.vid_pid)
        : [];
      // A receiver or keyboard can disappear before the USB Switch's hub.
      // Wait for a positively identified hub; never guess from array order or
      // a missing serial, since Windows instance IDs are usually non-empty.
      const hubCandidates = devices.filter((device) => /\bhub\b|集线器/i.test(device.product || ''));
      const hub = hubCandidates[0];
      const members = devices.filter((device) => device !== hub);
      if (!hub || members.length === 0) {
        if (!sawNonHubUsbChange) {
          sawNonHubUsbChange = true;
          calibrationUi(t("ordinary.usb.device.change.detected"), t("keep.using.usb.switch.unplugging.just.a.mouse.or.keyboard.will.not.ident"), t("waiting.for.switch.hub"), true);
        }
        return;
      }
      learnedTrigger = {
        anchor: { vid_pid: hub.vid_pid },
        members: members.map((device) => ({ vid_pid: device.vid_pid, ...(device.serial ? { serial: device.serial } : {}) })),
        debounce: { ...debounceSettings },
      };
      sidecarConfig = { ...(sidecarConfig || {}), trigger: learnedTrigger };
      await endCalibration();
      model.usbConfirmed = true;
      storePrototypeModel();
      if (recalibrationNeedsPersist) {
        try {
          const updatedConfig = localConfigFromModel();
          await S.request('config.set', { config: updatedConfig });
          sidecarConfig = updatedConfig;
          recalibrationNeedsPersist = false;
        } catch (error) {
          log(t("new.identification.has.not.been.saved"), String(error?.message || t("continue.setup.and.save.the.settings")));
        }
      }
      originalConfirmUsb();
      log(t("usb.switch.identified"), t("recorded.the.hub.and.value.connected.devices", {
  p0: members.length
}));
      await detectDisplaysForStepTwo();
    });
    try {
      await S.request('wizard.begin');
      calibrationUi(t("press.usb.switch.once.now"), t("switch.the.keyboard.and.mouse.to.the.other.computer.then.switch.back.her"), t("waiting.for.a.switch"), true);
      calibrationTimer = setTimeout(() => {
        if (calibrationActive) void failCalibration(t("usb.switch.not.detected"), t("check.the.usb.upstream.cable.and.complete.a.switch.within.60.seconds"));
      }, 60000);
    } catch (error) {
      await failCalibration(t("unable.to.start.identification"), String(error?.message || t("check.the.connections.and.try.again")));
    }
  };
  window.recalibrateUsb = async () => {
    if (calibrationActive) return;
    const previousConfig = sidecarConfig;
    const previousTrigger = learnedTrigger;
    let configCleared = false;
    try {
      await S.request('usb.watch.stop');
      learnedTrigger = null;
      sidecarConfig = {
        ...(sidecarConfig || {}),
        trigger: {
          anchor: { vid_pid: '' },
          members: [],
          debounce: { ...debounceSettings },
        },
      };
      const clearedConfig = localConfigFromModel();
      sidecarConfig = clearedConfig;
      await S.request('config.set', { config: clearedConfig });
      configCleared = true;
      recalibrationNeedsPersist = true;
      model.usbConfirmed = false;
      storePrototypeModel();
      navState();
      renderStatus();
      step = 0;
      renderGuide();
      await window.confirmUsb();
    } catch (error) {
      let previousConfigRestored = !configCleared;
      if (configCleared && previousConfig) {
        try {
          await S.request('config.set', { config: previousConfig });
          previousConfigRestored = true;
        } catch { /* report that the old configuration could not be restored */ }
      }
      sidecarConfig = previousConfig;
      learnedTrigger = previousTrigger;
      model.usbConfirmed = true;
      storePrototypeModel();
      redrawAll();
      try { await S.request('usb.watch.start'); } catch { /* keep the original error */ }
      log(
        t("unable.to.identify.usb.switch.again"),
        previousConfigRestored
          ? String(error?.message || t("previous.configuration.retained.try.again"))
          : t("the.previous.trigger.configuration.could.not.be.restored.identify.usb.sw"),
      );
    }
  };
  const originalSaveTarget = window.saveTarget;
  window.saveTarget = async () => {
    const previousModel = structuredClone(model);
    lastSwitchReport = null;
    window.__kvmflowLastSwitchReport = null;
    try {
      originalSaveTarget();
      await persistManualMapping();
      log(t("saved.on.this.computer"), t("the.other.computer.s.input.values.have.been.saved.locally")); redrawAll();
    } catch (error) {
      model = previousModel; step = 2; storePrototypeModel(); redrawAll();
      log(t("local.configuration.not.saved"), String(error?.message || t("please.save.again")));
    }
  };
  const originalSaveSettings = window.saveSettings;
  byId('save-settings').textContent = t("save.computer.settings");
  window.saveSettings = async () => {
    const button = byId('save-settings');
    if (button.disabled) return;
    button.disabled = true;
    const previousModel = structuredClone(model);
    const structureRevision = window.computerSettingsStructureRevision?.() ?? 0;
    try {
      if (!(await window.confirmLocalInputs())) return;
      originalSaveSettings();
      await persistManualMapping(true);
      window.dispatchEvent(new CustomEvent('settings-saved', {
        detail: { structureRevision },
      }));
      log(t("computer.group.saved"), sidecarConfig ? t("computer.names.ports.and.monitor.input.values.have.been.saved") : t("this.computer.group.will.be.used.for.automatic.switching.after.setup.is."));
    } catch (error) {
      model = previousModel;
      storePrototypeModel();
      log(t("local.configuration.not.saved"), String(error?.message || t("please.save.again")));
      redrawAll();
    } finally { button.disabled = false; }
  };
  const timingControls = [
    ['timing-absent', 't_absent_ms'],
    ['timing-stable', 't_stable_ms'],
    ['timing-cooldown', 't_cooldown_ms'],
  ];
  for (const [id, key] of timingControls) {
    byId(id)?.addEventListener('change', async (event) => {
      const control = event.currentTarget;
      const value = Number(control.value);
      control.disabled = true;
      try {
        await serializeConfigWrite(async () => {
          const timing = { ...debounceSettings, [key]: value };
          if (sidecarConfig) {
            const next = structuredClone(sidecarConfig);
            next.trigger.debounce = timing;
            await S.request('config.set', { config: next });
            sidecarConfig = next;
            if (learnedTrigger) learnedTrigger = { ...learnedTrigger, debounce: timing };
          } else {
            localStorage.setItem(timingDraftKey, JSON.stringify(timing));
          }
          debounceSettings = timing;
        });
        log(t("switch.timing.saved"), t("this.takes.effect.independently.and.does.not.save.the.computer.you.are.e"));
      } catch (error) {
        renderTimingSettings();
        log(t("switch.timing.not.saved"), String(error?.message || t("please.try.again")));
      } finally { control.disabled = false; }
    });
  }
  byId('arrival-correction')?.addEventListener('change', async (event) => {
    const control = event.currentTarget;
    const previous = arrivalCorrectionEnabled;
    arrivalCorrectionEnabled = model.computers.length <= 2 && control.checked === true;
    const label = control.closest('.toggle-control')?.querySelector('.toggle-label');
    if (label) label.textContent = control.checked ? t("on") : t("off");
    try {
      await persistManualMapping();
      log(arrivalCorrectionEnabled ? t("host.mode.enabled") : t("host.mode.disabled"), t("settings.saved.on.this.computer"));
    } catch (error) {
      arrivalCorrectionEnabled = previous;
      control.checked = previous;
      if (label) label.textContent = previous ? t("on") : t("off");
      log(t("host.mode.not.saved"), String(error?.message || t("please.try.again")));
    }
  });
  const updateStartupLabel = () => {
    const control = byId('startup-at-login');
    const label = control?.closest('.toggle-control')?.querySelector('.toggle-label');
    if (control && label) label.textContent = control.checked ? t("on") : t("off");
  };
  startupControl?.addEventListener('change', async (event) => {
    const control = event.currentTarget;
    try {
      const enabled = await S.startup.set(control.checked);
      control.checked = enabled;
      updateStartupLabel();
      log(enabled ? t("launch.at.login.enabled") : t("launch.at.login.disabled"), enabled ? t("kvmflow.will.run.in.the.background.when.you.sign.in") : t("kvmflow.will.not.launch.automatically.when.you.sign.in"));
    } catch (error) {
      control.checked = !control.checked;
      updateStartupLabel();
      log(t("could.not.save.launch.at.login.settings"), String(error?.message || t("please.try.again.later")));
    }
  });
  if (startupControl) {
    S.startup.get().then((enabled) => {
      startupControl.checked = enabled === true;
      startupControl.disabled = false;
      updateStartupLabel();
    }).catch(() => {
      startupControl.checked = false;
      startupControl.disabled = true;
      const label = startupControl.closest('.toggle-control')?.querySelector('.toggle-label');
      if (label) label.textContent = t("unable.to.read");
      log(t("unable.to.read.launch.at.login.status"), t("restart.the.app.and.try.again.in.settings"));
    });
  }
  const languageControl = byId('language-select');
  const refreshLanguage = () => {
    // Rerender translated UI without saving or discarding an in-progress edit.
    const editor = model.computers.find(computer => computer.id === editingId);
    const remote = target();
    const fields = [...document.querySelectorAll('#computer-editor input, #computer-editor select, #guide-content input')]
      .map(field => ({ id: field.id, value: field.value, confirmed: field.dataset.confirmedValue,
        preserve: !(field.id === 'edit-name' && editor?.nameKey && field.value.trim() === editor.name)
          && !(field.id === 'target-name' && remote?.nameKey && field.value.trim() === remote.name) }));
    const focused = document.activeElement?.id;
    const originHidden = byId('local-input-origin')?.hidden;
    const calibration = calibrationSnapshot;
    const detection = displayDetectionSnapshot;
    if (computerNameIsFallback) computerName = t('this.computer');
    for (const computer of model.computers) if (computer.nameKey) computer.name = t(computer.nameKey);
    navState(); renderStatus(); renderDiagnostics();
    if (!byId('settings-page').classList.contains('hidden') && !byId('local-input-confirmation').open) renderComputers();
    if (!byId('guide-page').classList.contains('hidden')) {
      renderGuide();
      if (step === 0 && !model.usbConfirmed && calibration) {
        calibrationUi(...calibration.slice(0, 3).map(window.KVMFlowI18n.format), calibration[3]);
      }
      if (step === 1 && detection) {
        displayDetectionUi(...detection.slice(0, 3).map(window.KVMFlowI18n.format), detection[3]);
      }
    }
    renderTimingSettings(); updateStartupLabel(); updateFinalStepUi();
    for (const draft of fields) {
      const field = byId(draft.id);
      if (!field || !draft.preserve) continue;
      field.value = draft.value;
      if (draft.confirmed != null) field.dataset.confirmedValue = draft.confirmed;
    }
    if (originHidden && byId('local-input-origin')) byId('local-input-origin').hidden = true;
    if (focused) byId(focused)?.focus();
    if (languageControl) languageControl.value = window.KVMFlowI18n.preference;
  };
  window.addEventListener('language-changed', refreshLanguage);
  if (languageControl) {
    languageControl.value = window.KVMFlowI18n.preference;
    languageControl.addEventListener('change', async () => {
      const preference = languageControl.value;
      languageControl.disabled = true;
      try {
        await S.language?.set(preference, window.KVMFlowI18n.systemLocale());
        window.KVMFlowI18n.setPreference(preference);
        byId('language-save-status').dataset.i18n = 'language.saved';
        byId('language-save-status').textContent = t('language.saved');
      } catch {
        languageControl.value = window.KVMFlowI18n.preference;
        byId('language-save-status').dataset.i18n = 'language.failed';
        byId('language-save-status').textContent = t('language.failed');
      } finally { languageControl.disabled = false; }
    });
  }
  byId('add-computer')?.addEventListener('click', () => setTimeout(renderTimingSettings, 0));
  byId('computer-list')?.addEventListener('click', (event) => {
    if (event.target.closest('.danger-button')) setTimeout(renderTimingSettings, 0);
  });
  document.querySelectorAll('[data-page="settings"]').forEach((control) => {
    control.addEventListener('click', () => setTimeout(renderTimingSettings, 0));
  });
  let settingsDetectionActive = false;
  const originalNavigate = window.navigate;
  window.navigate = (page) => {
    originalNavigate(page);
    if (page !== 'settings' || model.displays.length || settingsDetectionActive || calibrationActive) return;
    settingsDetectionActive = true;
    void (async () => {
      try {
        const response = await S.request('display.list');
        const displays = availableDisplays(response).filter(display => display.ddc?.state === 'available' &&
          Number.isInteger(display.ddc.input) && display.ddc.input >= 0 && display.ddc.input <= 255);
        if (calibrationActive || model.displays.length || !displays.length) return;
        // Keep computer drafts and in-flight edits; only fill missing monitor data.
        const draftName = byId('edit-name')?.value;
        const draftPort = byId('edit-port')?.value;
        model.displays = displays.map((display, index) => display.label || display.model_name || t("monitor.value", {
  p0: index + 1
}));
        displayFingerprints = displays.map((display, index) => display.edid_id || display.fingerprint || `local-display-${index + 1}`);
        current().sources = displays.map(display => display.ddc.input);
        model.localInputSources = displays.map(() => 'learned_active_read');
        for (const computer of model.computers) {
          if (!computer.local) computer.sources = displays.map((_, index) => computer.sources[index] ?? null);
        }
        if (!byId('settings-page').classList.contains('hidden')) {
          renderComputers();
          if (draftName != null) byId('edit-name').value = draftName;
          if (draftPort != null) byId('edit-port').value = draftPort;
        }
      } catch (error) { log(t("unable.to.read.monitor.input.values"), error?.message || t("check.the.monitor.connections")); }
      finally { settingsDetectionActive = false; }
    })();
  };
  byId('export-log').addEventListener('click', async (event) => {
    event.preventDefault();
    event.stopImmediatePropagation();
    try {
      const [diagnosticsResult, configResult] = await Promise.allSettled([
        S.request('diagnostics.collect', { tail: 200 }),
        S.request('config.get'),
      ]);
      const payload = {
        exported_at: new Date().toISOString(),
        ui: model,
        config: configResult.status === 'fulfilled' ? configResult.value : null,
        sidecar: diagnosticsResult.status === 'fulfilled' ? diagnosticsResult.value : null,
        errors: [diagnosticsResult, configResult].filter((result) => result.status === 'rejected').map((result) => String(result.reason?.message || result.reason)),
      };
      if (await S.exportDiagnostics(payload)) log(t("diagnostics.exported"), t("diagnostics.saved.to.the.selected.location"));
    } catch (error) { log(t("export.failed"), String(error?.message || t("unable.to.export.records"))); }
  }, true);
  S.onClientNavigate((page) => navigate(page === 'wizard' ? 'guide' : page));
  S.onNotification('state', (state) => {
    runtimeState = state?.enabled === false ? 'disabled' : state?.state || runtimeState;
    window.__kvmflowRuntimeState = runtimeState;
    navState();
    renderStatus();
    updateFinalStepUi();
    if (runtimeState === 'pushing') log(t("sending.switch.commands"), t("the.monitors.are.responding"));
    if (runtimeState === 'idle') log(t("waiting.for.usb.switch"), t("kvmflow.is.ready"));
  });
  S.onNotification('switch.report', (report) => {
    lastSwitchReport = report || null;
    window.__kvmflowLastSwitchReport = lastSwitchReport;
    renderStatus();
    updateFinalStepUi();
    const outcomes = Array.isArray(report?.per_monitor) ? report.per_monitor : [];
    const ok = outcomes.length > 0 && outcomes.every((entry) => entry.commanded === true || entry.ok === true);
    const box = byId('test-status');
    if (box) updateFinalStepUi();
    log(ok ? t("monitor.switch.commands.sent") : t("monitor.switching.failed"), ok ? t("check.the.actual.monitor.picture") : t("check.the.monitor.input.sources.in.diagnostics"));
  });
  S.onNotification('runtime.error', (error) => {
    runtimeState = 'error';
    window.__kvmflowRuntimeState = runtimeState;
    navState();
    log(t("background.component.unavailable"), error?.message || t("restart.the.app.and.check.diagnostics"));
  });
  S.request('state.get').then((state) => {
    runtimeState = state?.enabled === false ? 'disabled' : state?.state || runtimeState;
    lastSwitchReport = state?.lastMultiDeviceReport || state?.lastReport || null;
    window.__kvmflowLastSwitchReport = lastSwitchReport;
    window.__kvmflowRuntimeState = runtimeState;
    navState();
    updateFinalStepUi();
    renderStatus();
  }).catch((error) => {
    window.__kvmflowRuntimeState = 'error'; navState();
    log(t("background.component.unavailable"), error?.message || t("restart.the.app"));
  });
  try {
    const info = await S.info;
    computerNameIsFallback = !(typeof info.computerName === 'string' && info.computerName.trim());
    computerName = typeof info.computerName === 'string' && info.computerName.trim()
      ? info.computerName.trim() : t("this.computer");
    const config = await S.request('config.get');
    applySidecarConfig(config, null);
    redrawAll();
    const initialPage = S.initialPage || info.initialPage;
    navigate(initialPage === 'wizard' ? 'guide' : initialPage);
    await S.clientReady();
    // Display probes run in setup, where the user has put the physical
    // screen on this host. Loading the saved mappings needs no DDC traffic.
    if (!timingMigrationPending || !sidecarConfig || !validTrigger(sidecarConfig.trigger)) return;
    try {
      await S.request('config.set', { config: sidecarConfig });
      await S.request('usb.watch.start');
      timingMigrationPending = false;
      log(t("switch.timing.updated"), t("monitor.commands.will.now.be.sent.immediately.after.usb.switch.departure"));
    } catch (error) {
      log(t("switch.timing.not.updated"), String(error?.message || t("save.again.in.settings")));
    }
  } catch (error) {
    log(t("unable.to.read.this.computer.s.configuration"), error?.message || t("restart.the.app.and.try.again"));
    await S.clientReady();
  }
})().catch(error => console.error('KVMFlow initialization failed', error));
