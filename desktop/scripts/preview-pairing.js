// A loopback-only synthetic preview for the LAN pairing UI; it never
// launches the native runtime, touches displays or sends DDC commands.
// node scripts/preview-pairing.js
// http://127.0.0.1:4177/?scenario=pair-sas&lang=en
const http = require('node:http');
const fs = require('node:fs');
const path = require('node:path');
const renderer = path.resolve(__dirname, '../src/renderer');

// The synthetic transport the preview renderer talks to. Each scenario
// primes state and (optionally) drives the wizard through notifications.
const bridge = `
(() => {
  const query = new URLSearchParams(location.search);
  const scenarioName = query.get('scenario') || 'main-1-wait';
  const handlers = new Map();
  let runtimeSequence = 0;
  const snapshot = data => { const copy = structuredClone(data); copy.runtime_seq = ++runtimeSequence; return copy; };
  const emit = (kind, data) => { for (const cb of handlers.get('runtime-notification') || []) cb({ payload: { kind, data } }); };
  let pairSeq = 0;
  const pairEvent = data => ({ session_id: 'preview-session', event_seq: ++pairSeq, ...data });

  const peer = { device_id: 'dev-game', name: 'DESKTOP-GAME', platform: 'windows', online: true, addr: '192.168.1.23:47820',
    last_seen_ms: 1, initiator: null, app_version: '0.3.0', connection_id: 'fc3bbde6', key_fp: '8624e75f', last_error: null };
  const office = { ...peer, device_id: 'dev-mini', name: 'Office-Mini', platform: 'macos', addr: '192.168.1.41:47820' };
  const offline = { ...peer, device_id: 'dev-old', name: 'Old-Studio', online: false, addr: null };
  const lan = (peers) => ({ enabled: true, listen_port: 47820,
    self: { device_id: 'dev-mac', key_fp: '1f22c9e5', addr: '192.168.1.23' },
    peers, issues: [], sync: {},
    setup: { secondary: scenarioName.startsWith('secondary'), trigger_verified: false, shared_trigger: null } });

  const monitors = (count) => Array.from({ length: count }, (_, i) => ({
    fingerprint: 'DEL-A0F1-' + 'ABCDEF'[i], label: '',
    local_input: [15, 7, 17][i] ?? 15, source: scenarioName.startsWith('secondary') && i > 0 ? 'unknown' : 'learned_active_read',
  }));
  const count = ['main-2-ready-single'].includes(scenarioName) ? 1 : ['main-2-ready-three'].includes(scenarioName) ? 3 : 2;
  const groupDevices = [
    { device_id: 'dev-mac', name: 'MacBook Pro', port_index: 1, monitors: monitors(count).map(m => ({ ...m, local_input: m.local_input })) },
  ];
  const config = { schema_version: 2, local_device: { device_id: 'dev-mac', host_label: 'MacBook Pro', monitors: monitors(count) },
    switch_group: { group_id: 'preview-pair', revision: 2, devices: groupDevices },
    trigger: { anchor: { vid_pid: '1234:5678' }, members: [{ vid_pid: '2345:6789' }], debounce: { t_stable_ms: 0, t_absent_ms: 0, t_cooldown_ms: 1000, quorum_peripherals: 1 } },
    advanced: { ddc_retry: { attempts: 2, delay_ms: 100 }, usb_poll_ms: 250, arrival_correction_enabled: false } };
  const statePeers = scenarioName === 'pair-empty' ? [] : scenarioName === 'pair-offline' ? [offline]
    : scenarioName.startsWith('secondary') ? [{ ...peer, device_id: 'dev-macpro', name: 'MacBook Pro' }]
    : [peer, office, offline];
  const state = { state: 'idle', enabled: true, lan: lan(statePeers) };

  const profileReport = (pending = 1) => ({ device_id: 'dev-game', profile_rev: 2, monitors: [
    { fingerprint: 'DEL-A0F1-A', name: 'DELL U2723', builtin: false, input: { state: 'value', value: 17, source: 'learned_active_read' } },
    { fingerprint: 'DEL-A0F1-B', name: 'AOC Q27G2', builtin: false, input: pending ? { state: 'pending' } : { state: 'value', value: 9, source: 'learned_active_read' } },
    ...(count === 3 ? [{ fingerprint: 'DEL-A0F1-C', name: 'LG 27UP', builtin: false, input: { state: 'pending' } }] : []),
  ] });

  window.__TAURI__ = { event: { listen: async (kind, cb) => {
    if (!handlers.has(kind)) handlers.set(kind, new Set());
    handlers.get(kind).add(cb);
    return () => handlers.get(kind).delete(cb);
  } }, core: { invoke: async (command, args = {}) => {
    if (command === 'host_info') return { version: '0.3.0', platform: scenarioName === 'compat-report' ? 'macos' : 'preview', arch: 'aarch64', osVersion: '15.7.1', computerName: 'MacBook Pro',
      languagePreference: query.get('lang') || 'zh-CN', initialPage: 'wizard' };
    if (command === 'runtime_request') {
      const { method, params } = args;
      if (method === 'config.get') return config;
      if (method === 'state.get') return snapshot(state);
      if (method === 'lan.pair.window') return { open: params.open };
      if (method === 'lan.pair.start') return { phase: 'connecting', addr: params.addr,
        session_id: 'preview-session', event_seq: 0, local_confirmed: false, peer_confirmed: false };
      if (method === 'lan.pair.confirm') return { phase: params.sas_ok ? 'await_peer_confirm' : 'cancelled', sas: '482 913',
        session_id: 'preview-session', event_seq: ++pairSeq, local_confirmed: params.sas_ok, peer_confirmed: false };
      if (method === 'lan.pair.resolve') return { phase: 'ready_to_commit', issues: [], session_id: 'preview-session', event_seq: ++pairSeq };
      if (method === 'lan.profile.refresh') return { scheduled: true };
      if (method === 'lan.test.start') return { test_id: 'preview-test', duration_ms: params.duration_ms };
      if (method === 'lan.test.stop') return { test_id: 'preview-test', events: [] };
      if (method === 'lan.setup.secondary') return { secondary: true, phase: 'waiting_for_primary' };
      if (method === 'usb.snapshot') return { devices: [{ vid_pid: '1234:5678', vendor: 'UGREEN', product: 'USB Switch' }] };
      if (method === 'display.list') return { displays: scenarioName === 'compat-report' ? [
        { edid_id: 'DEL-A0F1-A', manufacturer: 'DEL', model_name: 'DELL U2723QE', ddc: { state: 'available', input: 15 } },
        { edid_id: 'DEL-A0F1-B', manufacturer: 'AOC', model_name: 'Q27G2', ddc: { state: 'unavailable' } },
      ] : [] };
      return {};
    }
    if (command === 'startup_get') return false;
    if (command === 'update_check') return null;
    return null;
  } } };

  window.__previewPairing = { emit };
  // bootstrap.js loads the renderer modules asynchronously, so a fixed delay
  // can run before KVMFlowPairing exists (the old secondary-2-wait capture
  // silently stayed on the classic wizard page). Wait for the real API.
  const drive = async () => {
    const waitFor = (check, timeout = 5000) => new Promise((resolve, reject) => {
      const started = Date.now();
      const poll = () => {
        if (check()) return resolve();
        if (Date.now() - started > timeout) return reject(new Error('preview timed out waiting for the renderer'));
        setTimeout(poll, 20);
      };
      poll();
    });
    await waitFor(() => typeof window.KVMFlowPairing?.startPrimaryAdd === 'function'
      && window.__kvmflowRuntimeState !== undefined);
    const S = window.KVMFlowPairing;
    const sleep = (ms) => new Promise(r => setTimeout(r, ms));
    await sleep(80);
    const primaryProfile = async (pending, includeProfile = true) => {
      window.navigate('guide'); window.nextStep(); window.nextStep();
      S.startPrimaryAdd(); S.pairWith('dev-game'); await sleep(40);
      emit('lan.pair', pairEvent({ phase: 'committed', device_id: 'dev-game', config_merged: !pending, local_confirmed: true, peer_confirmed: true }));
      await sleep(40);
      if (includeProfile) { emit('lan.profile', profileReport(pending)); await sleep(40); }
      document.querySelector('#pairing-body [data-pair-action="finish"]').click(); await sleep(40);
    };
    switch (scenarioName) {
      case 'compat-report': {
        await window.kvmflow.request('display.list');
        window.navigate('diagnostics');
        document.getElementById('submit-device').click();
        if (query.get('reportEnd') === '1') {
          await waitFor(() => document.getElementById('compatibility-dialog').open);
          const report = document.getElementById('compatibility-report');
          report.scrollTop = report.scrollHeight;
        }
        break;
      }
      case 'pair-select': {
        S.openDialog();
        await sleep(30);
        window.document.querySelector('[data-pair-peer="dev-game"]')?.click();
        await sleep(30);
        break;
      }
      case 'pair-connecting': {
        S.pairWith('dev-game');
        await new Promise(r => setTimeout(r, 30));
        break;
      }
      case 'pair-sas': case 'secondary-2-wait': {
        if (scenarioName === 'secondary-2-wait') { S.startRoleChoice(); window.document.querySelector('[data-pair-role="secondary"]').click(); window.document.querySelector('[data-pair-action="role-next"]').click(); await new Promise(r => setTimeout(r, 30)); }
        else { S.pairWith('dev-game'); await new Promise(r => setTimeout(r, 30)); }
        emit('lan.pair', pairEvent({ phase: 'await_confirm', device_id: scenarioName === 'secondary-2-wait' ? 'dev-macpro' : 'dev-game', initiator: scenarioName !== 'secondary-2-wait', sas: '482 913', local_confirmed: false, peer_confirmed: false }));
        break;
      }
      case 'pair-sas-waiting': {
        S.pairWith('dev-game');
        await new Promise(r => setTimeout(r, 30));
        emit('lan.pair', pairEvent({ phase: 'await_peer_confirm', device_id: 'dev-game', initiator: true, sas: '482 913', local_confirmed: true, peer_confirmed: false }));
        break;
      }
      case 'pair-conflict': {
        S.pairWith('dev-game');
        await new Promise(r => setTimeout(r, 30));
        emit('lan.pair', pairEvent({ phase: 'await_resolve', device_id: 'dev-game', initiator: true, sas: '482 913', local_confirmed: true, peer_confirmed: true, issues: [
          { severity: 'choice', detail: { kind: 'mixed', default: 'basis', resolved: false,
            basis: { enabled: true, revision: 1, assignment: [{ fingerprint: 'DEL-A0F1-A', device_id: 'dev-game' }, { fingerprint: 'DEL-A0F1-B', device_id: 'dev-mac' }] },
            joiner: { enabled: true, revision: 1, assignment: [{ fingerprint: 'DEL-A0F1-A', device_id: 'dev-mac' }, { fingerprint: 'DEL-A0F1-B', device_id: 'dev-game' }] } } },
        ], proposed_snapshot: {} }));
        break;
      }
      case 'pair-committing': {
        S.pairWith('dev-game');
        await new Promise(r => setTimeout(r, 30));
        emit('lan.pair', pairEvent({ phase: 'ready_to_commit', device_id: 'dev-game', initiator: true, local_confirmed: true, peer_confirmed: true, issues: [], proposed_snapshot: {} }));
        break;
      }
      case 'pair-error': {
        S.pairWith('dev-game');
        await new Promise(r => setTimeout(r, 30));
        emit('lan.pair', pairEvent({ phase: 'failed', code: 'E_LAN_PAIR_ONESIDED', message: 'one-sided commit undone', device_id: 'dev-game', local_confirmed: true, peer_confirmed: true }));
        break;
      }
      case 'secondary-1-role': {
        S.startRoleChoice();
        window.document.querySelector('[data-pair-role="secondary"]').click();
        break;
      }
      case 'secondary-3-done': {
        S.startRoleChoice();
        window.document.querySelector('[data-pair-role="secondary"]').click();
        window.document.querySelector('[data-pair-action="role-next"]').click();
        await new Promise(r => setTimeout(r, 30));
        emit('lan.pair', pairEvent({ phase: 'committed', device_id: 'dev-macpro', config_merged: true, peer_confirmed_commit: true, local_confirmed: true, peer_confirmed: true }));
        break;
      }
      case 'main-2-ready': case 'main-2-ready-single': case 'main-2-ready-three': {
        await primaryProfile(scenarioName !== 'main-2-ready-single');
        if (scenarioName === 'main-2-ready-single') emit('lan.profile', { device_id: 'dev-game', profile_rev: 3, monitors: [{ fingerprint: 'DEL-A0F1-A', name: 'DELL U2723', builtin: false, input: { state: 'value', value: 17 } }] });
        break;
      }
      case 'main-profile-waiting': {
        await primaryProfile(0, false);
        break;
      }
      case 'pair-alias': case 'pair-alias-different': case 'pair-replacement': case 'pair-choices': {
        S.pairWith('dev-game'); await sleep(40);
        const issues = [];
        if (scenarioName !== 'pair-replacement') issues.push({ severity: 'choice', detail: { kind: 'monitor_alias', group_fingerprint: 'DEL-A0F1-A', local_fingerprint: 'DEL-A0F1-GAME', default_same: true, resolved: false } });
        if (scenarioName !== 'pair-alias' && scenarioName !== 'pair-alias-different') issues.push({ severity: 'choice', detail: { kind: 'replace_device', old_device_id: 'Old-Studio', new_device_id: 'dev-game', default_replace: true, resolved: false } });
        emit('lan.pair', pairEvent({ phase: 'await_resolve', device_id: 'dev-game', initiator: true, local_confirmed: true, peer_confirmed: true, issues }));
        if (scenarioName === 'pair-alias-different') {
          await sleep(20);
          document.querySelector('[data-pair-alias="different"]')?.click();
          await sleep(20);
        }
        break;
      }
      case 'pair-pin-only': {
        S.pairWith('dev-game'); await sleep(40);
        emit('lan.pair', pairEvent({ phase: 'committed', device_id: 'dev-game', config_merged: false,
          peer_confirmed_commit: true, local_confirmed: true, peer_confirmed: true,
          issues: [{ severity: 'blocking', detail: { kind: 'monitor_unmatched', basis: ['DEL-A0F1-A'], joiner: ['DEL-A0F1-GAME'] } }] }));
        break;
      }
      case 'secondary-sas-waiting': case 'secondary-retry': {
        S.startRoleChoice(); document.querySelector('[data-pair-role="secondary"]').click(); document.querySelector('[data-pair-action="role-next"]').click(); await sleep(40);
        if (scenarioName === 'secondary-retry') emit('lan.pair', pairEvent({ phase: 'failed', code: 'E_LAN_PAIR_TIMEOUT', local_confirmed: false, peer_confirmed: false }));
        else {
          emit('lan.pair', pairEvent({ phase: 'await_confirm', device_id: 'dev-macpro', initiator: false, sas: '482 913', local_confirmed: false, peer_confirmed: false })); await sleep(30);
          document.querySelector('[data-pair-action="confirm"]').click();
        }
        break;
      }
      case 'main-3-try': case 'main-3-try-fail': {
        await primaryProfile(0);
        window.document.querySelector('[data-pair-action="done"]').click();
        await sleep(40);
        const first = { mode: 'multi_device', test_id: 'preview-test', press_id: 'dev-mac:1', reporting_device_id: 'dev-mac',
          target_device_id: 'dev-game', test_kind: 'commands_sent', per_monitor: [{ edid_id: 'DEL-A0F1-A', commanded: true }] };
        emit('switch.report', first);
        emit('lan.test', { test_id: 'preview-test', events: [
          { device_id: 'dev-mac', kind: 'switch.report', body: first },
          { device_id: 'dev-game', kind: 'test.event', body: { test_id: 'preview-test', press_id: 'dev-mac:1', reporting_device_id: 'dev-game',
            target_device_id: 'dev-game', test_kind: 'picture_arrived', ts_ms: Date.now() } },
        ] });
        await new Promise(r => setTimeout(r, 30));
        if (scenarioName === 'main-3-try-fail') {
          const second = { mode: 'multi_device', test_id: 'preview-test', press_id: 'dev-game:1', reporting_device_id: 'dev-game',
            target_device_id: 'dev-mac', test_kind: 'commands_sent', per_monitor: [{ edid_id: 'DEL-A0F1-A', commanded: true }, { edid_id: 'DEL-A0F1-B', commanded: false }] };
          emit('switch.report', second);
          emit('lan.test', { test_id: 'preview-test', events: [
            { device_id: 'dev-game', kind: 'switch.report', body: second },
            { device_id: 'dev-mac', kind: 'test.event', body: { test_id: 'preview-test', press_id: 'dev-game:1', reporting_device_id: 'dev-mac',
              target_device_id: 'dev-mac', test_kind: 'picture_arrived', ts_ms: Date.now() } },
          ] });
        }
        break;
      }
      case 'main-1-wait': case 'pair-empty': case 'pair-offline': {
        window.navigate('guide'); window.nextStep(); window.nextStep();
        window.KVMFlowPairing.startPrimaryAdd();
        break;
      }
      default: break;
    }
  };
  window.__previewPairing.ready = false;
  window.addEventListener('load', () => { setTimeout(async () => {
    try { await drive(); }
    catch (error) { window.__previewPairing.error = error.message; console.error('preview drive failed', error); }
    finally { await new Promise(r => setTimeout(r, 80)); window.__previewPairing.ready = true; }
  }, 50); });
})();
`;
const server = http.createServer((request, response) => {
  const url = new URL(request.url, 'http://localhost');
  let body, type;
  if (url.pathname === '/preview-tauri.js') { body = bridge; type = 'text/javascript'; }
  else {
    const relative = decodeURIComponent(url.pathname === '/' ? '/index.html' : url.pathname);
    const file = path.resolve(renderer, '.' + relative);
    if (!file.startsWith(renderer + path.sep)) { response.writeHead(403); return response.end(); }
    try { body = fs.readFileSync(file); } catch { response.writeHead(404); return response.end(); }
    type = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.json': 'application/json', '.png': 'image/png', '.svg': 'image/svg+xml' }[path.extname(file)] || 'application/octet-stream';
    if (file.endsWith('/index.html')) body = body.toString().replace('<script src="bootstrap.js">', '<script src="preview-tauri.js"></script><script src="bootstrap.js">');
  }
  response.writeHead(200, { 'Content-Type': type, 'Cache-Control': 'no-store' }); response.end(body);
});
server.listen(Number(process.env.KVMFLOW_PAIRING_PREVIEW_PORT || 4177), '127.0.0.1', () => console.log('Synthetic pairing preview: http://127.0.0.1:' + server.address().port + '/?scenario=main-1-wait'));
