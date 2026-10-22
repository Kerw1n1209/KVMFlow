const test = require('node:test');
const assert = require('node:assert/strict');
const { client, v2, tick, mixedState } = require('./client-helper');
const clone = value => structuredClone(value);
const state = (mixed, lan) => ({state:'idle',enabled:true,mixed,...(lan ? {lan} : {})});
const text = (win, selector) => win.document.querySelector(selector).textContent;

test('normal mode renders screens, observed keyboard holder and next physical port without LAN placeholders', async t => {
  const {win, requests} = await client(t);
  assert.equal(win.document.querySelector('#mixed-status').hidden, false);
  assert.equal(win.document.querySelectorAll('.desk-screen').length, 2);
  assert.equal(text(win, '.desk-computer.holder strong'), 'Windows');
  assert.match(text(win, '#next-switch'), /第三台/); // port 2 -> port 3
  assert.equal(win.document.querySelector('.desk-lan'), null);
  assert.equal(win.document.querySelector('.mixed-sync'), null);
  assert.equal(win.document.querySelector('#display-table'), null);
  assert.equal(win.document.querySelector('.switch-chassis'), null);
  assert.equal(requests.some(r => r.method === 'display.list' || r.method === 'mixed.set'), false);
});

test('unobserved holder stays unmarked and does not invent the next computer', async t => {
  const mixed = mixedState(); mixed.keyboard_holder_device_id = null;
  const {win} = await client(t, {state:state(mixed)});
  assert.equal(win.document.querySelector('.desk-computer.holder'), null);
  assert.match(text(win, '#next-switch'), /尚未确定键鼠位置/);
});

test('inferred holder is visibly qualified', async t => {
  const mixed = mixedState(); mixed.holder_confidence = 'inferred';
  const {win} = await client(t, {state:state(mixed)});
  assert.equal(text(win, '.holder-tag'), '键鼠预计在这里');
});

test('detached assigned screen uses target after invalidated input, without claiming picture proof', async t => {
  const mixed = mixedState(); mixed.enabled = true;
  Object.assign(mixed.destinations[0], {connected:false,current_device_id:null,target_device_id:'host-mac',fixed:true,evidence:'ddc_commanded'});
  const {win} = await client(t, {state:state(mixed)});
  assert.match(text(win, '.desk-screen.detached'), /Mac[\s\S]*本机已断开这块屏幕/);
  assert.match(win.document.querySelector('.desk-screen.detached').title, /尚未确认实际画面/);
  assert.equal(win.document.querySelector('.desk-screen.detached').textContent.includes('未知'), false);
  assert.equal(win.document.querySelectorAll('.desk-screen-label .settings-tag').length, 1);
});

test('available built-in screen is fixed locally, never gets an assignment dropdown', async t => {
  const mixed = mixedState(); mixed.destinations.push({fingerprint:'builtin',builtin:true,available:true,connected:true});
  const {win} = await client(t, {state:state(mixed)});
  assert.equal(win.document.querySelectorAll('.assign-select').length, 2);
  assert.match(text(win, '.assign-cell.builtin'), /Windows[\s\S]*始终属于本机/);
  assert.equal(win.document.querySelectorAll('.desk-screen.builtin').length, 1);
});

test('closed lid and single screen prevent enabling but never disabling active mixed mode', async t => {
  const mixed = mixedState(); mixed.destinations.splice(1);
  mixed.destinations.push({fingerprint:'builtin',builtin:true,available:false,lid_closed:true,connected:false});
  mixed.validation = {ok:false,reasons:[{code:'not_enough_screens',severity:'error'}]};
  const {win, subscriptions} = await client(t, {state:state(mixed)});
  assert.equal(win.document.querySelector('#mixed-toggle').disabled, true);
  assert.match(text(win, '#mixed-description'), /至少两块屏幕/);
  assert.match(text(win, '.assign-cell.builtin'), /已合盖/);
  assert.equal(win.document.querySelectorAll('.assign-select').length, 0);
  mixed.enabled = true; subscriptions.get('state')(state(mixed));
  assert.equal(win.document.querySelector('#mixed-toggle').disabled, false);
  win.document.querySelector('#mode-action').click();
  await tick();
});

test('all-to-one reason names the computer and the status action opens mixed settings', async t => {
  const mixed = mixedState(); mixed.assignment.forEach(a => a.device_id = 'host-mac');
  mixed.validation={ok:false,reasons:[{code:'all_to_one_computer',severity:'error'}]};
  const {win,requests} = await client(t, {state:state(mixed)});
  assert.match(text(win, '#mixed-description'), /Mac，混屏没有效果/);
  win.document.querySelector('#mode-action').click();
  assert.equal(win.document.querySelector('#settings-page').classList.contains('hidden'), false);
  assert.equal(requests.some(r=>r.method==='mixed.set'), false);
});

test('three-screen assignment warnings and unassigned screens use stable numbering', async t => {
  const config = clone(v2); config.local_device.monitors.forEach(m => delete m.label);
  config.switch_group.devices.forEach(d => d.monitors.forEach(m => delete m.label));
  const mixed = mixedState(config); mixed.enabled=true;
  mixed.destinations.push({fingerprint:'new-screen',available:true,builtin:false,connected:true,target_device_id:'host-win',current_device_id:null});
  mixed.validation.reasons=[{code:'unassigned_monitor',fingerprint:'new-screen',severity:'warning'},{code:'computer_without_screen',device_id:'host-linux',severity:'warning'}];
  const {win} = await client(t, {config,state:state(mixed)});
  assert.equal(win.document.querySelectorAll('.assign-select').length, 3);
  assert.match(text(win, '#mixed-notes'), /第三台 没有分到屏幕/);
  assert.match(text(win, '#mixed-notes'), /显示器 3 未分配/);
  assert.match(text(win, '#next-switch'), /未分配的 显示器 3/);
});

test('LAN unknown sync is hidden; offline and explicit synced fields are honored', async t => {
  const mixed = mixedState();
  const {win,subscriptions} = await client(t, {state:state(mixed,{peers:[{device_id:'host-mac',online:true}]})});
  assert.equal(win.document.querySelector('.mixed-sync'), null);
  assert.match(text(win, '[data-computer="host-mac"]'), /局域网已连接/);
  subscriptions.get('state')(state(mixed,{peers:[{device_id:'host-mac',online:false}]}));
  assert.match(text(win, '#status-banners'), /Mac 不在线/);
  subscriptions.get('state')(state(mixed,{peers:[{device_id:'host-mac',online:true,synced:true}]}));
  assert.match(text(win, '.mixed-sync'), /已与 Mac 同步/);
});

test('runtime restoring phase disables all edit controls and finishes with fallback on partial outcome', async t => {
  const mixed=mixedState(); mixed.transition={phase:'restoring',started_ms:10,outcome:'ok',reasons:[]};
  const {win,subscriptions}=await client(t,{state:state(mixed)});
  assert.match(text(win,'#mode-bar'), /正在恢复同屏/);
  assert.equal(win.document.querySelector('#mode-action').disabled,true);
  assert.equal(win.document.querySelector('#mixed-toggle').disabled,true);
  assert.ok([...win.document.querySelectorAll('.assign-select')].every(el=>el.disabled));
  mixed.transition={phase:'idle',outcome:'partial',reasons:[]};
  subscriptions.get('state')(state(mixed));
  assert.match(text(win,'#mode-bar'), /下次按切换器时恢复/);
  assert.equal(win.document.querySelector('#mixed-toggle').disabled,false);
});

test('saving assignment while off calls only mixed.assign, preserves unplugged entries and rolls back rejection', async t => {
  const mixed=mixedState(); mixed.assignment.push({fingerprint:'unplugged',device_id:'host-mac'});
  const {win,requests}=await client(t,{state:state(mixed),request:async(method,params)=>{
    if(method==='mixed.assign') throw new Error(JSON.stringify({reasons:[{code:'missing_input'}]}));
  }});
  const select=win.document.querySelector('.assign-select');
  const old=select.value; select.value='host-linux'; select.dispatchEvent(new win.Event('change',{bubbles:true}));
  await tick();
  const request=requests.find(r=>r.method==='mixed.assign');
  assert.ok(request.params.assignment.some(a=>a.fingerprint==='unplugged'&&a.device_id==='host-mac'));
  assert.equal(requests.some(r=>r.method==='mixed.set' || r.method==='display.set'),false);
  assert.equal(win.document.querySelector('.assign-select').value,old);
  assert.match(text(win,'#mixed-notes'), /已恢复保存的分配/);
});

test('mixed.set is serialized against assignment edits and partial execution is not completion', async t => {
  const mixed=mixedState(); let finish;
  const {win,requests}=await client(t,{state:state(mixed),request:(method)=>{
    if(method==='mixed.set') return new Promise(resolve=>{finish=resolve;});
  }});
  win.document.querySelector('#mode-action').click();
  await tick();
  assert.equal(win.document.querySelector('#mixed-toggle').disabled,true);
  assert.ok([...win.document.querySelectorAll('.assign-select')].every(el=>el.disabled));
  win.document.querySelector('#mode-action').click();
  const updated={...mixed,enabled:true,revision:2,execution:[{ok:false,error:{code:'E_BACKEND',message:'failed'}}],transition:{phase:'idle',outcome:'partial'}};
  finish({saved:true,mixed:updated}); await tick();
  assert.equal(requests.filter(r=>r.method==='mixed.set').length,1);
  assert.match(text(win,'#last-copy'), /部分操作未完成/);
});

test('repeated warning notifications log only once per code and fingerprint', async t => {
  const {win,subscriptions}=await client(t);
  const warning={code:'last_local_screen',fingerprint:'a',error:{code:'E_BACKEND',message:'last screen'}};
  subscriptions.get('mixed.warning')(warning); subscriptions.get('mixed.warning')(warning);
  assert.equal([...win.document.querySelectorAll('#event-list .event')].filter(el=>el.textContent.includes('last screen')).length,1);
});

test('unknown computer assignment stays selected for repair and dynamic labels cannot inject markup', async t => {
  const config=clone(v2);
  config.local_device.monitors.forEach(m=>delete m.label);
  config.switch_group.devices.forEach(device=>device.monitors.forEach(m=>delete m.label));
  const mixed=mixedState(config);mixed.assignment[0].device_id='deleted';
  const payload='<img src=x onerror="alert(1)">';
  mixed.destinations[0].display_name=payload;
  const {win}=await client(t,{config,state:state(mixed)});
  assert.ok(text(win,'#mixed-status').includes(payload));
  assert.ok(text(win,'#mixed-screens').includes(payload));
  assert.equal(win.document.querySelector('.assign-select[data-fingerprint="display-b"]').value,'deleted');
  assert.equal(win.document.querySelectorAll('#mixed-status img').length,0);
});

test('live English language changes cover all mixed views without changing user data', async t => {
  const {win}=await client(t);
  win.KVMFlowI18n.setPreference('en');await tick();
  assert.match(text(win,'#mode-bar'),/Normal mode/);
  assert.match(text(win,'#mixed-description'),/When enabled/);
  assert.match(text(win,'.holder-tag'),/Keyboard and mouse here/);
  assert.doesNotMatch(text(win,'#mixed-status'),/mixed\.(mode|screen)/);
});

test('dismissed unassigned warning stays dismissed across state notifications and language changes', async t => {
  const mixed=mixedState();mixed.enabled=true;mixed.assignment=[];
  const {win,subscriptions}=await client(t,{state:state(mixed)});
  const button=win.document.querySelector('[data-mixed-dismiss]');
  const key=button.dataset.mixedDismiss;button.click();
  subscriptions.get('state')(state(mixed));
  win.KVMFlowI18n.setPreference('en');await tick();
  assert.ok(![...win.document.querySelectorAll('[data-mixed-dismiss]')].some(el=>el.dataset.mixedDismiss===key));
});

test('repeated state snapshots preserve dropdown focus and LAN pairing entry needs pairing_available', async t => {
  const mixed = mixedState();
  const {win,subscriptions} = await client(t,{state:state(mixed)});
  win.navigate('settings');
  const select=win.document.querySelector('.assign-select');select.focus();
  subscriptions.get('state')(state(mixed));
  assert.equal(win.document.activeElement,select);
  assert.equal(win.document.querySelector('.computer-peer button'),null);
  let requested;
  win.kvmflow.lan={openPairing:async id=>{requested=id;}};
  subscriptions.get('state')(state(mixed,{peers:[{device_id:'host-mac',online:false,pairing_available:true}]}));
  const button=win.document.querySelector('.computer-peer button');assert.ok(button);
  assert.equal(button.closest('.computer-select'),null);
  button.click();await tick();assert.equal(requested,'host-mac');
  // Without the host bridge the in-app pairing flow takes over (no dial yet:
  // the discovered peer has no address).
  delete win.kvmflow.lan;
  button.click();await tick();
});

test('runtime numbering localizes generated display names and unavailable built-in reason is not a definite lid claim', async t => {
  const config=clone(v2);config.local_device.monitors.forEach(m=>delete m.label);config.switch_group.devices.forEach(d=>d.monitors.forEach(m=>delete m.label));
  const mixed=mixedState(config);mixed.destinations[0].default_display_number=4;mixed.destinations[0].display_name='显示器 4';mixed.destinations[0].label='';
  mixed.destinations.push({fingerprint:'builtin',builtin:true,available:false,unavailable_reason:'builtin_closed_or_unavailable'});
  const {win}=await client(t,{language:'en',config,state:state(mixed)});
  assert.match(text(win,'#screen-row'),/Monitor 4/);
  assert.doesNotMatch(text(win,'#screen-row'),/显示器 4/);
  assert.match(text(win,'#mixed-screens'),/closed or unavailable/);
});

test('later mixed revisions win over a stale request response', async t => {
  const mixed=mixedState();
  const {win,subscriptions}=await client(t,{state:state(mixed)});
  subscriptions.get('mixed.changed')({mixed:{...mixed,enabled:true,revision:9}});
  subscriptions.get('mixed.changed')({mixed:{...mixed,enabled:false,revision:8}});
  assert.equal(win.KVMFlowMixed.mixed.enabled,true);
});

test('equal revisions compare section timestamps/authors before accepting the state', async t => {
  const mixed=mixedState();mixed.section_meta={updated_at_ms:100,updated_by:'mac'};
  const {win,subscriptions}=await client(t,{state:state(mixed)});
  subscriptions.get('mixed.changed')({mixed:{...mixed,enabled:true,section_meta:{updated_at_ms:100,updated_by:'win'}}});
  assert.equal(win.KVMFlowMixed.mixed.enabled,true);
  subscriptions.get('mixed.changed')({mixed:{...mixed,enabled:false,section_meta:{updated_at_ms:100,updated_by:'mac'}}});
  assert.equal(win.KVMFlowMixed.mixed.enabled,true);
  subscriptions.get('mixed.changed')({mixed:{...mixed,enabled:false,section_meta:{updated_at_ms:101,updated_by:'mac'}}});
  assert.equal(win.KVMFlowMixed.mixed.enabled,false);
});

test('startup displays the last switch target without fabricating an event timestamp or picture confirmation', async t => {
  const initial=state(mixedState());
  initial.lastMultiDeviceReport={target_device_id:'host-mac',per_monitor:[{commanded:true}]};
  const {win}=await client(t,{state:initial});
  assert.match(text(win,'#last-copy'),/已向 Mac 发送显示器切换命令/);
  assert.doesNotMatch(text(win,'#last-copy'),/\d\d:\d\d|已切换|已确认/);
});

test('pending-remote notices cannot resurrect work superseded by a newer section version or mode', async t => {
  const mixed=mixedState();mixed.revision=3;mixed.section_meta={updated_at_ms:100,updated_by:'win'};
  const {win,subscriptions}=await client(t,{state:state(mixed)});
  const notify=data=>subscriptions.get('mixed.pending_remote')({group_id:v2.switch_group.group_id,enabled:false,revision:3,section_meta:mixed.section_meta,pending_remote:[{device_id:'host-mac'}],...data});
  notify({revision:2});assert.equal(win.KVMFlowMixed.mixed.pending_remote.length,0);
  notify({enabled:true});assert.equal(win.KVMFlowMixed.mixed.pending_remote.length,0);
  notify({section_meta:{updated_at_ms:99,updated_by:'win'}});assert.equal(win.KVMFlowMixed.mixed.pending_remote.length,0);
  notify({});assert.equal(win.KVMFlowMixed.mixed.pending_remote.length,1);
});
