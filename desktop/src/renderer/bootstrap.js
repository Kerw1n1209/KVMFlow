// Load classic scripts in order to preserve the shared renderer model.
(async () => {
  const load = source => new Promise((resolve, reject) => {
    const script = document.createElement('script');
    script.src = source;
    script.onload = resolve;
    script.onerror = () => reject(new Error(`Unable to load ${source}`));
    document.body.appendChild(script);
  });
  await window.KVMFlowI18n.ready;
  await load('bridge.js');
  const info = await window.kvmflow.info;
  window.KVMFlowI18n.setPreference(info.languagePreference ?? window.KVMFlowI18n.preference, { notify: false });
  let languageSaveFailed = false;
  try { await window.kvmflow.language.set(window.KVMFlowI18n.preference, window.KVMFlowI18n.systemLocale()); }
  catch { languageSaveFailed = true; }
  for (const source of ['shell.js', 'app.js', 'updates.js']) await load(source);
  if (languageSaveFailed) document.getElementById('language-save-status').textContent = window.KVMFlowI18n.t('language.failed');
})().catch(error => {
  console.error('KVMFlow startup failed', error);
  // Keep a visible error instead of leaving an apparently usable empty shell.
  const status = document.getElementById('nav-state');
  status.textContent = window.KVMFlowI18n.t('background.component.unavailable');
  status.setAttribute('role', 'alert');
});
