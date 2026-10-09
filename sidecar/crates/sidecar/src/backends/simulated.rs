//! Simulated backend - the backbone of the simulated verification tier.
//! Models the KVM-1 verified hardware behavior:
//! - two SAC monitors with dialect inputs (G73 15/16, G52plus 7/8)
//! - control matrix: reads/writes only work while the simulated host is the
//!   active input on that monitor (push-away model)
//! - scripted USB scenarios, including a replay of the real KVM-1 fixture.

use super::{Backend, DdcCapability, DisplayInfo, UsbDevice, VcpRead, VcpWriteOutcome};
use kvmflow_core::errors::{CoreError, E_DDC_FAILED, E_DDC_NOT_READABLE, E_DISPLAY_NOT_FOUND};
use serde_json::json;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

pub const G73: &str = "SAC-2763-S:0000000000001";
pub const G52PLUS: &str = "SAC-2466-S:0000000000000";

#[derive(Clone)]
struct SimMonitor {
    edid_id: &'static str,
    label: &'static str,
    here_input: u16,
    #[allow(dead_code)]
    away_input: u16,
    current: u16,
    /// When set, writes to this monitor always fail (failure-path tests).
    fail_writes: bool,
    /// Number of push-away writes after which the monitor finds no picture
    /// on the new input and auto-selects this host again.
    bounces_left: u32,
    bounce_pending: bool,
}

pub struct SimulatedBackend {
    monitors: Vec<SimMonitor>,
    #[allow(dead_code)]
    scenario: String,
}

/// One scenario step: wait `delay_ms`, then the present set becomes `devices`.
pub struct ScenarioStep {
    pub delay_ms: u64,
    pub devices: Vec<UsbDevice>,
}

pub fn switch_group() -> Vec<UsbDevice> {
    vec![
        usb("1a40:0101", "", "USB 2.0 Hub", "Terminus"),
        usb("3837:303c", "fixture-usb-3837-303c-1", "Ace 75", "MCHOSE"),
        usb("373b:10c9", "Љ", "NK mouse NANO dongle", "Compx"),
    ]
}

fn other_devices() -> Vec<UsbDevice> {
    vec![usb("05ac:12a8", "F2KLM123", "iPhone", "Apple Inc.")]
}

pub fn usb(vid_pid: &str, serial: &str, product: &str, vendor: &str) -> UsbDevice {
    UsbDevice {
        key: format!("{vid_pid}:{serial}"),
        vid_pid: vid_pid.to_string(),
        serial: serial.to_string(),
        product: product.to_string(),
        vendor: vendor.to_string(),
    }
}

impl SimulatedBackend {
    pub fn new(scenario: &str) -> Self {
        let fail_g52 = scenario.contains("ddc_fail");
        // A hand-back is the other host returning both monitors at once.
        let handback = scenario.contains("handback");
        let g52_bounces = if scenario.contains("g52_bounce_always") {
            u32::MAX
        } else if scenario.contains("g52_bounce") || handback {
            1
        } else {
            0
        };
        Self {
            monitors: vec![
                SimMonitor {
                    edid_id: G73,
                    label: "G73",
                    here_input: 15,
                    away_input: 16,
                    current: 15,
                    fail_writes: false,
                    bounces_left: u32::from(handback),
                    bounce_pending: false,
                },
                SimMonitor {
                    edid_id: G52PLUS,
                    label: "G52plus",
                    here_input: 7,
                    away_input: 8,
                    current: 7,
                    fail_writes: fail_g52,
                    bounces_left: g52_bounces,
                    bounce_pending: false,
                },
            ],
            scenario: scenario.to_string(),
        }
    }

    fn find_mut(&mut self, edid_id: &str) -> Result<&mut SimMonitor, CoreError> {
        self.monitors
            .iter_mut()
            .find(|m| m.edid_id == edid_id)
            .ok_or_else(|| {
                CoreError::new(
                    E_DISPLAY_NOT_FOUND,
                    format!("no display with edid_id {edid_id}"),
                )
            })
    }
}

impl Backend for SimulatedBackend {
    fn name(&self) -> &'static str {
        "simulated"
    }

    fn simulated(&self) -> bool {
        true
    }

    fn list_displays(&mut self) -> Result<Vec<DisplayInfo>, String> {
        Ok(self
            .monitors
            .iter()
            .enumerate()
            .map(|(i, m)| DisplayInfo {
                edid_id: m.edid_id.to_string(),
                manufacturer: "SAC".into(),
                product_code: if m.edid_id == G73 { 0x2763 } else { 0x2466 },
                model_name: m.label.to_string(),
                serial_string: if m.edid_id == G73 {
                    "0000000000001"
                } else {
                    "0000000000000"
                }
                .to_string(),
                display_index: (i + 1) as u32,
                builtin: false,
                is_main: i == 0,
                ddc: if m.current == m.here_input {
                    DdcCapability::Available { input: m.current }
                } else {
                    DdcCapability::Unavailable {
                        reason: "not_active_input".into(),
                    }
                },
            })
            .collect())
    }

    fn read_input(&mut self, edid_id: &str) -> Result<VcpRead, CoreError> {
        let m = self.find_mut(edid_id)?;
        if m.bounce_pending {
            m.bounce_pending = false;
            m.current = m.here_input;
        }
        if m.current != m.here_input {
            // Control matrix: degraded read when this host is not the active
            // input (KVM-1: reading the "other" side returned 0 / rc error).
            return Err(CoreError::new(
                E_DDC_NOT_READABLE,
                "simulated control matrix: this host is not the active input on this monitor",
            ));
        }
        Ok(VcpRead {
            value: m.current,
            max: 14,
        })
    }

    fn write_input(&mut self, edid_id: &str, value: u16) -> Result<VcpWriteOutcome, CoreError> {
        let m = self.find_mut(edid_id)?;
        if m.fail_writes {
            return Err(CoreError::new(
                E_DDC_FAILED,
                "simulated persistent DDC failure",
            ));
        }
        if m.current != m.here_input {
            return Err(CoreError::new(
                E_DDC_NOT_READABLE,
                "simulated control matrix: push-away only - this host does not hold the picture",
            ));
        }
        let previous = m.current;
        m.current = value;
        if value != m.here_input && m.bounces_left > 0 {
            m.bounces_left -= 1;
            m.bounce_pending = true;
        }
        Ok(VcpWriteOutcome {
            commanded: true,
            previous: Some(previous),
            error: None,
            evidence: json!({ "backend": "simulated", "wrote": value }),
        })
    }
}

/// Scenario timelines. Durations are chosen against the DEFAULT debounce
/// (T_stable 5s, T_absent 10s, T_cooldown 15s) unless a `fast_` prefix is
/// used together with a test config that shrinks the debounce.
pub fn scenario_steps(scenario: &str) -> Vec<ScenarioStep> {
    match scenario {
        "idle" => vec![ScenarioStep {
            delay_ms: 0,
            devices: switch_group(),
        }],
        "clean_roundtrip" | "clean_roundtrip_ddc_fail" => vec![
            ScenarioStep {
                delay_ms: 0,
                devices: group_plus_others(),
            },
            // Leave at t=8s (already stable), stay gone forever: GroupLeft
            // fires at 18s. The receiving side is the OTHER host - not us.
            ScenarioStep {
                delay_ms: 8_000,
                devices: other_devices(),
            },
        ],
        "replug" => vec![
            ScenarioStep {
                delay_ms: 0,
                devices: group_plus_others(),
            },
            // Bad-cable signature: gone 22s, then back (KVM-1 12:11-12:16).
            ScenarioStep {
                delay_ms: 8_000,
                devices: other_devices(),
            },
            ScenarioStep {
                delay_ms: 22_000,
                devices: group_plus_others(),
            },
        ],
        // Same state-machine signature as `replug`, compressed for automated
        // tests that already use the short debounce profile.
        "fast_replug" => vec![
            ScenarioStep {
                delay_ms: 0,
                devices: group_plus_others(),
            },
            ScenarioStep {
                delay_ms: 1_500,
                devices: other_devices(),
            },
            ScenarioStep {
                delay_ms: 2_500,
                devices: group_plus_others(),
            },
        ],
        "switch_back" => vec![
            ScenarioStep {
                delay_ms: 0,
                devices: group_plus_others(),
            },
            ScenarioStep {
                delay_ms: 8_000,
                devices: other_devices(),
            },
            // genuine switch-back after a long stay away
            ScenarioStep {
                delay_ms: 60_000,
                devices: group_plus_others(),
            },
        ],
        "fast_clean_roundtrip"
        | "fast_clean_roundtrip_g52_bounce"
        | "fast_clean_roundtrip_g52_bounce_always"
        | "fast_clean_roundtrip_handback" => vec![
            ScenarioStep {
                delay_ms: 0,
                devices: group_plus_others(),
            },
            ScenarioStep {
                delay_ms: 1_500,
                devices: other_devices(),
            },
        ],
        // Receiving-host path: the process starts while the USB group is on
        // another computer, then observes a stable arrival. Used to verify
        // that v2 runs its correction path only after an actual arrival, not
        // just because the sidecar started.
        "fast_arrival" => vec![
            ScenarioStep {
                delay_ms: 0,
                devices: other_devices(),
            },
            ScenarioStep {
                delay_ms: 1_500,
                devices: group_plus_others(),
            },
        ],
        "bounce" => {
            // Old-switch #1 signature: 7 cycles, ~3.1s dwell / ~2.5s gap.
            let mut steps = Vec::new();
            for _ in 0..7 {
                steps.push(ScenarioStep {
                    delay_ms: 3_100,
                    devices: group_plus_others(),
                });
                steps.push(ScenarioStep {
                    delay_ms: 2_500,
                    devices: other_devices(),
                });
            }
            steps
        }
        _ => Vec::new(),
    }
}

fn group_plus_others() -> Vec<UsbDevice> {
    let mut v = switch_group();
    v.extend(other_devices());
    v
}

/// Play a scenario onto the snapshot channel. Emits an initial snapshot
/// immediately (the poller contract) and then each step after its delay.
pub fn spawn_scenario_player(scenario: &str, tx: Sender<Vec<UsbDevice>>) -> thread::JoinHandle<()> {
    let steps = scenario_steps(scenario);
    thread::spawn(move || {
        for step in steps {
            thread::sleep(Duration::from_millis(step.delay_ms));
            let _ = tx.send(step.devices);
        }
        // After the last step, keep the bus quiet (no further snapshots).
    })
}

/// Replay a KVM-1 probe session fixture (JSONL with usb.connect/disconnect
/// events) as a live snapshot timeline, scaled by `time_scale` (e.g. 0.1
/// makes the 77s bounce fixture run in ~8s while preserving every gap).
pub fn spawn_fixture_replay(
    path: &str,
    time_scale: f64,
    tx: Sender<Vec<UsbDevice>>,
) -> Result<thread::JoinHandle<()>, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read fixture {path}: {e}"))?;
    let mut timeline: Vec<(u64, Vec<UsbDevice>)> = Vec::new();
    let mut present: Vec<UsbDevice> = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(event) = v.get("event").and_then(|e| e.as_str()) else {
            continue;
        };
        if !event.starts_with("usb.") {
            continue;
        }
        let ts = v
            .get("ts")
            .and_then(|t| t.as_str())
            .and_then(parse_rfc3339_ms)
            .unwrap_or_else(|| timeline.last().map(|(t, _)| *t).unwrap_or(0));
        match event {
            "usb.connect" => {
                if let Some(d) = fixture_device(&v) {
                    if !present.iter().any(|p| p.key == d.key) {
                        present.push(d);
                    }
                }
            }
            "usb.disconnect" => {
                if let Some(key) = v.get("key").and_then(|k| k.as_str()) {
                    present.retain(|p| p.key != key);
                }
            }
            _ => continue,
        }
        timeline.push((ts, present.clone()));
    }

    // Collapse consecutive identical snapshots, keep first of each run.
    let mut collapsed: Vec<(u64, Vec<UsbDevice>)> = Vec::new();
    for (ts, snap) in timeline {
        if collapsed.last().map(|(_, s)| *s == snap).unwrap_or(false) {
            continue;
        }
        collapsed.push((ts, snap));
    }

    Ok(thread::spawn(move || {
        let mut prev_ts: Option<u64> = None;
        for (ts, snap) in collapsed {
            if let Some(p) = prev_ts {
                let gap_ms = ts.saturating_sub(p);
                let scaled = (gap_ms as f64 * time_scale).max(1.0) as u64;
                thread::sleep(Duration::from_millis(scaled));
            }
            let _ = tx.send(snap);
            prev_ts = Some(ts);
        }
    }))
}

fn fixture_device(v: &serde_json::Value) -> Option<UsbDevice> {
    let vid_pid = v.get("vid_pid").and_then(|x| x.as_str())?.to_string();
    let key = v.get("key").and_then(|x| x.as_str())?.to_string();
    let serial = key
        .strip_prefix(&format!("{vid_pid}:"))
        .unwrap_or("")
        .to_string();
    Some(UsbDevice {
        key,
        vid_pid,
        serial,
        product: v
            .get("product")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        vendor: String::new(),
    })
}

fn parse_rfc3339_ms(s: &str) -> Option<u64> {
    // Fixture timestamps look like 2026-09-13T08:11:54.1692318Z (up to 7
    // fractional digits). Only intra-day deltas matter for replay, so parse
    // H:M:S + milliseconds.
    let (main, frac) = match s.split_once('.') {
        Some((m, f)) => (m, f.trim_end_matches('Z')),
        None => (s.trim_end_matches('Z'), ""),
    };
    if main.len() != 19 {
        return None;
    }
    let num = |a: usize, b: usize| main[a..b].parse::<u64>().ok();
    let (_y, _mo, _d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let secs = h * 3600 + mi * 60 + sec;
    let millis: u64 = frac
        .chars()
        .take(3)
        .fold(0, |acc, c| acc * 10 + c.to_digit(10).unwrap_or(0) as u64);
    Some(secs * 1000 + millis)
}
