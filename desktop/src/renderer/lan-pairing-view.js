// Pure view rules for LAN pairing and the primary/secondary setup wizard.
// Everything here maps the sidecar LAN contract (LAN_RUNTIME_PROTOCOL.md:
// lan.pair phases, lan.peers, lan.profile, lan.setup) into wizard states and
// text parameters; it never touches the DOM or the runtime. Optional data a
// phase does not carry stays absent — the UI must not invent hardware or
// group evidence.
(() => {
  const t = (id, params) => window.KVMFlowI18n.t(id, params);
  const nonempty = (value) => typeof value === 'string' && value.trim() ? value.trim() : '';

  // The runtime sends explicit confirmation flags on every pairing event.
  // A missing flag means "not confirmed" (never infer it from the phase).
  const confirmedFlags = (note) => ({
    local: note?.local_confirmed === true,
    peer: note?.peer_confirmed === true,
  });

  // Pairing screen state machine. `note` is the latest lan.pair payload (or
  // response of a lan.pair request); `requestError` is the code of a failed
  // lan.pair.* request. Returns a state object:
  //   { state: 'idle' | 'connecting' | 'committing' | 'peer_committed' | 'cancelled' }
  //   { state: 'sas', sas?, flags }          await_confirm / *_peer_confirm
  //   { state: 'resolve', flags }            await_resolve / ready_to_commit
  //   { state: 'committed', configMerged, peerAckPending }
  //   { state: 'failed', code, message }
  function pairPhase(note, requestError) {
    if (requestError) return { state: 'failed', code: requestError };
    if (!note) return { state: 'idle' };
    switch (note.phase) {
      case 'connecting': return { state: 'connecting' };
      case 'await_confirm':
      case 'await_peer_confirm':
      case 'peer_confirmed':
        return { state: 'sas', sas: note.sas, flags: confirmedFlags(note) };
      case 'await_resolve': case 'ready_to_commit': return { state: 'resolve', flags: confirmedFlags(note) };
      case 'committing': return { state: 'committing' };
      case 'committed': return {
        state: 'committed',
        configMerged: note.config_merged !== false,
        peerAckPending: note.peer_confirmed_commit === false,
      };
      case 'peer_committed': return { state: 'committed', configMerged: note.config_merged !== false, peerAckPending: false };
      case 'cancelled': return { state: 'cancelled' };
      case 'failed': return { state: 'failed', code: note.code, message: note.message };
      default: return { state: 'idle' };
    }
  }

  // E_LAN_* codes mapped to user-readable copy. Unknown codes fall back to a
  // generic sentence that still shows the raw code for diagnostics.
  function errorCopy(code, message) {
    const id = ({
      E_LAN_PAIR_REJECTED: 'lan.error.rejected',
      E_LAN_PAIR_CANCELLED: 'lan.error.cancelled',
      E_LAN_PAIR_ABORTED: 'lan.error.aborted',
      E_LAN_PAIR_TIMEOUT: 'lan.error.timeout',
      E_LAN_TIMEOUT: 'lan.error.timeout',
      E_LAN_PAIR_ONESIDED: 'lan.error.onesided',
      E_LAN_PAIR_COMMIT: 'lan.error.commit',
      E_LAN_PAIR_RECORD: 'lan.error.record',
      E_LAN_PAIR_INTERRUPTED: 'lan.error.interrupted',
      E_LAN_PAIR_NOT_CONFIRMED: 'lan.error.not.confirmed',
      E_LAN_PAIR_WINDOW: 'lan.error.window',
      E_LAN_WINDOW_CLOSE: 'lan.error.window.close',
      E_LAN_CLOSED: 'lan.error.disconnected',
      E_LAN_DISCONNECTED: 'lan.error.disconnected',
      E_LAN_OFFLINE: 'lan.error.disconnected',
      E_LAN_BUSY: 'lan.error.busy',
      E_LAN_ADDRESS: 'lan.error.address',
      E_LAN_SETUP_PENDING: 'lan.error.setup.pending',
      E_LAN_TOPOLOGY_MISSING_SELF: 'lan.error.topology.self',
      E_LAN_ID_CONFLICT: 'lan.error.id.conflict',
      E_LAN_ID_MIGRATION_REQUIRED: 'lan.error.id.migration',
      E_LAN_IDENTITY_IO: 'lan.error.identity.io',
      E_LAN_DISABLED: 'lan.error.disabled',
      E_LAN_LOCAL_SETUP: 'lan.error.local.setup',
      E_LAN_CONFIG_REFRESH: 'lan.error.config.refresh',
      E_CONFIG_IO: 'lan.error.config.save',
      E_LAN_NOISE: 'lan.error.network',
      E_LAN_IO: 'lan.error.network',
      E_LAN_LISTEN: 'lan.error.network',
      E_LAN_GROUP_CONFLICT: 'lan.error.group.conflict',
      E_LAN_PROTO: 'lan.error.proto',
    })[code];
    return id ? { id, code } : { id: 'lan.error.generic', code, message };
  }

  // Engine values stay lowercase on the wire; the discovery row uses the
  // same product names as the design copy.
  const PLATFORM_LABELS = { macos: 'macOS', windows: 'Windows', linux: 'Linux' };
  const platformLabel = (value) => {
    const platform = nonempty(value).toLowerCase();
    return platform ? (PLATFORM_LABELS[platform] || value.trim()) : null;
  };

  // Discovery list: LAN peers that are not already group members. Dialing
  // needs `addr`; an offline peer shows as unreachable instead.
  function discoveryList(lan, memberIds = []) {
    const members = new Set(memberIds);
    return (Array.isArray(lan?.peers) ? lan.peers : [])
      .filter(peer => peer && !members.has(peer.device_id))
      .map(peer => ({
        deviceId: peer.device_id,
        name: nonempty(peer.name) || peer.device_id,
        // The protocol omits `platform` when the peer never reported one;
        // the row then renders the address alone instead of guessing an OS.
        platform: platformLabel(peer.platform),
        addr: peer.online === true && typeof peer.addr === 'string' ? peer.addr
          : typeof peer.pair_addr === 'string' ? peer.pair_addr : typeof peer.addr === 'string' ? peer.addr : null,
        available: peer.online === true || typeof peer.pair_addr === 'string',
        online: peer.online === true,
      }));
  }

  // The primary's confirmation page rows: the secondary's reported monitors
  // from `lan.profile` (live input values or pending). `localNames` maps
  // fingerprint -> this machine's label for the same physical display.
  function profileRows(profile, localNames = new Map()) {
    const monitors = Array.isArray(profile?.monitors) ? profile.monitors : [];
    let external = 0;
    return monitors.filter(monitor => !monitor?.builtin).map((monitor) => {
      external += 1;
      const value = monitor.input?.state === 'value';
      return {
        fingerprint: monitor.fingerprint,
        name: nonempty(monitor.name) || t('monitor.value', { p0: external }),
        localName: localNames.get(monitor.fingerprint) || null,
        value: value ? monitor.input.value : null,
        pending: !value,
      };
    });
  }

  // Pending rows drive "还有 N 台显示器待读取" and the disabled 完成 button.
  const pendingCount = (rows) => rows.filter(row => row.pending).length;

  // Mixed-conflict choice (pair-conflict.png): built from a `mixed` Choice
  // issue's basis/joiner MixedConfig. Each side lists its assignment as
  // mini screens; only the initiator can answer.
  function mixedChoice(issue, basisName, joinerName) {
    const detail = issue?.detail || issue;
    if (detail?.kind !== 'mixed') return null;
    const side = (config) => (Array.isArray(config?.assignment) ? config.assignment : [])
      .map(entry => ({ fingerprint: entry.fingerprint, deviceId: entry.device_id }))
      .filter(entry => entry.deviceId);
    return {
      basis: { name: basisName, enabled: detail.basis?.enabled === true, assignment: side(detail.basis) },
      joiner: { name: joinerName, enabled: detail.joiner?.enabled === true, assignment: side(detail.joiner) },
      resolved: detail.resolved === true,
    };
  }

  // Split await_resolve issues into what the primary must answer (choices,
  // initiator only) and what merely blocks the merge.
  function issueGroups(issues, initiator) {
    const list = Array.isArray(issues) ? issues : [];
    return {
      choices: initiator ? list.filter(issue => issue?.severity === 'choice') : [],
      blockers: list.filter(issue => issue?.severity === 'blocking'),
      info: list.filter(issue => issue?.severity === 'info'),
    };
  }

  // Pending-input blockers on the secondary become actionable guidance on
  // the primary: switch that monitor to the secondary once with its buttons;
  // the value is then read automatically (design principle).
  function pendingGuidance(blockers, joinerId) {
    return blockers
      .filter(issue => issue?.detail?.kind === 'input_pending'
        && (!joinerId || issue.detail.device_id === joinerId))
      .map(issue => ({ fingerprint: issue.detail.fingerprint, deviceId: issue.detail.device_id }));
  }

  // 试一下 evidence for one press direction: "commands sent" comes from a
  // switch.report with a complete per_monitor list, "picture arrived" from
  // the other computer's picture_arrived evidence. They are reported
  // separately so a DDC success is never presented as a visible switch.
  // `labelOf` maps a fingerprint/edid to a display label.
  function trySummary(reports, labelOf) {
    const label = (entry) => labelOf(entry?.edid_id || entry?.fingerprint) || entry?.edid_id || '';
    const summarize = (report) => {
      const outcomes = Array.isArray(report?.per_monitor) ? report.per_monitor : [];
      const failed = outcomes.filter(entry => entry.commanded !== true).map(label).filter(Boolean);
      const commanded = report?.commandsSent === true;
      const arrived = report?.pictureArrived === true;
      return {
        ok: commanded && arrived,
        commanded, arrived, failed,
        target: report?.target_device_id || null,
      };
    };
    return { first: reports[0] ? summarize(reports[0]) : null, second: reports[1] ? summarize(reports[1]) : null };
  }

  window.KVMFlowPairingView = {
    pairPhase, errorCopy, discoveryList, profileRows, pendingCount,
    mixedChoice, issueGroups, pendingGuidance, trySummary, confirmedFlags,
  };
})();
