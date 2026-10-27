const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');
const { JSDOM } = require('jsdom');
const { initializeI18n } = require('./i18n-helper');
const root = path.resolve(__dirname, '..');
const read = relative => fs.readFileSync(path.join(root, relative), 'utf8');

const { client, v2, tick, automaticConfig } = require('./client-helper');

async function choosePrimary(win) {
  if (win.KVMFlowPairing.activeMode() !== 'role') return;
  win.document.querySelector('[data-pair-role="primary"]').click();
  win.document.querySelector('[data-pair-action="role-next"]').click();
  await tick();
}

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
  win.editComputer('draft-remote');
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
  assert.match(win.document.querySelector('#event-list').textContent, /电脑配置已保存/);
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

test('role-free setup and dynamically rendered actions work without inline JavaScript', async t => {
  const c = await require('./pairing-v2-helper').setup(t);
  await c.start();
  assert.match(c.win.document.querySelector('#guide-content').textContent, /连接另一台电脑/);
  assert.equal(c.win.document.querySelectorAll('[onclick]').length, 0);
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
  assert.equal(win.document.querySelector('#display-table'), null);
  assert.equal(win.document.querySelector('#edit-0').value, '8');
  assert.equal(win.document.querySelector('#edit-1').value, '16');
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

test('only beta builds offer a configuration reset, and a failed reset is reported', async t => {
  const stable = await client(t);
  assert.equal(stable.win.document.querySelector('#reset-config').hidden, true);
  let resets = 0;
  const beta = await client(t, { version: '0.4.0-beta.9', resetConfig: async () => { resets += 1; throw new Error('display restore timed out'); } });
  const button = beta.win.document.querySelector('#reset-config');
  assert.equal(button.hidden, false);
  button.click(); await tick();
  assert.equal(resets, 1);
  assert.equal(button.disabled, false);
  assert.match(beta.win.document.querySelector('#event-list').textContent, /重置失败.*display restore timed out/);
});

test('diagnostics use the native save bridge once with UI and backend evidence', async t => {
  const instance = await client(t);
  instance.win.document.querySelector('#export-log').click();
  await new Promise(resolve => setTimeout(resolve, 10));
  const payload = instance.export();
  assert.match(payload.config.local_device.device_id, /^device-\d+$/);
  assert.equal(payload.sidecar.backend, 'test');
  assert.ok(payload.ui.computers);
  assert.equal(instance.requests.filter(entry => entry.method === 'diagnostics.collect').length, 1);
  assert.equal(payload.redacted, true);
  const text = JSON.stringify(payload);
  for (const name of ['第三台', 'Windows']) assert.equal(text.includes(name), false, `${name} leaked`);
  assert.deepEqual(payload.config.switch_group.devices.map(device => device.name), ['Computer 1', 'Computer 2', 'Computer 3']);
});

test('paused and failed backend states stay visible and do not claim picture success', async t => {
  const { win, subscriptions } = await client(t);
  subscriptions.get('state')({ state: 'armed', enabled: false });
  assert.match(win.document.querySelector('#nav-state').textContent, /已暂停/);
  assert.equal(win.document.querySelector('#nav-state').title, win.document.querySelector('#nav-state').textContent);
  subscriptions.get('switch.report')({ per_monitor: [{ commanded: true }] });
  assert.equal(win.document.querySelector('#last-title').textContent, '最近一次切换');
  assert.match(win.document.querySelector('#last-copy').textContent, /已向.*发送显示器切换命令/);
  subscriptions.get('runtime.error')({ message: '请重启' });
  assert.equal(win.document.querySelector('#nav-state').textContent, '后台不可用');
});

test('settings USB calibration ignores a peripheral departure and requires the physical hub', async t => {
  const { win, subscriptions, requests } = await client(t);
  win.navigate('settings');
  await win.recalibrateUsb();
  await subscriptions.get('wizard.candidates')({ disappeared: [{ vid_pid: '3837:303c', product: 'keyboard' }] });
  assert.equal(requests.filter(r => r.method === 'wizard.end').length, 0);
  await subscriptions.get('wizard.candidates')({ disappeared: [{ vid_pid: '1a40:0101', product: 'USB Hub' }, { vid_pid: '3837:303c', product: 'keyboard' }] });
  assert.equal(requests.filter(r => r.method === 'wizard.end').length, 1);
  assert.ok(requests.filter(r => r.method === 'config.set').length);
});

test('English renders dynamic status, setup, timing and local-input confirmation', async t => {
  const c = await client(t, { language: 'en', config: automaticConfig(), computerName: '工作电脑' });
  const doc = c.win.document;
  assert.equal(doc.documentElement.lang, 'en');
  assert.equal(doc.querySelector('[data-computer="host-win"] strong').textContent, '工作电脑');
  assert.equal(doc.querySelector('#nav-state').textContent, 'Waiting for USB');
  assert.equal(doc.querySelector('#nav-state').title, 'Waiting for USB');
  c.win.navigate('guide');
  assert.equal(doc.querySelector('#steps .active').textContent, '1 Local setup');
  c.win.navigate('settings');
  assert.equal(doc.querySelector('#local-input-origin').textContent, 'Read automatically');
  assert.equal(doc.querySelector('label[for="timing-absent"]').textContent, 'Departure delay');
  const input = doc.querySelector('#edit-0');
  input.value = '15';
  input.dispatchEvent(new c.win.Event('change', { bubbles: true }));
  assert.equal(doc.querySelector('#local-input-confirmation-title').textContent, 'Change this computer’s monitor input values?');
  assert.match(doc.querySelector('#local-input-confirmation-copy').textContent, /8 → 15/);
  assert.match(doc.querySelector('#local-input-confirmation-copy').textContent, /prevent the monitor/);
  assert.equal(c.requests.some(entry => entry.method === 'config.set'), false);
});

test('language selection preserves unsaved computer fields without writing hardware configuration', async t => {
  const c = await client(t, { config: automaticConfig() });
  const doc = c.win.document;
  c.win.navigate('settings');
  doc.querySelector('#edit-name').value = '我的电脑 <script>literal</script>';
  doc.querySelector('#edit-port').value = '1';
  doc.querySelector('#edit-0').value = '15';
  const language = doc.querySelector('#language-select');
  language.value = 'en';
  language.dispatchEvent(new c.win.Event('change', { bubbles: true }));
  await tick();
  assert.equal(doc.documentElement.lang, 'en');
  assert.equal(doc.querySelector('#edit-name').value, '我的电脑 <script>literal</script>');
  assert.equal(doc.querySelector('#edit-port').value, '1');
  assert.equal(doc.querySelector('#edit-0').value, '15');
  assert.equal(doc.querySelector('#edit-0').dataset.confirmedValue, '8');
  assert.equal(c.win.hasUnsavedComputerEditor(), true);
  assert.equal(doc.querySelector('#settings-page').classList.contains('hidden'), false);
  assert.equal(c.win.localStorage.getItem('kvmflow-language-v1'), 'en');
  assert.equal(c.requests.some(entry => entry.method === 'config.set'), false);
  language.value = 'zh-CN';
  language.dispatchEvent(new c.win.Event('change', { bubbles: true }));
  await tick();
  assert.equal(doc.querySelector('#save-settings').textContent, '保存电脑配置');
  assert.equal(doc.querySelector('#edit-name').value, '我的电脑 <script>literal</script>');
});

test('a failed native language save restores the selected preference and retains the editor', async t => {
  const c = await client(t);
  c.win.kvmflow.language = { set: async () => { throw new Error('disk full'); } };
  c.win.navigate('settings');
  const doc = c.win.document;
  doc.querySelector('#edit-name').value = '未保存';
  const language = doc.querySelector('#language-select');
  language.value = 'en';
  language.dispatchEvent(new c.win.Event('change', { bubbles: true }));
  await tick();
  assert.equal(doc.documentElement.lang, 'zh-CN');
  assert.equal(language.value, 'zh-CN');
  assert.equal(language.disabled, false);
  assert.equal(doc.querySelector('#edit-name').value, '未保存');
  assert.equal(doc.querySelector('#language-save-status').textContent, '无法保存语言偏好，请重试。');
});

test('generated computer names and new diagnostic events follow language without changing user names', async t => {
  const c = await client(t, { config: null, computerName: null });
  c.win.navigate('settings');
  c.win.KVMFlowI18n.setPreference('en');
  const doc = c.win.document;
  assert.equal(doc.querySelector('#edit-name').value, 'This computer');
  assert.match(doc.querySelector('#computer-list').textContent, /Other computer/);
  assert.equal(c.win.hasUnsavedComputerEditor(), false);
  c.win.log(c.win.KVMFlowI18n.t('switch.timing.saved'), c.win.KVMFlowI18n.t('settings.saved.on.this.computer'));
  c.win.KVMFlowI18n.setPreference('zh-CN');
  assert.match(doc.querySelector('#event-list').textContent, /切换响应已保存/);
  assert.equal(doc.querySelector('#edit-name').value, '本机');
});

test('stored legacy diagnostic events follow language without changing unknown evidence', async t => {
  const events = [
    { title: '电脑配置组已保存', copy: '初始化完成后，该组电脑配置将用于自动切换。', time: '21:34' },
    { title: '本地配置未保存', copy: '请先检测显示器，再保存设置。', titleMessage: '本地配置未保存', time: '17:44' },
    { title: '设备原始记录 <script>bad()</script>', copy: '用户设备名称 工作电脑', time: '12:00' },
  ];
  // Configured: without a configuration, earlier launches' events are dropped.
  const c = await client(t, { computerName: '工作电脑', storage: { 'kvmflow-local-prototype-v2': JSON.stringify({
    localId: 'mac', nextId: 'win', usbConfirmed: false, displays: [], events,
    computers: [{ id: 'mac', name: '工作电脑', port: 1, local: true, sources: [] }],
  }) } });
  c.win.navigate('diagnostics');
  c.win.KVMFlowI18n.setPreference('en');
  const list = c.win.document.querySelector('#event-list');
  assert.match(list.textContent, /Computer group saved/);
  assert.match(list.textContent, /This computer group will be used/);
  assert.match(list.textContent, /Local configuration not saved/);
  assert.match(list.textContent, /Detect the monitors before saving settings/);
  assert.match(list.textContent, /用户设备名称 工作电脑/);
  assert.equal(list.querySelector('script'), null);
  c.win.KVMFlowI18n.setPreference('zh-CN');
  assert.match(list.textContent, /电脑配置已保存/);
  assert.match(list.textContent, /本地配置未保存/);
});

test('preset and custom timing options switch language without changing saved values', async t => {
  const config = automaticConfig();
  config.trigger.debounce.t_absent_ms = 2500;
  const c = await client(t, { config });
  c.win.navigate('settings');
  const select = c.win.document.querySelector('#timing-absent');
  assert.equal(select.selectedOptions[0].textContent, '2.5 秒（自定义）');
  const beforeWrites = c.requests.filter(entry => entry.method === 'config.set').length;
  c.win.KVMFlowI18n.setPreference('en');
  assert.equal(select.value, '2500');
  assert.equal(select.selectedOptions[0].textContent, '2.5 seconds (custom)');
  assert.equal(select.querySelector('option[value="0"]').textContent, 'Immediately');
  assert.equal(c.win.document.querySelector('#timing-stable option[value="1000"]').textContent, '1 second');
  assert.equal(c.win.document.querySelector('#timing-cooldown option[value="0"]').textContent, 'Off');
  assert.equal(c.win.document.querySelector('#timing-cooldown option[value="15000"]').textContent, '15 seconds');
  c.win.KVMFlowI18n.setPreference('zh-CN');
  assert.equal(select.selectedOptions[0].textContent, '2.5 秒（自定义）');
  assert.equal(c.requests.filter(entry => entry.method === 'config.set').length, beforeWrites);
});

test('language changes during background USB identification preserve the active session', async t => {
  const { setup, waiting, state } = require('./pairing-v2-helper');
  const c = await setup(t, { state: state([]) }); await waiting(c);
  c.win.KVMFlowI18n.setPreference('en');
  assert.match(c.win.document.querySelector('#guide-content').textContent, /Switch to the other computer/);
  assert.equal(c.requests.filter(r => r.method === 'wizard.begin').length, 1);
  assert.equal(c.requests.filter(r => r.method === 'wizard.end').length, 0);
});

test('an unreadable saved configuration is explained after startup in both languages', async t => {
  const state = { state: 'unconfigured', enabled: true, configRecovery: { preservedAs: 'config.json.corrupt-1792035104000', error: 'EOF while parsing' } };
  const zh = await client(t, { config: null, state });
  const zhEvents = zh.win.document.querySelector('#event-list').textContent;
  assert.match(zhEvents, /配置文件无法读取/);
  assert.match(zhEvents, /config\.json\.corrupt-1792035104000/);
  const en = await client(t, { config: null, state, language: 'en' });
  assert.match(en.win.document.querySelector('#event-list').textContent, /Saved configuration could not be read[\s\S]*config\.json\.corrupt-1792035104000/);
  const unpreserved = await client(t, { config: null, state: { ...state, configRecovery: { preservedAs: null, error: 'x' } } });
  assert.match(unpreserved.win.document.querySelector('#event-list').textContent, /配置文件无法读取[\s\S]*重新完成设置/);
  const healthy = await client(t);
  assert.doesNotMatch(healthy.win.document.querySelector('#event-list').textContent, /配置文件无法读取/);
});

test('computer editor removes follow controls and preserves archived policies and mixed assignment', async t => {
  const configured = structuredClone(v2);
  configured.switch_group.devices[0].monitors[0].follow = false;
  configured.switch_group.devices[0].arrival_layout_id = 'archived';
  configured.switch_group.layouts = [{layout_id:'archived',name:'Archive',monitors:[]}];
  configured.switch_group.mixed = {enabled:false,revision:7,assignment:[{fingerprint:'display-a',device_id:'host-mac'}]};
  const { win, requests } = await client(t, {config:configured});
  win.navigate('settings');
  win.editComputer('host-mac');
  assert.equal(win.document.querySelector('#edit-follow-0'), null);
  assert.doesNotMatch(win.document.querySelector('#computer-editor').textContent, /切换到这台电脑时/);
  win.document.querySelector('#edit-name').value = 'My Mac';
  await win.saveSettings();
  const saved = requests.findLast(entry => entry.method === 'config.set').params.config;
  assert.equal(saved.switch_group.devices[0].monitors.find(m=>m.fingerprint==='display-a').follow, false);
  assert.equal(saved.switch_group.devices[0].arrival_layout_id, 'archived');
  assert.deepEqual(saved.switch_group.layouts, configured.switch_group.layouts);
  assert.deepEqual(saved.switch_group.mixed.assignment, configured.switch_group.mixed.assignment);
});

test('a config migrated from v1 opens with both computers and keeps its device ids', async t => {
  const migrated = {
    schema_version: 2,
    local_device: { device_id: 'legacy-local', host_label: 'Old Mac', monitors: [
      { fingerprint: 'display-a', label: '主屏', local_input: 15, source: 'learned_active_read' },
    ] },
    switch_group: { group_id: 'legacy-switch-group', revision: 1, devices: [
      { device_id: 'legacy-local', name: 'Old Mac', port_index: 1, monitors: [{ fingerprint: 'display-a', label: '主屏', local_input: 15 }] },
      { device_id: 'legacy-remote', name: '', port_index: 2, monitors: [{ fingerprint: 'display-a', label: '主屏', local_input: 16 }] },
    ] },
    trigger: v2.trigger,
    advanced: v2.advanced,
  };
  const { win, requests } = await client(t, { config: migrated, computerName: 'Old Mac' });
  win.navigate('settings');
  assert.equal(win.document.querySelectorAll('.computer-row').length, 2);
  win.editComputer('legacy-remote');
  assert.notEqual(win.document.querySelector('#edit-name').value.trim(), '');
  assert.equal(win.document.querySelector('#edit-0').value, '16');
  await win.saveSettings();
  const saved = requests.findLast(entry => entry.method === 'config.set').params.config;
  assert.equal(saved.local_device.device_id, 'legacy-local');
  assert.equal(JSON.stringify(saved.switch_group.devices.map(device => device.device_id).sort()), '["legacy-local","legacy-remote"]');
});

test('sidebar status uses short fixed labels and keeps the full title in both languages', async t => {
  const cases = [
    ['zh-CN', ['等待 USB Switch', '正在识别', '正在发送指令', '已暂停', '后台不可用']],
    ['en', ['Waiting for USB', 'Identifying', 'Switching', 'Paused', 'Unavailable']],
  ];
  for (const [language, labels] of cases) {
    const { win, subscriptions } = await client(t, { language, config: automaticConfig() });
    const nav = win.document.querySelector('#nav-state');
    const check = expected => {
      assert.equal(nav.textContent, expected);
      assert.equal(nav.title, expected);
    };
    check(labels[0]);
    subscriptions.get('state')({ state: 'learning', enabled: true });
    check(labels[1]);
    subscriptions.get('state')({ state: 'pushing', enabled: true });
    check(labels[2]);
    subscriptions.get('state')({ state: 'armed', enabled: false });
    check(labels[3]);
    subscriptions.get('runtime.error')({ message: 'boom' });
    check(labels[4]);
  }
});

test('tray feedback navigation opens the dialog from both a running and a cold window', async t => {
  const running = await client(t);
  running.navigate('diagnostics#feedback');
  await tick();
  assert.equal(running.win.document.querySelector('#compatibility-dialog').open, true);
  assert.equal(running.win.document.querySelector('#diagnostics-page').classList.contains('hidden'), false);
  assert.equal(running.requests.some(r => r.method === 'diagnostics.collect'), false);
  const cold = await client(t, { initialPage: 'diagnostics#feedback' });
  assert.equal(cold.win.document.querySelector('#compatibility-dialog').open, true);
});

test('feedback uses the same redacted diagnostics collector as native export', async t => {
  const c = await client(t, { computerName: 'PRIVATE-HOST' });
  const payload = await c.win.kvmflow.collectRedactedDiagnostics({ requireRuntime: true });
  assert.equal(payload.redacted, true);
  assert.equal(payload.sidecar.backend, 'test');
  assert.equal(JSON.stringify(payload).includes('PRIVATE-HOST'), false);
  const failed = await client(t, { request: async method => { if (method === 'diagnostics.collect') throw new Error('offline'); } });
  await assert.rejects(failed.win.kvmflow.collectRedactedDiagnostics({ requireRuntime: true }));
  const partialExport = await failed.win.kvmflow.collectRedactedDiagnostics();
  assert.equal(partialExport.sidecar, null);
  assert.equal(partialExport.redacted, true);
});


test('cold tray feedback actively probes display metadata without needing cached inventory', async t => {
  const display = { edid_id: 'display-b', manufacturer: 'DEL', model_name: 'U2723QE', ddc: { state: 'available', input: 15 } };
  const c = await client(t, { initialPage: 'diagnostics#feedback', displays: [display] });
  assert.equal(c.win.kvmflow.cachedDisplays, undefined);
  const report = JSON.parse(c.win.document.querySelector('#compatibility-report').textContent).report;
  assert.deepEqual(JSON.parse(JSON.stringify(report.monitors[0])), { vendor: 'DEL', model: 'U2723QE', ddc: true, vcp60: 15, inputs: [] });
  assert.equal(c.requests.filter(r => r.method === 'display.list').length, 1);
});

test('native export and actual feedback attachment both remove unpaired peer identities and addresses', async t => {
  const { backend, secrets } = require('./fixtures/diagnostics-private');
  const diagnostics = () => ({...backend(),legacy:{local_device:{device_id:'mac'},devices:[{device_id:'win'}],platform:'macos',peer:{platform:'windows',backend:'windows_dxva2'},event:'mac -> win; macos windows'}});
  const c = await client(t, { request: async method => method === 'diagnostics.collect' ? diagnostics() : undefined });
  c.win.document.querySelector('#export-log').click(); await tick();
  const exported = c.export(); assert.equal(exported.redacted, true);
  c.navigate('diagnostics#feedback'); await tick();
  c.win.document.querySelector('[name="feedback-kind"][value="works"]').click();
  c.win.document.querySelector('#feedback-diagnostics').click(); await tick();
  const preview = JSON.parse(c.win.document.querySelector('#compatibility-report').textContent);
  let sent;
  c.win.kvmflow.sendFeedback = async payload => { sent = payload; return { ok: true, id: 'fb_01JABCDEF0123456789ABCDEFG' }; };
  c.win.document.querySelector('#feedback-send').click(); await tick();
  assert.equal(sent.diagnostics, preview.diagnostics);
  for (const text of [JSON.stringify(exported), sent.diagnostics]) for (const secret of secrets) {
    assert.equal(text.includes(secret), false, `${secret} leaked`);
    assert.equal(text.includes(JSON.stringify(secret).slice(1, -1)), false, `${secret} leaked as escaped JSON`);
  }
  for (const data of [exported, JSON.parse(sent.diagnostics)]) {
    assert.equal(data.sidecar.legacy.platform,'macos');
    assert.equal(data.sidecar.legacy.peer.platform,'windows');
    assert.equal(data.sidecar.legacy.peer.backend,'windows_dxva2');
    assert.match(data.sidecar.legacy.local_device.device_id,/^device-\d+$/);
    assert.match(data.sidecar.legacy.event,/^device-\d+ -> device-\d+; macos windows$/);
    assert.match(data.sidecar.events[0].fields.instance, /^device-path-\d+$/);
    assert.match(data.sidecar.events[0].fields.usb, /^device-path-\d+$/);
  }
  assert.deepEqual(JSON.parse(sent.diagnostics).sidecar, JSON.parse(JSON.stringify(exported.sidecar)));
});
