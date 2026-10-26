// LAN pairing UI: the pairing dialog on the primary, the "添加另一台电脑"
// wizard step with LAN discovery, and the secondary's role/waiting/done
// pages. State comes from the sidecar through lan.pair / lan.peers /
// lan.profile / lan.setup notifications (LAN_RUNTIME_PROTOCOL.md); the only
// requests issued are lan.pair.*, lan.setup.secondary, lan.profile.refresh,
// lan.test.* and lan.connect.manual. No DDC/USB command is issued here.
(() => {
  const t = (id, params) => window.KVMFlowI18n.t(id, params);
  const View = window.KVMFlowPairingView;
  const esc = (value) => escapeHtml(value ?? '');
  const byId = (id) => document.getElementById(id);
  // Design copy shows the host without the pairing port ("Windows · 192.168.1.23").
  const hostOf = (addr) => {
    const value = typeof addr === 'string' ? addr.trim() : '';
    return value.replace(/:\d+$/, '');
  };

  // ---- session state -------------------------------------------------------
  let note = null;            // latest lan.pair notification / response
  let secondaryNote = null;   // latest lan.setup notification
  let lan = null;             // latest lan object from state.get / lan.peers
  let requestError = null;    // code of the failed lan.pair.* request
  let windowOwned = false;    // we opened the inbound pairing window
  let dialPeer = null;        // discovered peer the primary is connecting to
  let resolveDraft = { mixed: null, replacement: null, aliases: {} }; // unanswered lan.pair.resolve answers
  let profileNote = null;     // latest lan.profile report (owner data)
  let peerName = '';          // display name of the pairing peer
  let tryReports = [];        // switch.report evidence during 试一下
  // Pairing wizard mode inside #guide-content. Null means the classic wizard.
  // 'add' | 'confirm' | 'try' | 'role' | 'secondary-wait' | 'secondary-done'
  let mode = null;
  let autoRole = false;       // the role page was opened by the first-run entry, not the user
  let primaryChosen = false;  // the user picked "this is the host" in this session

  let generation = 0;
  let session = null;
  const retiredSessions = new Set();
  const phaseRank = { connecting: 0, await_confirm: 1, await_peer_confirm: 2, peer_confirmed: 2,
    await_resolve: 3, ready_to_commit: 4, committing: 4, committed: 5, peer_committed: 6 };
  const terminal = () => ['committed', 'peer_committed', 'failed', 'cancelled'].includes(note?.phase);
  function resetSession(row = null) {
    if (session?.id) retiredSessions.add(session.id);
    if (retiredSessions.size > 32) retiredSessions.delete(retiredSessions.values().next().value);
    session = { generation: ++generation, deviceId: row?.deviceId || null, id: null, seq: -1, confirmPending: false };
    note = null; requestError = null; profileNote = null;
    resolveDraft = { mixed: null, replacement: null, aliases: {} };
    return session;
  }
  function retireSession() {
    if (session?.id) retiredSessions.add(session.id);
    session = null; generation += 1;
    note = null; requestError = null; profileNote = null;
  }
  function acceptPair(data, reply = false) {
    if (!data?.phase) return false;
    if (!session) {
      // Record reconciliation can surface a terminal event while the user is
      // not on a pairing screen (e.g. a one-sided commit found at startup).
      // It must not be swallowed, but it also never replaces a live session.
      const recordEvent = data.phase === 'peer_committed'
        || (data.phase === 'failed' && data.code === 'E_LAN_PAIR_ONESIDED');
      if (!recordEvent && (!windowWanted || !['await_confirm', 'peer_confirmed'].includes(data.phase))) return false;
      resetSession();
    }
    if (data.device_id && session.deviceId && data.device_id !== session.deviceId) return false;
    if (data.session_id && retiredSessions.has(data.session_id)) return false;
    if (data.session_id) {
      if (session.id && data.session_id !== session.id) return false;
    } else if (session.id && !reply) {
      // Recovery events from a record written by an older runtime may lack a
      // session id; only an RPC reply can still belong to the current one.
      return false;
    }
    if (Number.isSafeInteger(data.event_seq)) {
      // The start reply is seq 0 (session.seq starts at -1). Replies mirror
      // the notification they accompany, so an equal seq is only valid there.
      if (data.event_seq < session.seq || (data.event_seq === session.seq && !reply)) return false;
    }
    if (data.sas && note?.sas && data.sas !== note.sas) return false;
    if (['failed', 'cancelled'].includes(note?.phase) && data.phase !== 'failed') return false;
    if (['committed', 'peer_committed'].includes(note?.phase)
      && data.phase === 'failed' && data.code !== 'E_LAN_PAIR_ONESIDED') return false;
    if (['committed', 'peer_committed'].includes(note?.phase) && data.phase === 'cancelled') return false;
    const oldRank = phaseRank[note?.phase] ?? -1;
    const rank = phaseRank[data.phase] ?? oldRank;
    if (rank < oldRank) return false;
    const before = View.confirmedFlags(note), next = View.confirmedFlags(data);
    const merged = { ...note, ...data,
      local_confirmed: before.local || next.local, peer_confirmed: before.peer || next.peer };
    if (merged.phase === 'peer_committed') merged.peer_confirmed_commit = true;
    if (JSON.stringify(merged) === JSON.stringify(note)) return false;
    if (data.device_id) session.deviceId = data.device_id;
    if (data.session_id) session.id = data.session_id;
    if (Number.isSafeInteger(data.event_seq)) session.seq = data.event_seq;
    const oldReplacement = (note?.issues || []).find(i => i.detail?.kind === 'replace_device' && !i.detail.resolved)?.detail.old_device_id;
    const newReplacement = (merged.issues || []).find(i => i.detail?.kind === 'replace_device' && !i.detail.resolved)?.detail.old_device_id;
    if (oldReplacement !== newReplacement) resolveDraft.replacement = null;
    note = merged; requestError = null;
    return true;
  }
  const request = async (method, params) => {
    const current = generation;
    try { return await window.kvmflow.request(method, params); }
    catch (error) {
      if (current === generation && !terminal()) {
        requestError = error?.code || 'E_LAN_PAIR_ABORTED';
        render();
      }
      throw error;
    }
  };
  async function pairRequest(method, params) {
    const current = session;
    try {
      const result = await request(method, params);
      if (session === current) acceptPair(result, true);
      return result;
    } finally { if (session === current) render(); }
  }

  // The inbound pairing window is closed by default; it opens only while a
  // pairing screen is active (reserved backend change #3) and closes when
  // the screen is left.
  let windowWanted = false;
  let windowQueue = Promise.resolve();
  let windowOperation = null;
  let cleanupPending = Promise.resolve();
  let setupPending = false;
  let ignoredCloseEvents = 0;
  function setPairingWindow(open, force = false) {
    if (!force && windowWanted === open && (windowOwned === open || windowOperation?.open === open)) return windowQueue;
    const operation = windowOperation = { open };
    windowWanted = open;
    if (open) windowOwned = true;
    windowQueue = windowQueue.then(async () => {
      if (open && !windowWanted) return;
      for (let attempt = 0; attempt < (open ? 1 : 2); attempt += 1) {
        try {
          await window.kvmflow.request('lan.pair.window', { open });
          windowOwned = open;
          return;
        } catch {
          // Keep ownership until an acknowledged close. A later exit retries
          // too; do not silently treat a transport error as a closed window.
          if (attempt === (open ? 0 : 1)) {
            requestError = open ? 'E_LAN_PAIR_WINDOW' : 'E_LAN_WINDOW_CLOSE';
            render();
          }
        }
      }
    }).finally(() => { if (windowOperation === operation) windowOperation = null; });
    return windowQueue;
  }
  const openPairingWindow = () => setPairingWindow(true);
  const closePairingWindow = (force = false) => setPairingWindow(false, force);
  function endPairing(force = false) {
    const cancel = session && !terminal();
    retireSession();
    cleanupPending = cleanupPending.then(async () => {
      await closePairingWindow(force);
      if (cancel) {
        try { await window.kvmflow.request('lan.pair.cancel', {}); }
        catch { /* admission is closed even if no active session remains */ }
      }
    });
    return cleanupPending;
  }
  function markWindowClosed() { windowWanted = false; windowOwned = false; }

  // ---- discovery -----------------------------------------------------------

  function memberIds() {
    return (model.computers || []).map(computer => computer.id);
  }
  function peerRows() {
    return View.discoveryList(lan, memberIds());
  }

  // ---- shared fragments ----------------------------------------------------

  function sasHtml(sas, flags) {
    const flag = (done, doneText, waitingText) =>
      `<span class="pair-flag${done ? ' done' : ''}">${done ? '✓ ' : ''}${esc(t(done ? doneText : waitingText))}</span>`;
    return `<div class="sas-code" aria-live="polite">${esc(sas || '··· ···')}</div>
      <div class="pair-flags">${flag(flags?.local, 'lan.confirm.local.done', 'lan.confirm.local.waiting')}${flag(flags?.peer, 'lan.confirm.peer.done', 'lan.confirm.peer.waiting')}</div>`;
  }

  function miniScreens(assignment) {
    const name = (id) => {
      const computer = model.computers?.find(entry => entry.id === id);
      if (computer?.name) return computer.name;
      const peer = peerRows().find(entry => entry.deviceId === id);
      return peer?.name || id || t('computer');
    };
    return assignment.map((entry, index) => `<div class="pair-mini">
      <div class="pair-mini-screen">${esc(name(entry.deviceId))}</div>
      <span class="desk-stand" aria-hidden="true"></span>
      <span class="pair-mini-label">${t('monitor.value', { p0: index + 1 })}</span></div>`).join('');
  }

  function identityIssuesHtml() {
    return (lan?.issues || []).filter(issue => ['E_LAN_ID_CONFLICT', 'E_LAN_ID_MIGRATION_REQUIRED', 'E_LAN_IDENTITY_IO'].includes(issue?.code))
      .map(issue => `<p class="pair-error" role="alert">${t(View.errorCopy(issue.code).id)}</p>`).join('');
  }
  function manualConnectHtml() {
    return `<details class="pair-ip"><summary>${t('lan.ip.connect')}</summary>
      <form class="pair-ip-form"><input name="addr" aria-label="${t('lan.ip.address')}" placeholder="192.168.1.23:47820" autocomplete="off" spellcheck="false" required maxlength="21">
      <button type="submit" class="secondary" ${lan?.enabled === false ? 'disabled' : ''}>${t('lan.connect')}</button></form></details>`;
  }
  document.addEventListener('submit', event => {
    if (!event.target.matches('.pair-ip-form')) return;
    event.preventDefault();
    const value = event.target.elements.addr.value.trim();
    if (!value || (session && !terminal())) return;
    dialPeer = { addr: value, name: value, manual: true };
    void beginPairing();
  });

  function errorHtml(phase, compact) {
    const copy = View.errorCopy(phase.code, phase.message);
    return `<p class="pair-error${compact ? ' pair-pending' : ''}">${t(copy.id, { p0: esc(copy.code), p1: esc(phase.message || '') })}</p>`;
  }

  // ---- pairing dialog ------------------------------------------------------

  const dialog = () => byId('pairing-dialog');

  function conflictHtml() {
    const groups = View.issueGroups(note?.issues, note?.initiator === true);
    const issue = groups.choices.find(entry => entry?.detail?.kind === 'mixed');
    if (!issue) return null;
    const choice = View.mixedChoice(issue,
      model.computers?.find(computer => computer.local)?.name || t('this.computer'), peerName || t('other.computer'));
    if (!choice || choice.resolved) return null;
    const selected = (value) => resolveDraft.mixed === value
      || (resolveDraft.mixed === null && value === 'basis') ? ' selected' : '';
    const card = (side, value, title, hint) => `<div class="pair-option${selected(value)}" role="button" tabindex="0" aria-pressed="${resolveDraft.mixed === value || (resolveDraft.mixed === null && value === 'basis')}" data-pair-mixed="${value}">
      <div class="pair-option-copy"><strong>${title}</strong>
      ${hint ? `<span class="hint">${hint}</span>` : ''}</div>
      <div class="pair-mini-row">${miniScreens(side.assignment)}</div></div>`;
    return `<div class="pair-conflict">
      ${card(choice.basis, 'basis', t('lan.conflict.basis.title', { p0: esc(choice.basis.name) }), t('lan.conflict.basis.note'))}
      ${card(choice.joiner, 'joiner', t('lan.conflict.joiner.title', { p0: esc(choice.joiner.name) }), '')}</div>`;
  }

  const openChoices = () => View.issueGroups(note?.issues, note?.initiator === true).choices
    .filter(issue => issue?.detail?.resolved !== true);
  function additionalChoicesHtml() {
    return openChoices().filter(issue => issue.detail.kind !== 'mixed').map(({ detail }) => {
      if (detail.kind === 'monitor_alias') {
        const label = fp => esc(localNamesByFingerprint().get(fp) || fp);
        const fingerprint = detail.local_fingerprint;
        const choice = resolveDraft.aliases[fingerprint] || 'same';
        const option = (value, title, hint) => `<div class="pair-option${choice === value ? ' selected' : ''}" role="button" tabindex="0"
          aria-pressed="${choice === value}" data-pair-alias="${value}" data-pair-alias-fp="${esc(fingerprint)}">
          <div class="pair-option-copy"><strong>${title}</strong>${hint ? `<span class="hint">${hint}</span>` : ''}</div></div>`;
        return `<div class="pair-choice-section"><p>${t('lan.alias.question')}</p>
          <div class="pair-option pair-choice-identity"><span>${label(detail.group_fingerprint)}</span><span>↔</span><span>${label(detail.local_fingerprint)}</span></div>
          <div class="pair-conflict">
            ${option('same', t('lan.alias.same'), '')}
            ${option('different', t('lan.alias.different'), t('lan.alias.different.hint'))}</div>
          <p class="hint">${t('lan.alias.copy')}</p></div>`;
      }
      if (detail.kind === 'replace_device') {
        const name = id => esc(model.computers?.find(c => c.id === id)?.name || View.discoveryList(lan).find(p => p.deviceId === id)?.name || id);
        const selected = resolveDraft.replacement ?? detail.default_replace !== false;
        const card = (replace, title) => `<div class="pair-option${selected === replace ? ' selected' : ''}" role="button" tabindex="0" aria-pressed="${selected === replace}" data-pair-replacement="${replace}"><strong>${title}</strong></div>`;
        return `<div class="pair-choice-section"><p>${t('lan.replacement.question', { p0: name(detail.old_device_id), p1: name(detail.new_device_id) })}</p>
          <div class="pair-conflict">${card(true, t('lan.replacement.replace'))}${card(false, t('lan.replacement.keep'))}</div></div>`;
      }
      return `<p class="pair-pending">${t('lan.resolve.unsupported')}</p>`;
    }).join('');
  }
  function resolveParams() {
    if (note?.initiator !== true || !openChoices().length) return null;
    const params = {}, aliases = {};
    for (const { detail } of openChoices()) {
      if (detail.kind === 'mixed') params.mixed = resolveDraft.mixed || detail.default || 'basis';
      else if (detail.kind === 'monitor_alias' && detail.local_fingerprint && detail.group_fingerprint) {
        // `false` is the explicit "different physical display" answer: the
        // runtime keeps it unmatched and commits pin-only instead of
        // guessing a correspondence (or cancelling the whole pairing).
        aliases[detail.local_fingerprint] = (resolveDraft.aliases[detail.local_fingerprint] || 'same') === 'different'
          ? false : detail.group_fingerprint;
      } else if (detail.kind === 'replace_device' && detail.old_device_id) {
        if (params.replacement) return null; // current wire accepts one replacement
        params.replacement = { old_device_id: detail.old_device_id, replace: resolveDraft.replacement ?? detail.default_replace !== false };
      } else return null;
    }
    if (Object.keys(aliases).length) params.monitor_aliases = aliases;
    return params;
  }

  function resolveExtras() {
    const groups = View.issueGroups(note?.issues, note?.initiator === true);
    const out = [];
    const pending = View.pendingGuidance(groups.blockers, note?.device_id);
    if (pending.length) out.push(`<div class="pair-pending">${t('lan.resolve.pending', { p0: String(pending.length) })}</div>`);
    if (groups.blockers.some(issue => issue?.detail?.kind === 'monitor_unmatched')) out.push(`<div class="pair-pending">${t('lan.resolve.unmatched')}</div>`);
    if (groups.blockers.some(issue => issue?.detail?.kind === 'duplicate_input')) out.push(`<div class="pair-pending">${t('lan.resolve.duplicate')}</div>`);
    return out.join('');
  }

  function dialogBody(phase) {
    if (phase.state === 'failed') {
      return `<h2 id="pairing-title">${t('lan.fail.title')}</h2>
        ${errorHtml(phase)}
        <div class="confirmation-actions">
          <button class="secondary" data-pair-action="close">${t('lan.close')}</button>
          <button class="primary" data-pair-action="restart">${t('lan.retry.pair')}</button></div>`;
    }
    switch (phase.state) {
      case 'connecting':
        return `<h2 id="pairing-title">${t('lan.connecting.title', { p0: esc(peerName) })}</h2>
          <div class="pair-spinner" aria-hidden="true"></div>
          <p class="sub">${t('lan.connecting.copy')}</p>
          <div class="confirmation-actions"><button class="secondary" data-pair-action="cancel">${t('cancel')}</button></div>`;
      case 'sas': {
        const flags = phase.flags || {};
        const awaitingPeer = flags.local || session?.confirmPending;
        // Keep the quoted "一致，完成配对" / "They match…" phrase from splitting
        // across lines (the design breaks before the quote).
        const sasCopy = esc(t('lan.sas.copy', { p0: peerName }))
          .replace(/「[^」]+」/g, '<span class="pair-nowrap">$&</span>')
          .replace(/“[^”]+”/g, '<span class="pair-nowrap">$&</span>');
        return `<h2 id="pairing-title">${t('lan.sas.title')}</h2>
          <p class="sub">${sasCopy}</p>
          ${sasHtml(phase.sas, flags)}
          <div class="confirmation-actions">
            <button class="secondary" data-pair-action="cancel">${t('cancel')}</button>
            ${awaitingPeer ? '' : `<button class="secondary" data-pair-action="mismatch">${t('lan.sas.mismatch')}</button>
            <button class="primary" data-pair-action="confirm">${t('lan.sas.match')}</button>`}
          </div>`;
      }
      case 'resolve': {
        const conflict = conflictHtml();
        return `<h2 id="pairing-title">${t(conflict ? 'lan.conflict.title' : 'lan.resolve.title')}</h2>
          ${conflict ? `<p class="sub">${t('lan.conflict.head')}</p>${conflict}` : `<p class="sub">${t('lan.resolve.copy')}</p>`}
          ${additionalChoicesHtml()}
          ${resolveExtras()}
          <div class="confirmation-actions">
            <button class="secondary" data-pair-action="cancel">${t('lan.pair.cancel')}</button>
            ${openChoices().length ? `<button class="primary" data-pair-action="resolve" ${!resolveParams() || session?.resolvePending ? 'disabled' : ''}>${t(conflict && openChoices().length === 1 ? 'lan.conflict.use.selected' : 'lan.resolve.confirm')}</button>` : ''}
          </div>`;
      }
      case 'committing':
        return `<h2 id="pairing-title">${t('lan.committing.title')}</h2>
          <div class="pair-spinner" aria-hidden="true"></div>
          <p class="sub">${t('lan.committing.copy')}</p>
          <div class="confirmation-actions"><button class="secondary" disabled>${t('working')}</button></div>`;
      case 'committed':
        return `<h2 id="pairing-title">${t(phase.configMerged ? 'lan.committed.title' : 'lan.committed.pin.title')}</h2>
          <p class="sub">${t(phase.configMerged ? 'lan.committed.copy' : 'lan.committed.pin.copy')}</p>
          ${phase.configMerged ? '' : resolveExtras()}
          ${phase.peerAckPending ? `<p class="hint">${t('lan.committed.wait.peer')}</p>` : ''}
          <div class="confirmation-actions"><button class="primary" data-pair-action="finish">${t('continue')}</button></div>`;
      case 'peer_committed':
        return `<h2 id="pairing-title">${t('lan.committed.title')}</h2>
          <p class="sub">${t('lan.committed.copy')}</p>
          <div class="confirmation-actions"><button class="primary" data-pair-action="finish">${t('continue')}</button></div>`;
      case 'cancelled':
        return `<h2 id="pairing-title">${t('lan.cancelled.title')}</h2>
          <p class="sub">${t('lan.cancelled.copy')}</p>
        <div class="confirmation-actions"><button class="secondary" data-pair-action="close">${t('lan.close')}</button>
          <button class="primary" data-pair-action="restart">${t('lan.retry.pair')}</button></div>`;
      default:
        return `<h2 id="pairing-title">${t('lan.select.title')}</h2>
          <p class="sub">${t('lan.select.copy')}</p>
          <div class="pair-peer-list" id="pair-peer-list"></div>
          ${manualConnectHtml()}
          <p class="hint pair-search-hint">${t(lan?.enabled === false ? 'lan.list.empty.offline' : 'lan.searching')}</p>
          <div class="confirmation-actions">
            <button class="secondary" data-pair-action="close">${t('cancel')}</button>
            <button class="primary" id="pair-connect" data-pair-action="connect" disabled>${t('lan.connect')}</button></div>`;
    }
  }

  function renderDialog() {
    const root = dialog();
    if (!root || !root.open) return;
    const phase = View.pairPhase(note, requestError);
    // ready_to_commit on the initiator means the commit handshake started;
    // the explicit committing notification may never arrive separately.
    const effective = phase.state === 'resolve' && note?.phase === 'ready_to_commit' && note?.initiator !== false
      ? { state: 'committing' } : phase;
    const body = identityIssuesHtml() + dialogBody(effective);
    if (root.dataset.rendered !== body) {
      root.dataset.rendered = body;
      byId('pairing-body').innerHTML = body;
    }
    const conflict = (note?.issues || []).some(issue => issue?.severity === 'choice'
      && issue?.detail?.kind === 'mixed' && issue.detail.resolved !== true);
    root.classList.toggle('pair-wide', effective.state === 'resolve' && conflict);
    renderPeerList();
  }

  function renderPeerList() {
    const list = byId('pair-peer-list');
    if (!list) return;
    const rows = peerRows();
    const items = rows.map(row => `<button type="button" class="pair-peer${dialPeer?.deviceId === row.deviceId ? ' selected' : ''}${row.available && row.addr ? '' : ' offline'}"
        data-pair-peer="${esc(row.deviceId)}" aria-pressed="${dialPeer?.deviceId === row.deviceId}" ${row.available && row.addr ? '' : 'disabled'}>
        <span class="pair-peer-copy"><strong>${esc(row.name)}</strong>
        <span class="hint">${esc([row.platform, hostOf(row.addr)].filter(Boolean).join(' · ') || t('lan.peer.unreachable'))}</span></span>
        ${dialPeer?.deviceId === row.deviceId ? `<span class="settings-tag">${t('lan.selected')}</span>` : ''}</button>`).join('');
    const html = items || `<p class="hint pair-empty">${t(lan?.enabled === false ? 'lan.list.empty.offline' : 'lan.list.empty')}</p>`;
    if (list.dataset.rendered !== html) { list.dataset.rendered = html; list.innerHTML = html; }
    const connect = byId('pair-connect');
    if (connect) connect.disabled = !(dialPeer?.available && dialPeer?.addr);
  }

  // ---- primary wizard pages --------------------------------------------------

  function addComputerHtml() {
    const rows = peerRows();
    const disabled = lan?.enabled === false;
    const cards = rows.map(row => `<div class="pair-card${dialPeer?.deviceId === row.deviceId ? ' selected' : ''}">
      <div><strong>${esc(row.name)}</strong><span class="hint">${row.available && row.addr ? t('lan.peer.ready') : t('lan.peer.unreachable')}</span></div>
      <button class="primary" data-pair-peer="${esc(row.deviceId)}" ${row.available && row.addr ? '' : 'disabled'}>${t('lan.connect')}</button></div>`).join('');
    const empty = disabled
      ? `<div class="pair-card empty"><div><strong>${t('lan.list.empty.offline')}</strong></div></div>`
      : `<div class="pair-card empty"><div><strong>${t('lan.searching')}</strong><span class="hint">${t('lan.list.empty')}</span></div></div>`;
    return `<div class="wizard-head"><h1>${t('lan.add.title')}</h1>
      <p class="sub">${t('lan.add.copy')}</p></div>
      ${identityIssuesHtml()}
      <div class="pair-cards">${cards || empty}</div>
      ${manualConnectHtml()}
      ${disabled ? '' : `<p class="hint pair-search-hint">${t('lan.searching')}</p>`}
      <div class="wizard-footer">
        <p class="pair-manual"><span class="hint">${t('lan.no.lan')}</span> <button class="link pair-link" data-pair-action="manual">${t('lan.manual.fill')}</button></p>
        <button class="secondary" data-action="previousStep">${t('back')}</button></div>`;
  }

  function confirmHtml() {
    if (!profileNote) return `<div class="wizard-head"><h1>${t('lan.profile.waiting')}</h1><p class="sub">${t('lan.profile.waiting.copy')}</p></div>
      <div class="pair-waiting"><div class="pair-spinner"></div></div><div class="wizard-footer"><span></span><button class="primary" data-pair-action="done" disabled>${t('lan.done')}</button></div>`;
    const rows = View.profileRows(profileNote, localNamesByFingerprint());
    const pending = View.pendingCount(rows);
    const peerDevice = model.computers?.find(computer => !computer.local);
    const port = model.computers?.find(c => c.id === session?.deviceId)?.port || peerDevice?.port || 2;
    const monitorCards = rows.map(row => `<div class="pair-monitor${row.pending ? ' pending' : ''}">
      <div class="pair-monitor-copy"><strong>${row.localName ? `${esc(row.localName)} · ${esc(row.name)}` : esc(row.name)}</strong>
        ${row.pending ? `<p class="hint">${t('lan.monitor.pending.copy', { p0: esc(peerName) })}</p>` : ''}</div>
      <span class="settings-tag">${row.pending ? t('lan.monitor.pending') : t('lan.monitor.read')}</span></div>`).join('');
    return `<div class="wizard-head pair-head"><h1>${t('lan.confirm.ready.title', { p0: esc(peerName) })}</h1>
      <p class="sub">${t('lan.confirm.ready.copy', { p0: esc(peerName) })}</p></div>
      <div class="field"><label for="pair-peer-name">${t('computer.name')}</label>
        <input id="pair-peer-name" value="${esc(peerName)}" maxlength="32"></div>
      <p class="hint pair-port">${t('usb.switch.port')}：${port} <span class="settings-tag">${t('lan.port.inferred')}</span></p>
      <div class="form-section"><h3>${t('monitor.input.values')}</h3><div class="pair-monitors">${monitorCards}</div>
        <p class="hint pair-mixed-later">${t('lan.confirm.mixed.later')}</p></div>
      <div class="wizard-footer">
        ${pending ? `<span class="hint" id="pair-pending-count">${t('lan.pending.count', { p0: String(pending) })}</span>` : '<span></span>'}
        <button class="primary" data-pair-action="done" ${pending || !['committed', 'peer_committed'].includes(note?.phase) || requestError ? 'disabled' : ''}>${t('lan.done')}</button></div>`;
  }

  function localNamesByFingerprint() {
    const map = new Map();
    (model.displays || []).forEach((label, index) => {
      const fingerprint = window.__kvmflowDisplayFingerprints?.[index];
      if (fingerprint) map.set(fingerprint, label);
    });
    return map;
  }
  const labelOfFingerprint = (fingerprint) => {
    const index = (window.__kvmflowDisplayFingerprints || []).indexOf(fingerprint);
    return index >= 0 ? (model.displays?.[index] || null) : null;
  };

  let tryRun = null;
  async function startTry() {
    if (mode !== 'confirm' || !['committed', 'peer_committed'].includes(note?.phase)
      || !profileNote || View.pendingCount(View.profileRows(profileNote)) || requestError) return;
    mode = 'try'; tryReports = []; step = 3;
    const run = tryRun = { peerId: session.deviceId, localId: model.localId, started: false,
      startedMs: Date.now(), id: null, seen: new Set(), evidence: new Map(), stopPromise: null };
    run.startPromise = window.kvmflow.request('lan.test.start', { duration_ms: 120000 }).then(result => {
      run.id = result?.test_id || null;
      run.started = true;
    }).catch(() => { run.error = true; }).finally(() => { if (tryRun === run) renderWizard(); });
    renderGuide();
    await run.startPromise;
  }
  function stopTry() {
    const run = tryRun;
    if (!run) return Promise.resolve();
    if (!run.stopPromise) {
      run.stopPromise = run.startPromise.then(async () => {
        try { acceptTestEvidence(await window.kvmflow.request('lan.test.stop', {})); }
        catch { run.error = true; if (tryRun === run) renderWizard(); }
      });
    }
    return run.stopPromise;
  }
  function acceptTestEvidence(data) {
    const run = tryRun;
    if (!run?.id || data?.test_id !== run.id || !Array.isArray(data.events)) return;
    for (const event of data.events) {
      const report = event?.body;
      if (!['test.event', 'switch.report'].includes(event?.kind) || !report?.press_id
        || report.test_id !== run.id || ![run.localId, run.peerId].includes(event.device_id)
        || ![run.localId, run.peerId].includes(report.target_device_id)) continue;
      const key = `${report.press_id}:${report.target_device_id}`;
      if (!run.evidence.has(key)) run.evidence.set(key, { reports: new Map(), arrived: new Set() });
      const evidence = run.evidence.get(key);
      if (report.test_kind === 'picture_arrived') evidence.arrived.add(event.device_id);
      if (Array.isArray(report.per_monitor) && report.per_monitor.length) {
        evidence.reports.set(event.device_id, report);
      }
    }
    for (const report of tryReports) {
      if (!report.press_id) continue;
      const evidence = run.evidence.get(`${report.press_id}:${report.target_device_id}`);
      const outcomes = Array.isArray(report.per_monitor) ? report.per_monitor : [];
      const commandsSent = outcomes.length > 0 && outcomes.every(row => row.commanded === true);
      const pictureArrived = Boolean(evidence?.arrived.size);
      report.commandsSent = commandsSent;
      report.pictureArrived = pictureArrived;
      report.bothEndsVerified = commandsSent && pictureArrived;
    }
    renderWizard();
  }

  function tryHtml() {
    const summary = View.trySummary(tryReports, labelOfFingerprint);
    const peerComputer = model.computers?.find(computer => computer.id === tryRun?.peerId) || { name: peerName };
    const localComputer = model.computers?.find(computer => computer.local);
    // Mirror the mixed views' naming: "显示器 1、2" when all names are
    // defaults, otherwise each display's own label.
    const displays = model.displays || [];
    const allDefault = displays.every(label => /^显示器 \d+$/.test(label));
    const names = allDefault
      ? t('monitor.value', { p0: displays.map((_, index) => index + 1).join(t('mixed.list.separator')) })
      : displays.map(esc).join(t('mixed.list.separator'));
    const card = (result, text) => {
      const state = result ? (result.ok ? ' ok' : result.commanded ? '' : ' bad') : '';
      const commands = result
        ? t(result.commanded ? 'lan.test.commanded' : 'lan.test.failed')
        : t('lan.test.waiting');
      const arrival = result
        ? t(result.arrived ? 'lan.test.arrived' : 'lan.test.arrived.waiting')
        : null;
      return `<div class="pair-test${state}">
        <span>${text}</span>
        <span class="settings-tag">${commands}</span>
        ${arrival ? `<span class="settings-tag">${arrival}</span>` : ''}</div>`;
    };
    const failedDisplays = [...(summary.first?.failed || []), ...(summary.second?.failed || [])];
    return `<div class="wizard-head"><h1>${t('lan.try.title')}</h1>
      <p class="sub">${t('lan.try.copy')}</p></div>
      ${tryRun?.error ? `<p class="pair-error">${t('lan.try.error')}</p>` : ''}
      <div class="pair-tests">
        ${card(summary.first, t('lan.try.first', { p0: esc(peerComputer?.name || t('other.computer')), p1: names }))}
        ${card(summary.second, t('lan.try.second', { p0: esc(localComputer?.name || t('this.computer')) }))}
      </div>
      ${tryReports.some(report => !report.bothEndsVerified) ? `<p class="hint">${t('lan.try.evidence.pending')}</p>` : ''}
      ${failedDisplays.length ? `<div class="pair-failure">${t('lan.try.failed.hint', { p0: failedDisplays.map(esc).join(t('mixed.list.separator')) })}</div>` : ''}
      <div class="wizard-footer">
        <button class="link pair-link" data-pair-action="skip">${t('lan.try.skip')}</button>
        <button class="primary" data-pair-action="finish">${t('lan.done')}</button></div>`;
  }

  // ---- secondary wizard pages ------------------------------------------------

  function roleHtml() {
    const card = (value, title, copy) => {
      const selected = roleChoice === value;
      return `<div class="choice ${selected ? 'selected' : ''}" role="button" tabindex="0" aria-pressed="${selected}" data-pair-role="${value}">
        <div class="choice-title">${title}</div>
        <p class="hint choice-copy">${copy}</p></div>`;
    };
    return `<div class="wizard-head"><h1>${t('lan.welcome')}</h1>
      <p class="sub">${t('lan.role.question')}</p></div>
      <div class="choice-stack pair-role">
        ${card('primary', t('lan.role.primary'), t('lan.role.primary.copy'))}
        ${card('secondary', t('lan.role.secondary'), t('lan.role.secondary.copy'))}
      </div>
      <div class="wizard-footer"><span></span>
        <button class="primary" ${roleChoice ? '' : 'disabled'} data-pair-action="role-next">${t('continue')}</button></div>`;
  }

  const activePairPhase = () => View.pairPhase(note, requestError);

  function secondaryWaitHtml() {
    const localName = model.computers?.find(computer => computer.local)?.name || t('this.computer');
    const selfAddr = hostOf(lan?.self?.addr);
    const selfLine = t('lan.secondary.self', { p0: localName, p1: selfAddr ? `· ${selfAddr}` : '' });
    const manual = `<p class="pair-manual"><span class="hint">${t('lan.no.lan')}</span> <button class="link pair-link" data-pair-action="manual">${t('lan.manual.setup')}</button></p>`;
    const phase = activePairPhase();
    if (phase.state === 'failed' || phase.state === 'cancelled') {
      return `<div class="wizard-head"><h1>${t('lan.fail.title')}</h1></div>
        ${phase.state === 'cancelled' ? `<p class="hint">${t('lan.cancelled.copy')}</p>` : errorHtml(phase, true)}
        <div class="wizard-footer">${manual}
          <button class="secondary" data-pair-action="wait-again">${t('lan.retry.pair')}</button></div>`;
    }
    if (phase.state === 'committed' || phase.state === 'peer_committed') return secondaryDoneHtml();
    if (phase.state === 'sas') {
      return `<div class="wizard-head"><h1>${t('lan.secondary.wait.title')}</h1>
        <p class="sub">${t('lan.secondary.wait.copy')}</p></div>
        <div class="sas-panel">
          <p>${t('lan.secondary.request', { p0: esc(peerName) })}</p>
          ${sasHtml(phase.sas, phase.flags)}
        </div>
        ${identityIssuesHtml()}
        <p class="hint pair-self">${esc(selfLine)}</p>
        <div class="wizard-footer">${manual}
          <div class="pair-footer-actions">
            ${phase.flags?.local || session?.confirmPending ? `<span class="hint">${t('lan.confirm.local.done')}</span>` : `<button class="secondary" data-pair-action="mismatch">${t('lan.sas.mismatch')}</button>
            <button class="primary" data-pair-action="confirm">${t('lan.sas.agree')}</button>`}</div></div>`;
    }
    // connecting / resolve / committing: the primary drives the flow.
    const busyText = phase.state === 'connecting' ? t('lan.connecting.copy')
      : phase.state === 'committing' ? t('lan.committing.copy')
      : phase.state === 'resolve' ? t('lan.secondary.resolving')
      : t('lan.secondary.wait.standby');
    return `<div class="wizard-head"><h1>${t('lan.secondary.wait.title')}</h1>
      <p class="sub">${t('lan.secondary.wait.copy')}</p></div>
      <div class="pair-waiting"><div class="pair-spinner"></div><div><strong>${t('lan.secondary.wait.standby')}</strong>
        <p class="hint">${busyText}</p></div></div>
      ${identityIssuesHtml()}
      <p class="hint pair-self">${esc(selfLine)}</p>
      <div class="wizard-footer">${manual}
        <button class="secondary" data-pair-action="role-back">${t('back')}</button></div>`;
  }

  function secondaryDoneHtml() {
    const pendingRows = localPendingRows();
    return `<div class="wizard-head"><h1>${t('lan.secondary.done.title', { p0: esc(peerName || t('other.computer')) })}</h1>
      <p class="sub">${t('lan.secondary.done.copy')}</p></div>
      ${pendingRows.length ? `<div class="pair-notice">${t('lan.secondary.done.pending', { p0: pendingRows.map(row => esc(row.name)).join(t('mixed.list.separator')) })}</div>` : ''}
      <div class="wizard-footer"><span></span>
        <button class="primary" data-pair-action="close-window">${t('lan.close.window')}</button></div>`;
  }

  // Local monitors whose input this machine has not actively read (a
  // secondary's fresh profile reports them as unknown): the done page asks
  // the user to switch each one over once with the monitor's own buttons.
  function localPendingRows() {
    return (model.displays || []).map((label, index) => ({
      name: label,
      pending: model.localInputSources?.[index] !== 'learned_active_read' && model.localInputSources?.[index] !== 'manual',
    })).filter(row => row.pending);
  }

  // ---- wizard rendering ------------------------------------------------------

  let roleChoice = null;

  function wizardHtml() {
    switch (mode) {
      case 'add': return addComputerHtml();
      case 'confirm': return confirmHtml();
      case 'try': return tryHtml();
      case 'role': return roleHtml();
      case 'secondary-wait': return secondaryWaitHtml();
      case 'secondary-done': return secondaryDoneHtml();
      default: return null;
    }
  }

  function renderWizard() {
    const root = byId('guide-content');
    if (!root) return;
    root.classList.toggle('pair-wizard', Boolean(mode));
    if (!mode) return;
    if (mode === 'secondary-wait' && (activePairPhase().state === 'committed' || activePairPhase().state === 'peer_committed')) {
      mode = 'secondary-done';
    }
    // The secondary's pages are not part of the 4-step primary wizard.
    const secondary = mode === 'role' || mode === 'secondary-wait' || mode === 'secondary-done';
    byId('steps')?.classList.toggle('hidden', secondary);
    const counter = byId('guide-count');
    if (counter && secondary) counter.textContent = '';
    // Sidebar status follows the secondary lifecycle (secondary-2/3 designs).
    const navState = byId('nav-state');
    if (navState) {
      // The role choice is not the secondary lifecycle yet; keep the regular status.
      if (mode === 'role') window.navState?.();
      else if (mode === 'secondary-wait') { navState.textContent = t('nav.waiting.pair'); navState.title = navState.textContent; }
      else if (mode === 'secondary-done') { navState.textContent = t('nav.paired', { p0: esc(peerName || t('other.computer')) }); navState.title = navState.textContent; }
    }
    const html = wizardHtml();
    if (html !== null && root.dataset.pairRendered !== html) {
      root.dataset.pairRendered = html;
      root.innerHTML = html;
    }
    if (mode === 'add' || mode === 'confirm') renderPeerList();
  }

  function setupComplete() {
    return model.usbConfirmed && model.computers.length > 1;
  }

  // First run asks for the role before the primary wizard: no active pairing
  // mode, the first step, incomplete setup and no host choice in this
  // session. A persisted secondary skips the question and resumes waiting.
  function maybeChooseRole() {
    if (mode || step !== 0 || primaryChosen || setupComplete()) return;
    if (lan?.setup?.secondary) {
      mode = 'secondary-wait';
      secondaryNote ||= { secondary: true, phase: 'waiting_for_primary' };
      autoRole = false;
      return;
    }
    mode = 'role';
    roleChoice = null;
    autoRole = true;
  }

  // The classic wizard: step 3 (index 2) offers LAN pairing first; a first-run
  // host can step back to the role choice from step 1.
  const originalGuideView = window.guideView;
  if (typeof originalGuideView === 'function') {
    window.guideView = function overriddenGuideView() {
      const classic = originalGuideView();
      if (mode) return classic;
      if (step === 2) {
        const entry = `<div class="pair-entry"><button class="primary" data-pair-action="add-via-lan">${t('lan.add.title')}</button>
          <span class="hint">${t('lan.add.entry.hint')}</span></div>
          <p class="pair-entry-sep hint">${t('lan.add.entry.or')}</p>`;
        return classic.replace('<div class="wizard-head">', `${entry}<div class="wizard-head">`);
      }
      if (step === 0 && !setupComplete() && !lan?.setup?.secondary) {
        return classic.replace('<div class="wizard-footer"><span></span>',
          `<div class="wizard-footer"><button class="secondary" data-pair-action="role-back">${t('back')}</button>`);
      }
      return classic;
    };
  }
  const originalRenderGuide = window.renderGuide;
  window.renderGuide = function overriddenRenderGuide(...args) {
    maybeChooseRole();
    originalRenderGuide(...args);
    if (mode) {
      byId('guide-content').dataset.pairRendered = '';
      renderWizard();
    }
  };

  function render() {
    renderDialog();
    renderWizard();
  }

  // ---- actions ---------------------------------------------------------------

  document.addEventListener('click', (event) => {
    const peerCard = event.target.closest('[data-pair-peer]');
    if (peerCard && !peerCard.disabled) {
      const row = peerRows().find(entry => entry.deviceId === peerCard.dataset.pairPeer);
      if (row?.available && row?.addr) {
        dialPeer = row;
        peerName = row.name;
        if (dialog()?.open) renderDialog();
        else if (mode === 'add' || mode === null) void beginPairing();
        else renderWizard();
      }
      return;
    }
    const role = event.target.closest('[data-pair-role]');
    if (role && !event.target.closest('input,button:not([data-pair-role]),select,textarea,label')) {
      roleChoice = role.dataset.pairRole;
      renderWizard();
      return;
    }
    const replacement = event.target.closest('[data-pair-replacement]');
    if (replacement && note?.initiator === true) {
      resolveDraft.replacement = replacement.dataset.pairReplacement === 'true';
      renderDialog();
      return;
    }
    const alias = event.target.closest('[data-pair-alias]');
    if (alias && note?.initiator === true) {
      resolveDraft.aliases[alias.dataset.pairAliasFp] = alias.dataset.pairAlias;
      renderDialog();
      return;
    }
    const mixed = event.target.closest('[data-pair-mixed]');
    if (mixed && !event.target.closest('input,button:not([data-pair-mixed]),select,textarea,label')) {
      resolveDraft.mixed = mixed.dataset.pairMixed;
      renderDialog();
      return;
    }
    const control = event.target.closest('button[data-pair-action]');
    if (!control || control.disabled || !control.closest('#pairing-body, #guide-content')) return;
    void handleAction(control.dataset.pairAction);
  });
  document.addEventListener('keydown', (event) => {
    if (!['Enter', ' '].includes(event.key)) return;
    const control = event.target.closest('[role="button"][data-pair-role], [role="button"][data-pair-mixed], [role="button"][data-pair-replacement], [role="button"][data-pair-alias]');
    if (control) { event.preventDefault(); control.click(); }
  });

  async function beginPairing() {
    if (!dialPeer?.addr || (session && !terminal())) return;
    const row = dialPeer;
    const current = resetSession(row);
    peerName = row.name;
    openDialog({ admit: false });
    try {
      await cleanupPending;
      if (session !== current) return;
      await window.KVMFlowPairingConfig.preparePrimary();
      if (session !== current) return;
      await openPairingWindow();
      if (session !== current) return;
      await pairRequest(row.manual ? 'lan.connect_manual' : 'lan.pair.start', { addr: row.addr, ...(row.manual ? { pair: true } : {}) });
    } catch (error) {
      if (session === current && !terminal()) {
        requestError = error?.code || 'E_LAN_LOCAL_SETUP';
        await closePairingWindow(true);
      }
    }
    render();
  }

  async function handleAction(action) {
    switch (action) {
      case 'close':
        closeDialog();
        return;
      case 'cancel':
        closeDialog();
        dialPeer = null;
        renderWizard();
        return;
      case 'restart':
        await endPairing(); dialPeer = null;
        await openPairingWindow();
        renderDialog();
        return;
      case 'connect':
        await beginPairing();
        return;
      case 'confirm': {
        if (!session || session.confirmPending || View.pairPhase(note).state !== 'sas' || View.confirmedFlags(note).local || !note?.sas) return;
        const current = session;
        current.confirmPending = true;
        render();
        try { await pairRequest('lan.pair.confirm', { sas_ok: true }); }
        catch { /* request renders the error */ }
        finally { if (session === current) { current.confirmPending = false; render(); } }
        return;
      }
      case 'mismatch':
        try { await pairRequest('lan.pair.confirm', { sas_ok: false }); }
        catch { /* dialog shows the code */ }
        render();
        return;
      case 'resolve': {
        const params = resolveParams();
        if (!params || !session || session.resolvePending) return;
        const current = session;
        current.resolvePending = true; render();
        try { await pairRequest('lan.pair.resolve', params); }
        catch { /* request renders the error */ }
        finally { if (session === current) { current.resolvePending = false; render(); } }
        return;
      }
      case 'finish':
        if (mode === 'try') {
          await stopTry();
          exitWizard(3);
          window.navigate('status');
        } else if (['committed', 'peer_committed'].includes(note?.phase)) {
          closeDialog({ cancel: false });
          if (mode === 'add' || mode === 'confirm') { mode = 'confirm'; renderWizard(); }
        }
        return;
      case 'done':
        await startTry();
        return;
      case 'skip':
        await stopTry();
        exitWizard(3);
        return;
      case 'manual':
        closeDialog();
        exitWizard(2);
        choice = 'known';
        return;
      case 'add-via-lan':
        mode = 'add'; dialPeer = null;
        renderGuide();
        return;
      case 'choose-role':
        mode = 'role'; roleChoice = null; autoRole = false;
        renderGuide();
        return;
      case 'role-next':
        if (roleChoice === 'secondary') {
          autoRole = false;
          await startSecondary();
        } else {
          primaryChosen = true; autoRole = false;
          exitWizard(0);
        }
        return;
      case 'wait-again':
        await startSecondary();
        return;
      case 'role-back':
        roleChoice = null; mode = 'role'; autoRole = false;
        void endPairing();
        renderWizard();
        return;
      case 'close-window':
        void endPairing();
        exitWizard(3);
        return;
    }
  }

  async function startSecondary() {
    if (setupPending) return;
    const currentGeneration = generation;
    setupPending = true;
    try {
      await cleanupPending;
      if (generation !== currentGeneration) return;
      const result = await request('lan.setup.secondary', {});
      if (generation !== currentGeneration) {
        await closePairingWindow(true);
        return;
      }
      secondaryNote = result || { secondary: true, phase: 'waiting_for_primary' };
      resetSession();
      mode = 'secondary-wait';
      await openPairingWindow();
      renderWizard();
    } catch { render(); }
    finally { setupPending = false; }
  }

  function exitWizard(targetStep) {
    mode = null;
    autoRole = false;
    roleChoice = null;
    byId('steps')?.classList.remove('hidden');
    byId('guide-content')?.classList.remove('pair-wizard');
    byId('guide-content').dataset.pairRendered = '';
    step = targetStep;
    renderGuide();
    renderStatus();
    navState(); // restore the sidebar status the secondary pages overrode
  }

  function openDialog({ admit = true } = {}) {
    const root = dialog();
    if (!root) return;
    if (admit) void openPairingWindow();
    if (typeof root.showModal === 'function') { try { root.showModal(); } catch { /* already modal */ } }
    else root.setAttribute('open', '');
    root.dataset.rendered = '';
    renderDialog();
  }
  function closeDialog({ cancel = !terminal() } = {}) {
    if (cancel) void endPairing();
    else if (windowOwned) void closePairingWindow();
    const root = dialog();
    if (!root) return;
    if (typeof root.close === 'function' && root.open) {
      ignoredCloseEvents += 1;
      root.close();
    }
    root.removeAttribute('open');
    root.dataset.rendered = '';
  }
  dialog()?.addEventListener('cancel', event => { event.preventDefault(); closeDialog(); });
  dialog()?.addEventListener('close', () => {
    if (ignoredCloseEvents) { ignoredCloseEvents -= 1; return; }
    closeDialog();
  });
  function leavePairing(force = false) {
    if (mode === 'try') void stopTry();
    if (force || windowOwned || session || setupPending) void endPairing(force);
    closeDialog({ cancel: false });
    if (mode === 'secondary-wait' || mode === 'secondary-done') { mode = 'role'; autoRole = false; }
    else if (mode !== 'role') mode = null;
    renderWizard();
  }
  window.kvmflow?.onNotification?.('client.hidden', () => leavePairing(true));
  window.addEventListener('pagehide', () => leavePairing(true));

  // ---- notifications -----------------------------------------------------------

  window.kvmflow?.onNotification?.('lan.pair', async (data) => {
    if (!acceptPair(data)) return;
    // Prefer the handshake-authenticated name over the mDNS label.
    if (typeof data.peer_name === 'string' && data.peer_name.trim()) peerName = data.peer_name.trim();
    const current = session;
    const accepted = note;
    if (note?.phase === 'failed') {
      const copy = View.errorCopy(note.code, note.message);
      log(t('lan.log.pair.failed'), t(copy.id, { p0: note.code || '', p1: note.message || '' }));
      markWindowClosed(); // the runtime closed the session window
    } else if (note?.phase === 'cancelled') {
      markWindowClosed();
    } else if (note?.phase === 'committed') {
      // The runtime closes admission on commit; mirror it explicitly so the
      // UI never leaves an old pairing window open if that frame was missed.
      await closePairingWindow();
      await window.KVMFlowPairingConfig?.refresh().then(() => {
        if (session === current && requestError === 'E_LAN_CONFIG_REFRESH') requestError = null;
      }).catch(() => { if (session === current) requestError = 'E_LAN_CONFIG_REFRESH'; });
      if (session !== current || note !== accepted) return;
      log(t('lan.log.committed'), t(note.config_merged === false ? 'lan.committed.pin.copy' : 'lan.committed.copy'));
      markWindowClosed();
      if (mode === 'confirm' || mode === 'add') { /* stay until 完成 */ }
    } else if (note?.phase === 'peer_committed') {
      await closePairingWindow();
      markWindowClosed();
    }
    const authenticated = typeof note?.peer_name === 'string' && note.peer_name.trim();
    if (note?.device_id && !authenticated) {
      const row = View.discoveryList(lan).find(entry => entry.deviceId === note.device_id);
      if (row?.name) peerName = row.name;
    }
    render();
  });
  window.kvmflow?.onNotification?.('lan.sync', data => {
    if (Array.isArray(data?.issues)) {
      const issues = new Map((lan?.issues || []).map(issue => [issue.code, issue]));
      for (const issue of data.issues) issues.set(issue.code, issue);
      lan = { ...(lan || {}), issues: [...issues.values()] };
      render();
    }
  });
  window.kvmflow?.onNotification?.('lan.peers', (data) => {
    if (Array.isArray(data?.peers)) lan = { ...(lan || {}), peers: data.peers };
    renderPeerList();
    renderWizard();
  });
  window.kvmflow?.onNotification?.('lan.profile', async (data) => {
    if (Array.isArray(data?.monitors)) {
      // Owner reports from existing members still refresh settings, but can
      // never replace the selected peer's confirmation page.
      const current = session;
      await window.KVMFlowPairingConfig?.refresh().then(() => {
        if (session === current && requestError === 'E_LAN_CONFIG_REFRESH') requestError = null;
      }).catch(() => { if (session === current) requestError = 'E_LAN_CONFIG_REFRESH'; });
      if (session !== current || (session?.deviceId && data.device_id !== session.deviceId)) return;
      if (profileNote && (data.profile_rev ?? 0) < (profileNote.profile_rev ?? 0)) return;
      if (!session?.deviceId || data.device_id !== session.deviceId) return;
      profileNote = data;
      if (typeof data.name === 'string' && data.name.trim()) peerName = data.name.trim();
      else if (data?.device_id) {
        const row = peerRows().find(entry => entry.deviceId === data.device_id);
        if (row?.name) peerName = row.name;
      }
      if (mode === 'add') mode = 'confirm';
      render();
    } else if (data?.phase === 'inputs_learned' && Array.isArray(data.learned)) {
      log(t('lan.log.input.learned'), t('lan.log.input.learned.copy'));
      if (mode === 'confirm' || mode === 'secondary-done') {
        void window.KVMFlowPairingConfig?.refresh().then(() => renderWizard()).catch(() => {});
        void request('lan.profile.refresh', {}).catch(() => {});
      }
    }
  });
  window.kvmflow?.onNotification?.('lan.setup', (data) => {
    secondaryNote = data || null;
    renderWizard();
  });
  window.kvmflow?.onNotification?.('switch.report', (report) => {
    const run = tryRun;
    if (mode !== 'try' || !run?.started || run.error || run.stopPromise || !Array.isArray(report?.per_monitor) || !report.per_monitor.length) return;
    if (tryReports.length >= 2 || report.target_device_id !== [run.peerId, run.localId][tryReports.length]) return;
    if (!run.id || report.test_id !== run.id || !report.press_id) return;
    if (Number.isFinite(report.ts_ms) && report.ts_ms < run.startedMs) return;
    const key = `${report.press_id}:${report.target_device_id}`;
    if (run.seen.has(key)) return;
    run.seen.add(key);
    const commandsSent = report.per_monitor.every(row => row.commanded === true);
    tryReports.push({ ...report, commandsSent, pictureArrived: false, bothEndsVerified: false });
    renderWizard();
    if (tryReports.length === 2) void stopTry();
    if (View.trySummary(tryReports, labelOfFingerprint).first?.commanded === false
      || View.trySummary(tryReports, labelOfFingerprint).second?.commanded === false) {
      log(t('lan.try.failed.title'), t('lan.try.failed.copy'));
    }
  });
  window.kvmflow?.onNotification?.('lan.test', acceptTestEvidence);

  // state.get / state notifications carry the whole `lan` object; mixed.js
  // forwards its merged snapshot through this hook.
  function acceptLanState(state) {
    if (state && typeof state.lan === 'object' && state.lan) {
      lan = state.lan;
      if (autoRole && mode === 'role' && lan.setup?.secondary) {
        mode = 'secondary-wait';
        secondaryNote ||= { secondary: true, phase: 'waiting_for_primary' };
        autoRole = false;
      }
      renderPeerList();
      renderWizard();
    }
  }
  window.KVMFlowPairingStateHook = acceptLanState;

  // Leaving the guide page closes the secondary's pairing window.
  const originalNavigate = window.navigate;
  window.navigate = function overriddenNavigate(page, options) {
    if (page !== 'guide' && (mode || windowOwned || dialog()?.open || setupPending)) {
      leavePairing();
      byId('guide-content')?.classList.toggle('pair-wizard', Boolean(mode));
      byId('steps')?.classList.remove('hidden');
      navState();
    }
    return originalNavigate(page, options);
  };

  // ---- public API ----------------------------------------------------------------

  window.KVMFlowPairing = {
    startPrimaryAdd: () => { mode = 'add'; dialPeer = null; renderWizard(); },
    startRoleChoice: () => { mode = 'role'; roleChoice = null; autoRole = false; renderWizard(); },
    pairWith: (deviceId) => {
      const row = View.discoveryList(lan).find(entry => entry.deviceId === deviceId);
      if (!row?.addr || row.available === false) return;
      if (window.KVMFlowPairingConfig?.isTrusted(deviceId)) {
        void window.kvmflow.request('lan.connect.manual', { addr: row.addr }).catch(error => {
          const copy = View.errorCopy(error?.code, error?.message);
          log(t('lan.log.pair.failed'), t(copy.id, { p0: copy.code || '', p1: error?.message || '' }));
        });
        return;
      }
      if (session && !terminal()) return;
      dialPeer = row;
      peerName = row.name;
      void beginPairing();
    },
    openDialog, closeDialog, acceptLanState,
    activeMode: () => mode,
    renderWizard,
  };

  window.addEventListener('language-changed', () => {
    const root = dialog();
    if (root) root.dataset.rendered = '';
    const guide = byId('guide-content');
    if (guide) guide.dataset.pairRendered = '';
    render();
  });
})();
