//! KVM-1 real-event replay against the debounce engine (in-process, fast)
//! and through the sidecar binary (fixture replay mode, end to end).
//!
//! Fixture: probes/tests/fixtures/usb-switch-bounce-20260913.jsonl - the
//! 2026-09-13 capture of USB Sharing Switch #1 (bad contact): 67 events /
//! 77.1s, 7 bounce cycles, single physical press. Expected with the
//! validated parameters (quorum, T_stable>=5s, T_absent>=10s,
//! T_cooldown>=15s): ZERO GroupLeft emissions - the spec the debounce
//! engine must keep satisfying (KVM-1 final report §7-4).

use kvmflow_core::config::DevicePattern;
use kvmflow_core::debounce::{DebounceParams, DeviceKey, GroupTracker, TrackerEvent};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

const FIXTURE: &str = "../../../probes/tests/fixtures/usb-switch-bounce-20260913.jsonl";

/// Old switch group: hub 067b:2586 + composite 001f:0b26 (fixture keys).
fn old_switch_tracker(params: DebounceParams) -> GroupTracker {
    GroupTracker::new(
        DevicePattern {
            vid_pid: "067b:2586".into(),
            serial: None,
        },
        vec![DevicePattern {
            vid_pid: "001f:0b26".into(),
            serial: Some("fixture-composite-1".into()),
        }],
        params,
    )
}

fn parse_hms_ms(ts: &str) -> Option<u64> {
    let (main, frac) = match ts.split_once('.') {
        Some((m, f)) => (m, f.trim_end_matches('Z')),
        None => (ts.trim_end_matches('Z'), ""),
    };
    if main.len() != 19 {
        return None;
    }
    let h: u64 = main[11..13].parse().ok()?;
    let mi: u64 = main[14..16].parse().ok()?;
    let s: u64 = main[17..19].parse().ok()?;
    let millis = frac
        .chars()
        .take(3)
        .fold(0u64, |a, c| a * 10 + c.to_digit(10).unwrap_or(0) as u64);
    Some(((h * 3600 + mi * 60 + s) * 1000) + millis)
}

struct Snapshot {
    ts_ms: u64,
    keys: Vec<DeviceKey>,
}

fn fixture_snapshots() -> Vec<Snapshot> {
    let f = std::fs::File::open(FIXTURE).expect("fixture file");
    let mut present: Vec<DeviceKey> = Vec::new();
    let mut out: Vec<Snapshot> = Vec::new();
    for line in BufReader::new(f).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(event) = v["event"].as_str() else {
            continue;
        };
        if !event.starts_with("usb.") {
            continue;
        }
        let Some(ts) = v["ts"].as_str().and_then(parse_hms_ms) else {
            continue;
        };
        match event {
            "usb.connect" => {
                if let Some(key) = v["key"].as_str() {
                    let dk = key_to_device(key);
                    if !present
                        .iter()
                        .any(|d| d.vid_pid == dk.vid_pid && d.serial == dk.serial)
                    {
                        present.push(dk);
                    }
                }
            }
            "usb.disconnect" => {
                if let Some(key) = v["key"].as_str() {
                    let dk = key_to_device(key);
                    present.retain(|d| !(d.vid_pid == dk.vid_pid && d.serial == dk.serial));
                }
            }
            _ => {}
        }
        out.push(Snapshot {
            ts_ms: ts,
            keys: present.clone(),
        });
    }
    // collapse consecutive identical snapshots
    let mut collapsed: Vec<Snapshot> = Vec::new();
    for s in out {
        if collapsed.last().map(|p| p.keys == s.keys).unwrap_or(false) {
            continue;
        }
        collapsed.push(s);
    }
    collapsed
}

fn key_to_device(key: &str) -> DeviceKey {
    // key = "vid:pid:serial" with variable serial content
    let mut parts = key.splitn(3, ':');
    let vid = parts.next().unwrap_or("");
    let pid = parts.next().unwrap_or("");
    let serial = parts.next().unwrap_or("");
    DeviceKey {
        vid_pid: format!("{vid}:{pid}"),
        serial: serial.to_string(),
    }
}

fn drive(tracker: &mut GroupTracker, snapshots: &[Snapshot], tick_ms: u64) -> Vec<TrackerEvent> {
    // Walk time from the first to the last snapshot in tick_ms steps, feeding
    // the snapshot active at each step - mirrors the sidecar runtime, which
    // pumps the tracker on a 500ms tick with the current bus state, not only
    // when a snapshot changes.
    let mut out = Vec::new();
    let Some(first) = snapshots.first() else {
        return out;
    };
    let mut t = first.ts_ms;
    let mut idx = 0usize;
    loop {
        while idx + 1 < snapshots.len() && snapshots[idx + 1].ts_ms <= t {
            idx += 1;
        }
        out.extend(tracker.update(t, &snapshots[idx].keys));
        let Some(last) = snapshots.last() else { break };
        if t >= last.ts_ms {
            break;
        }
        t += tick_ms;
        if t > last.ts_ms {
            t = last.ts_ms;
        }
    }
    out
}

#[test]
fn bounce_fixture_replays_with_zero_false_triggers() {
    let mut tracker = old_switch_tracker(DebounceParams::default());
    let snapshots = fixture_snapshots();
    assert!(
        snapshots.len() > 10,
        "fixture loaded with real events (got {})",
        snapshots.len()
    );
    let mut lefts = 0;
    for ev in drive(&mut tracker, &snapshots, 500) {
        if matches!(ev, TrackerEvent::GroupLeft { .. }) {
            lefts += 1;
        }
    }
    assert_eq!(
        lefts, 0,
        "validated parameters must yield zero false triggers on the real bounce capture"
    );
}

#[test]
fn bounce_fixture_would_trigger_with_degenerate_params() {
    // Control for the test above: with tiny debounce the same capture DOES
    // produce triggers - proving the zero above comes from the parameters,
    // not from the engine ignoring the fixture.
    let mut tracker = old_switch_tracker(DebounceParams {
        t_stable_ms: 500,
        t_absent_ms: 500,
        t_cooldown_ms: 0,
        quorum_peripherals: 1,
    });
    let snapshots = fixture_snapshots();
    let mut lefts = 0;
    for ev in drive(&mut tracker, &snapshots, 500) {
        if matches!(ev, TrackerEvent::GroupLeft { .. }) {
            lefts += 1;
        }
    }
    assert!(
        lefts >= 1,
        "degenerate params must fire on the bounce capture (got {lefts})"
    );
}

/// The app ships immediate departure with a 1s cooldown. The capture is a
/// bad-contact bounce, so triggers are expected, but they must never come
/// faster than the cooldown allows.
#[test]
fn bounce_fixture_with_shipped_params_respects_cooldown() {
    let mut tracker = old_switch_tracker(DebounceParams {
        t_stable_ms: 0,
        t_absent_ms: 0,
        t_cooldown_ms: 1_000,
        quorum_peripherals: 1,
    });
    let snapshots = fixture_snapshots();
    let mut left_at = Vec::new();
    let Some(first) = snapshots.first() else {
        panic!("fixture empty")
    };
    let mut t = first.ts_ms;
    let mut idx = 0usize;
    let last = snapshots.last().unwrap().ts_ms;
    loop {
        while idx + 1 < snapshots.len() && snapshots[idx + 1].ts_ms <= t {
            idx += 1;
        }
        for ev in tracker.update(t, &snapshots[idx].keys) {
            if matches!(ev, TrackerEvent::GroupLeft { .. }) {
                left_at.push(t);
            }
        }
        if t >= last {
            break;
        }
        t = (t + 250).min(last);
    }
    for pair in left_at.windows(2) {
        assert!(
            pair[1] - pair[0] >= 1_000,
            "triggers {pair:?} closer than the cooldown"
        );
    }
}

/// End to end: the sidecar binary replays the real fixture at 20x speed
/// through the simulated USB source; armed with a matching profile and the
/// validated (scaled-consistently) debounce it must NOT emit switch.report.
#[test]
fn sidecar_fixture_replay_no_false_switch_report() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture_abs = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(FIXTURE)
        .canonicalize()
        .expect("fixture path");
    let fixture_str = fixture_abs.to_str().unwrap().to_string();

    let mut child = Command::new(env!("CARGO_BIN_EXE_kvmflow-sidecar"))
        .args([
            "--backend",
            "simulated",
            "--fixture",
            &fixture_str,
            "--fixture-scale",
            "0.2",
            "--config",
            tmp.path().join("config.json").to_str().unwrap(),
            "--log-dir",
            tmp.path().join("logs").to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sidecar");

    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    {
        let stdout = child.stdout.take().unwrap();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    let _ = tx.send(v);
                }
            }
        });
    }

    // ready
    let mut got_ready = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(v) if v["notification"].as_str() == Some("ready") => {
                got_ready = true;
                break;
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    assert!(got_ready, "sidecar became ready");

    // arm with the OLD switch profile and the validated default debounce
    // (fixture replay runs at 0.2x duration but the debounce constants stay
    // real-time; the capture's max gap is ~6.3s < T_absent 10s, so scaled
    // gaps of ~1.3s are far below it - no trigger expected either way).
    let cfg = serde_json::json!({
        "schemaVersion": 1,
        "monitors": [
            {"edidId": "SAC-2763-S:0000000000001", "hereInput": 15, "awayInput": 16, "awayInputSource": "heuristic_prefill"}
        ],
        "trigger": {
            "anchor": {"vidPid": "067b:2586"},
            "members": [{"vidPid": "001f:0b26", "serial": "fixture-composite-1"}],
            "debounce": {"tStableMs": 5000, "tAbsentMs": 10000, "tCooldownMs": 15000}
        }
    });
    let req = format!(
        r#"{{"v":1,"id":1,"method":"config.set","params":{{"config":{}}}}}"#,
        cfg
    );
    {
        let stdin = child.stdin.as_mut().unwrap();
        stdin.write_all(req.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    // Watch the full replay (77.1s * 0.2 = ~16s) and assert no switch.report.
    let mut saw_report = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(v) => {
                if v["notification"].as_str() == Some("switch.report") {
                    saw_report = true;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(_) => continue,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        !saw_report,
        "no false switch.report during the real bounce fixture replay"
    );
}
