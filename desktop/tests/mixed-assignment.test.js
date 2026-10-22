const test = require('node:test');
const assert = require('node:assert/strict');
const { client, v2, tick, mixedState } = require('./client-helper');
const clone = value => structuredClone(value);
const state = mixed => ({state:'idle',enabled:true,mixed});
const text = (win, selector) => win.document.querySelector(selector).textContent;

test('assigned monitors offer computer choices only; an unassigned placeholder cannot be selected', async t => {
  const mixed=mixedState();mixed.assignment.pop();
  const {win,requests}=await client(t,{state:state(mixed)});
  const assigned=win.document.querySelector('.assign-select[data-fingerprint="display-b"]');
  assert.equal(assigned.querySelector('option[value=""]'),null);
  const unassigned=win.document.querySelector('.assign-select[data-fingerprint="display-a"]');
  assert.equal(unassigned.value,'');
  assert.equal(unassigned.querySelector('option[value=""]').disabled,true);
  assigned.value='';assigned.dispatchEvent(new win.Event('change',{bubbles:true}));await tick();
  assert.equal(requests.filter(r=>r.method==='mixed.assign').length,0);
});
