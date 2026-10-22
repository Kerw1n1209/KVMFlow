//! Protocol request dispatch for the embedded runtime and JSONL server.

use super::*;

impl Runtime {
    pub(crate) fn handle_request(&mut self, req: kvmflow_core::protocol::RequestLine) -> bool {
        let id = req.id;
        if self.released_topology.is_some()
            && !matches!(
                req.method.as_str(),
                "hello"
                    | "state.get"
                    | "config.get"
                    | "backend.status"
                    | "protocol.describe"
                    | "shutdown"
            )
        {
            self.respond(ResponseLine::err(
                id,
                E_BACKEND,
                "displays released for exit/update; resume before changing runtime state",
            ));
            return true;
        }
        if req.method.starts_with("lan.") {
            self.handle_lan_request(id, &req.method, req.params);
            return true;
        }
        match req.method.as_str() {
            "hello" => {
                let result = json!({
                    "sidecar_version": kvmflow_core::SIDECAR_VERSION,
                    "protocol_version": PROTOCOL_VERSION,
                    "platform": std::env::consts::OS,
                    "backend": self.backend.name(),
                    "simulated": self.backend.simulated(),
                    "pid": std::process::id(),
                });
                self.respond(ResponseLine::ok(id, result));
            }
            "protocol.describe" => {
                let result = json!({
                    "protocol_version": PROTOCOL_VERSION,
                    "requests": REQUEST_METHODS,
                    "notifications": NOTIFICATION_KINDS,
                });
                self.respond(ResponseLine::ok(id, result));
            }
            "backend.status" => {
                let result = json!({
                    "backend": self.backend.name(),
                    "simulated": self.backend.simulated(),
                    "platform": std::env::consts::OS,
                    "configPath": self.config_path.to_string_lossy(),
                    "logPath": self.log.path().to_string_lossy(),
                    "groupPresent": self.tracker.as_ref().map(|t| t.group_present()).unwrap_or(false),
                    "configMode": self.config_mode(),
                });
                self.respond(ResponseLine::ok(id, result));
            }
            "config.get" => {
                self.respond(ResponseLine::ok(id, self.active_config_value()));
            }
            "config.set" => {
                let mut cfg_value = req.params.get("config").cloned().unwrap_or(Value::Null);
                // Internal convergence registers are never an external settings
                // surface. Reject injection and restore live values on UI saves.
                if let Some(sync) = cfg_value
                    .pointer("/switch_group/sync")
                    .and_then(Value::as_object)
                {
                    if sync.contains_key("profile_cache") || sync.contains_key("archived_arrivals")
                    {
                        self.respond(ResponseLine::err(
                            id,
                            kvmflow_core::errors::E_NOT_OWNER,
                            "sync caches are internal fields",
                        ));
                        return true;
                    }
                }
                if let Some(current) = self.config.as_ref().and_then(|c| c.switch_group.as_ref()) {
                    if cfg_value
                        .pointer("/switch_group/group_id")
                        .and_then(Value::as_str)
                        == Some(&current.group_id)
                    {
                        if let Some(group) = cfg_value
                            .get_mut("switch_group")
                            .and_then(Value::as_object_mut)
                        {
                            // A settings save never edits LAN trust or internal
                            // convergence registers: restore members and caches
                            // from the live snapshot when the caller omitted them.
                            let live = current.group_sync();
                            let sync = group.entry("sync").or_insert_with(|| json!({}));
                            if !sync.is_object() {
                                *sync = json!({});
                            }
                            if let Some(sync) = sync.as_object_mut() {
                                for key in ["profile_cache", "archived_arrivals"] {
                                    if let Some(saved) = current.sync.get(key) {
                                        sync.insert(key.into(), saved.clone());
                                    }
                                }
                                // Trust is never edited through settings:
                                // ALWAYS overwrite with the live pins, even
                                // when the supplied id list matches — a save
                                // with unchanged ids could otherwise swap a
                                // public key or lift a tombstone (P2-3).
                                sync.insert(
                                    "members".into(),
                                    serde_json::to_value(&live.members).unwrap_or(json!([])),
                                );
                            }
                        }
                    }
                }
                // Generic settings writes never own the mixed section. Merge
                // before deserialization so absence cannot trigger migration.
                if let Some(current) = self
                    .config
                    .as_ref()
                    .and_then(|c| c.switch_group.as_ref())
                    .and_then(|g| g.mixed.as_ref())
                {
                    if let Some(group) = cfg_value
                        .get_mut("switch_group")
                        .and_then(Value::as_object_mut)
                    {
                        group.insert("mixed".into(), serde_json::to_value(current).unwrap());
                    }
                } else if let Some(group) = cfg_value
                    .get_mut("switch_group")
                    .and_then(Value::as_object_mut)
                {
                    // Bootstrap also uses migration defaults. A settings save
                    // cannot arm mixed mode or import a caller-chosen revision.
                    group.remove("mixed");
                }
                match ConfigDocument::from_value(cfg_value) {
                    Ok(ConfigDocument::LegacyV1(cfg)) => {
                        if let Err(error) = cfg.validate() {
                            self.respond(ResponseLine::err(
                                id,
                                E_CONFIG_INVALID,
                                error.to_string(),
                            ));
                            return true;
                        }
                        let (LegacyMigration::Runnable(config)
                        | LegacyMigration::NeedsSetup(config)) =
                            MultiDeviceConfig::migrate_legacy(cfg);
                        self.persist_and_apply_config(id, config);
                    }
                    Ok(ConfigDocument::MultiDeviceV2(cfg)) => {
                        if let Err(error) = cfg.validate() {
                            self.respond(ResponseLine::err(
                                id,
                                E_CONFIG_INVALID,
                                error.to_string(),
                            ));
                            return true;
                        }
                        self.persist_and_apply_config(id, cfg);
                    }
                    Err(error) => {
                        self.respond(ResponseLine::err(id, E_CONFIG_INVALID, error.to_string()))
                    }
                }
            }
            "display.list" => {
                // KVM-7 remediation: this path used to leave no session event,
                // so "the wizard showed N displays" was only answerable by
                // the operator. Log the outcome (and Windows drop reasons).
                let list_result = self.backend.list_displays();
                // Drain AFTER list_displays: on Windows that call refreshes
                // the cache and records why any attached monitor was dropped.
                #[cfg(target_os = "windows")]
                let dropped = backends::windows::ddc::take_enumeration_diag();
                #[cfg(not(target_os = "windows"))]
                let dropped: Vec<String> = Vec::new();
                let has_drops = !dropped.is_empty();
                match list_result {
                    Ok(mut displays) => {
                        // One read-only VCP probe per display to fill ddc
                        // capability (wizard step 1 requirement).
                        for d in displays.iter_mut() {
                            if d.builtin {
                                d.ddc = backends::DdcCapability::Unavailable {
                                    reason: "builtin_display_has_no_ddc".into(),
                                };
                                continue;
                            }
                            match self.backend.read_input(&d.edid_id) {
                                Ok(r) => {
                                    d.ddc = backends::DdcCapability::Available { input: r.value }
                                }
                                Err(e) => {
                                    d.ddc = backends::DdcCapability::Unavailable {
                                        reason: e.to_string(),
                                    }
                                }
                            }
                        }
                        let summary = json!({
                            "count": displays.len(),
                            "displays": displays.iter().map(|d| json!({
                                "edid_id": d.edid_id,
                                "model": d.model_name,
                                "builtin": d.builtin,
                                "ddc": d.ddc,
                            })).collect::<Vec<_>>(),
                            "dropped": dropped,
                        });
                        let level = if displays.is_empty() || has_drops {
                            Level::Warn
                        } else {
                            Level::Info
                        };
                        self.emit_event(kinds::DISPLAY_LIST, level, summary);
                        self.respond(ResponseLine::ok(id, json!({ "displays": displays })));
                    }
                    Err(e) => {
                        self.emit_event(kinds::DISPLAY_LIST, Level::Error, json!({ "error": e }));
                        self.respond(ResponseLine::err(id, E_BACKEND, e));
                    }
                }
            }
            "display.readInput" => {
                let edid = req
                    .params
                    .get("edid_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let before = now_ms();
                match self.backend.read_input(edid) {
                    Ok(r) => {
                        self.emit_event(
                            kinds::DDC_GET,
                            Level::Info,
                            json!({"edid_id": edid, "value": r.value, "max": r.max, "elapsed_ms": now_ms() - before}),
                        );
                        self.respond(ResponseLine::ok(
                            id,
                            serde_json::to_value(r).unwrap_or(Value::Null),
                        ));
                    }
                    Err(e) => {
                        self.emit_event(
                            kinds::DDC_GET,
                            Level::Warn,
                            json!({"edid_id": edid, "error": e.to_string()}),
                        );
                        self.respond(ResponseLine::err(id, e.code, e.message));
                    }
                }
            }
            "display.writeInput" => {
                let edid = req
                    .params
                    .get("edid_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let value = req
                    .params
                    .get("value")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u16;
                let reason = req
                    .params
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("manual")
                    .to_string();
                let policy = self.active_retry_policy();
                let outcome = self.write_with_retry(&edid, value, &policy, true);
                self.emit_event(
                    kinds::DDC_SET,
                    if outcome.commanded {
                        Level::Info
                    } else {
                        Level::Error
                    },
                    json!({
                        "edid_id": edid, "value": value, "reason": reason,
                        "commanded": outcome.commanded, "attempts": outcome.attempts,
                        "note": REPORT_NOTE,
                    }),
                );
                self.respond(ResponseLine::ok(
                    id,
                    serde_json::to_value(&outcome).unwrap_or(Value::Null),
                ));
            }
            "usb.snapshot" => {
                self.respond(ResponseLine::ok(
                    id,
                    json!({ "devices": self.last_snapshot, "count": self.last_snapshot.len() }),
                ));
            }
            "usb.watch.start" => match self.active_trigger() {
                Some(t) if !t.anchor.vid_pid.is_empty() => {
                    let mut tracker = GroupTracker::new(t.anchor, t.members, t.debounce);
                    let keys: Vec<DeviceKey> = self
                        .last_snapshot
                        .iter()
                        .map(|d| DeviceKey {
                            vid_pid: d.vid_pid.clone(),
                            serial: d.serial.clone(),
                        })
                        .collect();
                    tracker.update(now_ms(), &keys);
                    let debounce = tracker.params().clone();
                    self.tracker = Some(tracker);
                    self.respond(ResponseLine::ok(
                        id,
                        json!({"armed": true, "debounce": debounce}),
                    ));
                }
                _ => {
                    self.respond(ResponseLine::err(
                        id,
                        E_TRIGGER_NOT_ARMED,
                        "trigger profile not configured (config.trigger.anchor is empty)",
                    ));
                }
            },
            "usb.watch.stop" => {
                self.tracker = None;
                self.respond(ResponseLine::ok(id, json!({"armed": false})));
            }
            "app.setEnabled" => {
                let enabled = req
                    .params
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                self.set_runtime_enabled(enabled);
                self.respond(ResponseLine::ok(
                    id,
                    json!({"state": self.fsm.state(), "enabled": enabled}),
                ));
            }
            "wizard.begin" => {
                self.wizard_active = true;
                self.learning_baseline = Some(self.last_snapshot.clone());
                self.last_candidates = None;
                let actions = self.fsm.handle(Input::WizardBegin, now_ms());
                self.run_actions(actions);
                self.respond(ResponseLine::ok(
                    id,
                    json!({"baseline": self.last_snapshot.len(), "hint": "press the USB switch now to move the group away; zero events = check the uplink cable (power-only cable incident, KVM-1 §7-3)"}),
                ));
            }
            "wizard.end" => {
                self.wizard_active = false;
                self.learning_baseline = None;
                let actions = self.fsm.handle(Input::WizardEnd, now_ms());
                self.run_actions(actions);
                self.respond(ResponseLine::ok(id, json!({"state": self.fsm.state()})));
            }
            "wizard.candidates" => {
                let c = self.last_candidates.clone().unwrap_or(Value::Null);
                self.respond(ResponseLine::ok(id, c));
            }
            "mixed.set" | "mixed.assign" => {
                self.handle_mixed_request(id, &req.method, &req.params);
                // A local mixed edit is a section change: broadcast the new
                // snapshot so peers align (S8). The reply was already sent;
                // this only fans out over LAN.
                self.lan_config_changed();
            }
            "state.get" => {
                let result = json!({
                    "state": self.fsm.state(),
                    "lan": self.lan_state(),
                    "mixed": self.mixed_state(),
                    "groupPresent": self.tracker.as_ref().map(|t| t.group_present()).unwrap_or(false),
                    "groupStable": self.tracker.as_ref().map(|t| t.group_stable()).unwrap_or(false),
                    "lastMultiDeviceReport": self.last_multi_device_report,
                    "lastLayoutApplication": self.last_layout_application,
                    "effectiveLayouts": self.effective_layouts_value(),
                    "monitors": self.active_monitor_count(),
                    "configMode": self.config_mode(),
                    "groupReady": self.config.as_ref().map(|c| c.ready_for_group_switch()).unwrap_or(false),
                    "enabled": self.enabled,
                    "configRecovery": self.config_recovery,
                });
                self.respond(ResponseLine::ok(id, result));
            }
            "diagnostics.collect" => {
                let tail = req
                    .params
                    .get("tail")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(200) as usize;
                let events: Vec<EventRecord> = self.log.tail(tail);
                let result = json!({
                    "logPath": self.log.path().to_string_lossy(),
                    "configPath": self.config_path.to_string_lossy(),
                    "sidecar_version": kvmflow_core::SIDECAR_VERSION,
                    "protocol_version": PROTOCOL_VERSION,
                    "platform": std::env::consts::OS,
                    "backend": self.backend.name(),
                    "simulated": self.backend.simulated(),
                    "config": self.active_config_value(),
                    "lan": self.lan_state(),
                    "config_mode": self.config_mode(),
                    "events": events,
                });
                self.respond(ResponseLine::ok(id, result));
            }
            "shutdown" => {
                self.respond(ResponseLine::ok(id, json!({"bye": true})));
                return false;
            }
            _ => {
                self.respond(ResponseLine::err(id, E_UNKNOWN_METHOD, req.method));
            }
        }
        true
    }
}
