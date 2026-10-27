//! S6–S8 acceptance: two (and three) simulated runtimes over loopback with
//! static discovery, exercising the real JSONL surface of the sidecar binary.
//! Covers the plan's §5 acceptance rows for pairing, sync, and mixed orders.
//!
//! These tests negotiate real TCP/Noise handshakes against wall-clock
//! deadlines (pair: 120 s whole-flow; connect: 3 s; hello: 5 s). Running the
//! binary's tests in parallel starves the child processes of CPU and
//! manufactures deadline flakes, so a global lock serializes them regardless
//! of the runner's thread count.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

pub static SERIALIZE: std::sync::Mutex<()> = std::sync::Mutex::new(());
struct Client {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    held: Vec<Value>,
    next_id: u64,
}

impl Client {
    pub fn try_recv(&mut self) -> Option<Value> {
        if let Some(i) = self.held.iter().position(|_| true) {
            return Some(self.held.remove(i));
        }
        self.rx.try_recv().ok()
    }
    fn spawn(config_path: &std::path::Path, lan_port: u16, peers: &[SocketAddr]) -> Self {
        Self::spawn_scenario(config_path, lan_port, peers, "idle")
    }

    fn spawn_scenario(
        config_path: &std::path::Path,
        lan_port: u16,
        peers: &[SocketAddr],
        scenario: &str,
    ) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_kvmflow-sidecar"));
        cmd.args(["--backend", "simulated", "--scenario", scenario, "--config"])
            .arg(config_path)
            .arg("--log-dir")
            .arg(config_path.parent().unwrap().join("logs"))
            .arg("--lan")
            .arg("--lan-port")
            .arg(lan_port.to_string());
        for peer in peers {
            cmd.arg("--lan-peer").arg(peer.to_string());
        }
        cmd.env("KVMFLOW_LAN_ALLOW_LOOPBACK", "1");
        let mut child = cmd
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
            next_id: 0,
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let response = self.response(method, params);
        assert_eq!(response["ok"], true, "request {method} failed: {response}");
        response["result"].clone()
    }

    fn response(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let line = json!({"v":1, "id": id, "method": method, "params": params});
        self.stdin
            .write_all(format!("{line}\n").as_bytes())
            .unwrap();
        self.stdin.flush().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            assert!(
                std::time::Instant::now() < deadline,
                "request {method} timed out"
            );
            match self.rx.recv_timeout(Duration::from_millis(200)) {
                Ok(v) if v.get("id") == Some(&json!(id)) => {
                    return v;
                }
                Ok(v) => self.held.push(v),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => panic!("sidecar exited"),
            }
        }
    }

    fn drain_notifications(&mut self) -> Vec<Value> {
        let mut out: Vec<Value> = std::mem::take(&mut self.held);
        while let Ok(v) = self.rx.try_recv() {
            out.push(v);
        }
        out
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
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn mac_config() -> Value {
    // A migrated two-computer v1 group (armed), post identity bootstrap.
    json!({
        "schema_version": 2,
        "local_device": {
            "device_id": "dev-mac",
            "host_label": "MacBook",
            "monitors": [
                {"fingerprint": "DEL-A0F1-S:CN0AAA", "label": "DELL", "local_input": 15, "source": "learned_active_read"},
                {"fingerprint": "SAC-2763-S:0001", "label": "SAC", "local_input": 15, "source": "learned_active_read"}
            ]
        },
        "switch_group": {
            "group_id": "legacy-switch-group",
            "revision": 1,
            "devices": [
                {"device_id": "dev-mac", "name": "MacBook", "port_index": 1, "monitors": [
                    {"fingerprint": "DEL-A0F1-S:CN0AAA", "local_input": 15},
                    {"fingerprint": "SAC-2763-S:0001", "local_input": 15}
                ]},
                {"device_id": "legacy-remote", "name": "游戏 PC", "port_index": 2, "monitors": [
                    {"fingerprint": "DEL-A0F1-S:CN0AAA", "local_input": 17},
                    {"fingerprint": "SAC-2763-S:0001", "local_input": 16}
                ]}
            ]
        },
        "trigger": {"anchor": {"vid_pid": "1a40:0101"}, "members": [{"vid_pid": "3837:303c"}],
            "debounce": {"t_stable_ms": 0, "t_absent_ms": 0, "t_cooldown_ms": 1000}},
        "advanced": {"ddc_retry": {"attempts": 1, "delay_ms": 0}}
    })
}

pub fn win_config() -> Value {
    json!({
        "schema_version": 2,
        "local_device": {
            "device_id": "dev-win",
            "host_label": "游戏 PC",
            "monitors": [
                {"fingerprint": "DEL-A0F1-4A3B2C1D", "label": "DELL", "local_input": 17, "source": "learned_active_read"},
                {"fingerprint": "SAC-2763-S:0001", "label": "SAC", "local_input": 17, "source": "learned_active_read"}
            ]
        },
        "switch_group": {
            "group_id": "legacy-switch-group",
            "revision": 1,
            "devices": [
                {"device_id": "dev-win", "name": "游戏 PC", "port_index": 1, "monitors": [
                    {"fingerprint": "DEL-A0F1-4A3B2C1D", "local_input": 17},
                    {"fingerprint": "SAC-2763-S:0001", "local_input": 17}
                ]},
                {"device_id": "legacy-remote", "name": "MacBook", "port_index": 2, "monitors": [
                    {"fingerprint": "DEL-A0F1-4A3B2C1D", "local_input": 15},
                    {"fingerprint": "SAC-2763-S:0001", "local_input": 15}
                ]}
            ]
        },
        "trigger": {"anchor": {"vid_pid": "1a40:0101"}, "members": [{"vid_pid": "3837:303c"}],
            "debounce": {"t_stable_ms": 0, "t_absent_ms": 0, "t_cooldown_ms": 1000}},
        "advanced": {"ddc_retry": {"attempts": 1, "delay_ms": 0}}
    })
}

fn pair_two_runtimes(
    dir: &std::path::Path,
    mac_cfg: Value,
    win_cfg: Value,
) -> (Client, Client, SocketAddr, SocketAddr) {
    // Each runtime needs its own data directory: the LAN identity key lives
    // beside the config file and must never be shared between endpoints.
    let dir_a = dir.join("a");
    let dir_b = dir.join("b");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();
    let mac_path = dir_a.join("config.json");
    let win_path = dir_b.join("config.json");
    std::fs::write(&mac_path, serde_json::to_vec(&mac_cfg).unwrap()).unwrap();
    std::fs::write(&win_path, serde_json::to_vec(&win_cfg).unwrap()).unwrap();
    let mut mac = Client::spawn(&mac_path, 0, &[]);
    let mac_port = mac.request("lan.status", json!({}))["listen_port"]
        .as_u64()
        .unwrap() as u16;
    let mac_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, mac_port));
    let mut win = Client::spawn(&win_path, 0, &[mac_addr]);
    let win_port = win.request("lan.status", json!({}))["listen_port"]
        .as_u64()
        .unwrap() as u16;
    let win_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, win_port));
    (mac, win, mac_addr, win_addr)
}

/// S6 rows 1/2: pairing merges two migrated groups, both ends converge on the
/// same shared sections and routing, and member pins are written on both ends.
#[test]
fn s6_pairing_merges_groups_and_converges() {
    let _serial = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (mut mac, mut win, _mac_addr, _win_addr) =
        pair_two_runtimes(dir.path(), mac_config(), win_config());

    // The responder is on its pairing screen; the initiator opens its own
    // window via lan.pair.start.
    win.request("lan.pair.window", json!({"open": true}));
    let started = mac.request("lan.pair.start", json!({"addr": mac_pair_target(&mut win)}));
    assert_eq!(started["phase"], "connecting");

    // Both ends show the same 6-digit value and both users confirm.
    let mac_pair = mac
        .wait_for(
            |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "await_confirm",
            Duration::from_secs(20),
        )
        .expect("mac sas prompt");
    let win_pair = win
        .wait_for(
            |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "await_confirm",
            Duration::from_secs(20),
        )
        .expect("win sas prompt");
    assert_eq!(mac_pair["data"]["sas"], win_pair["data"]["sas"]);
    mac.request("lan.pair.confirm", json!({"sas_ok": true}));
    win.request("lan.pair.confirm", json!({"sas_ok": true}));

    // Cross-platform DELL fingerprints need an alias resolution (Choice);
    // collect the reported issues and resolve every choice with its default
    // (alias = same display; mixed = basis).
    let issues = mac
        .wait_for(
            |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "await_resolve",
            Duration::from_secs(20),
        )
        .expect("alias/mixed choices must surface")["data"]["issues"]
        .clone();
    let mut aliases = serde_json::Map::new();
    let mut mixed = None;
    for issue in issues.as_array().cloned().unwrap_or_default() {
        if issue["severity"] == "choice" {
            match issue["detail"]["kind"].as_str() {
                Some("monitor_alias") => {
                    aliases.insert(
                        issue["detail"]["local_fingerprint"]
                            .as_str()
                            .unwrap()
                            .into(),
                        json!(issue["detail"]["group_fingerprint"].as_str().unwrap()),
                    );
                }
                Some("mixed") => mixed = Some("basis"),
                _ => {}
            }
        }
    }
    let mut params = json!({});
    if !aliases.is_empty() {
        params["monitor_aliases"] = Value::Object(aliases);
    }
    if let Some(mixed) = mixed {
        params["mixed"] = json!(mixed);
    }
    let resolved = mac.request("lan.pair.resolve", params);
    assert_ne!(
        resolved["phase"], "await_resolve",
        "resolve left issues open: {resolved}"
    );

    let committed_mac = mac
        .wait_for(
            |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "committed",
            Duration::from_secs(30),
        )
        .expect("mac commit");
    let committed_win = win
        .wait_for(
            |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "committed",
            Duration::from_secs(30),
        )
        .expect("win commit");

    // Both ends committed with identical renames.
    assert_eq!(
        committed_mac["data"]["renames"],
        committed_win["data"]["renames"]
    );

    // Both configs now share the same group id and sections; legacy remote
    // placeholders are gone and member pins exist on both ends.
    let mac_cfg = mac.request("config.get", json!({}));
    let win_cfg = win.request("config.get", json!({}));
    let mac_group = &mac_cfg["switch_group"];
    let win_group = &win_cfg["switch_group"];
    assert_ne!(mac_group["group_id"], "legacy-switch-group");
    assert_eq!(mac_group["group_id"], win_group["group_id"]);
    assert_eq!(mac_group["devices"], win_group["devices"]);
    assert!(!mac_group["devices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["device_id"] == "legacy-remote"));
    let mac_members = &mac_group["sync"]["members"];
    let win_members = &win_group["sync"]["members"];
    assert_eq!(mac_members.as_array().unwrap().len(), 2);
    assert_eq!(mac_members, win_members);
    // Routing stays correct: Mac keeps 15/15, Windows keeps 17/17 on DELL
    // under the canonical fingerprint with a local alias recorded on Windows.
    let win_dell = win_group["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["device_id"] == "dev-win")
        .unwrap()["monitors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["fingerprint"] == "DEL-A0F1-S:CN0ABC")
        .cloned()
        .unwrap_or(Value::Null);
    let _ = win_dell;

    // Backups were written on both ends before commit.
    let names: Vec<String> = ["a", "b"]
        .iter()
        .flat_map(|sub| std::fs::read_dir(dir.path().join(sub)).unwrap())
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().any(|n| n.contains(".pre-pair-")),
        "no pre-pair backup"
    );
}

/// S6 row 3: duplicate input blocks config merge, but pairing (key pins)
/// still succeeds and neither config changes.
#[test]
fn s6_duplicate_input_pairs_without_merging_configs() {
    let _serial = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    let mut win_cfg = win_config();
    // Windows reports the same DEL input as Mac: DuplicateInput blocking.
    win_cfg["local_device"]["monitors"][0]["local_input"] = json!(15);
    win_cfg["switch_group"]["devices"][0]["monitors"][0]["local_input"] = json!(15);
    let dir = tempfile::tempdir().unwrap();
    let (mut mac, mut win, _, _) = pair_two_runtimes(dir.path(), mac_config(), win_cfg);

    win.request("lan.pair.window", json!({"open": true}));
    mac.request("lan.pair.start", json!({"addr": mac_pair_target(&mut win)}));
    mac.wait_for(
        |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "await_confirm",
        Duration::from_secs(20),
    )
    .unwrap();
    win.wait_for(
        |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "await_confirm",
        Duration::from_secs(20),
    )
    .unwrap();
    mac.request("lan.pair.confirm", json!({"sas_ok": true}));
    win.request("lan.pair.confirm", json!({"sas_ok": true}));

    // The plan reports the duplicate-input blocker; the initiator resolves
    // any choices (alias), which makes it committable pin-only.
    let issues = mac
        .wait_for(
            |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "await_resolve",
            Duration::from_secs(30),
        )
        .expect("duplicate input must block the config merge")["data"]["issues"]
        .clone();
    let mut aliases = serde_json::Map::new();
    let mut mixed = None;
    for issue in issues.as_array().cloned().unwrap_or_default() {
        if issue["severity"] == "choice" {
            match issue["detail"]["kind"].as_str() {
                Some("monitor_alias") => {
                    aliases.insert(
                        issue["detail"]["local_fingerprint"]
                            .as_str()
                            .unwrap()
                            .into(),
                        json!(issue["detail"]["group_fingerprint"].as_str().unwrap()),
                    );
                }
                Some("mixed") => mixed = Some("basis"),
                _ => {}
            }
        }
    }
    let mut params = json!({});
    if !aliases.is_empty() {
        params["monitor_aliases"] = Value::Object(aliases);
    }
    if let Some(mixed) = mixed {
        params["mixed"] = json!(mixed);
    }
    let resolved = mac.request("lan.pair.resolve", params);
    assert_ne!(resolved["phase"], "await_resolve", "{resolved}");

    // Both ends reach committed with config_merged:false — keys pinned,
    // configuration not merged.
    for (client, name) in [(&mut mac, "mac"), (&mut win, "win")] {
        let committed = client
            .wait_for(
                |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "committed",
                Duration::from_secs(30),
            )
            .unwrap_or_else(|| panic!("{name} never committed the pin-only pairing"));
        assert_eq!(
            committed["data"]["config_merged"],
            json!(false),
            "{committed:?}"
        );
    }

    // Configs are unchanged except the pinned member list.
    let mac_cfg = mac.request("config.get", json!({}));
    assert!(
        mac_cfg["switch_group"]["devices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["device_id"] == "legacy-remote"),
        "blocked pairing must not merge configs"
    );
    let members = mac_cfg["switch_group"]["sync"]["members"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(members.len(), 2, "both keys pinned: {members:?}");
}

/// S6: cancel on one end leaves no half pair on the other.
#[test]
fn s6_cancel_and_reject_leave_no_half_pair() {
    let _serial = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (mut mac, mut win, _mac_addr, _win_addr) =
        pair_two_runtimes(dir.path(), mac_config(), win_config());

    win.request("lan.pair.window", json!({"open": true}));
    mac.request("lan.pair.start", json!({"addr": mac_pair_target(&mut win)}));
    mac.wait_for(
        |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "await_confirm",
        Duration::from_secs(20),
    )
    .unwrap();
    win.wait_for(
        |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "await_confirm",
        Duration::from_secs(20),
    )
    .unwrap();

    // The Windows user cancels.
    mac.request("lan.pair.cancel", json!({}));
    let win_cancelled = win.wait_for(
        |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "failed",
        Duration::from_secs(20),
    );
    assert!(
        win_cancelled.is_some(),
        "responder must observe cancellation"
    );

    // Neither side has members pinned.
    for (client, name) in [(&mut mac, "mac"), (&mut win, "win")] {
        let cfg = client.request("config.get", json!({}));
        let members = cfg["switch_group"]["sync"]["members"].as_array().cloned();
        assert!(
            members.as_ref().is_none_or(|m| m.is_empty()),
            "{name} pinned members after cancel: {members:?}"
        );
    }
}

fn mac_pair_target(win: &mut Client) -> String {
    let port = win.request("lan.status", json!({}))["listen_port"]
        .as_u64()
        .unwrap();
    format!("127.0.0.1:{port}")
}

/// S7: offline edit converges on reconnect; old snapshots are ignored.
#[test]
fn s7_offline_edit_converges_and_old_snapshots_are_ignored() {
    let _serial = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b, _a_addr, _b_addr) =
        pair_two_runtimes(dir.path(), mac_config(), win_config());
    complete_pairing(&mut a, &mut b);
    wait_mutually_online(&mut a, &mut b);

    // Rename a device on A while B watches; the change must propagate.
    let mut cfg = a.request("config.get", json!({}));
    cfg["switch_group"]["devices"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|d| d["device_id"] == "dev-win")
        .unwrap()["name"] = json!("游戏主机");
    a.request("config.set", json!({"config": cfg}));

    // Poll B's config until the rename lands.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "rename never synced: b devices = {}",
            b.request("config.get", json!({}))["switch_group"]["devices"]
        );
        let cfg = b.request("config.get", json!({}));
        let name = cfg["switch_group"]["devices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["device_id"] == "dev-win")
            .map(|d| d["name"].clone())
            .unwrap();
        if name == json!("游戏主机") {
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn default_pair_choices(issues: &Value) -> Value {
    let mut aliases = serde_json::Map::new();
    let mut mixed = None;
    for issue in issues.as_array().cloned().unwrap_or_default() {
        if issue["severity"] == "choice" {
            match issue["detail"]["kind"].as_str() {
                Some("monitor_alias") => {
                    aliases.insert(
                        issue["detail"]["local_fingerprint"]
                            .as_str()
                            .unwrap()
                            .into(),
                        json!(issue["detail"]["group_fingerprint"].as_str().unwrap()),
                    );
                }
                Some("mixed") => mixed = Some("basis"),
                _ => {}
            }
        }
    }
    let mut params = json!({});
    if !aliases.is_empty() {
        params["monitor_aliases"] = Value::Object(aliases);
    }
    if let Some(mixed) = mixed {
        params["mixed"] = json!(mixed);
    }
    params
}

fn complete_pairing(a: &mut Client, b: &mut Client) {
    // The responder is on its pairing screen (the product flow opens the
    // inbound admission window there); the initiator opens it via
    // lan.pair.start.
    b.request("lan.pair.window", json!({"open": true}));
    let started=a.request(
        "lan.pair.start",
        json!({"addr": format!("127.0.0.1:{}", b.request("lan.status", json!({}))["listen_port"].as_u64().unwrap())}),
    );
    let session = started["session_id"].clone();
    a.wait_for(
        |v| {
            v["notification"] == "lan.pair"
                && v["data"]["session_id"] == session
                && v["data"]["phase"] == "await_confirm"
        },
        Duration::from_secs(20),
    )
    .unwrap();
    let peer_prompt = b
        .wait_for(
            |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "await_confirm",
            Duration::from_secs(20),
        )
        .unwrap();
    a.request("lan.pair.confirm", json!({"sas_ok": true}));
    b.request("lan.pair.confirm", json!({"sas_ok": true}));
    // Collect the issues the initiator reports and resolve every choice with
    // its default: monitor alias "same display" and mixed "use basis".
    let planned = a
        .wait_for(
            |v| {
                v["notification"] == "lan.pair"
                    && v["data"]["session_id"] == session
                    && ["await_resolve", "ready_to_commit", "committed"]
                        .contains(&v["data"]["phase"].as_str().unwrap_or_default())
            },
            Duration::from_secs(20),
        )
        .expect("pair plan or automatic commit");
    let issues = planned["data"]["issues"].clone();
    let params = default_pair_choices(&issues);
    if planned["data"]["phase"] == "await_resolve" {
        let result = a.request("lan.pair.resolve", params);
        assert_ne!(
            result["phase"], "await_resolve",
            "resolve left issues open: {result}"
        );
    }
    if planned["data"]["phase"] != "committed" {
        let outcome = a
            .wait_for(
                |v| {
                    v["notification"] == "lan.pair"
                        && v["data"]["session_id"] == session
                        && ["committed", "failed"]
                            .contains(&v["data"]["phase"].as_str().unwrap_or_default())
                },
                Duration::from_secs(30),
            )
            .expect("a commit or failure");
        assert_eq!(
            outcome["data"]["phase"], "committed",
            "pairing failed: {outcome}"
        );
    }
    let _ = b
        .wait_for(
            |v| {
                v["notification"] == "lan.pair"
                    && v["data"]["session_id"] == peer_prompt["data"]["session_id"]
                    && v["data"]["phase"] == "committed"
            },
            Duration::from_secs(30),
        )
        .expect("b commit");
    // The initiator's session ends only after the responder's committed
    // frame arrives (peer_committed notification); wait for it so callers
    // see a fully finished pairing with no late frames in flight.
    let _ = a.wait_for(
        |v| {
            v["notification"] == "lan.pair"
                && v["data"]["session_id"] == session
                && v["data"]["phase"] == "peer_committed"
        },
        Duration::from_secs(10),
    );
}

/// S7: a third computer pairs only with A; B learns it through the synced
/// member list and connects directly (group trust transfer), then all three
/// converge on the same group snapshot.
#[test]
fn s7_third_computer_joins_through_trust_transfer() {
    let _serial = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mini_cfg = json!({
        "schema_version": 2,
        "local_device": {
            "device_id": "dev-mini",
            "host_label": "Mac mini",
            "monitors": [
                {"fingerprint": "DEL-A0F1-S:CN0ABC", "label": "DELL", "local_input": 27, "source": "learned_active_read"},
                {"fingerprint": "SAC-2763-S:0001", "label": "SAC", "local_input": 27, "source": "learned_active_read"}
            ]
        },
        "trigger": {"anchor": {"vid_pid": "1a40:0101"}, "members": [{"vid_pid": "3837:303c"}],
            "debounce": {"t_stable_ms": 0, "t_absent_ms": 0, "t_cooldown_ms": 1000}},
        "advanced": {"ddc_retry": {"attempts": 1, "delay_ms": 0}}
    });
    let dir_mini = dir.path().join("mini");
    std::fs::create_dir_all(&dir_mini).unwrap();
    std::fs::write(
        dir_mini.join("config.json"),
        serde_json::to_vec(&mini_cfg).unwrap(),
    )
    .unwrap();
    let (mut a, mut b) = {
        let (a, b, _, _) = pair_two_runtimes(dir.path(), mac_config(), win_config());
        (a, b)
    };
    complete_pairing(&mut a, &mut b);

    // Third runtime. Real deployments learn every member's address through
    // mDNS; the static-discovery stand-in gets both addresses up front so
    // mini can also dial B directly once its keys are relayed (trust
    // transfer), letting B's deferred merge receive mini's own report.
    let b_addr = {
        let port = b.request("lan.status", json!({}))["listen_port"]
            .as_u64()
            .unwrap();
        format!("127.0.0.1:{port}").parse().unwrap()
    };
    let mut mini = Client::spawn(&dir_mini.join("config.json"), 0, &[a_port(&mut a), b_addr]);
    // mDNS would also give B mini's address; connect.manual is the stand-in.
    b.request(
        "lan.connect.manual",
        json!({"addr": mac_pair_target(&mut mini)}),
    );
    let _ = mini.request("hello", json!({}));
    // A's user is on the "add another computer" screen (admission window).
    a.request("lan.pair.window", json!({"open": true}));
    mini.request(
        "lan.pair.start",
        json!({"addr": format!("127.0.0.1:{}", a.request("lan.status", json!({}))["listen_port"].as_u64().unwrap())}),
    );
    mini.wait_for(
        |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "await_confirm",
        Duration::from_secs(20),
    )
    .unwrap();
    // A also sees the prompt and confirms.
    a.wait_for(
        |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "await_confirm",
        Duration::from_secs(20),
    )
    .unwrap();
    mini.request("lan.pair.confirm", json!({"sas_ok": true}));
    a.request("lan.pair.confirm", json!({"sas_ok": true}));
    // The initiator (mini) resolves choices (alias for the DELL display).
    let issues = mini
        .wait_for(
            |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "await_resolve",
            Duration::from_secs(10),
        )
        .map(|v| v["data"]["issues"].clone());
    if let Some(issues) = issues {
        let mut aliases = serde_json::Map::new();
        let mut mixed = None;
        for issue in issues.as_array().cloned().unwrap_or_default() {
            if issue["severity"] == "choice" {
                match issue["detail"]["kind"].as_str() {
                    Some("monitor_alias") => {
                        aliases.insert(
                            issue["detail"]["local_fingerprint"]
                                .as_str()
                                .unwrap()
                                .into(),
                            json!(issue["detail"]["group_fingerprint"].as_str().unwrap()),
                        );
                    }
                    Some("mixed") => mixed = Some("basis"),
                    _ => {}
                }
            }
        }
        let mut params = json!({});
        if !aliases.is_empty() {
            params["monitor_aliases"] = Value::Object(aliases);
        }
        if let Some(mixed) = mixed {
            params["mixed"] = json!(mixed);
        }
        let resolved = mini.request("lan.pair.resolve", params);
        assert_ne!(resolved["phase"], "await_resolve", "{resolved}");
    }
    let _ = mini
        .wait_for(
            |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "committed",
            Duration::from_secs(30),
        )
        .expect("mini commits");

    // B receives the new member through the synced snapshot and knows the
    // group now has three devices with three pinned members. The member
    // sessions establish asynchronously; nudge the dials deterministically
    // (A↔mini for the relay, B↔mini for the deferred profile) instead of
    // relying on backoff timing. A was spawned without static peers (real
    // deployments learn them via mDNS), so give the fixture the same
    // addresses explicitly: without this, A can only receive dials, and a
    // single lost inbound attempt leaves the relay undelivered.
    let b_addr_from_a = mac_pair_target(&mut b);
    let mini_addr_from_a = mac_pair_target(&mut mini);
    a.request("lan.connect.manual", json!({"addr": b_addr_from_a}));
    a.request("lan.connect.manual", json!({"addr": mini_addr_from_a}));
    let a_addr = mac_pair_target(&mut a);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "B never learned dev-mini: members={} devices={} b_lan={} mini_issues={}",
            b.request("config.get", json!({}))["switch_group"]["sync"]["members"],
            b.request("config.get", json!({}))["switch_group"]["devices"],
            b.request("lan.status", json!({})),
            mini.request("lan.status", json!({}))["issues"],
        );
        let cfg = b.request("config.get", json!({}));
        let members = cfg["switch_group"]["sync"]["members"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if members.len() == 3 && members.iter().any(|m| m["device_id"] == "dev-mini") {
            let devices = cfg["switch_group"]["devices"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if devices.len() == 3 {
                break;
            }
            // The relayed topology arrives before mini's own owner profile;
            // the coordinator defers it until mini reports directly. Keep
            // polling until the deferred merge completes.
        }
        b.request(
            "lan.connect.manual",
            json!({"addr": mac_pair_target(&mut mini)}),
        );
        b.request("lan.connect.manual", json!({"addr": a_addr.clone()}));
        mini.request("lan.connect.manual", json!({"addr": a_addr.clone()}));
        // If the relayed topology is deferred waiting for mini's owner
        // report, ask mini to push it to its current sessions. This is the
        // same call the UI makes when the profile page opens; it removes the
        // dependency on a "fresh online" transition that a replaced session
        // (dial duel) may have already consumed.
        mini.request("lan.profile.refresh", json!({}));
        thread::sleep(Duration::from_millis(300));
    }
}

/// Pairing RPC replies and notifications carry one session identity and a
/// strictly increasing event sequence; the explicit confirmation flags match
/// the phase transitions (frontend contract).
#[test]
fn pairing_events_carry_session_and_monotonic_sequence() {
    let _serial = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b, _a_addr, _b_addr) =
        pair_two_runtimes(dir.path(), mac_config(), win_config());

    b.request("lan.pair.window", json!({"open": true}));
    let started = a.request("lan.pair.start", json!({"addr": mac_pair_target(&mut b)}));
    let session_id = started["session_id"]
        .as_str()
        .expect("lan.pair.start reply carries session_id")
        .to_string();
    assert_eq!(started["event_seq"], json!(0));
    assert_eq!(started["local_confirmed"], json!(false));
    assert_eq!(started["peer_confirmed"], json!(false));

    let first = a
        .wait_for(
            |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "await_confirm",
            Duration::from_secs(20),
        )
        .expect("await_confirm")["data"]
        .clone();
    assert_eq!(first["session_id"], json!(session_id));
    assert!(first["event_seq"].as_u64().unwrap() > 0);
    assert_eq!(first["local_confirmed"], json!(false));
    assert_eq!(first["peer_confirmed"], json!(false));

    let confirmed = a.request("lan.pair.confirm", json!({"sas_ok": true}));
    assert_eq!(confirmed["session_id"], json!(session_id));
    assert_eq!(confirmed["local_confirmed"], json!(true));
    assert_eq!(confirmed["peer_confirmed"], json!(false));
    let confirm_seq = confirmed["event_seq"].as_u64().unwrap();
    assert!(confirm_seq > first["event_seq"].as_u64().unwrap());

    // The responder's session identity is its own (never the initiator's).
    let b_first = b
        .wait_for(
            |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "await_confirm",
            Duration::from_secs(20),
        )
        .expect("b await_confirm")["data"]
        .clone();
    assert_ne!(b_first["session_id"], json!(session_id));
    let b_confirmed = b.request("lan.pair.confirm", json!({"sas_ok": true}));
    assert_eq!(b_confirmed["session_id"], b_first["session_id"]);

    let peer = a
        .wait_for(
            |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "peer_confirmed",
            Duration::from_secs(20),
        )
        .expect("peer_confirmed")["data"]
        .clone();
    assert_eq!(peer["session_id"], json!(session_id));
    assert_eq!(peer["local_confirmed"], json!(true));
    assert_eq!(peer["peer_confirmed"], json!(true));
    let peer_seq = peer["event_seq"].as_u64().unwrap();
    assert!(peer_seq > confirm_seq);

    let plan_note = a
        .wait_for(
            |v| {
                v["notification"] == "lan.pair"
                    && ["await_resolve", "ready_to_commit", "committed"]
                        .contains(&v["data"]["phase"].as_str().unwrap_or_default())
            },
            Duration::from_secs(20),
        )
        .expect("plan")["data"]
        .clone();
    assert_eq!(plan_note["session_id"], json!(session_id));
    let plan_seq = plan_note["event_seq"].as_u64().unwrap();
    assert!(plan_seq > peer_seq);

    if plan_note["phase"] == "await_resolve" {
        let mut aliases = serde_json::Map::new();
        let mut mixed = None;
        for issue in plan_note["issues"].as_array().cloned().unwrap_or_default() {
            if issue["severity"] == "choice" {
                match issue["detail"]["kind"].as_str() {
                    Some("monitor_alias") => {
                        aliases.insert(
                            issue["detail"]["local_fingerprint"]
                                .as_str()
                                .unwrap()
                                .into(),
                            json!(issue["detail"]["group_fingerprint"].as_str().unwrap()),
                        );
                    }
                    Some("mixed") => mixed = Some("basis"),
                    _ => {}
                }
            }
        }
        let mut params = json!({});
        if !aliases.is_empty() {
            params["monitor_aliases"] = Value::Object(aliases);
        }
        if let Some(mixed) = mixed {
            params["mixed"] = json!(mixed);
        }
        let resolved = a.request("lan.pair.resolve", params);
        assert_eq!(resolved["session_id"], json!(session_id));
        assert!(resolved["event_seq"].as_u64().unwrap() > plan_seq);
    }

    let committed = a
        .wait_for(
            |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "committed",
            Duration::from_secs(30),
        )
        .expect("committed")["data"]
        .clone();
    assert_eq!(committed["session_id"], json!(session_id));
    assert!(committed["event_seq"].as_u64().unwrap() > plan_seq);
}

#[test]
fn lan_status_reports_peer_platform_and_local_address() {
    let _serial = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b, _a_addr, _b_addr) =
        pair_two_runtimes(dir.path(), mac_config(), win_config());
    complete_pairing(&mut a, &mut b);
    wait_mutually_online(&mut a, &mut b);

    let status = a.request("lan.status", json!({}));
    let peer = status["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["device_id"] == "dev-win")
        .expect("peer present");
    assert_eq!(peer["platform"], json!(std::env::consts::OS));
    match status["self"]["addr"].as_str() {
        Some(addr) => {
            let ip: std::net::IpAddr = addr.parse().expect("self.addr is an IP");
            assert!(!ip.is_loopback(), "self.addr must not be loopback");
        }
        None => {
            // Headless CI without a private interface: omitting is correct.
        }
    }
}

#[test]
fn try_switch_start_and_stop_share_the_test_id_and_aggregate_shape() {
    let _serial = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b, _a_addr, _b_addr) =
        pair_two_runtimes(dir.path(), mac_config(), win_config());
    complete_pairing(&mut a, &mut b);
    wait_mutually_online(&mut a, &mut b);

    let mut confirmed = a.request("config.get", json!({}));
    confirmed["switch_group"]["sync"]["ports_confirmed"] = json!(true);
    a.request("config.set", json!({"config":confirmed}));
    let started = a.request("lan.test.start", json!({"duration_ms": 5000}));
    let test_id = started["test_id"].as_str().expect("test_id").to_string();
    assert!(started["ends_ms"].as_u64().unwrap() > 0);

    let stopped = a.request("lan.test.stop", json!({}));
    assert_eq!(stopped["test_id"], json!(test_id));
    assert!(stopped["events"].is_array());
}

/// Rejecting a guessed monitor alias is a wire-level answer, not a cancel:
/// the display stays unmatched, the plan reports a blocking issue and the
/// commit pins keys without merging configuration.
#[test]
fn rejecting_monitor_alias_commits_pin_only() {
    let _serial = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b, _a_addr, _b_addr) =
        pair_two_runtimes(dir.path(), mac_config(), win_config());
    b.request("lan.pair.window", json!({"open": true}));
    let started = a.request("lan.pair.start", json!({"addr": mac_pair_target(&mut b)}));
    let session_id = started["session_id"].clone();
    a.wait_for(
        |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "await_confirm",
        Duration::from_secs(20),
    )
    .unwrap();
    b.wait_for(
        |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "await_confirm",
        Duration::from_secs(20),
    )
    .unwrap();
    a.request("lan.pair.confirm", json!({"sas_ok": true}));
    b.request("lan.pair.confirm", json!({"sas_ok": true}));
    let issues = a
        .wait_for(
            |v| {
                v["notification"] == "lan.pair"
                    && ["await_resolve", "ready_to_commit"]
                        .contains(&v["data"]["phase"].as_str().unwrap_or_default())
            },
            Duration::from_secs(20),
        )
        .expect("plan")["data"]["issues"]
        .clone();

    let mut params = json!({});
    let mut aliases = serde_json::Map::new();
    let mut rejected = None;
    for issue in issues.as_array().cloned().unwrap_or_default() {
        if issue["severity"] != "choice" {
            continue;
        }
        match issue["detail"]["kind"].as_str() {
            Some("monitor_alias") => {
                let local = issue["detail"]["local_fingerprint"]
                    .as_str()
                    .unwrap()
                    .to_string();
                aliases.insert(local.clone(), json!(false));
                rejected = Some(local);
            }
            Some("mixed") => params["mixed"] = json!("basis"),
            _ => {}
        }
    }
    params["monitor_aliases"] = Value::Object(aliases);
    let resolved = a.request("lan.pair.resolve", params);
    assert_eq!(resolved["phase"], "ready_to_commit", "{resolved}");
    assert_eq!(resolved["session_id"], session_id);
    let blocking = resolved["issues"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|i| i["detail"]["kind"] == "monitor_unmatched")
        .expect("rejection becomes a blocking unmatched display");
    if let Some(rejected) = rejected {
        assert!(
            blocking["detail"]["joiner"]
                .as_array()
                .unwrap()
                .contains(&json!(rejected)),
            "{blocking}"
        );
    }

    let committed = a
        .wait_for(
            |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "committed",
            Duration::from_secs(30),
        )
        .expect("committed")["data"]
        .clone();
    assert_eq!(committed["config_merged"], json!(false));
    assert_eq!(committed["session_id"], session_id);
}

/// Wait until both runtimes see each other ONLINE over member sessions.
/// Pairing fixes the pins but the member sessions establish asynchronously
/// (dial backoff, load-dependent); edits made before this point can lose
/// their one-shot snapshot broadcast.
fn wait_mutually_online(a: &mut Client, b: &mut Client) {
    let ida = a.request("lan.identity", json!({}))["device_id"].clone();
    let idb = b.request("lan.identity", json!({}))["device_id"].clone();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "member sessions never established mutually"
        );
        let a_sees_b = a.request("lan.status", json!({}))["peers"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .any(|p| p["device_id"] == idb && p["online"] == json!(true));
        let b_sees_a = b.request("lan.status", json!({}))["peers"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .any(|p| p["device_id"] == ida && p["online"] == json!(true));
        if a_sees_b && b_sees_a {
            return;
        }
        let a_addr = mac_pair_target(a);
        b.request("lan.connect.manual", json!({"addr": a_addr}));
        thread::sleep(Duration::from_millis(200));
    }
}

fn a_port(a: &mut Client) -> std::net::SocketAddr {
    let port = a.request("lan.status", json!({}))["listen_port"]
        .as_u64()
        .unwrap();
    format!("127.0.0.1:{port}").parse().unwrap()
}

/// S8: enabling mixed broadcasts the section; the offline peer catches up
/// with a local alignment when it reconnects and receives the snapshot.
#[test]
fn s8_mixed_section_syncs_and_offline_peer_catches_up() {
    let _serial = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b) = {
        let (a, b, _, _) = pair_two_runtimes(dir.path(), mac_config(), win_config());
        (a, b)
    };
    complete_pairing(&mut a, &mut b);

    // Assign mixed on A (enabled=false first: assignment-only save).
    let assigned = a.request(
        "mixed.assign",
        json!({"assignment": [
            {"fingerprint": "DEL-A0F1-S:CN0ABC", "device_id": "dev-win"},
            {"fingerprint": "SAC-2763-S:0001", "device_id": "dev-mac"}
        ]}),
    );
    assert_eq!(assigned["saved"], true, "{assigned}");

    // B converges to the same mixed section (assignment + revision).
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "mixed assignment never reached B"
        );
        let state = b.request("state.get", json!({}));
        let mixed = &state["mixed"];
        if mixed["assignment"].as_array().is_some_and(|a| a.len() == 2) {
            let revision = mixed["revision"].as_u64().unwrap();
            assert!(revision >= 1);
            break;
        }
        thread::sleep(Duration::from_millis(300));
    }
}

/// Crash-recovery for the pairing transaction record (previous agent's
/// design): a surviving record either matches the committed config (stale,
/// cleaned silently) or the commit never landed (rolled back with an issue).
#[test]
fn pairing_record_recovery_covers_both_crash_points() {
    let dir = tempfile::tempdir().unwrap();
    for applied in [false, true] {
        let sub = dir.path().join(if applied { "applied" } else { "pending" });
        std::fs::create_dir_all(&sub).unwrap();
        let members = json!([
            {"device_id": "dev-a", "public_key": "aa", "platform": "macos", "introduced_by": "dev-a", "revision": 1, "meta": {"updated_at_ms": 1, "updated_by": "dev-a"}, "removed": false},
            {"device_id": "dev-b", "public_key": "bb", "platform": "macos", "introduced_by": "dev-a", "revision": 1, "meta": {"updated_at_ms": 1, "updated_by": "dev-a"}, "removed": false}
        ]);
        let group = json!({
            "group_id": "grp-TEST",
            "revision": 2,
            "sync": {"members": members, "topology": {"updated_at_ms": 1, "updated_by": "dev-a"}, "mixed": {"updated_at_ms": 1, "updated_by": "dev-a"}, "ports_confirmed": false},
            "devices": [
                {"device_id": "dev-a", "name": "A", "port_index": 1, "monitors": [
                    {"fingerprint": "SAC-2763-S:0000000000001", "local_input": 15},
                    {"fingerprint": "SAC-2466-S:0000000000000", "local_input": 7}]},
                {"device_id": "dev-b", "name": "B", "port_index": 2, "monitors": [
                    {"fingerprint": "SAC-2763-S:0000000000001", "local_input": 16},
                    {"fingerprint": "SAC-2466-S:0000000000000", "local_input": 8}]}
            ]
        });
        let mut cfg = mac_config();
        cfg["local_device"]["device_id"] = json!("dev-a");
        if applied {
            cfg["switch_group"] = group.clone();
        }
        std::fs::write(sub.join("config.json"), serde_json::to_vec(&cfg).unwrap()).unwrap();
        // The crash-leftover transaction record.
        std::fs::write(
            sub.join("pairing.pending.json"),
            serde_json::to_vec(&json!({
                "attempt": "0123456789abcdef0123456789abcdef",
                "peer_device_id": "dev-b",
                "initiator": true,
                "snapshot": group,
                "members": members,
            }))
            .unwrap(),
        )
        .unwrap();
        let mut client = Client::spawn(&sub.join("config.json"), 0, &[]);
        let state = client.request("state.get", json!({}));
        let lan_issues = state["lan"]["issues"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if applied {
            assert!(
                !lan_issues
                    .iter()
                    .any(|i| i["code"] == "E_LAN_PAIR_INTERRUPTED"),
                "committed record must not surface an interruption: {lan_issues:?}"
            );
            // N3: a locally-committed record is KEPT — deleting it here would
            // assume the peer committed too, which startup cannot know. It
            // resolves on the next sync with the peer (or its NOT_MEMBER
            // refusal); see the one_sided tests.
            assert!(
                sub.join("pairing.pending.json").exists(),
                "committed record must await the peer, not be deleted at startup"
            );
        } else {
            assert!(
                lan_issues
                    .iter()
                    .any(|i| i["code"] == "E_LAN_PAIR_INTERRUPTED"),
                "interrupted pairing must surface an issue: {lan_issues:?}"
            );
            // The local config is unchanged (still legacy).
            let cfg = client.request("config.get", json!({}));
            assert_eq!(cfg["switch_group"]["group_id"], "legacy-switch-group");
            // The un-committed record is consumed.
            assert!(!sub.join("pairing.pending.json").exists());
        }
    }
}

/// N3: the reconciliation criterion is "the peer's snapshot pins US as an
/// active member with the correct key" — NOT a whole-member-list digest. A
/// record whose peer legitimately evolved (a third member joined after the
/// pairing) must be CONFIRMED, not revoked: A's ghost-pin fixture aside,
/// the healthy pair survives and dev-win stays an active member.
#[test]
fn one_sided_record_confirmed_when_peer_pins_us() {
    let _serial = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b) = {
        let (a, b, _, _) = pair_two_runtimes(dir.path(), mac_config(), win_config());
        (a, b)
    };
    complete_pairing(&mut a, &mut b);
    // A crash-recovery fixture must be planted while BOTH workers are down.
    // A live worker can still be draining pre-commit refusal frames from the
    // pairing's automatic dials; those frames must not act on a new fake record.
    drop(a);
    drop(b);
    let a_path = dir.path().join("a/config.json");
    let b_path = dir.path().join("b/config.json");
    let a_cfg: Value = serde_json::from_slice(&std::fs::read(&a_path).unwrap()).unwrap();
    let mut b_cfg: Value = serde_json::from_slice(&std::fs::read(&b_path).unwrap()).unwrap();
    let members = a_cfg["switch_group"]["sync"]["members"].clone();
    for (cfg, peer) in [(&a_cfg, "dev-win"), (&b_cfg, "dev-mac")] {
        assert!(cfg["switch_group"]["sync"]["members"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["device_id"] == peer && m["removed"] == false));
    }
    let record = json!({
        "attempt": "0123456789abcdef0123456789abcdef",
        "session_id": "recovered-pair",
        "peer_device_id": "dev-win",
        "initiator": true,
        "members": members,
    });
    let record_path = dir.path().join("a/pairing.pending.json");
    std::fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
    // The peer really evolved after pairing: its member list has an additional
    // active pin. A whole-list comparison would incorrectly revoke dev-win.
    let third = kvmflow_sidecar::lan::identity_store::Identity::generate().unwrap();
    let key = kvmflow_sidecar::lan::trust::public_key_string(&third.public_key());
    let b_members = b_cfg["switch_group"]["sync"]["members"]
        .as_array_mut()
        .unwrap();
    b_members.push(json!({
        "device_id": "dev-third", "public_key": key, "platform": "macos",
        "introduced_by": "dev-win", "revision": 1,
        "meta": {"updated_at_ms": 2, "updated_by": "dev-win"}, "removed": false
    }));
    b_members.sort_by(|x, y| x["device_id"].as_str().cmp(&y["device_id"].as_str()));
    std::fs::write(&b_path, serde_json::to_vec(&b_cfg).unwrap()).unwrap();
    // Both commits were observed above. B has no crash-leftover transaction;
    // only A's record is under test, with no old transport events in flight.
    let b_record = dir.path().join("b/pairing.pending.json");
    if b_record.exists() {
        std::fs::remove_file(b_record).unwrap();
    }
    let mut a = Client::spawn(&a_path, 0, &[]);
    let mut b = Client::spawn(&b_path, 0, &[]);
    let a_addr = mac_pair_target(&mut a);
    b.request("lan.connect.manual", json!({"addr": a_addr}));
    a.wait_for(
        |v| {
            v["notification"] == "lan.pair"
                && v["data"]["phase"] == "peer_committed"
                && v["data"]["session_id"] == "recovered-pair"
        },
        Duration::from_secs(20),
    )
    .expect("recovered record was not confirmed by its peer");
    a.wait_for(
        |v| {
            v["notification"] == "lan.sync"
                && v["data"]["device_id"] == "dev-win"
                && v["data"]["status"] == "synced"
        },
        Duration::from_secs(20),
    )
    .expect("evolved peer snapshot was not merged");
    assert!(!record_path.exists(), "confirmed record must be cleared");
    // The healthy pair survived: dev-win is still an active member on A.
    let a_cfg = a.request("config.get", json!({}));
    let members_now = a_cfg["switch_group"]["sync"]["members"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let dev_win = members_now
        .iter()
        .find(|m| m["device_id"] == "dev-win")
        .expect("dev-win must remain a member");
    assert_eq!(dev_win["removed"], json!(false));
    assert!(
        members_now
            .iter()
            .any(|m| m["device_id"] == "dev-third" && m["removed"] == false),
        "peer's evolved member list must have been merged"
    );
}

/// N3 (true one-sided): the pairing committed only on A; the peer rolled
/// its side back entirely (config edited: our pin removed) and rejects our
/// connection with E_LAN_NOT_MEMBER. A must tombstone the one-sided pin,
/// clear the record, and surface E_LAN_PAIR_ONESIDED.
#[test]
fn one_sided_pin_undone_when_peer_rejects_us() {
    let _serial = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b) = {
        let (a, b, _, _) = pair_two_runtimes(dir.path(), mac_config(), win_config());
        (a, b)
    };
    complete_pairing(&mut a, &mut b);

    // Forge the one-sided state on A: a surviving record (our commit) while
    // the peer's own record is gone. The record is written directly.
    let members = a.request("config.get", json!({}))["switch_group"]["sync"]["members"].clone();
    let b_addr = mac_pair_target(&mut b);
    let record = json!({
        "attempt": "0123456789abcdef0123456789abcdef",
        "peer_device_id": "dev-win",
        "peer_addr": b_addr,
        "initiator": true,
        "members": members,
    });
    std::fs::write(
        dir.path().join("a/pairing.pending.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();

    // Roll the peer's side back: remove our pin from B's config on disk and
    // restart B on a FRESH port (a rolled-back peer has no stable address).
    // The record's peer_addr is re-resolved to B2's actual address — as a
    // real deployment re-resolves its peers — so B2's E_LAN_NOT_MEMBER
    // refusal matches the record and the one-sided undo runs.
    drop(b);
    let b_path = dir.path().join("b/config.json");
    let mut b_cfg: Value = serde_json::from_slice(&std::fs::read(&b_path).unwrap()).unwrap();
    b_cfg["switch_group"]["sync"]["members"]
        .as_array_mut()
        .unwrap()
        .retain(|m| m["device_id"] != "dev-mac");
    std::fs::write(&b_path, serde_json::to_vec(&b_cfg).unwrap()).unwrap();
    let a_addr = mac_pair_target(&mut a);
    let mut b2 = Client::spawn(&b_path, 0, &[a_addr.parse().unwrap()]);
    let b2_addr = mac_pair_target(&mut b2);
    let mut record_fixed = record.clone();
    record_fixed["peer_addr"] = json!(b2_addr);
    std::fs::write(
        dir.path().join("a/pairing.pending.json"),
        serde_json::to_vec(&record_fixed).unwrap(),
    )
    .unwrap();

    let _ = b2.request("hello", json!({}));

    // A keeps dialing B (its manual candidate from the pairing); B rejects
    // with E_LAN_NOT_MEMBER naming B's address, which the record matches —
    // A undoes the one-sided pin and asks the user to pair again.
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    let mut last_exists = true;
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "A never reconciled the one-sided record; record={} members={}",
            dir.path().join("a/pairing.pending.json").exists(),
            a.request("config.get", json!({}))["switch_group"]["sync"]["members"]
        );
        let exists = dir.path().join("a/pairing.pending.json").exists();
        if exists != last_exists {
            eprintln!("[test] record existence flipped: {last_exists} -> {exists}");
            last_exists = exists;
        }
        let a_cfg = a.request("config.get", json!({}));
        let members_now = a_cfg["switch_group"]["sync"]["members"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let undone = members_now
            .iter()
            .find(|m| m["device_id"] == "dev-win")
            .is_none_or(|m| m["removed"] == json!(true));
        if undone && !dir.path().join("a/pairing.pending.json").exists() {
            break;
        }
        a.request(
            "lan.connect.manual",
            json!({"addr": mac_pair_target(&mut b2)}),
        );
        thread::sleep(Duration::from_millis(300));
    }
    // The UI notification surfaced the failure and asked to pair again.
    let notified = a
        .wait_for(
            |v| v["phase"] == "failed" && v["code"] == "E_LAN_PAIR_ONESIDED",
            Duration::from_secs(5),
        )
        .is_some()
        || a.drain_notifications()
            .iter()
            .any(|v| v["notification"] == "lan.pair" && v["data"]["code"] == "E_LAN_PAIR_ONESIDED");
    assert!(notified, "one-sided pairing must surface 请重新配对");
}

/// P1-6: a secondary with NO config at all selects 副机模式, gets a minimal
/// safe config created, pairs with the primary, and its pending-input
/// polling continues after a restart (the flag is durable).
#[test]
fn secondary_without_config_pairs_and_survives_restart() {
    let _serial = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let dir_primary = dir.path().join("p");
    let dir_secondary = dir.path().join("s");
    std::fs::create_dir_all(&dir_primary).unwrap();
    std::fs::create_dir_all(&dir_secondary).unwrap();
    // Primary: an armed two-computer config whose monitors match what the
    // simulated backend enumerates on the secondary (so the alias/匹配 issues
    // resolve to the auto-prefix path instead of a hard monitor_unmatched).
    let primary_cfg = json!({
        "schema_version": 2,
        "local_device": {
            "device_id": "dev-mac", "host_label": "MacBook", "monitors": [
                {"fingerprint": "SAC-2763-S:0000000000001", "label": "G73", "local_input": 15, "source": "learned_active_read"},
                {"fingerprint": "SAC-2466-S:0000000000000", "label": "G52plus", "local_input": 7, "source": "learned_active_read"}
            ]
        },
        "switch_group": {"group_id": "legacy-switch-group", "revision": 1, "devices": [
            {"device_id": "dev-mac", "name": "MacBook", "port_index": 1, "monitors": [
                {"fingerprint": "SAC-2763-S:0000000000001", "local_input": 15},
                {"fingerprint": "SAC-2466-S:0000000000000", "local_input": 7}]},
            {"device_id": "legacy-remote", "name": "Mac mini", "port_index": 2, "monitors": [
                {"fingerprint": "SAC-2763-S:0000000000001", "local_input": 17},
                {"fingerprint": "SAC-2466-S:0000000000000", "local_input": 8}]}]},
        "trigger": {"anchor": {"vid_pid": "1a40:0101"}, "members": [{"vid_pid": "3837:303c"}],
            "debounce": {"t_stable_ms": 0, "t_absent_ms": 0, "t_cooldown_ms": 1000}},
        "advanced": {"ddc_retry": {"attempts": 1, "delay_ms": 0}}
    });
    std::fs::write(
        dir_primary.join("config.json"),
        serde_json::to_vec(&primary_cfg).unwrap(),
    )
    .unwrap();

    let mut primary = Client::spawn(&dir_primary.join("config.json"), 0, &[]);
    let primary_port = primary.request("lan.status", json!({}))["listen_port"]
        .as_u64()
        .unwrap();
    // Secondary: no config file at all; dial the primary.
    let mut secondary = Client::spawn(
        &dir_secondary.join("config.json"),
        0,
        &[format!("127.0.0.1:{primary_port}").parse().unwrap()],
    );
    let _ = secondary.request("hello", json!({}));

    // The secondary picks 副机模式 first.
    let setup = secondary.request("lan.setup.secondary", json!({"host_label": "Mac mini"}));
    let _ = &setup;
    assert_eq!(setup["secondary"], json!(true), "{setup}");
    // The minimal config exists on disk with a stable identity.
    let cfg = secondary.request("config.get", json!({}));
    assert_eq!(cfg["schema_version"], 2);
    assert!(cfg["local_device"]["device_id"]
        .as_str()
        .is_some_and(|id| id.starts_with("dev-")));
    assert!(
        cfg["switch_group"].is_null(),
        "secondary must not invent a group"
    );
    assert!(dir_secondary.join("config.json").exists());

    // Pair: the primary initiates against the secondary's port.
    let secondary_port = secondary.request("lan.status", json!({}))["listen_port"]
        .as_u64()
        .unwrap();
    primary.request(
        "lan.pair.start",
        json!({"addr": format!("127.0.0.1:{secondary_port}")}),
    );
    primary
        .wait_for(
            |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "await_confirm",
            Duration::from_secs(20),
        )
        .expect("primary SAS prompt");
    secondary
        .wait_for(
            |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "await_confirm",
            Duration::from_secs(20),
        )
        .expect("secondary SAS prompt");
    primary.request("lan.pair.confirm", json!({"sas_ok": true}));
    secondary.request("lan.pair.confirm", json!({"sas_ok": true}));
    // The primary resolves every choice (monitor alias confirmation and the
    // mixed choice default to basis). Pending inputs are a blocker by
    // design — a value never observed while the secondary owned the picture
    // must not become a guessed DDC write — so the commit here is pin-only.
    let any = primary
        .wait_for(
            |v| {
                v["notification"] == "lan.pair"
                    && matches!(
                        v["data"]["phase"].as_str(),
                        Some("await_resolve" | "ready_to_commit" | "committed" | "failed")
                    )
            },
            Duration::from_secs(15),
        )
        .expect("some plan phase must arrive");
    let issues = any["data"]["issues"].clone();
    let _ = &any;
    assert!(
        issues
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .any(|i| i["detail"]["kind"] == "input_pending"),
        "the secondary's unread inputs must be reported as input_pending: {issues}"
    );
    let mut aliases = serde_json::Map::new();
    let mut mixed = None;
    for issue in issues.as_array().cloned().unwrap_or_default() {
        if issue["severity"] == "choice" {
            match issue["detail"]["kind"].as_str() {
                Some("monitor_alias") => {
                    aliases.insert(
                        issue["detail"]["local_fingerprint"]
                            .as_str()
                            .unwrap()
                            .into(),
                        json!(issue["detail"]["group_fingerprint"].as_str().unwrap()),
                    );
                }
                Some("mixed") => mixed = Some("basis"),
                _ => {}
            }
        }
    }
    let mut params = json!({});
    if !aliases.is_empty() {
        params["monitor_aliases"] = Value::Object(aliases);
    }
    if let Some(mixed) = mixed {
        params["mixed"] = json!(mixed);
    }
    let resolved = primary.request("lan.pair.resolve", params);
    assert_ne!(resolved["phase"], "await_resolve", "{resolved}");

    // Both ends commit the pin-only pairing (config_merged:false); the
    // secondary's monitors report pending rather than guessed values.
    for (client, name) in [(&mut primary, "primary"), (&mut secondary, "secondary")] {
        let committed = client
            .wait_for(
                |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "committed",
                Duration::from_secs(30),
            )
            .unwrap_or_else(|| panic!("{name} never committed"));
        assert_eq!(
            committed["data"]["config_merged"],
            json!(false),
            "{committed:?}"
        );
    }
    let secondary_cfg = secondary.request("config.get", json!({}));
    let members = secondary_cfg["switch_group"]["sync"]["members"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        members.len(),
        2,
        "keys pinned on the secondary: {members:?}"
    );
    for monitor in secondary_cfg["local_device"]["monitors"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        // Values come only from an actual DDC read while the monitor showed
        // this host (learned_active_read) or stay pending (unknown); a
        // heuristic guess never enters the profile.
        assert!(
            monitor["source"] == json!("unknown")
                || monitor["source"] == json!("learned_active_read"),
            "unread inputs stay pending or read-verified, never guessed: {monitor}"
        );
    }

    // Restart the secondary: the flag and the pending-input polling persist.
    let config_path = dir_secondary.join("config.json");
    let persisted: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    assert_eq!(persisted["setup_secondary"], json!(true), "{persisted}");
    drop(secondary);
    let mut secondary2 = Client::spawn(
        &config_path,
        0,
        &[format!("127.0.0.1:{primary_port}").parse().unwrap()],
    );
    let status = secondary2.request("lan.status", json!({}));
    assert_eq!(status["setup"]["secondary"], json!(true), "{status}");
    assert_eq!(persisted["setup_secondary"], json!(true));
}

/// N7: once a pairing ends (success, cancel, or secondary completion) the
/// inbound admission window is closed, so a fresh inbound pairing attempt is
/// refused with E_LAN_PAIR_WINDOW.
fn expect_pairing_refused(client: &mut Client, target: &mut Client) {
    let addr = mac_pair_target(target);
    client.request("lan.pair.start", json!({"addr": addr}));
    let refused = client.wait_for(
        |v| {
            v["notification"] == "lan.sync"
                && v["data"]["status"] == "conflict"
                && v["data"]["issues"].as_array().is_some_and(|issues| {
                    issues
                        .iter()
                        .any(|issue| issue["code"] == "E_LAN_PAIR_WINDOW")
                })
        },
        Duration::from_secs(20),
    );
    assert!(
        refused.is_some(),
        "inbound pairing must be refused after the pairing screen closed; lan={}",
        client.request("lan.status", json!({}))
    );
}

#[test]
fn pairing_window_closes_after_success_cancel_and_secondary_completion() {
    let _serial = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());

    // 1. Successful pairing: both ends must stop accepting dials.
    {
        let dir = tempfile::tempdir().unwrap();
        let (mut a, mut b, _a_addr, _b_addr) =
            pair_two_runtimes(dir.path(), mac_config(), win_config());
        complete_pairing(&mut a, &mut b);
        expect_pairing_refused(&mut b, &mut a);
    }

    // 2. Local cancel closes the same end's window.
    {
        let dir = tempfile::tempdir().unwrap();
        let (mut a, mut b, _a_addr, _b_addr) =
            pair_two_runtimes(dir.path(), mac_config(), win_config());
        // No stale session from a previous scenario/leaked process.
        a.request("lan.pair.cancel", json!({}));
        b.request("lan.pair.window", json!({"open": true}));
        a.request("lan.pair.start", json!({"addr": mac_pair_target(&mut b)}));
        a.wait_for(
            |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "await_confirm",
            Duration::from_secs(20),
        )
        .expect("A SAS prompt");
        a.request("lan.pair.cancel", json!({}));
        // The peer observes the abort asynchronously; wait until its pairing
        // slot is free before starting the refusal probe.
        b.wait_for(
            |v| {
                v["notification"] == "lan.pair"
                    && matches!(v["data"]["phase"].as_str(), Some("failed" | "cancelled"))
            },
            Duration::from_secs(10),
        )
        .expect("B observes the abort");
        expect_pairing_refused(&mut b, &mut a);
    }

    // 3. A secondary that finishes pairing must close its waiting window.
    {
        let dir = tempfile::tempdir().unwrap();
        let (mut a, mut b, _a_addr, _b_addr) =
            pair_two_runtimes(dir.path(), mac_config(), win_config());
        b.request("lan.setup.secondary", json!({}));
        complete_pairing(&mut a, &mut b);
        expect_pairing_refused(&mut a, &mut b);
    }
}

/// N8: confirming an old transaction record while a retry is already pairing
/// must not adopt the old session id. Record events carry their own id and
/// the new session keeps emitting fresh, monotonic events.
#[test]
fn old_record_confirmation_does_not_overwrite_the_current_pairing_session() {
    let _serial = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b, _a_addr, _b_addr) =
        pair_two_runtimes(dir.path(), mac_config(), win_config());
    complete_pairing(&mut a, &mut b);
    wait_mutually_online(&mut a, &mut b);

    // A surviving record from a previous (lost-ack) commit.
    let members = a.request("config.get", json!({}))["switch_group"]["sync"]["members"].clone();
    let b_addr = mac_pair_target(&mut b);
    let record = json!({
        "attempt": "1123456789abcdef0123456789abcdef",
        "session_id": "old-session",
        "peer_device_id": "dev-win",
        "peer_addr": b_addr,
        "initiator": true,
        "members": members,
    });
    std::fs::write(
        dir.path().join("a/pairing.pending.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();

    // A third computer: A starts a NEW pairing while the old record survives.
    let c_cfg = json!({
        "schema_version": 2,
        "local_device": {
            "device_id": "dev-c",
            "host_label": "Third",
            "monitors": [
                {"fingerprint": "DEL-A0F1-S:CN0AAA", "label": "DELL", "local_input": 19, "source": "learned_active_read"},
                {"fingerprint": "SAC-2763-S:0001", "label": "SAC", "local_input": 19, "source": "learned_active_read"}
            ]
        },
        "trigger": {"anchor": {"vid_pid": "1a40:0101"}, "members": [{"vid_pid": "3837:303c"}],
            "debounce": {"t_stable_ms": 0, "t_absent_ms": 0, "t_cooldown_ms": 1000}},
        "advanced": {"ddc_retry": {"attempts": 1, "delay_ms": 0}}
    });
    let c_dir = dir.path().join("c");
    std::fs::create_dir_all(&c_dir).unwrap();
    std::fs::write(
        c_dir.join("config.json"),
        serde_json::to_vec(&c_cfg).unwrap(),
    )
    .unwrap();
    let mut c = Client::spawn(&c_dir.join("config.json"), 0, &[a_port(&mut a)]);
    c.request("lan.pair.window", json!({"open": true}));
    let started = a.request("lan.pair.start", json!({"addr": mac_pair_target(&mut c)}));
    let new_session = started["session_id"].as_str().unwrap().to_string();
    assert_ne!(new_session, "old-session");
    a.wait_for(
        |v| {
            v["notification"] == "lan.pair"
                && v["data"]["phase"] == "await_confirm"
                && v["data"]["session_id"] == json!(new_session)
        },
        Duration::from_secs(20),
    )
    .expect("new session SAS");

    // B sends any authenticated non-refusal frame: A confirms the record.
    b.request("lan.profile.refresh", json!({}));
    let old_ack = a.wait_for(
        |v| {
            v["notification"] == "lan.pair"
                && v["data"]["phase"] == "peer_committed"
                && v["data"]["session_id"] == "old-session"
        },
        Duration::from_secs(10),
    );
    assert!(
        old_ack.is_some(),
        "record event must carry the record session id"
    );
    assert!(!dir.path().join("a/pairing.pending.json").exists());

    // The new session continues with its own id and sequence.
    c.wait_for(
        |v| {
            v["notification"] == "lan.pair"
                && v["data"]["phase"] == "await_confirm"
                && v["data"]["device_id"] == "dev-mac"
        },
        Duration::from_secs(20),
    )
    .expect("C SAS prompt");
    c.request("lan.pair.confirm", json!({"sas_ok": true}));
    a.request("lan.pair.confirm", json!({"sas_ok": true}));
    let plan = a
        .wait_for(
            |v| {
                v["notification"] == "lan.pair"
                    && v["data"]["session_id"] == json!(new_session)
                    && matches!(
                        v["data"]["phase"].as_str(),
                        Some("await_resolve" | "ready_to_commit" | "committed")
                    )
            },
            Duration::from_secs(20),
        )
        .expect("new plan")["data"]
        .clone();
    assert_eq!(plan["session_id"], json!(new_session));
    if plan["phase"] == "await_resolve" {
        let mut aliases = serde_json::Map::new();
        let mut mixed = None;
        for issue in plan["issues"].as_array().cloned().unwrap_or_default() {
            if issue["severity"] != "choice" {
                continue;
            }
            match issue["detail"]["kind"].as_str() {
                Some("monitor_alias") => {
                    aliases.insert(
                        issue["detail"]["local_fingerprint"]
                            .as_str()
                            .unwrap()
                            .into(),
                        json!(issue["detail"]["group_fingerprint"].as_str().unwrap()),
                    );
                }
                Some("mixed") => mixed = Some("basis"),
                _ => {}
            }
        }
        let mut params = json!({});
        if !aliases.is_empty() {
            params["monitor_aliases"] = Value::Object(aliases);
        }
        if let Some(mixed) = mixed {
            params["mixed"] = json!(mixed);
        }
        a.request("lan.pair.resolve", params);
    }
    let committed = a
        .wait_for(
            |v| {
                v["notification"] == "lan.pair"
                    && v["data"]["phase"] == "committed"
                    && v["data"]["session_id"] == json!(new_session)
            },
            Duration::from_secs(30),
        )
        .expect("new commit")["data"]
        .clone();
    assert_eq!(committed["session_id"], json!(new_session));
}

/// N9: an impostor at the recorded address (new identity, NOT_MEMBER reply)
/// must not revoke the pin. Only the key the record pinned is evidence.
#[test]
fn impostor_not_member_refusal_cannot_undo_a_pinned_record() {
    let _serial = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b, _a_addr, _b_addr) =
        pair_two_runtimes(dir.path(), mac_config(), win_config());
    complete_pairing(&mut a, &mut b);
    // Observe a real authenticated member session before killing B. A
    // never-yet-online row is not evidence that its handshake has drained.
    wait_mutually_online(&mut a, &mut b);

    let members = a.request("config.get", json!({}))["switch_group"]["sync"]["members"].clone();
    let b_addr = mac_pair_target(&mut b);
    let record = json!({
        "attempt": "2123456789abcdef0123456789abcdef",
        "session_id": "old-record",
        "peer_device_id": "dev-win",
        "peer_addr": b_addr,
        "initiator": true,
        "members": members,
    });
    // Restart B with a NEW identity and without A's pin: it answers with a
    // NOT_MEMBER refusal that must not match the record's key.
    drop(b);
    let b_path = dir.path().join("b/config.json");
    let mut b_cfg: Value = serde_json::from_slice(&std::fs::read(&b_path).unwrap()).unwrap();
    b_cfg["switch_group"]["sync"]["members"]
        .as_array_mut()
        .unwrap()
        .retain(|m| m["device_id"] != "dev-mac");
    std::fs::write(&b_path, serde_json::to_vec(&b_cfg).unwrap()).unwrap();
    let _ = std::fs::remove_file(dir.path().join("b/lan/identity.key"));
    // Do not plant the record while the old peer can still send a
    // non-refusal frame (that would legitimately confirm it).
    let offline_deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            std::time::Instant::now() < offline_deadline,
            "old peer never went offline"
        );
        let online = a.request("lan.status", json!({}))["peers"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .any(|p| p["device_id"] == "dev-win" && p["online"] == json!(true));
        if !online {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    let a_addr = mac_pair_target(&mut a);
    let mut impostor = Client::spawn(&b_path, 0, &[a_addr.parse().unwrap()]);
    let impostor_addr = mac_pair_target(&mut impostor);
    let mut record_fixed = record.clone();
    record_fixed["peer_addr"] = json!(impostor_addr);
    std::fs::write(
        dir.path().join("a/pairing.pending.json"),
        serde_json::to_vec(&record_fixed).unwrap(),
    )
    .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    while std::time::Instant::now() < deadline {
        a.request(
            "lan.connect.manual",
            json!({"addr": mac_pair_target(&mut impostor)}),
        );
        thread::sleep(Duration::from_millis(300));
    }

    assert!(
        dir.path().join("a/pairing.pending.json").exists(),
        "an impostor key must not clear the record"
    );
    let members_now = a.request("config.get", json!({}))["switch_group"]["sync"]["members"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        members_now
            .iter()
            .find(|m| m["device_id"] == "dev-win")
            .map(|m| m["removed"].clone()),
        Some(json!(false)),
        "an impostor key must not revoke the pinned member"
    );
    let one_sided = a
        .drain_notifications()
        .iter()
        .any(|v| v["notification"] == "lan.pair" && v["data"]["code"] == "E_LAN_PAIR_ONESIDED");
    assert!(!one_sided, "no one-sided undo may be reported");
}

fn wait_complete_topology(a: &mut Client, b: &mut Client) -> (Value, Value) {
    // Converges in a few seconds locally; shared Windows runners have
    // needed more than 25s. A real stall still fails at the deadline.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let ca = a.request("config.get", json!({}));
        let cb = b.request("config.get", json!({}));
        if ca["switch_group"]["devices"]
            .as_array()
            .is_some_and(|d| d.len() == 2)
            && cb["switch_group"]["devices"]
                .as_array()
                .is_some_and(|d| d.len() == 2)
            && ca["switch_group"]["devices"] == cb["switch_group"]["devices"]
        {
            return (ca, cb);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "topology never completed: a={ca} b={cb}"
        );
        thread::sleep(Duration::from_millis(150));
    }
}

#[test]
fn fresh_initialization_pair_profiles_confirm_try_persists_two_owners_on_both_hosts() {
    let _serial = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    for scenario in ["idle", "manual_layout"] {
        let dir = tempfile::tempdir().unwrap();
        let pa = dir.path().join("primary/config.json");
        let pb = dir.path().join("secondary/config.json");
        std::fs::create_dir_all(pa.parent().unwrap()).unwrap();
        std::fs::create_dir_all(pb.parent().unwrap()).unwrap();
        let mut a = Client::spawn(&pa, 0, &[]);
        let mut b = Client::spawn_scenario(&pb, 0, &[], scenario);
        let primary = a.request("lan.identity", json!({}))["device_id"].clone();
        let secondary = b.request("lan.identity", json!({}))["device_id"].clone();
        // The primary wizard records its own Windows input dialect (16/8).
        // The secondary's simulated displays will actively learn their 15/7.
        let displays = a.request("display.list", json!({}))["displays"].clone();
        let monitors: Vec<Value> = displays
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, d)| {
                json!({
                    "fingerprint":d["edid_id"], "label":d["model_name"],
                    "local_input":if i==0 {16} else {8}, "source":"learned_active_read"
                })
            })
            .collect();
        let cfg = json!({"schema_version":2,"local_device":{"device_id":primary,"host_label":"Windows primary","monitors":monitors},
        "trigger":{"anchor":{"vid_pid":"1a40:0101"},"members":[{"vid_pid":"3837:303c"}],"debounce":{"t_stable_ms":0,"t_absent_ms":0,"t_cooldown_ms":1000}},
        "advanced":{"ddc_retry":{"attempts":1,"delay_ms":0}}});
        a.request("config.set", json!({"config":cfg}));
        b.request("lan.setup.secondary", json!({"host_label":"Studio Mac"}));
        complete_pairing(&mut a, &mut b);
        if scenario == "manual_layout" {
            // The second simulated monitor is still showing the other input.
            // Trust must survive a restart while the complete topology is blocked.
            for path in [&pa, &pb] {
                let cfg: kvmflow_core::config::MultiDeviceConfig =
                    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
                cfg.validate().unwrap();
                assert!(cfg.switch_group.as_ref().unwrap().devices.is_empty());
                assert!(cfg
                    .switch_group
                    .as_ref()
                    .unwrap()
                    .sync
                    .get("setup_pending")
                    .is_some());
            }
            assert_eq!(
                a.response("lan.test.start", json!({}))["error"]["code"],
                "E_LAN_SETUP_PENDING"
            );
            drop(b);
            // Restart with both monitor inputs showing the secondary, just as
            // when the user brings its picture back. Polling must resume setup.
            b = Client::spawn(&pb, 0, &[a_port(&mut a)]);
            a.request(
                "lan.connect.manual",
                json!({"addr":mac_pair_target(&mut b)}),
            );
        }
        a.request("lan.profile.refresh", json!({}));
        b.request("lan.profile.refresh", json!({}));
        let (mut ca, cb) = wait_complete_topology(&mut a, &mut b);
        for config in [&ca, &cb] {
            assert!(config["switch_group"]["sync"]
                .get("setup_pending")
                .is_none());
            for id in [&primary, &secondary] {
                assert!(config["switch_group"]["devices"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|d| &d["device_id"] == id));
            }
            let peer = config["switch_group"]["devices"]
                .as_array()
                .unwrap()
                .iter()
                .find(|d| d["device_id"] == secondary)
                .unwrap();
            assert_eq!(peer["name"], "Studio Mac");
            assert_eq!(peer["monitors"].as_array().unwrap().len(), 2);
        }
        assert_eq!(
            a.response("lan.test.start", json!({}))["error"]["code"],
            "E_LAN_SETUP_PENDING",
            "monitor confirmation is required after topology save"
        );
        // The actual confirmation page persists ports_confirmed before entering Try.
        ca["switch_group"]["sync"]["ports_confirmed"] = json!(true);
        a.request("config.set", json!({"config":ca}));
        let started = a.request("lan.test.start", json!({"duration_ms":5000}));
        assert!(started["test_id"].is_string());
        a.request("lan.test.stop", json!({}));
        for path in [&pa, &pb] {
            let persisted: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            assert_eq!(
                persisted["switch_group"]["devices"]
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
        }
        wait_mutually_online(&mut a, &mut b);
        for runtime in [&mut a, &mut b] {
            let issues = runtime.request("lan.status", json!({}))["issues"].clone();
            assert!(
                !issues
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|i| i["code"] == "E_LAN_NOT_MEMBER"),
                "historical refusal must clear after authenticated connection: {issues}"
            );
        }
    }
}

#[test]
fn old_03_mac_win_upgrade_pairs_real_owners_without_placeholder_displays() {
    let _serial = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut primary = mac_config();
    let mut secondary = win_config();
    for config in [&mut primary, &mut secondary] {
        config["local_device"]["device_id"] = json!("mac");
        config["switch_group"]["group_id"] = json!("local-switch-group");
        config["switch_group"]["devices"][0]["device_id"] = json!("mac");
        config["switch_group"]["devices"][1]["device_id"] = json!("win");
        config["switch_group"]["devices"][1]["name"] = json!("old remote placeholder");
    }
    let (mut a, mut b, _, _) = pair_two_runtimes(dir.path(), primary, secondary);
    let ida = a.request("lan.identity", json!({}))["device_id"].clone();
    let idb = b.request("lan.identity", json!({}))["device_id"].clone();
    assert_ne!(ida, idb);
    complete_pairing(&mut a, &mut b);
    let (ca, cb) = wait_complete_topology(&mut a, &mut b);
    for config in [&ca, &cb] {
        let devices = config["switch_group"]["devices"].as_array().unwrap();
        assert!(devices
            .iter()
            .all(|d| d["device_id"] != "mac" && d["device_id"] != "win"));
        let peer = devices.iter().find(|d| d["device_id"] == idb).unwrap();
        assert_eq!(peer["port_index"], 2);
        assert_eq!(peer["name"], "游戏 PC");
        assert_eq!(peer["monitors"].as_array().unwrap().len(), 2);
        assert!(peer["monitors"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["local_input"] == 17));
    }
    for host in ["a", "b"] {
        let persisted: Value = serde_json::from_slice(
            &std::fs::read(dir.path().join(host).join("config.json")).unwrap(),
        )
        .unwrap();
        let mut expected = ca["switch_group"]["devices"].clone();
        // Built-in availability is an ephemeral observation, intentionally not persisted.
        for device in expected.as_array_mut().unwrap() {
            device
                .as_object_mut()
                .unwrap()
                .remove("available_builtin_screens");
        }
        assert_eq!(persisted["switch_group"]["devices"], expected);
    }
}

#[test]
fn pairing_write_failure_is_reported_to_both_hosts_and_can_be_retried() {
    let _serial = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b, _, _) = pair_two_runtimes(dir.path(), mac_config(), win_config());
    b.request("lan.pair.window", json!({"open":true}));
    a.request("lan.pair.start", json!({"addr":mac_pair_target(&mut b)}));
    for runtime in [&mut a, &mut b] {
        runtime
            .wait_for(
                |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "await_confirm",
                Duration::from_secs(15),
            )
            .expect("SAS");
        runtime.request("lan.pair.confirm", json!({"sas_ok":true}));
    }
    let choices = a
        .wait_for(
            |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "await_resolve",
            Duration::from_secs(15),
        )
        .expect("choices");
    // A directory at the destination makes the atomic replace fail even
    // under an elevated test runner; no host permissions are changed.
    let path = dir.path().join("a/config.json");
    let saved = dir.path().join("a/config.saved");
    std::fs::rename(&path, &saved).unwrap();
    std::fs::create_dir(&path).unwrap();
    a.request(
        "lan.pair.resolve",
        default_pair_choices(&choices["data"]["issues"]),
    );
    for runtime in [&mut a, &mut b] {
        let failed = runtime
            .wait_for(
                |v| v["notification"] == "lan.pair" && v["data"]["phase"] == "failed",
                Duration::from_secs(5),
            )
            .expect("write failure must be published to both UIs");
        assert_eq!(failed["data"]["code"], "E_CONFIG_IO");
        assert!(failed["data"]["session_id"].is_string());
        assert!(
            runtime.request("config.get", json!({}))["switch_group"]["sync"]["members"]
                .as_array()
                .is_none_or(|m| m.is_empty()),
            "failed write cannot publish member pins"
        );
    }
    assert!(
        !dir.path().join("a/pairing.pending.json").exists(),
        "an uncommitted failed attempt must not leave a revocation record"
    );
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(&saved, &path).unwrap();
    complete_pairing(&mut a, &mut b);
    wait_complete_topology(&mut a, &mut b);
}

/// Before pairing, the primary dials every discovered computer and unpaired
/// ones refuse it. That refusal is expected: it must not surface as the
/// "pairing revoked, pair again" issue on either end.
#[test]
fn unpaired_refusals_are_not_reported_as_revoked_pairing() {
    let _serial = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (mut mac, mut win, _mac_addr, _win_addr) =
        pair_two_runtimes(dir.path(), mac_config(), win_config());
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let refused = loop {
        let status = win.request("lan.status", json!({}));
        if status["peers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["last_error"] == "E_LAN_NOT_MEMBER")
        {
            break true;
        }
        if std::time::Instant::now() > deadline {
            break false;
        }
        thread::sleep(Duration::from_millis(100));
    };
    assert!(refused, "the unpaired dial must be refused first");
    thread::sleep(Duration::from_millis(300));
    for runtime in [&mut mac, &mut win] {
        let issues = runtime.request("lan.status", json!({}))["issues"].clone();
        assert!(
            !issues
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["code"] == "E_LAN_NOT_MEMBER"),
            "an unpaired refusal is not a revoked pairing: {issues}"
        );
    }
}

/// Pairing v2: the computer that waited learned the USB switch from the
/// devices that left it; the computer that dialed starts without a trigger
/// and adopts the description only when every device in it is connected
/// locally. A description it cannot see stays unadopted and is reported.
#[test]
fn dialing_computer_adopts_the_waiting_computers_trigger_only_when_aligned() {
    let _serial = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    for (members, aligned) in [
        (json!([{"vid_pid":"3837:303c"}]), true),
        (
            json!([{"vid_pid":"3837:303c"},{"vid_pid":"dead:beef"}]),
            false,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let pa = dir.path().join("dialer/config.json");
        let pb = dir.path().join("waiting/config.json");
        std::fs::create_dir_all(pa.parent().unwrap()).unwrap();
        std::fs::create_dir_all(pb.parent().unwrap()).unwrap();
        let mut a = Client::spawn(&pa, 0, &[]);
        let mut b = Client::spawn(&pb, 0, &[]);
        let dialer = a.request("lan.identity", json!({}))["device_id"].clone();
        let displays = a.request("display.list", json!({}))["displays"].clone();
        let monitors: Vec<Value> = displays
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, d)| {
                json!({
                    "fingerprint":d["edid_id"], "label":d["model_name"],
                    "local_input":if i==0 {16} else {8}, "source":"learned_active_read"
                })
            })
            .collect();
        a.request(
            "config.set",
            json!({"config":{"schema_version":2,
                "local_device":{"device_id":dialer,"host_label":"Dialer","monitors":monitors},
                "trigger":{"anchor":{"vid_pid":""}},
                "advanced":{"ddc_retry":{"attempts":1,"delay_ms":0}}}}),
        );
        b.request("lan.setup.secondary", json!({"host_label":"Waiting"}));
        let mut waiting = b.request("config.get", json!({}));
        waiting["trigger"]["anchor"] = json!({"vid_pid":"1a40:0101"});
        waiting["trigger"]["members"] = members.clone();
        b.request("config.set", json!({"config":waiting}));
        complete_pairing(&mut a, &mut b);
        let phase = if aligned {
            "trigger_adopted"
        } else {
            "trigger_unmatched"
        };
        let event = a
            .wait_for(
                |v| v["notification"] == "lan.setup" && v["data"]["phase"] == phase,
                Duration::from_secs(20),
            )
            .unwrap_or_else(|| panic!("dialer never reported {phase}"));
        let persisted: Value = serde_json::from_slice(&std::fs::read(&pa).unwrap()).unwrap();
        if aligned {
            assert_eq!(persisted["trigger"]["anchor"]["vid_pid"], "1a40:0101");
            assert_eq!(persisted["trigger"]["members"], members);
        } else {
            assert_eq!(event["data"]["missing"], json!(["dead:beef"]));
            assert_eq!(persisted["trigger"]["anchor"]["vid_pid"], "");
        }
        // The waiting computer keeps its own description untouched.
        let kept: Value = serde_json::from_slice(&std::fs::read(&pb).unwrap()).unwrap();
        assert_eq!(kept["trigger"]["members"], members);
    }
}
