(() => {
  const S = window.kvmflow;
  const t = (id, params) => window.KVMFlowI18n.t(id, params);
  const byId = id => document.getElementById(id);
  const dialog = byId('compatibility-dialog');
  const preview = byId('compatibility-report');
  const status = byId('feedback-status');
  const entry = byId('submit-device');
  const message = byId('feedback-message');
  const contact = byId('feedback-contact');
  const include = byId('feedback-diagnostics');
  const send = byId('feedback-send');
  const copy = byId('copy-compatibility');
  const github = byId('open-compatibility');
  const cancel = byId('cancel-compatibility');
  const reportAPI = window.KVMFlowCompatReport;
  const bytes = text => new TextEncoder().encode(text).length;
  let report = null, diagnostics = null, payload = null;
  let pending = false, collecting = false, collectionError = '', receipt = null, failed = false;
  let collectionSequence = 0, opening = false;
  const formControls = [...dialog.querySelectorAll('input, textarea')];
  const selectedKind = () => dialog.querySelector('input[name="feedback-kind"]:checked')?.value || '';
  const cleanText = value => value.replace(/[\x00-\x08\x0B\x0C\x0E-\x1F\x7F-\x9F]/g, ' ');
  const validDraft = () => selectedKind() && Array.from(cleanText(message.value)).length <= 2000 && Array.from(cleanText(contact.value)).length <= 200;
  function render() {
    if (report) payload = { schema: 1, kind: selectedKind(), message: cleanText(message.value), contact: cleanText(contact.value), report, diagnostics: include.checked ? diagnostics : null };
    preview.textContent = payload ? JSON.stringify(payload, null, 2) : '';
    const tooLarge = payload && bytes(JSON.stringify(payload)) > 64 * 1024;
    send.disabled = pending || collecting || Boolean(collectionError) || !report || !validDraft() || tooLarge;
    send.textContent = t(pending ? 'feedback.sending' : failed ? 'feedback.retry' : 'feedback.send');
    formControls.forEach(control => { control.disabled = pending; });
    github.disabled = pending || collecting || !report;
    copy.disabled = pending || collecting;
    cancel.disabled = pending;
    cancel.textContent = t(receipt ? 'feedback.done' : 'cancel');
    byId('feedback-form').classList.toggle('hidden', Boolean(receipt));
    byId('feedback-success').classList.toggle('hidden', !receipt);
    send.classList.toggle('hidden', Boolean(receipt));
    copy.classList.toggle('hidden', !failed || Boolean(receipt));
    github.classList.toggle('hidden', Boolean(receipt));
    if (tooLarge) status.textContent = t('feedback.large');
  }
  function setError(key) { status.textContent = t(key); }
  async function open() {
    if (opening || dialog.open) return;
    opening = true; entry.disabled = true;
    try {
      const [host, config, usb, state, displays] = await Promise.all([
        S.hostInfo ? S.hostInfo() : S.info, S.request('config.get'), S.request('usb.snapshot'), S.request('state.get'),
        S.request('display.list').catch(() => ({ displays: S.cachedDisplays })),
      ]);
      report = reportAPI.buildReport({ host, config: config || {}, usb: usb?.devices, displays: displays?.displays || S.cachedDisplays, state: state || {} });
      if (receipt) {
        receipt = null; failed = false; status.textContent = '';
      }
      render(); dialog.showModal();
      dialog.querySelector('input[name="feedback-kind"]').focus();
    } catch { byId('compatibility-status').textContent = t('compat.prepare.failed'); }
    finally { opening = false; entry.disabled = false; }
  }
  entry.addEventListener('click', open);
  byId('feedback-preview').addEventListener('toggle', () => {
    if (byId('feedback-preview').open) preview.scrollIntoView?.({ block: 'nearest' });
  });
  for (const control of [...dialog.querySelectorAll('[name="feedback-kind"]'), message, contact]) {
    control.addEventListener('input', () => { if (!pending) { status.textContent = ''; render(); } });
  }
  include.addEventListener('change', async () => {
    const sequence = ++collectionSequence;
    diagnostics = null; collectionError = ''; collecting = include.checked;
    status.textContent = collecting ? t('feedback.collecting') : '';
    render();
    if (!include.checked) return;
    try {
      const redacted = await S.collectRedactedDiagnostics({ requireRuntime: true });
      if (sequence !== collectionSequence) return;
      const text = JSON.stringify(redacted, null, 2);
      if (bytes(text) > 48 * 1024) collectionError = 'feedback.large';
      else diagnostics = text;
    } catch { if (sequence === collectionSequence) collectionError = 'feedback.diagnostics.failed'; }
    finally {
      if (sequence === collectionSequence) {
        collecting = false; status.textContent = collectionError ? t(collectionError) : ''; render();
      }
    }
  });
  send.addEventListener('click', async () => {
    if (send.disabled || pending) return;
    const outgoing = structuredClone(payload);
    pending = true; failed = false; status.textContent = ''; render();
    try {
      const result = await S.sendFeedback(outgoing);
      if (result?.ok !== true || !/^fb_[a-z0-9]{26}$/i.test(result?.id || '')) throw { code: 'invalid_response' };
      receipt = result.id; byId('feedback-id').textContent = receipt;
    } catch (error) {
      failed = true;
      setError(({ rate_limited: 'feedback.rate', too_large: 'feedback.large', invalid_feedback: 'feedback.invalid' })[error?.code] || 'feedback.failed');
    } finally { pending = false; render(); }
  });
  const writeClipboard = async text => {
    try { await navigator.clipboard.writeText(text); return true; }
    catch { setError('compat.copy.failed'); return false; }
  };
  copy.addEventListener('click', async () => { if (await writeClipboard(JSON.stringify(payload, null, 2))) setError('compat.copied'); });
  byId('feedback-id').addEventListener('click', async () => { if (receipt && await writeClipboard(receipt)) setError('feedback.id.copied'); });
  github.addEventListener('click', async () => {
    if (github.disabled) return;
    // Contact and optional private diagnostics never enter the public issue.
    const kind = selectedKind();
    const text = reportAPI.renderReport(report, t) + (kind ? `\n\n## ${t(kind === 'works' ? 'feedback.works' : 'feedback.problem')}` : '') + (payload.message ? `\n\n${payload.message}` : '');
    const target = reportAPI.submission(text);
    github.disabled = true;
    try {
      if (target.copyFirst && !await writeClipboard(text)) return;
      await S.openCompatibilityIssue(target.url);
      if (target.copyFirst) setError('compat.paste');
    } catch { setError('compat.open.failed'); }
    finally { render(); }
  });
  byId('feedback-privacy').addEventListener('click', async () => {
    try { await S.openPrivacy(); } catch { setError('compat.open.failed'); }
  });
  cancel.addEventListener('click', () => dialog.close());
  dialog.addEventListener('cancel', event => { if (pending) event.preventDefault(); });
  dialog.addEventListener('close', () => entry.focus());
  S.onNotification('client.hidden', () => { if (dialog.open && !pending) dialog.close(); });
  window.KVMFlowFeedback = { open };
})();
