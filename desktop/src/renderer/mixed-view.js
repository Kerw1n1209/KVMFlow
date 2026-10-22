// Pure view rules for mixed mode. Everything here maps runtime data (the
// `mixed` object from MIXED_PROTOCOL.md, the switch group and the optional LAN
// state) to text and flags; it never touches the DOM or the runtime.
// Optional data (LAN peers) that is absent stays absent in the result.
(() => {
  const t = (id, params) => window.KVMFlowI18n.t(id, params);
  const byPort = (a, b) => Number(a.port) - Number(b.port);
  const nonempty = (value) => typeof value === 'string' && value.trim() ? value.trim() : '';

  // ctx: { mixed, config, computers: [{id,name,local,port}], lan }
  const computerName = (ctx, id) => ctx.computers.find((computer) => computer.id === id)?.name || t('computer');
  const localComputer = (ctx) => ctx.computers.find((computer) => computer.local) || ctx.computers[0];

  // Match the runtime's local monitor order/default_display_number. Retained
  // assignments and newly discovered screens are appended without renumbering.
  function monitorOrder(ctx) {
    const order = [];
    const add = (fingerprint, label) => {
      if (!fingerprint) return;
      const known = order.find((entry) => entry.fingerprint === fingerprint);
      if (known) { if (!known.label) known.label = nonempty(label); return; }
      order.push({ fingerprint, label: nonempty(label) });
    };
    for (const monitor of ctx.config?.local_device?.monitors || []) add(monitor.fingerprint, monitor.label);
    const devices = ctx.config?.switch_group?.devices || [];
    for (const device of devices) for (const monitor of device.monitors || []) add(monitor.fingerprint, monitor.label);
    const destinations = ctx.mixed?.destinations || [];
    for (const destination of destinations) if (!destination.builtin) add(destination.fingerprint, destination.label ?? ( /^显示器 \d+$/.test(destination.display_name || '') ? '' : destination.display_name));
    for (const entry of ctx.mixed?.assignment || []) add(entry.fingerprint);
    return order;
  }

  // External screens, with the default name "Monitor N" when no label exists.
  function externalScreens(ctx) {
    const destinations = ctx.mixed?.destinations || [];
    const assignment = ctx.mixed?.assignment || [];
    return monitorOrder(ctx).map((entry, index) => {
      const destination = destinations.find(item => item.fingerprint === entry.fingerprint) || null;
      const number = Number.isInteger(destination?.default_display_number) && destination.default_display_number > 0 ? destination.default_display_number : index + 1;
      return {
        fingerprint: entry.fingerprint, index: number - 1, defaultName: !entry.label,
        name: entry.label || t('monitor.value', {p0:number}), destination,
        assigned: assignment.find(item => item.fingerprint === entry.fingerprint)?.device_id || null,
      };
    });
  }

  function builtinScreens(ctx) {
    const name = localComputer(ctx)?.name || t('this.computer');
    return (ctx.mixed?.destinations || []).filter((destination) => destination.builtin).map((destination) => ({
      fingerprint: destination.fingerprint, builtin: true, destination, name: t('mixed.builtin.screen', { p0: name }),
    }));
  }

  // "Monitor 1, 2" when every name is a default one; otherwise the names.
  function joinScreens(screens) {
    if (!screens.length) return '';
    const separator = t('mixed.list.separator');
    if (screens.every((screen) => screen.defaultName)) {
      return t('monitor.value', { p0: screens.map((screen) => screen.index + 1).join(separator) });
    }
    return screens.map((screen) => screen.name).join(separator);
  }

  function reasonList(mixed, severity) {
    return (mixed?.validation?.reasons || []).filter((reason) => reason.severity === severity);
  }

  // Why mixed mode cannot be enabled, one sentence per error reason.
  function blockReasons(ctx) {
    const mixed = ctx.mixed;
    if (!mixed) return [];
    const screens = externalScreens(ctx);
    if (mixed.validation?.ok !== true && !reasonList(mixed, 'error').length) return [t('mixed.block.generic')];
    return reasonList(mixed, 'error').map((reason) => {
      switch (reason.code) {
        case 'not_enough_screens': return t('mixed.block.not.enough.screens');
        case 'not_enough_computers': return t('mixed.block.not.enough.computers');
        case 'unknown_device': return t('mixed.block.unknown.device');
        case 'missing_input': {
          const screen = screens.find((item) => item.fingerprint === reason.fingerprint);
          return t('mixed.block.missing.input', {
            p0: reason.device_id ? computerName(ctx, reason.device_id) : t('computer'),
            p1: screen?.name || t('monitor'),
          });
        }
        case 'all_to_one_computer': {
          const owners = new Set(screens.map((screen) => screen.assigned).filter(Boolean));
          if (builtinScreens(ctx).some(screen => screen.destination.available !== false)) owners.add(localComputer(ctx)?.id);
          if (owners.size === 1) return t('mixed.block.all.to.one', { p0: computerName(ctx, [...owners][0]) });
          return t('mixed.block.none.assigned');
        }
        default: return t('mixed.block.generic');
      }
    });
  }

  // Notes that do not block enabling: unassigned screens, computers without a
  // screen and runtime warnings.
  function notes(ctx) {
    const mixed = ctx.mixed;
    if (!mixed) return [];
    const screens = externalScreens(ctx);
    const seen = new Set();
    const out = [];
    const push = (key, text) => { if (!seen.has(key)) { seen.add(key); out.push(text); } };
    for (const reason of [...reasonList(mixed, 'warning'), ...(mixed.warnings || [])]) {
      const screen = screens.find((item) => item.fingerprint === reason.fingerprint);
      switch (reason.code) {
        case 'computer_without_screen':
          push(`cws:${reason.device_id}`, t('mixed.note.computer.without.screen', { p0: computerName(ctx, reason.device_id) }));
          break;
        case 'unassigned_monitor':
          push(`um:${reason.fingerprint}`, t('mixed.note.unassigned', { p0: screen?.name || t('monitor') }));
          break;
        case 'last_local_screen':
          push('lls', t('mixed.note.last.screen'));
          break;
        case 'unknown_keyboard_holder':
          push('ukh', t('mixed.note.unknown.holder'));
          break;
        case 'holder_inferred':
          push('inferred', t('mixed.note.holder.inferred'));
          break;
        case 'missing_input':
          push(`input:${reason.fingerprint}:${reason.device_id}`, t('mixed.block.missing.input', {p0:computerName(ctx,reason.device_id),p1:screen?.name || t('monitor')}));
          break;
        case 'topology_unavailable':
          push('topology', t('mixed.note.no.detach'));
          break;
        default: break;
      }
    }
    if (externalScreens(ctx).filter(screen => screen.destination?.available === true).length === 1
      && builtinScreens(ctx).some(screen => screen.destination.available === true)) push('builtin_lid', t('mixed.note.builtin.lid'));
    if (mixed.enabled && mixed.topology_error) push('topology', t('mixed.note.no.detach'));
    return out;
  }

  // LAN synchronisation, only when the runtime reports LAN peers.
  function sync(ctx) {
    const peers = ctx.lan?.peers;
    if (!Array.isArray(peers) || !peers.length) return null;
    const named = (list) => list.map((peer) => nonempty(peer.name) || computerName(ctx, peer.device_id)).join(t('mixed.list.separator'));
    const offline = peers.filter((peer) => peer.online === false);
    if (offline.length) return { tone: 'warn', offline, names: named(offline), text: t('mixed.sync.offline', { p0: named(offline) }) };
    if (peers.some((peer) => peer.online === true && peer.synced === false)) return { tone: 'pending', offline: [], names: named(peers), text: t('mixed.sync.pending', { p0: named(peers) }) };
    if (peers.every(peer => peer.online === true && peer.synced === true)) return { tone: 'ok', offline: [], names: named(peers), text: t('mixed.sync.ok', { p0: named(peers) }) };
    return null;
  }

  // Mode bar of the status page.
  // transition: null | 'enabling' | 'disabling' | 'failed' (disable request failed).
  // The runtime's own `transition` (phase/outcome) wins over the request this
  // window made; both feed the same three states.
  function effectiveTransition(mixed, local) {
    const phase = mixed?.transition?.phase;
    if (phase === 'enabling') return 'enabling';
    if (phase === 'disabling' || phase === 'restoring') return 'disabling';
    if (local === 'failed' && mixed?.enabled === false && phase === 'idle' && mixed.transition.outcome === 'ok'
      && !mixed.pending_remote?.length && !mixed.execution?.some(entry => entry.ok === false)) return null;
    if (local) return local;
    const outcome = mixed?.transition?.outcome;
    return !mixed?.enabled && (outcome === 'partial' || outcome === 'failed') ? 'failed' : null;
  }

  function modeBar(ctx, localTransition) {
    const mixed = ctx.mixed;
    if (!mixed) return null;
    const transition = effectiveTransition(mixed, localTransition);
    const pending = Array.isArray(mixed.pending_remote) ? mixed.pending_remote : [];
    const blocked = blockReasons(ctx);
    const peers = sync(ctx);
    if (mixed.enabled) {
      const syncText = peers?.tone === 'ok' ? ` ${t('mixed.bar.synced', { p0: peers.names })}` : '';
      return {
        mode: 'mixed', title: t('mixed.mode'), tag: t('mixed.on'),
        description: transition === 'disabling' ? t('mixed.restoring') : `${t('mixed.bar.mixed')}${syncText}`,
        action: 'disable', busy: transition === 'disabling' || transition === 'enabling', blocked: [],
      };
    }
    const restoreFailed = pending.length > 0 || transition === 'failed' || mixed.execution?.some(entry => entry.ok === false);
    return {
      mode: 'normal', title: t('mixed.mode.normal'), tag: null,
      description: transition === 'disabling' ? t('mixed.restoring') : transition === 'enabling' ? t('mixed.enabling')
        : restoreFailed ? t('mixed.restore.next.switch') : t('mixed.bar.normal'),
      action: 'enable', busy: transition === 'enabling' || transition === 'disabling', blocked,
    };
  }

  // The status page screen row.
  function statusScreens(ctx) {
    const mixed = ctx.mixed;
    const out = [];
    for (const screen of externalScreens(ctx)) {
      const destination = screen.destination;
      const unavailable = !destination || destination.available === false;
      const detached = Boolean(destination) && destination.available === true && destination.connected === false;
      const showingId = destination?.current_device_id || destination?.target_device_id || null;
      out.push({
        key: screen.fingerprint, name: screen.name, builtin: false,
        fixed: Boolean(mixed?.enabled && destination?.fixed),
        unassigned: Boolean(mixed?.enabled && !screen.assigned),
        state: unavailable ? 'offline' : detached ? 'detached' : 'normal',
        showing: unavailable || !showingId ? null : computerName(ctx, showingId),
        // Without a readable input the owner is the planned one, not proven.
        assumed: !destination?.current_device_id,
        evidence: destination?.evidence,
      });
    }
    for (const screen of builtinScreens(ctx)) {
      out.push({
        key: screen.fingerprint, name: screen.name, builtin: true, fixed: true, unassigned: false,
        state: screen.destination.lid_closed ? 'closed' : screen.destination.unavailable_reason === 'builtin_closed_or_unavailable' ? 'builtin-unavailable' : screen.destination.available === false ? 'offline' : 'normal',
        showing: computerName(ctx, localComputer(ctx)?.id), assumed: false,
      });
    }
    return out;
  }

  // Computers by port order, with the optional LAN line.
  function statusComputers(ctx) {
    const holder = ctx.mixed?.keyboard_holder_device_id;
    const inferred = ctx.mixed?.holder_confidence === 'inferred';
    const peers = Array.isArray(ctx.lan?.peers) ? ctx.lan.peers : null;
    return [...ctx.computers].sort(byPort).map((computer) => {
      const peer = peers?.find((item) => item.device_id === computer.id);
      return {
        id: computer.id, name: computer.name, local: computer.local, port: computer.port,
        holder: computer.id === holder, inferred: computer.id === holder && inferred,
        lan: computer.local || typeof peer?.online !== 'boolean' ? null : peer.online ? 'connected' : 'disconnected',
      };
    });
  }

  function nextSwitch(ctx) {
    const queue = [...ctx.computers].sort(byPort);
    if (queue.length < 2) return null;
    const holder = ctx.mixed?.keyboard_holder_device_id;
    if (!holder || !queue.some(computer => computer.id === holder)) return t('mixed.next.unknown');
    const at = queue.findIndex((computer) => computer.id === holder);
    const name = queue[(at + 1) % queue.length].name;
    const mixed = ctx.mixed;
    const followers = externalScreens(ctx).filter((screen) => screen.destination?.available === true && (mixed?.enabled ? !screen.assigned : true));
    if (mixed?.enabled) {
      return followers.length
        ? t('mixed.next.mixed.partial', { p0: joinScreens(followers), p1: name })
        : t('mixed.next.mixed', { p0: name });
    }
    if (!followers.length) return t('mixed.next.keys.only', { p0: name });
    return t(followers.length > 1 ? 'mixed.next.normal.many' : 'mixed.next.normal.one', { p0: joinScreens(followers), p1: name });
  }

  // Strip messages under the desktop map.
  function banners(ctx) {
    const mixed = ctx.mixed;
    if (!mixed) return [];
    const out = [];
    const push = (key, text, dismissible = false) => { if (!out.some(entry => entry.key === key)) out.push({key,text,dismissible}); };
    const peers = sync(ctx);
    if (peers?.offline.length) {
      const offlineIds = new Set(peers.offline.map(peer => peer.device_id));
      const stuck = externalScreens(ctx).filter(screen => mixed.enabled && offlineIds.has(screen.assigned) && screen.destination?.connected === false);
      push('offline', stuck.length ? t('mixed.banner.offline.stuck', {p0:peers.names,p1:joinScreens(stuck)}) : peers.text);
    }
    if (mixed.pending_remote?.length && !mixed.enabled) {
      const names = [...new Set(mixed.pending_remote.map(item => item.device_id ? computerName(ctx,item.device_id) : t('other.computer')))];
      push('restore', t('mixed.banner.restore.pending', {p0:names.join(t('mixed.list.separator'))}));
    }
    if (mixed.enabled) for (const screen of externalScreens(ctx)) {
      if (!screen.assigned && screen.destination?.available === true) push(`unassigned_monitor:${screen.fingerprint}`, t('mixed.banner.unassigned',{p0:screen.name}), true);
    }
    for (const warning of mixed.warnings || []) {
      if (warning.code === 'last_local_screen') push('last_local_screen',t('mixed.note.last.screen'),true);
      if (warning.code === 'unknown_keyboard_holder') push('unknown_keyboard_holder',t('mixed.note.unknown.holder'));
    }
    if (mixed.execution?.some(entry => entry.ok === false)) push('execution_failed',t('mixed.note.execution.failed'));
    if (mixed.enabled && mixed.transition?.phase === 'idle' && mixed.transition.outcome !== 'ok' && mixed.transition.outcome) push('partial', t('mixed.note.execution.failed'));
    if (mixed.enabled && mixed.topology_error) push('topology_unavailable',t('mixed.note.no.detach'));
    if (mixed.simulated) push('simulated',t('mixed.banner.simulated'));
    return out;
  }

  // Assignment after the user picks a configured computer for one
  // screen. Entries for screens that are not currently listed (unplugged) are
  // kept, as the runtime retains them.
  function assignmentAfterChoice(ctx, fingerprint, deviceId) {
    if (!ctx.computers.some(computer => computer.id === deviceId)) return [...(ctx.mixed?.assignment || [])];
    const listed = new Set(externalScreens(ctx).map((screen) => screen.fingerprint));
    const next = (ctx.mixed?.assignment || []).filter((entry) => !listed.has(entry.fingerprint));
    for (const screen of externalScreens(ctx)) {
      const device = screen.fingerprint === fingerprint ? deviceId : screen.assigned;
      if (device) next.push({ fingerprint: screen.fingerprint, device_id: device });
    }
    return next;
  }

  window.KVMFlowMixedView = {
    externalScreens, builtinScreens, joinScreens, blockReasons, notes, sync, modeBar, statusScreens,
    statusComputers, nextSwitch, banners, assignmentAfterChoice, effectiveTransition,
  };
})();
