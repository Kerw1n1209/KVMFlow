//! Switch state machine - single copy. Pure reducer: inputs are tracker
//! events / control commands plus a timestamp, outputs are Actions the
//! sidecar runtime executes against its backend. No I/O happens here.
//!
//! Model (KVM-1 verified):
//! - push-away: when the trigger group leaves THIS host, this host - which
//!   holds the pictures - writes every monitor's away_input. The receiving
//!   host performs no DDC (it cannot: only the active side reads/writes).
//! - a DDC command returning ok is "commanded", never "picture switched";
//!   picture confirmation is exclusively human.

use crate::config::Config;
use crate::debounce::TrackerEvent;
use crate::events::{kinds, Level};
use crate::retry::RetryPolicy;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FsmState {
    /// No valid config yet.
    Unconfigured,
    /// Wizard learning mode: tracker events are recorded, never acted on.
    Learning,
    /// Armed and watching.
    Idle,
    /// A push-away is executing (DDC writes in flight).
    Pushing,
    /// Post-push cooldown; new triggers are ignored until it expires.
    Cooldown,
    /// User-disabled (tray toggle).
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MonitorOutcome {
    pub edid_id: String,
    /// The VCP 0x60 value that this write attempted to send. This is
    /// diagnostic evidence, not a claim that the panel changed pictures.
    pub requested: u16,
    /// true = the DDC write was accepted by the backend (machine_observed
    /// only - NOT picture confirmation).
    pub commanded: bool,
    pub attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<u16>,
    /// Best-effort VCP 0x60 read made after an accepted write. Some monitors
    /// return stale or unstable values, so this never changes `commanded`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readback: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readback_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwitchReport {
    pub manual: bool,
    pub trigger_reason: String,
    pub ts_ms: u64,
    pub per_monitor: Vec<MonitorOutcome>,
    pub note: &'static str,
}

pub const REPORT_NOTE: &str =
    "ddc_commanded_is_machine_observed_only_picture_switch_requires_human_confirmation";

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Execute one DDC write with retries; report via `complete_push`.
    WriteInput {
        edid_id: String,
        value: u16,
        policy: RetryPolicy,
    },
    /// Emit a session-log event.
    Event {
        kind: &'static str,
        level: Level,
        fields: serde_json::Value,
    },
    /// Tell the Electron shell something (notification lines).
    Notify {
        kind: &'static str,
        data: serde_json::Value,
    },
}

#[derive(Debug, Clone)]
pub enum Input {
    Tracker(TrackerEvent),
    Tick {
        now_ms: u64,
    },
    WizardBegin,
    WizardEnd,
    ConfigUpdated(Config),
    SetDisabled(bool),
    /// Manual push (tray / wizard final test). Reason recorded in the report.
    ManualPush {
        reason: String,
    },
    /// Runtime reports the outcomes of the writes issued for the current push.
    PushOutcomes {
        per_monitor: Vec<MonitorOutcome>,
    },
}

pub struct SwitchFsm {
    state: FsmState,
    config: Option<Config>,
    cooldown_until_ms: Option<u64>,
    last_report: Option<SwitchReport>,
    /// Snapshot of tracker events seen while learning (for the wizard).
    learning_log: Vec<(u64, TrackerEvent)>,
    pending_reason: Option<String>,
    pending_manual: Option<bool>,
}

impl Default for SwitchFsm {
    fn default() -> Self {
        Self::new()
    }
}

impl SwitchFsm {
    pub fn new() -> Self {
        Self {
            state: FsmState::Unconfigured,
            config: None,
            cooldown_until_ms: None,
            last_report: None,
            learning_log: Vec::new(),
            pending_reason: None,
            pending_manual: None,
        }
    }

    pub fn state(&self) -> FsmState {
        self.state
    }

    pub fn config(&self) -> Option<&Config> {
        self.config.as_ref()
    }

    pub fn last_report(&self) -> Option<&SwitchReport> {
        self.last_report.as_ref()
    }

    pub fn learning_log(&self) -> &[(u64, TrackerEvent)] {
        &self.learning_log
    }

    fn set_state(&mut self, actions: &mut Vec<Action>, new: FsmState, now_ms: u64) {
        if self.state != new {
            actions.push(Action::Event {
                kind: kinds::STATE_CHANGE,
                level: Level::Info,
                fields: json!({ "from": self.state, "to": new, "ts_ms": now_ms }),
            });
            actions.push(Action::Notify {
                kind: "state",
                data: json!({ "state": new }),
            });
            self.state = new;
        }
    }

    fn base_state_for_config(&self, cfg: &Config) -> FsmState {
        if self.state == FsmState::Learning {
            FsmState::Learning
        } else if self.state == FsmState::Disabled {
            FsmState::Disabled
        } else if cfg.validate().is_ok() && cfg.armed_ready() {
            FsmState::Idle
        } else {
            FsmState::Unconfigured
        }
    }

    /// Begin a push: emits the write actions and moves to Pushing.
    fn begin_push(&mut self, actions: &mut Vec<Action>, now_ms: u64, manual: bool, reason: String) {
        let Some(cfg) = self.config.clone() else {
            return;
        };
        let policy = cfg.advanced.ddc_retry.to_policy();
        actions.push(Action::Event {
            kind: kinds::SWITCH_PUSH_BEGIN,
            level: Level::Info,
            fields: json!({ "manual": manual, "reason": reason, "monitors": cfg.monitors.len() }),
        });
        actions.push(Action::Notify {
            kind: "trigger",
            data: json!({ "action": "push_begin", "manual": manual, "reason": reason }),
        });
        for m in &cfg.monitors {
            actions.push(Action::WriteInput {
                edid_id: m.edid_id.clone(),
                value: m.away_input,
                policy: policy.clone(),
            });
        }
        self.set_state(actions, FsmState::Pushing, now_ms);
        // Remember why we are pushing so the report can cite it.
        self.pending_reason = Some(reason);
        self.pending_manual = Some(manual);
    }

    pub fn handle(&mut self, input: Input, now_ms: u64) -> Vec<Action> {
        let mut actions = Vec::new();
        match input {
            Input::ConfigUpdated(cfg) => {
                self.config = Some(cfg);
                let next = self.base_state_for_config(self.config.as_ref().unwrap());
                self.set_state(&mut actions, next, now_ms);
            }
            Input::WizardBegin => {
                self.learning_log.clear();
                actions.push(Action::Event {
                    kind: kinds::WIZARD_BEGIN,
                    level: Level::Info,
                    fields: json!({}),
                });
                self.set_state(&mut actions, FsmState::Learning, now_ms);
            }
            Input::WizardEnd => {
                actions.push(Action::Event {
                    kind: kinds::WIZARD_END,
                    level: Level::Info,
                    fields: json!({ "events_seen": self.learning_log.len() }),
                });
                let next = match self.config.as_ref() {
                    Some(cfg) if cfg.validate().is_ok() && cfg.armed_ready() => FsmState::Idle,
                    _ => FsmState::Unconfigured,
                };
                self.set_state(&mut actions, next, now_ms);
            }
            Input::SetDisabled(dis) => {
                if dis {
                    self.set_state(&mut actions, FsmState::Disabled, now_ms);
                } else {
                    // Re-enable: recompute from config, ignoring the current
                    // Disabled state (but a wizard in progress stays Learning).
                    if self.state != FsmState::Learning {
                        let next = match self.config.as_ref() {
                            Some(cfg) if cfg.validate().is_ok() && cfg.armed_ready() => {
                                FsmState::Idle
                            }
                            _ => FsmState::Unconfigured,
                        };
                        self.set_state(&mut actions, next, now_ms);
                    }
                }
            }
            Input::Tracker(ev) => {
                if self.state == FsmState::Learning {
                    self.learning_log.push((now_ms, ev.clone()));
                    // Still surface tracker events to the wizard UI.
                    actions.push(self.tracker_notify(&ev));
                    return actions;
                }
                match ev {
                    TrackerEvent::GroupStable { stable_for_ms } => {
                        actions.push(Action::Event {
                            kind: kinds::TRIGGER_GROUP_STABLE,
                            level: Level::Info,
                            fields: json!({ "stable_for_ms": stable_for_ms }),
                        });
                        actions.push(
                            self.tracker_notify(&TrackerEvent::GroupStable { stable_for_ms }),
                        );
                    }
                    TrackerEvent::GroupArrived { stable_for_ms } => {
                        actions.push(Action::Event {
                            kind: kinds::TRIGGER_GROUP_ARRIVED,
                            level: Level::Info,
                            fields: json!({ "stable_for_ms": stable_for_ms }),
                        });
                        actions.push(
                            self.tracker_notify(&TrackerEvent::GroupArrived { stable_for_ms }),
                        );
                    }
                    TrackerEvent::GroupLeft { absent_for_ms } => {
                        actions.push(Action::Event {
                            kind: kinds::TRIGGER_GROUP_LEFT,
                            level: Level::Info,
                            fields: json!({ "absent_for_ms": absent_for_ms }),
                        });
                        let in_cooldown =
                            self.cooldown_until_ms.map(|t| now_ms < t).unwrap_or(false);
                        match self.state {
                            FsmState::Idle if !in_cooldown => {
                                let reason = format!("group_absent_{absent_for_ms}ms");
                                self.begin_push(&mut actions, now_ms, false, reason);
                            }
                            FsmState::Idle => {
                                actions.push(Action::Notify {
                                    kind: "trigger",
                                    data: json!({ "action": "suppressed_cooldown", "absent_for_ms": absent_for_ms }),
                                });
                            }
                            _ => {}
                        }
                    }
                    TrackerEvent::UnexpectedReturn { gap_ms } => {
                        actions.push(Action::Event {
                            kind: kinds::TRIGGER_UNEXPECTED_RETURN,
                            level: Level::Warn,
                            fields: json!({ "gap_ms": gap_ms }),
                        });
                        // Both readings of this signature, honestly stated.
                        actions.push(Action::Notify {
                            kind: "trigger",
                            data: json!({
                                "action": "unexpected_return",
                                "gap_ms": gap_ms,
                                "interpretations": [
                                    "switch_back_pressed: normal - keyboard returned to this host",
                                    "replug_or_flaky_link: monitors may have been pushed away wrongly; use the monitor OSD or the recovery guidance to switch them back"
                                ]
                            }),
                        });
                    }
                    TrackerEvent::BounceAbsorbed { gap_ms } => {
                        actions.push(Action::Event {
                            kind: kinds::TRIGGER_BOUNCE_ABSORBED,
                            level: Level::Debug,
                            fields: json!({ "gap_ms": gap_ms }),
                        });
                    }
                }
            }
            Input::Tick { now_ms } => {
                if self.state == FsmState::Cooldown {
                    let expired = self.cooldown_until_ms.map(|t| now_ms >= t).unwrap_or(true);
                    if expired {
                        let next = match self.config.as_ref() {
                            Some(cfg) if cfg.validate().is_ok() && cfg.armed_ready() => {
                                FsmState::Idle
                            }
                            _ => FsmState::Unconfigured,
                        };
                        self.set_state(&mut actions, next, now_ms);
                    }
                }
            }
            Input::ManualPush { reason } => {
                let allowed = matches!(
                    self.state,
                    FsmState::Idle
                        | FsmState::Learning
                        | FsmState::Cooldown
                        | FsmState::Unconfigured
                ) && self.config.is_some();
                if allowed {
                    self.begin_push(&mut actions, now_ms, true, reason);
                } else {
                    actions.push(Action::Notify {
                        kind: "trigger",
                        data: json!({ "action": "manual_push_rejected", "state": self.state }),
                    });
                }
            }
            Input::PushOutcomes { per_monitor } => {
                if self.state != FsmState::Pushing {
                    return actions;
                }
                let reason = self
                    .pending_reason
                    .take()
                    .unwrap_or_else(|| "unknown".into());
                let manual = self.pending_manual.take().unwrap_or(false);
                let all_commanded = per_monitor.iter().all(|m| m.commanded);
                for m in &per_monitor {
                    let kind = if m.commanded {
                        kinds::SWITCH_MONITOR_COMMANDED
                    } else {
                        kinds::SWITCH_MONITOR_FAILED
                    };
                    let level = if m.commanded {
                        Level::Info
                    } else {
                        Level::Error
                    };
                    let mut fields = json!({ "edid_id": m.edid_id, "requested": m.requested, "attempts": m.attempts });
                    if let Some(p) = m.previous {
                        fields["previous"] = json!(p);
                    }
                    if let Some(e) = &m.last_error {
                        fields["error"] = json!(e);
                    }
                    if let Some(readback) = m.readback {
                        fields["readback"] = json!(readback);
                    }
                    if let Some(error) = &m.readback_error {
                        fields["readback_error"] = json!(error);
                    }
                    actions.push(Action::Event {
                        kind,
                        level,
                        fields,
                    });
                }
                let report = SwitchReport {
                    manual,
                    trigger_reason: reason,
                    ts_ms: now_ms,
                    per_monitor,
                    note: REPORT_NOTE,
                };
                actions.push(Action::Event {
                    kind: kinds::SWITCH_PUSH_REPORT,
                    level: if all_commanded {
                        Level::Info
                    } else {
                        Level::Warn
                    },
                    fields: serde_json::to_value(&report).unwrap_or_default(),
                });
                actions.push(Action::Notify {
                    kind: "switch.report",
                    data: serde_json::to_value(&report).unwrap_or_default(),
                });
                self.last_report = Some(report);
                let cooldown = self
                    .config
                    .as_ref()
                    .map(|c| c.trigger.debounce.t_cooldown_ms)
                    .unwrap_or(15_000);
                self.cooldown_until_ms = Some(now_ms + cooldown);
                self.set_state(&mut actions, FsmState::Cooldown, now_ms);
            }
        }
        actions
    }

    fn tracker_notify(&self, ev: &TrackerEvent) -> Action {
        let data = serde_json::to_value(ev).unwrap_or_default();
        Action::Notify {
            kind: "trigger",
            data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{example_config, AwayInputSource, InputSource, MonitorConfig};
    fn armed_fsm() -> SwitchFsm {
        let mut f = SwitchFsm::new();
        f.handle(Input::ConfigUpdated(example_config()), 0);
        assert_eq!(f.state(), FsmState::Idle);
        f
    }

    fn write_actions(actions: &[Action]) -> Vec<(String, u16)> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::WriteInput { edid_id, value, .. } => Some((edid_id.clone(), *value)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn group_left_while_idle_pushes_away_inputs() {
        let mut f = armed_fsm();
        let actions = f.handle(
            Input::Tracker(TrackerEvent::GroupLeft {
                absent_for_ms: 10_000,
            }),
            1_000,
        );
        let writes = write_actions(&actions);
        assert_eq!(writes, vec![("SAC-2763-S:0000000000001".to_string(), 16)]);
        assert_eq!(f.state(), FsmState::Pushing);
    }

    #[test]
    fn learning_mode_never_pushes() {
        let mut f = armed_fsm();
        f.handle(Input::WizardBegin, 10);
        let actions = f.handle(
            Input::Tracker(TrackerEvent::GroupLeft {
                absent_for_ms: 10_000,
            }),
            1_000,
        );
        assert!(write_actions(&actions).is_empty());
        assert_eq!(f.state(), FsmState::Learning);
        assert_eq!(f.learning_log().len(), 1);
    }

    #[test]
    fn cooldown_suppresses_immediate_retrigger() {
        let mut f = armed_fsm();
        f.handle(
            Input::Tracker(TrackerEvent::GroupLeft {
                absent_for_ms: 10_000,
            }),
            1_000,
        );
        let outcomes = vec![MonitorOutcome {
            edid_id: "SAC-2763-S:0000000000001".into(),
            requested: 17,
            commanded: true,
            attempts: 1,
            previous: Some(15),
            // A monitor may report a stale VCP value after accepting a write.
            // This must remain diagnostics, never a picture-success decision.
            readback: Some(31),
            readback_error: None,
            last_error: None,
        }];
        let actions = f.handle(
            Input::PushOutcomes {
                per_monitor: outcomes,
            },
            1_200,
        );
        assert!(actions.iter().any(|a| matches!(
            a,
            Action::Notify {
                kind: "switch.report",
                ..
            }
        )));
        assert_eq!(f.state(), FsmState::Cooldown);
        // trigger inside cooldown is suppressed
        let actions = f.handle(
            Input::Tracker(TrackerEvent::GroupLeft {
                absent_for_ms: 10_000,
            }),
            2_000,
        );
        assert!(write_actions(&actions).is_empty());
        // after cooldown expiry tick -> idle, trigger works again
        f.handle(Input::Tick { now_ms: 20_000 }, 20_000);
        assert_eq!(f.state(), FsmState::Idle);
        let actions = f.handle(
            Input::Tracker(TrackerEvent::GroupLeft {
                absent_for_ms: 10_000,
            }),
            21_000,
        );
        assert!(!write_actions(&actions).is_empty());
    }

    #[test]
    fn report_never_claims_picture_success() {
        let mut f = armed_fsm();
        f.handle(
            Input::Tracker(TrackerEvent::GroupLeft {
                absent_for_ms: 10_000,
            }),
            1_000,
        );
        let outcomes = vec![MonitorOutcome {
            edid_id: "SAC-2763-S:0000000000001".into(),
            requested: 17,
            commanded: true,
            attempts: 1,
            previous: Some(15),
            // A monitor may report a stale VCP value after accepting a write.
            // This must remain diagnostics, never a picture-success decision.
            readback: Some(31),
            readback_error: None,
            last_error: None,
        }];
        f.handle(
            Input::PushOutcomes {
                per_monitor: outcomes,
            },
            1_100,
        );
        let r = f.last_report().unwrap();
        assert!(r.per_monitor[0].commanded);
        assert_eq!(r.per_monitor[0].requested, 17);
        assert_eq!(r.per_monitor[0].readback, Some(31));
        assert!(r.note.contains("human_confirmation"));
    }

    #[test]
    fn manual_push_allowed_when_configured() {
        let mut f = armed_fsm();
        let actions = f.handle(
            Input::ManualPush {
                reason: "wizard_final_test".into(),
            },
            5,
        );
        assert!(!write_actions(&actions).is_empty());
        assert_eq!(f.state(), FsmState::Pushing);
    }

    #[test]
    fn unconfigured_trigger_still_allows_manual_push_for_wizard() {
        // The wizard's final supervised test happens BEFORE the trigger
        // profile is learned; a config with monitors + away inputs but no
        // anchor must still allow a manual push.
        let mut f = SwitchFsm::new();
        let mut cfg = example_config();
        cfg.trigger.anchor.vid_pid = String::new();
        f.handle(Input::ConfigUpdated(cfg), 0);
        assert_eq!(f.state(), FsmState::Unconfigured);
        let actions = f.handle(
            Input::ManualPush {
                reason: "wizard_final_test".into(),
            },
            1,
        );
        assert!(!write_actions(&actions).is_empty());
        // but with no config at all it is rejected
        let mut g = SwitchFsm::new();
        let actions = g.handle(Input::ManualPush { reason: "x".into() }, 1);
        assert!(write_actions(&actions).is_empty());
    }

    #[test]
    fn disabled_blocks_triggers() {
        let mut f = armed_fsm();
        f.handle(Input::SetDisabled(true), 5);
        let actions = f.handle(
            Input::Tracker(TrackerEvent::GroupLeft {
                absent_for_ms: 10_000,
            }),
            1_000,
        );
        assert!(write_actions(&actions).is_empty());
        f.handle(Input::SetDisabled(false), 6);
        assert_eq!(f.state(), FsmState::Idle);
    }

    #[test]
    fn partial_failure_reported_per_monitor() {
        let mut cfg = example_config();
        cfg.monitors.push(MonitorConfig {
            edid_id: "SAC-2466-S:0000000000000".into(),
            label: "G52plus".into(),
            here_input: 7,
            away_input: 8,
            here_input_source: InputSource::LearnedActiveRead,
            away_input_source: AwayInputSource::HeuristicPrefill,
            away_input_confirmed: false,
        });
        let mut f = SwitchFsm::new();
        f.handle(Input::ConfigUpdated(cfg), 0);
        f.handle(
            Input::Tracker(TrackerEvent::GroupLeft {
                absent_for_ms: 10_000,
            }),
            1_000,
        );
        let outcomes = vec![
            MonitorOutcome {
                edid_id: "SAC-2763-S:0000000000001".into(),
                requested: 17,
                commanded: true,
                attempts: 1,
                previous: Some(15),
                readback: Some(17),
                readback_error: None,
                last_error: None,
            },
            MonitorOutcome {
                edid_id: "SAC-2466-S:0000000000000".into(),
                requested: 8,
                commanded: false,
                attempts: 3,
                previous: Some(7),
                readback: None,
                readback_error: None,
                last_error: Some("E_DDC_FAILED: m1ddc exit 1".into()),
            },
        ];
        let actions = f.handle(
            Input::PushOutcomes {
                per_monitor: outcomes,
            },
            1_300,
        );
        let report_notify = actions
            .iter()
            .find_map(|a| match a {
                Action::Notify {
                    kind: "switch.report",
                    data,
                } => Some(data.clone()),
                _ => None,
            })
            .expect("switch.report notification");
        assert_eq!(report_notify["per_monitor"][1]["commanded"], false);
        assert!(report_notify["note"]
            .as_str()
            .unwrap()
            .contains("machine_observed_only"));
    }
}
