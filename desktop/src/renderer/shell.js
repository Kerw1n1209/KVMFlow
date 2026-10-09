// Local UI model. The persisted Rust configuration is authoritative; this
// cache stores navigation and recent UI events, never hardware evidence.
const key = 'kvmflow-local-prototype-v2';
const windowsHost = /Win/.test(navigator.platform);
const defaults = {
  localId: windowsHost ? 'win' : 'mac', nextId: windowsHost ? 'mac' : 'win',
  usbConfirmed: false,
  computers: [
    { id: windowsHost ? 'win' : 'mac', name: '本机', port: 1, local: true, sources: [] },
    { id: windowsHost ? 'mac' : 'win', name: '另一台电脑', port: 2, local: false, sources: [] },
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
const source = (value) => value !== '' && value != null && Number.isFinite(Number(value)) ? String(Number(value)) : '未填写';
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
  model.events.unshift({ title, copy, time: new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) });
  model.events = model.events.slice(0, 8);
  save(); renderDiagnostics();
}

function navState() {
  const state = window.__kvmflowRuntimeState;
  const label = state === 'error' ? '后台组件不可用'
    : !model.usbConfirmed ? '需要初始化'
    : state === 'learning' ? '正在识别 USB Switch'
    : state === 'pushing' ? '正在发送切换指令'
    : state === 'disabled' ? '自动切换已暂停' : '等待 USB Switch';
  $('#nav-dot').className = `dot${model.usbConfirmed && state !== 'error' ? ' good' : ''}`;
  $('#nav-state').textContent = label;
}

function renderStatus() {
  const a = local(), b = next();
  $('#home-computer').textContent = a.name;
  $('#home-port').textContent = `端口 ${a.port}`;
  $('#home-next').textContent = `${b.name} · 端口 ${b.port}`;
  $('#mapping-state').textContent = model.usbConfirmed ? '已记录输入映射' : '未完成';
  $('#display-table').innerHTML = model.displays.map((name, i) =>
    `<tr><td><strong>${escapeHtml(name)}</strong></td><td>${source(a.sources[i])}</td><td>${source(b.sources[i])}</td></tr>`).join('');
  const report = window.__kvmflowLastSwitchReport;
  const outcomes = report?.per_monitor || [];
  const accepted = outcomes.length && outcomes.every(entry => entry.commanded === true || entry.ok === true);
  $('#last-title').textContent = report ? (accepted ? '指令已接受' : '切换未完成') : '等待 USB Switch';
  $('#last-copy').textContent = report ? '请以显示器实际画面为准。无画面时打开诊断与恢复。' : '按 USB Switch 后会显示最近一次指令结果。';
  renderTopology();
}

const labels = ['确认 USB Switch', '记录这台电脑', '填写另一台电脑', '开始切换'];
function renderSteps() {
  $('#steps').innerHTML = labels.map((label, i) =>
    `<div class="step ${i === step ? 'active' : ''} ${i < step ? 'done' : ''}">${i + 1} ${label}</div>`).join('');
  $('#guide-count').textContent = `第 ${step + 1} 步，共 4 步`;
}
const copyIcon = '<svg viewBox="0 0 24 24"><rect x="9" y="9" width="10" height="10" rx="1"/><path d="M15 9V5H5v10h4"/></svg>';
function guideView() {
  const a = local(), b = next();
  return [
    `<div class="wizard-head"><h1>确认 USB Switch</h1><p class="sub">键鼠在这台电脑时，开始识别。</p></div>
      <div class="callout"><strong>${model.usbConfirmed ? '已识别 USB Switch' : '准备好后开始识别'}</strong></div>
      <div class="wizard-footer"><span></span><button class="primary" data-action="confirmUsb">${model.usbConfirmed ? '继续' : '开始识别'}</button></div>`,
    `<div class="wizard-head"><h1>记住这台电脑的输入值</h1><p class="sub">请记住每台显示器对应的原始数值。</p></div>
      <div class="value-list">${model.displays.map((display, i) =>
        `<div class="value-row"><strong>${escapeHtml(display)}</strong><span class="source-value">${source(a.sources[i])}</span>
        <button class="copy" title="复制" data-action="copyValue" data-value="${source(a.sources[i])}">${copyIcon}</button></div>`).join('')}</div>
      <div class="wizard-footer"><button class="secondary" data-action="previousStep">上一步</button><button class="primary" data-action="nextStep">我记住了</button></div>`,
    `<div class="wizard-head"><h1>另一台电脑的输入值</h1><p class="sub">填写显示器读到的原始数值。</p></div>
      <div class="choice-stack">
        <div class="choice ${choice === 'known' ? 'selected' : ''}" role="button" tabindex="0" data-action="selectChoice" data-value="known">
          <div class="choice-title"><span class="radio"></span>我已经知道</div>
          ${choice === 'known' ? `<div class="choice-body"><div class="source-fields">
            <div class="source-field"><label for="target-name">另一台电脑</label><input id="target-name" value="${escapeHtml(b.name)}" maxlength="32"></div>
            ${model.displays.map((display, i) => `<div class="source-field"><label for="target-${i}">${escapeHtml(display)} 输入值</label>
              <input id="target-${i}" type="number" required min="0" max="255" value="${source(b.sources[i]) === '未填写' ? '' : source(b.sources[i])}"></div>`).join('')}</div>
            <button class="secondary inline-link" data-action="openTargetSettings">管理已记录的电脑</button></div>` : ''}
        </div>
        <div class="choice ${choice === 'unknown' ? 'selected' : ''}" role="button" tabindex="0" data-action="selectChoice" data-value="unknown">
          <div class="choice-title"><span class="radio"></span>我还不知道</div>
          ${choice === 'unknown' ? `<div class="choice-body"><ol class="compact-steps">
            <li><span class="number">1</span><span>用显示器菜单切到另一台电脑</span></li>
            <li><span class="number">2</span><span>按 USB Switch</span></li>
            <li><span class="number">3</span><span>在另一台电脑打开 KVMFlow，记住它的输入值</span></li>
            <li><span class="number">4</span><span>切回这里填写</span></li></ol>
            <button class="secondary inline-link" data-action="setChoice" data-value="known">我已查看</button></div>` : ''}
        </div></div>
      <div class="wizard-footer"><button class="secondary" data-action="previousStep">上一步</button>
        <button class="primary" ${choice === 'known' ? '' : 'disabled'} data-action="saveTarget">保存并开始切换</button></div>`,
    `<div class="wizard-head"><h1>按 USB Switch</h1><p class="sub">按下后将向显示器发送切换指令。</p></div>
      <div id="test-status" class="test-state" aria-live="polite"><div class="test-status"><strong>等待 USB Switch</strong>
        <p id="test-status-copy" class="hint">按下实体按钮后，请确认实际画面。</p></div></div>
      <div class="wizard-footer"><button class="secondary" data-action="previousStep">上一步</button>
        <span id="test-action-hint" class="hint">等待 USB Switch</span></div>`,
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
  try { await navigator.clipboard.writeText(value); log('已复制输入值', value); }
  catch { log('请记住输入值', value); }
};
function inputValues(prefix) {
  const fields = model.displays.map((_, i) => $(`#${prefix}-${i}`));
  for (const field of fields) if (!field?.reportValidity()) throw new Error('请填写 0 到 255 之间的整数输入值。');
  return fields.map(field => Number(field.value));
}
window.saveTarget = () => {
  const values = inputValues('target'), b = next();
  b.name = $('#target-name').value.trim() || '另一台电脑';
  b.sources = values;
  save(); step = 3; renderStatus(); renderGuide();
};
function renderComputers() {
  $('#computer-list').innerHTML = [...model.computers].sort((a, b) => a.port - b.port).map(c =>
    `<div class="computer-row ${c.id === editingId ? 'selected' : ''}"><button class="computer-select" data-action="editComputer" data-value="${escapeHtml(c.id)}" aria-pressed="${c.id === editingId}">
      <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 5.5h16v11H4zM9 19h6M12 16.5V19"/></svg>
      <span class="computer-copy"><span><span class="computer-name">${escapeHtml(c.name)}</span>${c.local ? '<span class="settings-tag">本机</span>' : ''}</span><span class="hint">端口 ${c.port}</span></span></button>
      ${c.local ? '' : `<button class="settings-text-button danger-button" data-action="deleteComputer" data-value="${escapeHtml(c.id)}">删除</button>`}</div>`).join('');
  renderEditor(editingId);
}
function renderEditor(id) {
  const c = model.computers.find(item => item.id === id);
  if (!c) return;
  editingId = id;
  const automatic = c.local && model.displays.length > 0 && model.localInputSources?.every(value => value === 'learned_active_read');
  $('#computer-editor').innerHTML = `<div class="settings-editor-heading"><h3>${escapeHtml(c.name)}</h3>${c.local ? '<span class="settings-tag">本机</span>' : ''}</div>
    <div class="form-grid"><div class="field"><label for="edit-name">电脑名称</label>
      <input id="edit-name" value="${escapeHtml(c.name)}" maxlength="32"></div>
      <div class="field"><label for="edit-port">USB Switch 端口</label><select id="edit-port">
      ${model.computers.map((_, i) => `<option ${i + 1 === Number(c.port) ? 'selected' : ''}>${i + 1}</option>`).join('')}</select></div></div>
      <div class="form-section"><div class="settings-editor-heading"><h3>显示器输入值</h3>${automatic ? '<span id="local-input-origin" class="settings-tag">自动读取</span>' : ''}</div>
      <div class="input-fields">${model.displays.map((display, i) => {
        const value = source(c.sources[i]) === '未填写' ? '' : source(c.sources[i]);
        return `<div class="field settings-input-row"><label for="edit-${i}">${escapeHtml(display)}</label>
          <input id="edit-${i}" type="number" required min="0" max="255" step="1" data-confirmed-value="${escapeHtml(value)}" value="${escapeHtml(value)}"></div>`;
      }).join('') || '<p class="settings-empty" role="status">未读取到显示器输入值</p>'}</div></div>`;
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
    if (field.value === '') return current !== '未填写';
    return current === '未填写' || Number(field.value) !== Number(computer.sources[index]);
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
  const descriptions = fields.map(field => `${$(`label[for="${field.id}"]`).textContent}：${field.dataset.confirmedValue || '未填写'} → ${field.value}`);
  $('#local-input-confirmation-copy').textContent = `${descriptions.join('；')}。修改错误可能导致显示器无法切回本机。`;
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
  void window.confirmLocalInputs().catch(error => log('操作未完成', error.message || '请重试。'));
});
window.openTargetSettings = () => { editingId = next().id; navigate('settings'); };
window.deleteComputer = async (id) => {
  const c = model.computers.find(item => item.id === id);
  if (!c || c.local || !(await window.kvmflow.confirmDelete(c.name))) return;
  model.computers = model.computers.filter(item => item.id !== id).sort((a, b) => a.port - b.port);
  model.computers.forEach((item, i) => { item.port = i + 1; });
  if (model.nextId === id) model.nextId = model.computers.find(item => !item.local)?.id || model.localId;
  editingId = model.localId;
  save(); log('已移除电脑，尚未保存', '点击保存设置后生效。'); renderComputers(); renderStatus();
  markComputerSettingsStructureChanged();
};
function saveSettings() {
  const c = model.computers.find(item => item.id === editingId);
  if (!c) return;
  const values = inputValues('edit'), requestedPort = Number($('#edit-port').value);
  if (c.local) model.localInputSources = values.map((value, index) =>
    value === c.sources[index] ? (model.localInputSources?.[index] || 'unknown') : 'manual');
  c.name = $('#edit-name').value.trim() || c.name; c.sources = values;
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
  model.computers.push({ id, name: '新电脑', port: model.computers.length + 1, local: false, sources: model.displays.map(() => null) });
  editingId = id; save(); renderComputers();
  markComputerSettingsStructureChanged();
}
function renderDiagnostics() {
  $('#event-list').innerHTML = model.events.map(event =>
    `<div class="event"><div><strong>${escapeHtml(event.title)}</strong><p class="hint">${escapeHtml(event.copy)}</p></div>
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
$('#status-page .header').insertAdjacentHTML('afterend', '<section id="topology" class="topology" aria-label="USB Switch 端口顺序"></section>');
function renderTopology() {
  const current = local(), queue = [...model.computers].sort((a, b) => a.port - b.port);
  const at = Math.max(0, queue.findIndex(item => item.id === current.id));
  const target = queue[(at + 1) % queue.length] || current;
  const ready = model.usbConfirmed && queue.length > 1;
  $('#topology').innerHTML = `
    ${ready ? '' : '<div class="topology-head"><div><h2>尚未完成初始化</h2><p>记录每台电脑的显示器输入值后即可开始使用。</p></div><button class="primary" data-action="beginSetup">开始初始化</button></div>'}
    <div class="switch-board"><div class="switch-board-scroll"><div class="switch-board-inner" style="--port-count:${queue.length}">
      <div class="switch-chassis"><div class="switch-title"><strong>USB Switch</strong><span>${queue.length} 个端口</span></div>
      <div class="switch-ports">${queue.map(item => `<div class="switch-port ${item.id === current.id ? 'current' : item.id === target.id ? 'next' : 'waiting'}">
        <strong>${item.port}</strong><span class="usb-slot" aria-hidden="true"></span></div>`).join('')}</div></div>
      <div class="port-grid">${queue.map(item => `<div class="port-card ${item.id === current.id ? 'current' : item.id === target.id ? 'next' : 'waiting'}">
        <span class="port-index">端口 ${item.port}</span><span class="computer-icon">${computerIcon}</span><div class="node-name">${escapeHtml(item.name)}</div>
        <em class="port-state">${item.id === current.id ? '本机' : item.id === target.id ? '下一台' : '等待切换'}</em></div>`).join('')}</div>
    </div></div><div class="switch-summary"><span class="summary-mark">i</span>
      <span>本机位于端口 ${current.port}${queue.length > 1 ? `，下一台为端口 ${target.port}。` : '，请添加另一台电脑。'}</span></div></div>`;
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
  Promise.resolve(result).catch(error => log('操作未完成', error.message || '请重试。'));
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
