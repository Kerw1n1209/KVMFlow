const fs = require('node:fs');
const path = require('node:path');
const renderer = path.resolve(__dirname, '../src/renderer');
const config = JSON.parse(fs.readFileSync(path.join(renderer, 'i18n/messages.json'), 'utf8'));
async function initializeI18n(win, preference = 'zh-CN') {
  win.KVMFLOW_I18N_CONFIG = structuredClone(config);
  win.eval(fs.readFileSync(path.join(renderer, 'i18n.js'), 'utf8'));
  await win.KVMFlowI18n.ready;
  win.KVMFlowI18n.setPreference(preference, { persist: false, notify: false });
}
module.exports = { config, initializeI18n };
