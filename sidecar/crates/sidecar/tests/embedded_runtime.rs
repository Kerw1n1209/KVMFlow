//! Exercise the actual Tauri embedding boundary, including failures and
//! teardown. The stdio integration tests separately cover protocol parity.
use kvmflow_sidecar::{RuntimeHandle, RuntimeOptions};
use serde_json::{json, Value};
use std::sync::{mpsc, Arc};
use std::time::Duration;

fn start(
    dir: &std::path::Path,
    config_name: &str,
) -> (RuntimeHandle, mpsc::Receiver<(String, Value)>) {
    let mut options = RuntimeOptions::new(dir.join(config_name), dir.join("logs"));
    options.backend = "simulated".into();
    let (tx, rx) = mpsc::channel();
    let runtime = RuntimeHandle::start(options, move |kind, data| {
        let _ = tx.send((kind.into(), data));
    })
    .unwrap();
    (runtime, rx)
}

fn config() -> Value {
    json!({
        "schema_version": 1, "host_label": "embedded-test",
        "monitors": [
            { "edid_id": "SAC-2763-S:0000000000001", "here_input": 15, "away_input": 16 },
            { "edid_id": "SAC-2466-S:0000000000000", "here_input": 7, "away_input": 8 }
        ],
        "trigger": {
            "anchor": { "vid_pid": "1a40:0101" },
            "members": [{ "vid_pid": "3837:303c", "serial": "fixture-usb-3837-303c-1" }],
            "debounce": { "t_stable_ms": 0, "t_absent_ms": 0, "t_cooldown_ms": 1000 }
        },
        "advanced": { "ddc_retry": { "attempts": 1, "delay_ms": 0 }, "usb_poll_ms": 250 }
    })
}

#[test]
fn loads_existing_config_and_sends_events_without_stdio() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.json"),
        serde_json::to_vec(&config()).unwrap(),
    )
    .unwrap();
    let (runtime, rx) = start(dir.path(), "config.json");
    let loaded = runtime.request("config.get", json!({})).unwrap();
    assert_eq!(loaded["host_label"], "embedded-test");
    let ready = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(ready.0, "ready");
    assert_eq!(ready.1["simulated"], true);
    let displays = runtime.request("display.list", json!({})).unwrap();
    assert_eq!(displays["displays"].as_array().unwrap().len(), 2);
    let report = runtime
        .request("switch.pushNow", json!({ "reason": "embedding-smoke" }))
        .unwrap();
    assert!(report["report"]["per_monitor"]
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| entry["commanded"] == true));
    assert!(!report["report"]["note"]
        .as_str()
        .unwrap()
        .contains("human_verified"));
    runtime.shutdown();
    assert!(runtime.request("state.get", json!({})).is_err());
}

#[test]
fn concurrent_callers_receive_their_own_response() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, _) = start(dir.path(), "config.json");
    let runtime = Arc::new(runtime);
    let workers: Vec<_> = (0..12)
        .map(|index| {
            let runtime = runtime.clone();
            std::thread::spawn(move || {
                let method = if index % 2 == 0 {
                    "hello"
                } else {
                    "config.get"
                };
                let response = runtime.request(method, json!({})).unwrap();
                if method == "hello" {
                    assert_eq!(response["protocol_version"], 1);
                } else {
                    assert!(response.is_null());
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
}

#[test]
fn invalid_requests_keep_runtime_alive_and_preserve_config() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, _) = start(dir.path(), "config.json");
    assert_eq!(
        runtime.request("not-a-method", json!({})).unwrap_err().code,
        "E_UNKNOWN_METHOD"
    );
    assert!(runtime
        .request("config.set", json!({ "config": { "schema_version": 99 } }))
        .is_err());
    assert!(!dir.path().join("config.json").exists());
    assert!(runtime.request("state.get", json!({})).is_ok());
}

#[test]
fn backend_initialization_failure_returns_instead_of_hanging() {
    let dir = tempfile::tempdir().unwrap();
    let mut options = RuntimeOptions::new(dir.path().join("config.json"), dir.path().join("logs"));
    options.backend = "missing".into();
    let (tx, rx) = mpsc::channel();
    let runtime = RuntimeHandle::start(options, move |kind, data| {
        let _ = tx.send((kind.to_string(), data));
    })
    .unwrap();
    let (kind, _) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(kind, "runtime.error");
    assert!(runtime.request("config.get", json!({})).is_err());
}
