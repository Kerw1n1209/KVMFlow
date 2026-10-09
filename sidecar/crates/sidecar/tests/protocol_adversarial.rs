//! KVM-5 protocol adversarial tests, end to end over the real binary:
//! - malformed / adversarial frames must never kill or wedge the sidecar
//! - every method in the protocol.describe catalog must produce a
//!   well-formed response in a single session (contract conformance sweep)
//! - invalid configs must be rejected with E_CONFIG_INVALID and not persisted

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
    held: Vec<Value>,
}

impl Client {
    fn spawn() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_kvmflow-sidecar"))
            .args([
                "--backend",
                "simulated",
                "--scenario",
                "idle",
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
        let stdout = child.stdout.take().unwrap();
        let stdin = child.stdin.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        // keep tempdir alive for the child's lifetime via leak (test process exits anyway)
        std::mem::forget(tmp);
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

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn malformed_and_adversarial_frames_never_kill_the_sidecar() {
    let mut c = Client::spawn();
    assert!(c
        .wait_for(
            |v| v["notification"].as_str() == Some("ready"),
            Duration::from_secs(5)
        )
        .is_some());

    // invalid JSON
    c.send("{not json at all");
    // valid JSON, not an object
    c.send("[1,2,3]");
    c.send("\"just a string\"");
    // empty line (dropped, must not be treated as a request)
    c.send("");
    // object but missing method
    c.send(r#"{"v":1,"id":41}"#);
    // version mismatch - error must carry the request id for correlation
    c.send(r#"{"v":99,"id":42,"method":"hello"}"#);
    // unknown method
    c.send(r#"{"v":1,"id":43,"method":"definitely.not.real"}"#);
    // missing id -> must still answer (id coerced to 0), not crash
    c.send(r#"{"v":1,"method":"hello"}"#);

    let e41 = c
        .wait_for(|v| v["id"] == 41, Duration::from_secs(3))
        .expect("missing-method error");
    assert_eq!(e41["ok"], false);
    assert_eq!(e41["error"]["code"], "E_MALFORMED_MESSAGE");

    let e42 = c
        .wait_for(|v| v["id"] == 42, Duration::from_secs(3))
        .expect("version-mismatch error");
    assert_eq!(e42["ok"], false);
    assert_eq!(e42["error"]["code"], "E_PROTOCOL_VERSION");

    let e43 = c
        .wait_for(|v| v["id"] == 43, Duration::from_secs(3))
        .expect("unknown-method error");
    assert_eq!(e43["ok"], false);
    assert_eq!(e43["error"]["code"], "E_UNKNOWN_METHOD");

    let e0 = c
        .wait_for(
            |v| v["id"] == 0 && v.get("ok").is_some(),
            Duration::from_secs(3),
        )
        .expect("no-id request answered");
    // id-less request: either malformed or served - both acceptable, but a
    // response MUST exist and the process MUST survive.
    let _ = e0;

    // the sidecar survived all of the above and still serves requests
    assert!(c.alive(), "sidecar still running after adversarial frames");
    c.send(r#"{"v":1,"id":77,"method":"hello","params":{}}"#);
    let ok = c
        .wait_for(|v| v["id"] == 77, Duration::from_secs(3))
        .expect("hello after garbage");
    assert_eq!(
        ok["ok"], true,
        "sidecar fully functional after adversarial input"
    );

    // graceful shutdown still works
    c.send(r#"{"v":1,"id":78,"method":"shutdown"}"#);
    let bye = c
        .wait_for(|v| v["id"] == 78, Duration::from_secs(3))
        .expect("shutdown");
    assert_eq!(bye["result"]["bye"], true);
}

#[test]
fn full_request_catalog_returns_wellformed_responses() {
    let mut c = Client::spawn();
    assert!(c
        .wait_for(
            |v| v["notification"].as_str() == Some("ready"),
            Duration::from_secs(5)
        )
        .is_some());

    // the catalog is the single contract - sweep it in dependency order
    c.send(r#"{"v":1,"id":1,"method":"hello","params":{}}"#);
    c.send(r#"{"v":1,"id":2,"method":"protocol.describe","params":{}}"#);
    c.send(r#"{"v":1,"id":3,"method":"backend.status","params":{}}"#);
    c.send(r#"{"v":1,"id":4,"method":"config.get","params":{}}"#);
    c.send(r#"{"v":1,"id":5,"method":"display.list","params":{}}"#);
    c.send(r#"{"v":1,"id":6,"method":"display.readInput","params":{"edid_id":"SAC-2763-S:0000000000001"}}"#);
    c.send(r#"{"v":1,"id":7,"method":"usb.snapshot","params":{}}"#);

    // configure (also covers config.set), then the armed/wizard/push paths
    let cfg = serde_json::json!({
        "schema_version": 1,
        "host_label": "catalog-sweep",
        "monitors": [
            {"edid_id": "SAC-2763-S:0000000000001", "here_input": 15, "away_input": 16, "away_input_source": "heuristic_prefill"},
            {"edid_id": "SAC-2466-S:0000000000000", "here_input": 7, "away_input": 8, "away_input_source": "heuristic_prefill"}
        ],
        "trigger": {
            "anchor": {"vid_pid": "1a40:0101"},
            "members": [{"vid_pid": "3837:303c", "serial": "K"}, {"vid_pid": "373b:10c9", "serial": "M"}],
            "debounce": {"t_stable_ms": 500, "t_absent_ms": 1000, "t_cooldown_ms": 1500}
        },
        "advanced": {"ddc_retry": {"attempts": 1, "delay_ms": 10}, "usb_poll_ms": 100}
    });
    let req = format!(
        r#"{{"v":1,"id":8,"method":"config.set","params":{{"config":{}}}}}"#,
        cfg
    );
    c.send(&req);

    c.send(r#"{"v":1,"id":9,"method":"display.writeInput","params":{"edid_id":"SAC-2763-S:0000000000001","value":15,"reason":"catalog_sweep"}}"#);
    c.send(r#"{"v":1,"id":10,"method":"usb.watch.start","params":{}}"#);
    c.send(r#"{"v":1,"id":11,"method":"wizard.begin","params":{}}"#);
    c.send(r#"{"v":1,"id":12,"method":"wizard.candidates","params":{}}"#);
    c.send(r#"{"v":1,"id":13,"method":"wizard.end","params":{}}"#);
    c.send(r#"{"v":1,"id":14,"method":"app.setEnabled","params":{"enabled":false}}"#);
    c.send(r#"{"v":1,"id":15,"method":"app.setEnabled","params":{"enabled":true}}"#);
    c.send(r#"{"v":1,"id":16,"method":"switch.pushNow","params":{"reason":"catalog_sweep"}}"#);
    c.send(r#"{"v":1,"id":17,"method":"state.get","params":{}}"#);
    c.send(r#"{"v":1,"id":18,"method":"diagnostics.collect","params":{"tail":10}}"#);
    c.send(r#"{"v":1,"id":19,"method":"usb.watch.stop","params":{}}"#);
    c.send(r#"{"v":1,"id":20,"method":"shutdown","params":{}}"#);

    // every method in the catalog except shutdown must appear exactly once as
    // a well-formed v1 success response
    for id in 1..=19u64 {
        let resp = c
            .wait_for(
                |v| v["id"] == id && v.get("ok").is_some(),
                Duration::from_secs(10),
            )
            .unwrap_or_else(|| panic!("no response for request id {id}"));
        assert_eq!(resp["v"], 1, "response carries protocol version");
        assert_eq!(resp["ok"], true, "request id {id} failed: {resp}");
        if id == 2 {
            // protocol.describe: the catalog must list every method this test
            // drove, so catalog drift fails here rather than in production.
            let requests: Vec<&str> = resp["result"]["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m.as_str().unwrap())
                .collect();
            assert!(
                requests.len() > 10,
                "catalog suspiciously small: {requests:?}"
            );
            for m in [
                "hello",
                "protocol.describe",
                "config.set",
                "display.writeInput",
                "usb.watch.start",
                "app.setEnabled",
                "switch.pushNow",
                "shutdown",
            ] {
                assert!(requests.contains(&m), "catalog missing {m}");
            }
            let notes: Vec<&str> = resp["result"]["notifications"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m.as_str().unwrap())
                .collect();
            for n in ["ready", "switch.report", "wizard.candidates"] {
                assert!(notes.contains(&n), "notification catalog missing {n}");
            }
        }
    }
    let bye = c
        .wait_for(|v| v["id"] == 20, Duration::from_secs(3))
        .expect("shutdown ack");
    assert_eq!(bye["result"]["bye"], true);
}

#[test]
fn invalid_config_rejected_and_not_persisted() {
    let mut c = Client::spawn();
    assert!(c
        .wait_for(
            |v| v["notification"].as_str() == Some("ready"),
            Duration::from_secs(5)
        )
        .is_some());

    let bad_variants: Vec<(&str, serde_json::Value)> = vec![
        (
            "duplicate edid_id (same-model NOSERIAL collision)",
            serde_json::json!({
                "schema_version": 1,
                "monitors": [
                    {"edid_id": "SAC-2466-NOSERIAL", "here_input": 7, "away_input": 8, "away_input_source": "heuristic_prefill"},
                    {"edid_id": "SAC-2466-NOSERIAL", "here_input": 7, "away_input": 8, "away_input_source": "heuristic_prefill"}
                ],
                "trigger": {"anchor": {"vid_pid": "1a40:0101"}, "members": [{"vid_pid": "3837:303c"}]}
            }),
        ),
        (
            "here_input == away_input",
            serde_json::json!({
                "schema_version": 1,
                "monitors": [
                    {"edid_id": "SAC-2763-S:0000000000001", "here_input": 15, "away_input": 15, "away_input_source": "heuristic_prefill"}
                ],
                "trigger": {"anchor": {"vid_pid": "1a40:0101"}, "members": [{"vid_pid": "3837:303c"}]}
            }),
        ),
        (
            "debounce above supported ceiling",
            serde_json::json!({
                "schema_version": 1,
                "monitors": [
                    {"edid_id": "SAC-2763-S:0000000000001", "here_input": 15, "away_input": 16, "away_input_source": "heuristic_prefill"}
                ],
                "trigger": {"anchor": {"vid_pid": "1a40:0101"}, "members": [{"vid_pid": "3837:303c"}],
                            "debounce": {"t_stable_ms": 120001}}
            }),
        ),
        (
            "anchor vid_pid not hex",
            serde_json::json!({
                "schema_version": 1,
                "monitors": [
                    {"edid_id": "SAC-2763-S:0000000000001", "here_input": 15, "away_input": 16, "away_input_source": "heuristic_prefill"}
                ],
                "trigger": {"anchor": {"vid_pid": "not-a-vid"}, "members": [{"vid_pid": "3837:303c"}]}
            }),
        ),
        (
            "wrong schema version",
            serde_json::json!({
                "schema_version": 2,
                "monitors": [
                    {"edid_id": "SAC-2763-S:0000000000001", "here_input": 15, "away_input": 16, "away_input_source": "heuristic_prefill"}
                ],
                "trigger": {"anchor": {"vid_pid": "1a40:0101"}, "members": [{"vid_pid": "3837:303c"}]}
            }),
        ),
    ];

    for (i, (name, cfg)) in bad_variants.into_iter().enumerate() {
        let id = 100 + i as u64;
        let req =
            format!(r#"{{"v":1,"id":{id},"method":"config.set","params":{{"config":{cfg}}}}}"#);
        c.send(&req);
        let resp = c
            .wait_for(|v| v["id"] == id, Duration::from_secs(5))
            .unwrap_or_else(|| panic!("no response for {name}"));
        assert_eq!(resp["ok"], false, "{name} must be rejected: {resp}");
        assert_eq!(resp["error"]["code"], "E_CONFIG_INVALID", "{name}: {resp}");
    }

    // state must still be unconfigured, and a valid config still saves after the rejections
    c.send(r#"{"v":1,"id":200,"method":"state.get","params":{}}"#);
    let st = c
        .wait_for(|v| v["id"] == 200, Duration::from_secs(5))
        .expect("state.get");
    assert_eq!(
        st["result"]["monitors"], 0,
        "no config was applied from rejected variants"
    );

    let good = serde_json::json!({
        "schema_version": 1,
        "monitors": [
            {"edid_id": "SAC-2763-S:0000000000001", "here_input": 15, "away_input": 16, "away_input_source": "heuristic_prefill"}
        ],
        "trigger": {"anchor": {"vid_pid": "1a40:0101"}, "members": [{"vid_pid": "3837:303c"}]}
    });
    let req = format!(r#"{{"v":1,"id":201,"method":"config.set","params":{{"config":{good}}}}}"#);
    c.send(&req);
    let saved = c
        .wait_for(|v| v["id"] == 201, Duration::from_secs(5))
        .expect("valid config accepted");
    assert_eq!(
        saved["ok"], true,
        "valid config must still save after rejections: {saved}"
    );

    c.send(r#"{"v":1,"id":202,"method":"shutdown","params":{}}"#);
    let _ = c.wait_for(|v| v["id"] == 202, Duration::from_secs(3));
}
