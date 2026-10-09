// Connect the local UI to the shared Rust runtime through Tauri commands.
(async () => {
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
  let computerName = '本机';
  let learnedTrigger = null;
  let calibrationActive = false;
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
    row.innerHTML = '<div class="response-copy"><strong>开机自动启动</strong></div><label class="toggle-control" for="startup-at-login"><input id="startup-at-login" type="checkbox" checked><span class="toggle-track" aria-hidden="true"></span><span class="toggle-label">开启</span></label>';
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
      option.textContent = `${value / 1000} 秒（自定义）`;
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
      if (label) label.textContent = unavailable ? '不可用' : arrivalCorrectionEnabled ? '开启' : '关闭';
      if (description) description.textContent = unavailable
        ? `当前有 ${model.computers.length} 台电脑。主机模式仅支持两台电脑。`
        : '开启后，只需运行本机 KVMFlow，即可接管另一台电脑的切换。实验功能：仅当显示器在非当前输入上仍响应 DDC 时有效，否则两台电脑都需要运行 KVMFlow。';
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
        name: device.name || '电脑',
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
    localComputer.sources = localMonitors.map((monitor, index) => safe(monitor.here_input ?? monitor.local_input ?? monitor.ddc?.input, localComputer.sources[index]));
    model.localInputSources = localMonitors.map(monitor => monitor.source || monitor.here_input_source ||
      (monitor.ddc?.state === 'available' ? 'learned_active_read' : 'unknown'));
    remoteComputer.name = remoteGroupDevice?.name || remoteComputer.name;
    remoteComputer.sources = localMonitors.map((monitor, index) => {
      const fingerprint = monitor.edid_id || monitor.fingerprint;
      const groupMonitor = remoteGroupDevice?.monitors?.find((entry) => entry.fingerprint === fingerprint);
      return safe(groupMonitor?.local_input ?? legacyMonitors[index]?.away_input ?? localProfileMonitors[index]?.legacy_remote_preset?.value, remoteComputer.sources[index]);
    });
    if (localGroupDevice) localComputer.port = Number(localGroupDevice.port_index) || localComputer.port;
    if (remoteGroupDevice) remoteComputer.port = Number(remoteGroupDevice.port_index) || remoteComputer.port;
    displayFingerprints = localMonitors.map((monitor, index) => monitor.edid_id || monitor.fingerprint || `local-display-${index + 1}`);
    model.displays = localMonitors.map((monitor, index) => monitor.label || monitor.model_name || `显示器 ${index + 1}`);
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
    if (!monitorCount) throw new Error('请先检测显示器，再保存设置。');
    const previousMonitors = Array.isArray(previous.monitors) ? previous.monitors : (Array.isArray(previous.local_device?.monitors) ? previous.local_device.monitors : []);
    const monitors = Array.from({ length: monitorCount }, (_, index) => {
      const before = previousMonitors[index] || {};
      return { ...before, fingerprint: displayFingerprints[index] || before.edid_id || before.fingerprint || `local-display-${index + 1}`, label: model.displays[index] || `显示器 ${index + 1}`, local_input: safe(localComputer.sources[index], before.local_input ?? 0), source: model.localInputSources?.[index] || before.here_input_source || before.source || 'unknown' };
    });
    const localDeviceId = previous.local_device?.device_id || localComputer.id;
    const inputFor = (computer, index) => computer.local ? monitors[index].local_input : computer.sources[index];
    for (const computer of model.computers) {
      const missing = monitors.findIndex((_, index) => {
        const value = inputFor(computer, index);
        return value == null || value === '' || !Number.isFinite(Number(value));
      });
      if (missing >= 0) throw new Error(`请填写“${computer.name}”在“${monitors[missing].label}”上的输入，再保存。`);
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
    calibrationUi(title, detail, '重新识别');
    log(title, detail);
  };
  const displayDetectionUi = (title, detail, buttonLabel, disabled = false) => {
    if (step !== 1) return;
    byId('guide-content').innerHTML = `
      <div class="wizard-head"><h1>记住这台电脑的输入值</h1><p class="sub">先确认显示器在这台电脑上可用。</p></div>
      <div class="callout"><strong>${escapeHtml(title)}</strong>${detail ? `<p class="hint">${escapeHtml(detail)}</p>` : ''}</div>
      <div class="wizard-footer"><button class="secondary" data-action="previousStep">上一步</button><button class="primary" data-action="retryDisplayDetection" ${disabled ? 'disabled' : ''}>${buttonLabel}</button></div>`;
  };
  const updateFinalStepUi = () => {
    const status = byId('test-status');
    const copy = byId('test-status-copy');
    const action = byId('test-action-hint');
    if (!status || !copy || !action) return;
    const outcomes = Array.isArray(lastSwitchReport?.per_monitor) ? lastSwitchReport.per_monitor : [];
    const ok = outcomes.length > 0 && outcomes.every((entry) => entry.commanded === true || entry.ok === true);
    if (lastSwitchReport) {
      status.querySelector('strong').textContent = ok ? '切换指令已发送' : '切换失败';
      copy.textContent = ok ? '请确认显示器画面。' : '请打开诊断查看失败原因。';
      action.textContent = ok ? '指令已接受' : '可以重新按一次 USB Switch';
      return;
    }
    if (runtimeState === 'pushing') {
      status.querySelector('strong').textContent = '正在切换显示器';
      copy.textContent = '已检测到 USB Switch 切出，正在发送显示器指令。';
      action.textContent = '正在处理…';
      return;
    }
    if (runtimeState === 'cooldown') {
      status.querySelector('strong').textContent = '指令已接受';
      copy.textContent = '显示器指令已发送，请确认画面是否已经切换。';
      action.textContent = '指令已接受';
      return;
    }
    status.querySelector('strong').textContent = '等待 USB Switch';
    copy.textContent = '按下实体按钮，KVMFlow 会根据真实切换结果更新状态。';
    action.textContent = '等待 USB Switch';
  };
  const originalRenderGuide = window.renderGuide;
  if (typeof originalRenderGuide === 'function') {
    window.renderGuide = (...args) => {
      const result = originalRenderGuide(...args);
      updateFinalStepUi();
      if (step === 0 && model.usbConfirmed) {
        const footer = byId('guide-content')?.querySelector('.wizard-footer');
        if (footer && !byId('recalibrate-usb')) {
          const button = document.createElement('button');
          button.id = 'recalibrate-usb';
          button.type = 'button';
          button.className = 'secondary';
          button.textContent = '重新识别 USB Switch';
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
    displayDetectionUi('正在检测显示器', '请保持显示器画面在这台电脑上。', '正在检测…', true);
    try {
      const displays = await S.request('display.list');
      applySidecarConfig(sidecarConfig, displays);
      if (readableDisplayCount === 0) {
        displayDetectionUi('没有检测到可控制的显示器', '请把显示器切到这台电脑，开启 DDC/CI 后重试。', '重新检测');
        log('显示器暂不可用', '没有检测到可通过 DDC/CI 读取的外接显示器。');
        return;
      }
      renderGuide();
      log('已检测显示器', `已读取 ${readableDisplayCount} 台可控制的显示器。`);
    } catch (error) {
      displayDetectionUi('显示器检测失败', String(error?.message || '请检查连接后重试。'), '重新检测');
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
    calibrationUi('正在准备识别', '请暂时不要切换。', '正在监听…', true);
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
          calibrationUi('检测到普通 USB 设备变化', '继续按 USB Switch 切换；单独拔插鼠标或键盘不会被记录为切换器。', '等待 Switch Hub…', true);
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
          log('重新识别结果暂未保存', String(error?.message || '请继续初始化并保存设置。'));
        }
      }
      originalConfirmUsb();
      log('已识别 USB Switch', `已记录 Hub 和 ${members.length} 个连接设备。`);
      await detectDisplaysForStepTwo();
    });
    try {
      await S.request('wizard.begin');
      calibrationUi('现在按一次 USB Switch', '键鼠切到另一台电脑后，再切回本机继续。', '等待切换…', true);
      calibrationTimer = setTimeout(() => {
        if (calibrationActive) void failCalibration('没有检测到 USB Switch', '请检查 USB 上行线，并在 60 秒内完成一次切换。');
      }, 60000);
    } catch (error) {
      await failCalibration('无法开始识别', String(error?.message || '请检查连接后重试。'));
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
        '无法重新识别 USB Switch',
        previousConfigRestored
          ? String(error?.message || '原有配置已保留，请重试。')
          : '旧触发配置未能恢复，请重新识别 USB Switch。',
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
      log('已保存到本机', '另一台电脑的输入值已写入本地配置。'); redrawAll();
    } catch (error) {
      model = previousModel; step = 2; storePrototypeModel(); redrawAll();
      log('本地配置未保存', String(error?.message || '请重新保存。'));
    }
  };
  const originalSaveSettings = window.saveSettings;
  byId('save-settings').textContent = '保存电脑配置';
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
      log('电脑配置组已保存', sidecarConfig ? '电脑名称、端口和显示器输入值已保存。' : '初始化完成后，这组电脑配置将用于自动切换。');
    } catch (error) {
      model = previousModel;
      storePrototypeModel();
      log('本地配置未保存', String(error?.message || '请重新保存。'));
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
        log('切换响应已保存', '此设置单独生效，不会保存正在编辑的电脑。');
      } catch (error) {
        renderTimingSettings();
        log('切换响应未保存', String(error?.message || '请重试。'));
      } finally { control.disabled = false; }
    });
  }
  byId('arrival-correction')?.addEventListener('change', async (event) => {
    const control = event.currentTarget;
    const previous = arrivalCorrectionEnabled;
    arrivalCorrectionEnabled = model.computers.length <= 2 && control.checked === true;
    const label = control.closest('.toggle-control')?.querySelector('.toggle-label');
    if (label) label.textContent = control.checked ? '开启' : '关闭';
    try {
      await persistManualMapping();
      log(arrivalCorrectionEnabled ? '已开启主机模式' : '已关闭主机模式', '设置已保存到本机。');
    } catch (error) {
      arrivalCorrectionEnabled = previous;
      control.checked = previous;
      if (label) label.textContent = previous ? '开启' : '关闭';
      log('主机模式未保存', String(error?.message || '请重试。'));
    }
  });
  const updateStartupLabel = () => {
    const control = byId('startup-at-login');
    const label = control?.closest('.toggle-control')?.querySelector('.toggle-label');
    if (control && label) label.textContent = control.checked ? '开启' : '关闭';
  };
  startupControl?.addEventListener('change', async (event) => {
    const control = event.currentTarget;
    try {
      const enabled = await S.startup.set(control.checked);
      control.checked = enabled;
      updateStartupLabel();
      log(enabled ? '已开启开机自动启动' : '已关闭开机自动启动', enabled ? '登录系统后将在后台运行 KVMFlow。' : '登录系统后不会自动启动 KVMFlow。');
    } catch (error) {
      control.checked = !control.checked;
      updateStartupLabel();
      log('开机自动启动设置失败', String(error?.message || '请稍后重试。'));
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
      if (label) label.textContent = '无法读取';
      log('无法读取开机启动状态', '请重启应用后，在设置中重试。');
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
        model.displays = displays.map((display, index) => display.label || display.model_name || `显示器 ${index + 1}`);
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
      } catch (error) { log('无法读取显示器输入值', error?.message || '请检查显示器连接。'); }
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
      if (await S.exportDiagnostics(payload)) log('已导出诊断', '诊断文件已保存到选择的位置。');
    } catch (error) { log('导出失败', String(error?.message || '无法导出记录。')); }
  }, true);
  S.onClientNavigate((page) => navigate(page === 'wizard' ? 'guide' : page));
  S.onNotification('state', (state) => {
    runtimeState = state?.enabled === false ? 'disabled' : state?.state || runtimeState;
    window.__kvmflowRuntimeState = runtimeState;
    navState();
    renderStatus();
    updateFinalStepUi();
    if (runtimeState === 'pushing') log('正在发送切换指令', '显示器正在响应。');
    if (runtimeState === 'idle') log('等待 USB Switch', 'KVMFlow 已准备就绪。');
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
    log(ok ? '显示器切换指令已发送' : '显示器切换失败', ok ? '请以显示器实际画面为准。' : '请在诊断中检查显示器输入源。');
  });
  S.onNotification('runtime.error', (error) => {
    runtimeState = 'error';
    window.__kvmflowRuntimeState = runtimeState;
    navState();
    log('后台组件不可用', error?.message || '请重启应用并查看诊断。');
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
    log('后台组件不可用', error?.message || '请重启应用。');
  });
  try {
    const info = await S.info;
    computerName = typeof info.computerName === 'string' && info.computerName.trim()
      ? info.computerName.trim() : '本机';
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
      log('切换响应已更新', '现在将在检测到 USB Switch 切出后立即发送显示器指令。');
    } catch (error) {
      log('切换响应未更新', String(error?.message || '请在设置中重新保存。'));
    }
  } catch (error) {
    log('无法读取本机配置', error?.message || '请重启应用后重试。');
    await S.clientReady();
  }
})().catch(error => console.error('KVMFlow initialization failed', error));
