const test=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const {client,v2,mixedState}=require('./client-helper');
const fixture=()=>{
  const config=structuredClone(v2);config.local_device.monitors.splice(1);
  config.switch_group.devices.forEach(device=>device.monitors=device.monitors.filter(m=>m.fingerprint==='display-b'));
  const mixed=mixedState(config);mixed.destinations.push({fingerprint:'builtin',builtin:true,available:true,connected:true});
  return {config,state:{state:'idle',enabled:true,mixed}};
};
test('E2 explains the closed-lid limitation before enabling, in both languages',async t=>{
  for(const language of ['zh-CN','en']){
    const {win}=await client(t,{...fixture(),language});
    assert.equal(win.document.querySelector('#mixed-toggle').checked,false);
    assert.equal(win.document.querySelector('#mixed-toggle').disabled,false);
    assert.match(win.document.querySelector('#mixed-notes').textContent,language==='zh-CN'?/合盖使用时没有内屏，无法混屏/ : /lid is closed.*mixed mode/i);
  }
});
test('built-in panels use a 200 by 6 base; external monitors keep the 44 by 4 stand',async t=>{
  const {win}=await client(t,fixture());
  const sheet=win.document.createElement('style');sheet.textContent=fs.readFileSync(path.join(__dirname,'../src/renderer/mixed.css'),'utf8');win.document.head.append(sheet);
  const builtin=win.document.querySelector('.desk-screen.builtin');
  const base=win.getComputedStyle(builtin.nextElementSibling);
  assert.equal(win.getComputedStyle(builtin).width,'180px');
  assert.equal(win.getComputedStyle(builtin).height,'112px');
  assert.equal(base.width,'200px');assert.equal(base.height,'6px');
  const external=win.getComputedStyle(win.document.querySelector('.desk-screen:not(.builtin)').nextElementSibling);
  assert.equal(external.width,'44px');assert.equal(external.height,'4px');
});
