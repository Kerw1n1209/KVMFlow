// LAN pairing UI: role-free onboarding and the settings pairing dialog. State comes from the sidecar through lan.pair / lan.peers /
// lan.profile / lan.setup notifications (LAN_RUNTIME_PROTOCOL.md); the only
// requests use pairing, profile, setup, wizard capture and bounded test APIs.
// This module never issues a display switch or a USB watch command.
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
  let lan = null;             // latest lan object from state.get / lan.peers
  let requestError = null;    // code of the failed lan.pair.* request
  let windowOwned = false;    // we opened the inbound pairing window
  let dialPeer = null;        // discovered peer the primary is connecting to
  let resolveDraft = { mixed: null, replacement: null, aliases: {} }; // unanswered lan.pair.resolve answers
  let profileNote = null;     // latest lan.profile report (owner data)
  let peerName = '';          // display name of the pairing peer
  // Null leaves onboarding inactive while the settings dialog is in use.
  let mode = null;

  let generation = 0;
  let session = null;
  const retiredSessions = new Set();
  const phaseRank = { connecting: 0, await_confirm: 1, await_peer_confirm: 2, peer_confirmed: 2,
    await_resolve: 3, ready_to_commit: 4, committing: 4, committed: 5, peer_committed: 6 };
  const terminal = () => Boolean(requestError) || ['committed', 'peer_committed', 'failed', 'cancelled'].includes(note?.phase);
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
    const cancel = session && !['committed', 'peer_committed', 'failed', 'cancelled'].includes(note?.phase);
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
    return (lan?.issues || []).filter(issue => ['E_LAN_ID_CONFLICT', 'E_LAN_ID_MIGRATION_REQUIRED', 'E_LAN_IDENTITY_IO', 'E_LAN_TOPOLOGY_MISSING_SELF', 'E_LAN_SETUP_PENDING'].includes(issue?.code))
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
    return `<p role="alert" class="pair-error${compact ? ' pair-pending' : ''}">${t(copy.id, { p0: esc(copy.code), p1: esc(phase.message || '') })}</p>`;
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

  // ---- role-free onboarding -------------------------------------------------
  let discoveryTimer = null, followTimer = null, profileTimer = null;
  let flowGeneration = 0, initiator = false, busy = false, failedMonitors = [], recoveryMode = null;
  let detected = [], skipped = new Set(), capture = null, testRun = null;
  const committed = () => ['committed', 'peer_committed'].includes(note?.phase);
  const guideVisible = () => !byId('guide-page')?.classList.contains('hidden');
  const head = (title, copy = '') => `<div class="wizard-head"><h1>${title}</h1>${copy ? `<p class="sub">${copy}</p>` : ''}</div>`;
  const button = (action, id, disabled = false, link = false) => `<button class="${link ? 'link pair-link' : 'primary'}" data-pair-action="${action}" ${disabled ? 'disabled' : ''}>${t(id)}</button>`;
  const footer = (right = '', left = '') => `<div class="wizard-footer">${left || '<span></span>'}${right}</div>`;
  const rowsHtml = rows => `<div class="pair-monitors">${rows.map(row => `<div class="pair-monitor"><div class="pair-monitor-copy"><strong>${esc(row.name)}</strong>${row.copy ? `<p class="hint">${row.copy}</p>` : ''}</div><span class="settings-tag">${row.tag}</span></div>`).join('')}</div>`;
  const instructions = ids => `<ol class="v2-instructions">${ids.map((id, i) => `<li><span class="number">${i + 1}</span><span>${t(id)}</span></li>`).join('')}</ol>`;
  function localNamesByFingerprint() {
    return new Map((window.__kvmflowDisplayFingerprints || []).map((fp, i) => [fp, model.displays[i]]));
  }
  const profileRows = () => View.profileRows(profileNote, localNamesByFingerprint());
  function canTest() {
    return committed() && profileNote && !View.pendingCount(profileRows())
      && note.config_merged !== false && window.KVMFlowPairingConfig.hasTopology(session?.deviceId) && !requestError && !busy;
  }
  const waitingRows = () => View.waitingPeers(lan, memberIds());
  function peerCard(row, selectable = false) {
    const tag = selectable ? 'v2.waiting.tag' : 'v2.connecting';
    return `<${selectable ? 'button type="button"' : 'div'} class="pair-monitor v2-peer${selectable && dialPeer?.deviceId === row.deviceId ? ' selected' : ''}" ${selectable ? `data-v2-peer="${esc(row.deviceId)}" aria-pressed="${dialPeer?.deviceId === row.deviceId}"` : ''}><span class="pair-monitor-copy"><strong>${esc(row.name)}</strong><span class="hint">${esc(hostOf(row.addr))}</span></span><span class="settings-tag">${t(tag)}</span></${selectable ? 'button' : 'div'}>`;
  }
  function wizardHtml() {
    switch (mode) {
      case 'start': case 'detect': return head(t('v2.start.title'), t('v2.start.copy')) + '<div class="v2-space"></div>' + footer(button('start', 'v2.start', busy));
      case 'read-error': return head(t('v2.read.title'), detected.length ? '' : t('v2.ddc')) + rowsHtml(detected.map(row => ({ ...row, copy: row.ready ? '' : t('v2.ddc'), tag: t(row.ready ? 'v2.normal' : 'v2.read.tag') }))) + footer(button('read-retry', 'retry', busy), button('read-skip', 'v2.read.skip', busy || !detected.some(r => !r.ready), true));
      case 'search': return head(t('v2.connect.title')) + `<p class="hint" role="status">${t('lan.searching')}</p>` + footer('', button('v2-cancel', 'cancel', false, true));
      case 'wait': return head(t('v2.wait.title')) + instructions(['v2.wait.usb', 'v2.wait.display', 'v2.wait.open']) + `<div class="v2-address"><span class="hint">${t('v2.local.ip')}</span><strong>${esc(hostOf(lan?.self?.addr))}</strong></div>` + footer('', `<div class="v2-wait-actions"><span class="hint" role="status">${t('v2.wait.status')}</span>${button('ip', 'v2.ip.connect', false, true)}</div>`);
      case 'select': return head(t('v2.connect.title'), t('v2.select.copy')) + `<div class="pair-monitors">${waitingRows().map(row => peerCard(row, true)).join('')}</div>` + footer(button('v2-connect', 'lan.connect', !waitingRows().some(row => row.deviceId === dialPeer?.deviceId)));
      case 'connecting': return head(t('v2.connect.title')) + (dialPeer ? `<div class="pair-monitors">${peerCard(dialPeer)}</div>` : `<p class="hint" role="status">${t('v2.connecting')}</p>`) + footer('', button('v2-cancel', 'cancel', false, true));
      case 'confirm': return head(t('v2.connected', { p0: esc(peerName) })) + (profileNote ? rowsHtml(profileRows().map(row => ({ name: row.localName || row.name, copy: row.pending ? t('v2.pending.copy') : '', tag: t(row.pending ? 'v2.pending' : 'v2.ready') }))) : `<p class="hint" role="status">${t('lan.profile.waiting')}</p>`) + (note?.config_merged === false ? `<p class="hint">${t('lan.error.setup.pending')}</p>` : '') + footer(button('v2-next', 'v2.next', !canTest()));
      case 'try': case 'try-hint': return head(t('v2.test.title'), t('v2.test.copy', { p0: esc(peerName) })) + `<div class="callout v2-test" role="status">${t(mode === 'try-hint' ? 'v2.test.hint' : 'v2.test.wait', { p0: esc(peerName) })}</div>` + footer('', mode === 'try' ? button('v2-finish', 'v2.skip', busy, true) : '');
      case 'done': return head(t('v2.done.title')) + '<div class="v2-space"></div>' + footer(button('v2-finish', 'v2.done'));
      case 'test-error': return head(t('v2.test.error'), t('v2.test.error.copy', { p0: esc(failedMonitors.map(row => row.summary).join(t('mixed.list.separator'))) })) + rowsHtml((model.displays || []).map((name, i) => { const failed = failedMonitors.some(row => row.fingerprint === window.__kvmflowDisplayFingerprints?.[i]); return { name, copy: failed ? t('v2.ddc') : '', tag: t(failed ? 'v2.unswitched' : 'v2.ready') }; })) + footer(button('test-retry', 'retry', busy), button('feedback', 'v2.feedback', false, true));
      case 'ip': return head(t('v2.notfound')) + instructions(['v2.ip.open', 'v2.ip.network']) + `<form class="v2-ip-form"><label for="v2-ip">${t('v2.ip.connect')}</label><div class="v2-ip-row"><input id="v2-ip" name="addr" aria-describedby="v2-ip-hint" autocomplete="off" spellcheck="false" required placeholder="192.168.31.243"><button class="secondary" type="submit">${t('lan.connect')}</button></div><p id="v2-ip-hint" class="hint">${t('v2.ip.hint')}</p></form>` + footer(button('search', 'v2.search'));
      case 'error': return head(t('v2.error.title'), t('v2.error.copy')) + '<div class="v2-space"></div>' + footer(button('v2-retry', 'retry', busy));
      default: return null;
    }
  }
  function navStatus() {
    if (!mode || !guideVisible()) return null;
    return t(['wait', 'ip'].includes(mode) ? 'v2.nav.waiting' : ['done', 'confirm', 'try', 'try-hint', 'test-error'].includes(mode) ? 'v2.nav.paired' : ['search', 'select', 'connecting'].includes(mode) ? 'v2.nav.connecting' : 'nav.setup.required');
  }
  function renderWizard() {
    if (!mode || !guideVisible()) return;
    const root = byId('guide-content');
    if (!root) return;
    root.classList.add('pair-wizard', 'pairing-v2');
    byId('guide-page').classList.add('pairing-v2-page');
    const active = ['start', 'detect', 'read-error'].includes(mode) ? 0 : ['try', 'try-hint', 'test-error'].includes(mode) ? 2 : mode === 'done' ? -1 : 1;
    byId('steps').classList.remove('hidden');
    byId('steps').innerHTML = ['v2.step.local', 'v2.step.connect', 'v2.step.test'].map((id, i) => `<div class="step${active === i ? ' active' : ''}" ${active === i ? 'aria-current="step"' : ''}>${i + 1} ${t(id)}</div>`).join('');
    byId('guide-count').textContent = '';
    byId('nav-state').textContent = byId('nav-state').title = navStatus();
    const html = identityIssuesHtml() + wizardHtml();
    // Do not replace an IP input on unrelated runtime notifications.
    if (root.dataset.pairRendered !== html || root.dataset.v2Mode !== mode) {
      root.innerHTML = html; root.dataset.pairRendered = html; root.dataset.v2Mode = mode;
    }
  }
  window.renderGuide = () => {
    mode ||= 'start';
    renderWizard();
  };
  function render() { renderDialog(); renderWizard(); }

  function clearTimers() {
    clearTimeout(discoveryTimer); clearTimeout(followTimer); clearInterval(profileTimer);
    discoveryTimer = followTimer = profileTimer = null;
  }
  function startCapture() {
    if (capture) return capture.start;
    const currentFlow = flowGeneration;
    const run = capture = { stopped: false, saving: false };
    run.start = window.kvmflow.request('wizard.begin', {}).catch(error => {
      if (capture === run && !run.stopped && flowGeneration === currentFlow) failFlow(error);
    });
    return run.start;
  }
  function stopCapture() {
    const run = capture;
    if (!run) return Promise.resolve();
    run.stopped = true; capture = null;
    return run.start.then(() => window.kvmflow.request('wizard.end', {})).catch(() => {});
  }
  async function stopTest() {
    const run = testRun;
    if (!run) return;
    testRun = null; clearTimeout(followTimer);
    await run.start;
    try { await window.kvmflow.request('lan.test.stop', {}); } catch { /* leaving still closes admission */ }
  }
  async function cleanupFlow() {
    flowGeneration += 1; busy = false; clearTimers();
    await Promise.all([stopCapture(), stopTest(), endPairing(true)]);
  }
  function failFlow(error) {
    recoveryMode = committed() ? (initiator ? 'confirm' : 'done') : null;
    requestError = error?.code || 'E_LAN_PAIR_ABORTED';
    mode = 'error'; busy = false; clearTimers();
    void stopCapture(); void stopTest(); void closePairingWindow(true);
    renderWizard();
  }
  async function detectLocal() {
    if (busy) return;
    const token = flowGeneration; busy = true; mode = 'detect'; renderWizard();
    try {
      detected = await window.KVMFlowPairingConfig.detectLocal([...skipped]);
      if (token !== flowGeneration) return;
      busy = false;
      if (!detected.length || detected.some(row => !row.ready)) { mode = 'read-error'; renderWizard(); return; }
      await search();
    } catch (error) {
      if (token !== flowGeneration) return;
      busy = false; mode = 'read-error'; renderWizard();
    }
  }
  async function search() {
    const cleaning = cleanupFlow(), token = flowGeneration;
    await cleaning;
    if (token !== flowGeneration) return;
    mode = 'search'; initiator = false; dialPeer = null; profileNote = null; requestError = null;
    renderWizard();
    try {
      await window.KVMFlowPairingConfig.preparePrimary();
      if (token !== flowGeneration) return;
      if (discoverWaiting()) return;
      discoveryTimer = setTimeout(() => { if (token === flowGeneration && mode === 'search') void startWaiting(); }, 10000);
    } catch (error) { if (token === flowGeneration) failFlow(error); }
  }
  function discoverWaiting() {
    if (!['search', 'wait', 'select'].includes(mode) || busy || session && !terminal()) return false;
    const rows = waitingRows();
    if (!rows.length) {
      if (mode === 'select') {
        mode = 'search'; dialPeer = null;
        const token = flowGeneration;
        discoveryTimer = setTimeout(() => { if (token === flowGeneration && mode === 'search') void startWaiting(); }, 10000);
        renderWizard();
      }
      return false;
    }
    if (mode === 'wait') {
      const self = lan?.self?.device_id;
      if (!self) return false;
      // A deterministic total order prevents two open windows dialing each other.
      const eligible = rows.filter(row => self < row.deviceId);
      if (!eligible.length) return false;
      if (rows.length > 1) { mode = 'select'; dialPeer = null; renderWizard(); return true; }
    }
    clearTimeout(discoveryTimer);
    if (rows.length > 1 || mode === 'select') { mode = 'select'; renderWizard(); }
    else { dialPeer = rows[0]; void beginPairing(); }
    return true;
  }
  async function startWaiting() {
    if (busy) return;
    const token = flowGeneration; busy = true;
    try {
      await window.kvmflow.request('lan.setup.secondary', { host_label: model.computers?.find(c => c.local)?.name || '' });
      if (token !== flowGeneration) { await closePairingWindow(true); return; }
      mode = 'wait'; initiator = false;
      await openPairingWindow();
      if (token !== flowGeneration) return;
      if (requestError) throw new Error(requestError);
      await startCapture();
      if (token !== flowGeneration) return;
      busy = false; renderWizard(); discoverWaiting();
    } catch (error) { if (token === flowGeneration) failFlow(error); }
    finally { if (token === flowGeneration) busy = false; }
  }
  async function beginPairing() {
    if (!dialPeer?.addr || busy || session && !terminal()) return;
    const row = dialPeer;
    const token = flowGeneration;
    busy = true; initiator = true; peerName = row.name; mode = mode ? 'connecting' : null;
    const current = resetSession(row); render();
    try {
      await cleanupPending;
      if (token !== flowGeneration || session !== current) return;
      await stopCapture();
      await window.KVMFlowPairingConfig.preparePrimary();
      if (token !== flowGeneration || session !== current) return;
      // lan.pair.start opens admission itself; opening it here first would
      // briefly advertise this dialing computer as waiting.
      await pairRequest(row.manual ? 'lan.connect.manual' : 'lan.pair.start', { addr: row.addr, ...(row.manual ? { pair: true } : {}) });
      await advancePair();
    } catch (error) {
      if (session === current) { if (mode) failFlow(error); else { await closePairingWindow(true); render(); } }
    } finally { if (session === current) { busy = false; render(); } }
  }
  async function advancePair() {
    if (!mode || !session) return;
    const current = session;
    if (requestError || ['failed', 'cancelled'].includes(note?.phase)) { failFlow({ code: requestError || note.code }); return; }
    if (View.pairPhase(note).state === 'sas' && !note.local_confirmed && !current.autoConfirmed) {
      current.autoConfirmed = true;
      try { await pairRequest('lan.pair.confirm', { sas_ok: true }); }
      catch (error) { if (session === current) failFlow(error); return; }
      if (session !== current) return;
    }
    if (note?.phase === 'await_resolve' && note.initiator === true) {
      const params = View.defaultResolutions(note.issues);
      if (params === null) { failFlow({ code: 'E_LAN_PROTO' }); return; }
      const key = JSON.stringify(params);
      if (Object.keys(params).length && current.autoResolved !== key) {
        current.autoResolved = key;
        try { await pairRequest('lan.pair.resolve', params); }
        catch (error) { if (session === current) failFlow(error); return; }
        if (session !== current) return;
      }
    }
    if (committed() && !current.completionPending && !current.completionDone) {
      current.completionPending = true;
      try {
        await closePairingWindow();
        await stopCapture();
        await window.KVMFlowPairingConfig.refresh();
        if (session !== current) return;
        current.completionDone = true;
        if (initiator) {
          mode = 'confirm';
          await window.kvmflow.request('lan.profile.refresh', {});
          if (session !== current) return;
          profileTimer = setInterval(() => { if (mode === 'confirm') void window.kvmflow.request('lan.profile.refresh', {}).catch(failFlow); }, 2000);
        } else if (note.config_merged === false || !window.KVMFlowPairingConfig.hasTopology(current.deviceId)) failFlow({ code: 'E_LAN_SETUP_PENDING' });
        else mode = 'done';
      } catch (error) { if (session === current) failFlow(error); }
      finally { current.completionPending = false; }
    }
    render();
  }
  async function startTry() {
    if (!canTest()) return;
    const current = session; busy = true; renderWizard();
    try {
      await window.KVMFlowPairingConfig.confirmTopology(current.deviceId);
      await window.KVMFlowPairingConfig.refresh();
      if (session !== current || mode !== 'confirm') return;
      await beginTestRun();
    } catch (error) { if (session === current) failFlow(error); }
    finally { if (session === current) { busy = false; renderWizard(); } }
  }
  async function beginTestRun() {
    clearInterval(profileTimer); mode = 'try';
    const run = testRun = { id: null, startedMs: Date.now(), localId: model.localId, peerId: session.deviceId };
    run.start = window.kvmflow.request('lan.test.start', { duration_ms: 120000 }).then(data => { run.id = data?.test_id; });
    await run.start;
    if (testRun !== run) return;
    if (!window.KVMFlowPairingConfig.hasTrigger()) await startCapture();
    renderWizard();
  }
  async function retryTest() {
    if (busy || !session || !committed()) return;
    const current = session; busy = true; renderWizard();
    try {
      await stopTest();
      await window.KVMFlowPairingConfig.refresh();
      if (session !== current || mode !== 'test-error') return;
      failedMonitors = [];
      await beginTestRun();
    } catch (error) { if (session === current) failFlow(error); }
    finally { if (session === current) { busy = false; renderWizard(); } }
  }
  function pressObserved() {
    if (!['try', 'try-hint'].includes(mode) || followTimer) return;
    const token = flowGeneration;
    followTimer = setTimeout(() => { if (token === flowGeneration && mode === 'try') { mode = 'try-hint'; renderWizard(); } }, 5000);
  }
  function acceptReport(report, sender) {
    if (!report?.test_id || !report.press_id || !Array.isArray(report.per_monitor) || !report.per_monitor.length) return;
    if (!['try', 'try-hint', 'done', 'test-error'].includes(mode)) return;
    const localId = testRun?.localId || model.localId, peerId = testRun?.peerId || session?.deviceId;
    if (sender && report.reporting_device_id && sender !== report.reporting_device_id) return;
    if (testRun && Number.isFinite(report.ts_ms) && report.ts_ms < testRun.startedMs) return;
    if (![localId, peerId].includes(report.target_device_id) || ![localId, peerId].includes(report.reporting_device_id || sender)) return;
    if (testRun && report.test_id !== testRun.id) return;
    if (!testRun && report.target_device_id !== localId) return;
    if (testRun && report.target_device_id !== peerId) return;
    if (testRun) pressObserved();
    const names = localNamesByFingerprint();
    const failed = report.per_monitor.filter(row => row.commanded !== true).map(row => {
      const fingerprint = row.edid_id || row.fingerprint;
      const index = (window.__kvmflowDisplayFingerprints || []).indexOf(fingerprint);
      return { fingerprint, name: names.get(fingerprint) || row.name || t('monitor.value', { p0: report.per_monitor.indexOf(row) + 1 }), summary: t('monitor.value', { p0: index >= 0 ? index + 1 : report.per_monitor.indexOf(row) + 1 }) };
    });
    // DDC acceptance alone never proves that a picture arrived.
    if (!testRun && failed.length) { failedMonitors = failed; mode = 'test-error'; renderWizard(); }
  }

  // ---- actions and lifecycle -------------------------------------------------
  document.addEventListener('submit', event => {
    if (!event.target.matches('.v2-ip-form')) return;
    event.preventDefault();
    const addr = event.target.elements.addr.value.trim();
    if (!addr || busy) return;
    dialPeer = { addr, name: hostOf(addr), manual: true };
    void beginPairing();
  });
  document.addEventListener('click', event => {
    const card = event.target.closest('[data-v2-peer]');
    if (card) { dialPeer = waitingRows().find(row => row.deviceId === card.dataset.v2Peer) || null; renderWizard(); return; }
    const peer = event.target.closest('[data-pair-peer]');
    if (peer && !peer.disabled) { dialPeer = peerRows().find(row => row.deviceId === peer.dataset.pairPeer); peerName = dialPeer?.name || ''; renderDialog(); return; }
    const alias = event.target.closest('[data-pair-alias]');
    if (alias && note?.initiator === true) { resolveDraft.aliases[alias.dataset.pairAliasFp] = alias.dataset.pairAlias; renderDialog(); return; }
    const replacement = event.target.closest('[data-pair-replacement]');
    if (replacement && note?.initiator === true) { resolveDraft.replacement = replacement.dataset.pairReplacement === 'true'; renderDialog(); return; }
    const mixed = event.target.closest('[data-pair-mixed]');
    if (mixed && note?.initiator === true) { resolveDraft.mixed = mixed.dataset.pairMixed; renderDialog(); return; }
    const action = event.target.closest('button[data-pair-action]');
    if (action && !action.disabled && action.closest('#pairing-body, #guide-content')) void handleAction(action.dataset.pairAction);
  });
  document.addEventListener('keydown', event => {
    if (!['Enter', ' '].includes(event.key)) return;
    const control = event.target.closest('[role="button"][data-pair-mixed], [role="button"][data-pair-replacement], [role="button"][data-pair-alias]');
    if (control) { event.preventDefault(); control.click(); }
  });
  async function handleAction(action) {
    switch (action) {
      case 'start': skipped = new Set(); detected = []; await detectLocal(); return;
      case 'read-retry': await detectLocal(); return;
      case 'read-skip': {
        const row = detected.find(row => !row.ready);
        if (!row) return;
        skipped.add(row.fingerprint);
        detected = detected.filter(item => item !== row);
        await window.KVMFlowPairingConfig.skipLocal(row.fingerprint);
        if (detected.some(item => !item.ready)) renderWizard(); else await search();
        return;
      }
      case 'search': await search(); return;
      case 'ip': { const token = flowGeneration; await stopCapture(); if (token === flowGeneration) { mode = 'ip'; renderWizard(); } return; }
      case 'v2-connect': if (waitingRows().some(row => row.deviceId === dialPeer?.deviceId)) await beginPairing(); return;
      case 'v2-cancel': await cleanupFlow(); mode = 'start'; renderWizard(); return;
      case 'v2-retry': {
        if (recoveryMode && committed()) {
          const target = recoveryMode; busy = true; renderWizard();
          try {
            await window.KVMFlowPairingConfig.refresh();
            if (!window.KVMFlowPairingConfig.hasTopology(session?.deviceId)) throw new Error('E_LAN_SETUP_PENDING');
            note.config_merged = true; requestError = null; mode = target;
            await window.kvmflow.request('lan.profile.refresh', {});
          } catch (error) { failFlow(error); } finally { busy = false; renderWizard(); }
          return;
        }
        const row = dialPeer, wasInitiator = initiator;
        const cleaning = cleanupFlow(), token = flowGeneration;
        await cleaning;
        if (token !== flowGeneration) return;
        requestError = null;
        if (wasInitiator && row) { dialPeer = row; await beginPairing(); } else await search();
        return;
      }
      case 'v2-next': await startTry(); return;
      case 'v2-finish': await cleanupFlow(); mode = 'start'; window.navigate('status'); return;
      case 'test-retry': await retryTest(); return;
      case 'feedback': byId('submit-device')?.click(); return;
      case 'connect': await beginPairing(); return;
      case 'close': case 'cancel': closeDialog(); return;
      case 'restart': { const row = dialPeer; await endPairing(); dialPeer = row; if (row) await beginPairing(); else await openPairingWindow(); return; }
      case 'confirm':
        if (!session || session.confirmPending || View.pairPhase(note).state !== 'sas' || View.confirmedFlags(note).local || !note?.sas) return;
        { const current = session; current.confirmPending = true;
          try { await pairRequest('lan.pair.confirm', { sas_ok: true }); } catch { /* dialog renders the error */ } finally { if (session === current) current.confirmPending = false; render(); } return; }
      case 'mismatch': try { await pairRequest('lan.pair.confirm', { sas_ok: false }); } catch {} return;
      case 'resolve': {
        const params = resolveParams();
        if (!params || !session || session.resolvePending) return;
        const current = session; current.resolvePending = true;
        try { await pairRequest('lan.pair.resolve', params); } catch {} finally { if (session === current) current.resolvePending = false; render(); } return;
      }
      case 'finish': closeDialog({ cancel: !committed() }); return;
    }
  }
  function openDialog({ admit = true } = {}) {
    mode = null;
    const root = dialog(); if (!root) return;
    if (admit) void openPairingWindow();
    try { root.showModal(); } catch { root.setAttribute('open', ''); }
    root.dataset.rendered = ''; renderDialog();
  }
  function closeDialog({ cancel = !terminal() } = {}) {
    if (cancel) void endPairing(); else if (windowOwned) void closePairingWindow();
    const root = dialog(); if (!root) return;
    if (root.open) { ignoredCloseEvents += 1; root.close(); }
    root.removeAttribute('open'); root.dataset.rendered = '';
  }
  dialog()?.addEventListener('cancel', event => { event.preventDefault(); closeDialog(); });
  dialog()?.addEventListener('close', () => { if (ignoredCloseEvents) { ignoredCloseEvents -= 1; return; } closeDialog(); });
  function leavePairing() {
    mode = null; void cleanupFlow(); closeDialog({ cancel: false });
  }
  window.kvmflow?.onNotification?.('client.hidden', leavePairing);
  window.addEventListener('pagehide', leavePairing);
  const originalNavigate = window.navigate;
  window.navigate = (page, options) => {
    if (page !== 'guide' && (mode || windowOwned || session || dialog()?.open)) leavePairing();
    return originalNavigate(page, options);
  };

  // ---- notifications --------------------------------------------------------
  window.kvmflow?.onNotification?.('wizard.candidates', async data => {
    const run = capture, token = flowGeneration;
    if (!run || run.stopped || run.saving || !['wait', 'connecting', 'try', 'try-hint'].includes(mode)) return;
    const trigger = window.KVMFlowPairingConfig.triggerFromCandidates(data);
    if (!trigger) return;
    run.saving = true;
    try {
      await stopCapture();
      if (token !== flowGeneration) return;
      await window.KVMFlowPairingConfig.saveTrigger(trigger);
      if (token === flowGeneration) { if (testRun) pressObserved(); renderWizard(); }
    } catch (error) { if (token === flowGeneration) failFlow(error); }
  });
  window.kvmflow?.onNotification?.('lan.pair', async data => {
    if (!acceptPair(data)) return;
    if (['failed', 'cancelled'].includes(note?.phase)) log(t('lan.log.pair.failed'), t(View.errorCopy(note.code).id, { p0: note.code || '', p1: note.message || '' }));
    const current = session;
    peerName = data.peer_name?.trim() || View.discoveryList(lan).find(row => row.deviceId === session.deviceId)?.name || peerName;
    if (mode) {
      if (data.initiator === false) initiator = false;
      if (['wait', 'ip', 'search', 'select'].includes(mode)) mode = 'connecting';
      await advancePair();
    } else if (committed()) {
      await closePairingWindow();
      try { await window.KVMFlowPairingConfig.refresh(); } catch (error) { if (session === current) requestError = error?.code || 'E_LAN_CONFIG_REFRESH'; }
    }
    render();
  });
  window.kvmflow?.onNotification?.('lan.peers', data => {
    if (Array.isArray(data?.peers)) lan = { ...(lan || {}), peers: data.peers };
    discoverWaiting(); render();
  });
  window.kvmflow?.onNotification?.('lan.status', data => {
    if (!data || typeof data !== 'object') return;
    lan = { ...(lan || {}), ...(data.lan || data) };
    discoverWaiting(); render();
  });
  window.kvmflow?.onNotification?.('lan.profile', async data => {
    const current = session;
    if (!Array.isArray(data?.monitors)) {
      if (data?.phase === 'inputs_learned') void window.kvmflow.request('lan.profile.refresh', {}).catch(failFlow);
      return;
    }
    // Settings still follows every owner, but a stale/foreign owner cannot
    // unlock the selected peer's Next button.
    try { await window.KVMFlowPairingConfig.refresh(); }
    catch (error) { if (session === current) requestError = error?.code || 'E_LAN_CONFIG_REFRESH'; }
    if (session !== current || !session?.deviceId || data.device_id !== session.deviceId || (data.profile_rev ?? 0) < (profileNote?.profile_rev ?? 0)) return;
    profileNote = data;
    if (committed() && window.KVMFlowPairingConfig.hasTopology(session.deviceId)) note.config_merged = true;
    if (requestError === 'E_LAN_CONFIG_REFRESH') requestError = null;
    render();
  });
  window.kvmflow?.onNotification?.('lan.setup', async data => {
    if (!['topology_ready', 'trigger_adopted', 'trigger_unmatched'].includes(data?.phase)) return;
    const current = session, token = flowGeneration;
    try {
      await window.KVMFlowPairingConfig.refresh();
      if (session !== current || token !== flowGeneration) return;
      if (data.phase === 'topology_ready' && note) note.config_merged = true;
      // Adoption may be retried after a USB arrival. The config is the source
      // of truth; device identifiers in `missing` never enter product copy.
      if (window.KVMFlowPairingConfig.hasTrigger() && ['try', 'try-hint'].includes(mode)) await stopCapture();
      render();
    } catch (error) { if (mode && session === current && token === flowGeneration) failFlow(error); }
  });
  window.kvmflow?.onNotification?.('lan.sync', async data => {
    if (Array.isArray(data?.resolved)) lan = { ...(lan || {}), issues: (lan?.issues || []).filter(issue => !data.resolved.includes(issue.code)) };
    if (Array.isArray(data?.issues)) lan = { ...(lan || {}), issues: data.issues };
    render();
  });
  window.kvmflow?.onNotification?.('switch.report', acceptReport);
  window.kvmflow?.onNotification?.('lan.test', data => {
    if (testRun && data?.test_id !== testRun.id) return;
    for (const event of data?.events || []) {
      if (!['test.event', 'switch.report'].includes(event?.kind) || event.body?.test_id !== data.test_id) continue;
      acceptReport(event.body, event.device_id);
    }
  });
  function acceptLanState(state) {
    if (state?.lan) { lan = state.lan; discoverWaiting(); render(); }
  }
  window.KVMFlowPairingStateHook = acceptLanState;
  window.KVMFlowPairing = {
    openDialog, closeDialog, acceptLanState, renderWizard,
    activeMode: () => mode, navStatus,
    // Entry points use the same role-free flow, including re-running setup.
    startPrimaryAdd: () => { window.navigate('guide'); mode = 'start'; renderWizard(); },
    startRoleChoice: () => { window.navigate('guide'); mode = 'start'; renderWizard(); },
    pairWith: deviceId => {
      const row = View.discoveryList(lan).find(entry => entry.deviceId === deviceId);
      if (!row?.addr || !row.available) return;
      if (window.KVMFlowPairingConfig?.isTrusted(deviceId)) { void window.kvmflow.request('lan.connect.manual', { addr: row.addr }).catch(() => {}); return; }
      if (session && !terminal()) return;
      mode = null; dialPeer = row; peerName = row.name; openDialog(); void beginPairing();
    },
  };
  window.addEventListener('language-changed', () => { if (dialog()) dialog().dataset.rendered = ''; render(); });
})();
