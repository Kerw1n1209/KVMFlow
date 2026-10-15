//! Multi-device DDC writes: the source fast path, arrival correction,
//! per-monitor retries and the bounce watch that re-sends a target input once.

use super::*;

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

pub(crate) struct BounceWatch {
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

impl Runtime {
    pub(crate) fn run_multi_device_writes(
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
            "note": REPORT_NOTE,
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

    pub(crate) fn start_bounce_watch(
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

    pub(crate) fn check_bounce_watch(&mut self) {
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

    pub(crate) fn handle_multi_device_tracker_event(&mut self, event: &TrackerEvent) {
        if !self.enabled {
            return;
        }
        let Some(config) = self.config.clone() else {
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
                Ok(route) if route.writes.is_empty() => self.emit_event(
                    "switch.push.skipped",
                    Level::Info,
                    json!({
                        "mode": "multi_device",
                        "phase": "source_fast_path",
                        "reason": "no_following_monitors",
                        "target_device_id": route.target_device_id,
                    }),
                ),
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
                Ok(correction) if correction.writes.is_empty() => {}
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

    pub(crate) fn write_with_retry(
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

#[cfg(test)]
mod retry_policy_tests {
    use super::retry_policy_for_phase;
    use kvmflow_core::retry::RetryPolicy;

    #[test]
    pub(crate) fn arrival_correction_waits_longer_than_the_configured_policy() {
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
    pub(crate) fn arrival_correction_never_shrinks_a_larger_configured_policy() {
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
    pub(crate) fn source_fast_path_keeps_the_configured_policy() {
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
