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
            { "edid_id": "SAC-2763-S:0000000000001", "here_input": 15, "away_input": 16, "away_input_source": "heuristic_prefill" },
            { "edid_id": "SAC-2466-S:0000000000000", "here_input": 7, "away_input": 8, "away_input_source": "heuristic_prefill" }
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
fn loads_a_v1_config_migrates_it_on_disk_and_sends_events_without_stdio() {
    let dir = tempfile::tempdir().unwrap();
    let original = serde_json::to_vec(&config()).unwrap();
    std::fs::write(dir.path().join("config.json"), &original).unwrap();
    let (runtime, rx) = start(dir.path(), "config.json");
    let loaded = runtime.request("config.get", json!({})).unwrap();
    assert_eq!(loaded["schema_version"], 2);
    assert_eq!(loaded["local_device"]["host_label"], "embedded-test");
    assert_eq!(
        loaded["switch_group"]["devices"].as_array().unwrap().len(),
        2
    );
    let ready = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(ready.0, "ready");
    assert_eq!(ready.1["simulated"], true);
    let state = runtime.request("state.get", json!({})).unwrap();
    assert_eq!(state["configMode"], "multi_device_armed");
    assert_eq!(state["state"], "idle");
    let on_disk: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("config.json")).unwrap()).unwrap();
    assert_eq!(on_disk["schema_version"], 2);
    assert_eq!(
        std::fs::read(dir.path().join("config.json.v1-backup")).unwrap(),
        original
    );
    let displays = runtime.request("display.list", json!({})).unwrap();
    assert_eq!(displays["displays"].as_array().unwrap().len(), 2);
    assert_eq!(
        runtime
            .request("switch.pushNow", json!({ "reason": "embedding-smoke" }))
            .unwrap_err()
            .code,
        "E_UNKNOWN_METHOD"
    );
    runtime.shutdown();
    assert!(runtime.request("state.get", json!({})).is_err());
}

#[test]
fn an_incomplete_v1_config_is_converted_in_memory_without_touching_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut incomplete = config();
    incomplete["monitors"][0]
        .as_object_mut()
        .unwrap()
        .remove("away_input_source");
    let original = serde_json::to_vec(&incomplete).unwrap();
    std::fs::write(dir.path().join("config.json"), &original).unwrap();
    let (runtime, _) = start(dir.path(), "config.json");
    let loaded = runtime.request("config.get", json!({})).unwrap();
    assert_eq!(loaded["schema_version"], 2);
    assert!(loaded["switch_group"].is_null());
    assert_eq!(
        loaded["local_device"]["monitors"][0]["legacy_remote_preset"]["value"],
        16
    );
    let state = runtime.request("state.get", json!({})).unwrap();
    assert_eq!(state["configMode"], "multi_device_setup");
    assert_eq!(state["state"], "unconfigured");
    assert_eq!(
        std::fs::read(dir.path().join("config.json")).unwrap(),
        original
    );
    assert!(!dir.path().join("config.json.v1-backup").exists());
}

#[test]
fn incomplete_v1_identity_survives_restart_and_first_explicit_save_backs_up_original() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut incomplete = config();
    incomplete["monitors"][0]
        .as_object_mut()
        .unwrap()
        .remove("away_input_source");
    let original = serde_json::to_vec(&incomplete).unwrap();
    std::fs::write(&path, &original).unwrap();
    let (rt, _) = start(dir.path(), "config.json");
    let first = rt.request("config.get", json!({})).unwrap();
    let id = first["local_device"]["device_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(id.starts_with("dev-"));
    rt.shutdown_and_join().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let (rt, _) = start(dir.path(), "config.json");
    let current = rt.request("config.get", json!({})).unwrap();
    assert_eq!(current["local_device"]["device_id"], id);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    rt.request("config.set", json!({"config":current})).unwrap();
    rt.shutdown_and_join().unwrap();
    let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["local_device"]["device_id"], id);
    let backup = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("config.json.pre-identity-")
        })
        .unwrap();
    assert_eq!(std::fs::read(backup.path()).unwrap(), original);
    assert_eq!(
        saved["local_device"]["monitors"][0]["legacy_remote_preset"]["value"],
        16
    );
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

#[test]
fn unreadable_config_is_set_aside_before_a_new_save() {
    let dir = tempfile::tempdir().unwrap();
    let truncated = r#"{ "schema_version": 1, "host_label": "embedded-te"#;
    std::fs::write(dir.path().join("config.json"), truncated).unwrap();
    let (runtime, _) = start(dir.path(), "config.json");
    assert!(runtime.request("config.get", json!({})).unwrap().is_null());
    let recovery = runtime.request("state.get", json!({})).unwrap()["configRecovery"].clone();
    let preserved_name = recovery["preservedAs"].as_str().unwrap().to_string();
    assert!(
        preserved_name.starts_with("config.json.corrupt-"),
        "{recovery}"
    );
    assert!(!recovery["error"].as_str().unwrap().is_empty());
    runtime
        .request("config.set", json!({ "config": config() }))
        .unwrap();
    assert!(runtime.request("state.get", json!({})).unwrap()["configRecovery"].is_null());

    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let preserved: Vec<&String> = names
        .iter()
        .filter(|name| name.starts_with("config.json.corrupt-"))
        .collect();
    assert_eq!(preserved.len(), 1, "files: {names:?}");
    assert_eq!(preserved[0], &preserved_name);
    assert_eq!(
        std::fs::read_to_string(dir.path().join(preserved[0])).unwrap(),
        truncated
    );
    assert!(
        !names.iter().any(|name| name.ends_with(".tmp")),
        "files: {names:?}"
    );
    let saved: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("config.json")).unwrap()).unwrap();
    assert_eq!(saved["local_device"]["host_label"], "embedded-test");
}

#[test]
fn runtime_snapshots_have_one_monotonic_sequence_across_events_and_responses() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.json"),
        serde_json::to_vec(&config()).unwrap(),
    )
    .unwrap();
    let (runtime, rx) = start(dir.path(), "config.json");
    let before = runtime.request("state.get", json!({})).unwrap();
    let first = before["runtime_seq"]
        .as_u64()
        .expect("full snapshot sequence");
    assert_eq!(before["mixed"]["runtime_seq"], first);
    let paused = runtime
        .request("app.setEnabled", json!({"enabled":false}))
        .unwrap();
    let pause_seq = paused["runtime_seq"]
        .as_u64()
        .expect("partial response sequence");
    assert!(pause_seq > first);
    let unchanged = runtime
        .request("mixed.set", json!({"enabled":false}))
        .unwrap();
    let mixed_seq = unchanged["runtime_seq"]
        .as_u64()
        .expect("mixed response sequence");
    assert!(mixed_seq > pause_seq);
    assert_eq!(unchanged["mixed"]["runtime_seq"], mixed_seq);
    let after = runtime.request("state.get", json!({})).unwrap();
    assert!(after["runtime_seq"].as_u64().unwrap() > mixed_seq);
    let mut previous = 0;
    for (kind, data) in rx.try_iter() {
        if matches!(
            kind.as_str(),
            "state" | "mixed.changed" | "mixed.pending_remote"
        ) {
            let sequence = data["runtime_seq"].as_u64().expect("event sequence");
            assert!(sequence > previous);
            previous = sequence;
            if data["mixed"].is_object() {
                assert_eq!(data["mixed"]["runtime_seq"], sequence);
            }
        }
    }
    assert!(previous > 0);
    runtime.shutdown_and_join().unwrap();
}

#[test]
fn manual_connect_commands_validate_before_dispatch_and_identity_works_without_lan() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, _) = start(dir.path(), "config.json");
    let identity = runtime.request("lan.identity", json!({})).unwrap();
    assert!(identity["device_id"].as_str().unwrap().starts_with("dev-"));
    assert_eq!(
        runtime.request("lan.identity", json!({})).unwrap(),
        identity
    );
    for method in ["lan.connect_manual", "lan.connect.manual"] {
        for pair in [true, false] {
            assert_eq!(
                runtime
                    .request(method, json!({"addr":"8.8.8.8","pair":pair}))
                    .unwrap_err()
                    .code,
                "E_LAN_ADDRESS"
            );
            assert_eq!(
                runtime
                    .request(method, json!({"addr":"192.168.1.23","pair":pair}))
                    .unwrap_err()
                    .code,
                "E_LAN_DISABLED"
            );
        }
    }
    runtime.shutdown();
}

#[test]
fn paired_legacy_config_emits_actionable_issue_without_changing_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = kvmflow_core::config::MultiDeviceConfig::from_legacy(
        kvmflow_core::config::example_config(),
    );
    cfg.local_device.device_id = "mac".into();
    let mut value = serde_json::to_value(cfg).unwrap();
    value["switch_group"] = json!({"group_id":"paired","revision":1,"devices":[],"sync":{"members":[{"device_id":"peer","public_key":"pin"}]}});
    let original = serde_json::to_vec(&value).unwrap();
    std::fs::write(dir.path().join("config.json"), &original).unwrap();
    let (runtime, _) = start(dir.path(), "config.json");
    let state = runtime.request("lan.status", json!({})).unwrap();
    assert!(state["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|issue| issue["code"] == "E_LAN_ID_MIGRATION_REQUIRED"));
    assert_eq!(
        runtime.request("lan.identity", json!({})).unwrap_err().code,
        "E_LAN_ID_MIGRATION_REQUIRED"
    );
    assert_eq!(
        std::fs::read(dir.path().join("config.json")).unwrap(),
        original
    );
    runtime.shutdown();
}
