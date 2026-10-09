//! USB trigger-group debounce engine - single copy, shared by both platforms.
//!
//! KVM-1 verified model (2026-09-13):
//! - Initial recognition requires the FULL device group (anchor + >=1
//!   peripheral). Once stable, only the WHOLE group leaving this host is
//!   evidence of a switch; losing any single device (receiver, keyboard, or
//!   the hub itself while others remain) is not. A same-end replug is not a
//!   switch.
//! - macOS delivers duplicate disconnect notifications per device; the engine
//!   is therefore SNAPSHOT-driven (it consumes "which keys are present now",
//!   never edge events), which makes duplicate notifications structurally
//!   impossible to double-count.
//! - Validated parameters (0 false triggers replaying the real bounce
//!   fixture): quorum = anchor + >=1 peripheral, T_stable >= 5s,
//!   T_absent >= 10s, T_cooldown >= 15s.
//! - The bad-cable incident (group gone 22s then back) means a leave that
//!   later returns is surfaced as `UnexpectedReturn` so the UI can offer
//!   recovery guidance; it is not silently ignored.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DebounceParams {
    /// How long the group must be continuously present to count as "stable
    /// with me".
    #[serde(default = "default_t_stable")]
    pub t_stable_ms: u64,
    /// How long the whole group must stay gone before we declare "left for the
    /// other host" (the push-away trigger).
    #[serde(default = "default_t_absent")]
    pub t_absent_ms: u64,
    /// Cooldown after a push before a new trigger is accepted (enforced by
    /// the state machine; carried here so the config has one debounce block).
    #[serde(default = "default_t_cooldown")]
    pub t_cooldown_ms: u64,
    /// Peripherals (besides the anchor) required to initially establish the
    /// group. KVM-1 quorum: anchor + >=1 peripheral.
    #[serde(default = "default_quorum")]
    pub quorum_peripherals: u32,
}

fn default_t_stable() -> u64 {
    5_000
}
fn default_t_absent() -> u64 {
    10_000
}
fn default_t_cooldown() -> u64 {
    15_000
}
fn default_quorum() -> u32 {
    1
}

impl Default for DebounceParams {
    fn default() -> Self {
        Self {
            t_stable_ms: default_t_stable(),
            t_absent_ms: default_t_absent(),
            t_cooldown_ms: default_t_cooldown(),
            quorum_peripherals: default_quorum(),
        }
    }
}

impl DebounceParams {
    pub fn validate(&self) -> Result<(), String> {
        if self.t_stable_ms > 120_000 {
            return Err(format!(
                "t_stable_ms {} outside 0..=120000",
                self.t_stable_ms
            ));
        }
        if self.t_absent_ms > 300_000 {
            return Err(format!(
                "t_absent_ms {} outside 0..=300000",
                self.t_absent_ms
            ));
        }
        if self.t_cooldown_ms > 600_000 {
            return Err(format!(
                "t_cooldown_ms {} outside 0..=600000",
                self.t_cooldown_ms
            ));
        }
        if self.quorum_peripherals < 1 || self.quorum_peripherals > 8 {
            return Err(format!(
                "quorum_peripherals {} outside 1..=8",
                self.quorum_peripherals
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceKey {
    pub vid_pid: String,
    pub serial: String,
}

/// Matching rule (single copy): both anchor and peripherals match on
/// VID:PID. The serial is carried as identity information only - Windows
/// instance-path "serials" are port-dependent and must never break quorum
/// (a false-positive match is guarded by quorum + debounce; a false-negative
/// would silently kill all triggering on one platform).
pub fn matches_anchor(vid_pid: &str, anchor: &crate::config::DevicePattern) -> bool {
    vid_pid.eq_ignore_ascii_case(&anchor.vid_pid)
}

pub fn matches_peripheral(
    vid_pid: &str,
    _serial: &str,
    member: &crate::config::DevicePattern,
) -> bool {
    vid_pid.eq_ignore_ascii_case(&member.vid_pid)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presence {
    /// Group not seen at all yet (startup state).
    Initial,
    /// Group absent; may be an episode worth tracking.
    Absent,
    /// Group present but not yet stable.
    Unstable { since: u64 },
    /// Group present and stable.
    Stable { since: u64 },
    /// Group was Stable, every group device disappeared, inside the T_absent window.
    PendingAbsent { since: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrackerEvent {
    /// Group became present and stayed for T_stable.
    GroupStable { stable_for_ms: u64 },
    /// The group was known to be absent on this host, then became stable.
    /// Unlike the initial startup `GroupStable`, this is evidence that the USB
    /// switch has arrived here and can safely drive the v2 correction path.
    GroupArrived { stable_for_ms: u64 },
    /// Every group device vanished and stayed gone for T_absent -> push-away trigger.
    GroupLeft { absent_for_ms: u64 },
    /// Group came back after a GroupLeft had already fired. From this host's
    /// USB view alone a replug (bad cable / flaky switch) and a genuine
    /// switch-back are indistinguishable - the UI must present BOTH
    /// interpretations and recovery guidance. Monitors may have been pushed
    /// away wrongly in the replug case.
    UnexpectedReturn { gap_ms: u64 },
    /// Group vanished for less than T_absent and returned (bounce absorbed,
    /// no trigger). The stable clock restarts.
    BounceAbsorbed { gap_ms: u64 },
}

pub struct GroupTracker {
    params: DebounceParams,
    anchor: crate::config::DevicePattern,
    members: Vec<crate::config::DevicePattern>,
    presence: Presence,
    /// GroupLeft fired and no stable return has been observed since.
    left_fired: bool,
    /// Set by an absent -> present transition until it has stabilized.
    pending_arrival: bool,
    last_absent_started: Option<u64>,
    /// When the last GroupLeft was emitted; enforces `t_cooldown_ms`.
    last_left_emitted: Option<u64>,
    present_keys: Vec<String>,
}

impl GroupTracker {
    pub fn new(
        anchor: crate::config::DevicePattern,
        members: Vec<crate::config::DevicePattern>,
        params: DebounceParams,
    ) -> Self {
        Self {
            params,
            anchor,
            members,
            presence: Presence::Initial,
            left_fired: false,
            pending_arrival: false,
            last_absent_started: None,
            last_left_emitted: None,
            present_keys: Vec::new(),
        }
    }

    /// Mark the group as departed and emit GroupLeft unless the previous one
    /// fired less than `t_cooldown_ms` ago. A suppressed departure sends no
    /// monitors away, so the later return must not look like an unexpected one.
    fn depart(&mut self, now_ms: u64, absent_for_ms: u64, out: &mut Vec<TrackerEvent>) {
        self.presence = Presence::Absent;
        let cooling_down = self
            .last_left_emitted
            .is_some_and(|at| now_ms.saturating_sub(at) < self.params.t_cooldown_ms);
        if cooling_down {
            self.left_fired = false;
            return;
        }
        self.left_fired = true;
        self.last_left_emitted = Some(now_ms);
        out.push(TrackerEvent::GroupLeft { absent_for_ms });
    }

    pub fn group_present(&self) -> bool {
        matches!(
            self.presence,
            Presence::Unstable { .. } | Presence::Stable { .. } | Presence::PendingAbsent { .. }
        )
    }

    pub fn group_stable(&self) -> bool {
        matches!(self.presence, Presence::Stable { .. })
    }

    pub fn present_keys(&self) -> &[String] {
        &self.present_keys
    }

    pub fn params(&self) -> &DebounceParams {
        &self.params
    }

    /// Feed one USB snapshot. `now_ms` should be monotonic-ish (wall clock is
    /// acceptable; only differences matter). Returns emitted events.
    pub fn update(&mut self, now_ms: u64, present: &[DeviceKey]) -> Vec<TrackerEvent> {
        let matched: Vec<&DeviceKey> = present
            .iter()
            .filter(|d| {
                matches_anchor(&d.vid_pid, &self.anchor)
                    || self
                        .members
                        .iter()
                        .any(|m| matches_peripheral(&d.vid_pid, &d.serial, m))
            })
            .collect();
        let anchor_present = matched
            .iter()
            .any(|d| matches_anchor(&d.vid_pid, &self.anchor));
        let peripherals: usize = matched
            .iter()
            .filter(|d| !matches_anchor(&d.vid_pid, &self.anchor))
            .count();
        let group_now = anchor_present && peripherals as u32 >= self.params.quorum_peripherals;
        self.present_keys = matched
            .iter()
            .map(|d| format!("{}:{}", d.vid_pid, d.serial))
            .collect();

        let mut out = Vec::new();

        // The peripheral quorum is required to establish that this is our USB
        // Switch, but once established the group only counts as departed when
        // NONE of its configured devices remain. A switch press removes the
        // hub and every downstream device together, whereas unplugging one
        // receiver or keyboard leaves the rest enumerated. This also holds when
        // the learned "anchor" is not a hub (a hub can be invisible to the
        // platform enumerator), so a single device loss never looks like a
        // switch.
        let stable_member_present = !matched.is_empty()
            && matches!(
                self.presence,
                Presence::Stable { .. } | Presence::PendingAbsent { .. }
            );

        if group_now {
            match self.presence {
                Presence::Initial => {
                    self.presence = Presence::Unstable { since: now_ms };
                }
                Presence::Absent => {
                    if self.left_fired {
                        // The group "left for the other host" (we may have
                        // pushed monitors away) and is now back on THIS host.
                        // Replug and genuine switch-back look identical here.
                        let gap = self
                            .last_absent_started
                            .map(|s| now_ms.saturating_sub(s))
                            .unwrap_or(0);
                        out.push(TrackerEvent::UnexpectedReturn { gap_ms: gap });
                        self.left_fired = false;
                    }
                    self.pending_arrival = true;
                    self.presence = Presence::Unstable { since: now_ms };
                }
                Presence::Unstable { since } => {
                    if now_ms.saturating_sub(since) >= self.params.t_stable_ms {
                        self.presence = Presence::Stable { since };
                        let stable_for = now_ms.saturating_sub(since);
                        out.push(TrackerEvent::GroupStable {
                            stable_for_ms: stable_for,
                        });
                        if self.pending_arrival {
                            out.push(TrackerEvent::GroupArrived {
                                stable_for_ms: stable_for,
                            });
                            self.pending_arrival = false;
                        }
                    }
                }
                Presence::Stable { .. } => {}
                Presence::PendingAbsent { since } => {
                    let gap = now_ms.saturating_sub(since);
                    out.push(TrackerEvent::BounceAbsorbed { gap_ms: gap });
                    self.presence = Presence::Unstable { since: now_ms };
                    self.last_absent_started = None;
                }
            }
            // A zero stability window means the changed USB snapshot itself
            // is sufficient evidence. Do not wait for the next idle tick.
            if self.params.t_stable_ms == 0 {
                if let Presence::Unstable { since } = self.presence {
                    self.presence = Presence::Stable { since };
                    out.push(TrackerEvent::GroupStable { stable_for_ms: 0 });
                    if self.pending_arrival {
                        out.push(TrackerEvent::GroupArrived { stable_for_ms: 0 });
                        self.pending_arrival = false;
                    }
                }
            }
        } else if stable_member_present {
            match self.presence {
                Presence::Stable { .. } => {}
                Presence::PendingAbsent { since } => {
                    // A group device returned before the absence window
                    // elapsed, even if the rest are still enumerating. Cancel
                    // the departure without requiring the full peripheral
                    // quorum to reappear.
                    let gap = now_ms.saturating_sub(since);
                    out.push(TrackerEvent::BounceAbsorbed { gap_ms: gap });
                    self.presence = Presence::Stable { since: now_ms };
                    self.last_absent_started = None;
                }
                Presence::Initial | Presence::Absent | Presence::Unstable { .. } => {
                    unreachable!("stable_member_present is only true for stable states")
                }
            }
        } else {
            match self.presence {
                Presence::Initial => {
                    self.presence = Presence::Absent;
                }
                Presence::Absent => {}
                Presence::Unstable { since } => {
                    // Never stabilized - flicker during discovery; restart.
                    let _ = since;
                    self.presence = Presence::Absent;
                    self.pending_arrival = false;
                }
                Presence::Stable { .. } => {
                    self.presence = Presence::PendingAbsent { since: now_ms };
                    self.last_absent_started = Some(now_ms);
                }
                Presence::PendingAbsent { since } => {
                    if now_ms.saturating_sub(since) >= self.params.t_absent_ms {
                        self.depart(now_ms, now_ms.saturating_sub(since), &mut out);
                    }
                }
            }
            // Likewise, immediate departure must fire in the same update as
            // the disappearance instead of waiting for the 500 ms idle tick.
            if self.params.t_absent_ms == 0 {
                if let Presence::PendingAbsent { .. } = self.presence {
                    self.depart(now_ms, 0, &mut out);
                }
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DevicePattern;

    const HUB: &str = "1a40:0101";
    const KB: &str = "3837:303c";
    const MOUSE: &str = "373b:10c9";

    fn tracker() -> GroupTracker {
        GroupTracker::new(
            DevicePattern {
                vid_pid: HUB.into(),
                serial: None,
            },
            vec![
                DevicePattern {
                    vid_pid: KB.into(),
                    serial: Some("fixture-usb-3837-303c-1".into()),
                },
                DevicePattern {
                    vid_pid: MOUSE.into(),
                    serial: Some("Љ".into()),
                },
            ],
            DebounceParams::default(),
        )
    }

    fn full(_t: u64) -> Vec<DeviceKey> {
        vec![
            DeviceKey {
                vid_pid: HUB.into(),
                serial: String::new(),
            },
            DeviceKey {
                vid_pid: KB.into(),
                serial: "fixture-usb-3837-303c-1".into(),
            },
            DeviceKey {
                vid_pid: MOUSE.into(),
                serial: "Љ".into(),
            },
        ]
    }

    fn hub_only(_t: u64) -> Vec<DeviceKey> {
        vec![DeviceKey {
            vid_pid: HUB.into(),
            serial: String::new(),
        }]
    }

    #[test]
    fn stable_then_long_absence_fires_group_left() {
        let mut tr = tracker();
        assert!(tr.update(0, &full(0)).is_empty());
        assert!(tr.update(1_000, &full(1_000)).is_empty());
        let evs = tr.update(5_000, &full(5_000));
        assert_eq!(
            evs,
            vec![TrackerEvent::GroupStable {
                stable_for_ms: 5_000
            }]
        );
        // gone
        assert!(tr.update(6_000, &[]).is_empty());
        assert!(tr.update(10_000, &[]).is_empty());
        let evs = tr.update(16_000, &[]);
        assert_eq!(
            evs,
            vec![TrackerEvent::GroupLeft {
                absent_for_ms: 10_000
            }]
        );
    }

    #[test]
    fn return_after_left_is_unexpected_return() {
        let mut tr = tracker();
        tr.update(0, &full(0));
        tr.update(5_000, &full(5_000));
        tr.update(6_000, &[]);
        tr.update(16_000, &[]); // GroupLeft
                                // comes back 22s after leaving (bad-cable signature)
        let evs = tr.update(28_000, &full(28_000));
        assert_eq!(evs, vec![TrackerEvent::UnexpectedReturn { gap_ms: 22_000 }]);
        // and then re-stabilizes as a normal arrival. It is also an arrival
        // for v2 correction (the UI retains the bad-cable warning above).
        let evs = tr.update(33_000, &full(33_000));
        assert_eq!(
            evs,
            vec![
                TrackerEvent::GroupStable {
                    stable_for_ms: 5_000
                },
                TrackerEvent::GroupArrived {
                    stable_for_ms: 5_000
                },
            ]
        );
    }

    #[test]
    fn short_bounce_is_absorbed() {
        let mut tr = tracker();
        tr.update(0, &full(0));
        tr.update(5_000, &full(5_000));
        tr.update(6_000, &[]);
        let evs = tr.update(9_000, &full(9_000)); // 3s gap < T_absent
        assert_eq!(evs, vec![TrackerEvent::BounceAbsorbed { gap_ms: 3_000 }]);
        // stable clock restarted
        assert!(tr.update(12_000, &full(12_000)).is_empty());
        let evs = tr.update(14_000, &full(14_000));
        assert_eq!(
            evs,
            vec![TrackerEvent::GroupStable {
                stable_for_ms: 5_000
            }]
        );
    }

    #[test]
    fn arrival_after_clean_switch_is_group_stable() {
        // Receiving host: never held the group, sees it appear after the
        // press on the other side. This is distinct from initial startup.
        let mut tr = tracker();
        tr.update(0, &[]);
        tr.update(1_000, &[]);
        assert!(tr.update(2_000, &full(2_000)).is_empty());
        let evs = tr.update(7_000, &full(7_000));
        assert_eq!(
            evs,
            vec![
                TrackerEvent::GroupStable {
                    stable_for_ms: 5_000
                },
                TrackerEvent::GroupArrived {
                    stable_for_ms: 5_000
                },
            ]
        );
        // full cycle on the pushing side: stable -> left -> back later
        let mut tr3 = tracker();
        tr3.update(0, &full(0));
        tr3.update(5_000, &full(5_000));
        tr3.update(6_000, &[]);
        assert_eq!(
            tr3.update(16_000, &[]),
            vec![TrackerEvent::GroupLeft {
                absent_for_ms: 10_000
            }]
        );
        // switch-back: group returns; USB view = UnexpectedReturn shape
        assert_eq!(
            tr3.update(100_000, &full(100_000)),
            vec![TrackerEvent::UnexpectedReturn { gap_ms: 94_000 }]
        );
        let evs = tr3.update(105_000, &full(105_000));
        assert_eq!(
            evs,
            vec![
                TrackerEvent::GroupStable {
                    stable_for_ms: 5_000
                },
                TrackerEvent::GroupArrived {
                    stable_for_ms: 5_000
                },
            ]
        );
    }

    #[test]
    fn hub_alone_never_counts_as_group() {
        let mut tr = tracker();
        tr.update(0, &hub_only(0));
        tr.update(10_000, &hub_only(10_000));
        tr.update(60_000, &hub_only(60_000));
        assert!(!tr.group_present());
        // and leaving again must NOT fire
        tr.update(61_000, &[]);
        assert!(tr.update(200_000, &[]).is_empty());
    }

    #[test]
    fn duplicate_snapshots_are_inert() {
        // Snapshot-driven: calling update with the same present set many
        // times (the macOS duplicate-notification case, collapsed by the
        // backend into identical snapshots) emits nothing extra.
        let mut tr = tracker();
        for t in (0..=5_000).step_by(100) {
            tr.update(t, &full(t as u64));
        }
        assert!(tr.group_stable());
    }

    #[test]
    fn peripheral_without_serial_still_matches() {
        // Windows may not surface the serial; quorum must survive.
        let tr = GroupTracker::new(
            DevicePattern {
                vid_pid: HUB.into(),
                serial: None,
            },
            vec![DevicePattern {
                vid_pid: KB.into(),
                serial: Some("fixture-usb-3837-303c-1".into()),
            }],
            DebounceParams::default(),
        );
        let mut tr = tr;
        let present = vec![
            DeviceKey {
                vid_pid: HUB.into(),
                serial: String::new(),
            },
            DeviceKey {
                vid_pid: KB.into(),
                serial: "OTHER-INSTANCE".into(),
            },
        ];
        tr.update(0, &present);
        tr.update(5_000, &present);
        assert!(tr.group_stable());
    }

    #[test]
    fn params_validation_bounds() {
        assert!(DebounceParams::default().validate().is_ok());
        assert!(DebounceParams {
            t_stable_ms: 0,
            t_absent_ms: 0,
            ..Default::default()
        }
        .validate()
        .is_ok());
        assert!(DebounceParams {
            quorum_peripherals: 0,
            ..Default::default()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn zero_windows_emit_in_the_same_snapshot_update() {
        let mut tr = GroupTracker::new(
            DevicePattern {
                vid_pid: HUB.into(),
                serial: None,
            },
            vec![DevicePattern {
                vid_pid: KB.into(),
                serial: None,
            }],
            DebounceParams {
                t_stable_ms: 0,
                t_absent_ms: 0,
                t_cooldown_ms: 0,
                quorum_peripherals: 1,
            },
        );
        let present = vec![
            DeviceKey {
                vid_pid: HUB.into(),
                serial: String::new(),
            },
            DeviceKey {
                vid_pid: KB.into(),
                serial: String::new(),
            },
        ];
        assert_eq!(
            tr.update(0, &present),
            vec![TrackerEvent::GroupStable { stable_for_ms: 0 }]
        );
        assert_eq!(
            tr.update(1, &[]),
            vec![TrackerEvent::GroupLeft { absent_for_ms: 0 }]
        );
    }

    /// Real Windows config: the platform enumerator never reported the hub, so
    /// the learned "anchor" is the mouse receiver and the only member is the
    /// keyboard, with zero debounce windows.
    fn receiver_anchored_tracker() -> GroupTracker {
        GroupTracker::new(
            DevicePattern {
                vid_pid: MOUSE.into(),
                serial: None,
            },
            vec![DevicePattern {
                vid_pid: KB.into(),
                serial: Some("fixture-usb-3837-303c-1".into()),
            }],
            DebounceParams {
                t_stable_ms: 0,
                t_absent_ms: 0,
                t_cooldown_ms: 1_000,
                quorum_peripherals: 1,
            },
        )
    }

    fn key(vid_pid: &str) -> DeviceKey {
        DeviceKey {
            vid_pid: vid_pid.into(),
            serial: String::new(),
        }
    }

    #[test]
    fn unplugging_one_device_never_triggers_when_anchor_is_not_a_hub() {
        let mut tr = receiver_anchored_tracker();
        let both = [key(MOUSE), key(KB)];
        assert_eq!(
            tr.update(0, &both),
            vec![TrackerEvent::GroupStable { stable_for_ms: 0 }]
        );

        // Receiver (the anchor) unplugged: the keyboard is still enumerated.
        assert!(tr.update(1_000, &[key(KB)]).is_empty());
        assert!(tr.update(60_000, &[key(KB)]).is_empty());
        // Receiver replugged: nothing to absorb or report.
        assert!(tr.update(61_000, &both).is_empty());

        // Keyboard unplugged: the receiver is still enumerated.
        assert!(tr.update(70_000, &[key(MOUSE)]).is_empty());
        assert!(tr.update(130_000, &[key(MOUSE)]).is_empty());
        assert!(tr.update(131_000, &both).is_empty());
    }

    #[test]
    fn whole_group_leaving_still_triggers_when_anchor_is_not_a_hub() {
        let both = [key(MOUSE), key(KB)];

        // A real switch press removes every group device in the same snapshot.
        let mut tr = receiver_anchored_tracker();
        tr.update(0, &both);
        assert_eq!(
            tr.update(1_000, &[]),
            vec![TrackerEvent::GroupLeft { absent_for_ms: 0 }]
        );

        // Staggered departure triggers once, when the last device is gone.
        let mut tr = receiver_anchored_tracker();
        tr.update(0, &both);
        assert!(tr.update(1_000, &[key(KB)]).is_empty());
        assert_eq!(
            tr.update(1_250, &[]),
            vec![TrackerEvent::GroupLeft { absent_for_ms: 0 }]
        );
    }
}
