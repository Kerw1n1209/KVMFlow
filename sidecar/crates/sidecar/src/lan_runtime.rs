//! Runtime-owned LAN coordinator (S6–S8). Configuration, membership and
//! hardware never leave this worker; the transport layer only moves frames.
use crate::lan::{
    conn::{LanEvent, LanHandle, LanOptions, SessionGeneration},
    discovery::{Discovery, MdnsDiscovery},
    identity_store,
    messages::{Envelope, Hello},
    trust::{public_key_string, MemberStore},
};
use crate::*;
use kvmflow_core::{
    config::MultiDeviceConfig,
    multidevice::SwitchGroupSnapshot,
    pairing::ProfileReport,
    sync::{self, AuthenticatedMember, Member, SyncSummary},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    path::Path,
    sync::{Arc, RwLock},
    time::Duration,
};

mod commands;
mod pairing;
mod setup;
mod test_evidence;

pub struct RuntimeLanOptions {
    pub enabled: bool,
    pub transport: LanOptions,
    pub discovery: Option<Box<dyn Discovery>>,
    pub peers: Vec<SocketAddr>,
}
impl Default for RuntimeLanOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            transport: LanOptions::default(),
            discovery: None,
            peers: vec![],
        }
    }
}
impl RuntimeLanOptions {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
}

#[derive(Clone)]
pub(crate) struct Source {
    device_id: String,
    key: [u8; 32],
    generation: SessionGeneration,
}

#[derive(Default)]
pub(crate) struct LanRuntime {
    handle: Option<LanHandle>,
    members: Arc<RwLock<Vec<Member>>>,
    public_key: String,
    pub(crate) device_id: String,
    issues: Vec<Value>,
    sync_states: BTreeMap<String, Value>,
    online: BTreeSet<String>,
    last_status: Value,
    next_id: u64,
    pending: BTreeMap<u64, commands::Pending>,
    action_source: Option<Source>,
    pair: Option<pairing::Pairing>,
    secondary: bool,
    profile: Option<ProfileReport>,
    next_profile_read: u64,
    shared_trigger: Option<Value>,
    trigger_verified: bool,
    test: Option<test_evidence::TestEvidence>,
    /// Identity and ordering of the current (or last) pairing session.
    /// Every `lan.pair` notification and pairing RPC reply carries both so
    /// the UI can drop late/duplicate events from an older attempt.
    pair_session_id: Option<String>,
    pair_event_seq: u64,
    /// Explicit confirmation flags (the UI no longer has to infer them from
    /// the phase name).
    pair_local_confirmed: bool,
    pair_peer_confirmed: bool,
    /// Session id minted by `lan.pair.start`, claimed by the PairReady link.
    pair_pending_session: Option<String>,
    /// Remotes whose merge is blocked until their owner's profile arrives.
    deferred: Vec<(Source, SwitchGroupSnapshot)>,
    mixed_txn: Option<commands::MixedTransaction>,
    /// LAN start failed before identity/config existed: retry when config
    /// is created.
    lan_wanted: bool,
    /// Stale-session reset cooldown deadline (ms).
    reset_cooldown: Option<u64>,
    /// Per-peer resync serve cooldowns (epoch ms), receiver side (P3).
    resync_cooldowns: std::collections::BTreeMap<String, u64>,
    /// Platform learned from a peer's owner profile, used when its hello
    /// predates the `platform` field.
    peer_platforms: BTreeMap<String, String>,
    /// Last connection identity per peer, so a REPLACED session (not just a
    /// fresh online transition) also triggers catchup + profile exchange.
    peer_connection_ids: BTreeMap<String, String>,
    /// Session generation for which we answered a peer's owner profile with
    /// our own. Makes the exchange symmetric and self-healing: a lost push on
    /// a replaced session is recovered without waiting for a new transition.
    profile_exchanges: BTreeMap<String, SessionGeneration>,
}

impl Runtime {
    /// Reconcile a leftover pairing transaction record from a crash during
    /// commit. The record is written BEFORE the config write and removed
    /// after; a surviving record therefore means the crash happened inside
    /// that window. The commit itself is idempotent (the plan snapshot is in
    /// the record): if the on-disk config already carries the record's
    /// group snapshot, the commit had landed and only the record is stale;
    /// otherwise the config write never happened and the record is cleaned
    /// up without applying anything (the pairing session is gone anyway and
    /// keys were pinned only on the peer that finished).
    fn reconcile_pairing_record(&mut self) {
        let record_path = self.config_path.with_file_name("pairing.pending.json");
        let unreadable = std::fs::read_to_string(&record_path)
            .ok()
            .is_some_and(|text| serde_json::from_str::<Value>(&text).is_err());
        if unreadable {
            self.lan_issue(
                "E_LAN_PAIR_RECORD",
                "发现无法解析的配对事务记录（pairing.pending.json），已保留；请检查后手动删除。",
            );
            return;
        }
        let Some(record) = self.read_pairing_record() else {
            return;
        };
        let locally_committed = self.record_members_match(&record);
        self.adopt_record_session(&record);
        if locally_committed {
            // Our own commit landed; the peer's outcome is still unknown.
            // NEVER delete the record here (N3): it stays until the peer is
            // actually observed — the sync-path reconciliation confirms or
            // rejects it.
            self.emit_event(
                "lan.pair.recovered",
                Level::Info,
                json!({"outcome": "awaiting_peer"}),
            );
        } else {
            // The config write never happened on this end; the pairing
            // session is gone. Clean up and surface the ambiguity.
            self.emit_event(
                "lan.pair.recovered",
                Level::Warn,
                json!({"outcome": "rolled_back"}),
            );
            self.lan_issue(
                "E_LAN_PAIR_INTERRUPTED",
                "上次配对在提交时中断；本机配置未更改。若对面的配对未完成，请重新配对。",
            );
            self.clear_pairing_record();
        }
    }

    fn read_pairing_record(&self) -> Option<Value> {
        let record_path = self.config_path.with_file_name("pairing.pending.json");
        let text = std::fs::read_to_string(&record_path).ok()?;
        serde_json::from_str::<Value>(&text).ok()
    }

    fn clear_pairing_record(&self) {
        let record_path = self.config_path.with_file_name("pairing.pending.json");
        let _ = std::fs::remove_file(&record_path);
    }

    /// A surviving transaction record also names the pairing session it came
    /// from. Adopt it for the UI ONLY while idle: a live pairing (e.g. a retry
    /// after a lost acknowledgement) owns the current session identity, and an
    /// old record must never make the new session's events look stale.
    fn adopt_record_session(&mut self, record: &Value) {
        if self.lan_runtime.pair.is_some() {
            return;
        }
        if let Some(session_id) = record.get("session_id").and_then(Value::as_str) {
            self.lan_runtime.pair_session_id = Some(session_id.to_string());
        }
    }

    /// The key the transaction record pinned for its pairing peer.
    fn record_peer_key(record: &Value) -> Option<&str> {
        let peer = record.get("peer_device_id").and_then(Value::as_str)?;
        record
            .get("members")
            .and_then(Value::as_array)?
            .iter()
            .find(|m| {
                m.get("device_id").and_then(Value::as_str) == Some(peer)
                    && m.get("removed").and_then(Value::as_bool) != Some(true)
            })
            .and_then(|m| m.get("public_key"))
            .and_then(Value::as_str)
    }

    /// True when an authenticated frame/refusal came from the exact key the
    /// record pinned for its peer. One-sided reconciliation and record
    /// confirmation must never trust an address alone (N9/P3).
    fn is_active_member_key(&self, key: &[u8; 32]) -> bool {
        let key = public_key_string(key);
        self.lan_runtime
            .members
            .read()
            .unwrap()
            .iter()
            .any(|member| !member.removed && member.public_key == key)
    }

    fn record_key_matches(record: &Value, key: &[u8; 32]) -> bool {
        Self::record_peer_key(record) == Some(public_key_string(key).as_str())
    }

    /// True when the live config already carries exactly the record's member
    /// list (the durable marker that this end committed the pairing).
    fn record_members_match(&self, record: &Value) -> bool {
        let result = self
            .config
            .as_ref()
            .and_then(|c| c.switch_group.as_ref())
            .is_some_and(|current| {
                kvmflow_core::sync::digest(&current.group_sync().members)
                    == record
                        .get("members")
                        .cloned()
                        .map(|m| kvmflow_core::sync::digest(&m))
                        .unwrap_or_default()
            });
        result
    }

    /// Our own identity as pinned in a peer's snapshot: an active member
    /// entry with our exact public key (N3's reconciliation criterion).
    fn peer_snapshot_pins_us(&self, remote: &SwitchGroupSnapshot) -> bool {
        let Some(cfg) = self.config.as_ref() else {
            return false;
        };
        let local_id = &cfg.local_device.device_id;
        remote.group_sync().members.iter().any(|m| {
            m.device_id == *local_id && !m.removed && m.public_key == self.lan_runtime.public_key
        })
    }

    /// Sync-path reconciliation (N3): with a surviving record, the peer's
    /// snapshot decides. If the peer pins US as an active member with the
    /// correct key, the peer committed — confirm and clear. Otherwise the
    /// pairing is one-sided from our end: tombstone the peer pin and
    /// surface E_LAN_PAIR_ONESIDED. The previous whole-list digest
    /// comparison misfired whenever the peer's list legitimately evolved
    /// (a third member joined) and revoked a healthy pair.
    fn reconcile_pairing_record_from_peer(
        &mut self,
        source: &Source,
        remote: &SwitchGroupSnapshot,
    ) {
        let Some(record) = self.read_pairing_record() else {
            return;
        };
        if record.get("peer_device_id").and_then(Value::as_str) != Some(source.device_id.as_str()) {
            return;
        }
        if !Self::record_key_matches(&record, &source.key) {
            return;
        }
        if self.peer_snapshot_pins_us(remote) {
            self.clear_pairing_record();
            self.pair_emit_record(Some(&record), json!({"phase": "peer_committed"}));
            return;
        }
        self.undo_one_sided_pin(&source.device_id);
    }

    /// Tombstone the peer's member pin (one-sided pairing undo) and report
    /// E_LAN_PAIR_ONESIDED (N3). Used by the sync-path reconciliation and
    /// the E_LAN_NOT_MEMBER rejection path.
    fn undo_one_sided_pin(&mut self, peer_id: &str) {
        let record = self.read_pairing_record();
        self.clear_pairing_record();
        let Some(mut cfg) = self.config.clone() else {
            return;
        };
        let Some(mut group) = cfg.switch_group.clone() else {
            return;
        };
        let mut sync = group.group_sync();
        let now = now_ms();
        let local_id = cfg.local_device.device_id.clone();
        if let Some(member) = sync
            .members
            .iter_mut()
            .find(|m| m.device_id == peer_id && !m.removed)
        {
            member.removed = true;
            member.revision = member.revision.saturating_add(1);
            member.meta = kvmflow_core::sync::SectionMeta {
                updated_at_ms: now,
                updated_by: local_id,
            };
            group.set_group_sync(sync);
            cfg.switch_group = Some(group);
            if self.lan_save(cfg).is_ok() {
                if let Some(handle) = self.lan_runtime.handle.as_ref() {
                    let _ = handle.disconnect(peer_id);
                }
            }
        }
        self.pair_emit_record(
            record.as_ref(),
            json!({
                "phase": "failed", "code": "E_LAN_PAIR_ONESIDED", "device_id": peer_id,
                "message": "上次配对只在单端完成，已自动撤销；请重新配对。",
            }),
        );
        self.lan_issue(
            "E_LAN_PAIR_ONESIDED",
            "上次配对只在单端完成，已自动撤销；请重新配对。",
        );
    }

    pub(crate) fn start_lan(&mut self, options: Option<RuntimeLanOptions>) {
        let mut options = options.unwrap_or_else(|| {
            // The standalone JSONL diagnostic server defaults LAN off; the
            // native embedded runtime (options None) enables it. Simulated
            // backends never auto-enable (they are test sandboxes).
            if self.backend.simulated() {
                RuntimeLanOptions::disabled()
            } else {
                RuntimeLanOptions::default()
            }
        });
        self.lan_runtime.lan_wanted = options.enabled;
        if !options.enabled
            || self
                .config
                .as_ref()
                .and_then(|c| c.lan.as_ref())
                .is_some_and(|l| !l.enabled)
        {
            return;
        }
        // The persisted secondary flag drives the pending-input polling and
        // trigger adoption across restarts (§6.1).
        if self.config.as_ref().is_some_and(|c| c.setup_secondary) {
            self.lan_runtime.secondary = true;
        }
        // Recheck at the transport boundary as config may have been applied
        // since startup. Never advertise an unsafe legacy identity.
        if let Some(config) = self.config.clone() {
            if !config.local_device.device_id.starts_with("dev-") {
                let original = std::fs::read_to_string(&self.config_path)
                    .unwrap_or_else(|_| config_store::persisted_config_text(&config));
                let migrated = self.bootstrap_loaded_identity(config, &original, true);
                if !migrated.local_device.device_id.starts_with("dev-") {
                    return;
                }
                self.apply_config(migrated);
            }
        }
        self.reconcile_pairing_record();
        let outcome = (|| -> lan::Result<LanHandle> {
            let device_id = match self.config.as_ref() {
                Some(c) => c.local_device.device_id.clone(),
                None => config_store::load_or_create_device_id(&self.config_path)?,
            };
            self.lan_runtime.device_id = device_id.clone();
            let lan_dir = self
                .config_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("lan");
            let identity = Arc::new(identity_store::load_or_create(
                &lan_dir.join("identity.key"),
            )?);
            self.lan_runtime.public_key = public_key_string(&identity.public_key());
            self.publish_lan_members();
            let mut hello = Hello::new(device_id);
            hello.name = self
                .config
                .as_ref()
                .map(|c| c.local_device.host_label.clone())
                .filter(|name| !name.trim().is_empty());
            hello.group_id = self
                .config
                .as_ref()
                .and_then(|c| c.switch_group.as_ref())
                .map(|g| g.group_id.clone());
            hello.holds_usb = self.tracker.as_ref().is_some_and(|t| t.group_present());
            hello.summary = self.lan_summary();
            hello.caps = vec!["mixed.v1".into(), "profile.v1".into()];
            let label = self
                .config
                .as_ref()
                .map(|c| c.local_device.host_label.clone())
                .unwrap_or_default();
            let mut transport = options.transport;
            if transport.listen_port == DEFAULT_LAN_PORT {
                if let Some(l) = self.config.as_ref().and_then(|c| c.lan.as_ref()) {
                    transport.listen_port = l.listen_port;
                }
            }
            let discovery = match options.discovery.take() {
                Some(d) => d,
                None => Box::new(MdnsDiscovery::new(transport.address_policy.clone())?)
                    as Box<dyn Discovery>,
            };
            let requested_port = transport.listen_port;
            let handle = LanHandle::start(
                identity,
                hello,
                label,
                self.lan_runtime.members.clone(),
                discovery,
                transport,
            )?;
            let actual_port = handle.status().listen_port;
            if requested_port != 0 && actual_port != requested_port {
                self.emit_event("lan.listen.fallback", Level::Warn,
                    json!({"requested_port":requested_port,"listen_port":actual_port,"reason":"address_in_use"}));
            }
            Ok(handle)
        })();
        match outcome {
            Ok(handle) => {
                for addr in options.peers {
                    let _ = handle.connect_manual(addr);
                }
                self.lan_runtime.handle = Some(handle);
                self.notify("lan.sync", json!({"status":"started"}));
            }
            Err(e) => self.lan_issue(e.code, &e.message),
        }
    }

    pub(crate) fn stop_lan(&mut self) {
        self.lan_runtime.pair.take();
        if let Some(mut handle) = self.lan_runtime.handle.take() {
            handle.shutdown();
        }
    }

    pub(crate) fn lan_state(&self) -> Value {
        let mut state = self
            .lan_runtime
            .handle
            .as_ref()
            .map(|h| h.status_json())
            .unwrap_or_else(|| json!({"enabled": false, "peers": [], "issues": []}));
        let mut issues = self.lan_runtime.issues.clone();
        if state["issues"]
            .as_array()
            .is_some_and(|items| items.iter().any(|issue| issue == "E_LAN_ID_CONFLICT"))
            && !issues
                .iter()
                .any(|issue| issue["code"] == "E_LAN_ID_CONFLICT")
        {
            issues.push(json!({"code": "E_LAN_ID_CONFLICT"}));
        }
        state["issues"] = json!(issues);
        if let Some(peers) = state.get_mut("peers").and_then(Value::as_array_mut) {
            for peer in peers {
                if peer.get("platform").is_none_or(Value::is_null) {
                    if let Some(platform) = peer
                        .get("device_id")
                        .and_then(Value::as_str)
                        .and_then(|id| self.lan_runtime.peer_platforms.get(id))
                    {
                        peer["platform"] = json!(platform);
                    }
                }
            }
        }
        state["sync"] = json!(self.lan_runtime.sync_states);
        state["setup"] = json!({
            "secondary": self.lan_runtime.secondary,
            "trigger_verified": self.lan_runtime.trigger_verified,
            "shared_trigger": self.lan_runtime.shared_trigger,
        });
        state
    }

    pub(crate) fn lan_issue(&mut self, code: &str, message: &str) {
        if self
            .lan_runtime
            .issues
            .iter()
            .any(|issue| issue["code"] == code && issue["message"] == message)
        {
            return;
        }
        if self.lan_runtime.issues.len() >= 32 {
            self.lan_runtime.issues.remove(0);
        }
        let issue = json!({"code": code, "message": message});
        self.lan_runtime.issues.push(issue.clone());
        self.notify("lan.sync", json!({"status": "conflict", "issues": [issue]}));
    }

    fn publish_lan_members(&mut self) {
        let members: Vec<Member> = self
            .config
            .as_ref()
            .and_then(|c| c.switch_group.as_ref())
            .map(|g| g.group_sync().members)
            .unwrap_or_default();
        let changed = {
            let mut store = self.lan_runtime.members.write().unwrap();
            let changed = *store != members;
            *store = members;
            changed
        };
        if changed {
            // Queued merges predate this trust change (revocation or key
            // rotation): they must never replay against the new pins (P2-1).
            self.lan_runtime.deferred.clear();
            // New trust resets dial backoff so a previously rejected peer is
            // retried immediately instead of waiting out its old timer.
            if let Some(handle) = self.lan_runtime.handle.as_ref() {
                let _ = handle.reannounce();
            }
        }
    }

    fn lan_summary(&self) -> Value {
        self.config
            .as_ref()
            .and_then(|c| c.switch_group.as_ref())
            .filter(|g| {
                kvmflow_core::identity::legacy_placeholders(g, &self.lan_runtime.device_id)
                    .is_empty()
            })
            .map(|g| serde_json::to_value(SyncSummary::from_snapshot(g)).unwrap_or(Value::Null))
            .unwrap_or(json!({}))
    }

    fn lan_send(&self, id: &str, message: Envelope) -> lan::Result<()> {
        self.lan_runtime
            .handle
            .as_ref()
            .ok_or_else(|| lan::LanError::new("E_LAN_DISABLED", "LAN disabled"))?
            .send(id, message)
    }

    fn current_source(&self, source: &Source) -> bool {
        self.lan_runtime
            .members
            .check(&source.device_id, &source.key)
            .is_ok()
            && self
                .lan_runtime
                .handle
                .as_ref()
                .is_some_and(|h| h.is_current_session(&source.device_id, source.generation))
    }

    /// Transport admission is not durable execution authorization: every
    /// hardware action rechecks the CURRENT session and membership.
    pub(crate) fn lan_action_allowed(&self) -> bool {
        self.lan_runtime
            .action_source
            .as_ref()
            .is_none_or(|s| self.current_source(s))
            && self.released_topology.is_none()
    }

    fn lan_ack(&mut self, id: &str, re: Option<u64>, result: Result<Value, CoreError>) {
        if let Some(re) = re {
            let (ok, body) = match result {
                Ok(v) => (true, v),
                Err(e) => (false, json!({"code": e.code, "message": e.message})),
            };
            let mut msg = Envelope::new("ack", body);
            msg.re = Some(re);
            msg.ok = Some(ok);
            let _ = self.lan_send(id, msg);
        }
    }

    pub(crate) fn lan_config_changed(&mut self) {
        // A runtime toggle of lan.enabled/port requires restarting the
        // transport to take effect (P3): stop when disabled, restart when
        // the settings diverge from the running handle.
        if let Some(cfg) = self.config.as_ref() {
            let lan_cfg = cfg.lan.as_ref();
            let handle = self.lan_runtime.handle.as_ref();
            let enabled = lan_cfg.is_none_or(|l| l.enabled);
            match (handle, enabled) {
                (Some(_), false) => {
                    self.stop_lan();
                    self.lan_issue("E_LAN_DISABLED", "局域网已在设置中关闭。");
                    return;
                }
                (None, true) => {
                    if self.lan_runtime.lan_wanted {
                        self.start_lan(None);
                    }
                }
                _ => {}
            }
        }
        self.publish_lan_members();
        self.lan_broadcast_snapshot("change");
        // After a pairing commit, share the USB switch trigger description
        // so the peer can adopt and verify it (§6.1 step 4).
        if let Some(trigger) = self.active_trigger() {
            if !trigger.anchor.vid_pid.is_empty() {
                let description = json!({
                    "anchor": trigger.anchor,
                    "members": trigger.members,
                });
                self.lan_runtime.shared_trigger = Some(description.clone());
                for peer in self.lan_runtime.online.clone() {
                    let _ = self.lan_send(
                        &peer,
                        Envelope::new("setup.trigger", json!({"trigger": description})),
                    );
                }
            }
        }
    }

    fn lan_broadcast_snapshot(&mut self, reason: &str) {
        if let Some(g) = self.config.as_ref().and_then(|c| c.switch_group.clone()) {
            self.emit_event(
                "lan.sync.broadcast",
                Level::Debug,
                json!({"reason": reason, "targets": self.lan_runtime.online.iter().cloned().collect::<Vec<_>>()}),
            );
            for id in self.lan_runtime.online.clone() {
                self.send_snapshot(&id, &g, reason);
            }
        }
    }

    fn send_snapshot(&mut self, id: &str, g: &SwitchGroupSnapshot, reason: &str) {
        if !kvmflow_core::identity::legacy_placeholders(g, &self.lan_runtime.device_id).is_empty() {
            // Old manual presets remain locally usable, but never claim to
            // be remote owners. The authenticated peer report replaces them.
            self.lan_send_profile(id);
            let _ = self.lan_send(
                id,
                Envelope::new("resync", json!({"reason":"legacy_placeholder"})),
            );
            return;
        }
        let _ = self.lan_request(
            id,
            "snapshot",
            json!({"group_id": g.group_id, "snapshot": g, "reason": reason}),
            Duration::from_secs(5),
            Some(commands::PendingKind::Snapshot),
        );
    }

    pub(crate) fn poll_lan(&mut self) {
        for _ in 0..128 {
            let Some(event) = self.lan_runtime.handle.as_ref().and_then(|h| h.try_event()) else {
                break;
            };
            match event {
                LanEvent::Peers(peers) => {
                    let online: BTreeSet<_> = peers
                        .iter()
                        .filter(|p| p.online)
                        .map(|p| p.device_id.clone())
                        .collect();
                    let fresh: Vec<_> = online
                        .difference(&self.lan_runtime.online)
                        .cloned()
                        .collect();
                    let reconnected: Vec<_> = peers
                        .iter()
                        .filter(|p| p.online)
                        .filter(|p| {
                            p.connection_id.as_ref().is_some_and(|connection_id| {
                                self.lan_runtime
                                    .peer_connection_ids
                                    .get(&p.device_id)
                                    .is_none_or(|known| known != connection_id)
                            })
                        })
                        .map(|p| p.device_id.clone())
                        .collect();
                    for peer in &peers {
                        match (&peer.connection_id, peer.online) {
                            (Some(connection_id), true) => {
                                self.lan_runtime
                                    .peer_connection_ids
                                    .insert(peer.device_id.clone(), connection_id.clone());
                            }
                            (_, false) => {
                                self.lan_runtime.peer_connection_ids.remove(&peer.device_id);
                            }
                            _ => {}
                        }
                    }
                    for id in self
                        .lan_runtime
                        .online
                        .difference(&online)
                        .cloned()
                        .collect::<Vec<_>>()
                    {
                        self.lan_sync_state(&id, "pending", Some("E_LAN_OFFLINE"));
                    }
                    if !online.is_empty()
                        && self
                            .lan_runtime
                            .issues
                            .iter()
                            .any(|issue| issue["code"] == "E_LAN_NOT_MEMBER")
                    {
                        self.lan_runtime
                            .issues
                            .retain(|issue| issue["code"] != "E_LAN_NOT_MEMBER");
                        self.notify("lan.sync", json!({"resolved":["E_LAN_NOT_MEMBER"]}));
                    }
                    self.lan_runtime.online = online;
                    let mut peers = peers;
                    for peer in &mut peers {
                        if peer.platform.is_none() {
                            peer.platform = self
                                .lan_runtime
                                .peer_platforms
                                .get(&peer.device_id)
                                .cloned();
                        }
                    }
                    self.notify("lan.peers", json!({"peers": peers}));
                    let mut catchup: BTreeSet<String> = fresh.into_iter().collect();
                    catchup.extend(reconnected);
                    for id in catchup {
                        if let Some(g) = self.config.as_ref().and_then(|c| c.switch_group.clone()) {
                            self.send_snapshot(&id, &g, "catchup");
                        }
                        self.lan_send_profile(&id);
                    }
                }
                LanEvent::Issue { code, message } => self.lan_issue(&code, &message),
                LanEvent::Message {
                    device_id,
                    authenticated_key,
                    generation,
                    message,
                } => {
                    let source = Source {
                        device_id: device_id.clone(),
                        key: authenticated_key,
                        generation,
                    };
                    // An authenticated NON-REFUSAL frame from the record's
                    // peer proves the peer's live trust list pins THIS device
                    // id with THIS exact key (the transport validates both on
                    // admission): a surviving transaction record is therefore
                    // CONFIRMED (N3) — regardless of which session generation
                    // delivered this particular frame. A NOT_MEMBER refusal
                    // ack is the opposite evidence and handled below.
                    let is_refusal = message.t == "ack"
                        && message.ok == Some(false)
                        && message.body.get("code").and_then(Value::as_str)
                            == Some("E_LAN_NOT_MEMBER");
                    if !is_refusal {
                        if let Some(record) = self.read_pairing_record() {
                            if record.get("peer_device_id").and_then(Value::as_str)
                                == Some(source.device_id.as_str())
                                && Self::record_key_matches(&record, &source.key)
                            {
                                self.clear_pairing_record();
                                self.pair_emit_record(
                                    Some(&record),
                                    json!({"phase": "peer_committed"}),
                                );
                            }
                        }
                    }
                    // A membership refusal is about the PEER's trust list,
                    // not about one session's freshness: the peer rejected
                    // our identity (its pins lack us). Honor it even when it
                    // arrives on a session the dial duel already replaced —
                    // with a surviving record naming this peer it is the
                    // definitive one-sided outcome (N3). While a pairing
                    // session is still active the refusal is EXPECTED: the
                    // two-phase commit window has us committed but the peer
                    // not yet — that is not one-sided, so the undo only runs
                    // after the pairing flow itself has ended.
                    if message.t == "ack"
                        && message.ok == Some(false)
                        && message.body.get("code").and_then(Value::as_str)
                            == Some("E_LAN_NOT_MEMBER")
                        && self.lan_runtime.pair.is_none()
                    {
                        let matches_record = self.read_pairing_record().is_some_and(|record| {
                            record.get("peer_device_id").and_then(Value::as_str)
                                == Some(source.device_id.as_str())
                                && Self::record_key_matches(&record, &source.key)
                        });
                        if matches_record {
                            self.undo_one_sided_pin(&source.device_id);
                            continue;
                        }
                    }
                    if self.current_source(&source) {
                        self.lan_message(source, message);
                    } else {
                        // A replaced/closing session's in-flight snapshot or
                        // profile report was dropped while the peer believes
                        // it was delivered. Ask the peer to re-run its
                        // catchup (idempotent snapshot + profile resend) over
                        // the CURRENT session instead of resetting anything:
                        // a disconnect would race the replacement again.
                        let stateful = matches!(message.t.as_str(), "snapshot" | "profile.report");
                        let pinned = self
                            .lan_runtime
                            .members
                            .check(&device_id, &source.key)
                            .is_ok();
                        if stateful && pinned && self.lan_runtime.reset_cooldown.is_none() {
                            self.lan_runtime.reset_cooldown = Some(now_ms() + 2_000);
                            let _ = self.lan_send(&device_id, Envelope::new("resync", json!({})));
                        }
                    }
                }
                LanEvent::SendFailed {
                    device_id,
                    id,
                    code,
                } => {
                    if let Some(id) = id {
                        self.lan_complete(id, false, json!({"code": code}));
                    }
                    self.lan_sync_state(&device_id, "pending", Some(&code));
                }
                LanEvent::Rejected {
                    addr,
                    code,
                    authenticated_key,
                } => {
                    // The same pairing-session guard as the refusal-ack path:
                    // during the two-phase commit window the peer's rejection
                    // is expected, not one-sided.
                    if code == "E_LAN_NOT_MEMBER" && self.lan_runtime.pair.is_none() {
                        // A surviving record whose peer rejects us is the
                        // definitive one-sided outcome (N3) — but ONLY when
                        // the refusing key is the key the record pinned for
                        // that device (N9). An impostor at the recorded
                        // address must not be able to revoke the pin.
                        if let Some(record) = self.read_pairing_record() {
                            let key_matches = authenticated_key
                                .as_ref()
                                .is_some_and(|key| Self::record_key_matches(&record, key));
                            if let Some(peer) = record.get("peer_device_id").and_then(Value::as_str)
                            {
                                let addr_matches = record_matches_addr(&record, &addr)
                                    || self
                                        .lan_runtime
                                        .handle
                                        .as_ref()
                                        .map(|h| h.status())
                                        .is_some_and(|status| {
                                            status.peers.iter().any(|p| {
                                                p.addr.as_ref() == Some(&addr)
                                                    && p.device_id == peer
                                            })
                                        });
                                if key_matches && addr_matches {
                                    self.undo_one_sided_pin(peer);
                                    continue;
                                }
                            }
                        }
                    }
                    // Dialing every discovered computer means unpaired ones
                    // refuse us as a matter of course; only a refusal from a
                    // computer we pinned says our pairing was revoked.
                    if code == "E_LAN_NOT_MEMBER"
                        && !authenticated_key
                            .as_ref()
                            .is_some_and(|key| self.is_active_member_key(key))
                    {
                        self.emit_event(
                            "lan.peer.unpaired_refusal",
                            Level::Debug,
                            json!({"code":code}),
                        );
                        continue;
                    }
                    if code == "E_LAN_NOT_MEMBER" && self.lan_runtime.pair.is_some() {
                        self.emit_event(
                            "lan.pair.admission_pending",
                            Level::Debug,
                            json!({"code":code}),
                        );
                        continue;
                    }
                    self.lan_issue(&code, lan_hint(&code));
                }
                LanEvent::PairReady(link) => self.lan_pair_ready(link),
            }
        }
        if let Some(deadline) = self.lan_runtime.reset_cooldown {
            if now_ms() > deadline {
                self.lan_runtime.reset_cooldown = None;
            }
        }
        self.poll_pair();
        self.poll_lan_commands();
        self.poll_lan_profile();
        // `status` fan-out whenever the local picture changed (§3.3).
        let status = json!({
            "holds_usb": self.tracker.as_ref().is_some_and(|t| t.group_present()),
            "mixed_applied_revision": self.mixed_state().get("revision"),
            "detached": self.mixed_state().get("detached"),
        });
        if status != self.lan_runtime.last_status {
            self.lan_runtime.last_status = status.clone();
            for id in self.lan_runtime.online.clone() {
                let _ = self.lan_send(&id, Envelope::new("status", status.clone()));
            }
        }
    }

    fn lan_sync_state(&mut self, id: &str, status: &str, code: Option<&str>) {
        let state = json!({"device_id": id, "status": status, "code": code});
        self.lan_runtime
            .sync_states
            .insert(id.into(), state.clone());
        self.notify("lan.sync", state);
    }

    fn lan_message(&mut self, source: Source, message: Envelope) {
        let id = source.device_id.clone();
        let result = match message.t.as_str() {
            "ack" => {
                // Refusal reconciliation belongs to poll_lan's single guard,
                // before current-session dispatch. Repeating it here bypasses
                // the active-pairing protection during the two-phase commit.
                if let Some(re) = message.re {
                    self.lan_complete_from(&source, re, message.ok == Some(true), message.body);
                }
                return;
            }
            "snapshot" => self.lan_merge(&source, &message.body),
            "status" => {
                self.lan_observe_status(&source, &message.body);
                return;
            }
            "display.push" => self.lan_display_push(&source, &message.body),
            "profile.report" => {
                self.lan_receive_profile(&source, &message.body);
                return;
            }
            "setup.trigger" => {
                self.lan_receive_trigger(&source, &message.body);
                return;
            }
            "test.begin" | "test.end" | "test.event" => {
                self.lan_test_event(&id, message.t, message.body);
                return;
            }
            "resync" => {
                // The peer dropped one of our in-flight stateful frames on a
                // replaced session; re-run the fresh-session catchup.
                // Receiver-side cooldown (P3): a flapping peer cannot turn
                // the resync into a snapshot storm.
                let now = now_ms();
                let due = self
                    .lan_runtime
                    .resync_cooldowns
                    .get(&id)
                    .is_none_or(|last| now.saturating_sub(*last) >= 2_000);
                if due {
                    self.lan_runtime.resync_cooldowns.insert(id.clone(), now);
                    self.lan_send_profile(&id);
                    if let Some(g) = self.config.as_ref().and_then(|c| c.switch_group.clone()) {
                        self.send_snapshot(&id, &g, "catchup");
                    }
                }
                return;
            }
            _ => Err(CoreError::new("E_LAN_PROTO", "unknown LAN message")),
        };
        self.lan_ack(&id, message.id, result);
    }

    fn lan_observe_status(&mut self, source: &Source, body: &Value) {
        if let Some(state) = self.lan_runtime.sync_states.get_mut(&source.device_id) {
            state["holds_usb"] = body.get("holds_usb").cloned().unwrap_or(Value::Null);
            state["mixed_applied_revision"] = body
                .get("mixed_applied_revision")
                .cloned()
                .unwrap_or(Value::Null);
        }
        // Track H observations for the close transaction's holder decisions.
        if body.get("holds_usb").and_then(Value::as_bool) == Some(true) {
            if self.mixed_runtime.holder.as_deref() != Some(source.device_id.as_str())
                && !self.tracker.as_ref().is_some_and(|t| t.group_present())
            {
                self.mixed_runtime.holder = Some(source.device_id.clone());
                self.mixed_runtime.holder_known = true;
            }
        }
    }

    fn lan_merge(&mut self, source: &Source, body: &Value) -> Result<Value, CoreError> {
        let remote: SwitchGroupSnapshot =
            serde_json::from_value(body.get("snapshot").cloned().unwrap_or(Value::Null))
                .map_err(|e| CoreError::new(E_CONFIG_INVALID, e.to_string()))?;
        let mut cfg = self
            .config
            .clone()
            .ok_or_else(|| CoreError::new(E_CONFIG_INVALID, "no local group"))?;
        let local = cfg
            .switch_group
            .as_ref()
            .ok_or_else(|| CoreError::new(E_CONFIG_INVALID, "no local group"))?;
        if body.get("group_id").and_then(Value::as_str) != Some(local.group_id.as_str())
            || remote.group_id != local.group_id
        {
            return Err(CoreError::new(E_LAN_GROUP_CONFLICT, "different groups"));
        }
        if !remote.devices.is_empty()
            && !remote
                .devices
                .iter()
                .any(|d| d.device_id == cfg.local_device.device_id)
        {
            let code = "E_LAN_TOPOLOGY_MISSING_SELF";
            self.lan_issue(
                code,
                "对方的配置缺少本机，正在用本机资料修复；若未恢复，请在主力机重新添加这台电脑。",
            );
            self.lan_send_profile(&source.device_id);
            return Err(CoreError::new(
                code,
                "snapshot omits the receiving computer; resend topology with its owner profile",
            ));
        }
        // Trust can converge before topology: merge the member list first
        // (authenticated against this session's key), then the sections.
        let (members, _) = sync::merge_members(
            &local.group_sync().members,
            &remote.group_sync().members,
            &AuthenticatedMember {
                device_id: source.device_id.clone(),
                public_key: public_key_string(&source.key),
            },
        )?;
        let mut trusted = local.clone();
        let mut gs = trusted.group_sync();
        gs.members = members;
        trusted.set_group_sync(gs);
        if local.sync.get("setup_pending").is_some() && remote.devices.is_empty() {
            self.lan_send_profile(&source.device_id);
            return Ok(json!({"pending":true}));
        }
        let (merged, changed) = sync::merge_group(
            &trusted,
            &remote,
            &cfg.local_device.device_id,
            &source.device_id,
        )?;
        if let Some(error) = changed.validation_error.as_ref() {
            // Keep the trust list (it converged) but not the broken routing.
            cfg.switch_group = Some(trusted);
            self.lan_save(cfg)?;
            if self.lan_runtime.deferred.len() < 16 {
                self.lan_runtime.deferred.push((source.clone(), remote));
            }
            self.lan_sync_state(&source.device_id, "pending", Some("E_LAN_PROFILE_PENDING"));
            return Ok(json!({"summary": self.lan_summary(), "pending": error}));
        }
        // A changed mixed section must reach the hardware: apply it through
        // the trusted entry BEFORE the merge persists it, so apply_remote
        // sees the old local section, executes the transition (restore or
        // detach) and reports the outcome (P1-4: saving first made the
        // apply a no-op). The merge below then converges the remaining
        // sections; its mixed section is now equal on both sides.
        let mut applied_result = Value::Null;
        if changed.mixed {
            match self.lan_apply_remote_mixed_snapshot(source, &remote) {
                Ok(result) => applied_result = result,
                Err(error) => {
                    // N6: the mixed application FAILED — persisting the
                    // merged snapshot would leave the config changed while
                    // the hardware never executed the transition. Refuse the
                    // whole merge so the peer retries (the released-topology
                    // gate returns the same class of error).
                    return Err(error);
                }
            }
        }
        cfg.switch_group = Some(merged);
        self.lan_save(cfg)?;
        for section in &changed.divergent {
            self.notify(
                "lan.sync",
                json!({"status": "conflict", "code": "lan.sync.divergent", "section": section}),
            );
        }
        if !changed.rejected_profiles.is_empty() {
            self.lan_send_profile(&source.device_id);
        }
        // A relayed topology may include a new member: connect to it and let
        // its own owner profile arrive over a direct authenticated session.
        for member in self
            .config
            .as_ref()
            .and_then(|c| c.switch_group.as_ref())
            .map(|g| g.group_sync().members)
            .unwrap_or_default()
        {
            if !member.removed
                && member.device_id != self.lan_runtime.device_id
                && !self.lan_runtime.online.contains(&member.device_id)
            {
                self.lan_sync_state(&member.device_id, "pending", Some("E_LAN_OFFLINE"));
            }
        }
        self.clear_topology_issue();
        self.lan_sync_state(&source.device_id, "synced", None);
        self.reconcile_pairing_record_from_peer(source, &remote);
        Ok(json!({
            "summary": self.lan_summary(),
            "mixed_applied": applied_result,
        }))
    }

    /// Execute the remote snapshot's mixed section through T1's trusted
    /// in-process entry (mixed.apply_remote), capturing the reply so the
    /// worker never emits a stray JSONL response. Returns the apply result
    /// ({saved, applied, mixed}) for the caller's ack. Never emits
    /// display.push from the merge path.
    fn lan_apply_remote_mixed_snapshot(
        &mut self,
        _source: &Source,
        remote: &SwitchGroupSnapshot,
    ) -> Result<Value, CoreError> {
        let Some(mixed) = remote.mixed.clone() else {
            return Ok(Value::Null);
        };
        let section_meta = remote.group_sync().mixed;
        let holder = self.mixed_runtime.holder.clone();
        let confidence = if self.mixed_runtime.holder_known {
            "known"
        } else {
            "inferred"
        };
        let params = json!({
            "group_id": remote.group_id,
            "revision": mixed.revision,
            "enabled": mixed.enabled,
            "assignment": mixed.assignment,
            "meta": serde_json::to_value(section_meta).unwrap_or(Value::Null),
            "keyboard_holder_device_id": holder,
            "holder_confidence": confidence,
        });
        self.lan_apply_trusted_mixed(&params)
    }

    /// Execute the trusted in-process mixed application, capturing the reply
    /// so the worker never emits a stray JSONL response.
    fn lan_apply_trusted_mixed(&mut self, params: &Value) -> Result<Value, CoreError> {
        // The same release gate as public mutations: while displays are
        // released for exit/update, LAN traffic must not change topology
        // (P2-2).
        if self.released_topology.is_some() {
            return Err(CoreError::new(
                E_BACKEND,
                "displays released for exit/update; remote update blocked until resume",
            ));
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.reply = Some(tx);
        self.handle_mixed_request(0, "mixed.apply_remote", params);
        // handle_mixed_request always responds exactly once.
        match rx.recv() {
            Ok(r) if r.ok => Ok(r.result.unwrap_or(Value::Null)),
            Ok(r) => Err(r
                .error
                .map(|e| CoreError::new(E_CONFIG_INVALID, e.message))
                .unwrap_or_else(|| CoreError::new(E_CONFIG_INVALID, "remote apply failed"))),
            Err(_) => Err(CoreError::new(E_INTERNAL, "remote apply dropped its reply")),
        }
    }

    /// `display.push` receiver: validated against the snapshot, executed with
    /// is_current_session checks immediately before the hardware write.
    fn lan_display_push(&mut self, source: &Source, body: &Value) -> Result<Value, CoreError> {
        let cfg = self
            .config
            .clone()
            .ok_or_else(|| CoreError::new(E_CONFIG_INVALID, "no configuration"))?;
        let group = cfg
            .switch_group
            .as_ref()
            .ok_or_else(|| CoreError::new(E_CONFIG_INVALID, "no switch group"))?;
        let mixed = group
            .mixed
            .as_ref()
            .ok_or_else(|| CoreError::new(E_CONFIG_INVALID, "no mixed section"))?;
        if mixed.enabled {
            return Err(CoreError::new(
                "E_STALE",
                "mixed is still enabled locally; refusing push",
            ));
        }
        let revision = body
            .get("mixed_revision")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);
        if mixed.revision < revision {
            return Err(CoreError::new(
                "E_STALE",
                "local mixed section is older than the push; awaiting merge",
            ));
        }
        let holder = body
            .get("target_device_id")
            .and_then(Value::as_str)
            .ok_or_else(|| CoreError::new(E_CONFIG_INVALID, "target_device_id required"))?;
        let holder_monitors = group
            .devices
            .iter()
            .find(|d| d.device_id == holder)
            .ok_or_else(|| CoreError::new(E_CONFIG_INVALID, "unknown target device"))?;
        let mut writes: Vec<(String, u16)> = Vec::new();
        for write in body
            .get("writes")
            .and_then(Value::as_array)
            .ok_or_else(|| CoreError::new(E_CONFIG_INVALID, "writes array required"))?
        {
            let fingerprint = write
                .get("fingerprint")
                .and_then(Value::as_str)
                .ok_or_else(|| CoreError::new(E_CONFIG_INVALID, "fingerprint required"))?;
            let value = write
                .get("value")
                .and_then(Value::as_u64)
                .ok_or_else(|| CoreError::new(E_CONFIG_INVALID, "value required"))?
                as u16;
            // The value must equal the holder's input in our merged snapshot.
            let expected = holder_monitors
                .monitors
                .iter()
                .find(|m| m.fingerprint == fingerprint)
                .ok_or_else(|| CoreError::new(E_CONFIG_INVALID, "unknown monitor for target"))?;
            if expected.local_input != value {
                return Err(CoreError::new(
                    "E_STALE",
                    "push value disagrees with the merged snapshot",
                ));
            }
            writes.push((fingerprint.to_string(), value));
        }
        let mut results = Vec::new();
        let policy = self.active_retry_policy();
        for (fingerprint, value) in writes {
            // Recheck the CURRENT session immediately before hardware action.
            if !self.current_source(source) || !self.lan_action_allowed() {
                results.push(json!({
                    "fingerprint": fingerprint,
                    "outcome": "failed",
                    "error": {"code": "E_LAN_NOT_MEMBER", "message": "session no longer current"},
                }));
                continue;
            }
            // The monitor must currently be showing THIS device (skipped
            // otherwise): only the current picture owner can push it away.
            let showing_local = self
                .mixed_runtime
                .current
                .get(&fingerprint)
                .and_then(|o| o.clone())
                .is_some_and(|owner| {
                    owner
                        == self
                            .config
                            .as_ref()
                            .map(|c| c.local_device.device_id.clone())
                            .unwrap_or_default()
                });
            if !showing_local {
                results.push(json!({
                    "fingerprint": fingerprint,
                    "outcome": "skipped_not_showing",
                }));
                continue;
            }
            let outcome = self.write_with_retry(&fingerprint, value, &policy, false);
            self.emit_event(
                kinds::DDC_SET,
                if outcome.commanded {
                    Level::Info
                } else {
                    Level::Warn
                },
                json!({
                    "reason": "lan.display.push",
                    "fingerprint": fingerprint,
                    "value": value,
                    "outcome": outcome,
                    "peer": source.device_id,
                    "note": REPORT_NOTE,
                }),
            );
            results.push(json!({
                "fingerprint": fingerprint,
                "outcome": if outcome.commanded { "commanded" } else { "failed" },
                "error": outcome.last_error.map(|e| json!({"message": e})).unwrap_or(Value::Null),
            }));
        }
        Ok(json!({"writes": results}))
    }

    fn lan_send_profile(&mut self, id: &str) {
        let Some(cfg) = self.config.clone() else {
            return;
        };
        let report = ProfileReport::from_config(&cfg, std::env::consts::OS);
        let _ = self.lan_send(
            id,
            Envelope::new(
                "profile.report",
                serde_json::to_value(report).unwrap_or(json!({})),
            ),
        );
    }

    fn lan_receive_profile(&mut self, source: &Source, body: &Value) {
        let Ok(report) = serde_json::from_value::<ProfileReport>(body.clone()) else {
            return;
        };
        if report.device_id != source.device_id {
            return; // only the owner's own report
        }
        if !report.platform.is_empty() {
            self.lan_runtime
                .peer_platforms
                .insert(report.device_id.clone(), report.platform.clone());
        }
        let changed = if self.resume_pair_setup(source, &report) {
            true
        } else {
            self.apply_peer_profile(source, &report)
        };
        let _ = changed;
        self.notify(
            "lan.profile",
            json!({
                "device_id": report.device_id,
                "name": report.name,
                "monitors": report.monitors,
                "profile_rev": report.profile_rev,
            }),
        );
        // Symmetric exchange, once per session generation. If a deferred
        // merge is waiting for this owner's profile, replying with ours also
        // gives the peer a reason to answer on its side; a push lost on a
        // replaced session recovers without waiting for an online change.
        if self.lan_runtime.profile_exchanges.get(&source.device_id) != Some(&source.generation) {
            self.lan_runtime
                .profile_exchanges
                .insert(source.device_id.clone(), source.generation);
            self.lan_send_profile(&source.device_id);
        }
    }

    /// Fold a peer's owner report into our snapshot as a section edit on the
    /// sender's side (merge_group accepts owner-vetted relay through the
    /// authenticated source). Simplest correct route: bump our copy via merge.
    fn clear_topology_issue(&mut self) {
        let count = self.lan_runtime.issues.len();
        self.lan_runtime
            .issues
            .retain(|issue| issue["code"] != "E_LAN_TOPOLOGY_MISSING_SELF");
        if count == self.lan_runtime.issues.len() {
            return;
        }
        self.notify(
            "lan.sync",
            json!({"status":"synced","resolved":["E_LAN_TOPOLOGY_MISSING_SELF"]}),
        );
    }

    fn apply_peer_profile(&mut self, source: &Source, report: &ProfileReport) -> bool {
        let Some(cfg) = self.config.clone() else {
            return false;
        };
        let Some(local) = cfg.switch_group.as_ref() else {
            return false;
        };
        if local.group_id.is_empty() {
            return false;
        }
        // Build the remote view: our snapshot with the owner's fresh profile.
        // A deferred merge (relayed new member whose profile was missing)
        // resolves here: the owner's direct report supplies the profile, so
        // the device entry is created if the relay never placed it.
        let mut remote = local.clone();
        match kvmflow_core::identity::replace_legacy_placeholder(
            &mut remote,
            &cfg.local_device.device_id,
            report,
            now_ms(),
        ) {
            Ok(true) => {
                let mut updated = cfg.clone();
                updated.switch_group = Some(remote);
                if self.lan_save(updated).is_err() {
                    return false;
                }
                self.clear_topology_issue();
                self.lan_broadcast_snapshot("legacy_placeholder_repaired");
                return true;
            }
            Err(error) => {
                self.lan_issue("E_LAN_TOPOLOGY_MISSING_SELF", &error.message);
                return false;
            }
            Ok(false) => {}
        }
        let mut next_rev = report.profile_rev;
        let has_device = remote
            .devices
            .iter()
            .any(|d| d.device_id == report.device_id);
        let mut pending_monitors: Vec<String> = Vec::new();
        for device in &mut remote.devices {
            if device.device_id == report.device_id {
                // Pending monitors are KEPT in the owner's profile (the
                // report itself) but cannot enter the routed device entry:
                // a guessed local_input of 0 would collide across devices
                // and display.push would treat it as a real value (P2-4).
                let monitors = kvmflow_core::identity::routed_profile_monitors(
                    report,
                    &device.monitors,
                    false,
                );
                pending_monitors.extend(
                    report
                        .monitors
                        .iter()
                        .filter(|m| matches!(m.input, kvmflow_core::pairing::ProfileInput::Pending))
                        .map(|m| m.fingerprint.clone()),
                );
                if monitors.is_empty() && !report.monitors.is_empty() {
                    return false; // still pending; nothing to fold
                }
                if !monitors.is_empty() {
                    device.monitors = monitors;
                }
                device.profile_rev = device.profile_rev.max(report.profile_rev);
                next_rev = device.profile_rev;
            }
        }

        let _ = next_rev;
        if !has_device && !report.monitors.is_empty() {
            // The relay's topology was rejected pending this very report;
            // append the owner's device so the retried merge validates.
            // Pending monitors never enter the routed entry (see above);
            // only read-verified values create device monitors.
            let monitors: Vec<_> = report
                .monitors
                .iter()
                .filter(|m| !m.builtin)
                .filter_map(|m| match &m.input {
                    kvmflow_core::pairing::ProfileInput::Value { value, .. } => {
                        Some(kvmflow_core::multidevice::MonitorInput {
                            local_fingerprint: None,
                            fingerprint: m.fingerprint.clone(),
                            label: m.name.clone(),
                            local_input: *value,
                            follow: true,
                        })
                    }
                    kvmflow_core::pairing::ProfileInput::Pending => None,
                })
                .collect();
            let _ = &pending_monitors;
            let used_ports: std::collections::BTreeSet<u16> =
                remote.devices.iter().map(|d| d.port_index).collect();
            let port = (1..=u16::MAX)
                .find(|p| !used_ports.contains(p))
                .unwrap_or(1);
            remote.devices.push(kvmflow_core::multidevice::GroupDevice {
                device_id: report.device_id.clone(),
                name: if report.name.is_empty() {
                    report.device_id.clone()
                } else {
                    report.name.clone()
                },
                port_index: port,
                monitors,
                profile_rev: report.profile_rev,
                available_builtin_screens: Some(
                    report.monitors.iter().filter(|m| m.builtin).count() as u32,
                ),
                arrival_layout_id: None,
            });
        }
        let group_id = local.group_id.clone();
        match sync::merge_group(
            local,
            &remote,
            &cfg.local_device.device_id,
            &source.device_id,
        ) {
            Ok((merged, changed)) => {
                if changed.validation_error.is_some() {
                    return false;
                }
                let mut updated = cfg.clone();
                updated.switch_group = Some(merged);
                if self.lan_save(updated).is_ok() {
                    // A deferred merge may now validate: retry once. The
                    // queued source must STILL be a current pinned session —
                    // a revocation or key change since queuing invalidates
                    // the replay (P2-1).
                    let deferred = std::mem::take(&mut self.lan_runtime.deferred);
                    for (deferred_source, snapshot) in deferred {
                        if self.current_source(&deferred_source) {
                            let _ = self.lan_merge(
                                &deferred_source,
                                &json!({"group_id": group_id, "snapshot": snapshot}),
                            );
                        }
                    }
                    return true;
                }
            }
            Err(_) => {}
        }
        false
    }

    /// Low-frequency pending-input polling (§6.1 step 4): while any local
    /// monitor input was never observed (source Unknown), retry reading it
    /// (the user may have just switched the monitor to this host) and fold
    /// the learned value into the profile, then push it to the peers.
    fn poll_lan_profile(&mut self) {
        let now = now_ms();
        if now < self.lan_runtime.next_profile_read {
            return;
        }
        self.lan_runtime.next_profile_read = now + 2_000;
        // Only a secondary (or a machine with an active pairing) polls for
        // pending inputs: the primary's own inputs are learned by its setup
        // flow, and rewriting unrelated configs during runtime breaks the
        // byte-stability expectations of setup/update flows.
        if !self.lan_runtime.secondary {
            return;
        }
        if let Some(mut cfg) = self.config.clone() {
            let mut learned: Vec<String> = Vec::new();
            let fingerprints: Vec<String> = cfg
                .local_device
                .monitors
                .iter()
                .map(|m| m.fingerprint.clone())
                .collect();
            for fingerprint in &fingerprints {
                // At most ONE blocking read per tick (≤600 ms): the worker
                // also serves every UI request and USB debounce window
                // (P2-5).
                if !learned.is_empty() {
                    break;
                }
                let monitor_source = cfg
                    .local_device
                    .monitors
                    .iter()
                    .find(|m| &m.fingerprint == fingerprint)
                    .map(|m| m.source);
                if monitor_source != Some(kvmflow_core::config::InputSource::Unknown) {
                    continue;
                }
                let backend_id = cfg.backend_fingerprint(fingerprint).to_owned();
                if let Ok(read) = self
                    .backend
                    .read_input_with_timeout(&backend_id, Duration::from_millis(600))
                {
                    if let Some(monitor) = cfg
                        .local_device
                        .monitors
                        .iter_mut()
                        .find(|m| &m.fingerprint == fingerprint)
                    {
                        monitor.local_input = read.value;
                        monitor.source = kvmflow_core::config::InputSource::LearnedActiveRead;
                        learned.push(fingerprint.clone());
                    }
                }
            }
            if !learned.is_empty() {
                // An unpersisted v1 document stays byte-identical until the
                // first explicit save; learned pending inputs are reported
                // over LAN and kept in memory only in that case.
                let disk_is_v1 = std::fs::read_to_string(&self.config_path)
                    .ok()
                    .and_then(|text| {
                        serde_json::from_str::<serde_json::Value>(&text)
                            .ok()
                            .and_then(|v| v.get("schema_version").and_then(Value::as_u64))
                    })
                    == Some(1);
                if let Some(group) = cfg.switch_group.as_mut() {
                    let local_id = cfg.local_device.device_id.clone();
                    if let Some(device) = group.devices.iter_mut().find(|d| d.device_id == local_id)
                    {
                        for monitor in &cfg.local_device.monitors {
                            if let Some(entry) = device
                                .monitors
                                .iter_mut()
                                .find(|m| m.backend_fingerprint() == &monitor.fingerprint)
                            {
                                entry.local_input = monitor.local_input;
                            }
                        }
                        device.profile_rev = device.profile_rev.saturating_add(1);
                    }
                }
                let saved = if disk_is_v1 {
                    // Keep the on-disk v1 untouched; apply in memory only.
                    if cfg.validate().is_ok() {
                        self.config = Some(cfg);
                        true
                    } else {
                        false
                    }
                } else {
                    self.lan_save(cfg).is_ok()
                };
                if saved {
                    self.notify(
                        "lan.profile",
                        json!({"learned": learned, "phase": "inputs_learned"}),
                    );
                    for peer in self.lan_runtime.online.clone() {
                        self.lan_send_profile(&peer);
                    }
                    if !disk_is_v1 {
                        self.lan_broadcast_snapshot("change");
                    }
                }
            }
        }
        // Verify the shared USB trigger once (§6.1: secondary reports result).
        if self.lan_runtime.shared_trigger.is_some() && !self.lan_runtime.trigger_verified {
            if let Some(trigger) = self.active_trigger() {
                let seen = self
                    .last_snapshot
                    .iter()
                    .any(|d| d.vid_pid == trigger.anchor.vid_pid);
                if seen {
                    self.lan_runtime.trigger_verified = true;
                    self.notify(
                        "lan.setup",
                        json!({"trigger_verified": true, "trigger": self.lan_runtime.shared_trigger}),
                    );
                    for peer in self.lan_runtime.online.clone() {
                        let _ = self.lan_send(
                            &peer,
                            Envelope::new(
                                "setup.trigger",
                                json!({"verified": true, "trigger": self.lan_runtime.shared_trigger}),
                            ),
                        );
                    }
                }
            }
        }
    }

    fn lan_receive_trigger(&mut self, source: &Source, body: &Value) {
        if !self.current_source(source) {
            return;
        }
        if body.get("verified").and_then(Value::as_bool) == Some(true) {
            self.lan_runtime.trigger_verified = true;
            self.notify(
                "lan.setup",
                json!({"trigger_verified": true, "by": source.device_id}),
            );
            return;
        }
        let Some(trigger) = body.get("trigger") else {
            return;
        };
        self.lan_runtime.shared_trigger = Some(trigger.clone());
        self.notify("lan.setup", json!({"shared_trigger": trigger}));
        // A secondary without its own trigger profile adopts the shared one
        // (shared topology per §1.1; trigger config itself stays local).
        let empty_trigger = self
            .config
            .as_ref()
            .map(|c| c.trigger.anchor.vid_pid.is_empty())
            .unwrap_or(true);
        if self.lan_runtime.secondary && empty_trigger {
            if let Some(cfg) = self.config.clone() {
                let mut next = cfg.clone();
                if let (Some(anchor), Some(members)) = (
                    trigger.get("anchor").cloned(),
                    trigger.get("members").cloned(),
                ) {
                    next.trigger.anchor =
                        serde_json::from_value(anchor).unwrap_or(next.trigger.anchor);
                    next.trigger.members =
                        serde_json::from_value(members).unwrap_or(next.trigger.members);
                    if next.trigger != cfg.trigger {
                        if self.lan_save(next).is_ok() {
                            self.notify(
                                "lan.setup",
                                json!({"phase": "trigger_adopted", "source": source.device_id}),
                            );
                        }
                    }
                }
            }
        }
    }

    fn lan_test_event(&mut self, id: &str, kind: String, body: Value) {
        let now = now_ms();
        match kind.as_str() {
            "test.begin" => {
                let Some(test_id) = body.get("test_id").and_then(Value::as_str) else {
                    return;
                };
                let duration = body
                    .get("duration_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(30_000)
                    .min(120_000);
                self.lan_runtime.test = Some(test_evidence::TestEvidence::start(
                    test_id.to_string(),
                    now,
                    duration,
                ));
            }
            "test.end" => {
                // The starter took its own snapshot; keep nothing behind.
                self.lan_runtime.test = None;
            }
            "test.event" => {
                let Some(test_id) = body.get("test_id").and_then(Value::as_str) else {
                    return;
                };
                let local_id = self
                    .config
                    .as_ref()
                    .map(|c| c.local_device.device_id.clone())
                    .unwrap_or_default();
                let Some(mut evidence) = self.lan_runtime.test.take() else {
                    return;
                };
                if evidence.test_id != test_id {
                    self.lan_runtime.test = Some(evidence);
                    return;
                }
                let frames = evidence.accept_remote_event(id, body, &local_id, now);
                let summary = evidence.summary();
                self.lan_runtime.test = Some(evidence);
                for frame in frames {
                    for peer in self.lan_runtime.online.clone() {
                        if peer != id {
                            let _ =
                                self.lan_send(&peer, Envelope::new("test.event", frame.clone()));
                        }
                    }
                }
                self.notify("lan.test", summary);
            }
            _ => {}
        }
    }

    /// Tag a local `switch.report` with the active test's correlation fields
    /// and stream it to the other computer (called from `Runtime::notify`).
    pub(crate) fn lan_test_report(&mut self, report: &mut Value) {
        if !report.get("per_monitor").is_some_and(Value::is_array) {
            return;
        }
        let Some(local_id) = self
            .config
            .as_ref()
            .map(|c| c.local_device.device_id.clone())
        else {
            return;
        };
        let now = now_ms();
        let Some(mut evidence) = self.lan_runtime.test.take() else {
            return;
        };
        if now > evidence.ends_ms {
            self.lan_runtime.test = Some(evidence);
            return;
        }
        let frame = evidence.stamp_local_report(report.clone(), &local_id, now);
        let summary = evidence.summary();
        self.lan_runtime.test = Some(evidence);
        if let Some(frame) = frame {
            *report = frame.clone();
            for peer in self.lan_runtime.online.clone() {
                let _ = self.lan_send(&peer, Envelope::new("test.event", frame.clone()));
            }
            self.notify("lan.test", summary);
        }
    }

    /// Local USB observation during a test (`picture_arrived` / `display_left`).
    pub(crate) fn lan_test_observation(&mut self, kind: &str) {
        let Some(local_id) = self
            .config
            .as_ref()
            .map(|c| c.local_device.device_id.clone())
        else {
            return;
        };
        let now = now_ms();
        let Some(mut evidence) = self.lan_runtime.test.take() else {
            return;
        };
        if now > evidence.ends_ms {
            self.lan_runtime.test = Some(evidence);
            return;
        }
        let frame = evidence.record_observation(kind, &local_id, now);
        let summary = evidence.summary();
        self.lan_runtime.test = Some(evidence);
        if let Some(frame) = frame {
            for peer in self.lan_runtime.online.clone() {
                let _ = self.lan_send(&peer, Envelope::new("test.event", frame.clone()));
            }
            self.notify("lan.test", summary);
        }
    }

    fn lan_save(&mut self, cfg: MultiDeviceConfig) -> Result<(), CoreError> {
        if let Err(e) = cfg.validate() {
            return Err(CoreError::new(E_CONFIG_INVALID, e.to_string()));
        }
        config_store::write_atomically(
            &self.config_path,
            config_store::persisted_config_text(&cfg).as_bytes(),
        )
        .map_err(|e| CoreError::new(E_CONFIG_IO, e.to_string()))?;
        self.config = Some(cfg);
        self.publish_lan_members();
        Ok(())
    }
}

const DEFAULT_LAN_PORT: u16 = 47820;

fn lan_hint(code: &str) -> &'static str {
    match code {
        "E_LAN_KEY_CHANGED" => "对方的安全密钥变了，请重新配对。",
        "E_LAN_NOT_MEMBER" => "连接未授权或已被取消，请重新配对。",
        "E_LAN_LISTEN" => {
            "无法监听局域网端口。请检查端口占用和 Windows 防火墙设置。"
        }
        _ => {
            "无法连接对面电脑。请确认两台电脑在同一局域网；Windows 请使用专用网络并允许 KVMFlow，代理工具应放行局域网直连。双向拨号都失败才需要检查防火墙。"
        }
    }
}

/// The record's peer may be resolvable by the address the rejection names
/// when the status lookup cannot (peer offline, id unknown yet).
/// The record's peer may be resolvable by the address the rejection names
/// when the status lookup cannot (peer offline, id unknown yet). A listener
/// is reachable through every local interface form (127.0.0.1, ::1, fe80…),
/// so the comparison matches the RECORD's address against the event address
/// by port plus same-host loopback equivalence — never a bare string equal.
fn record_matches_addr(record: &Value, addr: &SocketAddr) -> bool {
    let Some(recorded) = record.get("peer_addr").and_then(Value::as_str) else {
        return false;
    };
    let Ok(recorded_addr) = recorded.parse::<SocketAddr>() else {
        return false;
    };
    if recorded_addr.port() != addr.port() {
        return false;
    }
    if recorded_addr.ip() == addr.ip() {
        return true;
    }
    // Loopback forms (127.0.0.1 / ::1) may name the same local listener.
    // Link-local IPv6 is deliberately NOT treated as interchangeable: an
    // arbitrary fe80 address is not the recorded peer (N9).
    let local_form = |ip: std::net::IpAddr| match ip {
        std::net::IpAddr::V4(v4) => v4.is_loopback(),
        std::net::IpAddr::V6(v6) => v6.is_loopback(),
    };
    local_form(recorded_addr.ip()) && local_form(addr.ip())
}

#[cfg(test)]
mod record_key_tests {
    use super::*;

    fn key(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    fn runtime(dir: &Path) -> Runtime {
        Runtime {
            runtime_seq: 0,
            lan_runtime: lan_runtime::LanRuntime::default(),
            backend: Box::new(backends::simulated::SimulatedBackend::new("idle")),
            topology: Box::new(topology::NoopDisplayTopology),
            released_topology: None,
            released_enabled: false,
            mixed_runtime: mixed::MixedRuntime::default(),
            tracker: None,
            fsm: SwitchFsm::new(),
            config: None,
            enabled: true,
            last_multi_device_report: None,
            last_layout_application: None,
            bounce_watch: None,
            usb_snapshot_initialized: false,
            config_path: dir.join("config.json"),
            log: SessionLog::open(dir.join("test.jsonl")),
            stdout: std::io::stdout(),
            notification: Some(Arc::new(|_, _| {})),
            reply: None,
            last_snapshot: Vec::new(),
            learning_baseline: None,
            last_candidates: None,
            wizard_active: false,
            config_recovery: None,
        }
    }

    fn owner_config() -> MultiDeviceConfig {
        serde_json::from_value(json!({
            "schema_version":2,
            "local_device":{"device_id":"dev-local","host_label":"Primary","monitors":[
                {"fingerprint":"SAC-2763-local","label":"G73","local_input":16,"source":"learned_active_read"},
                {"fingerprint":"SAC-2466-local","label":"G52","local_input":8,"source":"learned_active_read"}]},
            "switch_group":{"group_id":"grp-ready","revision":1,"devices":[
                {"device_id":"dev-local","name":"Primary","port_index":1,"monitors":[{"fingerprint":"SAC-2763-local","local_input":16},{"fingerprint":"SAC-2466-local","local_input":8}]},
                {"device_id":"win","name":"Old guess","port_index":2,"monitors":[{"fingerprint":"SAC-2763-local","local_input":17},{"fingerprint":"SAC-2466-local","local_input":9}]}]},
            "trigger":{"anchor":{"vid_pid":"1a40:0101"},"members":[{"vid_pid":"3837:303c"}]}
        })).unwrap()
    }

    #[test]
    fn builtin_profile_cleanup_removes_the_old_routed_internal_screen() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = runtime(dir.path());
        let mut cfg = owner_config();
        let mut internal = cfg.local_device.monitors[0].clone();
        internal.fingerprint = "UNK-internal".into();
        cfg.local_device.monitors.push(internal);
        let owner = &mut cfg.switch_group.as_mut().unwrap().devices[0];
        let mut routed = owner.monitors[0].clone();
        routed.fingerprint = "UNK-internal".into();
        owner.monitors.push(routed);
        rt.mixed_runtime
            .displays
            .push(kvmflow_core::mixed::TopologyDisplay {
                fingerprint: "UNK-internal".into(),
                builtin: true,
                connected: true,
            });
        rt.strip_builtin_profile(&mut cfg);
        cfg.validate().unwrap();
        assert_eq!(cfg.local_device.monitors.len(), 2);
        assert_eq!(
            cfg.switch_group.as_ref().unwrap().devices[0].monitors.len(),
            2
        );
        let once = cfg.clone();
        rt.strip_builtin_profile(&mut cfg);
        assert_eq!(cfg, once);
    }

    #[test]
    fn every_snapshot_command_withholds_unpaired_legacy_devices() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = runtime(dir.path());
        rt.lan_runtime.device_id = "dev-local".into();
        let group = owner_config().switch_group.unwrap();
        let error = rt
            .lan_request(
                "dev-peer",
                "snapshot",
                json!({"snapshot":group}),
                Duration::from_secs(1),
                None,
            )
            .unwrap_err();
        assert_eq!(error.code, "E_LAN_SETUP_PENDING");
        assert!(rt.lan_summary().as_object().unwrap().is_empty());
    }

    #[test]
    fn missing_self_snapshot_returns_actionable_error_without_mutating_config() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = runtime(dir.path());
        let cfg = owner_config();
        let original = serde_json::to_value(&cfg).unwrap();
        rt.config = Some(cfg.clone());
        let mut incoming = cfg.switch_group.unwrap();
        incoming.devices[0].device_id = "dev-other".into();
        let source = Source {
            device_id: "dev-peer".into(),
            key: key(2),
            generation: SessionGeneration::for_test(),
        };
        let result = rt.lan_merge(
            &source,
            &json!({"group_id":incoming.group_id,"snapshot":incoming}),
        );
        assert_eq!(result.unwrap_err().code, "E_LAN_TOPOLOGY_MISSING_SELF");
        assert_eq!(
            serde_json::to_value(rt.config.as_ref().unwrap()).unwrap(),
            original
        );
        assert!(rt
            .lan_runtime
            .issues
            .iter()
            .any(|issue| issue["code"] == "E_LAN_TOPOLOGY_MISSING_SELF"));
    }

    #[test]
    fn authenticated_owner_profile_repairs_placeholder_and_removes_ghost_displays() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = runtime(dir.path());
        rt.config = Some(owner_config());
        let source = Source {
            device_id: "dev-peer".into(),
            key: key(2),
            generation: SessionGeneration::for_test(),
        };
        let report: ProfileReport = serde_json::from_value(json!({"device_id":"dev-peer","name":"Studio Mac","platform":"macos","profile_rev":3,"monitors":[
            {"fingerprint":"SAC-2763-owner","name":"G73","builtin":false,"input":{"state":"value","value":15,"source":"learned_active_read"}},
            {"fingerprint":"SAC-2466-owner","name":"G52","builtin":false,"input":{"state":"value","value":7,"source":"learned_active_read"}},
            {"fingerprint":"UNK-internal","name":"Internal","builtin":true,"input":{"state":"pending"}}
        ]})).unwrap();
        assert!(rt.apply_peer_profile(&source, &report));
        let cfg = rt.config.as_ref().unwrap();
        let group = cfg.switch_group.as_ref().unwrap();
        assert_eq!(group.devices.len(), 2);
        let peer = group
            .devices
            .iter()
            .find(|d| d.device_id == "dev-peer")
            .unwrap();
        assert_eq!(peer.name, "Studio Mac");
        assert_eq!(peer.port_index, 2);
        assert_eq!(peer.monitors.len(), 2);
        assert_eq!(peer.available_builtin_screens, Some(1));
        assert!(peer
            .monitors
            .iter()
            .all(|m| m.backend_fingerprint().ends_with("owner")));
        let persisted: MultiDeviceConfig =
            serde_json::from_slice(&std::fs::read(&rt.config_path).unwrap()).unwrap();
        assert_eq!(
            config_store::persisted_config_text(&persisted),
            config_store::persisted_config_text(cfg)
        );
        assert!(
            !rt.apply_peer_profile(&source, &report),
            "same owner report must be idempotent"
        );
    }

    #[test]
    fn startup_repairs_beta3_snapshot_that_displaced_the_local_owner_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = runtime(dir.path());
        let mut cfg = owner_config();
        cfg.local_device.device_id = "dev-secondary".into();
        cfg.local_device.host_label = "Studio Mac".into();
        cfg.local_device.monitors[0].local_input = 15;
        cfg.local_device.monitors[1].local_input = 7;
        std::fs::write(&rt.config_path, config_store::persisted_config_text(&cfg)).unwrap();
        rt.load_config();
        let loaded = rt.config.as_ref().unwrap();
        loaded.validate().unwrap();
        let peer = loaded
            .switch_group
            .as_ref()
            .unwrap()
            .devices
            .iter()
            .find(|d| d.device_id == "dev-secondary")
            .unwrap();
        assert_eq!(peer.port_index, 2);
        assert_eq!(peer.name, "Studio Mac");
        let once = std::fs::read(&rt.config_path).unwrap();
        rt.load_config();
        assert_eq!(std::fs::read(&rt.config_path).unwrap(), once);
        assert_eq!(
            std::fs::read_dir(dir.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().contains("pre-placeholder"))
                .count(),
            1
        );
    }

    #[test]
    fn record_confirmation_requires_the_pinned_peer_key() {
        let record = json!({
            "peer_device_id": "dev-win",
            "members": [
                {"device_id": "dev-mac", "public_key": public_key_string(&key(1)), "removed": false},
                {"device_id": "dev-win", "public_key": public_key_string(&key(2)), "removed": false}
            ]
        });
        assert!(Runtime::record_key_matches(&record, &key(2)));
        assert!(!Runtime::record_key_matches(&record, &key(3)));

        let removed = json!({
            "peer_device_id": "dev-win",
            "members": [
                {"device_id": "dev-win", "public_key": public_key_string(&key(2)), "removed": true}
            ]
        });
        assert!(!Runtime::record_key_matches(&removed, &key(2)));
    }

    /// poll_lan deliberately declines reconciliation during two-phase commit.
    /// Its current-session ack dispatch must not undo that decision a second
    /// time. The old inner handler removed even this matching durable record.
    #[test]
    fn refusal_ack_inner_dispatch_preserves_the_pairing_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = runtime(dir.path());

        let record_path = dir.path().join("pairing.pending.json");
        let record = json!({
            "peer_device_id": "dev-win",
            "members": [{"device_id": "dev-win", "public_key": public_key_string(&key(2)), "removed": false}]
        });
        std::fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
        let mut refusal = Envelope::new("ack", json!({"code": "E_LAN_NOT_MEMBER"}));
        refusal.ok = Some(false);
        rt.lan_message(
            Source {
                device_id: "dev-win".into(),
                key: key(2),
                generation: SessionGeneration::for_test(),
            },
            refusal,
        );
        assert_eq!(
            rt.read_pairing_record(),
            Some(record),
            "inner ack handler bypassed the outer active-pairing guard"
        );
    }

    #[test]
    fn link_local_addresses_are_not_interchangeable_with_a_recorded_peer() {
        let record = json!({"peer_addr": "[fe80::1]:47820"});
        let other: SocketAddr = "[fe80::2]:47820".parse().unwrap();
        assert!(!record_matches_addr(&record, &other));
        let loopback: SocketAddr = "127.0.0.1:47820".parse().unwrap();
        let loopback6: SocketAddr = "[::1]:47820".parse().unwrap();
        let record_loopback = json!({"peer_addr": loopback.to_string()});
        assert!(record_matches_addr(&record_loopback, &loopback6));
    }
}
