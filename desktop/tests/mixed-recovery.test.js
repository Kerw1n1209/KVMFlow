const test = require('node:test');
const assert = require('node:assert/strict');
const { client, v2, tick, mixedState } = require('./client-helper');
const clone = value => structuredClone(value);
const state = mixed => ({state:'idle',enabled:true,mixed});
const text = (win, selector) => win.document.querySelector(selector).textContent;

test('successful authoritative restore clears an earlier close request error while retaining diagnostics', async t => {
  const mixed=mixedState();mixed.enabled=true;
  const {win,subscriptions}=await client(t,{state:state(mixed),request:async method=>{
    if(method==='mixed.set')throw new Error('request timed out');
  }});
  await win.KVMFlowMixed.setEnabled(false);
  assert.match(text(win,'#mixed-notes'),/request timed out/);
  subscriptions.get('state')({...state({...mixed,enabled:false,transition:{phase:'idle',outcome:'partial',started_ms:2},pending_remote:[{device_id:'host-mac'}]}),runtime_seq:20});
  assert.match(text(win,'#mode-bar'),/下次按切换器时恢复/);
  assert.match(text(win,'#mixed-notes'),/request timed out/);
  subscriptions.get('state')({...state({...mixed,enabled:false,transition:{phase:'idle',outcome:'ok',started_ms:3},pending_remote:[],execution:[]}),runtime_seq:21});
  assert.match(text(win,'#mode-bar'),/按切换器时，键鼠和全部显示器一起切换/);
  assert.doesNotMatch(text(win,'#mode-bar'),/下次按切换器时恢复/);
  assert.doesNotMatch(text(win,'#mixed-notes'),/request timed out/);
  assert.match(text(win,'#event-list'),/request timed out/);
});
