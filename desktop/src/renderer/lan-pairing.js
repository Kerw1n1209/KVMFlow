// LAN pairing UI: role-free onboarding and the settings pairing dialog. State comes from the sidecar through lan.pair / lan.peers /
// lan.profile / lan.setup notifications (LAN_RUNTIME_PROTOCOL.md); the only
// requests use pairing, profile, setup, wizard capture and bounded test APIs.
// This module never issues a display switch or a USB watch command.
(() => {
  const t = (id, params) => window.KVMFlowI18n.t(id, params);
  const View = window.KVMFlowPairingView;
  const esc = (value) => escapeHtml(value ?? '');
  const byId = (id) => document.getElementById(id);
  // Design copy shows the host without the pairing port ("Windows 192.168.1.23").
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
  let resolveDraft = { replacement: null, aliases: {} }; // unanswered lan.pair.resolve answers
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
    resolveDraft = { replacement: null, aliases: {} };
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
      if (detail.kind === 'mixed') params.mixed = detail.default || 'basis';
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
        return `<h2 id="pairing-title">${t('lan.resolve.title')}</h2>
          <p class="sub">${t('lan.resolve.copy')}</p>
          ${additionalChoicesHtml()}
          ${resolveExtras()}
          <div class="confirmation-actions">
            <button class="secondary" data-pair-action="cancel">${t('lan.pair.cancel')}</button>
            ${openChoices().length ? `<button class="primary" data-pair-action="resolve" ${!resolveParams() || session?.resolvePending ? 'disabled' : ''}>${t('lan.resolve.confirm')}</button>` : ''}
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
    renderPeerList();
  }

  function renderPeerList() {
    const list = byId('pair-peer-list');
    if (!list) return;
    const rows = peerRows();
    const items = rows.map(row => `<button type="button" class="pair-peer${dialPeer?.deviceId === row.deviceId ? ' selected' : ''}${row.available && row.addr ? '' : ' offline'}"
        data-pair-peer="${esc(row.deviceId)}" aria-pressed="${dialPeer?.deviceId === row.deviceId}" ${row.available && row.addr ? '' : 'disabled'}>
        <span class="pair-peer-copy"><strong>${esc(row.name)}</strong>
        <span class="hint">${esc([row.platform, hostOf(row.addr)].filter(Boolean).join(' ') || t('lan.peer.unreachable'))}</span></span>
        ${dialPeer?.deviceId === row.deviceId ? `<span class="settings-tag">${t('lan.selected')}</span>` : ''}</button>`).join('');
    const html = items || `<p class="hint pair-empty">${t(lan?.enabled === false ? 'lan.list.empty.offline' : 'lan.list.empty')}</p>`;
    if (list.dataset.rendered !== html) { list.dataset.rendered = html; list.innerHTML = html; }
    const connect = byId('pair-connect');
    if (connect) connect.disabled = !(dialPeer?.available && dialPeer?.addr);
  }

  // ---- role-free onboarding -------------------------------------------------
  // Full-screen pages (onboarding-view.js): welcome → theme → this computer →
  // connect → pair → test → done. Modes keep the names the runtime glue and
  // diagnostics already use; `pageOf` maps them to the page that shows them.
  const Ob = window.KVMFlowOnboardingView;
  // Short pauses let a finished step register before the next page replaces it.
  const PAIRED_PAUSE_MS = 900, SWITCHED_PAUSE_MS = 1100, HINT_MS = 5000, ROW_EXIT_MS = 130;
  let followTimer = null, profileTimer = null, advanceTimer = null;
  // The 2 s poll retries a failed profile refresh; only a run of failures
  // (about 10 s) ends the flow, so one dropped request cannot.
  let profileFailures = 0;
  function pollProfile() {
    const current = session;
    return window.kvmflow.request('lan.profile.refresh', {}).then(() => { profileFailures = 0; }, error => {
      if (session !== current || mode !== 'confirm') return;
      if (++profileFailures >= 5) failFlow(error);
    });
  }
  let flowGeneration = 0, initiator = false, busy = false, failedMonitors = [], recoveryMode = null;
  let detected = [], skipped = new Set(), capture = null, testRun = null;
  // Pairing confirms the code automatically unless the user asked to compare it.
  let manualSas = false, ipOpen = false, ipError = '', errorInPairing = false;
  // Which entry started the flow: first-run setup, or the "add a computer"
  // page. Pairing pages are shared; going back returns to where it began.
  let entry = 'setup', addState = 'idle', addRefreshing = false, addDial = null;
  // Setup's choice of switching method. Until the user picks one, an existing
  // USB switch keeps its mode. Adding a computer follows what is in use.
  let flowKind = null, hotkeyDraft = window.KVMFlowHotkey.DEFAULT, stopHotkeyRecording = null, hotkeyTaken = false;
  const Km = window.KVMFlowKm;
  const defaultKind = () => (Km.status()?.enabled || !window.KVMFlowPairingConfig.hasTrigger() ? 'hotkey' : 'switch');
  const currentKind = () => (entry === 'setup' && flowKind) || defaultKind();
  const hotkeyFlow = () => currentKind() === 'hotkey';
  Km.onChange(() => { if (mode === 'perms') renderWizard(); });
  let appInfo = null;
  window.kvmflow?.info?.then?.(info => { appInfo = info; if (mode === 'welcome') renderWizard(); }, () => {});
  const PLATFORM_NAMES = { macos: 'macOS', windows: 'Windows', linux: 'Linux' };
  const selfPlatform = () => PLATFORM_NAMES[appInfo?.platform] || '';
  const committed = () => ['committed', 'peer_committed'].includes(note?.phase);
  const guideVisible = () => !byId('guide-page')?.classList.contains('hidden');
  const addVisible = () => byId('add-page') ? !byId('add-page').classList.contains('hidden') : false;
  function localNamesByFingerprint() {
    return new Map((window.__kvmflowDisplayFingerprints || []).map((fp, i) => [fp, model.displays[i]]));
  }
  const profileRows = () => View.profileRows(profileNote, localNamesByFingerprint());
  function canTest() {
    return committed() && profileNote && !View.pendingCount(profileRows())
      && note.config_merged !== false && window.KVMFlowPairingConfig.hasTopology(session?.deviceId) && !requestError && !busy;
  }
  const pairableRows = () => View.pairablePeers(lan, memberIds());
  const connectRows = () => pairableRows().map(row => ({ deviceId: row.deviceId, name: row.name, detail: [row.platform, hostOf(row.addr)].filter(Boolean).join(' ') }));
  const validAddress = value => {
    const match = /^(\d{1,3}(?:\.\d{1,3}){3})(?::(\d{1,5}))?$/.exec(value);
    return Boolean(match) && match[1].split('.').every(part => Number(part) <= 255)
      && (match[2] === undefined || (Number(match[2]) >= 1 && Number(match[2]) <= 65535));
  };

  function pageOf() {
    switch (mode) {
      case 'detect': case 'local': return 'local';
      case 'connecting': case 'confirm': return 'pair';
      case 'try': case 'try-hint': case 'try-ok': case 'test-error': return 'test';
      case 'error': return errorInPairing ? 'pair' : 'error';
      default: return mode;
    }
  }
  function pairPeer() {
    const id = session?.deviceId || dialPeer?.deviceId;
    const row = id ? View.discoveryList(lan).find(entry => entry.deviceId === id) : null;
    return { peer: peerName || dialPeer?.name || row?.name || '', peerPlatform: row?.platform || dialPeer?.platform || '' };
  }
  // A closed window on the computer we dialed means different things
  // depending on what its discovery record says about pairing requests.
  function refusalCopy() {
    if (requestError !== 'E_LAN_PAIR_WINDOW' || !initiator || entry !== 'add') return null;
    const id = session?.deviceId || dialPeer?.deviceId;
    const row = id ? View.discoveryList(lan).find(item => item.deviceId === id) : null;
    if (!row) return null;
    return row.requests === null ? 'v2.error.upgrade' : row.requests ? 'v2.error.declined' : 'v2.error.requests.off';
  }
  function errorCopy() {
    const copy = View.setupErrorCopy(requestError), refusal = refusalCopy();
    return { title: esc(t(copy.title)), copy: esc(t(refusal || copy.copy, copy.params)) };
  }
  // The dialer confirmed; a computer that is not on its add page shows our
  // attempt as a request, so tell the user where to accept it.
  function waitingAccept() {
    if (!initiator || note?.local_confirmed !== true || note?.peer_confirmed === true) return false;
    const id = session?.deviceId || dialPeer?.deviceId;
    const row = id ? View.discoveryList(lan).find(item => item.deviceId === id) : null;
    return row ? !row.waiting && row.requests !== false : false;
  }
  function pageModel(page) {
    const issues = identityIssuesHtml();
    switch (page) {
      case 'welcome': return { version: appInfo?.version || '' };
      case 'theme': return { preference: window.KVMFlowTheme?.preference || 'system', issues };
      case 'mode': return { kind: currentKind(), issues };
      case 'perms': {
        const granted = Km.status()?.permissions || {};
        return { issues, canNext: Boolean(granted.accessibility && granted.input_monitoring), rows: [
          { id: 'accessibility', title: 'km.perm.accessibility', copy: 'km.perm.accessibility.copy', granted: granted.accessibility },
          { id: 'input_monitoring', title: 'km.perm.input', copy: 'km.perm.input.copy', granted: granted.input_monitoring },
        ] };
      }
      case 'hotkey': return { keys: Km.keys(hotkeyDraft), recording: Boolean(stopHotkeyRecording), taken: hotkeyTaken, busy, issues };
      case 'local': return { reading: mode === 'detect', rows: detected, busy, issues,
        canNext: !busy && mode === 'local' && detected.length > 0 && detected.every(row => row.ready) };
      case 'connect': return { busy, issues, ipOpen, ipError, manual: manualSas, offline: lan?.enabled === false, hotkey: hotkeyFlow(),
        selfIp: hostOf(lan?.self?.addr), rows: connectRows() };
      case 'pair': {
        const base = { ...pairPeer(), selfPlatform: selfPlatform(), busy, skippable: entry !== 'add', issues };
        if (mode === 'error') return { ...base, state: 'fail', error: errorCopy() };
        if (mode === 'confirm') {
          const notes = [];
          if (initiator && !profileNote) notes.push(esc(t('lan.profile.waiting')));
          if (note?.config_merged === false) notes.push(esc(t('lan.error.setup.pending')));
          const pendingRows = initiator && profileNote ? profileRows().filter(row => row.pending)
            .map(row => ({ name: row.localName || row.name, copy: esc(t('v2.pending.copy')), tag: esc(t('v2.pending')) })) : [];
          return { ...base, state: 'ok', notes, pendingRows };
        }
        const phase = View.pairPhase(note);
        const sas = manualSas && phase.state === 'sas' && note?.sas && !phase.flags.local && !session?.confirmPending;
        return { ...base, state: sas ? 'sas' : 'run', sas: note?.sas, waitingAccept: !sas && waitingAccept() };
      }
      case 'test': {
        const base = { ...pairPeer(), selfPlatform: selfPlatform(), busy, issues, keys: hotkeyFlow() ? Km.keys(Km.status()?.hotkey || hotkeyDraft).join(' + ') : '' };
        if (mode !== 'test-error') return { ...base, state: mode === 'try-ok' ? 'ok' : 'wait', hint: mode === 'try-hint' };
        const failedRows = (model.displays || []).map((name, i) => {
          const failed = failedMonitors.some(row => row.fingerprint === window.__kvmflowDisplayFingerprints?.[i]);
          return { name, copy: failed ? esc(t('v2.ddc')) : '', tag: esc(t(failed ? 'v2.unswitched' : 'v2.ready')), bad: failed };
        });
        return { ...base, state: 'error', failedRows, failedNames: failedMonitors.map(row => row.summary).join(t('mixed.list.separator')) };
      }
      case 'error': return { ...errorCopy(), busy, skippable: entry !== 'add', issues };
      default: return { issues };
    }
  }
  function navStatus() {
    if (!mode || !guideVisible()) return null;
    return t(mode === 'connect' ? 'v2.nav.waiting' : ['done', 'confirm', 'try', 'try-hint', 'try-ok', 'test-error'].includes(mode) ? 'v2.nav.paired' : mode === 'connecting' ? 'v2.nav.connecting' : 'nav.setup.required');
  }
  // Same page, new data: patch the live regions so discovery updates never
  // reset the IP draft, the open IP drawer or the entrance animation.
  function patchRegions(root, html) {
    const next = document.createElement('template');
    next.innerHTML = html;
    for (const region of next.content.querySelectorAll('[data-region]')) {
      const live = root.querySelector(`[data-region="${region.dataset.region}"]`);
      if (live && live.innerHTML !== region.innerHTML) live.innerHTML = region.innerHTML;
    }
    const submit = root.querySelector('.v2-ip-form button[type="submit"]');
    if (submit) submit.disabled = busy;
  }
  function addModel() {
    const issues = identityIssuesHtml();
    if (addState === 'error' || (mode === 'error' && !guideVisible())) return { state: 'error', error: errorCopy(), busy, issues };
    if (addState === 'local') return { state: 'local', busy, issues };
    return { state: addState === 'ready' ? 'ready' : 'loading', rows: addState === 'ready' ? connectRows() : [], busy, issues, ipError,
      manual: manualSas, offline: lan?.enabled === false, refreshing: addRefreshing,
      selfName: model.computers?.find(c => c.local)?.name || '', selfIp: hostOf(lan?.self?.addr) };
  }
  function renderAdd() {
    const root = byId('add-content');
    if (!root || !addVisible()) return;
    const data = addModel(), html = Ob.add(data);
    if (root.dataset.addRendered === html) return;
    if (data.state === 'ready' && root.dataset.addState === 'ready') {
      patchRegions(root, html);
      // The refresh spin lives outside the patched regions.
      root.querySelector('.add-refresh')?.classList.toggle('spin', addRefreshing);
      root.dataset.addRendered = html;
      return;
    }
    const draft = root.querySelector('.add-ip input')?.value;
    root.innerHTML = html;
    root.dataset.addRendered = html; root.dataset.addState = data.state;
    const input = root.querySelector('.add-ip input');
    if (input && draft) input.value = draft;
  }
  function renderWizard() {
    renderAdd();
    if (!mode || !guideVisible()) return;
    const root = byId('guide-content');
    if (!root) return;
    scheduleAdvance();
    root.classList.add('ob-root');
    byId('steps')?.classList.add('hidden');
    byId('guide-count').textContent = '';
    byId('app-state').textContent = navStatus();
    const page = pageOf(), data = pageModel(page);
    const locale = document.documentElement.lang;
    const html = Ob.top({ page, locale, exit: !['welcome', 'done'].includes(page), dots: entry !== 'add' })
      + `<div class="ob-view" data-ob-page="${page}">${Ob.pages[page](data)}</div>`;
    if (root.dataset.pairRendered === html) return;
    // Page states that differ only in progress keep one entrance animation.
    const key = `${page}:${['ok', 'run', 'wait'].includes(data.state) ? '' : data.state || ''}`;
    if (page === 'connect' && root.dataset.obKey === key && root.dataset.obLocale === locale) {
      patchRegions(root, html);
      root.dataset.pairRendered = html;
      return;
    }
    const draft = root.querySelector('.ob-ipform input')?.value;
    const focused = document.activeElement && root.contains(document.activeElement) ? document.activeElement.id : '';
    root.classList.toggle('ob-quiet', root.dataset.obKey === key);
    root.innerHTML = html;
    root.dataset.pairRendered = html; root.dataset.v2Mode = mode; root.dataset.obKey = key; root.dataset.obLocale = locale;
    const input = root.querySelector('.ob-ipform input');
    if (input && draft) input.value = draft;
    if (focused) byId(focused)?.focus();
  }
  window.renderGuide = () => {
    if (!mode) { mode = 'welcome'; entry = 'setup'; }
    renderWizard();
  };
  function render() { renderDialog(); renderWizard(); renderRequests(); renderRequestSetting(); }
  // Pairing from the add page continues full screen, like first-run setup.
  function showFlow() { if (entry === 'add' && !guideVisible()) originalNavigate('guide'); }
  const backToAdd = () => window.navigate('add');

  // ---- pairing requests ----------------------------------------------------
  // Another computer asked to pair while no pairing screen was open. The
  // sidecar holds it at the SAS step until the user accepts or declines it.
  let incoming = null;          // { deviceId, sessionId, name, data }
  const answered = new Set();   // request sessions already accepted, declined or ended
  let accepting = false;
  const requestName = request => request.name
    || View.discoveryList(lan).find(row => row.deviceId === request.deviceId)?.name || request.deviceId;
  function takeRequest(data) {
    if (!data?.device_id || !data.session_id || answered.has(data.session_id)) return false;
    if (session?.id === data.session_id) return false;
    if (incoming?.sessionId === data.session_id) { incoming.data = { ...incoming.data, ...data }; return false; }
    incoming = { deviceId: data.device_id, sessionId: data.session_id, name: data.peer_name?.trim() || '', data };
    return true;
  }
  function endRequest(copy) {
    if (!incoming) return;
    answered.add(incoming.sessionId);
    if (copy && typeof log === 'function') log(t(copy, { p0: requestName(incoming) }), '');
    incoming = null;
  }
  // A reloaded window learns about an unanswered request from lan state.
  // Only events end a request: a stale snapshot must not drop a live one.
  function syncRequest() {
    const pending = lan?.pair_request;
    if (!pending?.session_id) return;
    takeRequest({ phase: 'await_confirm', request: true, initiator: false, local_confirmed: false,
      device_id: pending.device_id, session_id: pending.session_id, sas: pending.sas, peer_name: pending.peer_name });
  }
  // Returns true when the event belongs to a request rather than the session.
  function requestEvent(data) {
    if (data?.request !== true || (session && data.session_id && session.id === data.session_id)) return false;
    if (['failed', 'cancelled'].includes(data.phase)) {
      if (incoming?.sessionId === data.session_id) endRequest(data.code === 'E_LAN_PAIR_DECLINED' ? null : 'pair.request.ended');
      else if (data.session_id) answered.add(data.session_id);
    } else if (takeRequest(data) && mode === 'connect' && !busy && !(session && !terminal())) {
      // The pairing screen opened after the request arrived: accept it the
      // same way that screen accepts every inbound attempt.
      void acceptIncoming();
    }
    renderRequests();
    return true;
  }
  async function acceptIncoming() {
    const request = incoming;
    if (!request || accepting || (session && !terminal())) return;
    accepting = true; answered.add(request.sessionId); incoming = null;
    try {
      if (mode !== 'connect') { await cleanupFlow(); entry = 'add'; }
      initiator = false; dialPeer = null; profileNote = null; requestError = null; errorInPairing = false; recoveryMode = null;
      peerName = requestName(request);
      resetSession({ deviceId: request.deviceId });
      acceptPair(request.data);
      session.peerName = request.name;
      mode = 'connecting'; showFlow(); render();
      await advancePair();
    } finally { accepting = false; render(); }
  }
  async function declineIncoming() {
    const request = incoming;
    if (!request) return;
    answered.add(request.sessionId); incoming = null; renderRequests();
    try { await window.kvmflow.request('lan.pair.decline', { device_id: request.deviceId }); }
    catch { /* the request already ended */ }
    if (typeof log === 'function') log(t('pair.request.declined', { p0: requestName(request) }), '');
  }
  function renderRequests() {
    const root = byId('pair-requests'), list = byId('pair-request-list');
    if (!root || !list) return;
    root.hidden = !incoming;
    const html = incoming ? `<div class="nav-request"><div class="nav-request-copy"><strong>${esc(requestName(incoming))}</strong><span>${esc(t('pair.request.copy'))}</span></div>
      <div class="nav-request-actions"><button type="button" class="secondary" data-request-action="decline">${esc(t('pair.request.decline'))}</button><button type="button" data-request-action="accept">${esc(t('pair.request.accept'))}</button></div></div>` : '';
    if (list.dataset.rendered !== html) { list.innerHTML = html; list.dataset.rendered = html; }
  }
  document.addEventListener('click', event => {
    const action = event.target.closest?.('[data-request-action]');
    if (!action || !action.closest('#pair-requests')) return;
    if (action.dataset.requestAction === 'accept') void acceptIncoming(); else void declineIncoming();
  });

  // Settings: allow requests while no pairing screen is open.
  let requestSettingPending = false;
  function renderRequestSetting() {
    const control = byId('pair-requests-toggle');
    if (!control) return;
    const known = typeof lan?.pair_requests === 'boolean';
    if (known && !requestSettingPending) control.checked = lan.pair_requests;
    control.disabled = requestSettingPending || !known;
    const label = control.closest('.toggle-control')?.querySelector('.toggle-label');
    if (label) { label.dataset.i18n = control.checked ? 'on' : 'off'; label.textContent = t(label.dataset.i18n); }
  }
  document.addEventListener('change', async event => {
    if (event.target?.id !== 'pair-requests-toggle') return;
    const control = event.target, enabled = control.checked, status = byId('pair-requests-status');
    requestSettingPending = true; renderRequestSetting();
    try {
      const result = await window.kvmflow.request('lan.pair.requests', { enabled });
      lan = { ...(lan || {}), pair_requests: result?.enabled === true };
      if (status) status.textContent = '';
    } catch {
      control.checked = !enabled;
      if (status) status.textContent = t('pair.requests.save.failed');
    } finally { requestSettingPending = false; renderRequestSetting(); }
  });

  function clearTimers() {
    clearTimeout(followTimer); clearInterval(profileTimer); clearTimeout(advanceTimer);
    followTimer = profileTimer = advanceTimer = null;
    stopHotkeyRecording?.(); stopHotkeyRecording = null;
  }
  // Paired: the initiator moves on to the switch test once the other
  // computer's monitors are known; the waiting computer is done.
  function scheduleAdvance() {
    if (advanceTimer || mode !== 'confirm' || (initiator && !canTest())) return;
    const token = flowGeneration, current = session;
    advanceTimer = setTimeout(() => {
      advanceTimer = null;
      if (token !== flowGeneration || session !== current || mode !== 'confirm') return;
      if (!initiator) { mode = 'done'; renderWizard(); return; }
      if (canTest()) void startTry(); else renderWizard();
    }, PAIRED_PAUSE_MS);
  }
  // The USB switch was identified during this setup run.
  let switchLearned = false;
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
  async function cleanupFlow({ keepCapture = false } = {}) {
    flowGeneration += 1; busy = false; clearTimers();
    await Promise.all([keepCapture ? null : stopCapture(), stopTest(), endPairing(true)]);
  }
  function failFlow(error) {
    recoveryMode = committed() ? (initiator ? 'confirm' : 'done') : null;
    errorInPairing = ['connecting', 'confirm'].includes(mode) || (mode === 'error' && errorInPairing);
    // Without a code the cause is unknown; never guess a network failure.
    requestError = error?.code || (/^E_[A-Z0-9_]+$/.test(error?.message || '') ? error.message : 'E_UNKNOWN');
    // The error page shows one generic message; keep the cause in diagnostics.
    if (typeof log === 'function') log(t('v2.error.title'), `${requestError}${error?.message ? `: ${error.message}` : ''}`);
    mode = 'error'; busy = false; clearTimers();
    void stopCapture(); void stopTest(); void closePairingWindow(true);
    renderWizard();
  }
  async function detectLocal() {
    if (busy) return;
    const token = flowGeneration; busy = true; mode = 'detect'; renderWizard();
    try {
      // Users press the USB switch as soon as they leave this computer, often
      // before they reach the connect page: watch from here.
      switchLearned = false; if (!hotkeyFlow()) void startCapture();
      detected = await window.KVMFlowPairingConfig.detectLocal([...skipped]);
    } catch {
      detected = [];
    }
    if (token !== flowGeneration) return;
    busy = false; mode = 'local'; renderWizard();
  }
  // The connect page both waits to be found and lists computers that wait.
  async function enterConnect() {
    const cleaning = cleanupFlow({ keepCapture: true }), token = flowGeneration;
    await cleaning;
    if (token !== flowGeneration) return;
    mode = 'connect'; initiator = false; dialPeer = null; profileNote = null; requestError = null; ipError = ''; errorInPairing = false;
    busy = true; renderWizard();
    try {
      await window.KVMFlowPairingConfig.preparePrimary();
      if (token !== flowGeneration) return;
      await window.kvmflow.request('lan.setup.secondary', { host_label: model.computers?.find(c => c.local)?.name || '' });
      if (token !== flowGeneration) { await closePairingWindow(true); return; }
      await openPairingWindow();
      if (token !== flowGeneration) return;
      if (requestError) throw new Error(requestError);
      if (!switchLearned && !hotkeyFlow()) await startCapture();
      if (token !== flowGeneration) return;
      busy = false; renderWizard();
      if (incoming) await acceptIncoming();
    } catch (error) { if (token === flowGeneration) failFlow(error); }
    finally { if (token === flowGeneration) busy = false; }
  }
  // The add page: this computer becomes findable (as on the connect page)
  // and lists the others that are. It needs this computer's monitors first.
  async function openAdd() {
    const cleaning = cleanupFlow(), token = flowGeneration;
    await cleaning;
    if (token !== flowGeneration || !addVisible()) return;
    entry = 'add'; mode = 'connect'; addState = 'loading';
    initiator = false; dialPeer = null; profileNote = null; requestError = null; ipError = ''; errorInPairing = false; recoveryMode = null;
    busy = true; renderAdd();
    try {
      try { await window.KVMFlowPairingConfig.preparePrimary(); }
      catch (error) {
        if (error?.code !== 'E_LAN_LOCAL_SETUP') throw error;
        if (token === flowGeneration) { addState = 'local'; mode = null; }
        return;
      }
      if (token !== flowGeneration) return;
      await window.kvmflow.request('lan.setup.secondary', { host_label: model.computers?.find(c => c.local)?.name || '' });
      if (token !== flowGeneration) { await closePairingWindow(true); return; }
      await openPairingWindow();
      if (token !== flowGeneration) return;
      if (requestError) throw new Error(requestError);
      addState = 'ready';
      const row = addDial; addDial = null;
      if (row) { busy = false; dialPeer = row; await beginPairing(); }
      else if (incoming) { busy = false; await acceptIncoming(); }
    } catch (error) {
      if (token !== flowGeneration) return;
      requestError = error?.code || (/^E_[A-Z0-9_]+$/.test(error?.message || '') ? error.message : 'E_UNKNOWN');
      if (typeof log === 'function') log(t('v2.error.title'), `${requestError}${error?.message ? `: ${error.message}` : ''}`);
      addState = 'error'; mode = null; void closePairingWindow(true);
    } finally { if (token === flowGeneration) { busy = false; addDial = null; renderAdd(); } }
  }
  async function refreshNearby() {
    if (addRefreshing || addState !== 'ready') return;
    addRefreshing = true; renderAdd();
    // Long enough to read as a refresh even when the reply is instant.
    const shown = new Promise(resolve => setTimeout(resolve, 600));
    try {
      const data = await window.kvmflow.request('lan.discover.start', {});
      if (data && typeof data === 'object') lan = { ...(lan || {}), ...(data.lan || data) };
    } catch { /* discovery keeps running; the list still updates */ }
    await shown;
    addRefreshing = false; render();
  }
  async function beginPairing() {
    if (!dialPeer?.addr || busy || session && !terminal()) return;
    const row = dialPeer;
    // The sidecar pairs with one computer at a time: dialing the computer
    // that is asking already is the same as accepting it.
    if (incoming) {
      if (incoming.deviceId === row.deviceId) { await acceptIncoming(); return; }
      await declineIncoming();
    }
    const token = flowGeneration;
    busy = true; initiator = true; peerName = row.name; mode = mode ? 'connecting' : null;
    const current = resetSession(row); showFlow(); render();
    try {
      await cleanupPending;
      if (token !== flowGeneration || session !== current) return;
      await stopCapture();
      await window.KVMFlowPairingConfig.preparePrimary();
      if (token !== flowGeneration || session !== current) return;
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
    if (View.pairPhase(note).state === 'sas' && !note.local_confirmed && !current.autoConfirmed && !manualSas) {
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
          profileFailures = 0;
          profileTimer = setInterval(() => { if (mode === 'confirm') void pollProfile(); }, 2000);
        } else if (note.config_merged === false || !window.KVMFlowPairingConfig.hasTopology(current.deviceId)) failFlow({ code: 'E_LAN_SETUP_PENDING' });
        else mode = 'confirm';
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
      if (hotkeyFlow()) {
        clearInterval(profileTimer);
        hotkeyDraft = Km.status()?.hotkey || hotkeyDraft;
        mode = 'hotkey';
        return;
      }
      await beginTestRun();
    } catch (error) { if (session === current) failFlow(error); }
    finally { if (session === current) { busy = false; renderWizard(); } }
  }
  const permissionsGranted = () => {
    const granted = Km.status()?.permissions;
    return Boolean(granted?.accessibility && granted?.input_monitoring);
  };
  // System Settings has no callback for a grant; poll while the page shows.
  function watchPermissions() {
    const timer = setInterval(() => {
      if (mode !== 'perms') { clearInterval(timer); return; }
      void Km.refresh();
    }, 1500);
  }
  async function finishHotkey() {
    if (mode !== 'hotkey' || busy || stopHotkeyRecording) return;
    const current = session; busy = true; renderWizard();
    try {
      await Km.configure({ enabled: true, server_device_id: model.localId, hotkey: hotkeyDraft });
      if (session !== current || mode !== 'hotkey') return;
      await beginTestRun();
    } catch (error) { if (session === current) failFlow(error); }
    finally { if (session === current) { busy = false; renderWizard(); } }
  }
  async function beginTestRun() {
    clearInterval(profileTimer); mode = 'try';
    // The sidecar cannot test without a trigger: identify the switch from
    // this press first; startTestRun follows once it is saved.
    if (!hotkeyFlow() && !window.KVMFlowPairingConfig.hasTrigger()) { await startCapture(); renderWizard(); return; }
    await startTestRun();
  }
  async function startTestRun() {
    const run = testRun = { id: null, startedMs: Date.now(), localId: model.localId, peerId: session.deviceId };
    run.start = window.kvmflow.request('lan.test.start', { duration_ms: 120000 }).then(data => { run.id = data?.test_id; });
    await run.start;
    if (testRun !== run) return;
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
    followTimer = setTimeout(() => { if (token === flowGeneration && mode === 'try') { mode = 'try-hint'; renderWizard(); } }, HINT_MS);
  }
  function acceptReport(report, sender) {
    if (!report?.test_id || !report.press_id || !Array.isArray(report.per_monitor) || !report.per_monitor.length) return;
    if (!['try', 'try-hint', 'try-ok', 'confirm', 'done', 'test-error'].includes(mode) || (mode === 'confirm' && initiator)) return;
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
    // This computer's own commands failing is proof enough that a monitor
    // stayed put; DDC acceptance alone never proves that a picture arrived.
    const ownCommands = testRun && (report.reporting_device_id || sender) === localId && report.test_kind !== 'picture_arrived';
    if (failed.length && (!testRun || ownCommands)) {
      clearTimeout(advanceTimer); advanceTimer = null;
      failedMonitors = failed; mode = 'test-error'; renderWizard();
    }
  }
  // The other computer saw the keyboard and mouse arrive during this test.
  function acceptArrival(body, sender) {
    if (!testRun?.id || !['try', 'try-hint'].includes(mode) || body?.test_kind !== 'picture_arrived') return;
    if (body.test_id !== testRun.id || body.target_device_id !== testRun.peerId) return;
    if ((sender || body.reporting_device_id) !== testRun.peerId) return;
    clearTimeout(followTimer); followTimer = null;
    mode = 'try-ok'; renderWizard();
    const token = flowGeneration;
    advanceTimer = setTimeout(async () => {
      advanceTimer = null;
      if (token !== flowGeneration || mode !== 'try-ok') return;
      await stopTest();
      if (token !== flowGeneration || mode !== 'try-ok') return;
      mode = 'done'; renderWizard();
    }, SWITCHED_PAUSE_MS);
  }

  // ---- actions and lifecycle -------------------------------------------------
  document.addEventListener('submit', event => {
    if (!event.target.matches('.v2-ip-form')) return;
    event.preventDefault();
    const input = event.target.elements.addr, addr = input.value.trim();
    if (busy) return;
    const message = event.target.closest('.ob-ipbody, .add-ip')?.querySelector('[data-region="ip-error"]');
    if (!validAddress(addr)) {
      ipError = 'ob.ip.invalid';
      if (message) message.textContent = t(ipError);
      input.classList.remove('ob-shake'); void input.offsetWidth; input.classList.add('ob-shake');
      input.focus();
      return;
    }
    ipError = '';
    if (message) message.textContent = '';
    dialPeer = { addr, name: hostOf(addr), manual: true };
    void beginPairing();
  });
  function chooseTheme(name) {
    const apply = () => {
      window.KVMFlowTheme?.set(name);
      document.querySelectorAll('[data-ob-theme]').forEach(card => {
        const on = card.dataset.obTheme === name;
        card.classList.toggle('on', on); card.setAttribute('aria-checked', String(on));
        card.querySelector('.ob-box')?.classList.toggle('on', on);
      });
    };
    const still = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
    if (document.startViewTransition && !still) document.startViewTransition(apply); else apply();
  }
  async function chooseLanguage(value) {
    if (document.documentElement.lang === value) return;
    window.KVMFlowI18n.setPreference(value);
    try { await window.kvmflow.language?.set(value, window.KVMFlowI18n.systemLocale()); } catch { /* the choice still applies to this window */ }
  }
  function toggleIp(control) {
    const wrap = control.closest('.ob-ipwrap');
    if (!wrap) return;
    ipOpen = !ipOpen;
    wrap.classList.toggle('open', ipOpen);
    control.setAttribute('aria-expanded', String(ipOpen));
    wrap.querySelector('.ob-ipbody')?.toggleAttribute('inert', !ipOpen);
    if (ipOpen) setTimeout(() => wrap.querySelector('input')?.focus(), 120);
  }
  document.addEventListener('change', event => {
    if (event.target.matches?.('[data-ob-manual]')) manualSas = event.target.checked;
  });
  document.addEventListener('click', event => {
    const root = event.target.closest?.('#guide-content.ob-root');
    if (root) {
      const theme = event.target.closest('[data-ob-theme]');
      if (theme) { chooseTheme(theme.dataset.obTheme); return; }
      const language = event.target.closest('[data-ob-lang]');
      if (language) { void chooseLanguage(language.dataset.obLang); return; }
      const ip = event.target.closest('[data-ob-ip]');
      if (ip) { toggleIp(ip); return; }
      // A click on the welcome page finishes its entrance at once.
      const welcome = event.target.closest('.ob-view[data-ob-page="welcome"]');
      if (welcome && !event.target.closest('button') && !welcome.classList.contains('ob-skip')) { welcome.classList.add('ob-skip'); return; }
    }
    const peer = event.target.closest('[data-pair-peer]');
    if (peer && !peer.disabled) { dialPeer = peerRows().find(row => row.deviceId === peer.dataset.pairPeer); peerName = dialPeer?.name || ''; renderDialog(); return; }
    const alias = event.target.closest('[data-pair-alias]');
    if (alias && note?.initiator === true) { resolveDraft.aliases[alias.dataset.pairAliasFp] = alias.dataset.pairAlias; renderDialog(); return; }
    const replacement = event.target.closest('[data-pair-replacement]');
    if (replacement && note?.initiator === true) { resolveDraft.replacement = replacement.dataset.pairReplacement === 'true'; renderDialog(); return; }
    const action = event.target.closest('button[data-pair-action]');
    if (action && !action.disabled && action.closest('#pairing-body, #guide-content, #add-content')) void handleAction(action.dataset.pairAction, action);
  });
  document.addEventListener('keydown', event => {
    if (!['Enter', ' '].includes(event.key)) return;
    const control = event.target.closest('[role="button"][data-pair-replacement], [role="button"][data-pair-alias]');
    if (control) { event.preventDefault(); control.click(); }
  });
  async function handleAction(action, control = null) {
    switch (action) {
      case 'ob-theme': mode = 'theme'; renderWizard(); return;
      case 'ob-welcome': mode = 'welcome'; renderWizard(); return;
      case 'ob-mode': mode = 'mode'; renderWizard(); return;
      case 'ob-kind': flowKind = control?.dataset.kind || flowKind; renderWizard(); return;
      case 'mode-next':
        if (hotkeyFlow() && Km.needsPermissions()) {
          await Km.refresh();
          if (!permissionsGranted()) { mode = 'perms'; watchPermissions(); renderWizard(); return; }
        }
        skipped = new Set(); detected = []; await detectLocal(); return;
      case 'perms-next':
        if (mode !== 'perms' || !permissionsGranted()) return;
        skipped = new Set(); detected = []; await detectLocal(); return;
      case 'hotkey-change':
        if (mode !== 'hotkey' || stopHotkeyRecording) return;
        hotkeyTaken = false;
        stopHotkeyRecording = window.KVMFlowHotkey.record(hotkey => {
          stopHotkeyRecording = null;
          if (hotkey) hotkeyDraft = hotkey;
          renderWizard();
        }, {
          isTaken: hotkey => hotkey === (Km.status()?.home_hotkey || window.KVMFlowHotkey.HOME),
          onTaken: () => { hotkeyTaken = true; renderWizard(); },
        });
        renderWizard();
        return;
      case 'hotkey-next': await finishHotkey(); return;
      case 'ob-back-theme': if (entry === 'add') { backToAdd(); return; } await cleanupFlow(); mode = 'mode'; renderWizard(); return;
      case 'read-retry': await detectLocal(); return;
      case 'read-skip': {
        const fingerprint = control?.dataset.fp;
        const row = detected.find(item => !item.ready && (!fingerprint || item.fingerprint === fingerprint));
        if (!row || busy) return;
        skipped.add(row.fingerprint);
        detected = detected.filter(item => item !== row);
        await window.KVMFlowPairingConfig.skipLocal(row.fingerprint);
        const card = control?.closest('.ob-row');
        if (card) { card.classList.add('ob-gone'); setTimeout(renderWizard, ROW_EXIT_MS); } else renderWizard();
        return;
      }
      case 'local-next':
        if (mode !== 'local' || busy || !detected.length || !detected.every(row => row.ready)) return;
        if (entry === 'add') backToAdd(); else await enterConnect();
        return;
      case 'add-local': if (!busy) { entry = 'add'; await rereadLocal(); } return;
      case 'add-retry': await openAdd(); return;
      case 'add-refresh': await refreshNearby(); return;
      case 'v2-connect': {
        const row = pairableRows().find(item => item.deviceId === control?.dataset.v2Peer);
        if (!row || busy) return;
        dialPeer = row; await beginPairing(); return;
      }
      case 'v2-cancel': if (entry === 'add') backToAdd(); else await enterConnect(); return;
      case 'ob-exit':
        if (entry === 'add' && !committed()) { backToAdd(); return; }
        await cleanupFlow(); mode = null; window.navigate('status'); navState(); return;
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
        const row = dialPeer, wasInitiator = initiator, pairing = errorInPairing;
        const cleaning = cleanupFlow(), token = flowGeneration;
        await cleaning;
        if (token !== flowGeneration) return;
        requestError = null;
        if (pairing && wasInitiator && row) { dialPeer = row; mode = 'connecting'; await beginPairing(); } else if (entry === 'add') backToAdd(); else await enterConnect();
        return;
      }
      case 'v2-skip': await stopTest(); clearTimers(); mode = 'done'; renderWizard(); return;
      case 'v2-finish': await cleanupFlow(); mode = null; window.navigate('status'); return;
      case 'test-retry': await retryTest(); return;
      case 'feedback': byId('submit-device')?.click(); return;
      case 'connect': await beginPairing(); return;
      case 'close': case 'cancel': closeDialog(); return;
      case 'restart': { const row = dialPeer; await endPairing(); dialPeer = row; if (row) await beginPairing(); else await openPairingWindow(); return; }
      case 'confirm':
        if (!session || session.confirmPending || View.pairPhase(note).state !== 'sas' || View.confirmedFlags(note).local || !note?.sas) return;
        { const current = session; current.confirmPending = true; render();
          try { await pairRequest('lan.pair.confirm', { sas_ok: true }); } catch { /* the page renders the error */ } finally { if (session === current) current.confirmPending = false; render(); }
          if (mode && session === current) await advancePair();
          return; }
      case 'mismatch':
        try { await pairRequest('lan.pair.confirm', { sas_ok: false }); } catch {}
        if (mode && session) await advancePair();
        return;
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
    if (!run || run.stopped || run.saving || !['detect', 'local', 'connect', 'try', 'try-hint'].includes(mode)) return;
    const trigger = window.KVMFlowPairingConfig.triggerFromCandidates(data);
    if (!trigger) return;
    run.saving = true;
    try {
      await stopCapture();
      if (token !== flowGeneration) return;
      await window.KVMFlowPairingConfig.saveTrigger(trigger);
      switchLearned = true;
      if (token !== flowGeneration) return;
      if (['try', 'try-hint'].includes(mode) && !testRun) {
        // The identifying press could not switch the monitors yet.
        mode = 'try-hint'; renderWizard();
        await startTestRun();
      }
      renderWizard();
    } catch (error) { if (token === flowGeneration) failFlow(error); }
  });
  window.kvmflow?.onNotification?.('lan.pair', async data => {
    if (requestEvent(data)) return;
    if (!acceptPair(data)) return;
    if (['failed', 'cancelled'].includes(note?.phase)) log(t('lan.log.pair.failed'), t(View.errorCopy(note.code).id, { p0: note.code || '', p1: note.message || '' }));
    const current = session;
    // Only some events carry the name; a discovery row without one falls back to the device id.
    if (data.peer_name?.trim()) session.peerName = data.peer_name.trim();
    peerName = session.peerName || View.discoveryList(lan).find(row => row.deviceId === session.deviceId)?.name || peerName;
    if (mode) {
      if (data.initiator === false) initiator = false;
      if (mode === 'connect') { mode = 'connecting'; showFlow(); }
      await advancePair();
    } else if (committed()) {
      await closePairingWindow();
      try { await window.KVMFlowPairingConfig.refresh(); } catch (error) { if (session === current) requestError = error?.code || 'E_LAN_CONFIG_REFRESH'; }
    }
    render();
  });
  window.kvmflow?.onNotification?.('lan.peers', data => {
    if (Array.isArray(data?.peers)) lan = { ...(lan || {}), peers: data.peers };
    render();
  });
  window.kvmflow?.onNotification?.('lan.status', data => {
    if (!data || typeof data !== 'object') return;
    lan = { ...(lan || {}), ...(data.lan || data) };
    syncRequest(); render();
  });
  window.kvmflow?.onNotification?.('lan.profile', async data => {
    const current = session;
    if (!Array.isArray(data?.monitors)) {
      if (data?.phase === 'inputs_learned') void pollProfile();
      return;
    }
    // Settings still follows every owner, but a stale/foreign owner cannot
    // unlock the selected peer's switch test.
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
      if (window.KVMFlowPairingConfig.hasTrigger() && ['try', 'try-hint'].includes(mode)) {
        await stopCapture();
        if (!testRun && session === current && token === flowGeneration) await startTestRun();
      }
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
      acceptArrival(event.body, event.device_id);
    }
  });
  function acceptLanState(state) {
    if (state?.lan) { lan = state.lan; syncRequest(); render(); }
  }
  // Re-running setup from the app skips the welcome and appearance pages.
  async function rereadLocal() {
    if (mode) await cleanupFlow();
    skipped = new Set(); detected = []; mode = 'detect';
    originalNavigate('guide');
    await detectLocal();
  }
  async function restartSetup() { entry = 'setup'; await rereadLocal(); }
  async function showWelcome() {
    await cleanupFlow();
    entry = 'setup'; mode = 'welcome';
    originalNavigate('guide');
  }
  // A hidden window closed admission; coming back to the add page reopens it.
  const reopenAdd = () => { if (addVisible() && !mode && !busy && addState === 'ready') void openAdd(); };
  window.addEventListener('focus', reopenAdd);
  byId('show-welcome')?.addEventListener('click', () => { void showWelcome(); });
  document.addEventListener('visibilitychange', () => { if (document.visibilityState === 'visible') reopenAdd(); });
  window.KVMFlowPairingStateHook = acceptLanState;
  window.KVMFlowPairing = {
    openDialog, closeDialog, acceptLanState, renderWizard,
    activeMode: () => mode, navStatus,
    startPrimaryAdd: () => { void restartSetup(); },
    startRoleChoice: () => { void restartSetup(); },
    openAdd: () => openAdd(), showWelcome: () => showWelcome(),
    // Pairing from settings runs through the add page, which first makes
    // this computer ready, then continues full screen.
    addWith: deviceId => {
      const row = View.discoveryList(lan).find(item => item.deviceId === deviceId);
      if (!row?.addr || !row.available) return;
      if (window.KVMFlowPairingConfig?.isTrusted(deviceId)) { void window.kvmflow.request('lan.connect.manual', { addr: row.addr }).catch(() => {}); return; }
      if (session && !terminal()) return;
      addDial = row; window.navigate('add');
    },
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
