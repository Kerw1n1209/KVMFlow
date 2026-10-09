const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');
const { JSDOM } = require('jsdom');
const root = path.resolve(__dirname, '..');
const read = relative => fs.readFileSync(path.join(root, relative), 'utf8');

const v2 = {
  schema_version: 2,
  local_device: { device_id: 'host-win', host_label: 'Windows', monitors: [
    { fingerprint: 'display-b', label: '副屏', local_input: 8 },
    { fingerprint: 'display-a', label: '主屏', local_input: 16 },
  ] },
  switch_group: { group_id: 'preserved-group', revision: 4, devices: [
    { device_id: 'host-mac', name: 'Mac', port_index: 1, monitors: [
      { fingerprint: 'display-a', local_input: 15 }, { fingerprint: 'display-b', local_input: 7 },
    ] },
    { device_id: 'host-win', name: 'Windows', port_index: 2, monitors: [
      { fingerprint: 'display-b', local_input: 8 }, { fingerprint: 'display-a', local_input: 16 },
    ] },
    { device_id: 'host-linux', name: '第三台', port_index: 3, monitors: [
      { fingerprint: 'display-a', local_input: 17 }, { fingerprint: 'display-b', local_input: 9 },
    ] },
  ] },
  trigger: { anchor: { vid_pid: '1a40:0101' }, members: [{ vid_pid: '3837:303c' }], debounce: {
    t_stable_ms: 0, t_absent_ms: 0, t_cooldown_ms: 1000, quorum_peripherals: 1,
  } },
  advanced: { ddc_retry: { attempts: 3, delay_ms: 200 }, usb_poll_ms: 300, arrival_correction_enabled: false },
};

async function client(t, options = {}) {
  const { config = v2, failSave = false, displays = [], storage = {} } = options;
  const computerName = Object.hasOwn(options, 'computerName') ? options.computerName : config?.local_device?.host_label;
  const dom = new JSDOM(read('src/renderer/index.html'), { url: 'http://tauri.localhost', runScripts: 'outside-only' });
  t.after(() => dom.window.close());
  const win = dom.window, requests = [], subscriptions = new Map();
  for (const [key, value] of Object.entries(storage)) win.localStorage.setItem(key, value);
  let exported = null;
  win.structuredClone = structuredClone;
  // jsdom does not implement modal dialogs; keep their open/close lifecycle real.
  win.HTMLDialogElement.prototype.showModal = function () { this.setAttribute('open', ''); };
  win.HTMLDialogElement.prototype.close = function () { this.removeAttribute('open'); this.dispatchEvent(new win.Event('close')); };
  win.kvmflow = {
    ready: Promise.resolve(), info: Promise.resolve({ platform: 'windows', version: '0.2.0', computerName, initialPage: config ? 'status' : 'wizard' }),
    clientReady: async () => {},
    request: async (method, params) => {
      requests.push({ method, params });
      if (method === 'config.get') return structuredClone(config);
      if (method === 'state.get') return { state: 'idle', enabled: true };
      if (method === 'display.list') return { displays };
      if (method === 'config.set' && failSave) throw new Error('保存失败');
      if (method === 'diagnostics.collect') return { events: [], backend: 'test' };
      return {};
    },
    onNotification: (kind, cb) => { subscriptions.set(kind, cb); return () => subscriptions.delete(kind); },
    onClientNavigate: () => () => {},
    startup: { get: async () => false, set: async enabled => enabled },
    confirmDelete: async () => true,
    exportDiagnostics: async payload => { exported = payload; return true; },
  };
  vm.runInContext(read('src/renderer/shell.js'), dom.getInternalVMContext());
  vm.runInContext(read('src/renderer/app.js'), dom.getInternalVMContext());
  await new Promise(resolve => setTimeout(resolve, 10));
  return { win, requests, subscriptions, export: () => exported };
}

const tick = () => new Promise(resolve => setTimeout(resolve, 10));
const automaticConfig = () => {
  const config = structuredClone(v2);
  config.local_device.monitors.forEach(monitor => { monitor.source = 'learned_active_read'; });
  return config;
};

test('settings save belongs to computer configuration, with startup separate from timing', async t => {
  const { win } = await client(t);
  win.navigate('settings');
  assert.ok(win.document.querySelector('.computer-settings #save-settings'));
  assert.equal(win.document.querySelector('#settings-page > .header #save-settings'), null);
  assert.ok(win.document.querySelector('.startup-panel #startup-at-login'));
  assert.equal(win.document.querySelector('.response-panel #startup-at-login'), null);
  assert.equal(win.document.querySelectorAll('.response-panel .response-copy span').length, 4); // three short hints and hidden host mode
});

test('only the local computer can show the automatically read input tag', async t => {
  const { win, requests } = await client(t, { config: automaticConfig() });
  win.navigate('settings');
  assert.equal(win.document.querySelector('#local-input-origin').textContent, '自动读取');
  assert.equal(win.document.querySelector('#edit-0').value, '8');
  win.document.querySelector('[data-action="editComputer"][data-value="host-mac"]').click();
  assert.equal(win.document.querySelector('#local-input-origin'), null);
  assert.equal(win.document.querySelector('#edit-0').value, '7');
  assert.equal(requests.some(entry => entry.method === 'display.list'), false);
});

test('cancelling a local input edit restores only that value and does not save', async t => {
  const { win, requests } = await client(t, { config: automaticConfig() });
  win.navigate('settings');
  win.document.querySelector('#edit-name').value = '未保存的名称';
  const input = win.document.querySelector('#edit-0');
  input.value = '15';
  input.dispatchEvent(new win.Event('change', { bubbles: true }));
  assert.equal(win.document.querySelector('#local-input-confirmation').open, true);
  assert.match(win.document.querySelector('#local-input-confirmation-copy').textContent, /8 → 15/);
  win.document.querySelector('#cancel-local-input').click();
  await tick();
  assert.equal(input.value, '8');
  assert.equal(win.document.querySelector('#edit-name').value, '未保存的名称');
  assert.equal(win.document.querySelector('#local-input-origin').hidden, false);
  assert.equal(requests.some(entry => entry.method === 'config.set'), false);
});

test('cancelling the only local input edit leaves no unsaved computer settings', async t => {
  const { win } = await client(t, { config: automaticConfig() });
  win.navigate('settings');
  const input = win.document.querySelector('#edit-0');
  input.value = '15';
  input.dispatchEvent(new win.Event('change', { bubbles: true }));
  assert.equal(win.hasUnsavedComputerEditor(), true);
  win.document.querySelector('#cancel-local-input').click();
  await tick();
  assert.equal(win.hasUnsavedComputerEditor(), false);
});

test('confirmed local overrides save once as manual and survive reopening', async t => {
  const { win, requests } = await client(t, { config: automaticConfig() });
  win.navigate('settings');
  const input = win.document.querySelector('#edit-0');
  input.value = '15';
  input.dispatchEvent(new win.Event('change', { bubbles: true }));
  win.document.querySelector('#confirm-local-input').click();
  await tick();
  assert.equal(requests.some(entry => entry.method === 'config.set'), false);
  await win.saveSettings();
  assert.equal(win.document.querySelector('#local-input-confirmation').open, false);
  const saved = requests.find(entry => entry.method === 'config.set').params.config;
  assert.equal(saved.local_device.monitors[0].local_input, 15);
  assert.equal(saved.local_device.monitors[0].source, 'manual');
  const reopened = await client(t, { config: saved });
  reopened.win.navigate('settings');
  assert.equal(reopened.win.document.querySelector('#edit-0').value, '15');
  assert.equal(reopened.win.document.querySelector('#local-input-origin'), null);
});

test('a failed manual override save restores the previous value and automatic source', async t => {
  const { win } = await client(t, { config: automaticConfig(), failSave: true });
  win.navigate('settings');
  win.document.querySelector('#edit-0').value = '15';
  const save = win.saveSettings();
  win.document.querySelector('#confirm-local-input').click();
  await save;
  assert.equal(win.document.querySelector('#edit-0').value, '8');
  assert.equal(win.document.querySelector('#local-input-origin').textContent, '自动读取');
  assert.match(win.document.querySelector('#event-list').textContent, /本地配置未保存/);
});

test('invalid local input values neither open confirmation nor write configuration', async t => {
  const { win, requests } = await client(t);
  win.navigate('settings');
  for (const value of ['', '-1', '256', '1.5']) {
    win.document.querySelector('#edit-0').value = value;
    await win.saveSettings();
    assert.equal(win.document.querySelector('#local-input-confirmation').open, false);
  }
  assert.equal(requests.some(entry => entry.method === 'config.set'), false);
});

test('save cannot bypass local input confirmation, and Escape cancels', async t => {
  const { win, requests } = await client(t);
  win.navigate('settings');
  win.document.querySelector('#edit-0').value = '17';
  const save = win.saveSettings();
  const dialog = win.document.querySelector('#local-input-confirmation');
  assert.equal(dialog.open, true);
  dialog.dispatchEvent(new win.Event('cancel', { cancelable: true }));
  await save;
  assert.equal(win.document.querySelector('#edit-0').value, '8');
  assert.equal(win.document.querySelector('#save-settings').disabled, false);
  assert.equal(requests.some(entry => entry.method === 'config.set'), false);
});

test('a stale close event cannot cancel a reopened local-input warning', async t => {
  const { win } = await client(t);
  win.navigate('settings');
  const input = win.document.querySelector('#edit-0');
  const dialog = win.document.querySelector('#local-input-confirmation');
  input.value = '17';
  const first = win.confirmLocalInputs();
  win.document.querySelector('#cancel-local-input').click();
  await first;
  input.value = '17';
  const second = win.confirmLocalInputs();
  dialog.dispatchEvent(new win.Event('close'));
  assert.equal(dialog.open, true);
  assert.equal(input.value, '17');
  win.document.querySelector('#confirm-local-input').click();
  assert.equal(await second, true);
});

test('older WebViews use native confirmation, restoring edits on cancel', async t => {
  const { win, requests } = await client(t);
  win.HTMLDialogElement.prototype.showModal = undefined;
  win.navigate('settings');
  win.document.querySelector('#edit-0').value = '17';
  let warning;
  win.kvmflow.confirmLocalInput = async changes => { warning = changes; return false; };
  await win.saveSettings();
  assert.match(warning, /8 → 17/);
  assert.equal(win.document.querySelector('#edit-0').value, '8');
  assert.equal(requests.some(entry => entry.method === 'config.set'), false);
});

test('remote input edits need no local warning and save the remote profile only', async t => {
  const { win, requests } = await client(t);
  win.navigate('settings');
  win.editComputer('host-mac');
  win.document.querySelector('#edit-0').value = '10';
  win.document.querySelector('#edit-0').dispatchEvent(new win.Event('change', { bubbles: true }));
  await win.saveSettings();
  assert.equal(win.document.querySelector('#local-input-confirmation').open, false);
  const saved = requests.find(entry => entry.method === 'config.set').params.config;
  assert.equal(saved.local_device.monitors[0].local_input, 8);
  assert.equal(saved.switch_group.devices.find(device => device.device_id === 'host-mac').monitors[0].local_input, 10);
});

test('unconfigured settings read real local monitor values without arming switching', async t => {
  const { win, requests } = await client(t, { config: null, displays: [
    { fingerprint: 'detected', label: '真实显示器', ddc: { state: 'available', input: 15 } },
    { builtin: true, label: '内置屏', ddc: { state: 'available', input: 1 } },
    { label: '不可读', ddc: { state: 'unavailable' } },
  ] });
  assert.equal(requests.some(entry => entry.method === 'display.list'), false);
  win.navigate('settings');
  win.document.querySelector('#edit-name').value = '尚未保存的名称';
  await tick();
  assert.equal(win.document.querySelector('#edit-0').value, '15');
  assert.equal(win.document.querySelector('#local-input-origin').textContent, '自动读取');
  assert.equal(win.document.querySelector('#edit-name').value, '尚未保存的名称');
  assert.equal(requests.some(entry => entry.method === 'usb.watch.start' || entry.method === 'config.set'), false);
  win.editComputer('win');
  assert.equal(win.document.querySelector('#local-input-origin'), null);
  assert.equal(win.document.querySelector('#edit-0').value, '');
});

test('upgrade reconstructs all computers and matches monitors by identity', async t => {
  const { win, requests } = await client(t);
  win.navigate('settings');
  assert.equal(win.document.querySelectorAll('.computer-row').length, 3);
  assert.equal(win.document.querySelector('#edit-name').value, 'Windows');
  await win.saveSettings();
  const config = requests.find(entry => entry.method === 'config.set').params.config;
  assert.equal(config.local_device.device_id, 'host-win');
  assert.equal(config.switch_group.group_id, 'preserved-group');
  assert.equal(config.switch_group.revision, 5);
  assert.equal(config.switch_group.devices.length, 3);
  assert.equal(config.switch_group.devices[0].monitors[0].local_input, 7);
  assert.equal(config.switch_group.devices[2].monitors[0].local_input, 9);
  assert.equal(config.advanced.usb_poll_ms, 300);
});

test('failed saves restore the UI and never claim saved settings', async t => {
  const { win } = await client(t, { failSave: true });
  win.navigate('settings');
  win.document.querySelector('#edit-name').value = 'new-name';
  await win.saveSettings();
  assert.equal(win.document.querySelector('#edit-name').value, 'Windows');
  assert.match(win.document.querySelector('#event-list').textContent, /本地配置未保存/);
  assert.doesNotMatch(win.document.querySelector('#event-list').textContent, /已保存设置/);
});

test('computer names save before hardware setup and survive reopening without arming hardware', async t => {
  const { win, requests } = await client(t, { config: null, computerName: '系统名称' });
  win.navigate('settings');
  win.document.querySelector('#edit-name').value = '我的工作电脑';
  await win.saveSettings();
  assert.equal(win.document.querySelector('#edit-name').value, '我的工作电脑');
  assert.equal(requests.some(entry => entry.method === 'config.set' || entry.method === 'usb.watch.start'), false);
  assert.match(win.document.querySelector('#event-list').textContent, /电脑配置组已保存/);
  const storage = { 'kvmflow-computer-group-draft-v1': win.localStorage.getItem('kvmflow-computer-group-draft-v1') };
  const reopened = await client(t, { config: null, computerName: '另一个系统名称', storage });
  reopened.win.navigate('settings');
  assert.equal(reopened.win.document.querySelector('#edit-name').value, '另一个系统名称');
  assert.equal(reopened.win.document.querySelectorAll('.computer-row').length, 2);
  assert.match(reopened.win.document.querySelector('#nav-state').textContent, /需要初始化/);
});

test('a saved draft with only the local computer reopens safely', async t => {
  const { win } = await client(t, { config: null });
  win.navigate('settings');
  const remote = win.document.querySelector('.danger-button').dataset.value;
  await win.deleteComputer(remote);
  await win.saveSettings();
  const reopened = await client(t, { config: null, storage: {
    'kvmflow-computer-group-draft-v1': win.localStorage.getItem('kvmflow-computer-group-draft-v1'),
  } });
  reopened.win.navigate('settings');
  assert.equal(reopened.win.document.querySelectorAll('.computer-row').length, 1);
  assert.doesNotMatch(reopened.win.document.querySelector('#event-list').textContent, /无法读取本机配置/);
});

test('saving a configured computer group changes the name but preserves timing and advanced settings', async t => {
  const { win, requests } = await client(t);
  win.navigate('settings');
  win.document.querySelector('#edit-name').value = '新电脑名称';
  // Merely editing another control must not broaden the save button's scope.
  win.document.querySelector('#timing-absent').value = '3000';
  win.document.querySelector('#arrival-correction').checked = true;
  await win.saveSettings();
  const saved = requests.find(entry => entry.method === 'config.set').params.config;
  assert.equal(saved.local_device.host_label, '新电脑名称');
  assert.equal(saved.switch_group.devices.find(c => c.device_id === 'host-win').name, '新电脑名称');
  assert.deepEqual(saved.trigger, v2.trigger);
  assert.deepEqual(saved.advanced, v2.advanced);
  assert.equal(requests.some(entry => entry.method === 'usb.watch.start'), false);
  assert.equal(win.document.querySelector('#edit-name').value, '新电脑名称');
  const reopened = await client(t, { config: saved });
  reopened.win.navigate('settings');
  assert.equal(reopened.win.document.querySelector('#edit-name').value, '新电脑名称');
});

test('switch timing saves separately without committing or resetting an edited computer name', async t => {
  const { win, requests } = await client(t);
  win.navigate('settings');
  win.document.querySelector('#edit-name').value = '尚未保存的名称';
  const select = win.document.querySelector('#timing-absent');
  select.value = '3000';
  select.dispatchEvent(new win.Event('change', { bubbles: true }));
  await new Promise(resolve => setTimeout(resolve, 10));
  const saved = requests.find(entry => entry.method === 'config.set').params.config;
  assert.equal(saved.trigger.debounce.t_absent_ms, 3000);
  assert.deepEqual(saved.switch_group, v2.switch_group);
  assert.deepEqual(saved.local_device, v2.local_device);
  assert.equal(win.document.querySelector('#edit-name').value, '尚未保存的名称');
  await win.saveSettings();
  const groupSave = requests.filter(entry => entry.method === 'config.set').at(-1).params.config;
  assert.equal(groupSave.local_device.host_label, '尚未保存的名称');
  assert.equal(groupSave.trigger.debounce.t_absent_ms, 3000);
});

test('failed timing saves restore just the timing control and retain the computer editor', async t => {
  const { win } = await client(t, { failSave: true });
  win.navigate('settings');
  win.document.querySelector('#edit-name').value = '尚未保存的名称';
  const select = win.document.querySelector('#timing-absent');
  select.value = '3000';
  select.dispatchEvent(new win.Event('change', { bubbles: true }));
  await new Promise(resolve => setTimeout(resolve, 10));
  assert.equal(select.value, '0');
  assert.equal(select.disabled, false);
  assert.equal(win.document.querySelector('#edit-name').value, '尚未保存的名称');
  assert.match(win.document.querySelector('#event-list').textContent, /切换响应未保存/);
});

test('overlapping timing and group saves preserve both changes', async t => {
  const { win, requests } = await client(t);
  win.navigate('settings');
  const select = win.document.querySelector('#timing-absent');
  select.value = '3000';
  select.dispatchEvent(new win.Event('change', { bubbles: true }));
  win.document.querySelector('#edit-name').value = '同时保存的名称';
  await win.saveSettings();
  const writes = requests.filter(entry => entry.method === 'config.set');
  assert.equal(writes.length, 2);
  assert.equal(writes[1].params.config.trigger.debounce.t_absent_ms, 3000);
  assert.equal(writes[1].params.config.local_device.host_label, '同时保存的名称');
});

test('timing preferences can be saved independently before hardware setup', async t => {
  const { win, requests } = await client(t, { config: null });
  win.navigate('settings');
  const select = win.document.querySelector('#timing-absent');
  select.value = '3000';
  select.dispatchEvent(new win.Event('change', { bubbles: true }));
  await new Promise(resolve => setTimeout(resolve, 10));
  assert.equal(requests.some(entry => entry.method === 'config.set'), false);
  const reopened = await client(t, { config: null, storage: {
    'kvmflow-timing-draft-v1': win.localStorage.getItem('kvmflow-timing-draft-v1'),
  } });
  assert.equal(reopened.win.document.querySelector('#timing-absent').value, '3000');
});

test('a missing remote input blocks saving instead of sending null to the runtime', async t => {
  const incomplete = structuredClone(v2);
  incomplete.switch_group.devices[2].monitors = [{ fingerprint: 'display-a', local_input: 17 }];
  const { win, requests } = await client(t, { config: incomplete });
  win.navigate('settings');
  await win.saveSettings();
  assert.equal(requests.some(entry => entry.method === 'config.set'), false);
  assert.match(win.document.querySelector('#event-list').textContent, /本地配置未保存/);
  assert.match(win.document.querySelector('#event-list').textContent, /请填写/);
});

test('dynamic names and diagnostics are text rather than executable markup', async t => {
  const config = structuredClone(v2);
  config.switch_group.devices[0].name = '<img src=x onerror=alert(1)>';
  const { win } = await client(t, { config });
  win.navigate('settings');
  assert.equal(win.document.querySelectorAll('#computer-list img').length, 0);
  assert.match(win.document.querySelector('#computer-list').textContent, /<img src=x/);
  win.log('错误', '<svg onload=alert(1)>');
  assert.equal(win.document.querySelectorAll('#event-list svg').length, 0);
});

test('setup choices and dynamically rendered actions work without inline JavaScript', async t => {
  const { win } = await client(t);
  win.navigate('guide');
  win.nextStep(); win.nextStep();
  const choice = win.document.querySelector('[data-value="unknown"]');
  choice.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
  assert.match(win.document.querySelector('#guide-content').textContent, /用显示器菜单/);
  win.document.querySelector('[data-action="setChoice"]').click();
  assert.ok(win.document.querySelector('#target-name'));
  assert.equal(win.document.querySelectorAll('[onclick]').length, 0);
});

test('fresh setup never displays invented hardware or starts USB calibration automatically', async t => {
  const { win, requests } = await client(t, { config: null });
  assert.equal(win.document.querySelectorAll('#display-table tr').length, 0);
  assert.equal(requests.filter(entry => entry.method === 'wizard.begin').length, 0);
  assert.equal(win.document.querySelector('#guide-page').classList.contains('hidden'), false);
});

test('fresh setup starts with two computers and uses the system name for the local one', async t => {
  const { win } = await client(t, { config: null, computerName: ' 我的工作电脑 \n' });
  win.navigate('settings');
  assert.equal(win.document.querySelectorAll('.computer-row').length, 2);
  assert.equal(win.document.querySelector('#edit-name').value, '我的工作电脑');
  assert.match(win.document.querySelector('#computer-list').textContent, /另一台电脑/);
});

test('unavailable system names fall back to 本机 without inventing a platform name', async t => {
  for (const computerName of [undefined, null, '', ' \n']) {
    const { win } = await client(t, { config: null, computerName });
    win.navigate('settings');
    assert.equal(win.document.querySelector('#edit-name').value, '本机');
    assert.equal(win.document.querySelectorAll('.computer-row').length, 2);
  }
});

test('the system name replaces a stale saved name without changing input mappings', async t => {
  const { win } = await client(t, { computerName: '系统的新名称' });
  win.navigate('settings');
  assert.equal(win.document.querySelector('#edit-name').value, '系统的新名称');
  assert.equal(win.document.querySelector('#display-table').textContent, '副屏89主屏1617');
});

test('hidden host mode retains its saved value and is not cleared by saving settings', async t => {
  const config = structuredClone(v2);
  config.switch_group.devices.pop();
  config.advanced.arrival_correction_enabled = true;
  const { win, requests } = await client(t, { config });
  win.navigate('settings');
  const control = win.document.querySelector('#arrival-correction');
  assert.equal(control.closest('.response-row').classList.contains('hidden'), true);
  assert.equal(control.checked, true);
  assert.ok(win.document.querySelector('#startup-at-login'));
  await win.saveSettings();
  const saved = requests.find(entry => entry.method === 'config.set').params.config;
  assert.equal(saved.advanced.arrival_correction_enabled, true);
});

test('diagnostics use the native save bridge once with UI and backend evidence', async t => {
  const instance = await client(t);
  instance.win.document.querySelector('#export-log').click();
  await new Promise(resolve => setTimeout(resolve, 10));
  const payload = instance.export();
  assert.equal(payload.config.local_device.device_id, 'host-win');
  assert.equal(payload.sidecar.backend, 'test');
  assert.ok(payload.ui.computers);
  assert.equal(instance.requests.filter(entry => entry.method === 'diagnostics.collect').length, 1);
});

test('paused and failed backend states stay visible and do not claim picture success', async t => {
  const { win, subscriptions } = await client(t);
  subscriptions.get('state')({ state: 'armed', enabled: false });
  assert.match(win.document.querySelector('#nav-state').textContent, /自动切换已暂停/);
  subscriptions.get('switch.report')({ per_monitor: [{ commanded: true }] });
  assert.equal(win.document.querySelector('#last-title').textContent, '指令已接受');
  subscriptions.get('runtime.error')({ message: '请重启' });
  assert.equal(win.document.querySelector('#nav-state').textContent, '后台组件不可用');
});

test('USB calibration ignores a peripheral departure and requires the physical hub', async t => {
  const { win, subscriptions, requests } = await client(t, { config: null, displays: [
    { edid_id: 'screen', model_name: '屏幕', builtin: false, ddc: { state: 'available', input: 15 } },
  ] });
  await win.confirmUsb();
  assert.equal(requests.filter(entry => entry.method === 'wizard.begin').length, 1);
  const candidates = subscriptions.get('wizard.candidates');
  await candidates({ disappeared: [{ vid_pid: '3837:303c', product: 'keyboard', serial: 'kbd' }] });
  assert.match(win.document.querySelector('#guide-content').textContent, /等待 Switch Hub/);
  assert.equal(requests.filter(entry => entry.method === 'display.list').length, 0);
  await candidates({ disappeared: [
    { vid_pid: '1a40:0101', product: 'USB Hub', serial: 'hub' },
    { vid_pid: '3837:303c', product: 'keyboard', serial: 'kbd' },
  ] });
  assert.equal(requests.filter(entry => entry.method === 'wizard.end').length, 1);
  assert.equal(requests.filter(entry => entry.method === 'display.list').length, 1);
  assert.match(win.document.querySelector('#guide-content').textContent, /屏幕/);
});
