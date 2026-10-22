const test = require('node:test');
const assert = require('node:assert/strict');
const { client, v2, tick, mixedState } = require('./client-helper');
const clone = value => structuredClone(value);
const state = mixed => ({state:'idle',enabled:true,mixed});
const text = (win, selector) => win.document.querySelector(selector).textContent;

test('warnings can notify again after their active condition resolves', async t => {
  const warning={code:'last_local_screen',fingerprint:'a',severity:'warning'};
  const mixed=mixedState();mixed.warnings=[warning];
  const {win,subscriptions}=await client(t,{state:state(mixed)});
  subscriptions.get('mixed.warning')(warning);subscriptions.get('mixed.warning')(warning);
  const before=[...win.document.querySelectorAll('#event-list .event')].filter(e=>e.textContent.includes('last_local_screen')).length;
  assert.equal(before,1);
  subscriptions.get('state')(state({...mixed,warnings:[]}));
  subscriptions.get('mixed.warning')(warning);
  const after=[...win.document.querySelectorAll('#event-list .event')].filter(e=>e.textContent.includes('last_local_screen')).length;
  assert.equal(after,2);
});
