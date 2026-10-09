const t = (id, params) => window.KVMFlowI18n.t(id, params);
// Local UI model. The persisted Rust configuration is authoritative; this
// cache stores navigation and recent UI events, never hardware evidence.
const key = 'kvmflow-local-prototype-v2';
const windowsHost = /Win/.test(navigator.platform);
const defaults = {
  localId: windowsHost ? 'win' : 'mac', nextId: windowsHost ? 'mac' : 'win',
  usbConfirmed: false,
  computers: [
    { id: windowsHost ? 'win' : 'mac', name: t("this.computer"), nameKey: "this.computer", port: 1, local: true, sources: [] },
    { id: windowsHost ? 'mac' : 'win', name: t("other.computer"), nameKey: "other.computer", port: 2, local: false, sources: [] },
  ],
  displays: [], events: [],
};
let model;
try {
  model = JSON.parse(localStorage.getItem(key));
  if (!Array.isArray(model?.computers) || !model.computers.some(c => c.id === model.localId)) model = null;
} catch { model = null; }
model ||= structuredClone(defaults);
let step = 0, choice = 'known', editingId = model.localId;
let computerSettingsStructureRevision = 0;
const $ = (selector) => document.querySelector(selector);
const escapeHtml = (value) => String(value ?? '').replace(/[&<>"']/g, char => ({
  '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
})[char]);
const source = (value) => value !== '' && value != null && Number.isFinite(Number(value)) ? String(Number(value)) : t("not.set");
const local = () => model.computers.find(c => c.id === model.localId);
const next = () => model.computers.find(c => c.id === model.nextId) || model.computers.find(c => !c.local) || local();
const save = () => localStorage.setItem(key, JSON.stringify(model));
window.computerSettingsStructureRevision = () => computerSettingsStructureRevision;
const markComputerSettingsStructureChanged = () => {
  computerSettingsStructureRevision += 1;
  window.dispatchEvent(new CustomEvent('computer-settings-structure-changed', {
    detail: { revision: computerSettingsStructureRevision },
  }));
};

function log(title, copy) {
  model.events.unshift({ title, copy, titleMessage: window.KVMFlowI18n.describe(title), copyMessage: window.KVMFlowI18n.describe(copy), time: new Date().toLocaleTimeString(window.KVMFlowI18n.locale, { hour: '2-digit', minute: '2-digit' }) });
  model.events = model.events.slice(0, 8);
  save(); renderDiagnostics();
}

function navState() {
  const state = window.__kvmflowRuntimeState;
  const label = state === 'error' ? t("background.component.unavailable")
    : !model.usbConfirmed ? t("setup.required")
    : state === 'learning' ? t("identifying.usb.switch")
    : state === 'pushing' ? t("sending.switch.commands")
    : state === 'disabled' ? t("automatic.switching.paused") : t("waiting.for.usb.switch");
  $('#nav-dot').className = `dot${model.usbConfirmed && state !== 'error' ? ' good' : ''}`;
  $('#nav-state').textContent = label;
  $('#nav-state').title = label;
}

function renderStatus() {
  const a = local(), b = next();
  $('#home-computer').textContent = a.name;
  $('#home-port').textContent = t("port.value", {
  p0: a.port
});
  $('#home-next').textContent = t("value.port.value", {
  p0: b.name,
  p1: b.port
});
  $('#mapping-state').textContent = model.usbConfirmed ? t("input.mappings.saved") : t("incomplete");
  $('#display-table').innerHTML = model.displays.map((name, i) =>
    `<tr><td><strong>${escapeHtml(name)}</strong></td><td>${source(a.sources[i])}</td><td>${source(b.sources[i])}</td></tr>`).join('');
  const report = window.__kvmflowLastSwitchReport;
  const outcomes = report?.per_monitor || [];
  const accepted = outcomes.length && outcomes.every(entry => entry.commanded === true || entry.ok === true);
  $('#last-title').textContent = report ? (accepted ? t("command.accepted") : t("switch.incomplete")) : t("waiting.for.usb.switch");
  $('#last-copy').textContent = report ? t("check.the.actual.display.if.there.is.no.picture.open.diagnostics.and.rec") : t("the.latest.command.result.appears.after.you.press.usb.switch");
  renderTopology();
}

const labels = ["identify.usb.switch", "record.this.computer", "set.up.the.other.computer", "start.switching"];
function renderSteps() {
  $('#steps').innerHTML = labels.map((label, i) =>
    `<div class="step ${i === step ? 'active' : ''} ${i < step ? 'done' : ''}">${i + 1} ${t(label)}</div>`).join('');
  $('#guide-count').textContent = t("step.value.of.4", {
  p0: step + 1
});
}
const copyIcon = '<svg viewBox="0 0 24 24"><rect x="9" y="9" width="10" height="10" rx="1"/><path d="M15 9V5H5v10h4"/></svg>';
function guideView() {
  const a = local(), b = next();
  return [
    `<div class="wizard-head"><h1>${t("identify.usb.switch")}</h1><p class="sub">${t("start.identification.while.the.keyboard.and.mouse.are.connected.to.this.")}</p></div>
      <div class="callout"><strong>${model.usbConfirmed ? t("usb.switch.identified") : t("start.when.you.are.ready")}</strong></div>
      <div class="wizard-footer"><span></span><button class="primary" data-action="confirmUsb">${model.usbConfirmed ? t("continue") : t("start.identification")}</button></div>`,
    `<div class="wizard-head"><h1>${t("remember.this.computer.s.input.values")}</h1><p class="sub">${t("note.the.raw.input.value.for.each.monitor")}</p></div>
      <div class="value-list">${model.displays.map((display, i) => `<div class="value-row"><strong>${escapeHtml(display)}</strong><span class="source-value">${source(a.sources[i])}</span>
        <button class="copy" title="${t("copy")}" data-action="copyValue" data-value="${source(a.sources[i])}">${copyIcon}</button></div>`).join('')}</div>
      <div class="wizard-footer"><button class="secondary" data-action="previousStep">${t("back")}</button><button class="primary" data-action="nextStep">${t("i.have.noted.them")}</button></div>`,
    `<div class="wizard-head"><h1>${t("the.other.computer.s.input.values")}</h1><p class="sub">${t("enter.the.raw.values.read.from.the.monitors")}</p></div>
      <div class="choice-stack">
        <div class="choice ${choice === 'known' ? 'selected' : ''}" role="button" tabindex="0" data-action="selectChoice" data-value="known">
          <div class="choice-title"><span class="radio"></span>${t("i.know.the.values")}</div>
          ${choice === 'known' ? `<div class="choice-body"><div class="source-fields">
            <div class="source-field"><label for="target-name">${t("other.computer")}</label><input id="target-name" value="${escapeHtml(b.name)}" maxlength="32"></div>
            ${model.displays.map((display, i) => `<div class="source-field"><label for="target-${i}">${escapeHtml(display)} ${t("input.value")}</label>
              <input id="target-${i}" type="number" required min="0" max="255" value="${source(b.sources[i]) === t("not.set") ? '' : source(b.sources[i])}"></div>`).join('')}</div>
            <button class="secondary inline-link" data-action="openTargetSettings">${t("manage.saved.computers")}</button></div>` : ''}
        </div>
        <div class="choice ${choice === 'unknown' ? 'selected' : ''}" role="button" tabindex="0" data-action="selectChoice" data-value="unknown">
          <div class="choice-title"><span class="radio"></span>${t("i.do.not.know.yet")}</div>
          ${choice === 'unknown' ? `<div class="choice-body"><ol class="compact-steps">
            <li><span class="number">1</span><span>${t("use.the.monitor.menu.to.switch.to.the.other.computer")}</span></li>
            <li><span class="number">2</span><span>${t("press.usb.switch")}</span></li>
            <li><span class="number">3</span><span>${t("open.kvmflow.on.the.other.computer.and.note.its.input.values")}</span></li>
            <li><span class="number">4</span><span>${t("switch.back.here.and.enter.the.values")}</span></li></ol>
            <button class="secondary inline-link" data-action="setChoice" data-value="known">${t("i.have.checked")}</button></div>` : ''}
        </div></div>
      <div class="wizard-footer"><button class="secondary" data-action="previousStep">${t("back")}</button>
        <button class="primary" ${choice === 'known' ? '' : 'disabled'} data-action="saveTarget">${t("save.and.start.switching")}</button></div>`,
    `<div class="wizard-head"><h1>${t("press.usb.switch")}</h1><p class="sub">${t("press.the.button.to.send.input.switch.commands.to.the.monitors")}</p></div>
      <div id="test-status" class="test-state" aria-live="polite"><div class="test-status"><strong>${t("waiting.for.usb.switch")}</strong>
        <p id="test-status-copy" class="hint">${t("after.pressing.the.physical.button.check.the.actual.display")}</p></div></div>
      <div class="wizard-footer"><button class="secondary" data-action="previousStep">${t("back")}</button>
        <span id="test-action-hint" class="hint">${t("waiting.for.usb.switch")}</span></div>`,
  ][step];
}
function renderGuide() { renderSteps(); $('#guide-content').innerHTML = guideView(); }
window.confirmUsb = () => { step = 1; renderGuide(); };
window.nextStep = () => { step = Math.min(3, step + 1); renderGuide(); };
window.previousStep = () => { step = Math.max(0, step - 1); renderGuide(); };
window.setChoice = (value) => { if (choice !== value) { choice = value; renderGuide(); } };
window.selectChoice = (event, value) => {
  if (event.target.closest('input,button,select,textarea,label')) return;
  window.setChoice(value);
};
window.copyValue = async (value) => {
  try { await navigator.clipboard.writeText(value); log(t("input.value.copied"), value); }
  catch { log(t("note.the.input.value"), value); }
};
function inputValues(prefix) {
  const fields = model.displays.map((_, i) => $(`#${prefix}-${i}`));
  for (const field of fields) if (!field?.reportValidity()) throw new Error(t("enter.an.integer.input.value.from.0.to.255"));
  return fields.map(field => Number(field.value));
}
window.saveTarget = () => {
  const values = inputValues('target'), b = next();
  const name = $('#target-name').value.trim() || t("other.computer");
  if (name !== b.name) delete b.nameKey;
  b.name = name;
  b.sources = values;
  save(); step = 3; renderStatus(); renderGuide();
};
function renderComputers() {
  $('#computer-list').innerHTML = [...model.computers].sort((a, b) => a.port - b.port).map(c =>
    `<div class="computer-row ${c.id === editingId ? 'selected' : ''}"><button class="computer-select" data-action="editComputer" data-value="${escapeHtml(c.id)}" aria-pressed="${c.id === editingId}">
      <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 5.5h16v11H4zM9 19h6M12 16.5V19"/></svg>
      <span class="computer-copy"><span><span class="computer-name">${escapeHtml(c.name)}</span>${c.local ? `<span class="settings-tag">${t("this.computer")}</span>` : ''}</span><span class="hint">${t("port")} ${c.port}</span></span></button>
      ${c.local ? '' : `<button class="settings-text-button danger-button" data-action="deleteComputer" data-value="${escapeHtml(c.id)}">${t("delete")}</button>`}</div>`).join('');
  renderEditor(editingId);
}
function renderEditor(id) {
  const c = model.computers.find(item => item.id === id);
  if (!c) return;
  editingId = id;
  const automatic = c.local && model.displays.length > 0 && model.localInputSources?.every(value => value === 'learned_active_read');
  $('#computer-editor').innerHTML = `<div class="settings-editor-heading"><h3>${escapeHtml(c.name)}</h3>${c.local ? `<span class="settings-tag">${t("this.computer")}</span>` : ''}</div>
    <div class="form-grid"><div class="field"><label for="edit-name">${t("computer.name")}</label>
      <input id="edit-name" value="${escapeHtml(c.name)}" maxlength="32"></div>
      <div class="field"><label for="edit-port">${t("usb.switch.port")}</label><select id="edit-port">
      ${model.computers.map((_, i) => `<option ${i + 1 === Number(c.port) ? 'selected' : ''}>${i + 1}</option>`).join('')}</select></div></div>
      <div class="form-section"><div class="settings-editor-heading"><h3>${t("monitor.input.values")}</h3>${automatic ? `<span id="local-input-origin" class="settings-tag">${t("read.automatically")}</span>` : ''}</div>
      <div class="input-fields">${model.displays.map((display, i) => {
  const value = source(c.sources[i]) === t("not.set") ? '' : source(c.sources[i]);
  return `<div class="field settings-input-row"><label for="edit-${i}">${escapeHtml(display)}</label>
          <input id="edit-${i}" type="number" required min="0" max="255" step="1" data-confirmed-value="${escapeHtml(value)}" value="${escapeHtml(value)}"></div>`;
}).join('') || `<p class="settings-empty" role="status">${t("no.monitor.input.values.were.read")}</p>`}</div></div>`;
}
window.hasUnsavedComputerEditor = () => {
  const computer = model.computers.find(item => item.id === editingId);
  if (!computer) return false;
  const name = $('#edit-name');
  const port = $('#edit-port');
  const fields = [...document.querySelectorAll('#computer-editor .input-fields input')];
  if (!name || !port) return false;
  if ((name.value.trim() || computer.name) !== computer.name || Number(port.value) !== computer.port) return true;
  return fields.some((field, index) => {
    const current = source(computer.sources[index]);
    if (!field.validity.valid) return true;
    if (field.value === '') return current !== t("not.set");
    return current === t("not.set") || Number(field.value) !== Number(computer.sources[index]);
  });
};
window.editComputer = (id) => { editingId = id; renderComputers(); };

// A single modal guards both direct field edits and save/keyboard paths.
// It only accepts a draft value; writing hardware configuration still requires Save.
let localInputConfirmation = null;
window.confirmLocalInputs = async () => {
  if (localInputConfirmation) return localInputConfirmation;
  const computer = model.computers.find(item => item.id === editingId);
  if (!computer?.local) return true;
  const fields = [...document.querySelectorAll('#computer-editor .input-fields input')]
    .filter(field => field.value !== field.dataset.confirmedValue);
  if (!fields.length) return true;
  if (fields.some(field => !field.reportValidity())) return false;
  const dialog = $('#local-input-confirmation');
  const cancel = $('#cancel-local-input'), confirm = $('#confirm-local-input');
  const descriptions = fields.map(field => `${$(`label[for="${field.id}"]`).textContent}：${field.dataset.confirmedValue || t("not.set")} → ${field.value}`);
  $('#local-input-confirmation-copy').textContent = t("value.an.incorrect.value.may.prevent.the.monitor.from.switching.back.to.", {
  p0: descriptions.join('；')
});
  const draft = fields.map(field => ({ field, value: field.value, previous: field.dataset.confirmedValue }));
  const applyDraft = accepted => {
    if (editingId !== computer.id || draft.some(({ field }) => !field.isConnected)) return false;
    for (const { field, value, previous } of draft) {
      if (!field.isConnected) continue;
      field.value = accepted ? value : previous;
      if (accepted) field.dataset.confirmedValue = value;
    }
    if (accepted && $('#local-input-origin')) $('#local-input-origin').hidden = true;
    return accepted;
  };
  // Older macOS WebViews do not implement <dialog>. Use the same protected
  // confirmation through Tauri rather than silently accepting or blocking edits.
  if (typeof dialog.showModal !== 'function') {
    localInputConfirmation = (async () => {
      try {
        const accepted = await window.kvmflow.confirmLocalInput(descriptions.join('；'));
        return applyDraft(accepted === true);
      } catch (error) { applyDraft(false); throw error; }
    })();
    try { return await localInputConfirmation; }
    finally { localInputConfirmation = null; }
  }
  localInputConfirmation = new Promise((resolve, reject) => {
    const finish = (accepted) => {
      cancel.removeEventListener('click', onCancel);
      confirm.removeEventListener('click', onConfirm);
      dialog.removeEventListener('cancel', onEscape);
      dialog.removeEventListener('close', onClose);
      const applied = applyDraft(accepted);
      // Restore values before close() returns focus, so a second edit to the
      // same proposed value still emits the browser's native change event.
      if (dialog.open) dialog.close();
      resolve(applied);
    };
    const onCancel = () => finish(false);
    const onConfirm = () => finish(true);
    const onEscape = event => { event.preventDefault(); finish(false); };
    // Browsers queue close events. A previous dialog's event must not cancel
    // a modal that the user has already reopened.
    const onClose = () => { if (!dialog.open) finish(false); };
    cancel.addEventListener('click', onCancel);
    confirm.addEventListener('click', onConfirm);
    dialog.addEventListener('cancel', onEscape);
    dialog.addEventListener('close', onClose);
    try { dialog.showModal(); } catch (error) { finish(false); reject(error); }
  });
  try { return await localInputConfirmation; }
  finally { localInputConfirmation = null; }
};
$('#computer-editor').addEventListener('change', event => {
  if (!event.target.matches('.input-fields input')) return;
  void window.confirmLocalInputs().catch(error => log(t("action.incomplete"), error.message || t("please.try.again")));
});
window.openTargetSettings = () => { editingId = next().id; navigate('settings'); };
window.deleteComputer = async (id) => {
  const c = model.computers.find(item => item.id === id);
  if (!c || c.local || !(await window.kvmflow.confirmDelete(c.name))) return;
  model.computers = model.computers.filter(item => item.id !== id).sort((a, b) => a.port - b.port);
  model.computers.forEach((item, i) => { item.port = i + 1; });
  if (model.nextId === id) model.nextId = model.computers.find(item => !item.local)?.id || model.localId;
  editingId = model.localId;
  save(); log(t("computer.removed.changes.not.saved"), t("save.the.computer.settings.to.apply.this.change")); renderComputers(); renderStatus();
  markComputerSettingsStructureChanged();
};
function saveSettings() {
  const c = model.computers.find(item => item.id === editingId);
  if (!c) return;
  const values = inputValues('edit'), requestedPort = Number($('#edit-port').value);
  if (c.local) model.localInputSources = values.map((value, index) =>
    value === c.sources[index] ? (model.localInputSources?.[index] || 'unknown') : 'manual');
  const name = $('#edit-name').value.trim() || c.name;
  if (name !== c.name) delete c.nameKey;
  c.name = name; c.sources = values;
  const queue = model.computers.filter(item => item.id !== c.id).sort((a, b) => a.port - b.port);
  queue.splice(Math.max(0, Math.min(queue.length, requestedPort - 1)), 0, c);
  queue.forEach((item, i) => { item.port = i + 1; });
  model.computers = queue;
  const at = queue.findIndex(item => item.id === model.localId);
  model.nextId = queue[(at + 1) % queue.length].id;
  save(); renderComputers(); renderStatus();
}
function addComputer() {
  const id = 'pc-' + Date.now();
  model.computers.push({ id, name: t("new.computer"), nameKey: "new.computer", port: model.computers.length + 1, local: false, sources: model.displays.map(() => null) });
  editingId = id; save(); renderComputers();
  markComputerSettingsStructureChanged();
}
function renderDiagnostics() {
  $('#event-list').innerHTML = model.events.map(event =>
    `<div class="event"><div><strong>${escapeHtml(window.KVMFlowI18n.format(event.titleMessage ?? event.title))}</strong><p class="hint">${escapeHtml(window.KVMFlowI18n.format(event.copyMessage ?? event.copy))}</p></div>
      <span class="time">${escapeHtml(event.time)}</span></div>`).join('');
}
const pages = ['status', 'guide', 'settings', 'diagnostics'];
function navigate(page) {
  if (!pages.includes(page)) return;
  $('.shell').classList.toggle('settings-active', page === 'settings');
  pages.forEach(name => $(`#${name}-page`).classList.toggle('hidden', name !== page));
  document.querySelectorAll('.nav-item').forEach(item => item.classList.toggle('active', item.dataset.page === page));
  $('main').scrollTop = 0;
  if (page === 'status') renderStatus();
  if (page === 'guide') renderGuide();
  if (page === 'settings') renderComputers();
  if (page === 'diagnostics') renderDiagnostics();
}
const computerIcon = '<svg viewBox="0 0 48 48" aria-hidden="true"><rect x="8" y="9" width="32" height="23" rx="2.5"/><path d="M18 38h12M24 32v6M6 40h36"/></svg>';
$('#status-page .header').insertAdjacentHTML('afterend', `<section id="topology" class="topology" aria-label="${t("usb.switch.port.order")}"></section>`);
function renderTopology() {
  const current = local(), queue = [...model.computers].sort((a, b) => a.port - b.port);
  const at = Math.max(0, queue.findIndex(item => item.id === current.id));
  const target = queue[(at + 1) % queue.length] || current;
  const ready = model.usbConfirmed && queue.length > 1;
  $('#topology').innerHTML = `
    ${ready ? '' : `<div class="topology-head"><div><h2>${t("setup.incomplete")}</h2><p>${t("record.each.computer.s.monitor.input.values.to.get.started")}</p></div><button class="primary" data-action="beginSetup">${t("start.setup")}</button></div>`}
    <div class="switch-board"><div class="switch-board-scroll"><div class="switch-board-inner" style="--port-count:${queue.length}">
      <div class="switch-chassis"><div class="switch-title"><strong>USB Switch</strong><span>${queue.length} ${t("ports")}</span></div>
      <div class="switch-ports">${queue.map(item => `<div class="switch-port ${item.id === current.id ? 'current' : item.id === target.id ? 'next' : 'waiting'}">
        <strong>${item.port}</strong><span class="usb-slot" aria-hidden="true"></span></div>`).join('')}</div></div>
      <div class="port-grid">${queue.map(item => `<div class="port-card ${item.id === current.id ? 'current' : item.id === target.id ? 'next' : 'waiting'}">
        <span class="port-index">${t("port")} ${item.port}</span><span class="computer-icon">${computerIcon}</span><div class="node-name">${escapeHtml(item.name)}</div>
        <em class="port-state">${item.id === current.id ? t("this.computer") : item.id === target.id ? t("next.computer") : t("waiting.to.switch")}</em></div>`).join('')}</div>
    </div></div><div class="switch-summary"><span class="summary-mark">i</span>
      <span>${t("this.computer.is.on.port")} ${current.port}${queue.length > 1 ? t("the.next.computer.is.on.port.value", {
  p0: target.port
}) : t("add.another.computer")}</span></div></div>`;
}
const actions = new Set(['confirmUsb', 'previousStep', 'nextStep', 'copyValue', 'selectChoice', 'setChoice', 'saveTarget', 'openTargetSettings', 'editComputer', 'deleteComputer', 'retryDisplayDetection']);
function dispatchAction(event) {
  const control = event.target.closest('[data-action]');
  if (!control || control.disabled) return;
  const action = control.dataset.action;
  if (action === 'beginSetup') { step = 0; navigate('guide'); return; }
  if (!actions.has(action)) return;
  const result = action === 'selectChoice'
    ? window.selectChoice(event, control.dataset.value)
    : window[action](control.dataset.value);
  Promise.resolve(result).catch(error => log(t("action.incomplete"), error.message || t("please.try.again")));
}
document.addEventListener('click', dispatchAction);
document.addEventListener('keydown', event => {
  if (event.target.matches('[role="button"][data-action]') && ['Enter', ' '].includes(event.key)) {
    event.preventDefault(); dispatchAction(event);
  }
});
document.querySelectorAll('[data-page]').forEach(control => control.addEventListener('click', () => navigate(control.dataset.page)));
$('#save-settings').addEventListener('click', () => { void window.saveSettings(); });
$('#add-computer').addEventListener('click', addComputer);
navState(); renderStatus(); renderDiagnostics();
