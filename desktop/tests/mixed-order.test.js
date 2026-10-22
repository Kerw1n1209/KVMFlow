const test = require('node:test');
const assert = require('node:assert/strict');
const { client, v2, tick, mixedState } = require('./client-helper');
const clone = value => structuredClone(value);
const state = mixed => ({state:'idle',enabled:true,mixed});
const text = (win, selector) => win.document.querySelector(selector).textContent;

test('delayed startup snapshot cannot overwrite newer holder, pause flag or display evidence', async t => {
  let finish;
  const older={...state(mixedState()),runtime_seq:10};
  const {win,subscriptions}=await client(t,{request:method=>method==='state.get'?new Promise(resolve=>{finish=resolve;}):undefined});
  const newer=clone(older);newer.runtime_seq=20;newer.enabled=false;
  newer.mixed.keyboard_holder_device_id='host-mac';
  Object.assign(newer.mixed.destinations[0],{current_device_id:null,target_device_id:'host-mac',evidence:'commanded_unconfirmed'});
  subscriptions.get('state')(newer);
  assert.equal(text(win,'.desk-computer.holder strong'),'Mac');
  finish(older);await tick();
  assert.equal(text(win,'.desk-computer.holder strong'),'Mac');
  assert.equal(win.__kvmflowRuntimeState,'disabled');
  assert.equal(win.KVMFlowMixed.mixed.destinations[0].evidence,'commanded_unconfirmed');
});

test('delayed mixed response and pending notice cannot replace later runtime observations', async t => {
  const mixed=mixedState();let finish;
  const {win,subscriptions}=await client(t,{state:{...state(mixed),runtime_seq:1},request:method=>method==='mixed.set'?new Promise(resolve=>{finish=resolve;}):undefined});
  const operation=win.KVMFlowMixed.setEnabled(true);await tick();
  const latest={...mixed,enabled:false,keyboard_holder_device_id:'host-mac'};
  subscriptions.get('state')({...state(latest),runtime_seq:30});
  finish({saved:true,mixed:{...mixed,enabled:true},runtime_seq:20});await operation;
  assert.equal(win.KVMFlowMixed.mixed.enabled,false);
  assert.equal(win.KVMFlowMixed.mixed.keyboard_holder_device_id,'host-mac');
  assert.doesNotMatch(text(win,'#last-copy'),/已开启混屏/);
  subscriptions.get('mixed.pending_remote')({group_id:v2.switch_group.group_id,enabled:false,revision:mixed.revision,pending_remote:[{device_id:'host-mac'}],runtime_seq:10});
  assert.equal(win.KVMFlowMixed.mixed.pending_remote.length,0);
  subscriptions.get('state')({...state(mixed),runtime_seq:31});
  assert.equal(win.KVMFlowMixed.mixed.keyboard_holder_device_id,'host-win');
});

test('partial mixed response does not hide a delayed pause snapshot', async t => {
  const mixed=mixedState();let finish;
  const {win,subscriptions}=await client(t,{state:{...state(mixed),runtime_seq:1},request:method=>method==='mixed.set'?new Promise(resolve=>{finish=resolve;}):undefined});
  const operation=win.KVMFlowMixed.setEnabled(true);await tick();
  finish({runtime_seq:5,mixed:{...mixed,enabled:true,keyboard_holder_device_id:'host-mac'}});await operation;
  subscriptions.get('state')({...state(mixed),enabled:false,runtime_seq:4});
  assert.equal(win.__kvmflowRuntimeState,'disabled');
  assert.equal(win.KVMFlowMixed.mixed.enabled,true);
  assert.equal(win.KVMFlowMixed.mixed.keyboard_holder_device_id,'host-mac');
});

test('filling older mixed data cannot undo a more recent pause field', async t => {
  const mixed=mixedState();
  const {win,subscriptions}=await client(t,{state:{...state(mixed),runtime_seq:1}});
  subscriptions.get('state')({runtime_seq:6,state:'disabled',enabled:false});
  subscriptions.get('state')({...state({...mixed,enabled:true}),runtime_seq:5});
  assert.equal(win.KVMFlowMixed.mixed.enabled,true);
  assert.equal(win.__kvmflowRuntimeState,'disabled');
});

test('new worker generation resets field sequences and rejects older worker responses', async t => {
  const mixed=mixedState();
  const {win,subscriptions}=await client(t,{state:{...state(mixed),runtime_seq:90}});
  subscriptions.get('mixed.changed')({runtime_generation:1,runtime_seq:2,mixed:{...mixed,enabled:true,keyboard_holder_device_id:'host-mac'}});
  assert.equal(win.KVMFlowMixed.mixed.enabled,true);
  subscriptions.get('state')({...state(mixed),enabled:false,runtime_generation:1,runtime_seq:1});
  assert.equal(win.__kvmflowRuntimeState,'disabled');
  assert.equal(win.KVMFlowMixed.mixed.keyboard_holder_device_id,'host-mac');
  subscriptions.get('state')({...state(mixed),runtime_generation:0,runtime_seq:999,lan:{peers:[]}});
  subscriptions.get('mixed.changed')({runtime_seq:1000,mixed});
  assert.equal(win.__kvmflowRuntimeState,'disabled');
  assert.equal(win.KVMFlowMixed.mixed.enabled,true);
  assert.equal(win.KVMFlowMixed.mixed.keyboard_holder_device_id,'host-mac');
});

test('nested generation and pending notices keep the new worker sequence domain', async t => {
  const mixed=mixedState();
  const {win,subscriptions}=await client(t,{state:{...state(mixed),runtime_generation:3,runtime_seq:90}});
  subscriptions.get('mixed.changed')({mixed:{...mixed,runtime_generation:4,runtime_seq:1}});
  assert.equal(win.KVMFlowMixed.mixed.runtime_generation,4);
  const pending={group_id:v2.switch_group.group_id,enabled:mixed.enabled,revision:mixed.revision,pending_remote:[{device_id:'host-mac'}],runtime_generation:4,runtime_seq:2};
  subscriptions.get('mixed.pending_remote')(pending);
  assert.equal(win.KVMFlowMixed.mixed.pending_remote.length,1);
  subscriptions.get('mixed.pending_remote')({...pending,runtime_generation:3,runtime_seq:999,pending_remote:[]});
  assert.equal(win.KVMFlowMixed.mixed.pending_remote.length,1);
});

test('runtime error events remain visible without sequence and respect worker generation', async t => {
  const mixed=mixedState();
  const {win,subscriptions}=await client(t,{state:{...state(mixed),runtime_seq:5}});
  subscriptions.get('runtime.error')({message:'worker failed'});
  assert.equal(win.__kvmflowRuntimeState,'error');
  subscriptions.get('state')({...state(mixed),runtime_seq:4});
  assert.equal(win.__kvmflowRuntimeState,'error');
  subscriptions.get('state')({...state(mixed),runtime_generation:1,runtime_seq:1});
  subscriptions.get('runtime.error')({message:'old worker failed',runtime_generation:0});
  assert.equal(win.__kvmflowRuntimeState,'idle');
  subscriptions.get('runtime.error')({message:'new worker failed',runtime_generation:1});
  assert.equal(win.__kvmflowRuntimeState,'error');
});
