(() => {
  const t = (id, params) => window.KVMFlowI18n.t(id, params);
  const api = window.kvmflow?.updates;
  if (!api) return;
  const toolbar = document.getElementById('update-toolbar');
  const action = document.getElementById('update-action');
  const status = document.getElementById('update-status');
  const section = document.createElement('section');
  section.className = 'panel update-settings';
  section.innerHTML = `<div class="panel-title"><div><h2>${t("app.updates")}</h2><p id="update-version" class="hint"></p></div><button id="check-update" class="secondary">${t("check.for.updates")}</button></div><p id="update-detail" class="hint" role="status" aria-live="polite"></p><details id="update-notes" class="hidden"><summary>${t("release.notes")}</summary><p></p></details>`;
  document.getElementById('settings-page').append(section);
  const checkButton = section.querySelector('#check-update');
  const detail = section.querySelector('#update-detail');
  const notes = section.querySelector('#update-notes');
  let update = null;
  let lastMessage = '';
  let appVersion = '';
  const message = (id, params = {}) => ({ id, params });
  let phase = 'idle';
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
  const busy = () => ['checking', 'downloading', 'installing'].includes(phase);
  function render(message = '') {
    toolbar.classList.toggle('hidden', !update && phase !== 'error');
    lastMessage = message;
    const text = typeof message === 'object' ? t(message.id, message.params) : message;
    status.textContent = text;
    detail.textContent = text;
    detail.setAttribute('aria-live', phase === 'error' ? 'assertive' : 'polite');
    action.disabled = busy();
    checkButton.disabled = busy() || Boolean(update);
    checkButton.textContent = phase === 'checking' ? t("checking") : update ? t("update.available") : t("check.for.updates");
    action.textContent = ({
      downloading: t("downloading"), ready: t("restart.and.update"),
      installing: t("installing"), error: t("retry.update"),
    })[phase] || t("update.to.value", {
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
  async function perform() {
    if (busy()) return;
    if (!update) return check(true);
    if (phase === 'ready' || (phase === 'error' && retry === 'install')) {
      if (hasUnsavedSettings()) {
        render(message("settings.have.not.been.saved.save.them.before.restarting.to.update"));
        return;
      }
      phase = 'installing';
      render(message("installing.the.update.the.app.will.restart.when.complete"));
      try { await api.install(); }
      catch (error) {
        phase = 'error'; retry = 'install';
        render(String(error?.message || error).includes('E_UPDATE_RESTORE:')
          ? message("update.failed.and.automatic.switching.was.not.restored.restart.the.app.a")
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
  action.addEventListener('click', perform);
  checkButton.addEventListener('click', () => check(true));
  api.onProgress(({ percent }) => {
    if (phase !== 'downloading') return;
    render(percent == null ? message("downloading.the.update") : percent >= 100 ? message("verifying.the.update.package") : message("downloading.the.update.value", {
  p0: percent
}));
  });
  const renderVersion = () => {
    section.querySelector('#update-version').textContent = t('current.version.value', { p0: appVersion });
  };
  window.kvmflow.info.then(info => { appVersion = info.version; renderVersion(); });
  window.addEventListener('language-changed', () => {
    section.querySelector('h2').textContent = t('app.updates');
    notes.querySelector('summary').textContent = t('release.notes');
    renderVersion();
    render(lastMessage);
  });
  window.kvmflow.ready.then(() => {
    check();
    const timer = setInterval(() => check(), 6 * 60 * 60 * 1000);
    window.addEventListener('beforeunload', () => clearInterval(timer), { once: true });
  });
})();
