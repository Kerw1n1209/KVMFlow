//! S7/S8 outbound coordination: tracked requests with deadlines, the close
//! mixed transaction (§3.4 three phases) and the frontend `lan.*` requests.
//! All state lives on the runtime worker; transport only delivers frames.
use super::*;
use std::time::Instant;

const MIXED_SET_TIMEOUT: Duration = Duration::from_secs(8);
const PUSH_BASE_TIMEOUT: Duration = Duration::from_secs(5);
const PUSH_PER_DISPLAY: Duration = Duration::from_millis(1500);

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PendingKind {
    Snapshot,
    DisplayPush { txn: String, step_id: String },
}

pub(crate) struct Pending {
    pub id: u64,
    pub device_id: String,
    pub kind: PendingKind,
    pub deadline: Instant,
}

#[derive(Debug)]
struct PushStep {
    step_id: String,
    /// Current picture owner that must execute the DDC write.
    device_id: String,
    fingerprint: String,
    target_value: u16,
    status: &'static str,
}

pub(crate) struct MixedTransaction {
    /// Section version identity of the pending work (stable pending id base).
    id: String,
    holder: String,
    holder_local: bool,
    /// The mixed revision this close is executing; phase B passes once H
    /// reports having applied ≥ this revision (N2).
    closing_revision: u64,
    /// Snapshot request awaiting H's application ack (phase B).
    holder_request: Option<u64>,
    phase_b_deadline: Option<Instant>,
    pushes: Vec<PushStep>,
    cursor: usize,
    phase_c_deadline: Option<Instant>,
    finished: Option<&'static str>,
}

fn core_err(code: &'static str, message: impl Into<String>) -> CoreError {
    CoreError::new(code, message.into())
}

impl Runtime {
    /// Send a request envelope and register its deadline.
    pub(crate) fn lan_request(
        &mut self,
        device_id: &str,
        t: &str,
        body: Value,
        timeout: Duration,
        kind: Option<PendingKind>,
    ) -> Result<u64, CoreError> {
        let handle = self
            .lan_runtime
            .handle
            .as_ref()
            .ok_or_else(|| core_err("E_LAN_DISABLED", "LAN disabled"))?;
        let id = self.lan_runtime.next_id + 1;
        let mut message = crate::lan::messages::Envelope::new(t, body);
        message.id = Some(id);
        handle
            .send(device_id, message)
            .map_err(|e| core_err(e.code, e.message))?;
        self.lan_runtime.next_id = id;
        if let Some(kind) = kind {
            self.lan_runtime.pending.insert(
                id,
                Pending {
                    id,
                    device_id: device_id.to_string(),
                    kind,
                    deadline: Instant::now() + timeout,
                },
            );
        }
        Ok(id)
    }

    pub(crate) fn lan_complete(&mut self, id: u64, ok: bool, body: Value) {
        if let Some(pending) = self.lan_runtime.pending.remove(&id) {
            self.lan_step_result(&pending, ok, body);
        }
    }

    pub(crate) fn lan_complete_from(&mut self, source: &Source, re: u64, ok: bool, body: Value) {
        let Some(pending) = self.lan_runtime.pending.remove(&re) else {
            return;
        };
        // An ack must come from the device the request was sent to, over a
        // still-current session (validated by the caller's source check).
        if pending.device_id != source.device_id {
            self.lan_runtime.pending.insert(re, pending);
            return;
        }
        self.lan_step_result(&pending, ok, body);
    }

    fn lan_step_result(&mut self, pending: &Pending, ok: bool, body: Value) {
        match &pending.kind {
            PendingKind::Snapshot => {
                // Phase B gate (N2): H acknowledged the snapshot request we
                // issued. The gate criterion is H's REPORTED applied mixed
                // revision (from this ack's summary or its status frames):
                // once H reports ≥ the closing revision, H has the disabled
                // section and has run its restore — whether OUR snapshot
                // was the frame that delivered it (the earlier broadcast may
                // already have applied it, making our ack's applied flag
                // null) is irrelevant.
                if ok {
                    if let Some(txn) = self.lan_runtime.mixed_txn.as_mut() {
                        if txn.holder_request == Some(pending.id) {
                            let reported = body["summary"]["mixed"][0]
                                .as_u64()
                                .or_else(|| body["summary"]["mixed"].as_u64())
                                .or_else(|| body["mixed_applied"]["mixed"]["revision"].as_u64())
                                .or_else(|| {
                                    body["mixed_applied"]["mixed"]["applied_revision"].as_u64()
                                });
                            if reported.is_some_and(|rev| rev >= txn.closing_revision) {
                                txn.holder_request = None; // phase B complete
                                txn.phase_b_deadline = None;
                            }
                        }
                    }
                }
            }
            PendingKind::DisplayPush { txn, step_id } => {
                let Some(transaction) = self.lan_runtime.mixed_txn.as_mut() else {
                    return;
                };
                if &transaction.id != txn {
                    return;
                }
                if let Some(step) = transaction
                    .pushes
                    .iter_mut()
                    .find(|s| &s.step_id == step_id)
                {
                    step.status = if ok {
                        "acked"
                    } else if body.get("code").and_then(Value::as_str) == Some("E_LAN_OFFLINE") {
                        "offline"
                    } else {
                        "failed"
                    };
                }
            }
        }
    }

    fn txn_start(&mut self) {
        let Some(cfg) = self.config.as_ref().and_then(|c| c.switch_group.clone()) else {
            return;
        };
        let Some(mixed) = cfg.mixed.as_ref() else {
            return;
        };
        // Only closing work dispatches display.push (§3.4). Enabling work is
        // executed by each peer's own reconcile after applying the section.
        if mixed.enabled {
            return;
        }
        let pending = self.mixed_runtime.pending_remote.clone();
        if pending.is_empty() || self.lan_runtime.mixed_txn.is_some() {
            return;
        }
        let Some(holder) = self.mixed_runtime.holder.clone() else {
            return;
        };
        if !self.mixed_runtime.holder_known {
            return;
        }
        let local_id = self
            .config
            .as_ref()
            .map(|c| c.local_device.device_id.clone())
            .unwrap_or_default();
        let version_key = pending
            .first()
            .map(|p| p.id.clone())
            .unwrap_or_else(|| holder.clone());
        let mut steps = Vec::new();
        for entry in &pending {
            // Resolve the picture owner: an observed owner wins; otherwise
            // the pre-close assignment identifies it (§3.4 占屏电脑).
            let owner = entry.device_id.clone().or_else(|| {
                mixed
                    .assignment
                    .iter()
                    .find(|a| a.fingerprint == entry.fingerprint)
                    .map(|a| a.device_id.clone())
            });
            let Some(owner) = owner else {
                continue;
            };
            if owner == local_id {
                continue; // handled locally by T1's own plan
            }
            let Some(value) = cfg
                .devices
                .iter()
                .find(|d| d.device_id == holder)
                .and_then(|d| {
                    d.monitors
                        .iter()
                        .find(|m| m.fingerprint == entry.fingerprint)
                })
                .map(|m| m.local_input)
            else {
                continue;
            };
            steps.push(PushStep {
                step_id: entry.id.clone(),
                device_id: owner.clone(),
                fingerprint: entry.fingerprint.clone(),
                target_value: value,
                status: "queued",
            });
        }
        if steps.is_empty() {
            return;
        }
        // Phase B: H must acknowledge the new section before any push. The
        // local holder has already restored its connections in phase A.
        let holder_local = holder == local_id;
        let mut txn = MixedTransaction {
            id: version_key,
            holder: holder.clone(),
            holder_local,
            closing_revision: mixed.revision,
            holder_request: None,
            phase_b_deadline: None,
            pushes: steps,
            cursor: 0,
            phase_c_deadline: None,
            finished: None,
        };
        if holder_local {
            txn.phase_b_deadline = Some(Instant::now()); // already satisfied
        } else if self.lan_runtime.online.contains(&holder) {
            match self.lan_request(
                &holder,
                "snapshot",
                json!({"group_id": cfg.group_id, "snapshot": cfg, "reason": "change"}),
                MIXED_SET_TIMEOUT,
                Some(PendingKind::Snapshot),
            ) {
                Ok(id) => {
                    txn.holder_request = Some(id);
                    txn.phase_b_deadline = Some(Instant::now() + MIXED_SET_TIMEOUT);
                }
                Err(_) => {
                    txn.pushes.iter_mut().for_each(|s| s.status = "offline");
                    txn.finished = Some("holder_offline");
                }
            }
        } else {
            txn.pushes.iter_mut().for_each(|s| s.status = "offline");
            txn.finished = Some("holder_offline");
        }
        self.notify(
            "lan.sync",
            json!({"status":"mixed_closing","holder":holder,"steps":txn.pushes.len()}),
        );
        self.lan_runtime.mixed_txn = Some(txn);
    }

    /// Advance the close transaction: phase B ack gate, sequential phase C
    /// pushes, per-step rereads of live state before every hardware action.
    fn txn_advance(&mut self) {
        let Some(mut txn) = self.lan_runtime.mixed_txn.take() else {
            return;
        };
        // Validity re-check: the section version must still be active work.
        let still_active = self
            .mixed_runtime
            .pending_remote
            .iter()
            .any(|p| p.id == txn.id)
            || txn.pushes.iter().any(|s| s.step_id == txn.id);
        let mixed_disabled = self
            .config
            .as_ref()
            .and_then(|c| c.switch_group.as_ref())
            .and_then(|g| g.mixed.as_ref())
            .is_some_and(|m| !m.enabled);
        if !still_active || !mixed_disabled {
            txn.pushes.iter_mut().for_each(|s| s.status = "superseded");
            txn.finished = Some("superseded");
            self.txn_finish(&txn);
            return;
        }
        if let Some(finished) = txn.finished {
            self.txn_finish(&{
                let mut t = txn;
                t.finished = Some(finished);
                t
            });
            return;
        }
        // Phase B gate.
        if let Some(deadline) = txn.phase_b_deadline {
            if !txn.holder_local && txn.holder_request.is_some() {
                // The same revision criterion from H's observed status
                // frames: H broadcasting an applied revision ≥ the close
                // satisfies the gate even if the snapshot ack is delayed.
                let holder_applied = self
                    .lan_runtime
                    .sync_states
                    .get(&txn.holder)
                    .and_then(|s| s["mixed_applied_revision"].as_u64())
                    .is_some_and(|rev| rev >= txn.closing_revision);
                if holder_applied {
                    txn.holder_request = None;
                    txn.phase_b_deadline = None;
                } else if Instant::now() > deadline {
                    // H never acknowledged: skip phase C entirely (§3.4.2).
                    txn.pushes
                        .iter_mut()
                        .filter(|s| s.status == "queued")
                        .for_each(|s| s.status = "timed_out");
                    txn.finished = Some("holder_ack_timeout");
                    self.txn_finish(&txn);
                    return;
                } else {
                    self.lan_runtime.mixed_txn = Some(txn);
                    return;
                }
            }
        }
        // Phase C: sequential pushes, one device at a time.
        loop {
            let Some(step) = txn.pushes.get_mut(txn.cursor) else {
                txn.finished = Some("completed");
                self.txn_finish(&txn);
                return;
            };
            match step.status {
                "queued" => {
                    if !self.lan_runtime.online.contains(&step.device_id) {
                        step.status = "offline";
                        txn.cursor += 1;
                        continue;
                    }
                    // Reread H and confidence immediately before dispatch.
                    let holder_now = self.mixed_runtime.holder.clone();
                    let confidence_ok = self.mixed_runtime.holder_known;
                    let active = self
                        .mixed_runtime
                        .pending_remote
                        .iter()
                        .any(|p| p.id == step.step_id);
                    if !confidence_ok
                        || holder_now.as_deref() != Some(txn.holder.as_str())
                        || !active
                    {
                        step.status = "superseded";
                        txn.cursor += 1;
                        continue;
                    }
                    let step_id = step.step_id.clone();
                    let txn_id = txn.id.clone();
                    let device = step.device_id.clone();
                    let target = txn.holder.clone();
                    let write = json!({
                        "txn_id": txn_id,
                        "step_id": step_id,
                        "mixed_revision": self
                            .config
                            .as_ref()
                            .and_then(|c| c.switch_group.as_ref())
                            .and_then(|g| g.mixed.as_ref())
                            .map(|m| m.revision),
                        "target_device_id": target,
                        "writes": [{"fingerprint": step.fingerprint, "value": step.target_value}],
                    });
                    step.status = "sent";
                    match self.lan_request(
                        &device,
                        "display.push",
                        write,
                        PUSH_BASE_TIMEOUT + PUSH_PER_DISPLAY,
                        Some(PendingKind::DisplayPush {
                            txn: txn.id.clone(),
                            step_id: step.step_id.clone(),
                        }),
                    ) {
                        Ok(_) => {
                            txn.phase_c_deadline =
                                Some(Instant::now() + PUSH_BASE_TIMEOUT + PUSH_PER_DISPLAY);
                        }
                        Err(_) => step.status = "failed",
                    }
                    self.lan_runtime.mixed_txn = Some(txn);
                    return;
                }
                "sent" => {
                    if let Some(deadline) = txn.phase_c_deadline {
                        if Instant::now() > deadline {
                            step.status = "timed_out";
                            txn.cursor += 1;
                            txn.phase_c_deadline = None;
                            continue;
                        }
                    }
                    self.lan_runtime.mixed_txn = Some(txn);
                    return;
                }
                _ => {
                    txn.cursor += 1;
                }
            }
        }
    }

    fn txn_finish(&mut self, txn: &MixedTransaction) {
        let steps: Vec<Value> = txn
            .pushes
            .iter()
            .map(|s| {
                json!({
                    "step_id": s.step_id,
                    "device_id": s.device_id,
                    "fingerprint": s.fingerprint,
                    "status": s.status,
                })
            })
            .collect();
        let all = steps.iter().all(|s| s["status"] == "acked");
        self.notify(
            "lan.sync",
            json!({
                "status": "mixed_close_result",
                "outcome": if all {"ok"} else {"partial"},
                "holder": txn.holder,
                "reason": txn.finished,
                "steps": steps,
            }),
        );
        self.emit_event(
            "lan.mixed.close",
            if all { Level::Info } else { Level::Warn },
            json!({"holder": txn.holder, "reason": txn.finished, "steps": steps.len()}),
        );
    }

    pub(crate) fn poll_lan_commands(&mut self) {
        // 1. Expire tracked requests.
        let now = Instant::now();
        let expired: Vec<u64> = self
            .lan_runtime
            .pending
            .iter()
            .filter(|(_, p)| now > p.deadline)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            if let Some(pending) = self.lan_runtime.pending.remove(&id) {
                match &pending.kind {
                    PendingKind::DisplayPush { txn, step_id } => {
                        if let Some(transaction) = self.lan_runtime.mixed_txn.as_mut() {
                            if &transaction.id == txn {
                                if let Some(step) = transaction
                                    .pushes
                                    .iter_mut()
                                    .find(|s| &s.step_id == step_id)
                                {
                                    if step.status == "sent" {
                                        step.status = "timed_out";
                                    }
                                }
                            }
                        }
                    }
                    PendingKind::Snapshot => {}
                }
            }
        }
        // 2. Advance / start the close transaction.
        self.txn_advance();
        self.txn_start();
    }

    /// Frontend request surface (`lan.*`).
    pub(crate) fn handle_lan_request(&mut self, id: u64, method: &str, params: Value) {
        match method {
            "lan.status" | "lan.peers" => {
                self.respond(ResponseLine::ok(id, self.lan_state()));
            }
            "lan.discover.start" | "lan.discover.stop" => {
                if let Some(handle) = self.lan_runtime.handle.as_ref() {
                    let _ = handle.reannounce();
                }
                self.respond(ResponseLine::ok(id, self.lan_state()));
            }
            "lan.connect.manual" => {
                let result = (|| -> Result<Value, CoreError> {
                    let addr: SocketAddr = params
                        .get("addr")
                        .and_then(Value::as_str)
                        .and_then(|s| s.parse().ok())
                        .ok_or_else(|| core_err("E_CONFIG_INVALID", "addr must be host:port"))?;
                    let handle = self
                        .lan_runtime
                        .handle
                        .as_ref()
                        .ok_or_else(|| core_err("E_LAN_DISABLED", "LAN disabled"))?;
                    handle
                        .connect_manual(addr)
                        .map_err(|e| core_err(e.code, e.message))?;
                    Ok(json!({"connecting": addr.to_string()}))
                })();
                match result {
                    Ok(v) => self.respond(ResponseLine::ok(id, v)),
                    Err(e) => self.respond(ResponseLine::err(id, e.code, e.message)),
                }
            }
            "lan.pair.start" => {
                let result = (|| -> Result<Value, CoreError> {
                    if self.lan_runtime.pair.is_some() {
                        return Err(core_err("E_LAN_BUSY", "a pairing is already active"));
                    }
                    let addr: SocketAddr = params
                        .get("addr")
                        .and_then(Value::as_str)
                        .and_then(|s| s.parse().ok())
                        .ok_or_else(|| core_err("E_CONFIG_INVALID", "addr must be host:port"))?;
                    let handle = self
                        .lan_runtime
                        .handle
                        .as_ref()
                        .ok_or_else(|| core_err("E_LAN_DISABLED", "LAN disabled"))?;
                    handle.pair(addr).map_err(|e| core_err(e.code, e.message))?;
                    // The user is on a pairing screen: accept inbound
                    // attempts from the other machine too (both ends can
                    // press 连接).
                    handle.set_pairing_window(true);
                    // Mint the session identity now so the RPC reply and every
                    // later notification of this attempt carry the same id.
                    let session_id = Runtime::new_pair_session_id();
                    self.lan_runtime.pair_session_id = Some(session_id.clone());
                    self.lan_runtime.pair_event_seq = 0;
                    self.lan_runtime.pair_local_confirmed = false;
                    self.lan_runtime.pair_peer_confirmed = false;
                    self.lan_runtime.pair_pending_session = Some(session_id.clone());
                    Ok(json!({
                        "phase": "connecting", "addr": addr.to_string(),
                        "session_id": session_id, "event_seq": 0,
                        "local_confirmed": false, "peer_confirmed": false,
                    }))
                })();
                match result {
                    Ok(v) => self.respond(ResponseLine::ok(id, v)),
                    Err(e) => self.respond(ResponseLine::err(id, e.code, e.message)),
                }
            }
            "lan.pair.confirm" => {
                let ok_value = params
                    .get("sas_ok")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let result = (|| -> Result<Value, CoreError> {
                    let Some(pair) = self.lan_runtime.pair.as_mut() else {
                        return Err(core_err("E_LAN_BUSY", "no active pairing"));
                    };
                    if !ok_value {
                        let _ = pair
                            .link
                            .send("pair.abort", json!({"code": "E_LAN_PAIR_REJECTED"}));
                        pair.link.close();
                        self.lan_runtime.pair = None;
                        if let Some(handle) = self.lan_runtime.handle.as_ref() {
                            handle.set_pairing_window(false);
                        }
                        return Ok(json!({"phase": "cancelled"}));
                    }
                    pair.local_confirmed = true;
                    self.lan_runtime.pair_local_confirmed = true;
                    // The phase reflects what the UI should show: waiting
                    // for the peer's press, or already past it (peer
                    // confirmed first — N1) and negotiating.
                    let mut send_side = None;
                    if pair.peer_confirmed {
                        pair.phase = super::pairing::Phase::Negotiating;
                        if !pair.side_sent {
                            pair.side_sent = true;
                            send_side =
                                Some(serde_json::to_value(&pair.side).unwrap_or(Value::Null));
                        }
                    } else {
                        pair.phase = super::pairing::Phase::AwaitPeerConfirm;
                    }
                    let _ = pair.link.send("pair.confirm", json!({}));
                    if let Some(side) = send_side {
                        let _ = pair.link.send("pair.hello_pair", json!({"side": side}));
                    }
                    let phase = if pair.peer_confirmed {
                        "negotiating"
                    } else {
                        "await_peer_confirm"
                    };
                    Ok(json!({"phase": phase, "sas": pair.link.sas}))
                })();
                match result {
                    Ok(v) => {
                        let meta = self.pair_next_meta();
                        let mut note = v.clone();
                        if let Some(pair) = self.lan_runtime.pair.as_ref() {
                            if let Some(obj) = note.as_object_mut() {
                                obj.insert("device_id".into(), json!(pair.link.device_id));
                                obj.insert("initiator".into(), json!(pair.link.initiator));
                                obj.insert("sas".into(), json!(pair.link.sas));
                            }
                        }
                        self.pair_notify_with(note, &meta);
                        let mut reply = v;
                        if let (Some(dst), Some(meta)) = (reply.as_object_mut(), meta.as_object()) {
                            for (k, val) in meta {
                                dst.insert(k.clone(), val.clone());
                            }
                        }
                        self.respond(ResponseLine::ok(id, reply));
                        self.pair_maybe_plan();
                    }
                    Err(e) => self.respond(ResponseLine::err(id, e.code, e.message)),
                }
            }
            "lan.pair.resolve" => {
                let result = (|| -> Result<Value, CoreError> {
                    let (link_initiator, options_now, peer_side) = self
                        .lan_runtime
                        .pair
                        .as_ref()
                        .map(|p| {
                            (
                                p.link.initiator,
                                p.options.clone(),
                                p.peer_side.clone().unwrap_or_else(|| p.side.clone()),
                            )
                        })
                        .ok_or_else(|| core_err("E_LAN_BUSY", "no active pairing"))?;
                    if !link_initiator {
                        return Err(core_err(
                            "E_NOT_OWNER",
                            "choices are resolved on the initiating computer",
                        ));
                    }
                    let mut options = options_now
                        .ok_or_else(|| core_err("E_LAN_BUSY", "plan not computed yet"))?;
                    if let Some(aliases) = params.get("monitor_aliases").and_then(Value::as_object)
                    {
                        for (local, group) in aliases {
                            match group {
                                Value::String(group) => {
                                    options.monitor_aliases.insert(local.clone(), group.clone());
                                }
                                // Explicit rejection: "different physical
                                // displays". The pair stays unmatched (a
                                // blocking issue) instead of guessing an
                                // alias; never send `null`/`false` as a map
                                // target.
                                Value::Bool(false) | Value::Null => {
                                    options.monitor_alias_rejections.insert(local.clone());
                                }
                                _ => {
                                    return Err(core_err(
                                        "E_CONFIG_INVALID",
                                        "alias target must be a string or false",
                                    ))
                                }
                            }
                        }
                    }
                    if let Some(choice) = params.get("mixed").and_then(Value::as_str) {
                        options.mixed = Some(match choice {
                            "basis" => kvmflow_core::pairing::MixedChoice::Basis,
                            "joiner" => kvmflow_core::pairing::MixedChoice::Joiner,
                            other => {
                                return Err(core_err(
                                    "E_CONFIG_INVALID",
                                    format!("unknown mixed choice {other}"),
                                ))
                            }
                        });
                    }
                    if let Some(replacement) = params.get("replacement").and_then(Value::as_object)
                    {
                        options.replacement = Some(kvmflow_core::pairing::ReplacementChoice {
                            old_device_id: replacement
                                .get("old_device_id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            replace: replacement
                                .get("replace")
                                .and_then(Value::as_bool)
                                .unwrap_or(true),
                        });
                    }
                    // Recompute on the initiator and re-send the identical
                    // options; the responder recomputes and commits on digest
                    // agreement when the plan becomes complete.
                    let online = peer_side.online_device_ids.clone();
                    self.lan_pair_compute(options, online, false);
                    let current = self
                        .lan_runtime
                        .pair
                        .as_ref()
                        .ok_or_else(|| core_err("E_INTERNAL", "pairing vanished"))?;
                    let _ = current.link.send(
                        "pair.plan",
                        json!({
                            "options": serde_json::to_value(current.options.clone()).unwrap_or(Value::Null),
                            "online_device_ids": current
                                .peer_side
                                .as_ref()
                                .map(|s| s.online_device_ids.clone())
                                .unwrap_or_default(),
                        }),
                    );
                    let (issues, ready) = self.pair_issue_view();
                    Ok(json!({
                        "phase": if ready { "ready_to_commit" } else { "await_resolve" },
                        "issues": issues,
                        "proposed_snapshot": self.pair_preview(),
                    }))
                })();
                match result {
                    Ok(v) => {
                        let meta = self.pair_next_meta();
                        let mut note = v.clone();
                        if let Some(pair) = self.lan_runtime.pair.as_ref() {
                            if let Some(obj) = note.as_object_mut() {
                                obj.insert("device_id".into(), json!(pair.link.device_id));
                                obj.insert("initiator".into(), json!(pair.link.initiator));
                                obj.insert("sas".into(), json!(pair.link.sas));
                            }
                        }
                        self.pair_notify_with(note, &meta);
                        let mut reply = v;
                        if let (Some(dst), Some(meta)) = (reply.as_object_mut(), meta.as_object()) {
                            for (k, val) in meta {
                                dst.insert(k.clone(), val.clone());
                            }
                        }
                        self.respond(ResponseLine::ok(id, reply));
                        self.pair_try_auto_commit();
                    }
                    Err(e) => self.respond(ResponseLine::err(id, e.code, e.message)),
                }
            }
            "lan.pair.window" => {
                let open = params.get("open").and_then(Value::as_bool).unwrap_or(true);
                if let Some(handle) = self.lan_runtime.handle.as_ref() {
                    handle.set_pairing_window(open);
                }
                self.respond(ResponseLine::ok(id, json!({"open": open})));
            }
            "lan.pair.cancel" => {
                let device_id = self
                    .lan_runtime
                    .pair
                    .as_ref()
                    .map(|pair| pair.link.device_id.clone());
                if let Some(pair) = self.lan_runtime.pair.take() {
                    let _ = pair
                        .link
                        .send("pair.abort", json!({"code": "E_LAN_PAIR_CANCELLED"}));
                    pair.link.close();
                }
                // No pairing screen is active any more: close admission even
                // when there was no live session left to take.
                if let Some(handle) = self.lan_runtime.handle.as_ref() {
                    handle.set_pairing_window(false);
                }
                let meta = self.pair_next_meta();
                let mut note = json!({"phase": "cancelled"});
                if let Some(device_id) = device_id {
                    note["device_id"] = json!(device_id);
                }
                self.pair_notify_with(note, &meta);
                let mut reply = json!({"phase": "cancelled"});
                if let (Some(dst), Some(meta)) = (reply.as_object_mut(), meta.as_object()) {
                    for (k, val) in meta {
                        dst.insert(k.clone(), val.clone());
                    }
                }
                self.respond(ResponseLine::ok(id, reply));
            }
            "lan.unpair" => {
                let result = (|| -> Result<Value, CoreError> {
                    let target = params
                        .get("device_id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| core_err("E_CONFIG_INVALID", "device_id required"))?
                        .to_string();
                    let mut cfg = self
                        .config
                        .clone()
                        .ok_or_else(|| core_err("E_CONFIG_INVALID", "no configuration"))?;
                    let group = cfg
                        .switch_group
                        .as_mut()
                        .ok_or_else(|| core_err("E_CONFIG_INVALID", "no switch group"))?;
                    let now = now_ms();
                    let mut next = group.clone();
                    {
                        let mut sync = next.group_sync();
                        if let Some(member) = sync
                            .members
                            .iter_mut()
                            .find(|m| m.device_id == target && !m.removed)
                        {
                            member.removed = true;
                            member.revision += 1;
                            member.meta = kvmflow_core::sync::SectionMeta {
                                updated_at_ms: now,
                                updated_by: cfg.local_device.device_id.clone(),
                            };
                        }
                        next.set_group_sync(sync);
                    }
                    let (stamped, _) = kvmflow_core::sync::stamp_local_edit(
                        group,
                        &next,
                        &cfg.local_device.device_id,
                        now,
                    )
                    .map_err(|e| core_err(e.code, e.message))?;
                    *group = stamped;
                    self.lan_save(cfg)?;
                    if let Some(handle) = self.lan_runtime.handle.as_ref() {
                        let _ = handle.disconnect(&target);
                    }
                    self.lan_runtime.sync_states.remove(&target);
                    Ok(json!({"removed": target}))
                })();
                match result {
                    Ok(v) => {
                        self.notify(
                            "lan.sync",
                            json!({"status":"synced","unpaired": v["removed"]}),
                        );
                        self.respond(ResponseLine::ok(id, v));
                    }
                    Err(e) => self.respond(ResponseLine::err(id, e.code, e.message)),
                }
            }
            "lan.setup.secondary" => {
                let result = (|| -> Result<Value, CoreError> {
                    // First run on a fresh secondary has no config: create
                    // the minimal safe one (stable identity + label only, no
                    // switch group) so pairing can proceed and the flag is
                    // durable across restarts.
                    let mut created = false;
                    let mut cfg = match self.config.clone() {
                        Some(cfg) => cfg,
                        None => {
                            let device_id =
                                config_store::load_or_create_device_id(&self.config_path)
                                    .map_err(|e| core_err("E_CONFIG_IO", e.to_string()))?;
                            created = true;
                            // Enumerate the local monitors now: the profile
                            // reports them with pending inputs (source
                            // Unknown) until each is read while the monitor
                            // actually shows this host (§6.1 step 4).
                            let monitors = self
                                .backend
                                .list_displays()
                                .unwrap_or_default()
                                .into_iter()
                                .map(|d| kvmflow_core::config::LocalMonitorConfig {
                                    fingerprint: d.edid_id,
                                    label: d.model_name,
                                    local_input: 0,
                                    source: kvmflow_core::config::InputSource::Unknown,
                                    legacy_remote_preset: None,
                                })
                                .collect();
                            MultiDeviceConfig {
                                lan: None,
                                setup_secondary: true,
                                schema_version: kvmflow_core::config::MULTI_DEVICE_SCHEMA_VERSION,
                                local_device: kvmflow_core::config::LocalDeviceProfile {
                                    device_id,
                                    host_label: params
                                        .get("host_label")
                                        .and_then(Value::as_str)
                                        .unwrap_or("KVMFlow")
                                        .to_string(),
                                    monitors,
                                },
                                switch_group: None,
                                trigger: Default::default(),
                                advanced: Default::default(),
                            }
                        }
                    };
                    if !cfg.setup_secondary {
                        cfg.setup_secondary = true;
                        created = true; // needs persisting
                    }
                    if created || cfg.setup_secondary {
                        if let Err(e) = cfg.validate() {
                            return Err(core_err(e.code, e.message));
                        }
                        config_store::write_atomically(
                            &self.config_path,
                            config_store::persisted_config_text(&cfg).as_bytes(),
                        )
                        .map_err(|e| core_err("E_CONFIG_IO", e.to_string()))?;
                        self.config = Some(cfg.clone());
                        self.apply_config(cfg);
                    }
                    self.lan_runtime.secondary = true;
                    // A waiting secondary accepts the primary's inbound
                    // pairing (its pairing screen IS the waiting screen).
                    if let Some(handle) = self.lan_runtime.handle.as_ref() {
                        handle.set_pairing_window(true);
                    }
                    // LAN may have been skipped at startup because no config
                    // existed; start it now that an identity is available.
                    if self.lan_runtime.handle.is_none() && self.lan_runtime.lan_wanted {
                        let wanted = self.lan_runtime.lan_wanted;
                        self.start_lan(Some(RuntimeLanOptions {
                            enabled: wanted,
                            ..RuntimeLanOptions::default()
                        }));
                    }
                    Ok(json!({"secondary": true, "phase": "waiting_for_primary"}))
                })();
                match result {
                    Ok(v) => {
                        self.notify("lan.setup", v.clone());
                        self.respond(ResponseLine::ok(id, v));
                    }
                    Err(e) => self.respond(ResponseLine::err(id, e.code, e.message)),
                }
            }
            "lan.profile.refresh" => {
                self.lan_runtime.next_profile_read = 0;
                // Re-send the owner report to every online peer: the UI calls
                // this when its profile page (re)opens, and it is the
                // deterministic recovery for a report lost on a replaced
                // session.
                for peer in self.lan_runtime.online.clone() {
                    self.lan_send_profile(&peer);
                }
                self.respond(ResponseLine::ok(id, json!({"scheduled": true})));
            }
            "lan.test.start" => {
                let duration = params
                    .get("duration_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(30_000)
                    .min(120_000);
                let test_id = Runtime::new_pair_session_id();
                let started = now_ms();
                self.lan_runtime.test = Some(super::test_evidence::TestEvidence::start(
                    test_id.clone(),
                    started,
                    duration,
                ));
                for peer in self.lan_runtime.online.clone() {
                    let _ = self.lan_send(
                        &peer,
                        crate::lan::messages::Envelope::new(
                            "test.begin",
                            json!({"test_id": test_id, "duration_ms": duration}),
                        ),
                    );
                }
                self.respond(ResponseLine::ok(
                    id,
                    json!({
                        "test_id": test_id,
                        "duration_ms": duration,
                        "ends_ms": started + duration,
                    }),
                ));
            }
            "lan.test.stop" => {
                let test_id = self
                    .lan_runtime
                    .test
                    .as_ref()
                    .map(|test| test.test_id.clone());
                let summary = self
                    .lan_runtime
                    .test
                    .take()
                    .map(|test| test.summary())
                    .unwrap_or(json!({"test_id": null, "events": []}));
                for peer in self.lan_runtime.online.clone() {
                    let _ = self.lan_send(
                        &peer,
                        crate::lan::messages::Envelope::new(
                            "test.end",
                            json!({"test_id": test_id}),
                        ),
                    );
                }
                self.respond(ResponseLine::ok(id, summary));
            }
            _ => self.respond(ResponseLine::err(id, E_UNKNOWN_METHOD, method)),
        }
    }
}

impl Runtime {
    /// After both confirms and a resolved plan, the initiator opens the
    /// two-phase commit handshake: commit_request → (responder verifies,
    /// answers commit_ready) → initiator commits → commit_go → responder
    /// commits → committed. Neither side commits without the other's
    /// verified digest, and no side commits before the local user confirmed.
    pub(crate) fn pair_try_auto_commit(&mut self) {
        let Some(pair) = self.lan_runtime.pair.as_ref() else {
            return;
        };
        if !pair.link.initiator {
            return;
        }
        if !pair.local_confirmed || !pair.peer_confirmed {
            return;
        }
        // Full merge when the plan validates; pin-only when only blockers
        // remain (both are committable states).
        let committable = pair
            .plan
            .as_ref()
            .is_some_and(|plan| plan.snapshot.is_some() || !plan.needs_choice());
        if !committable {
            return;
        }
        let Some(digest) = Runtime::pair_commit_digest(pair) else {
            return;
        };
        let pin_only = pair
            .plan
            .as_ref()
            .is_some_and(|plan| plan.snapshot.is_none());
        let _ = pair.link.send(
            "pair.commit_request",
            json!({"plan_digest": digest, "pin_only": pin_only}),
        );
        if let Some(pair) = self.lan_runtime.pair.as_mut() {
            pair.phase = super::pairing::Phase::Committing;
        }
    }
}
