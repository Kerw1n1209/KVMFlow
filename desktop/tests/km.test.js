const assert = require('node:assert/strict');
const test = require('node:test');
const { client, pairedV2 } = require('./client-helper');
const { setup, state, click, body, emit, profile, tick } = require('./pairing-v2-helper');
const calls = (c, method) => c.requests.filter(r => r.method === method);
const $ = (c, selector) => c.win.document.querySelector(selector);

const km = (over = {}) => ({ enabled: true, configured: true, server_device_id: 'host-win', hotkey: 'Control+Alt+k', clipboard: true, role: 'server',
  home_hotkey: 'Control+Alt+Escape', swap_command_control: true, invert_scroll: false, keep_awake: false, peer_platform: null,
  input_here: true, peer_device_id: 'dev-peer', peer_online: true, running: true, error: null, permissions: { accessibility: true, input_monitoring: true }, ...over });
const off = () => km({ enabled: false, configured: false, peer_device_id: null, peer_online: false, running: false });
const kmRequests = current => async (method, params) => {
  if (method === 'km.get') return current();
  if (method === 'km.configure') return km(params);
  if (method === 'km.permissions.request') return current();
  if (method === 'lan.test.start') return { test_id: 'test-1' };
  return undefined;
};

async function toShortcutPage(c) {
  click(c, 'ob-mode'); await tick();
  click(c, 'ob-kind', '[data-kind="hotkey"]'); await tick();
  click(c, 'mode-next'); await tick();
  click(c, 'local-next'); await tick();
  await c.pair();
  c.setConfig(pairedV2());
  await emit(c, 'lan.pair', { phase: 'committed', device_id: 'dev-peer', initiator: true, config_merged: true });
  await emit(c, 'lan.profile', profile());
  await c.advance();
}

test('shortcut labels follow the platform and need a modifier', async t => {
  const { win } = await client(t, { state: state() });
  const Hotkey = win.KVMFlowHotkey;
  assert.deepEqual([...Hotkey.labels('Control+Alt+k', 'macos')], ['Control', 'Option', 'K']);
  assert.deepEqual([...Hotkey.labels('Control+Super+F2', 'windows')], ['Ctrl', 'Win', 'F2']);
  assert.equal(Hotkey.fromEvent({ code: 'KeyJ', ctrlKey: true, shiftKey: true }), 'Control+Shift+j');
  assert.equal(Hotkey.fromEvent({ code: 'Digit4', metaKey: true, altKey: true }), 'Alt+Super+4');
  assert.equal(Hotkey.fromEvent({ code: 'KeyJ' }), null, 'a bare letter would stop typing');
  assert.equal(Hotkey.fromEvent({ code: 'ShiftLeft', shiftKey: true }), null, 'a modifier alone is not a shortcut');
});

test('shortcut setup skips USB capture, saves the recorded shortcut and starts the test', async t => {
  const c = await setup(t, { request: kmRequests(off) });
  await toShortcutPage(c);
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'hotkey');
  assert.match(body(c), /CTRL|Ctrl/);
  assert.equal(calls(c, 'wizard.begin').length, 0, 'there is no USB switch to identify');

  click(c, 'hotkey-change'); await tick();
  assert.match(body(c), /按下新的快捷键/);
  assert.equal($(c, '[data-pair-action="hotkey-next"]').disabled, true);
  c.win.dispatchEvent(new c.win.KeyboardEvent('keydown', { code: 'KeyJ' }));
  await tick();
  assert.match(body(c), /按下新的快捷键/, 'a bare key keeps listening');
  c.win.dispatchEvent(new c.win.KeyboardEvent('keydown', { code: 'KeyJ', ctrlKey: true, shiftKey: true }));
  await tick();
  assert.match(body(c), /Ctrl.*Shift.*J/s);

  click(c, 'hotkey-next'); await tick(); await tick();
  assert.deepEqual({ ...calls(c, 'km.configure')[0].params }, { enabled: true, server_device_id: c.win.eval('model.localId'), hotkey: 'Control+Shift+j' });
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'try');
  assert.equal(calls(c, 'lan.test.start').length, 1);
  assert.equal(calls(c, 'wizard.begin').length, 0);
  assert.match(body(c), /按 Ctrl \+ Shift \+ J，切到 GAME/);
});

test('Esc during recording keeps the previous shortcut', async t => {
  const c = await setup(t, { request: kmRequests(off) });
  await toShortcutPage(c);
  click(c, 'hotkey-change'); await tick();
  c.win.dispatchEvent(new c.win.KeyboardEvent('keydown', { code: 'Escape' }));
  await tick();
  assert.match(body(c), /Ctrl.*Alt.*K/s);
});

test('macOS asks for both permissions before continuing', async t => {
  let granted = { accessibility: true, input_monitoring: false };
  const c = await setup(t, { platform: 'macos', request: kmRequests(() => ({ ...off(), permissions: granted })) });
  await tick();
  click(c, 'ob-mode'); await tick();
  click(c, 'ob-kind', '[data-kind="hotkey"]'); await tick();
  click(c, 'mode-next'); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'perms');
  assert.equal($(c, '[data-pair-action="perms-next"]').disabled, true);
  $(c, '[data-km-permission="input_monitoring"]').click(); await tick();
  assert.equal(calls(c, 'km.permissions.request')[0].params.permission, 'input_monitoring');
  granted = { accessibility: true, input_monitoring: true };
  await c.win.KVMFlowKm.refresh(); await tick();
  assert.equal($(c, '[data-pair-action="perms-next"]').disabled, false);
  click(c, 'perms-next'); await tick();
  assert.equal(c.win.KVMFlowPairing.activeMode(), 'local');
});

test('the USB switch mode never asks for keyboard permissions', async t => {
  const c = await setup(t, { platform: 'macos', request: kmRequests(off) });
  await tick();
  click(c, 'ob-mode'); await tick();
  click(c, 'ob-kind', '[data-kind="switch"]'); await tick();
  click(c, 'mode-next'); await tick();
  assert.notEqual(c.win.KVMFlowPairing.activeMode(), 'perms');
  assert.equal(calls(c, 'wizard.begin').length, 1);
});

test('the status page shows sharing, a lost peer and a missing permission', async t => {
  let current = km();
  const c = await client(t, { config: pairedV2(), state: state(), request: kmRequests(() => current) });
  await tick();
  assert.equal($(c, '#km-toggle-row').hidden, false);
  assert.equal($(c, '#km-toggle').checked, true);
  assert.equal($(c, '#km-alert').hidden, true);
  assert.equal($(c, '#km-hint').hidden, false);

  const notify = c.subscriptions.get('km.changed');
  current = km({ peer_online: false });
  await notify(current); await tick();
  assert.match($(c, '#km-alert').textContent, /断开了.*键鼠已回到这台电脑/s);
  assert.equal($(c, '#km-hint').hidden, true);

  current = km({ error: 'E_KM_PERMISSION', permissions: { accessibility: false, input_monitoring: true } });
  await notify(current); await tick();
  assert.match($(c, '#km-alert').textContent, /键鼠共享已暂停.*辅助功能未授权/s);
  $(c, '#km-alert [data-km-permission]').click(); await tick();
  assert.equal(calls(c, 'km.permissions.request')[0].params.permission, 'accessibility');

  $(c, '#km-toggle').checked = false;
  $(c, '#km-toggle').dispatchEvent(new c.win.Event('change')); await tick();
  assert.equal(calls(c, 'km.configure').at(-1).params.enabled, false);
});

test('in shortcut mode the holder comes from sharing and the switch queue is hidden', async t => {
  const { win } = await client(t, { state: state() });
  const View = win.KVMFlowMixedView;
  const ctx = { km: { enabled: true, input_here: false, peer_device_id: 'pc' }, selfPlatform: 'macOS',
    computers: [{ id: 'mac', name: 'Mac', local: true, port: 1 }, { id: 'pc', name: 'PC', local: false, port: 2 }] };
  const rows = View.statusComputers(ctx);
  assert.deepEqual([...rows].map(row => [row.id, row.holder, row.detail]), [['mac', false, 'macOS'], ['pc', true, '']]);
  assert.equal(View.nextSwitch(ctx), null);
});

test('settings send each sharing change on its own', async t => {
  const c = await client(t, { config: pairedV2(), state: state(), request: kmRequests(() => km()) });
  await tick();
  const section = $(c, '[data-km-section]');
  assert.equal(section.hidden, false);
  const server = $(c, '#km-server');
  assert.equal(server.options.length, 2);
  $(c, '#km-clipboard').checked = false;
  $(c, '#km-clipboard').dispatchEvent(new c.win.Event('change')); await tick();
  assert.deepEqual({ ...calls(c, 'km.configure').at(-1).params }, { clipboard: false });
  $(c, '[data-km-hotkey="hotkey"]').click(); await tick();
  assert.match($(c, '[data-km-hotkey="hotkey"]').textContent, /按下新的快捷键/);
  c.win.dispatchEvent(new c.win.KeyboardEvent('keydown', { code: 'F2', ctrlKey: true }));
  await tick();
  assert.deepEqual({ ...calls(c, 'km.configure').at(-1).params }, { hotkey: 'Control+F2' });
  $(c, '[data-km-hotkey="home_hotkey"]').click(); await tick();
  c.win.dispatchEvent(new c.win.KeyboardEvent('keydown', { code: 'KeyB', ctrlKey: true, altKey: true }));
  await tick();
  assert.deepEqual({ ...calls(c, 'km.configure').at(-1).params }, { home_hotkey: 'Control+Alt+b' });
  for (const [id, name] of [['#km-invert', 'invert_scroll'], ['#km-awake', 'keep_awake']]) {
    $(c, id).checked = true;
    $(c, id).dispatchEvent(new c.win.Event('change')); await tick();
    assert.deepEqual({ ...calls(c, 'km.configure').at(-1).params }, { [name]: true });
  }
});

test('a shortcut already in use keeps listening instead of saving', async t => {
  const c = await client(t, { config: pairedV2(), state: state(), request: kmRequests(() => km({ home_hotkey: 'Control+Alt+Escape' })) });
  await tick();
  const field = $(c, '[data-km-hotkey="hotkey"]');
  field.click(); await tick();
  c.win.dispatchEvent(new c.win.KeyboardEvent('keydown', { code: 'Escape', ctrlKey: true, altKey: true }));
  await tick();
  assert.match(field.textContent, /已被占用/);
  assert.equal(calls(c, 'km.configure').length, 0);
  c.win.dispatchEvent(new c.win.KeyboardEvent('keydown', { code: 'KeyJ', ctrlKey: true, altKey: true }));
  await tick();
  assert.deepEqual({ ...calls(c, 'km.configure').at(-1).params }, { hotkey: 'Control+Alt+j' });
});

test('a system shortcut keeps listening instead of saving', async t => {
  const c = await client(t, { config: pairedV2(), state: state(), request: kmRequests(() => km()) });
  await tick();
  const field = $(c, '[data-km-hotkey="hotkey"]');
  for (const keys of [{ code: 'KeyC', metaKey: true }, { code: 'KeyV', ctrlKey: true }, { code: 'F4', altKey: true }, { code: 'Digit4', metaKey: true, shiftKey: true }]) {
    field.click(); await tick();
    c.win.dispatchEvent(new c.win.KeyboardEvent('keydown', keys));
    await tick();
    assert.match(field.textContent, /已被占用/);
    assert.equal(calls(c, 'km.configure').length, 0);
  }
  c.win.dispatchEvent(new c.win.KeyboardEvent('keydown', { code: 'KeyC', ctrlKey: true, altKey: true }));
  await tick();
  assert.deepEqual({ ...calls(c, 'km.configure').at(-1).params }, { hotkey: 'Control+Alt+c' });
});

test('key mapping appears only between a Mac and a PC and names the keyboard side', async t => {
  const options = (platform, current) => ({ platform, config: pairedV2(), state: state(), request: kmRequests(current) });
  const c = await client(t, options('macos', () => km({ peer_platform: 'windows', server_device_id: 'host-win' })));
  await tick();
  const local = c.win.eval('model.localId');
  assert.equal($(c, '#km-swap-row').hidden, false);
  const expectLabel = (context, label, copy) => {
    assert.equal(context.win.document.querySelector('#km-swap option[value="on"]').textContent, label);
    assert.equal(context.win.document.querySelector('#km-swap-copy').textContent, copy);
  };
  await c.subscriptions.get('km.changed')(km({ peer_platform: 'windows', server_device_id: local })); await tick();
  expectLabel(c, 'Command 映射为 Ctrl', '用 Mac 键盘操作 Windows 时');
  await c.subscriptions.get('km.changed')(km({ peer_platform: 'windows', server_device_id: 'dev-peer' })); await tick();
  expectLabel(c, 'Ctrl 映射为 Command', '用 Windows 键盘操作 Mac 时');
  $(c, '#km-swap').value = 'off';
  $(c, '#km-swap').dispatchEvent(new c.win.Event('change')); await tick();
  assert.deepEqual({ ...calls(c, 'km.configure').at(-1).params }, { swap_command_control: false });

  const same = await client(t, options('macos', () => km({ peer_platform: 'macos' })));
  await tick();
  assert.equal($(same, '#km-swap-row').hidden, true);
});
