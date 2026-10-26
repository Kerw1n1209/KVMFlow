(() => {
  const S = window.kvmflow;
  const t = (id, params) => window.KVMFlowI18n.t(id, params);
  const byId = id => document.getElementById(id);
  const dialog = byId('compatibility-dialog');
  const preview = byId('compatibility-report');
  const status = byId('compatibility-status');
  const entry = byId('submit-device');
  const copy = byId('copy-compatibility');
  const open = byId('open-compatibility');
  let report = '';
  let busy = false;
  const showStatus = key => { status.textContent = t(key); };
  const lock = value => { busy = value; copy.disabled = value; open.disabled = value; };
  const copyReport = () => navigator.clipboard.writeText(report);
  entry.addEventListener('click', async () => {
    if (entry.disabled) return;
    entry.disabled = true;
    status.textContent = '';
    copy.textContent = t('compat.copy');
    try {
      // These requests only read memory/configuration. display.list would probe
      // DDC; use its previous response, if any, instead of probing to report.
      const [host, config, usb, state] = await Promise.all([
        S.info, S.request('config.get'), S.request('usb.snapshot'), S.request('state.get'),
      ]);
      report = window.KVMFlowCompatReport.buildReport({ host, config: config || {}, usb: usb?.devices, displays: S.cachedDisplays, state }, t);
      preview.textContent = report;
      dialog.showModal();
    } catch { showStatus('compat.prepare.failed'); }
    finally { entry.disabled = false; }
  });
  copy.addEventListener('click', async () => {
    if (busy) return;
    lock(true);
    try { await copyReport(); copy.textContent = t('compat.copied'); }
    catch { copy.textContent = t('compat.copy.failed'); }
    finally { lock(false); }
  });
  open.addEventListener('click', async () => {
    if (busy) return;
    lock(true);
    const { url, copyFirst } = window.KVMFlowCompatReport.submission(report);
    try {
      if (copyFirst) {
        try { await copyReport(); }
        catch { copy.textContent = t('compat.copy.failed'); return; }
      }
      await S.openCompatibilityIssue(url);
      dialog.close();
      if (copyFirst) showStatus('compat.paste');
    } catch { open.textContent = t('compat.open.failed'); }
    finally { lock(false); }
  });
  byId('cancel-compatibility').addEventListener('click', () => dialog.close());
  dialog.addEventListener('close', () => { open.textContent = t('compat.open'); entry.focus(); });
  S.onNotification('client.hidden', () => { if (dialog.open) dialog.close(); });
})();
