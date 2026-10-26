//! S6 pairing flow state machine. All decisions happen on the runtime worker;
//! the quarantined PairLink only carries `pair.*`/`profile.report` frames.
//!
//! Commit model (two-phase, replacing the previous fire-and-forget commit):
//!
//! 1. SAS commit-reveal completes; both ends show the number. NOTHING about
//!    the group (members, names, monitors, inputs) crosses the wire until
//!    BOTH users confirmed, so an unconfirmed endpoint leaks nothing.
//! 2. After both confirms, the initiator generates the one `PairOptions`
//!    (`group_random`, `now_ms`), sends `pair.plan`, and both ends recompute
//!    `plan_pair` with identical inputs; the responder echoes the options
//!    plus its plan digest so the initiator can verify byte-for-byte
//!    agreement. Any plan computation, display or commit step requires
//!    local_confirmed && peer_confirmed — messages arriving before the local
//!    user confirmed fail the pairing outright.
//! 3. Two-phase commit: the initiator sends `pair.commit_request` with the
//!    plan digest; the responder verifies the digest and answers
//!    `pair.commit_ready` (WITHOUT committing); the initiator commits
//!    locally, then sends `pair.commit_go`; the responder commits and
//!    answers `pair.committed`. Only then do both ends drop the transaction
//!    record, so a network cut leaves a recoverable record on whichever end
//!    had not yet committed, reconciled at startup against the peer.
//!
//! Cancellation/rejection/timeout abort both ends and change nothing.
//! A blocked-but-resolved plan (duplicate inputs) uses the same handshake
//! with `pin_only: true`: only member keys are pinned, configurations stay.
use super::*;
use kvmflow_core::pairing::{PairOptions, PairPlan, PairSide};

pub(crate) enum Phase {
    /// SAS shown, waiting for the local user's `lan.pair.confirm`.
    /// No group data is sent or accepted in this phase.
    AwaitLocalConfirm,
    /// Local user confirmed; waiting for the peer's confirmation. The side
    /// exchange happens once the peer confirms too.
    AwaitPeerConfirm,
    /// Both confirmed; sides exchanged; waiting for plan/resolve/commit.
    Negotiating,
    /// Commit in progress (we sent/awaited commit handshake messages).
    Committing,
}
pub(crate) struct Pairing {
    pub link: Arc<crate::lan::pair_link::PairLink>,
    pub phase: Phase,
    /// Our own side inputs (recomputed locally, never trusted from the wire).
    pub side: PairSide,
    /// Peer's claimed side, authenticated only by the pairing channel key.
    /// Locked once received (first frame wins; later frames are rejected).
    pub peer_side: Option<PairSide>,
    /// Latest computed plan; recomputed with resolutions.
    pub plan: Option<PairPlan>,
    pub options: Option<PairOptions>,
    /// Set once the local user confirmed the SAS value.
    pub local_confirmed: bool,
    pub peer_confirmed: bool,
    /// Our own side has been sent exactly once.
    pub side_sent: bool,
    /// The peer's plan digest from its pair.plan echo (initiator side).
    pub peer_plan_digest: Option<String>,
    /// Commit handshake state for the two-phase protocol.
    pub commit_confirmed_by_peer: bool,
}

impl Runtime {
    /// Mint a fresh pairing session identity. The same id is used for this
    /// attempt's notifications and RPC replies; a retry gets a new one.
    pub(crate) fn new_pair_session_id() -> String {
        let mut bytes = [0u8; 16];
        if getrandom::fill(&mut bytes).is_err() {
            return format!("p{:x}", now_ms());
        }
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Meta fields every `lan.pair` payload carries (requirement: the UI can
    /// reject late and duplicate events from an older session).
    pub(crate) fn pair_meta_current(&self) -> Value {
        json!({
            "session_id": self.lan_runtime.pair_session_id,
            "event_seq": self.lan_runtime.pair_event_seq,
            "local_confirmed": self.lan_runtime.pair_local_confirmed,
            "peer_confirmed": self.lan_runtime.pair_peer_confirmed,
        })
    }

    /// Allocate the next event sequence for a pairing notification.
    pub(crate) fn pair_next_meta(&mut self) -> Value {
        self.lan_runtime.pair_event_seq = self.lan_runtime.pair_event_seq.saturating_add(1);
        self.pair_meta_current()
    }

    /// Emit a pairing notification with the given (already allocated) meta.
    pub(crate) fn pair_notify_with(&mut self, mut data: Value, meta: &Value) {
        if let (Some(dst), Some(meta)) = (data.as_object_mut(), meta.as_object()) {
            for (k, v) in meta {
                dst.insert(k.clone(), v.clone());
            }
        }
        self.notify("lan.pair", data);
    }

    /// The pairing screen is over: stop accepting inbound attempts. Called
    /// at every terminal commit path, not only on failure/cancel.
    fn close_inbound_window(&self) {
        if let Some(handle) = self.lan_runtime.handle.as_ref() {
            handle.set_pairing_window(false);
        }
    }

    /// Emit a pairing notification, allocating the next sequence. Works after
    /// the live Pairing has been taken (failure/peer_committed paths) because
    /// the session identity and flags live on the runtime.
    pub(crate) fn pair_emit(&mut self, data: Value) {
        let meta = self.pair_next_meta();
        self.pair_notify_with(data, &meta);
    }

    /// Emit a reconciliation event for a surviving transaction record. It
    /// carries the RECORD's session id, never the live session's: a retry may
    /// already be pairing with someone else, and the UI must not treat the
    /// old record as part of the new attempt. A legacy record without an id
    /// emits an explicit null (the UI then keeps it out of a live session).
    pub(crate) fn pair_emit_record(&mut self, record: Option<&Value>, data: Value) {
        let mut meta = self.pair_next_meta();
        if let Some(record) = record {
            meta["session_id"] = record.get("session_id").cloned().unwrap_or(Value::Null);
        }
        self.pair_notify_with(data, &meta);
    }

    fn pair_notify(&mut self, phase: &str, extra: Value) {
        let Some(pair) = self.lan_runtime.pair.as_ref() else {
            return;
        };
        let mut data = json!({
            "phase": phase,
            "device_id": pair.link.device_id,
            "initiator": pair.link.initiator,
            "sas": pair.link.sas,
            // Authenticated hello name (when the peer supplied one). The UI
            // must prefer it over the mDNS-discovered label.
            "peer_name": pair.link.peer_name,
        });
        if let (Some(dst), Some(map)) = (data.as_object_mut(), extra.as_object()) {
            for (k, v) in map {
                dst.insert(k.clone(), v.clone());
            }
        }
        self.pair_emit(data);
    }

    /// An inbound or dialed pairing session completed the commit-reveal SAS.
    /// No configuration data is exchanged here: only the SAS is on screen.
    pub(super) fn strip_builtin_profile(&mut self, cfg: &mut MultiDeviceConfig) {
        let builtin_ids: BTreeSet<String> = self
            .mixed_runtime
            .displays
            .iter()
            .filter(|d| d.builtin)
            .map(|d| d.fingerprint.clone())
            .chain(
                self.backend
                    .list_displays()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|d| d.builtin)
                    .map(|d| d.edid_id),
            )
            .collect();
        cfg.local_device
            .monitors
            .retain(|m| !builtin_ids.contains(&m.fingerprint));
        if let Some(group) = cfg.switch_group.as_mut() {
            if let Some(owner) = group
                .devices
                .iter_mut()
                .find(|d| d.device_id == cfg.local_device.device_id)
            {
                owner
                    .monitors
                    .retain(|m| !builtin_ids.contains(m.backend_fingerprint()));
            }
            for layout in &mut group.layouts {
                layout
                    .monitors
                    .retain(|m| !builtin_ids.contains(&m.fingerprint));
            }
            if let Some(mixed) = group.mixed.as_mut() {
                mixed
                    .assignment
                    .retain(|m| !builtin_ids.contains(&m.fingerprint));
            }
        }
    }

    pub(crate) fn lan_pair_ready(&mut self, link: Arc<crate::lan::pair_link::PairLink>) {
        if self.lan_runtime.pair.is_some() {
            link.close();
            return;
        }
        // A secondary without a config can still pair (its minimal config is
        // created by lan.setup.secondary); a primary with no config at all
        // cannot offer a side.
        let Some(mut cfg) = self.config.clone() else {
            link.close();
            return;
        };
        self.strip_builtin_profile(&mut cfg);
        self.config = Some(cfg.clone());
        let side = PairSide::from_config(&cfg, std::env::consts::OS, &self.lan_runtime.public_key);
        // Outbound attempts claim the id minted by `lan.pair.start`; inbound
        // attempts mint their own. Either way this pairing flow starts a new
        // session identity and sequence.
        let claimed = if link.initiator {
            self.lan_runtime.pair_pending_session.take()
        } else {
            None
        };
        let session_id = claimed.unwrap_or_else(Self::new_pair_session_id);
        self.lan_runtime.pair_session_id = Some(session_id);
        self.lan_runtime.pair_event_seq = 0;
        self.lan_runtime.pair_local_confirmed = false;
        self.lan_runtime.pair_peer_confirmed = false;
        self.lan_runtime.pair = Some(Pairing {
            link,
            phase: Phase::AwaitLocalConfirm,
            side,
            peer_side: None,
            plan: None,
            options: None,
            local_confirmed: false,
            peer_confirmed: false,
            side_sent: false,
            peer_plan_digest: None,
            commit_confirmed_by_peer: false,
        });
        self.pair_notify("await_confirm", json!({}));
    }

    pub(crate) fn pair_fail(&mut self, code: &str, message: &str) {
        if let Some(pair) = self.lan_runtime.pair.take() {
            let _ = pair.link.send("pair.abort", json!({"code": code}));
            pair.link.close();
            // Pairing screen over: stop accepting inbound attempts.
            if let Some(handle) = self.lan_runtime.handle.as_ref() {
                handle.set_pairing_window(false);
            }
            self.pair_emit(
                json!({"phase":"failed","code":code,"message":message,"device_id":pair.link.device_id}),
            );
            self.lan_issue(code, message);
        }
    }

    /// Drain quarantined pairing messages. Runs on the worker thread.
    /// Messages already delivered by the reader are processed BEFORE the
    /// closed check: a peer may commit and close in quick succession, and its
    /// final handshake frames must not be lost to the EOF.
    pub(crate) fn poll_pair(&mut self) {
        let mut messages = Vec::new();
        if let Some(pair) = self.lan_runtime.pair.as_ref() {
            for _ in 0..64 {
                match pair.link.try_message() {
                    Some(m) => messages.push(m),
                    None => break,
                }
            }
        }
        for message in messages {
            self.lan_pair_message(message);
        }
        if let Some(pair) = self.lan_runtime.pair.as_ref() {
            if pair.link.closed() && pair.link.try_message().is_none() {
                if pair.commit_confirmed_by_peer {
                    // Handshake complete; the session ended cleanly.
                    self.lan_runtime.pair = None;
                } else {
                    let id = pair.link.device_id.clone();
                    self.pair_fail("E_LAN_CLOSED", &format!("pairing session with {id} closed"));
                }
            }
        }
    }

    fn lan_pair_message(&mut self, message: crate::lan::messages::Envelope) {
        match message.t.as_str() {
            "pair.confirm" => self.lan_pair_peer_confirm(),
            "pair.hello_pair" => self.lan_pair_peer_side(&message.body),
            "pair.plan" => self.lan_pair_peer_plan(&message.body),
            "pair.plan.reject" => {
                let reason = message
                    .body
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("rejected")
                    .to_string();
                self.pair_fail("E_LAN_PAIR_REJECTED", &reason);
            }
            "pair.commit_request" => self.lan_pair_commit_request(&message.body),
            "pair.commit_ready" => self.lan_pair_commit_ready(&message.body),
            "pair.commit_go" => self.lan_pair_commit_go(&message.body),
            "pair.committed" => self.lan_pair_peer_committed(&message.body),
            "pair.abort" => {
                let code = message
                    .body
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or("E_LAN_PAIR_ABORTED")
                    .to_string();
                self.pair_fail(&code, "peer aborted pairing");
            }
            _ => {}
        }
    }

    /// Every protocol step beyond the SAS confirmation requires the LOCAL
    /// user to have confirmed. Frames arriving before that are an attack
    /// surface (the peer tries to push us toward commit) and abort the
    /// pairing.
    fn pair_require_local_confirmed(&mut self, what: &str) -> bool {
        let Some(pair) = self.lan_runtime.pair.as_ref() else {
            return false;
        };
        if !pair.local_confirmed {
            self.pair_fail(
                "E_LAN_PAIR_NOT_CONFIRMED",
                &format!("peer sent {what} before the local user confirmed"),
            );
            return false;
        }
        true
    }

    fn lan_pair_peer_confirm(&mut self) {
        // The peer's confirmation is accepted ANYTIME (the two users press
        // the button independently; the peer confirming first is normal,
        // not an attack — N1). Only group-data and commit frames require
        // the local confirmation.
        let mut maybe_side = None;
        if let Some(pair) = self.lan_runtime.pair.as_mut() {
            pair.peer_confirmed = true;
            self.lan_runtime.pair_peer_confirmed = true;
            // Both confirmed: exchange sides exactly once at THIS moment,
            // whichever end confirmed last. When the local user confirmed
            // first, the local confirm already sent our side.
            if pair.local_confirmed && !pair.side_sent {
                pair.side_sent = true;
                maybe_side = Some(serde_json::to_value(&pair.side).unwrap_or(Value::Null));
            }
        }
        if let (Some(pair), Some(side)) = (self.lan_runtime.pair.as_ref(), maybe_side) {
            let _ = pair.link.send("pair.hello_pair", json!({"side": side}));
        }
        self.pair_notify("peer_confirmed", json!({}));
        self.pair_maybe_plan();
    }

    fn lan_pair_peer_side(&mut self, body: &Value) {
        if !self.pair_require_local_confirmed("pair.hello_pair") {
            return;
        }
        let Some(pair) = self.lan_runtime.pair.as_ref() else {
            return;
        };
        if !pair.peer_confirmed {
            self.pair_fail("E_LAN_PROTO", "side data arrived before the peer confirmed");
            return;
        }
        if pair.peer_side.is_some() {
            // Locked after the first frame: a later replacement is tampering.
            self.pair_fail("E_LAN_PROTO", "peer side already locked");
            return;
        }
        let Some(peer): Option<PairSide> =
            serde_json::from_value(body.get("side").cloned().unwrap_or(Value::Null)).ok()
        else {
            self.pair_fail("E_LAN_PROTO", "invalid pair.hello_pair");
            return;
        };
        // The claimed identity must match the authenticated pairing channel.
        let key_ok = crate::lan::trust::public_key_string(&pair.link.public_key) == peer.public_key
            && pair.link.device_id == peer.device_id;
        if !key_ok || peer.device_id.is_empty() {
            self.pair_fail("E_LAN_IDENTITY", "pairing identity mismatch");
            return;
        }
        // Send our own side now if not already sent (both ends trigger on
        // their own peer-confirmed transition; this is the inbound-order
        // fallback) — exactly once either way.
        let mut send_side = None;
        if let Some(pair) = self.lan_runtime.pair.as_mut() {
            pair.peer_side = Some(peer);
            if !pair.side_sent {
                pair.side_sent = true;
                send_side = Some(serde_json::to_value(&pair.side).unwrap_or(Value::Null));
            }
        }
        if let (Some(pair), Some(side)) = (self.lan_runtime.pair.as_ref(), send_side) {
            let _ = pair.link.send("pair.hello_pair", json!({"side": side}));
        }
        self.pair_maybe_plan();
    }

    pub(crate) fn pair_maybe_plan(&mut self) {
        let Some((link, local_confirmed, peer_confirmed, side_device_id, peer_side_ready)) =
            self.lan_runtime.pair.as_ref().map(|p| {
                (
                    p.link.clone(),
                    p.local_confirmed,
                    p.peer_confirmed,
                    p.side.device_id.clone(),
                    p.peer_side.is_some(),
                )
            })
        else {
            return;
        };
        if !local_confirmed || !peer_confirmed || !peer_side_ready {
            return;
        }
        // After the commit handshake started, recomputing is meaningless
        // (trust pins changed by the commit itself would make plan_pair
        // reject the established flag) and must not abort the session.
        if self
            .lan_runtime
            .pair
            .as_ref()
            .is_some_and(|p| matches!(p.phase, Phase::Committing))
            || self
                .lan_runtime
                .pair
                .as_ref()
                .is_some_and(|p| p.commit_confirmed_by_peer)
        {
            return;
        }
        if link.initiator {
            // One random token and one timestamp, generated once here.
            let mut group_random = [0u8; 16];
            if getrandom::fill(&mut group_random).is_err() {
                self.pair_fail("E_INTERNAL", "random source unavailable");
                return;
            }
            let online = self
                .lan_runtime
                .online
                .iter()
                .cloned()
                .chain([link.device_id.clone()])
                .collect();
            let options = PairOptions {
                initiator_device_id: side_device_id.clone(),
                group_random,
                now_ms: now_ms(),
                monitor_aliases: Default::default(),
                monitor_alias_rejections: Default::default(),
                mixed: None,
                replacement: None,
            };
            self.lan_pair_compute(options, online, true);
            let Some(current) = self.lan_runtime.pair.as_ref() else {
                return;
            };
            let _ = current.link.send(
                "pair.plan",
                json!({
                    "options": serde_json::to_value(current.options.clone()).unwrap_or(Value::Null),
                    "online_device_ids": current.peer_side.as_ref().map(|s| s.online_device_ids.clone()).unwrap_or_default(),
                }),
            );
            // A plan with no outstanding choices commits without a resolve
            // round-trip; blocked plans wait for the user's input.
            self.pair_try_auto_commit();
        }
        // Non-initiators compute when the plan options arrive (pair.plan).
    }

    pub(crate) fn lan_pair_compute(
        &mut self,
        options: PairOptions,
        online: BTreeSet<String>,
        notify: bool,
    ) {
        let Some(current) = self.lan_runtime.pair.as_mut() else {
            return;
        };
        // Both sides must carry identical reachability inputs.
        current.side.online_device_ids = online.clone();
        if let Some(peer) = current.peer_side.as_mut() {
            peer.online_device_ids = online;
        }
        let (a, b) = (
            current.side.clone(),
            match current.peer_side.clone() {
                Some(peer) => peer,
                None => return,
            },
        );
        // `established` must be recomputed against our live member list.
        let established_peer = self
            .lan_runtime
            .members
            .read()
            .unwrap()
            .iter()
            .any(|m| !m.removed && m.device_id != current.side.device_id);
        let mut local = a;
        local.established = established_peer;
        let plan = match kvmflow_core::pairing::plan_pair(&local, &b, &options) {
            Ok(plan) => plan,
            Err(e) => {
                let _ = current.link.send(
                    "pair.plan.reject",
                    json!({"reason": format!("{}: {}", e.code, e.message)}),
                );
                self.pair_fail(e.code, &e.message);
                return;
            }
        };
        current.plan = Some(plan);
        current.options = Some(options);
        if notify {
            // All choices resolved: commit proceeds (full merge when the plan
            // validates, key pinning when a blocker still exists).
            let snapshot_ready = self
                .lan_runtime
                .pair
                .as_ref()
                .and_then(|p| p.plan.as_ref())
                .is_some_and(|plan| plan.snapshot.is_some() || !plan.needs_choice());
            let (issues, _) = self.pair_issue_view();
            let phase = if snapshot_ready {
                "ready_to_commit"
            } else {
                "await_resolve"
            };
            self.pair_notify(
                phase,
                json!({"issues": issues, "proposed_snapshot": self.pair_preview()}),
            );
        }
    }

    pub(crate) fn pair_issue_view(&self) -> (Value, bool) {
        let Some(pair) = self.lan_runtime.pair.as_ref() else {
            return (json!([]), false);
        };
        let Some(plan) = pair.plan.as_ref() else {
            return (json!([]), false);
        };
        let issues: Vec<Value> = plan
            .issues
            .iter()
            .map(|issue| serde_json::to_value(issue).unwrap_or(Value::Null))
            .collect();
        // Committable = full merge, or choices done with only blockers left
        // (key pinning proceeds; the configuration merge waits).
        let committable = plan.snapshot.is_some() || !plan.needs_choice();
        (json!(issues), committable)
    }

    pub(crate) fn pair_preview(&self) -> Value {
        self.lan_runtime
            .pair
            .as_ref()
            .and_then(|p| p.plan.as_ref())
            .and_then(|plan| plan.proposed_snapshot.as_ref())
            .and_then(|s| {
                let mut v = serde_json::to_value(s).ok()?;
                if let Some(obj) = v.as_object_mut() {
                    obj.remove("sync");
                }
                Some(v)
            })
            .unwrap_or(Value::Null)
    }

    /// The digest identifying what a commit would land: the plan's snapshot
    /// for a full merge, or its member list for pin-only commits.
    pub(crate) fn pair_commit_digest(pair: &Pairing) -> Option<String> {
        let plan = pair.plan.as_ref()?;
        if let Some(snapshot) = plan.snapshot.as_ref() {
            Some(kvmflow_core::sync::SyncSummary::from_snapshot(snapshot).digest)
        } else {
            Some(kvmflow_core::sync::digest(&plan.members))
        }
    }

    fn lan_pair_peer_plan(&mut self, body: &Value) {
        if !self.pair_require_local_confirmed("pair.plan") {
            return;
        }
        // A late plan echo after the commit handshake started is redundant.
        if self
            .lan_runtime
            .pair
            .as_ref()
            .is_some_and(|p| matches!(p.phase, Phase::Committing))
        {
            return;
        }
        let Some(options): Option<PairOptions> =
            serde_json::from_value(body.get("options").cloned().unwrap_or(Value::Null)).ok()
        else {
            self.pair_fail("E_LAN_PROTO", "invalid pair.plan");
            return;
        };
        // PairOptions must be bound to the authenticated initiator of THIS
        // pairing session (LAN_CORE.md): the initiator id in the options is
        // the remote end of an inbound session, or ourselves on the dialer.
        let Some(pair) = self.lan_runtime.pair.as_ref() else {
            return;
        };
        let expected_initiator = if pair.link.initiator {
            pair.side.device_id.clone()
        } else {
            pair.link.device_id.clone()
        };
        if options.initiator_device_id != expected_initiator {
            self.pair_fail(
                "E_LAN_PROTO",
                "pair options are not bound to this session's initiator",
            );
            return;
        }
        let online: BTreeSet<String> =
            serde_json::from_value(body.get("online_device_ids").cloned().unwrap_or(json!([])))
                .unwrap_or_default();
        let peer_plan_digest = body
            .get("plan_digest")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.lan_pair_compute(options, online, true);
        // Two-sided recomputation agreement (plan §1.4): when the initiator
        // receives the responder's echoed options plus its plan digest, the
        // independently recomputed plans must match byte for byte.
        let is_initiator = self
            .lan_runtime
            .pair
            .as_ref()
            .is_some_and(|p| p.link.initiator);
        if is_initiator {
            if let Some(expected) = peer_plan_digest {
                let local = self
                    .lan_runtime
                    .pair
                    .as_ref()
                    .and_then(|p| p.plan.as_ref())
                    .and_then(|plan| plan.proposed_snapshot.as_ref())
                    .map(|s| kvmflow_core::sync::digest(s))
                    .unwrap_or_default();
                if local != expected {
                    self.pair_fail(
                        "E_LAN_PROTO",
                        "pair plans diverged; both apps must be on the same version",
                    );
                    return;
                }
                if let Some(pair) = self.lan_runtime.pair.as_mut() {
                    pair.peer_plan_digest = Some(expected);
                }
            }
            // The echo itself may complete the agreement; if the peer is
            // already waiting for a commit, its commit handshake follows.
            return;
        }
        // Only the responder echoes the identical options back once; the
        // initiator receiving its own echoed options recomputes (agreement
        // check) but never re-sends, which would ping-pong forever.
        let Some(current) = self.lan_runtime.pair.as_ref() else {
            return;
        };
        let plan_digest = current
            .plan
            .as_ref()
            .and_then(|plan| plan.proposed_snapshot.as_ref())
            .map(kvmflow_core::sync::digest)
            .unwrap_or_default();
        let _ = current.link.send(
            "pair.plan",
            json!({
                "options": serde_json::to_value(current.options.clone()).unwrap_or(Value::Null),
                "online_device_ids": current.peer_side.as_ref().map(|s| s.online_device_ids.clone()).unwrap_or_default(),
                "plan_digest": plan_digest,
            }),
        );
    }

    // ---- Two-phase commit handshake -------------------------------------

    /// Initiator → responder: I want to commit this plan (digest given).
    /// The responder verifies against its own recomputation and answers
    /// commit_ready WITHOUT committing.
    fn lan_pair_commit_request(&mut self, body: &Value) {
        if !self.pair_require_local_confirmed("pair.commit_request") {
            return;
        }
        let Some(pair) = self.lan_runtime.pair.as_ref() else {
            return;
        };
        let pin_only = body.get("pin_only").and_then(Value::as_bool) == Some(true);
        let expected = body.get("plan_digest").and_then(Value::as_str);
        let local_digest = Self::pair_commit_digest(pair);
        match (expected, local_digest) {
            (Some(expected), Some(local)) if expected == local => {
                let _ = pair.link.send(
                    "pair.commit_ready",
                    json!({"plan_digest": local, "pin_only": pin_only}),
                );
                if let Some(pair) = self.lan_runtime.pair.as_mut() {
                    pair.phase = Phase::Committing;
                }
            }
            _ => self.pair_fail(
                "E_LAN_PROTO",
                "pair plans diverged; update both apps and pair again",
            ),
        }
    }

    /// Responder → initiator: my recomputation agrees, go ahead. The
    /// initiator commits NOW and, on local success, sends commit_go.
    fn lan_pair_commit_ready(&mut self, body: &Value) {
        if !self.pair_require_local_confirmed("pair.commit_ready") {
            return;
        }
        let Some(pair) = self.lan_runtime.pair.as_ref() else {
            return;
        };
        if !pair.link.initiator {
            self.pair_fail("E_LAN_PROTO", "commit_ready as non-initiator");
            return;
        }
        let expected = body.get("plan_digest").and_then(Value::as_str);
        let local_digest = Self::pair_commit_digest(pair);
        match (expected, local_digest) {
            (Some(expected), Some(local)) if expected == local => {
                if let Err(e) = self.lan_pair_commit() {
                    self.pair_fail(e.code, &e.message);
                    return;
                }
                // Committed locally; tell the responder to commit too. The
                // initiator KEEPS the session open until the responder's
                // pair.committed arrives (N3): closing earlier lost the
                // confirmation and forced the sync-path reconciliation to
                // guess. The transaction record on disk covers a cut inside
                // this window; the session deadline bounds the wait.
                let Some(current) = self.lan_runtime.pair.as_ref() else {
                    return;
                };
                current.link.release_gate();
                let _ = current
                    .link
                    .send("pair.commit_go", json!({"plan_digest": local}));
                // The pairing screen's work is done locally. Close admission
                // now as well: the responder's pair.committed may be lost,
                // and an open window after commit is never wanted (N7).
                self.close_inbound_window();
                self.pair_emit(json!({
                    "phase": "committed",
                    "device_id": current.link.device_id,
                    "config_merged": true,
                    "peer_confirmed_commit": false,
                }));
            }
            _ => self.pair_fail(
                "E_LAN_PROTO",
                "pair plans diverged; update both apps and pair again",
            ),
        }
    }

    /// Initiator → responder: I committed; you may commit the same digest.
    /// This is the ONLY path where the responder commits.
    fn lan_pair_commit_go(&mut self, body: &Value) {
        if !self.pair_require_local_confirmed("pair.commit_go") {
            return;
        }
        let Some(pair) = self.lan_runtime.pair.as_ref() else {
            return;
        };
        if pair.link.initiator {
            self.pair_fail("E_LAN_PROTO", "commit_go as initiator");
            return;
        }
        let expected = body.get("plan_digest").and_then(Value::as_str);
        let local_digest = Self::pair_commit_digest(pair);
        match (expected, local_digest) {
            (Some(expected), Some(local)) if expected == local => {
                if let Err(e) = self.lan_pair_commit() {
                    self.pair_fail(e.code, &e.message);
                    return;
                }
                // The local commit landed; admission control can accept the
                // next pairing while this session finishes its handshake.
                if let Some(current) = self.lan_runtime.pair.as_ref() {
                    current.link.release_gate();
                }
                let digest = local;
                let Some(current) = self.lan_runtime.pair.as_ref() else {
                    return;
                };
                let _ = current
                    .link
                    .send("pair.committed", json!({"plan_digest": digest}));
                // Both ends are durable now (we committed and confirmed the
                // initiator's digest); drop our record and admission before
                // closing. The pairing screen must not keep accepting dials.
                self.close_inbound_window();
                let record_path = self.config_path.with_file_name("pairing.pending.json");
                let _ = std::fs::remove_file(&record_path);
                if let Some(pair) = self.lan_runtime.pair.as_mut() {
                    pair.commit_confirmed_by_peer = true;
                }
                if let Some(pair) = self.lan_runtime.pair.take() {
                    pair.link.close();
                }
            }
            _ => self.pair_fail(
                "E_LAN_PROTO",
                "pair plans diverged; update both apps and pair again",
            ),
        }
    }

    /// Responder → initiator: I committed too (fast path while the link is
    /// still draining). The initiator's session usually already ended after
    /// commit_go; the record is then reconciled through the sync path.
    fn lan_pair_peer_committed(&mut self, body: &Value) {
        let Some(pair) = self.lan_runtime.pair.take() else {
            // Session already ended by our commit: reconcile the record from
            // the digest the peer just sent, if it matches.
            let expected = body.get("plan_digest").and_then(Value::as_str);
            self.confirm_pairing_record(expected);
            return;
        };
        if !pair.local_confirmed {
            self.lan_runtime.pair = Some(pair);
            self.pair_fail(
                "E_LAN_PAIR_NOT_CONFIRMED",
                "peer sent pair.committed before the local user confirmed",
            );
            return;
        }
        if !pair.link.initiator {
            self.lan_runtime.pair = Some(pair);
            self.pair_fail("E_LAN_PROTO", "committed as non-initiator");
            return;
        }
        let expected = body.get("plan_digest").and_then(Value::as_str);
        let local_digest = Self::pair_commit_digest(&pair);
        if let (Some(expected), Some(local)) = (expected, local_digest) {
            if expected == local {
                let record_path = self.config_path.with_file_name("pairing.pending.json");
                let _ = std::fs::remove_file(&record_path);
                // The initiator's commit is confirmed by the responder: the
                // pairing is fully durable on both ends, so stop accepting
                // inbound pairing attempts.
                self.close_inbound_window();
                self.pair_emit(
                    json!({"phase": "peer_committed", "device_id": pair.link.device_id}),
                );
                pair.link.close();
                return;
            }
        }
        self.lan_runtime.pair = Some(pair);
        self.pair_fail(
            "E_LAN_PROTO",
            "peer committed a different plan; update both apps and pair again",
        );
    }

    /// The pairing peer confirmed its commit out-of-band (or we reconciled a
    /// matching snapshot): drop the pending record and tell the UI the pair
    /// is durable on both ends.
    fn confirm_pairing_record(&mut self, _peer_digest: Option<&str>) {
        let record_path = self.config_path.with_file_name("pairing.pending.json");
        if record_path.exists() {
            let record = self.read_pairing_record();
            let _ = std::fs::remove_file(&record_path);
            self.close_inbound_window();
            self.pair_emit_record(record.as_ref(), json!({"phase": "peer_committed"}));
        }
    }

    /// Commit the plan: backup, persist transaction record, apply, ack.
    /// The record is only removed when the peer's commitment is confirmed
    /// (`pair.committed` on the initiator, send of `pair.committed` on the
    /// responder) or reconciled at startup.
    pub(crate) fn lan_pair_commit(&mut self) -> Result<(), CoreError> {
        let Some(mut pair) = self.lan_runtime.pair.take() else {
            return Err(CoreError::new(E_INTERNAL, "no pairing to commit"));
        };
        let result = self.commit_pair_plan(&mut pair);
        if result.is_err() {
            if let Some(record) = self.read_pairing_record() {
                if record.get("session_id").and_then(Value::as_str)
                    == self.lan_runtime.pair_session_id.as_deref()
                    && !self.record_members_match(&record)
                {
                    // This attempt never committed locally. A stale transaction
                    // must not revoke the peer when the user retries.
                    self.clear_pairing_record();
                }
            }
        }
        // Keep the session until the caller publishes success or failure.
        // In particular a validation/backup/write error must reach pair_fail.
        self.lan_runtime.pair = Some(pair);
        result
    }

    fn commit_pair_plan(&mut self, pair: &mut Pairing) -> Result<(), CoreError> {
        // The local user must have confirmed before anything touches disk.
        if !pair.local_confirmed || !pair.peer_confirmed {
            return Err(CoreError::new(
                "E_LAN_PAIR_NOT_CONFIRMED",
                "commit attempted before both users confirmed",
            ));
        }
        let Some(plan) = pair.plan.clone() else {
            return Err(CoreError::new(E_INTERNAL, "no computed pair plan"));
        };
        // Blocked topology: persist trust plus the SAS-confirmed plan;
        // owner input reports resume it without another numeric check.
        if plan.snapshot.is_none() {
            if plan.needs_choice() {
                return Err(CoreError::new(
                    E_CONFIG_INVALID,
                    "plan has unresolved choices",
                ));
            }
            return self.lan_pair_pin_only(pair, &plan);
        }
        let Some(snapshot) = plan.snapshot.clone() else {
            return Err(CoreError::new(
                E_CONFIG_INVALID,
                "plan has unresolved issues",
            ));
        };
        let Some(mut cfg) = self.config.clone() else {
            return Err(CoreError::new(E_CONFIG_INVALID, "no local config"));
        };
        let mut checked = cfg.clone();
        checked.switch_group = Some(snapshot.clone());
        checked.validate()?;
        // 1. Backup before replacing anything (both ends do this locally).
        let backup = self.config_path.with_file_name(format!(
            "{}.pre-pair-{}",
            self.config_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
            now_ms()
        ));
        if let Ok(original) = std::fs::read(&self.config_path) {
            config_store::write_atomically(&backup, &original)
                .map_err(|e| CoreError::new(E_CONFIG_IO, e.to_string()))?;
        }
        // 2. Recoverable transaction record BEFORE the config write. It
        // survives until the peer's commitment is confirmed so a cut between
        // the two commits is detectable and reconcilable.
        let record = json!({
            "attempt": format!("{:032x}", u128::from_be_bytes(pair.link.attempt[..16].try_into().unwrap())),
            "session_id": self.lan_runtime.pair_session_id,
            "peer_device_id": pair.link.device_id,
            "peer_addr": pair.link.peer_addr.to_string(),
            "initiator": pair.link.initiator,
            "snapshot": snapshot,
            "members": plan.members,
        });
        let record_path = self.config_path.with_file_name("pairing.pending.json");
        config_store::write_atomically(
            &record_path,
            serde_json::to_vec(&record).unwrap_or_default().as_slice(),
        )
        .map_err(|e| CoreError::new(E_CONFIG_IO, e.to_string()))?;
        // 3. Apply: adopt the merged snapshot, clear consumed presets, keep
        // local-only trigger/advanced/lan fields.
        for monitor in &mut cfg.local_device.monitors {
            monitor.legacy_remote_preset = None;
        }
        cfg.switch_group = Some(snapshot);
        let text = config_store::persisted_config_text(&cfg);
        config_store::write_atomically(&self.config_path, text.as_bytes())
            .map_err(|e| CoreError::new(E_CONFIG_IO, e.to_string()))?;
        self.config = Some(cfg.clone());
        self.apply_config(cfg);
        self.lan_config_changed();
        // 4. Publish pins. The transaction record survives on disk; it is
        // confirmed (deleted) by the peer's pair.committed arriving while the
        // link drains, or by the sync-path/startup reconciliation. The
        // session itself is returned to the caller, which decides whether to
        // keep it briefly (responder sends committed) or close it now
        // (initiator proceeds from the record).
        self.publish_lan_members();
        let notification = json!({
            "phase": "committed",
            "device_id": pair.link.device_id,
            "config_merged": true,
            "peer_confirmed_commit": false,
            "renames": plan.renames,
        });
        self.pair_emit(notification);
        Ok(())
    }

    /// Pin member keys after both numeric confirmations while configuration
    /// blockers remain (plan §1.4: "只完成配对（固定公钥），不合并配置").
    /// Persist trust and the confirmed plan so owner profiles can finish setup.
    fn lan_pair_pin_only(&mut self, pair: &mut Pairing, plan: &PairPlan) -> Result<(), CoreError> {
        let Some(cfg) = self.config.clone() else {
            return Err(CoreError::new(E_CONFIG_INVALID, "no local config"));
        };
        // A fresh secondary has no switch group yet: the pin-only commit
        // creates a trust-only group (members pinned, no devices). The
        // topology fills in when the deferred owner profiles arrive.
        let mut group = match cfg.switch_group.clone() {
            Some(group) => group,
            None => {
                let proposed = plan.proposed_snapshot.as_ref();
                SwitchGroupSnapshot {
                    group_id: proposed
                        .map(|s| s.group_id.clone())
                        .unwrap_or_else(|| "grp-pending".into()),
                    revision: proposed.map(|s| s.revision).unwrap_or(0),
                    devices: Vec::new(),
                    layouts: Vec::new(),
                    mixed: proposed.and_then(|s| s.mixed.clone()),
                    sync: serde_json::Value::Null,
                }
            }
        };
        let mut sync = group.group_sync();
        for member in &plan.members {
            match sync
                .members
                .iter_mut()
                .find(|m| m.device_id == member.device_id)
            {
                Some(existing) => {
                    existing.public_key = member.public_key.clone();
                    existing.removed = false;
                    existing.revision = existing.revision.max(member.revision);
                    existing.meta = member.meta.clone();
                }
                None => sync.members.push(member.clone()),
            }
        }
        sync.members.sort_by(|a, b| a.device_id.cmp(&b.device_id));
        sync.ports_confirmed = false;
        sync.extra.insert(
            "setup_pending".into(),
            json!({"side":pair.side,"peer_side":pair.peer_side,"options":pair.options}),
        );
        group.set_group_sync(sync);
        let mut updated = cfg.clone();
        updated.switch_group = Some(group);
        // N5: the persisted result must VALIDATE — never write a config the
        // loader would reject. When the surrounding configuration carries a
        // pre-existing defect (a duplicate input is exactly the blocker that
        // routed the plan here), the pin is kept IN MEMORY ONLY: the pairing
        // still commits (keys work over the session), and the pin reaches
        // disk with the next healthy save.
        match updated.validate() {
            Ok(()) => {
                config_store::write_atomically(
                    &self.config_path,
                    config_store::persisted_config_text(&updated).as_bytes(),
                )
                .map_err(|e| CoreError::new(E_CONFIG_IO, e.to_string()))?;
                self.config = Some(updated);
            }
            Err(error) => {
                self.emit_event(
                    "lan.pair.pin_only_unvalidated",
                    Level::Warn,
                    json!({"error": error.to_string()}),
                );
                self.lan_issue("E_LAN_SETUP_PENDING", &error.to_string());
                self.config = Some(updated);
            }
        }
        self.publish_lan_members();
        let notification = json!({
            "phase": "committed",
            "device_id": pair.link.device_id,
            "config_merged": false,
            "peer_confirmed_commit": pair.commit_confirmed_by_peer,
            "renames": plan.renames,
        });
        self.pair_emit(notification);
        Ok(())
    }
}
