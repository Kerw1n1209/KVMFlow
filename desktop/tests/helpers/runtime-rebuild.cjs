const assert = require('node:assert/strict');
const fs = require('node:fs');
const { client } = require('../client-helper');
(async () => {
  const payload = JSON.parse(fs.readFileSync(0, 'utf8'));
  let close;
  const { win, subscriptions } = await client({ after(fn) { close = fn; } }, {
    config: payload.config, state: payload.old,
  });
  try {
    assert.equal(win.KVMFlowMixed.mixed.enabled, false);
    subscriptions.get('mixed.changed')(payload.changed.data);
    assert.equal(win.KVMFlowMixed.mixed.enabled, true, 'first new-worker notification is accepted immediately');
    assert.equal(win.document.querySelector('#mixed-toggle').checked, true);
    subscriptions.get('state')(payload.fresh.data);
    subscriptions.get('state')(payload.old);
    subscriptions.get('runtime.error')(payload.error.data);
    assert.equal(win.KVMFlowMixed.markError(payload.error.data), false, 'late old-worker error is rejected');
    assert.equal(win.KVMFlowMixed.mixed.enabled, true, 'late old state and error cannot replace new worker');
    assert.equal(win.document.querySelector('#mixed-toggle').checked, true);
  } finally { close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
