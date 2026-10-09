//! KVM-1 real-event replay tests against the debounce engine and the
//! sidecar binary. The bounce fixture is the actual 2026-09-13 capture
//! (probes/tests/fixtures/usb-switch-bounce-20260913.jsonl, commit ab25c61);
//! the clean round-trip and bad-cable timelines are digitized from the
//! archived KVM-1 report (12:40:30-33 window, 12:11-12:16 window).

use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::process::ChildStdin;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

struct Client {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    /// Messages seen while waiting for something else; re-checked first so
    /// notifications that precede their trigger's response are not lost.
    held: Vec<Value>,
}

impl Client {
    fn spawn(extra_args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kvmflow-sidecar"))
            .args(extra_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sidecar");
        let stdout = child.stdout.take().unwrap();
        let stdin = child.stdin.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    let _ = tx.send(v);
                }
            }
        });
        Self {
            child,
            stdin,
            rx,
            held: Vec::new(),
        }
    }

    fn send(&mut self, line: &str) {
        self.stdin.write_all(line.as_bytes()).unwrap();
        self.stdin.write_all(b"\n").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Scan the notification/response stream until `pred` matches or timeout.
    /// Non-matching messages are retained and re-checked by later calls.
    fn wait_for(&mut self, pred: impl Fn(&Value) -> bool, timeout: Duration) -> Option<Value> {
        if let Some(i) = self.held.iter().position(|v| pred(v)) {
            return Some(self.held.remove(i));
        }
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            match self.rx.recv_timeout(Duration::from_millis(200)) {
                Ok(v) if pred(&v) => return Some(v),
                Ok(v) => self.held.push(v),
                Err(RecvTimeoutError::Disconnected) => return None,
                Err(RecvTimeoutError::Timeout) => continue,
            }
        }
        None
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Schema v2 persists, uses the synchronized physical route on departure, and
/// never falls through to the legacy v1 manual-push path.
#[test]
fn e2e_v2_config_persists_and_blocks_legacy_manual_push() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config.json");
    let log_dir = tmp.path().join("logs");
    let v2 = serde_json::json!({
        "schema_version": 2,
        "local_device": {
            "device_id": "mac-work",
            "host_label": "工作 Mac",
            "monitors": [
                {"fingerprint": "SAC-2763-S:0000000000001", "label": "G73", "local_input": 15, "source": "learned_active_read"},
                {"fingerprint": "SAC-2466-S:0000000000000", "label": "G52plus", "local_input": 7, "source": "learned_active_read"}
            ]
        },
        "switch_group": {
            "group_id": "desk",
            "revision": 4,
            "devices": [
                {"device_id": "mac-work", "name": "工作 Mac", "port_index": 1, "monitors": [
                    {"fingerprint": "SAC-2763-S:0000000000001", "label": "G73", "local_input": 15},
                    {"fingerprint": "SAC-2466-S:0000000000000", "label": "G52plus", "local_input": 7}
                ]},
                {"device_id": "windows", "name": "Windows", "port_index": 2, "monitors": [
                    {"fingerprint": "SAC-2763-S:0000000000001", "label": "G73", "local_input": 17},
                    {"fingerprint": "SAC-2466-S:0000000000000", "label": "G52plus", "local_input": 8}
                ]}
            ]
        },
        "trigger": {
            "anchor": {"vid_pid": "1a40:0101"},
            "members": [{"vid_pid": "3837:303c"}],
            "debounce": {"t_stable_ms": 500, "t_absent_ms": 1000, "t_cooldown_ms": 1500, "quorum_peripherals": 1}
        },
        "advanced": {"ddc_retry": {"attempts": 1, "delay_ms": 10}, "usb_poll_ms": 100}
    });

    {
        let mut client = Client::spawn(&[
            "--backend",
            "simulated",
            "--scenario",
            "fast_clean_roundtrip",
            "--config",
            config.to_str().unwrap(),
            "--log-dir",
            log_dir.to_str().unwrap(),
        ]);
        assert!(client
            .wait_for(
                |value| value["notification"] == "ready",
                Duration::from_secs(5)
            )
            .is_some());
        client.send(&format!(
            r#"{{"v":1,"id":1,"method":"config.set","params":{{"config":{v2}}}}}"#
        ));
        let saved = client
            .wait_for(|value| value["id"] == 1, Duration::from_secs(5))
            .expect("v2 config.set");
        assert_eq!(saved["ok"], true, "v2 config accepted: {saved}");
        assert_eq!(saved["result"]["mode"], "multi_device_armed");

        client.send(r#"{"v":1,"id":2,"method":"state.get","params":{}}"#);
        let state = client
            .wait_for(|value| value["id"] == 2, Duration::from_secs(5))
            .expect("v2 state.get");
        assert_eq!(state["result"]["configMode"], "multi_device_armed");
        assert_eq!(state["result"]["groupReady"], true);

        let report = client
            .wait_for(
                |value| {
                    value["notification"] == "switch.report"
                        && value["data"]["phase"] == "source_fast_path"
                },
                Duration::from_secs(6),
            )
            .expect("v2 source fast-path report");
        assert_eq!(report["data"]["target_device_id"], "windows");
        let per_monitor = report["data"]["per_monitor"]
            .as_array()
            .expect("monitor outcomes");
        assert_eq!(per_monitor.len(), 2);
        assert!(per_monitor
            .iter()
            .all(|outcome| outcome["commanded"] == true));
        assert!(per_monitor
            .iter()
            .all(|outcome| outcome["requested"].is_number()));

        client.send(r#"{"v":1,"id":3,"method":"switch.pushNow","params":{"reason":"must_not_use_v1_away_input"}}"#);
        let push = client
            .wait_for(|value| value["id"] == 3, Duration::from_secs(5))
            .expect("v2 push response");
        assert_eq!(push["ok"], false);
        assert!(push["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("physical USB trigger"));
    }

    let mut reloaded = Client::spawn(&[
        "--backend",
        "simulated",
        "--scenario",
        "idle",
        "--config",
        config.to_str().unwrap(),
        "--log-dir",
        log_dir.to_str().unwrap(),
    ]);
    assert!(reloaded
        .wait_for(
            |value| value["notification"] == "ready",
            Duration::from_secs(5)
        )
        .is_some());
    reloaded.send(r#"{"v":1,"id":4,"method":"config.get","params":{}}"#);
    let loaded = reloaded
        .wait_for(|value| value["id"] == 4, Duration::from_secs(5))
        .expect("v2 config.get after restart");
    assert_eq!(loaded["result"]["schema_version"], 2);
    assert_eq!(loaded["result"]["local_device"]["device_id"], "mac-work");
}

fn two_monitor_source_config() -> Value {
    serde_json::json!({
        "schema_version": 2,
        "local_device": {
            "device_id": "mac-work",
            "host_label": "工作 Mac",
            "monitors": [
                {"fingerprint": "SAC-2763-S:0000000000001", "label": "G73", "local_input": 15, "source": "learned_active_read"},
                {"fingerprint": "SAC-2466-S:0000000000000", "label": "G52plus", "local_input": 7, "source": "learned_active_read"}
            ]
        },
        "switch_group": {
            "group_id": "desk",
            "revision": 4,
            "devices": [
                {"device_id": "mac-work", "name": "工作 Mac", "port_index": 1, "monitors": [
                    {"fingerprint": "SAC-2763-S:0000000000001", "label": "G73", "local_input": 15},
                    {"fingerprint": "SAC-2466-S:0000000000000", "label": "G52plus", "local_input": 7}
                ]},
                {"device_id": "windows", "name": "Windows", "port_index": 2, "monitors": [
                    {"fingerprint": "SAC-2763-S:0000000000001", "label": "G73", "local_input": 16},
                    {"fingerprint": "SAC-2466-S:0000000000000", "label": "G52plus", "local_input": 8}
                ]}
            ]
        },
        "trigger": {
            "anchor": {"vid_pid": "1a40:0101"},
            "members": [{"vid_pid": "3837:303c"}],
            "debounce": {"t_stable_ms": 500, "t_absent_ms": 1000, "t_cooldown_ms": 1500, "quorum_peripherals": 1}
        },
        "advanced": {"ddc_retry": {"attempts": 1, "delay_ms": 10}, "usb_poll_ms": 100}
    })
}

/// Collects bounce-handling events until the watch window has certainly
/// closed, so a missing or extra re-send is observable.
fn bounce_events(scenario: &str, pause_after_push: bool) -> Vec<Value> {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config.json");
    let log_dir = tmp.path().join("logs");
    let mut client = Client::spawn(&[
        "--backend",
        "simulated",
        "--scenario",
        scenario,
        "--config",
        config.to_str().unwrap(),
        "--log-dir",
        log_dir.to_str().unwrap(),
    ]);
    assert!(client
        .wait_for(|v| v["notification"] == "ready", Duration::from_secs(5))
        .is_some());
    let v2 = two_monitor_source_config();
    client.send(&format!(
        r#"{{"v":1,"id":1,"method":"config.set","params":{{"config":{v2}}}}}"#
    ));
    assert_eq!(
        client
            .wait_for(|v| v["id"] == 1, Duration::from_secs(5))
            .unwrap()["ok"],
        true
    );
    client
        .wait_for(
            |v| v["notification"] == "switch.report" && v["data"]["phase"] == "source_fast_path",
            Duration::from_secs(6),
        )
        .expect("source fast-path report");
    if pause_after_push {
        client.send(r#"{"v":1,"id":2,"method":"app.setEnabled","params":{"enabled":false}}"#);
        let paused = client
            .wait_for(|value| value["id"] == 2, Duration::from_secs(5))
            .expect("pause response");
        assert_eq!(paused["result"]["enabled"], false);
    }
    let is_bounce = |v: &Value| {
        v["notification"] == "event"
            && v["data"]["kind"]
                .as_str()
                .is_some_and(|kind| kind.starts_with("switch.monitor.bounce_"))
    };
    let mut events = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(14);
    while std::time::Instant::now() < deadline {
        if let Some(event) = client.wait_for(is_bounce, Duration::from_millis(500)) {
            events.push(event["data"].clone());
        }
    }
    events
}

#[test]
fn e2e_source_resends_once_when_a_monitor_bounces_back() {
    let events = bounce_events("fast_clean_roundtrip_g52_bounce", false);
    assert_eq!(events.len(), 1, "exactly one re-send: {events:?}");
    assert_eq!(events[0]["kind"], "switch.monitor.bounce_resent");
    assert_eq!(
        events[0]["fields"]["fingerprint"],
        "SAC-2466-S:0000000000000"
    );
    assert_eq!(events[0]["fields"]["requested"], 8);
    assert_eq!(events[0]["fields"]["observed"], 7);
    assert_eq!(events[0]["fields"]["commanded"], true);
}

#[test]
fn e2e_source_gives_up_after_a_second_bounce() {
    let events = bounce_events("fast_clean_roundtrip_g52_bounce_always", false);
    let kinds: Vec<&str> = events.iter().filter_map(|e| e["kind"].as_str()).collect();
    assert_eq!(
        kinds,
        [
            "switch.monitor.bounce_resent",
            "switch.monitor.bounce_gave_up"
        ],
        "one re-send, then no further writes: {events:?}"
    );
}

#[test]
fn e2e_source_leaves_monitors_alone_when_the_other_host_hands_back() {
    let events = bounce_events("fast_clean_roundtrip_handback", false);
    let kinds: Vec<&str> = events.iter().filter_map(|e| e["kind"].as_str()).collect();
    assert_eq!(
        kinds,
        ["switch.monitor.bounce_ignored"],
        "no re-send on hand-back: {events:?}"
    );
    assert_eq!(events[0]["fields"]["reason"], "all_monitors_returned");
}

#[test]
fn e2e_source_does_not_resend_when_monitors_stay_switched() {
    assert!(bounce_events("fast_clean_roundtrip", false).is_empty());
}

#[test]
fn e2e_pause_cancels_pending_monitor_resends() {
    assert!(bounce_events("fast_clean_roundtrip_g52_bounce", true).is_empty());
}

/// A first-device profile is intentionally useful before any peer or USB
/// trigger exists. It persists as setup data, but the real sidecar must never
/// turn that into a watcher or an automatic DDC write.
#[test]
fn e2e_v2_local_profile_waits_for_pairing_and_usb_calibration() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config.json");
    let log_dir = tmp.path().join("logs");
    let local_profile = serde_json::json!({
        "schema_version": 2,
        "local_device": {
            "device_id": "pending-account-device",
            "host_label": "这台 Mac",
            "monitors": [
                {"fingerprint": "SAC-2763-S:0000000000001", "label": "G73", "local_input": 15, "source": "learned_active_read"}
            ]
        },
        "switch_group": null,
        "trigger": {
            "anchor": {"vid_pid": ""}, "members": [],
            "debounce": {"t_stable_ms": 500, "t_absent_ms": 1000, "t_cooldown_ms": 1500, "quorum_peripherals": 1}
        },
        "advanced": {"ddc_retry": {"attempts": 1, "delay_ms": 10}, "usb_poll_ms": 100}
    });
    let mut client = Client::spawn(&[
        "--backend",
        "simulated",
        "--scenario",
        "fast_clean_roundtrip",
        "--config",
        config.to_str().unwrap(),
        "--log-dir",
        log_dir.to_str().unwrap(),
    ]);
    assert!(client
        .wait_for(
            |value| value["notification"] == "ready",
            Duration::from_secs(5)
        )
        .is_some());
    client.send(&format!(
        r#"{{"v":1,"id":1,"method":"config.set","params":{{"config":{local_profile}}}}}"#
    ));
    let saved = client
        .wait_for(|value| value["id"] == 1, Duration::from_secs(5))
        .expect("local profile config.set");
    assert_eq!(saved["ok"], true);
    assert_eq!(saved["result"]["mode"], "multi_device_setup");

    client.send(r#"{"v":1,"id":2,"method":"state.get","params":{}}"#);
    let state = client
        .wait_for(|value| value["id"] == 2, Duration::from_secs(5))
        .expect("setup state");
    assert_eq!(state["result"]["configMode"], "multi_device_setup");
    assert_eq!(state["result"]["groupReady"], false);

    client.send(r#"{"v":1,"id":3,"method":"usb.watch.start","params":{}}"#);
    let watch = client
        .wait_for(|value| value["id"] == 3, Duration::from_secs(5))
        .expect("watch response");
    assert_eq!(watch["ok"], false);
    assert_eq!(watch["error"]["code"], "E_TRIGGER_NOT_ARMED");

    client.send(r#"{"v":1,"id":4,"method":"config.get","params":{}}"#);
    let loaded = client
        .wait_for(|value| value["id"] == 4, Duration::from_secs(5))
        .expect("local profile config.get");
    assert_eq!(
        loaded["result"]["local_device"]["device_id"],
        "pending-account-device"
    );
    assert_eq!(loaded["result"]["switch_group"], Value::Null);
}

/// A receiving v2 client must not write merely because it starts. After the
/// trigger group has been absent and then stably arrives, it rewrites its own
/// local input as the correction path.
#[test]
fn e2e_v2_arrival_corrects_only_after_usb_arrival() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config.json");
    let log_dir = tmp.path().join("logs");
    let v2 = serde_json::json!({
        "schema_version": 2,
        "local_device": {
            "device_id": "windows",
            "host_label": "Windows",
            "monitors": [
                {"fingerprint": "SAC-2763-S:0000000000001", "label": "G73", "local_input": 15, "source": "learned_active_read"}
            ]
        },
        "switch_group": {
            "group_id": "desk",
            "revision": 4,
            "devices": [
                {"device_id": "mac-work", "name": "工作 Mac", "port_index": 1, "monitors": [
                    {"fingerprint": "SAC-2763-S:0000000000001", "label": "G73", "local_input": 17}
                ]},
                {"device_id": "windows", "name": "Windows", "port_index": 2, "monitors": [
                    {"fingerprint": "SAC-2763-S:0000000000001", "label": "G73", "local_input": 15}
                ]}
            ]
        },
        "trigger": {
            "anchor": {"vid_pid": "1a40:0101"},
            "members": [{"vid_pid": "3837:303c"}],
            "debounce": {"t_stable_ms": 500, "t_absent_ms": 1000, "t_cooldown_ms": 1500, "quorum_peripherals": 1}
        },
        "advanced": {
            "ddc_retry": {"attempts": 1, "delay_ms": 10},
            "usb_poll_ms": 100,
            "arrival_correction_enabled": true
        }
    });

    let mut client = Client::spawn(&[
        "--backend",
        "simulated",
        "--scenario",
        "fast_arrival",
        "--config",
        config.to_str().unwrap(),
        "--log-dir",
        log_dir.to_str().unwrap(),
    ]);
    assert!(client
        .wait_for(
            |value| value["notification"] == "ready",
            Duration::from_secs(5)
        )
        .is_some());
    client.send(&format!(
        r#"{{"v":1,"id":1,"method":"config.set","params":{{"config":{v2}}}}}"#
    ));
    let saved = client
        .wait_for(|value| value["id"] == 1, Duration::from_secs(5))
        .expect("v2 config.set");
    assert_eq!(saved["ok"], true);

    let report = client
        .wait_for(
            |value| {
                value["notification"] == "switch.report"
                    && value["data"]["phase"] == "target_arrival_correction"
            },
            Duration::from_secs(6),
        )
        .expect("v2 target arrival correction report");
    assert_eq!(report["data"]["target_device_id"], "windows");
    assert_eq!(
        report["data"]["per_monitor"][0]["edid_id"],
        "SAC-2763-S:0000000000001"
    );
    assert_eq!(report["data"]["per_monitor"][0]["commanded"], true);
    assert_eq!(report["data"]["per_monitor"][0]["requested"], 15);
}

#[test]
fn e2e_v2_arrival_correction_can_be_disabled() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config.json");
    let log_dir = tmp.path().join("logs");
    let v2 = serde_json::json!({
        "schema_version": 2,
        "local_device": {
            "device_id": "windows",
            "host_label": "Windows",
            "monitors": [{"fingerprint": "SAC-2763-S:0000000000001", "label": "G73", "local_input": 15}]
        },
        "switch_group": {
            "group_id": "desk",
            "revision": 4,
            "devices": [
                {"device_id": "mac", "name": "Mac", "port_index": 1, "monitors": [
                    {"fingerprint": "SAC-2763-S:0000000000001", "label": "G73", "local_input": 17}
                ]},
                {"device_id": "windows", "name": "Windows", "port_index": 2, "monitors": [
                    {"fingerprint": "SAC-2763-S:0000000000001", "label": "G73", "local_input": 15}
                ]}
            ]
        },
        "trigger": {
            "anchor": {"vid_pid": "1a40:0101"},
            "members": [{"vid_pid": "3837:303c"}],
            "debounce": {"t_stable_ms": 0, "t_absent_ms": 0, "t_cooldown_ms": 1000, "quorum_peripherals": 1}
        },
        "advanced": {
            "ddc_retry": {"attempts": 1, "delay_ms": 10},
            "usb_poll_ms": 100,
            "arrival_correction_enabled": false
        }
    });
    let mut client = Client::spawn(&[
        "--backend",
        "simulated",
        "--scenario",
        "fast_arrival",
        "--config",
        config.to_str().unwrap(),
        "--log-dir",
        log_dir.to_str().unwrap(),
    ]);
    assert!(client
        .wait_for(
            |value| value["notification"] == "ready",
            Duration::from_secs(5)
        )
        .is_some());
    client.send(&format!(
        r#"{{"v":1,"id":1,"method":"config.set","params":{{"config":{v2}}}}}"#
    ));
    assert_eq!(
        client
            .wait_for(|value| value["id"] == 1, Duration::from_secs(5))
            .unwrap()["ok"],
        true
    );

    let report = client.wait_for(
        |value| value["notification"] == "switch.report",
        Duration::from_secs(3),
    );
    assert!(
        report.is_none(),
        "disabled arrival correction must not write DDC"
    );
    client.send(r#"{"v":1,"id":2,"method":"diagnostics.collect","params":{"tail":50}}"#);
    let diagnostics = client
        .wait_for(|value| value["id"] == 2, Duration::from_secs(5))
        .unwrap();
    assert!(diagnostics["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| { event["kind"] == "trigger.arrival_correction_skipped" }));
}

/// Starting the app while the USB group is already attached is a baseline,
/// not an arrival. A startup correction used to write DDC immediately and
/// could black-screen Windows before the user pressed the physical switch.
#[test]
fn e2e_v2_startup_with_group_present_does_not_write() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config.json");
    let log_dir = tmp.path().join("logs");
    let v2 = serde_json::json!({
        "schema_version": 2,
        "local_device": {
            "device_id": "windows",
            "host_label": "Windows",
            "monitors": [
                {"fingerprint": "SAC-2763-S:0000000000001", "label": "G73", "local_input": 16, "source": "manual"},
                {"fingerprint": "SAC-2466-S:0000000000000", "label": "G52plus", "local_input": 8, "source": "manual"}
            ]
        },
        "switch_group": {
            "group_id": "desk", "revision": 1,
            "devices": [
                {"device_id": "mac", "name": "Mac", "port_index": 1, "monitors": [
                    {"fingerprint": "SAC-2763-S:0000000000001", "local_input": 15},
                    {"fingerprint": "SAC-2466-S:0000000000000", "local_input": 7}
                ]},
                {"device_id": "windows", "name": "Windows", "port_index": 2, "monitors": [
                    {"fingerprint": "SAC-2763-S:0000000000001", "local_input": 16},
                    {"fingerprint": "SAC-2466-S:0000000000000", "local_input": 8}
                ]}
            ]
        },
        "trigger": {
            "anchor": {"vid_pid": "1a40:0101"},
            "members": [{"vid_pid": "3837:303c", "serial": "fixture-usb-3837-303c-1"}],
            "debounce": {"t_stable_ms": 500, "t_absent_ms": 1000, "t_cooldown_ms": 1500, "quorum_peripherals": 1}
        },
        "advanced": {"ddc_retry": {"attempts": 1, "delay_ms": 10}, "usb_poll_ms": 100}
    });
    std::fs::write(&config, serde_json::to_vec_pretty(&v2).unwrap()).unwrap();

    let mut client = Client::spawn(&[
        "--backend",
        "simulated",
        "--scenario",
        "idle",
        "--config",
        config.to_str().unwrap(),
        "--log-dir",
        log_dir.to_str().unwrap(),
    ]);
    assert!(client
        .wait_for(
            |value| value["notification"] == "ready",
            Duration::from_secs(5)
        )
        .is_some());
    let report = client.wait_for(
        |value| value["notification"] == "switch.report",
        Duration::from_secs(3),
    );
    assert!(
        report.is_none(),
        "startup baseline must not emit a switch report"
    );
}

#[test]
fn e2e_v2_pause_blocks_physical_trigger_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config.json");
    let log_dir = tmp.path().join("logs");
    let v2 = serde_json::json!({
        "schema_version": 2,
        "local_device": {
            "device_id": "mac-work",
            "host_label": "工作 Mac",
            "monitors": [
                {"fingerprint": "SAC-2763-S:0000000000001", "label": "G73", "local_input": 15, "source": "learned_active_read"}
            ]
        },
        "switch_group": {
            "group_id": "desk", "revision": 4,
            "devices": [
                {"device_id": "mac-work", "name": "工作 Mac", "port_index": 1, "monitors": [{"fingerprint": "SAC-2763-S:0000000000001", "label": "G73", "local_input": 15}]},
                {"device_id": "windows", "name": "Windows", "port_index": 2, "monitors": [{"fingerprint": "SAC-2763-S:0000000000001", "label": "G73", "local_input": 17}]}
            ]
        },
        "trigger": {
            "anchor": {"vid_pid": "1a40:0101"}, "members": [{"vid_pid": "3837:303c"}],
            "debounce": {"t_stable_ms": 500, "t_absent_ms": 1000, "t_cooldown_ms": 1500, "quorum_peripherals": 1}
        },
        "advanced": {"ddc_retry": {"attempts": 1, "delay_ms": 10}, "usb_poll_ms": 100}
    });
    let mut client = Client::spawn(&[
        "--backend",
        "simulated",
        "--scenario",
        "fast_clean_roundtrip",
        "--config",
        config.to_str().unwrap(),
        "--log-dir",
        log_dir.to_str().unwrap(),
    ]);
    assert!(client
        .wait_for(
            |value| value["notification"] == "ready",
            Duration::from_secs(5)
        )
        .is_some());
    client.send(&format!(
        r#"{{"v":1,"id":1,"method":"config.set","params":{{"config":{v2}}}}}"#
    ));
    assert_eq!(
        client
            .wait_for(|value| value["id"] == 1, Duration::from_secs(5))
            .unwrap()["ok"],
        true
    );
    client.send(r#"{"v":1,"id":2,"method":"app.setEnabled","params":{"enabled":false}}"#);
    let paused = client
        .wait_for(|value| value["id"] == 2, Duration::from_secs(5))
        .expect("pause response");
    assert_eq!(paused["result"]["enabled"], false);

    assert!(
        client
            .wait_for(
                |value| value["notification"] == "switch.report"
                    && value["data"]["mode"] == "multi_device",
                Duration::from_secs(4),
            )
            .is_none(),
        "paused v2 must not write on the scheduled USB departure",
    );
    client.send(r#"{"v":1,"id":3,"method":"state.get","params":{}}"#);
    let state = client
        .wait_for(|value| value["id"] == 3, Duration::from_secs(5))
        .expect("state after pause");
    assert_eq!(state["result"]["enabled"], false);
}

/// End-to-end over the real binary + real protocol: wizard flow, config,
/// trigger, push, report - all on the simulated backend.
#[test]
fn e2e_clean_roundtrip_pushes_monitors_and_reports_commanded() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config.json");
    let log_dir = tmp.path().join("logs");

    let mut c = Client::spawn(&[
        "--backend",
        "simulated",
        "--scenario",
        "clean_roundtrip",
        "--config",
        config.to_str().unwrap(),
        "--log-dir",
        log_dir.to_str().unwrap(),
    ]);

    // ready notification
    let ready = c.wait_for(
        |v| v["notification"].as_str() == Some("ready"),
        Duration::from_secs(5),
    );
    assert!(ready.is_some(), "ready notification");

    c.send(r#"{"v":1,"id":1,"method":"hello","params":{"client":"test"}}"#);
    let hello = c
        .wait_for(|v| v["id"] == 1 && v["ok"] == true, Duration::from_secs(5))
        .expect("hello ok");
    assert_eq!(hello["result"]["protocol_version"], 1);
    assert_eq!(hello["result"]["backend"], "simulated");

    // display.list: two SAC monitors, DDC available (active input)
    c.send(r#"{"v":1,"id":2,"method":"display.list"}"#);
    let disp = c
        .wait_for(|v| v["id"] == 2, Duration::from_secs(5))
        .expect("display.list");
    let displays = disp["result"]["displays"]
        .as_array()
        .expect("displays array");
    assert_eq!(displays.len(), 2);
    assert!(displays
        .iter()
        .any(|d| d["edid_id"] == "SAC-2763-S:0000000000001" && d["ddc"]["state"] == "available"));
    assert!(displays
        .iter()
        .any(|d| d["edid_id"] == "SAC-2466-S:0000000000000"));

    // full config (KVM-1 learned values; shortened debounce for test speed)
    let cfg = serde_json::json!({
        "schema_version": 1,
        "host_label": "e2e-test-host",
        "monitors": [
            {"edid_id": "SAC-2763-S:0000000000001", "label": "G73", "here_input": 15, "away_input": 16,
             "here_input_source": "learned_active_read", "away_input_source": "heuristic_prefill"},
            {"edid_id": "SAC-2466-S:0000000000000", "label": "G52plus", "here_input": 7, "away_input": 8,
             "here_input_source": "learned_active_read", "away_input_source": "heuristic_prefill"}
        ],
        "trigger": {
            "anchor": {"vid_pid": "1a40:0101"},
            "members": [
                {"vid_pid": "3837:303c", "serial": "fixture-usb-3837-303c-1"},
                {"vid_pid": "373b:10c9", "serial": "Љ"}
            ],
            "debounce": {"t_stable_ms": 500, "t_absent_ms": 1000, "t_cooldown_ms": 1500, "quorum_peripherals": 1}
        },
        "advanced": {"ddc_retry": {"attempts": 2, "delay_ms": 50}, "usb_poll_ms": 100}
    });
    let req = format!(
        r#"{{"v":1,"id":3,"method":"config.set","params":{{"config":{}}}}}"#,
        cfg
    );
    c.send(&req);
    let saved = c
        .wait_for(|v| v["id"] == 3, Duration::from_secs(5))
        .expect("config.set");
    assert_eq!(saved["ok"], true, "config accepted: {saved}");
    assert!(config.exists(), "config persisted to disk");

    // wizard flow smoke: begin -> candidates on leave -> end
    c.send(r#"{"v":1,"id":4,"method":"wizard.begin"}"#);
    let wb = c
        .wait_for(|v| v["id"] == 4, Duration::from_secs(5))
        .expect("wizard.begin");
    assert_eq!(
        wb["result"]["baseline"].as_u64(),
        Some(4),
        "baseline snapshot sees 4 devices (group + phone)"
    );

    let candidates = c
        .wait_for(
            |v| v["notification"].as_str() == Some("wizard.candidates"),
            Duration::from_secs(10),
        )
        .expect("wizard.candidates fired when group left");
    let disappeared = candidates["data"]["disappeared"].as_array().unwrap();
    assert_eq!(
        disappeared.len(),
        3,
        "the switch group (hub+kb+mouse) is identified"
    );
    assert!(disappeared.iter().any(|d| d["vid_pid"] == "1a40:0101"));

    c.send(r#"{"v":1,"id":5,"method":"wizard.end"}"#);
    let _ = c.wait_for(|v| v["id"] == 5, Duration::from_secs(5));

    // armed: watch the trigger
    c.send(r#"{"v":1,"id":6,"method":"usb.watch.start"}"#);
    let armed = c
        .wait_for(|v| v["id"] == 6, Duration::from_secs(5))
        .expect("usb.watch.start");
    assert_eq!(armed["result"]["armed"], true);

    // state: with config valid the FSM should be idle (or learning->idle after wizard.end)
    c.send(r#"{"v":1,"id":7,"method":"state.get"}"#);
    let st = c
        .wait_for(|v| v["id"] == 7, Duration::from_secs(5))
        .expect("state.get");
    assert_eq!(
        st["result"]["state"], "idle",
        "armed after config + wizard.end"
    );

    // The scenario: group left at t=8s (already elapsed during wizard);
    // with the SHORT debounce the GroupLeft already fired while in Learning
    // (suppressed - learning never pushes). Re-trigger via manual push to
    // exercise the write path deterministically.
    c.send(r#"{"v":1,"id":8,"method":"switch.pushNow","params":{"reason":"wizard_final_test"}}"#);
    let push = c
        .wait_for(|v| v["id"] == 8, Duration::from_secs(10))
        .expect("pushNow");
    assert_eq!(push["ok"], true, "manual push accepted");

    // switch.report notification: both monitors commanded, honest note
    let report = c
        .wait_for(
            |v| v["notification"].as_str() == Some("switch.report"),
            Duration::from_secs(10),
        )
        .expect("switch.report after push");
    let per = report["data"]["per_monitor"].as_array().unwrap();
    assert_eq!(per.len(), 2);
    assert!(per.iter().all(|m| m["commanded"] == true));
    assert!(per.iter().any(|m| m["requested"] == 16));
    assert!(per.iter().any(|m| m["requested"] == 8));
    assert!(per.iter().any(|m| m["previous"] == 15));
    assert!(per.iter().any(|m| m["previous"] == 7));
    // Automatic switching never waits for a read from the input it just left.
    assert!(per
        .iter()
        .all(|m| m["readback"].is_null() && m["readback_error"].is_null()));
    assert!(report["data"]["note"]
        .as_str()
        .unwrap()
        .contains("machine_observed_only"));

    // after push, reads degrade (control matrix) - readInput must fail honestly
    c.send(r#"{"v":1,"id":9,"method":"display.readInput","params":{"edid_id":"SAC-2763-S:0000000000001"}}"#);
    let read = c
        .wait_for(|v| v["id"] == 9, Duration::from_secs(5))
        .expect("readInput");
    assert_eq!(read["ok"], false);
    assert_eq!(read["error"]["code"], "E_DDC_NOT_READABLE");

    // diagnostics
    c.send(r#"{"v":1,"id":10,"method":"diagnostics.collect","params":{"tail":50}}"#);
    let diag = c
        .wait_for(|v| v["id"] == 10, Duration::from_secs(5))
        .expect("diagnostics");
    assert!(diag["result"]["events"].as_array().unwrap().len() >= 5);

    // protocol version rejection
    c.send(r#"{"v":9,"id":11,"method":"hello"}"#);
    let bad = c
        .wait_for(|v| v["id"] == 11, Duration::from_secs(5))
        .expect("version rejection");
    assert_eq!(bad["ok"], false);
    assert_eq!(bad["error"]["code"], "E_PROTOCOL_VERSION");

    // graceful shutdown
    c.send(r#"{"v":1,"id":12,"method":"shutdown"}"#);
    let bye = c
        .wait_for(|v| v["id"] == 12, Duration::from_secs(5))
        .expect("shutdown ack");
    assert_eq!(bye["result"]["bye"], true);
    let _ = c.child.wait();
}

/// Failure path: one monitor's DDC persistently fails -> per-monitor honest
/// reporting, retry attempts recorded.
#[test]
fn e2e_ddc_failure_reports_per_monitor() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config.json");
    let log_dir = tmp.path().join("logs");

    let mut c = Client::spawn(&[
        "--backend",
        "simulated",
        "--scenario",
        "clean_roundtrip_ddc_fail",
        "--config",
        config.to_str().unwrap(),
        "--log-dir",
        log_dir.to_str().unwrap(),
    ]);
    let _ = c.wait_for(
        |v| v["notification"].as_str() == Some("ready"),
        Duration::from_secs(5),
    );

    let cfg = serde_json::json!({
        "schema_version": 1,
        "monitors": [
            {"edid_id": "SAC-2763-S:0000000000001", "here_input": 15, "away_input": 16, "away_input_source": "heuristic_prefill"},
            {"edid_id": "SAC-2466-S:0000000000000", "here_input": 7, "away_input": 8, "away_input_source": "heuristic_prefill"}
        ],
        "trigger": {
            "anchor": {"vid_pid": "1a40:0101"},
            "members": [
                {"vid_pid": "3837:303c", "serial": "fixture-usb-3837-303c-1"},
                {"vid_pid": "373b:10c9", "serial": "Љ"}
            ],
            "debounce": {"t_stable_ms": 500, "t_absent_ms": 1000, "t_cooldown_ms": 1500}
        },
        "advanced": {"ddc_retry": {"attempts": 3, "delay_ms": 30}, "usb_poll_ms": 100}
    });
    let req = format!(
        r#"{{"v":1,"id":1,"method":"config.set","params":{{"config":{}}}}}"#,
        cfg
    );
    c.send(&req);
    let _ = c.wait_for(|v| v["id"] == 1, Duration::from_secs(5));

    c.send(r#"{"v":1,"id":2,"method":"switch.pushNow","params":{"reason":"test"}}"#);
    let _ = c.wait_for(|v| v["id"] == 2, Duration::from_secs(10));
    let report = c
        .wait_for(
            |v| v["notification"].as_str() == Some("switch.report"),
            Duration::from_secs(10),
        )
        .expect("switch.report");
    let per = report["data"]["per_monitor"].as_array().unwrap();
    let g73 = per
        .iter()
        .find(|m| m["edid_id"] == "SAC-2763-S:0000000000001")
        .unwrap();
    let g52 = per
        .iter()
        .find(|m| m["edid_id"] == "SAC-2466-S:0000000000000")
        .unwrap();
    assert_eq!(g73["commanded"], true);
    assert_eq!(g52["commanded"], false);
    assert_eq!(g52["attempts"], 3, "retry policy honored");
    assert!(g52["last_error"]
        .as_str()
        .unwrap()
        .contains("simulated persistent DDC failure"));
}

/// Replug signature: the group leaves (trigger fires) then comes back -
/// UnexpectedReturn notification with both interpretations.
#[test]
fn e2e_replug_yields_unexpected_return() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config.json");
    let log_dir = tmp.path().join("logs");

    let mut c = Client::spawn(&[
        "--backend",
        "simulated",
        "--scenario",
        "fast_replug",
        "--config",
        config.to_str().unwrap(),
        "--log-dir",
        log_dir.to_str().unwrap(),
    ]);
    let _ = c.wait_for(
        |v| v["notification"].as_str() == Some("ready"),
        Duration::from_secs(5),
    );

    let cfg = serde_json::json!({
        "schema_version": 1,
        "monitors": [
            {"edid_id": "SAC-2763-S:0000000000001", "here_input": 15, "away_input": 16, "away_input_source": "heuristic_prefill"}
        ],
        "trigger": {
            "anchor": {"vid_pid": "1a40:0101"},
            "members": [
                {"vid_pid": "3837:303c", "serial": "fixture-usb-3837-303c-1"},
                {"vid_pid": "373b:10c9", "serial": "Љ"}
            ],
            "debounce": {"t_stable_ms": 500, "t_absent_ms": 1000, "t_cooldown_ms": 1500}
        },
        "advanced": {"ddc_retry": {"attempts": 1, "delay_ms": 10}, "usb_poll_ms": 100}
    });
    let req = format!(
        r#"{{"v":1,"id":1,"method":"config.set","params":{{"config":{}}}}}"#,
        cfg
    );
    c.send(&req);
    let _ = c.wait_for(|v| v["id"] == 1, Duration::from_secs(5));

    // automatic trigger: group leaves at scenario t=1.5s with SHORT debounce
    // (t_absent 1s) -> GroupLeft -> push -> report
    let report = c
        .wait_for(
            |v| v["notification"].as_str() == Some("switch.report"),
            Duration::from_secs(20),
        )
        .expect("automatic push on group leave");
    assert_eq!(report["data"]["manual"], false);

    // group returns at scenario t=4s -> UnexpectedReturn
    let ur = c.wait_for(
        |v| {
            v["notification"] == "trigger" && v["data"]["kind"] == "unexpected_return"
                || (v["notification"].as_str() == Some("trigger")
                    && v["data"].get("action").and_then(|a| a.as_str())
                        == Some("unexpected_return"))
        },
        Duration::from_secs(30),
    );
    assert!(
        ur.is_some(),
        "unexpected_return surfaced (either via tracker event notify or interpretations action)"
    );
}

/// KVM-7 regression: display.list must leave a session event (count,
/// identities, drop reasons) in sidecar-session.jsonl - the Windows zero-
/// display defect shipped invisibly because this path wrote nothing.
#[test]
fn e2e_display_list_writes_session_event() {
    let tmp = tempfile::tempdir().unwrap();
    let log_dir = tmp.path().join("logs");

    let mut c = Client::spawn(&[
        "--backend",
        "simulated",
        "--scenario",
        "idle",
        "--config",
        tmp.path().join("config.json").to_str().unwrap(),
        "--log-dir",
        log_dir.to_str().unwrap(),
    ]);
    let _ = c.wait_for(
        |v| v["notification"].as_str() == Some("ready"),
        Duration::from_secs(5),
    );

    c.send(r#"{"v":1,"id":1,"method":"display.list"}"#);
    let disp = c
        .wait_for(|v| v["id"] == 1, Duration::from_secs(5))
        .expect("display.list");
    assert_eq!(disp["result"]["displays"].as_array().unwrap().len(), 2);

    // the mirrored event notification must carry the same enumeration outcome
    let ev = c
        .wait_for(
            |v| {
                v["notification"].as_str() == Some("event")
                    && v["data"]["kind"].as_str() == Some("display.list")
            },
            Duration::from_secs(5),
        )
        .expect("display.list session event");
    assert_eq!(ev["data"]["fields"]["count"], 2);
    let ids: Vec<&str> = ev["data"]["fields"]["displays"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["edid_id"].as_str())
        .collect();
    assert!(ids.contains(&"SAC-2763-S:0000000000001"));
    assert!(ids.contains(&"SAC-2466-S:0000000000000"));
    assert_eq!(ev["data"]["level"], "info");

    c.send(r#"{"v":1,"id":2,"method":"shutdown"}"#);
    let _ = c.wait_for(|v| v["id"] == 2, Duration::from_secs(5));
    let _ = c.child.wait();

    // and it must be persisted in the session JSONL, not just notified
    let log_path = log_dir.join("sidecar-session.jsonl");
    let text = std::fs::read_to_string(&log_path).expect("session log exists");
    let record = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|r| r["kind"] == "display.list")
        .expect("display.list record persisted");
    assert_eq!(record["fields"]["count"], 2);
}
