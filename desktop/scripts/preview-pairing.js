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
  const scenarioName = query.get('scenario') || 'v2-a1';
  const v2 = scenarioName.startsWith('v2-');
  // Synthetic time only: expose bounded discovery and the follow-up hint quickly.
  if (v2) { const timeout = window.setTimeout.bind(window); window.setTimeout = (fn, ms, ...args) => timeout(fn, ms === 10000 ? 50 : ms === 5000 ? 250 : ms, ...args); }
  const handlers = new Map();
  let runtimeSequence = 0;
  const snapshot = data => { const copy = structuredClone(data); copy.runtime_seq = ++runtimeSequence; return copy; };
  const emit = (kind, data) => { for (const cb of handlers.get('runtime-notification') || []) cb({ payload: { kind, data } }); };
  let pairSeq = 0;
  const pairEvent = data => ({ session_id: 'preview-session', event_seq: ++pairSeq, ...data });

  const peer = { waiting: true, device_id: 'dev-game', name: 'DESKTOP-GAME', platform: 'windows', online: false, addr: null, pair_addr: '192.168.1.23:47820',
    last_seen_ms: 1, initiator: null, app_version: '0.3.0', connection_id: 'fc3bbde6', key_fp: '8624e75f', last_error: null };
  const office = { ...peer, device_id: 'dev-mini', name: 'Office-Mini', platform: 'macos', pair_addr: '192.168.1.41:47820' };
  const offline = { ...peer, device_id: 'dev-old', name: 'Old-Studio', online: false, addr: null, pair_addr: null };
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
  if (v2) config.trigger = query.get('trigger') === 'present' ? config.trigger : null;
  const waitingScenario = ['v2-a1', 'v2-a2', 'v2-a3', 'v2-e1', 'v2-e3', 'v2-e4'].includes(scenarioName);
  const state = { state: 'idle', enabled: true, lan: lan(v2 ? waitingScenario ? [] : scenarioName === 'v2-b1a' ? [peer, office] : [peer] : statePeers) };

  const profileReport = (pending = 1) => ({ device_id: 'dev-game', profile_rev: 2, monitors: [
    { fingerprint: 'DEL-A0F1-A', name: v2 ? (query.get('lang') === 'en' ? 'Display 1 · G73' : '显示器 1 · G73') : 'DELL U2723', builtin: false, input: { state: 'value', value: 17, source: 'learned_active_read' } },
    { fingerprint: 'DEL-A0F1-B', name: v2 ? (query.get('lang') === 'en' ? 'Display 2 · G52plus' : '显示器 2 · G52plus') : 'AOC Q27G2', builtin: false, input: pending ? { state: 'pending' } : { state: 'value', value: 9, source: 'learned_active_read' } },
    ...(count === 3 ? [{ fingerprint: 'DEL-A0F1-C', name: 'LG 27UP', builtin: false, input: { state: 'pending' } }] : []),
  ] });

  window.__TAURI__ = { event: { listen: async (kind, cb) => {
    if (!handlers.has(kind)) handlers.set(kind, new Set());
    handlers.get(kind).add(cb);
    return () => handlers.get(kind).delete(cb);
  } }, core: { invoke: async (command, args = {}) => {
    if (command === 'host_info') return { version: '0.3.0', platform: scenarioName === 'compat-report' || scenarioName.startsWith('feedback') ? 'macos' : 'preview', arch: 'aarch64', osVersion: '15.7.1', receiveBeta: query.get('beta') === '1', computerName: 'MacBook Pro',
      languagePreference: query.get('lang') || 'zh-CN', initialPage: 'wizard' };
    if (command === 'feedback_send') {
      window.__previewFeedbackPayload = structuredClone(args.payload);
      if (scenarioName === 'feedback-failure') throw { code: query.get('failure') || 'unavailable' };
      return { ok: true, id: 'fb_01JABCDEF0123456789ABCDEFG' };
    }
    if (command === 'feedback_open_privacy' || command === 'compatibility_open_issue') {
      window.__previewExternal = { command, args }; return null;
    }
    if (command === 'runtime_request') {
      const { method, params } = args;
      if (method === 'lan.identity') return state.lan.self;
      if (method === 'config.get') return structuredClone(config);
      if (method === 'config.set') { if (window.__previewPairing.failSave) throw { code:'E_CONFIG_IO',message:'synthetic save failure' }; Object.assign(config,structuredClone(params.config)); return {}; }
      if (method === 'state.get') return snapshot(state);
      if (method === 'lan.pair.window') return { open: params.open };
      if (method === 'lan.pair.start' || method === 'lan.connect.manual') return { phase: 'connecting', addr: params.addr,
        session_id: 'preview-session', event_seq: 0, local_confirmed: false, peer_confirmed: false };
      if (method === 'lan.pair.confirm') return { phase: params.sas_ok ? 'await_peer_confirm' : 'cancelled', sas: '482 913',
        session_id: 'preview-session', event_seq: ++pairSeq, local_confirmed: params.sas_ok, peer_confirmed: false };
      if (method === 'lan.pair.resolve') return { phase: 'ready_to_commit', issues: [], session_id: 'preview-session', event_seq: ++pairSeq };
      if (method === 'lan.profile.refresh') return { scheduled: true };
      if (method === 'lan.test.start') return { test_id: 'preview-test', duration_ms: params.duration_ms };
      if (method === 'lan.test.stop') return { test_id: 'preview-test', events: [] };
      if (method === 'lan.setup.secondary') return { secondary: true, phase: 'waiting_for_primary' };
      if (method === 'usb.snapshot') return { devices: [{ vid_pid: '1234:5678', vendor: 'UGREEN', product: 'USB Switch' }] };
      if (method === 'diagnostics.collect') return { backend: 'simulated', events: [{ kind: 'display.list', fields: { edid_id: 'DEL-A261-0000BEEF', path: '/Users/preview/report', addr: '192.168.1.22' } }] };
      if (method === 'display.list') return { displays: scenarioName === 'compat-report' || scenarioName.startsWith('feedback') ? [
        { edid_id: 'DEL-A0F1-A', manufacturer: 'DEL', model_name: 'DELL U2723QE', ddc: { state: 'available', input: 15 } },
        { edid_id: 'DEL-A0F1-B', manufacturer: 'AOC', model_name: 'Q27G2', ddc: { state: 'unavailable' } },
      ] : v2 ? [
        { edid_id: 'DEL-A0F1-A', model_name: query.get('lang') === 'en' ? 'Display 1 · G73' : '显示器 1 · G73', ddc: { state: 'available', input: 15 } },
        { edid_id: 'DEL-A0F1-B', model_name: query.get('lang') === 'en' ? 'Display 2 · G52plus' : '显示器 2 · G52plus', ddc: scenarioName === 'v2-e4' ? { state: 'unavailable' } : { state: 'available', input: 7 } },
      ] : [] };
      return {};
    }
    if (command === 'startup_get') return false;
    if (command === 'update_check') return null;
    return null;
  } } };

  const completeTopology = () => {
    if (!config.switch_group.devices.some(d => d.device_id === peer.device_id)) config.switch_group.devices.push({
      device_id:peer.device_id,name:peer.name,port_index:2,profile_rev:2,
      monitors:profileReport(0).monitors.slice(0,count).map(m=>({fingerprint:m.fingerprint,label:m.name,local_input:m.input.value})) });
  };
  window.__previewPairing = { emit, completeTopology, config, failSave:false };
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
    if (v2) {
      window.navigate('guide');
      if (scenarioName === 'v2-a1') return;
      document.querySelector('[data-pair-action="start"]').click();
      await waitFor(() => !['start', 'detect'].includes(S.activeMode()));
      if (scenarioName === 'v2-e4' || scenarioName === 'v2-b1' || scenarioName === 'v2-b1a') return;
      if (waitingScenario) {
        await waitFor(() => S.activeMode() === 'wait');
        if (scenarioName === 'v2-a2') return;
        if (scenarioName === 'v2-e1') { document.querySelector('[data-pair-action="ip"]').click(); await waitFor(() => S.activeMode() === 'ip'); return; }
        emit('lan.pair', pairEvent({ phase: 'await_confirm', device_id: peer.device_id, initiator: false, sas: '482 913' }));
        await new Promise(r => setTimeout(r, 30));
      }
      if (scenarioName === 'v2-e2') { emit('lan.pair', pairEvent({ phase: 'failed', device_id: peer.device_id, code: 'E_LAN_CLOSED' })); await waitFor(() => S.activeMode() === 'error'); return; }
      completeTopology();
      emit('lan.pair', pairEvent({ phase: 'committed', device_id: peer.device_id, initiator: !waitingScenario, config_merged: true, local_confirmed: true, peer_confirmed: true }));
      await waitFor(() => S.activeMode() === (waitingScenario ? 'done' : 'confirm'));
      if (scenarioName === 'v2-a3') return;
      if (scenarioName === 'v2-e3') {
        emit('lan.test', { test_id: 'preview-test', events: [{ kind: 'switch.report', device_id: peer.device_id, body: { test_id: 'preview-test', press_id: 'preview-press', reporting_device_id: peer.device_id, target_device_id: 'dev-mac', per_monitor: [{ edid_id: 'DEL-A0F1-B', commanded: false }] } }] });
        await waitFor(() => S.activeMode() === 'test-error'); return;
      }
      emit('lan.profile', profileReport(scenarioName === 'v2-b2a' ? 1 : 0));
      await waitFor(() => document.querySelectorAll('.pair-monitor').length === 2);
      if (scenarioName === 'v2-b2' || scenarioName === 'v2-b2a') return;
      await waitFor(() => !document.querySelector('[data-pair-action="v2-next"]').disabled);
      document.querySelector('[data-pair-action="v2-next"]').click();
      await waitFor(() => S.activeMode() === 'try');
      if (scenarioName === 'v2-b3a') {
        emit('switch.report', { test_id: 'preview-test', press_id: 'preview-press', reporting_device_id: 'dev-mac', target_device_id: peer.device_id, per_monitor: [{ edid_id: 'DEL-A0F1-B', commanded: false }] });
        await waitFor(() => S.activeMode() === 'try-hint');
      }
      return;
    }
    const sleep = (ms) => new Promise(r => setTimeout(r, ms));
    await sleep(80);
    switch (scenarioName) {
      case 'compat-report': case 'feedback': case 'feedback-preview': case 'feedback-success': case 'feedback-failure': {
        // Feedback must obtain its own inventory, as on a cold tray open.
        window.navigate('diagnostics');
        document.getElementById('submit-device').click();
        await waitFor(() => document.getElementById('compatibility-dialog').open);
        if (scenarioName !== 'feedback' && scenarioName !== 'compat-report') {
          const kind = document.querySelector('[name="feedback-kind"][value="' + (scenarioName === 'feedback-success' ? 'works' : 'problem') + '"]');
          kind.checked = true; kind.dispatchEvent(new Event('input', { bubbles: true }));
          const message = document.getElementById('feedback-message');
          message.value = query.get('lang') === 'en'
            ? scenarioName === 'feedback-success' ? 'Input switching works on both displays.' : 'The second display stays black after switching.'
            : scenarioName === 'feedback-success' ? '两台显示器都能正常切换输入。' : '第二台显示器在切换后没有画面。';
          message.dispatchEvent(new Event('input', { bubbles: true }));
        }
        if (scenarioName === 'feedback-preview') {
          document.getElementById('feedback-diagnostics').click();
          await waitFor(() => !document.getElementById('feedback-send').disabled);
          document.getElementById('feedback-preview').open = true;
        }
        if (scenarioName === 'feedback-success' || scenarioName === 'feedback-failure') {
          document.getElementById('feedback-send').click();
          await waitFor(() => scenarioName === 'feedback-success' ? !document.getElementById('feedback-success').classList.contains('hidden') : !document.getElementById('copy-compatibility').classList.contains('hidden'));
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
      case 'pair-sas': {
        S.pairWith('dev-game'); await sleep(30);
        emit('lan.pair', pairEvent({ phase: 'await_confirm', device_id: 'dev-game', initiator: true, sas: '482 913', local_confirmed: false, peer_confirmed: false }));
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
server.listen(Number(process.env.KVMFLOW_PAIRING_PREVIEW_PORT || 4177), '127.0.0.1', () => console.log('Synthetic pairing preview: http://127.0.0.1:' + server.address().port + '/?scenario=v2-a1'));
