(() => {
  const api = window.kvmflow?.updates;
  if (!api) return;
  const toolbar = document.getElementById('update-toolbar');
  const action = document.getElementById('update-action');
  const status = document.getElementById('update-status');
  const section = document.createElement('section');
  section.className = 'panel update-settings';
  section.innerHTML = '<div class="panel-title"><div><h2>应用更新</h2><p id="update-version" class="hint"></p></div><button id="check-update" class="secondary">检查更新</button></div><p id="update-detail" class="hint" role="status" aria-live="polite"></p><details id="update-notes" class="hidden"><summary>更新说明</summary><p></p></details>';
  document.getElementById('settings-page').append(section);
  const checkButton = section.querySelector('#check-update');
  const detail = section.querySelector('#update-detail');
  const notes = section.querySelector('#update-notes');
  let update = null;
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
    status.textContent = message;
    detail.textContent = message;
    detail.setAttribute('aria-live', phase === 'error' ? 'assertive' : 'polite');
    action.disabled = busy();
    checkButton.disabled = busy() || Boolean(update);
    checkButton.textContent = phase === 'checking' ? '正在检查' : update ? '已发现更新' : '检查更新';
    action.textContent = ({
      downloading: '正在下载', ready: '重启并更新',
      installing: '正在安装', error: '重试更新',
    })[phase] || `更新至 ${update?.version || ''}`;
    notes.classList.toggle('hidden', !update?.notes);
    notes.querySelector('p').textContent = update?.notes || '';
  }
  async function check(manual = false) {
    if (busy() || update) return;
    phase = 'checking';
    render(manual ? '正在检查更新。' : '');
    try {
      update = await api.check();
      phase = update?.downloaded ? 'ready' : update ? 'available' : 'idle';
      render(update ? `有新版本 ${update.version}。` : manual ? '当前已是最新版本。' : '');
    } catch {
      // A failed background check should not interrupt normal use.
      phase = manual ? 'error' : 'idle';
      retry = 'check';
      render('无法检查更新。请检查网络后重试。');
    }
  }
  async function perform() {
    if (busy()) return;
    if (!update) return check(true);
    if (phase === 'ready' || (phase === 'error' && retry === 'install')) {
      if (hasUnsavedSettings()) {
        render('设置尚未保存。请先保存设置，再重启并更新。');
        return;
      }
      phase = 'installing';
      render('正在安装更新，完成后将重启应用。');
      try { await api.install(); }
      catch (error) {
        phase = 'error'; retry = 'install';
        render(String(error?.message || error).includes('自动切换未恢复')
          ? '更新失败，自动切换未恢复。请重启应用后重试。'
          : '无法安装更新。请重试或从官网下载最新版。');
      }
      return;
    }
    phase = 'downloading';
    render('正在下载更新，自动切换继续运行。');
    try {
      await api.download();
      phase = 'ready';
      render('更新包已验证。请先保存设置，再重启并更新。');
    } catch {
      phase = 'error'; retry = 'download';
      render('下载或验证失败。请检查网络后重试。');
    }
  }
  action.addEventListener('click', perform);
  checkButton.addEventListener('click', () => check(true));
  api.onProgress(({ percent }) => {
    if (phase !== 'downloading') return;
    render(percent == null ? '正在下载更新。' : percent >= 100 ? '正在验证更新包。' : `正在下载更新，${percent}%。`);
  });
  window.kvmflow.info.then(info => {
    section.querySelector('#update-version').textContent = `当前版本 ${info.version}`;
  });
  window.kvmflow.ready.then(() => {
    check();
    const timer = setInterval(() => check(), 6 * 60 * 60 * 1000);
    window.addEventListener('beforeunload', () => clearInterval(timer), { once: true });
  });
})();
