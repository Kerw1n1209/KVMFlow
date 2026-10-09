//! Shared runtime for the Tauri desktop host and the standalone JSONL server.
//! Hardware and state transitions execute on one worker, never the UI thread.

mod backends;
mod session;

use backends::{Backend, UsbDevice};
use kvmflow_core::config::{Config, ConfigDocument, MultiDeviceConfig, TriggerConfig};
use kvmflow_core::debounce::{DeviceKey, GroupTracker, TrackerEvent};
use kvmflow_core::errors::*;
use kvmflow_core::events::kinds;
use kvmflow_core::events::{EventRecord, Level};
use kvmflow_core::protocol::{
    encode, parse_client_line, NotificationLine, ResponseLine, NOTIFICATION_KINDS,
    PROTOCOL_VERSION, REQUEST_METHODS,
};
use kvmflow_core::statemachine::{Action, Input, MonitorOutcome, SwitchFsm};
use kvmflow_core::transaction::local_arrival_correction;
use serde_json::{json, Value};
use session::{now_ms, SessionLog};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::Duration;

enum Msg {
    Line(String),
    Request(kvmflow_core::protocol::RequestLine, Sender<ResponseLine>),
    Usb(Vec<UsbDevice>),
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    HubProbe(Vec<(String, String)>),
    Shutdown,
}

/// Some monitors (G52plus) drop the link on inactive inputs. When the target
/// host re-links slower than the monitor's no-signal timeout, the monitor
/// auto-selects the source host again and the target stays black. The
/// source re-sends the target input once; a second bounce is left alone so a
/// monitor that never accepts the target cannot loop.
///
/// The other host switching back looks the same from here: it pushes every
/// monitor to this host's input, and USB arrival is seen ~1s later. So a
/// bounce only counts when it persists across consecutive checks while at
/// least one other watched monitor has not come back; every monitor back at
/// once is a hand-back and ends the watch. Single-monitor watches therefore
/// never re-send.
const BOUNCE_FIRST_CHECK_MS: u64 = 1_500;
const BOUNCE_CHECK_INTERVAL_MS: u64 = 1_000;
const BOUNCE_WATCH_MS: u64 = 12_000;
const BOUNCE_PROBE_TIMEOUT_MS: u64 = 750;
const BOUNCE_CONFIRM_CHECKS: u8 = 2;

struct BounceWatch {
    started_ms: u64,
    next_check_ms: u64,
    monitors: Vec<BounceWatchMonitor>,
}

struct BounceWatchMonitor {
    fingerprint: String,
    target_input: u16,
    local_input: u16,
    back_streak: u8,
    resent: bool,
    done: bool,
}

struct Runtime {
    backend: Box<dyn Backend>,
    tracker: Option<GroupTracker>,
    fsm: SwitchFsm,
    /// Explicit legacy mode. It remains runnable during the v1 → v2 migration
    /// window so a previously working two-computer setup is never silently
    /// reinterpreted as a switch group.
    config: Option<Config>,
    /// Schema v2 group configuration. It can be detected, synchronized and
    /// inspected before the group transaction runtime is switched on.
    multi_device_config: Option<MultiDeviceConfig>,
    multi_device_enabled: bool,
    /// Last v2 write attempt. It is deliberately separate from the legacy
    /// state-machine report because v2 has source and arrival-correction
    /// phases rather than a single "push away" action.
    last_multi_device_report: Option<Value>,
    bounce_watch: Option<BounceWatch>,
    usb_snapshot_initialized: bool,
    config_path: PathBuf,
    log: SessionLog,
    stdout: std::io::Stdout,
    notification: Option<NotificationSink>,
    reply: Option<Sender<ResponseLine>>,
    last_snapshot: Vec<UsbDevice>,
    learning_baseline: Option<Vec<UsbDevice>>,
    last_candidates: Option<Value>,
    wizard_active: bool,
}

impl Runtime {
    fn notify(&mut self, kind: &str, data: Value) {
        if let Some(sink) = &self.notification {
            sink(kind, data);
            return;
        }
        let line = encode(&NotificationLine::new(kind, data));
        let _ = self.stdout.write_all(line.as_bytes());
        let _ = self.stdout.flush();
    }

    fn emit_event(&mut self, kind: &str, level: Level, fields: Value) {
        let mut map = serde_json::Map::new();
        if let Value::Object(o) = fields {
            map = o;
        }
        let record = EventRecord {
            ts_ms: now_ms(),
            kind: kind.to_string(),
            level,
            fields: map,
        };
        self.log.write(&record);
        let data = serde_json::to_value(&record).unwrap_or(Value::Null);
        self.notify("event", data);
    }

    fn respond(&mut self, resp: ResponseLine) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(resp);
            return;
        }
        let line = encode(&resp);
        let _ = self.stdout.write_all(line.as_bytes());
        let _ = self.stdout.flush();
    }

    fn load_config(&mut self) {
        let path = self.config_path.clone();
        match std::fs::read_to_string(&path) {
            Ok(text) => match ConfigDocument::parse_json(&text) {
                Ok(ConfigDocument::LegacyV1(cfg)) => {
                    if let Err(e) = cfg.validate() {
                        self.emit_event(
                            kinds::CONFIG_SAVED,
                            Level::Warn,
                            json!({"loaded": "invalid", "error": e.to_string()}),
                        );
                    }
                    self.apply_config(cfg);
                }
                Ok(ConfigDocument::MultiDeviceV2(cfg)) => {
                    if let Err(e) = cfg.validate() {
                        self.emit_event(
                            kinds::CONFIG_SAVED,
                            Level::Warn,
                            json!({"loaded": "invalid_v2", "error": e.to_string()}),
                        );
                    }
                    self.apply_multi_device_config(cfg);
                }
                Err(e) => {
                    self.emit_event(
                        kinds::CONFIG_SAVED,
                        Level::Warn,
                        json!({"loaded": "parse_error", "error": e.to_string()}),
                    );
                }
            },
            Err(_) => {
                self.emit_event(kinds::CONFIG_SAVED, Level::Debug, json!({"loaded": "none"}));
            }
        }
    }

    fn apply_config(&mut self, cfg: Config) {
        let mut tracker = GroupTracker::new(
            cfg.trigger.anchor.clone(),
            cfg.trigger.members.clone(),
            cfg.trigger.debounce.clone(),
        );
        // Seed with the current bus state so a group that is already present
        // counts toward stability instead of starting from an unknown state.
        let keys: Vec<DeviceKey> = self
            .last_snapshot
            .iter()
            .map(|d| DeviceKey {
                vid_pid: d.vid_pid.clone(),
                serial: d.serial.clone(),
            })
            .collect();
        tracker.update(now_ms(), &keys);
        self.tracker = Some(tracker);
        self.bounce_watch = None;
        let actions = self.fsm.handle(Input::ConfigUpdated(cfg.clone()), now_ms());
        self.config = Some(cfg);
        self.multi_device_config = None;
        self.multi_device_enabled = true;
        self.run_actions(actions);
    }

    fn apply_multi_device_config(&mut self, cfg: MultiDeviceConfig) {
        let mut tracker = GroupTracker::new(
            cfg.trigger.anchor.clone(),
            cfg.trigger.members.clone(),
            cfg.trigger.debounce.clone(),
        );
        let keys: Vec<DeviceKey> = self
            .last_snapshot
            .iter()
            .map(|device| DeviceKey {
                vid_pid: device.vid_pid.clone(),
                serial: device.serial.clone(),
            })
            .collect();
        tracker.update(now_ms(), &keys);
        self.tracker = Some(tracker);
        self.config = None;
        self.multi_device_config = Some(cfg);
        self.multi_device_enabled = true;
        // V2 owns its own source/arrival transaction paths. A fresh legacy FSM
        // prevents an old v1 away-input from firing after a migration.
        self.fsm = SwitchFsm::new();
        self.last_multi_device_report = None;
        self.bounce_watch = None;
        let mode = if self
            .multi_device_config
            .as_ref()
            .is_some_and(MultiDeviceConfig::ready_for_group_switch)
        {
            "multi_device_armed"
        } else {
            "multi_device_setup"
        };
        self.notify("state", json!({"state": self.fsm.state(), "mode": mode}));
    }

    fn active_trigger(&self) -> Option<TriggerConfig> {
        self.config
            .as_ref()
            .map(|config| config.trigger.clone())
            .or_else(|| {
                self.multi_device_config
                    .as_ref()
                    .map(|config| config.trigger.clone())
            })
    }

    fn active_monitor_count(&self) -> usize {
        self.config
            .as_ref()
            .map(|config| config.monitors.len())
            .or_else(|| {
                self.multi_device_config
                    .as_ref()
                    .map(|config| config.local_device.monitors.len())
            })
            .unwrap_or(0)
    }

    fn active_retry_policy(&self) -> kvmflow_core::retry::RetryPolicy {
        self.config
            .as_ref()
            .map(|config| config.advanced.ddc_retry.to_policy())
            .or_else(|| {
                self.multi_device_config
                    .as_ref()
                    .map(|config| config.advanced.ddc_retry.to_policy())
            })
            .unwrap_or_default()
    }

    fn active_config_value(&self) -> Value {
        if let Some(config) = &self.multi_device_config {
            return serde_json::to_value(config).unwrap_or(Value::Null);
        }
        self.config
            .as_ref()
            .and_then(|config| serde_json::to_value(config).ok())
            .unwrap_or(Value::Null)
    }

    fn config_mode(&self) -> &'static str {
        if let Some(config) = &self.multi_device_config {
            if config.ready_for_group_switch() {
                "multi_device_armed"
            } else {
                "multi_device_setup"
            }
        } else if self.config.is_some() {
            "legacy_v1"
        } else {
            "none"
        }
    }

    fn persist_and_apply_legacy_config(&mut self, id: u64, config: Config) {
        let text = serde_json::to_string_pretty(&config).unwrap_or_default();
        self.persist_config_text(id, text, "legacy_v1", move |runtime| {
            runtime.apply_config(config)
        });
    }

    fn persist_and_apply_multi_device_config(&mut self, id: u64, config: MultiDeviceConfig) {
        let text = serde_json::to_string_pretty(&config).unwrap_or_default();
        let mode = if config.ready_for_group_switch() {
            "multi_device_armed"
        } else {
            "multi_device_setup"
        };
        self.persist_config_text(id, text, mode, move |runtime| {
            runtime.apply_multi_device_config(config)
        });
    }

    fn persist_config_text<F>(&mut self, id: u64, text: String, mode: &'static str, apply: F)
    where
        F: FnOnce(&mut Self),
    {
        if let Some(parent) = self.config_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&self.config_path, text) {
            Ok(()) => {
                apply(self);
                self.emit_event(
                    kinds::CONFIG_SAVED,
                    Level::Info,
                    json!({"path": self.config_path.to_string_lossy(), "mode": mode}),
                );
                self.respond(ResponseLine::ok(
                    id,
                    json!({"saved": true, "path": self.config_path.to_string_lossy(), "mode": mode}),
                ));
            }
            Err(error) => self.respond(ResponseLine::err(id, E_CONFIG_IO, error.to_string())),
        }
    }

    fn run_actions(&mut self, actions: Vec<Action>) {
        let mut outcomes: Vec<MonitorOutcome> = Vec::new();
        let mut has_writes = false;
        for action in actions {
            match action {
                Action::Event {
                    kind,
                    level,
                    fields,
                } => {
                    self.emit_event(kind, level, fields);
                }
                Action::Notify { kind, data } => {
                    self.notify(kind, data);
                }
                Action::WriteInput {
                    edid_id,
                    value,
                    policy,
                } => {
                    has_writes = true;
                    let outcome = self.write_with_retry(&edid_id, value, &policy, false);
                    outcomes.push(outcome);
                }
            }
        }
        if has_writes {
            let actions = self.fsm.handle(
                Input::PushOutcomes {
                    per_monitor: outcomes,
                },
                now_ms(),
            );
            self.run_actions(actions);
        }
    }

    fn run_multi_device_writes(
        &mut self,
        phase: &'static str,
        group_id: &str,
        group_revision: u64,
        source_device_id: &str,
        target_device_id: &str,
        writes: Vec<kvmflow_core::multidevice::RouteWrite>,
    ) -> Vec<MonitorOutcome> {
        let policy = retry_policy_for_phase(phase, self.active_retry_policy());
        if let Err(error) = self.backend.refresh_displays() {
            self.emit_event(
                "display.refresh_failed",
                Level::Warn,
                json!({"phase": phase, "error": error.to_string()}),
            );
        }
        self.emit_event(
            kinds::SWITCH_PUSH_BEGIN,
            Level::Info,
            json!({
                "mode": "multi_device",
                "phase": phase,
                "group_id": group_id,
                "group_revision": group_revision,
                "source_device_id": source_device_id,
                "target_device_id": target_device_id,
                "monitors": writes.len(),
            }),
        );
        let mut outcomes = Vec::new();
        // Give every screen its first command before spending the retry budget
        // on an unavailable screen. One missing monitor must not hold the rest.
        let first_pass = kvmflow_core::retry::RetryPolicy {
            attempts: 1,
            delay_ms: 0,
        };
        for write in &writes {
            outcomes.push(self.write_with_retry(
                &write.fingerprint,
                write.value,
                &first_pass,
                false,
            ));
        }
        for (write, outcome) in writes.iter().zip(outcomes.iter_mut()) {
            if !outcome.commanded && policy.attempts > 1 {
                thread::sleep(Duration::from_millis(policy.delay_ms));
                let _ = self.backend.refresh_displays();
                let remaining = kvmflow_core::retry::RetryPolicy {
                    attempts: policy.attempts - 1,
                    delay_ms: policy.delay_ms,
                };
                let mut retried =
                    self.write_with_retry(&write.fingerprint, write.value, &remaining, false);
                retried.attempts += outcome.attempts;
                *outcome = retried;
            }
            self.emit_event(
                if outcome.commanded {
                    kinds::SWITCH_MONITOR_COMMANDED
                } else {
                    kinds::SWITCH_MONITOR_FAILED
                },
                if outcome.commanded {
                    Level::Info
                } else {
                    Level::Error
                },
                json!({
                    "mode": "multi_device", "phase": phase,
                    "fingerprint": write.fingerprint, "requested": write.value,
                    "commanded": outcome.commanded, "attempts": outcome.attempts,
                    "previous": outcome.previous, "readback": outcome.readback,
                    "readback_error": outcome.readback_error, "error": outcome.last_error,
                }),
            );
        }
        let all_commanded = outcomes.iter().all(|outcome| outcome.commanded);
        let report = json!({
            "mode": "multi_device",
            "phase": phase,
            "group_id": group_id,
            "group_revision": group_revision,
            "source_device_id": source_device_id,
            "target_device_id": target_device_id,
            "per_monitor": outcomes,
            "commanded": all_commanded,
            "note": kvmflow_core::statemachine::REPORT_NOTE,
        });
        self.emit_event(
            kinds::SWITCH_PUSH_REPORT,
            if all_commanded {
                Level::Info
            } else {
                Level::Warn
            },
            report.clone(),
        );
        self.last_multi_device_report = Some(report.clone());
        self.notify("switch.report", report);
        outcomes
    }

    fn start_bounce_watch(
        &mut self,
        local_monitors: &[(String, u16)],
        outcomes: &[MonitorOutcome],
    ) {
        let monitors: Vec<BounceWatchMonitor> = outcomes
            .iter()
            .filter(|outcome| outcome.commanded)
            .filter_map(|outcome| {
                let local_input = local_monitors
                    .iter()
                    .find(|(fingerprint, _)| fingerprint == &outcome.edid_id)
                    .map(|(_, input)| *input)?;
                (local_input != outcome.requested).then(|| BounceWatchMonitor {
                    fingerprint: outcome.edid_id.clone(),
                    target_input: outcome.requested,
                    local_input,
                    back_streak: 0,
                    resent: false,
                    done: false,
                })
            })
            .collect();
        let now = now_ms();
        self.bounce_watch = (!monitors.is_empty()).then(|| BounceWatch {
            started_ms: now,
            next_check_ms: now + BOUNCE_FIRST_CHECK_MS,
            monitors,
        });
    }

    fn check_bounce_watch(&mut self) {
        let now = now_ms();
        let Some(watch) = self.bounce_watch.as_ref() else {
            return;
        };
        if now >= watch.started_ms + BOUNCE_WATCH_MS {
            self.bounce_watch = None;
            return;
        }
        if now < watch.next_check_ms {
            return;
        }
        let Some(mut watch) = self.bounce_watch.take() else {
            return;
        };
        let deadline_ms = watch.started_ms + BOUNCE_WATCH_MS;
        let probe_timeout = || {
            Duration::from_millis(
                deadline_ms
                    .saturating_sub(now_ms())
                    .min(BOUNCE_PROBE_TIMEOUT_MS),
            )
        };
        // A monitor that returned to this host exposes its link again, so the
        // cached display handles must be rebuilt before reading it.
        let _ = self.backend.refresh_displays_with_timeout(probe_timeout());
        // A bounce resend is one best-effort physical write. The normal retry
        // path refreshes displays between attempts and can outlive this watch.
        let policy = kvmflow_core::retry::RetryPolicy {
            attempts: 1,
            delay_ms: 0,
        };
        let back: Vec<bool> = watch
            .monitors
            .iter()
            .map(|monitor| {
                matches!(
                    self.backend
                        .read_input_with_timeout(&monitor.fingerprint, probe_timeout()),
                    Ok(read) if read.value == monitor.local_input
                )
            })
            .collect();
        if now_ms() >= deadline_ms {
            return;
        }
        if back.iter().all(|here| *here) {
            self.emit_event(
                "switch.monitor.bounce_ignored",
                Level::Info,
                json!({ "reason": "all_monitors_returned", "monitors": watch.monitors.len() }),
            );
            return;
        }
        for (monitor, back_here) in watch.monitors.iter_mut().zip(back) {
            if monitor.done {
                continue;
            }
            if !back_here {
                monitor.back_streak = 0;
                continue;
            }
            monitor.back_streak += 1;
            if monitor.back_streak < BOUNCE_CONFIRM_CHECKS {
                continue;
            }
            if now_ms() >= deadline_ms {
                return;
            }
            monitor.back_streak = 0;
            if monitor.resent {
                monitor.done = true;
                self.emit_event(
                    "switch.monitor.bounce_gave_up",
                    Level::Warn,
                    json!({
                        "fingerprint": monitor.fingerprint,
                        "requested": monitor.target_input,
                        "observed": monitor.local_input,
                    }),
                );
                continue;
            }
            monitor.resent = true;
            let outcome =
                self.write_with_retry(&monitor.fingerprint, monitor.target_input, &policy, false);
            self.emit_event(
                "switch.monitor.bounce_resent",
                if outcome.commanded {
                    Level::Info
                } else {
                    Level::Warn
                },
                json!({
                    "fingerprint": monitor.fingerprint,
                    "requested": monitor.target_input,
                    "observed": monitor.local_input,
                    "commanded": outcome.commanded,
                    "attempts": outcome.attempts,
                    "error": outcome.last_error,
                }),
            );
        }
        if watch.monitors.iter().all(|monitor| monitor.done) {
            return;
        }
        watch.next_check_ms = now_ms() + BOUNCE_CHECK_INTERVAL_MS;
        self.bounce_watch = Some(watch);
    }

    fn handle_multi_device_tracker_event(&mut self, event: &TrackerEvent) {
        if !self.multi_device_enabled {
            return;
        }
        let Some(config) = self.multi_device_config.clone() else {
            return;
        };
        if !config.ready_for_group_switch() {
            return;
        }
        let Some(group) = config.switch_group.as_ref() else {
            return;
        };
        if !matches!(event, TrackerEvent::GroupLeft { .. }) {
            self.bounce_watch = None;
        }
        match event {
            TrackerEvent::GroupLeft { .. } => match group.route_after_departure(&config.local_device.device_id) {
                Ok(route) => {
                    let outcomes = self.run_multi_device_writes(
                        "source_fast_path",
                        &route.group_id,
                        route.revision,
                        &route.source_device_id,
                        &route.target_device_id,
                        route.writes,
                    );
                    let local_monitors: Vec<(String, u16)> = config
                        .local_device
                        .monitors
                        .iter()
                        .map(|monitor| (monitor.fingerprint.clone(), monitor.local_input))
                        .collect();
                    self.start_bounce_watch(&local_monitors, &outcomes);
                }
                Err(error) => self.emit_event(
                    kinds::SWITCH_PUSH_REPORT,
                    Level::Error,
                    json!({"mode": "multi_device", "phase": "source_fast_path", "error": error.to_string()}),
                ),
            },
            TrackerEvent::GroupArrived { .. } if config.advanced.arrival_correction_enabled => match local_arrival_correction(group, &config.local_device.device_id) {
                Ok(correction) => {
                    self.run_multi_device_writes(
                        "target_arrival_correction",
                        &correction.group_id,
                        correction.group_revision,
                        "unknown_until_transaction_sync",
                        &correction.local_device_id,
                        correction.writes,
                    );
                }
                Err(error) => self.emit_event(
                    kinds::SWITCH_PUSH_REPORT,
                    Level::Error,
                    json!({"mode": "multi_device", "phase": "target_arrival_correction", "error": error.to_string()}),
                ),
            },
            TrackerEvent::GroupArrived { .. } => self.emit_event(
                "trigger.arrival_correction_skipped",
                Level::Info,
                json!({"reason": "disabled_by_user", "local_device_id": config.local_device.device_id}),
            ),
            _ => {}
        }
    }

    fn write_with_retry(
        &mut self,
        edid_id: &str,
        value: u16,
        policy: &kvmflow_core::retry::RetryPolicy,
        capture_readback: bool,
    ) -> MonitorOutcome {
        let mut attempts = 0u32;
        let mut previous = None;
        let mut readback = None;
        let mut readback_error = None;
        let mut last_error = None;
        let mut commanded = false;
        while attempts < policy.attempts {
            if attempts > 0 {
                thread::sleep(Duration::from_millis(policy.delay_ms));
                // A failed command can be the first observable sign that the
                // OS rebuilt display handles. Re-resolve EDID -> handle/index
                // before retrying on both macOS and Windows.
                let _ = self.backend.refresh_displays();
            }
            attempts += 1;
            match self.backend.write_input(edid_id, value) {
                Ok(o) => {
                    previous = o.previous;
                    if o.commanded {
                        commanded = true;
                        if capture_readback {
                            match self.backend.read_input(edid_id) {
                                Ok(result) => readback = Some(result.value),
                                Err(error) => readback_error = Some(error.to_string()),
                            }
                        }
                        last_error = None;
                        break;
                    }
                    last_error = o.error.clone();
                }
                Err(e) => {
                    last_error = Some(e.to_string());
                }
            }
        }
        MonitorOutcome {
            edid_id: edid_id.to_string(),
            requested: value,
            commanded,
            attempts,
            previous,
            readback,
            readback_error,
            last_error,
        }
    }

    fn handle_usb_snapshot(&mut self, devices: Vec<UsbDevice>) {
        let first_snapshot = !self.usb_snapshot_initialized;
        self.usb_snapshot_initialized = true;
        let changed = devices != self.last_snapshot;
        self.last_snapshot = devices.clone();
        if !changed {
            return;
        }
        self.emit_event(
            kinds::USB_CHANGED,
            Level::Debug,
            json!({ "count": devices.len(), "keys": devices.iter().map(|d| d.key.clone()).collect::<Vec<_>>() }),
        );

        // Loading a persisted config happens before the platform USB poller
        // delivers its first snapshot. Treat that first observation as the
        // baseline, never as a device arrival: launching KVMFlow while the
        // keyboard is already on this host must not write any monitor input.
        if first_snapshot {
            if self.wizard_active && self.learning_baseline.as_ref().is_none_or(Vec::is_empty) {
                self.learning_baseline = Some(devices.clone());
            }
            if let Some(trigger) = self.active_trigger() {
                let mut tracker =
                    GroupTracker::new(trigger.anchor, trigger.members, trigger.debounce);
                let keys: Vec<DeviceKey> = devices
                    .iter()
                    .map(|device| DeviceKey {
                        vid_pid: device.vid_pid.clone(),
                        serial: device.serial.clone(),
                    })
                    .collect();
                tracker.update(now_ms(), &keys);
                self.tracker = Some(tracker);
            }
            return;
        }

        // Wizard trigger-group candidate detection: devices that were present
        // at wizard start and have now disappeared.
        if self.wizard_active {
            if let Some(baseline) = &self.learning_baseline {
                let gone: Vec<&UsbDevice> = baseline
                    .iter()
                    .filter(|b| !devices.iter().any(|d| d.key == b.key))
                    .collect();
                let staying = baseline.len() - gone.len();
                if !gone.is_empty() {
                    let candidates = json!({
                        "disappeared": gone,
                        "staying_count": staying,
                        "note": "devices that left this host while the wizard listened - confirm to make them the trigger group",
                    });
                    self.last_candidates = Some(candidates.clone());
                    self.emit_event(kinds::WIZARD_CANDIDATES, Level::Info, candidates.clone());
                    self.notify("wizard.candidates", candidates);
                }
            }
        }

        self.pump_tracker();
    }

    /// Feed the current bus state + current time into the debounce tracker
    /// and dispatch resulting FSM actions. Called on every snapshot change
    /// AND on every idle tick: stability/absence windows are time-based, so
    /// a quiet bus must still advance them (KVM-1: the group leaves once and
    /// the bus stays silent while T_absent elapses).
    fn pump_tracker(&mut self) {
        let Some(tracker) = self.tracker.as_mut() else {
            return;
        };
        let keys: Vec<DeviceKey> = self
            .last_snapshot
            .iter()
            .map(|d| DeviceKey {
                vid_pid: d.vid_pid.clone(),
                serial: d.serial.clone(),
            })
            .collect();
        let evs = tracker.update(now_ms(), &keys);
        for ev in evs {
            if self.multi_device_config.is_some() {
                self.handle_multi_device_tracker_event(&ev);
            }
            let actions = self.fsm.handle(Input::Tracker(ev), now_ms());
            self.run_actions(actions);
        }
    }

    fn handle_request(&mut self, req: kvmflow_core::protocol::RequestLine) -> bool {
        let id = req.id;
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
                let cfg_value = req.params.get("config").cloned().unwrap_or(Value::Null);
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
                        self.persist_and_apply_legacy_config(id, cfg);
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
                        self.persist_and_apply_multi_device_config(id, cfg);
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
                        "note": kvmflow_core::statemachine::REPORT_NOTE,
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
                if self.multi_device_config.is_some() {
                    self.multi_device_enabled = enabled;
                    if !enabled {
                        self.bounce_watch = None;
                    }
                    let state = if enabled { "armed" } else { "disabled" };
                    self.notify(
                        "state",
                        json!({"state": state, "mode": self.config_mode(), "enabled": enabled}),
                    );
                    self.respond(ResponseLine::ok(
                        id,
                        json!({"state": state, "enabled": enabled}),
                    ));
                    return true;
                }
                let actions = self.fsm.handle(Input::SetDisabled(!enabled), now_ms());
                self.run_actions(actions);
                self.respond(ResponseLine::ok(id, json!({"state": self.fsm.state()})));
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
            "state.get" => {
                let result = json!({
                    "state": self.fsm.state(),
                    "groupPresent": self.tracker.as_ref().map(|t| t.group_present()).unwrap_or(false),
                    "groupStable": self.tracker.as_ref().map(|t| t.group_stable()).unwrap_or(false),
                    "lastReport": self.fsm.last_report(),
                    "lastMultiDeviceReport": self.last_multi_device_report,
                    "monitors": self.active_monitor_count(),
                    "configMode": self.config_mode(),
                    "groupReady": self.multi_device_config.as_ref().map(|c| c.ready_for_group_switch()).unwrap_or(false),
                    "enabled": if self.multi_device_config.is_some() { self.multi_device_enabled } else { self.fsm.state() != kvmflow_core::statemachine::FsmState::Disabled },
                });
                self.respond(ResponseLine::ok(id, result));
            }
            "switch.pushNow" => {
                if self.multi_device_config.is_some() {
                    self.respond(ResponseLine::err(
                        id,
                        E_TRIGGER_NOT_ARMED,
                        "multi-device switching is driven by the physical USB trigger; manual push is unavailable",
                    ));
                    return true;
                }
                let reason = req
                    .params
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("manual")
                    .to_string();
                let actions = self.fsm.handle(Input::ManualPush { reason }, now_ms());
                let started = actions
                    .iter()
                    .any(|a| matches!(a, Action::WriteInput { .. }));
                self.run_actions(actions);
                if started {
                    self.respond(ResponseLine::ok(
                        id,
                        json!({"started": true, "state": self.fsm.state(), "report": self.fsm.last_report()}),
                    ));
                } else {
                    self.respond(ResponseLine::err(
                        id,
                        E_TRIGGER_NOT_ARMED,
                        format!("push rejected in state {:?}", self.fsm.state()),
                    ));
                }
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

fn default_config_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join("Library/Application Support/KVMFlow/config.json")
    } else {
        let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
        PathBuf::from(appdata).join("KVMFlow").join("config.json")
    }
}

fn default_log_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join("Library/Logs/KVMFlow")
    } else {
        let appdata = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
        PathBuf::from(appdata).join("KVMFlow").join("logs")
    }
}

fn spawn_usb_source(
    backend_kind: &str,
    poll_ms: u64,
    tx: Sender<Msg>,
    stopped: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    let kind = backend_kind.to_string();
    thread::spawn(move || match kind.as_str() {
        "simulated" => {
            // The simulated source is driven by the scenario player
            // (spawned separately); nothing to poll here.
        }
        #[cfg(target_os = "macos")]
        "macos_m1ddc" => {
            let mut last: Option<Vec<UsbDevice>> = None;
            while !stopped.load(Ordering::Acquire) {
                let snap = backends::macos::usb::usb_snapshot();
                if last.as_ref() != Some(&snap) {
                    last = Some(snap.clone());
                    if tx.send(Msg::Usb(snap)).is_err() {
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(poll_ms.max(100)));
            }
        }
        #[cfg(target_os = "windows")]
        "windows_dxva2" => {
            let mut last: Option<Vec<UsbDevice>> = None;
            while !stopped.load(Ordering::Acquire) {
                let snap = backends::windows::usb::usb_snapshot();
                let probes = backends::windows::usb::take_hub_probe_changes();
                if !probes.is_empty() && tx.send(Msg::HubProbe(probes)).is_err() {
                    break;
                }
                if last.as_ref() != Some(&snap) {
                    last = Some(snap.clone());
                    if tx.send(Msg::Usb(snap)).is_err() {
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(poll_ms.max(100)));
            }
        }
        _ => {}
    })
}

/// The monitor may still be on the other computer's input when an arrival
/// correction runs, so this host's OS can need a few seconds to expose the
/// display again. Other phases keep the configured policy.
fn retry_policy_for_phase(
    phase: &str,
    mut policy: kvmflow_core::retry::RetryPolicy,
) -> kvmflow_core::retry::RetryPolicy {
    if phase == "target_arrival_correction" {
        policy.attempts = policy.attempts.max(6);
        policy.delay_ms = policy.delay_ms.max(500);
    }
    policy
}

pub fn run_stdio() {
    let mut backend_name = String::from("auto");
    let mut scenario = String::from("idle");
    let mut fixture: Option<String> = None;
    let mut fixture_scale = 1.0f64;
    let mut config_path = default_config_path();
    let mut log_dir = default_log_dir();
    #[cfg(target_os = "macos")]
    let mut ddc_binary: Option<String> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--backend" => backend_name = args.next().unwrap_or_default(),
            "--scenario" => scenario = args.next().unwrap_or_default(),
            "--fixture" => fixture = args.next(),
            "--fixture-scale" => {
                fixture_scale = args.next().and_then(|s| s.parse().ok()).unwrap_or(1.0)
            }
            "--config" => config_path = PathBuf::from(args.next().unwrap_or_default()),
            "--log-dir" => log_dir = PathBuf::from(args.next().unwrap_or_default()),
            "--ddc-binary" => {
                #[cfg(target_os = "macos")]
                {
                    ddc_binary = args.next();
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = args.next();
                }
            }
            _ => {}
        }
    }
    let options = RuntimeOptions {
        backend: backend_name,
        scenario,
        fixture,
        fixture_scale,
        config_path,
        log_dir,
        ddc_binary: {
            #[cfg(target_os = "macos")]
            {
                ddc_binary
            }
            #[cfg(not(target_os = "macos"))]
            {
                None
            }
        },
    };
    let (tx, rx) = mpsc::channel();
    {
        let tx = tx.clone();
        thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                match line {
                    Ok(line) => {
                        if tx.send(Msg::Line(line)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.send(Msg::Shutdown);
        });
    }
    if let Err(error) = run_runtime(options, tx, rx, None, Arc::new(AtomicBool::new(false))) {
        eprintln!("kvmflow: {error}");
        std::process::exit(3);
    }
}

type NotificationSink = Arc<dyn Fn(&str, Value) + Send + Sync>;

pub struct RuntimeOptions {
    pub backend: String,
    pub scenario: String,
    pub fixture: Option<String>,
    pub fixture_scale: f64,
    pub config_path: PathBuf,
    pub log_dir: PathBuf,
    pub ddc_binary: Option<String>,
}

impl RuntimeOptions {
    pub fn new(config_path: PathBuf, log_dir: PathBuf) -> Self {
        Self {
            backend: std::env::var("KVMFLOW_BACKEND").unwrap_or_else(|_| "auto".into()),
            scenario: std::env::var("KVMFLOW_SCENARIO").unwrap_or_else(|_| "idle".into()),
            fixture: None,
            fixture_scale: 1.0,
            config_path,
            log_dir,
            ddc_binary: None,
        }
    }
}

/// Owns the in-process hardware worker. Requests keep the same canonical
/// contract as the JSONL server, including validation and structured errors.
pub struct RuntimeHandle {
    tx: Sender<Msg>,
    next_id: AtomicU64,
    stopped: Arc<AtomicBool>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
}

impl RuntimeHandle {
    pub fn start(
        options: RuntimeOptions,
        sink: impl Fn(&str, Value) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_tx = tx.clone();
        let worker_stop = stopped.clone();
        let sink: NotificationSink = Arc::new(sink);
        let worker = thread::Builder::new()
            .name("kvmflow-runtime".into())
            .spawn(move || {
                if let Err(error) = run_runtime(
                    options,
                    worker_tx,
                    rx,
                    Some(sink.clone()),
                    worker_stop.clone(),
                ) {
                    sink("runtime.error", json!({ "message": error }));
                }
                worker_stop.store(true, Ordering::Release);
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            tx,
            next_id: AtomicU64::new(1),
            stopped,
            worker: Mutex::new(Some(worker)),
        })
    }

    pub fn request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, kvmflow_core::protocol::RpcErrorBody> {
        use kvmflow_core::protocol::RpcErrorBody;
        let fail = |message: &str| RpcErrorBody {
            code: E_BACKEND.into(),
            message: message.into(),
        };
        if self.stopped.load(Ordering::Acquire) {
            return Err(fail("后台组件不可用，请重启应用并查看诊断。"));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let line = encode(&kvmflow_core::protocol::RequestLine {
            v: PROTOCOL_VERSION,
            id,
            method: method.into(),
            params,
        });
        let req = match parse_client_line(&line).map_err(|error| fail(&error.to_string()))? {
            Ok(req) => req,
            Err(response) => return Err(response.error.unwrap()),
        };
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Msg::Request(req, tx))
            .map_err(|_| fail("后台组件已停止。"))?;
        // DDC retries can take substantially longer than a normal UI action.
        let response = rx
            .recv_timeout(Duration::from_secs(120))
            .map_err(|_| fail("后台操作超时或已停止，请查看诊断后重试。"))?;
        if response.ok {
            Ok(response.result.unwrap_or(Value::Null))
        } else {
            Err(response.error.unwrap_or_else(|| fail("后台操作失败。")))
        }
    }

    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::Release);
        let _ = self.tx.send(Msg::Shutdown);
    }

    pub fn shutdown_and_join(&self) {
        self.shutdown();
        let Some(worker) = self.worker.lock().unwrap().take() else {
            return;
        };
        // The worker only observes `stopped` between messages, so a DDC retry
        // loop in flight could otherwise hold up application exit.
        let (done_tx, done_rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = worker.join();
            let _ = done_tx.send(());
        });
        let _ = done_rx.recv_timeout(Duration::from_secs(5));
    }
}

impl Drop for RuntimeHandle {
    fn drop(&mut self) {
        self.shutdown_and_join();
    }
}

fn run_runtime(
    options: RuntimeOptions,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    notification: Option<NotificationSink>,
    stopped: Arc<AtomicBool>,
) -> Result<(), String> {
    let RuntimeOptions {
        mut backend,
        scenario,
        fixture,
        fixture_scale,
        config_path,
        log_dir,
        ddc_binary,
    } = options;
    #[cfg(not(target_os = "macos"))]
    let _ = &ddc_binary;
    if backend == "auto" {
        backend = if cfg!(target_os = "macos") {
            "macos_m1ddc".into()
        } else {
            "windows_dxva2".into()
        };
    }

    std::fs::create_dir_all(&log_dir).ok();
    let log = SessionLog::open(log_dir.join("sidecar-session.jsonl"));

    let backend_name = backend;
    let backend: Box<dyn Backend> = match backend_name.as_str() {
        "simulated" => Box::new(backends::simulated::SimulatedBackend::new(&scenario)),
        #[cfg(target_os = "macos")]
        "macos_m1ddc" => match backends::macos::ddc::MacDdcBackend::new(ddc_binary.as_deref()) {
            Ok(b) => Box::new(b),
            Err(e) => return Err(e.to_string()),
        },
        #[cfg(target_os = "windows")]
        "windows_dxva2" => Box::new(backends::windows::ddc::WindowsDdcBackend::new()),
        other => return Err(format!("unknown backend {other}")),
    };

    // USB source
    if backend_name == "simulated" {
        // Scenario/fixture players emit plain device snapshots; forward them
        // into the runtime channel.
        let (stx, srx) = mpsc::channel::<Vec<UsbDevice>>();
        {
            let tx = tx.clone();
            thread::spawn(move || {
                for devices in srx {
                    let _ = tx.send(Msg::Usb(devices));
                }
            });
        }
        match fixture {
            Some(path) => {
                if let Err(e) = backends::simulated::spawn_fixture_replay(&path, fixture_scale, stx)
                {
                    eprintln!("kvmflow-sidecar: {e}");
                }
            }
            None => {
                let _ = backends::simulated::spawn_scenario_player(&scenario, stx);
            }
        }
    } else {
        let poll = 250u64;
        let _ = spawn_usb_source(&backend_name, poll, tx.clone(), stopped.clone());
    }

    let mut rt = Runtime {
        backend,
        tracker: None,
        fsm: SwitchFsm::new(),
        config: None,
        multi_device_config: None,
        multi_device_enabled: true,
        last_multi_device_report: None,
        bounce_watch: None,
        usb_snapshot_initialized: false,
        config_path,
        log,
        stdout: std::io::stdout(),
        notification,
        reply: None,
        last_snapshot: Vec::new(),
        learning_baseline: None,
        last_candidates: None,
        wizard_active: false,
    };

    rt.notify(
        "ready",
        json!({
            "sidecar_version": kvmflow_core::SIDECAR_VERSION,
            "protocol_version": PROTOCOL_VERSION,
            "platform": std::env::consts::OS,
            "backend": rt.backend.name(),
            "simulated": rt.backend.simulated(),
            "pid": std::process::id(),
        }),
    );
    rt.emit_event(
        kinds::SIDECAR_READY,
        Level::Info,
        json!({"backend": rt.backend.name(), "protocol": PROTOCOL_VERSION}),
    );
    rt.load_config();

    while !stopped.load(Ordering::Acquire) {
        // Idle tick path: drain with timeout so cooldown expiry is observed.
        let msg = match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(m) => m,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Time-based tracker windows (T_stable / T_absent) advance on
                // a quiet bus too - pump before the FSM tick.
                rt.pump_tracker();
                let actions = rt.fsm.handle(Input::Tick { now_ms: now_ms() }, now_ms());
                rt.run_actions(actions);
                rt.check_bounce_watch();
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match msg {
            Msg::Request(req, reply) => {
                rt.reply = Some(reply);
                if !rt.handle_request(req) {
                    break;
                }
            }
            Msg::Line(line) => {
                match parse_client_line(&line) {
                    Ok(Ok(req)) => {
                        if !rt.handle_request(req) {
                            break;
                        }
                    }
                    Ok(Err(resp)) => rt.respond(resp),
                    Err(e) => {
                        rt.notify("log", json!({ "level": "warn", "msg": format!("dropping malformed line: {e}") }));
                    }
                }
            }
            Msg::Usb(devices) => rt.handle_usb_snapshot(devices),
            Msg::HubProbe(changes) => {
                for (key, outcome) in changes {
                    rt.emit_event(
                        kinds::USB_HUB_PROBE,
                        Level::Info,
                        json!({ "key": key, "outcome": outcome }),
                    );
                }
            }
            Msg::Shutdown => break,
        }
        // Busy request traffic can starve the idle tick, so the watch is also
        // advanced after every message.
        rt.check_bounce_watch();
    }

    rt.emit_event(kinds::SIDECAR_SHUTDOWN, Level::Info, json!({}));
    stopped.store(true, Ordering::Release);
    Ok(())
}

#[cfg(test)]
mod retry_policy_tests {
    use super::retry_policy_for_phase;
    use kvmflow_core::retry::RetryPolicy;

    #[test]
    fn arrival_correction_waits_longer_than_the_configured_policy() {
        let base = RetryPolicy {
            attempts: 2,
            delay_ms: 100,
        };
        let policy = retry_policy_for_phase("target_arrival_correction", base);
        assert_eq!(
            policy,
            RetryPolicy {
                attempts: 6,
                delay_ms: 500
            }
        );
    }

    #[test]
    fn arrival_correction_never_shrinks_a_larger_configured_policy() {
        let base = RetryPolicy {
            attempts: 9,
            delay_ms: 800,
        };
        assert_eq!(
            retry_policy_for_phase("target_arrival_correction", base.clone()),
            base
        );
    }

    #[test]
    fn source_fast_path_keeps_the_configured_policy() {
        let base = RetryPolicy {
            attempts: 2,
            delay_ms: 100,
        };
        assert_eq!(
            retry_policy_for_phase("source_fast_path", base.clone()),
            base
        );
    }
}
