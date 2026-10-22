(() => {
  const t = (id, params) => window.KVMFlowI18n.t(id, params);
  const api = window.kvmflow?.updates;
  if (!api) return;
  const toolbar = document.getElementById('update-toolbar');
  const action = document.getElementById('update-action');
  const status = document.getElementById('update-status');
  const section = document.createElement('section');
  section.className = 'panel update-settings';
  section.innerHTML = `<div class="panel-title"><div><h2>${t("app.updates")}</h2><p id="update-version" class="hint"></p></div><button id="check-update" class="secondary">${t("check.for.updates")}</button></div><div class="response-row"><div class="response-copy"><label id="receive-beta-label" for="receive-beta">${t("receive.beta.updates")}</label></div><label class="toggle-control" for="receive-beta"><input id="receive-beta" type="checkbox" aria-labelledby="receive-beta-label" disabled><span class="toggle-track" aria-hidden="true"></span></label></div><p id="update-detail" class="hint" role="status" aria-live="polite"></p><details id="update-notes" class="hidden" open><summary>${t("release.notes")}</summary><p></p></details>`;
  document.getElementById('settings-page').append(section);
  const recoveryActions = document.createElement('div');
  recoveryActions.id = 'update-recovery-actions';
  recoveryActions.className = 'hidden';
  recoveryActions.innerHTML = `<button id="update-retry-recovery" class="secondary"></button> <a id="update-manual-download" href="https://github.com/Kerw1n1209/KVMFlow/releases" target="_blank" rel="noopener noreferrer"></a> <button id="update-install-anyway" class="secondary"></button>`;
  section.append(recoveryActions);
  const retryRecovery = recoveryActions.querySelector('#update-retry-recovery');
  const manualDownload = recoveryActions.querySelector('#update-manual-download');
  const installAnyway = recoveryActions.querySelector('#update-install-anyway');
  const checkButton = section.querySelector('#check-update');
  const betaControl = section.querySelector('#receive-beta');
  let receiveBeta = false;
  let preferencesLoaded = false;
  const detail = section.querySelector('#update-detail');
  const notes = section.querySelector('#update-notes');
  let update = null;
  let lastMessage = '';
  let appVersion = '';
  const message = (id, params = {}) => ({ id, params });
  let phase = 'idle';
  let recoveryFailed = false;
  let retry = 'check';
  let structureRevision = window.computerSettingsStructureRevision?.() ?? 0;
  let savedStructureRevision = structureRevision;
  window.addEventListener('computer-settings-structure-changed', event => {
    structureRevision = event.detail?.revision ?? structureRevision + 1;
  });
  window.addEventListener('settings-saved', event => {
    savedStructureRevision = event.detail?.structureRevision ?? structureRevision;
  });
  const hasUnsavedSettings = () =>
    structureRevision !== savedStructureRevision || window.hasUnsavedComputerEditor?.() === true;
  const busy = () => ['checking', 'downloading', 'installing', 'saving-channel'].includes(phase);
  function render(message = '') {
    toolbar.classList.toggle('hidden', !update && phase !== 'error');
    lastMessage = message;
    const text = typeof message === 'object' ? t(message.id, message.params) : message;
    status.textContent = text;
    detail.textContent = text;
    detail.setAttribute('aria-live', phase === 'error' ? 'assertive' : 'polite');
    recoveryActions.classList.toggle('hidden', !recoveryFailed);
    retryRecovery.textContent = t('native.recovery.retry');
    manualDownload.textContent = t('update.recovery.manualDownload');
    installAnyway.textContent = t('update.recovery.installAnyway');
    retryRecovery.disabled = busy(); installAnyway.disabled = busy();
    betaControl.disabled = busy() || !preferencesLoaded;
    action.disabled = busy();
    checkButton.disabled = busy();
    checkButton.classList.toggle('primary', Boolean(update));
    checkButton.classList.toggle('secondary', !update);
    const phaseLabel = ({
      checking: t("checking"),
      downloading: t("downloading"), ready: t("restart.and.update"),
      installing: t("installing"), error: t(recoveryFailed ? "native.recovery.retry" : "retry.update"),
    })[phase];
    checkButton.textContent = phaseLabel || t(update ? "download.update" : "check.for.updates");
    action.textContent = phaseLabel || t("update.to.value", {
  p0: update?.version || ''
});
    notes.classList.toggle('hidden', !update?.notes);
    notes.querySelector('p').textContent = update?.notes || '';
  }
  async function check(manual = false) {
    if (busy() || update) return;
    phase = 'checking';
    render(manual ? message("checking.for.updates") : '');
    try {
      update = await api.check();
      phase = update?.downloaded ? 'ready' : update ? 'available' : 'idle';
      render(update ? message("version.value.is.available", {
  p0: update.version
}) : manual ? message("you.are.up.to.date") : '');
    } catch {
      // A failed background check should not interrupt normal use.
      phase = manual ? 'error' : 'idle';
      retry = 'check';
      render(message("unable.to.check.for.updates.check.your.network.and.try.again"));
    }
  }
  async function perform(allowUnrestored = false) {
    if (busy()) return;
    if (!update) return check(true);
    if (phase === 'ready' || (phase === 'error' && retry === 'install')) {
      if (hasUnsavedSettings()) {
        render(message("settings.have.not.been.saved.save.them.before.restarting.to.update"));
        return;
      }
      phase = 'installing';
      render(message("installing.the.update.the.app.will.restart.when.complete"));
      try { await api.install(allowUnrestored === true ? { allowUnrestored: true } : undefined); }
      catch (error) {
        phase = 'error'; retry = 'install';
        const code = String(error?.message || error);
        // Keep the escape routes after any later installer failure too.
        recoveryFailed = recoveryFailed || code.includes('E_UPDATE_RESTORE:') || code.includes('E_UPDATE_CANCELLED:');
        render(recoveryFailed
          ? message(code.includes('E_UPDATE_CANCELLED:') ? 'update.recovery.cancelled' : 'update.recovery.failed')
          : message("unable.to.install.the.update.try.again.or.download.the.latest.version.fr"));
      }
      return;
    }
    phase = 'downloading';
    render(message("downloading.the.update.automatic.switching.continues"));
    try {
      await api.download();
      phase = 'ready';
      render(message("update.package.verified.save.your.settings.before.restarting.to.update"));
    } catch {
      phase = 'error'; retry = 'download';
      render(message("download.or.verification.failed.check.your.network.and.try.again"));
    }
  }
  betaControl.addEventListener('change', async () => {
    const previousPhase = phase;
    phase = 'saving-channel';
    render();
    try {
      receiveBeta = await api.setBeta(betaControl.checked);
      betaControl.checked = receiveBeta;
      update = null;
      recoveryFailed = false;
      retry = 'check';
      phase = 'idle';
      await check(true);
    } catch {
      betaControl.checked = receiveBeta;
      phase = previousPhase;
      render(message('update.channel.saveFailed'));
    }
  });
  action.addEventListener('click', () => perform());
  checkButton.addEventListener('click', () => perform());
  retryRecovery.addEventListener('click', () => perform());
  installAnyway.addEventListener('click', () => perform(true));
  manualDownload.addEventListener('click', event => {
    if (api.openDownload) { event.preventDefault(); api.openDownload().catch(() => render(message('update.recovery.downloadUrl'))); }
  });
  api.onProgress(({ percent }) => {
    if (phase !== 'downloading') return;
    render(percent == null ? message("downloading.the.update") : percent >= 100 ? message("verifying.the.update.package") : message("downloading.the.update.value", {
  p0: percent
}));
  });
  const renderVersion = () => {
    section.querySelector('#update-version').textContent = t('current.version.value', { p0: appVersion });
  };
  const preferencesReady = window.kvmflow.info.then(info => {
    appVersion = info.version;
    receiveBeta = info.receiveBeta === true;
    betaControl.checked = receiveBeta;
    preferencesLoaded = true;
    renderVersion();
    render(lastMessage);
  });
  window.addEventListener('language-changed', () => {
    section.querySelector('h2').textContent = t('app.updates');
    section.querySelector('#receive-beta-label').textContent = t('receive.beta.updates');
    notes.querySelector('summary').textContent = t('release.notes');
    renderVersion();
    render(lastMessage);
  });
  Promise.all([window.kvmflow.ready, preferencesReady]).then(() => {
    check();
    const timer = setInterval(() => check(), 6 * 60 * 60 * 1000);
    window.addEventListener('beforeunload', () => clearInterval(timer), { once: true });
  });
})();
