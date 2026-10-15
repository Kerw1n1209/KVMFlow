//! Runtime status machine - single copy. Pure reducer: tracker events and
//! control commands plus a timestamp in, session-log events and client
//! notifications out. It never decides a DDC write: switching is planned
//! from the switch group (`multidevice` / `transaction`) by the runtime.
//!
//! Model (KVM-1 verified):
//! - wizard learning records tracker events and never acts on them.
//! - a DDC command returning ok is "commanded", never "picture switched";
//!   picture confirmation is exclusively human.

use crate::debounce::TrackerEvent;
use crate::events::{kinds, Level};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FsmState {
    /// No switch group that can switch yet.
    Unconfigured,
    /// Wizard learning mode: tracker events are recorded, never acted on.
    Learning,
    /// Armed and watching.
    Idle,
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

pub const REPORT_NOTE: &str =
    "ddc_commanded_is_machine_observed_only_picture_switch_requires_human_confirmation";

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Emit a session-log event.
    Event {
        kind: &'static str,
        level: Level,
        fields: serde_json::Value,
    },
    /// Tell the client something (notification lines).
    Notify {
        kind: &'static str,
        data: serde_json::Value,
    },
}

#[derive(Debug, Clone)]
pub enum Input {
    Tracker(TrackerEvent),
    WizardBegin,
    WizardEnd,
    /// Whether the applied config can switch (a complete switch group and
    /// trigger).
    ConfigReady(bool),
    SetDisabled(bool),
}

#[derive(Debug, Clone)]
pub struct SwitchFsm {
    state: FsmState,
    ready: bool,
    learning_log: Vec<(u64, TrackerEvent)>,
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
            ready: false,
            learning_log: Vec::new(),
        }
    }

    pub fn state(&self) -> FsmState {
        self.state
    }

    pub fn learning_log(&self) -> &[(u64, TrackerEvent)] {
        &self.learning_log
    }

    fn settled_state(&self) -> FsmState {
        if self.ready {
            FsmState::Idle
        } else {
            FsmState::Unconfigured
        }
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

    pub fn handle(&mut self, input: Input, now_ms: u64) -> Vec<Action> {
        let mut actions = Vec::new();
        match input {
            Input::ConfigReady(ready) => {
                self.ready = ready;
                // A wizard in progress stays Learning; a pause stays paused.
                if !matches!(self.state, FsmState::Learning | FsmState::Disabled) {
                    self.set_state(&mut actions, self.settled_state(), now_ms);
                }
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
                self.set_state(&mut actions, self.settled_state(), now_ms);
            }
            Input::SetDisabled(true) => self.set_state(&mut actions, FsmState::Disabled, now_ms),
            Input::SetDisabled(false) => {
                if self.state != FsmState::Learning {
                    self.set_state(&mut actions, self.settled_state(), now_ms);
                }
            }
            Input::Tracker(ev) => {
                if self.state == FsmState::Learning {
                    self.learning_log.push((now_ms, ev.clone()));
                    // Still surface tracker events to the wizard UI.
                    actions.push(tracker_notify(&ev));
                    return actions;
                }
                match ev {
                    TrackerEvent::GroupStable { stable_for_ms } => {
                        actions.push(Action::Event {
                            kind: kinds::TRIGGER_GROUP_STABLE,
                            level: Level::Info,
                            fields: json!({ "stable_for_ms": stable_for_ms }),
                        });
                        actions.push(tracker_notify(&ev));
                    }
                    TrackerEvent::GroupArrived { stable_for_ms } => {
                        actions.push(Action::Event {
                            kind: kinds::TRIGGER_GROUP_ARRIVED,
                            level: Level::Info,
                            fields: json!({ "stable_for_ms": stable_for_ms }),
                        });
                        actions.push(tracker_notify(&ev));
                    }
                    TrackerEvent::GroupLeft { absent_for_ms } => {
                        actions.push(Action::Event {
                            kind: kinds::TRIGGER_GROUP_LEFT,
                            level: Level::Info,
                            fields: json!({ "absent_for_ms": absent_for_ms }),
                        });
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
        }
        actions
    }
}

fn tracker_notify(ev: &TrackerEvent) -> Action {
    Action::Notify {
        kind: "trigger",
        data: serde_json::to_value(ev).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn states(actions: &[Action]) -> Vec<serde_json::Value> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::Notify {
                    kind: "state",
                    data,
                } => Some(data["state"].clone()),
                _ => None,
            })
            .collect()
    }

    fn ready_fsm() -> SwitchFsm {
        let mut fsm = SwitchFsm::new();
        fsm.handle(Input::ConfigReady(true), 0);
        assert_eq!(fsm.state(), FsmState::Idle);
        fsm
    }

    #[test]
    fn readiness_moves_between_unconfigured_and_idle() {
        let mut fsm = SwitchFsm::new();
        assert_eq!(states(&fsm.handle(Input::ConfigReady(true), 1)), ["idle"]);
        assert_eq!(
            states(&fsm.handle(Input::ConfigReady(false), 2)),
            ["unconfigured"]
        );
        assert!(states(&fsm.handle(Input::ConfigReady(false), 3)).is_empty());
    }

    #[test]
    fn learning_records_tracker_events_and_survives_config_changes() {
        let mut fsm = ready_fsm();
        fsm.handle(Input::WizardBegin, 10);
        assert_eq!(fsm.state(), FsmState::Learning);
        let actions = fsm.handle(
            Input::Tracker(TrackerEvent::GroupLeft {
                absent_for_ms: 1_000,
            }),
            11,
        );
        assert!(
            actions.iter().all(|action| matches!(
                action,
                Action::Notify {
                    kind: "trigger",
                    ..
                }
            )),
            "learning only surfaces tracker events: {actions:?}"
        );
        assert_eq!(fsm.learning_log().len(), 1);
        fsm.handle(Input::ConfigReady(true), 12);
        assert_eq!(fsm.state(), FsmState::Learning);
        fsm.handle(Input::WizardEnd, 13);
        assert_eq!(fsm.state(), FsmState::Idle);
    }

    #[test]
    fn pause_holds_until_resumed() {
        let mut fsm = ready_fsm();
        fsm.handle(Input::SetDisabled(true), 1);
        fsm.handle(Input::ConfigReady(true), 2);
        assert_eq!(fsm.state(), FsmState::Disabled);
        assert_eq!(states(&fsm.handle(Input::SetDisabled(false), 3)), ["idle"]);
    }

    #[test]
    fn tracker_events_are_logged_with_honest_return_interpretations() {
        let mut fsm = ready_fsm();
        let left = fsm.handle(
            Input::Tracker(TrackerEvent::GroupLeft {
                absent_for_ms: 1_000,
            }),
            1,
        );
        assert!(matches!(
            left.as_slice(),
            [Action::Event {
                kind: kinds::TRIGGER_GROUP_LEFT,
                ..
            }]
        ));
        let returned = fsm.handle(
            Input::Tracker(TrackerEvent::UnexpectedReturn { gap_ms: 22_000 }),
            2,
        );
        let interpretations = returned.iter().find_map(|action| match action {
            Action::Notify {
                kind: "trigger",
                data,
            } => data["interpretations"].as_array().cloned(),
            _ => None,
        });
        assert_eq!(interpretations.map(|items| items.len()), Some(2));
    }
}
