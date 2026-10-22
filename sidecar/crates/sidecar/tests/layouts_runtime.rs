//! The former manual-layout API is replaced by mixed mode. Exercise the
//! public worker boundary, real persistence, simulated DDC and fake topology.
use kvmflow_core::mixed::TopologyDisplay;
use kvmflow_sidecar::{
    topology::{DisplayTopology, FakeDisplayTopology},
    RuntimeHandle, RuntimeOptions,
};
use serde_json::{json, Value};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};
const MAIN: &str = "SAC-2763-S:0000000000001";
const SECOND: &str = "SAC-2466-S:0000000000000";
fn config() -> Value {
    json!({"schema_version":2,"local_device":{"device_id":"mac","monitors":[{"fingerprint":MAIN,"local_input":15},{"fingerprint":SECOND,"local_input":7}]},
        "switch_group":{"group_id":"desk","revision":1,"mixed":{"enabled":false,"revision":4,"assignment":[{"fingerprint":MAIN,"device_id":"mac"},{"fingerprint":SECOND,"device_id":"windows"}]},
        "devices":[{"device_id":"mac","name":"Mac","port_index":1,"monitors":[{"fingerprint":MAIN,"local_input":15},{"fingerprint":SECOND,"local_input":7}]},{"device_id":"windows","name":"PC","port_index":2,"monitors":[{"fingerprint":MAIN,"local_input":16},{"fingerprint":SECOND,"local_input":8}]}]},
        "trigger":{"anchor":{"vid_pid":"1a40:0101"},"members":[{"vid_pid":"3837:303c"}],"debounce":{"t_stable_ms":0,"t_absent_ms":0,"t_cooldown_ms":1000}},"advanced":{"ddc_retry":{"attempts":2,"delay_ms":0}}})
}

#[test]
fn lan_alias_translates_group_routes_to_local_backend_ddc_identity() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    let mut cfg = config();
    for device in cfg["switch_group"]["devices"].as_array_mut().unwrap() {
        device["monitors"][0]["fingerprint"] = json!("DEL-group-identity");
    }
    cfg["switch_group"]["devices"][0]["monitors"][0]["local_fingerprint"] = json!(MAIN);
    cfg["switch_group"]["mixed"]["assignment"][0]["fingerprint"] = json!("DEL-group-identity");
    let (rt, rx) = start(
        dir.path(),
        "fast_clean_roundtrip",
        &cfg,
        Box::new(topo.clone()),
    );
    let report = wait(&rx, "switch.report");
    assert_eq!(report["commanded"], true, "{report}");
    assert!(report["per_monitor"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["edid_id"] == "DEL-group-identity"
            && m["requested"] == 16
            && m["commanded"] == true));
    assert!(topo
        .operations
        .lock()
        .unwrap()
        .iter()
        .any(|op| op.contains(MAIN) && op.starts_with("ddc_push")));
    rt.shutdown_and_join().unwrap();
}
fn expected_pending_id(mixed: &Value, fingerprint: &str, target: &str) -> String {
    kvmflow_core::mixed::pending_remote_id_with_meta(
        "desk",
        mixed["revision"].as_u64().unwrap(),
        mixed["section_meta"]["updated_at_ms"].as_u64().unwrap(),
        mixed["section_meta"]["updated_by"].as_str().unwrap(),
        fingerprint,
        target,
    )
}
fn topology() -> FakeDisplayTopology {
    FakeDisplayTopology::new(
        [MAIN, SECOND]
            .iter()
            .map(|fp| TopologyDisplay {
                fingerprint: (*fp).into(),
                builtin: false,
                connected: true,
            })
            .collect(),
    )
}
fn start(
    dir: &std::path::Path,
    scenario: &str,
    cfg: &Value,
    topology: Box<dyn DisplayTopology>,
) -> (RuntimeHandle, Receiver<(String, Value)>) {
    std::fs::write(dir.join("config.json"), serde_json::to_vec(cfg).unwrap()).unwrap();
    let mut options = RuntimeOptions::new(dir.join("config.json"), dir.join("logs"));
    options.backend = "simulated".into();
    options.scenario = scenario.into();
    options.topology = Some(topology);
    let (tx, rx) = mpsc::channel();
    let rt = RuntimeHandle::start(options, move |kind, data| {
        let _ = tx.send((kind.into(), data));
    })
    .unwrap();
    rt.request("hello", json!({})).unwrap();
    (rt, rx)
}
fn wait(rx: &Receiver<(String, Value)>, kind: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let (k, d) = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        if k == kind {
            return d;
        }
    }
}
#[test]
fn scenario_2_assignment_only_saves_and_roundtrips_without_ddc() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    let (rt, _) = start(dir.path(), "idle", &config(), Box::new(topo.clone()));
    let assignment = json!([{"fingerprint":MAIN,"device_id":"windows"},{"fingerprint":SECOND,"device_id":"mac"}]);
    let r = rt
        .request("mixed.assign", json!({"assignment":assignment}))
        .unwrap();
    assert_eq!(r["mixed"]["revision"], 5);
    assert_eq!(r["mixed"]["assignment"], assignment);
    assert_eq!(
        rt.request("display.readInput", json!({"edid_id":MAIN}))
            .unwrap()["value"],
        15
    );
    assert!(topo.operations.lock().unwrap().is_empty());
    let saved = rt.request("config.get", json!({})).unwrap();
    rt.shutdown_and_join().unwrap();
    let (rt, _) = start(dir.path(), "idle", &saved, Box::new(topology()));
    let state = rt.request("state.get", json!({})).unwrap();
    assert_eq!(state["mixed"]["assignment"], assignment);
    assert_eq!(state["mixed"]["revision"], 5);
}
#[test]
fn scenario_3_enable_persists_pushes_detaches_and_shutdown_restores() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    let (rt, rx) = start(dir.path(), "idle", &config(), Box::new(topo.clone()));
    rt.request("app.setEnabled", json!({"enabled":false}))
        .unwrap();
    let r = rt.request("mixed.set", json!({"enabled":true})).unwrap();
    let mixed = &r["mixed"];
    assert_eq!(mixed["enabled"], true);
    assert_eq!(mixed["revision"], 5);
    assert_eq!(mixed["detached"], json!([SECOND]));
    assert_eq!(mixed["execution"][0]["step"]["step"], "ddc_push");
    assert_eq!(mixed["execution"][1]["step"]["step"], "detach");
    assert!(mixed["destinations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["fingerprint"] == SECOND)
        .unwrap()["current_device_id"]
        .is_null()); // command accepted is not picture proof
    assert_eq!(
        *topo.operations.lock().unwrap(),
        vec![format!("ddc_push:{SECOND}:8"), format!("detach:{SECOND}")]
    );
    assert_eq!(
        rt.request("state.get", json!({})).unwrap()["enabled"],
        false
    );
    let saved: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("config.json")).unwrap()).unwrap();
    assert_eq!(saved["switch_group"]["mixed"]["enabled"], true);
    wait(&rx, "mixed.changed");
    rt.shutdown_and_join().unwrap();
    assert_eq!(
        topo.operations.lock().unwrap().last().unwrap(),
        "restore_all"
    );
    assert!(topo.displays.lock().unwrap().iter().all(|d| d.connected));
}
#[test]
fn scenario_6_disable_restores_waits_and_queues_remote_owner_with_revision() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    let mut cfg = config();
    cfg["switch_group"]["mixed"]["enabled"] = json!(true);
    let (rt, rx) = start(dir.path(), "manual_layout", &cfg, Box::new(topo.clone()));
    // Startup aligns enabled state even when not using a setter.
    assert_eq!(
        rt.request("state.get", json!({})).unwrap()["mixed"]["detached"],
        json!([SECOND])
    );
    // Initial GroupStable provides H without treating startup as a switch.
    let deadline = Instant::now() + Duration::from_secs(3);
    while rt.request("state.get", json!({})).unwrap()["mixed"]["keyboard_holder_device_id"]
        .is_null()
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(25));
    }
    let r = rt.request("mixed.set", json!({"enabled":false})).unwrap();
    assert_eq!(r["mixed"]["detached"], json!([]));
    assert_eq!(r["mixed"]["execution"][0]["step"]["step"], "attach");
    assert_eq!(
        r["mixed"]["execution"][1]["step"]["step"],
        "wait_enumeration"
    );
    assert_eq!(
        r["mixed"]["pending_remote"],
        json!([{"id":expected_pending_id(&r["mixed"],SECOND,"mac"),"device_id":null,"fingerprint":SECOND,"target_device_id":"mac"}])
    );
    let n = loop {
        let n = wait(&rx, "mixed.pending_remote");
        if n["revision"] == 5 {
            break n;
        }
    };
    assert_eq!(n["revision"], 5);
    assert_eq!(n["enabled"], false);
}
#[test]
fn scenario_7_unknown_owner_remains_pending_and_noop_is_honest() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, rx) = start(
        dir.path(),
        "manual_layout",
        &config(),
        Box::new(kvmflow_sidecar::topology::NoopDisplayTopology),
    );
    let assignment = json!([{"fingerprint":MAIN,"device_id":"windows"},{"fingerprint":SECOND,"device_id":"mac"}]);
    rt.request("mixed.assign", json!({"assignment":assignment}))
        .unwrap();
    let r = rt.request("mixed.set", json!({"enabled":true})).unwrap();
    assert_eq!(r["mixed"]["detached"], json!([]));
    assert!(r["mixed"]["topology_error"].is_string());
    assert_eq!(r["mixed"]["pending_remote"][0]["device_id"], Value::Null);
    assert_eq!(r["mixed"]["pending_remote"][0]["target_device_id"], "mac");
    assert_eq!(wait(&rx, "mixed.pending_remote")["enabled"], true);
}
#[test]
fn invalid_requests_never_change_revision_or_saved_file_and_manual_api_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, _) = start(dir.path(), "idle", &config(), Box::new(topology()));
    let before = std::fs::read(dir.path().join("config.json")).unwrap();
    for (method, params) in [
        ("mixed.set", json!({})),
        ("mixed.set", json!({"enabled":"yes"})),
        (
            "mixed.assign",
            json!({"assignment":[{"fingerprint":MAIN,"device_id":"mac"},{"fingerprint":MAIN,"device_id":"windows"}]}),
        ),
    ] {
        assert_eq!(
            rt.request(method, params).unwrap_err().code,
            "E_CONFIG_INVALID"
        );
        assert_eq!(
            std::fs::read(dir.path().join("config.json")).unwrap(),
            before
        );
    }
    assert_eq!(
        rt.request("apply_layout", json!({"layout_id":"builtin:mac"}))
            .unwrap_err()
            .code,
        "E_UNKNOWN_METHOD"
    );
    assert!(
        !rt.request("protocol.describe", json!({})).unwrap()["requests"]
            .as_array()
            .unwrap()
            .contains(&json!("apply_layout"))
    );
    rt.request("mixed.assign",json!({"assignment":[{"fingerprint":MAIN,"device_id":"mac"},{"fingerprint":SECOND,"device_id":"mac"}]})).unwrap();
    let before = std::fs::read(dir.path().join("config.json")).unwrap();
    assert_eq!(
        rt.request("mixed.set", json!({"enabled":true}))
            .unwrap_err()
            .code,
        "E_CONFIG_INVALID"
    );
    assert_eq!(
        std::fs::read(dir.path().join("config.json")).unwrap(),
        before
    );
}
#[test]
fn ddc_failure_prevents_detach() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    let (rt, _) = start(
        dir.path(),
        "idle_ddc_fail",
        &config(),
        Box::new(topo.clone()),
    );
    let r = rt.request("mixed.set", json!({"enabled":true})).unwrap();
    assert_eq!(r["mixed"]["detached"], json!([]));
    assert_eq!(r["mixed"]["execution"][0]["ok"], false);
    assert_eq!(
        r["mixed"]["execution"][1]["error"]["code"],
        "E_MIXED_PREREQUISITE"
    );
    assert!(topo.operations.lock().unwrap().is_empty());
}
#[test]
fn legacy_follow_migrates_disabled_to_assignment_and_archives_layouts() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config();
    cfg["switch_group"].as_object_mut().unwrap().remove("mixed");
    cfg["switch_group"]["devices"][1]["monitors"][1]["follow"] = json!(false);
    let (rt, _) = start(dir.path(), "idle", &cfg, Box::new(topology()));
    let migrated = rt.request("config.get", json!({})).unwrap();
    assert_eq!(migrated["switch_group"]["mixed"]["enabled"], false);
    assert_eq!(
        migrated["switch_group"]["mixed"]["assignment"],
        json!([{"fingerprint":MAIN,"device_id":"windows"},{"fingerprint":SECOND,"device_id":"mac"}])
    );
    assert_eq!(
        migrated["switch_group"]["layouts"][0]["source"]["kind"],
        "migrated_follow"
    );
    rt.request("config.set", json!({"config":migrated}))
        .unwrap();
    let saved = rt.request("config.get", json!({})).unwrap();
    rt.shutdown_and_join().unwrap();
    let (rt, _) = start(dir.path(), "idle", &saved, Box::new(topology()));
    assert_eq!(rt.request("config.get", json!({})).unwrap(), saved);
}
#[test]
fn e3_e9_runtime_hotplug_keeps_last_screen_and_e8_new_monitor_warns() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    let (rt, rx) = start(dir.path(), "idle", &config(), Box::new(topo.clone()));
    rt.request("mixed.set", json!({"enabled":true})).unwrap();
    // E7 unplug the owned display; E9 the remaining screen cannot be detached.
    {
        let mut displays = topo.displays.lock().unwrap();
        displays.retain(|d| d.fingerprint == SECOND);
        assert!(!displays[0].connected);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let state = rt.request("state.get", json!({})).unwrap();
        if state["mixed"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["code"] == "last_local_screen")
        {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(topo.displays.lock().unwrap()[0].connected);
    topo.displays.lock().unwrap().push(TopologyDisplay {
        fingerprint: "new-screen".into(),
        builtin: false,
        connected: true,
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (k, d) = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        if k == "state"
            && d["mixed"]["validation"]["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["code"] == "unassigned_monitor")
        {
            break;
        }
    }
    let state = rt.request("state.get", json!({})).unwrap();
    assert_eq!(state["mixed"]["assignment"].as_array().unwrap().len(), 2);
}

struct FailingRestore(FakeDisplayTopology);
impl DisplayTopology for FailingRestore {
    fn list(&mut self) -> Result<Vec<TopologyDisplay>, kvmflow_core::errors::CoreError> {
        self.0.list()
    }
    fn detach(&mut self, fp: &str) -> Result<(), kvmflow_core::errors::CoreError> {
        self.0.detach(fp)
    }
    fn attach(&mut self, _: &str) -> Result<(), kvmflow_core::errors::CoreError> {
        Err(kvmflow_core::errors::CoreError::new(
            "E_BACKEND",
            "test_attach_failed",
        ))
    }
    fn wait_enumeration(&mut self) -> Result<(), kvmflow_core::errors::CoreError> {
        Err(kvmflow_core::errors::CoreError::new(
            "E_BACKEND",
            "test_wait_failed",
        ))
    }
    fn restore_all(&mut self) -> Result<(), kvmflow_core::errors::CoreError> {
        self.0.restore_all()
    }
}
#[test]
fn failed_restore_or_enumeration_prevents_later_push_and_detach() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    topo.displays.lock().unwrap()[0].connected = false;
    let mut cfg = config();
    cfg["switch_group"]["mixed"]["enabled"] = json!(true);
    let (rt, _) = start(
        dir.path(),
        "idle",
        &cfg,
        Box::new(FailingRestore(topo.clone())),
    );
    let s = rt.request("state.get", json!({})).unwrap();
    let execution = s["mixed"]["execution"].as_array().unwrap();
    assert_eq!(execution[0]["error"]["code"], "E_BACKEND");
    assert!(execution
        .iter()
        .filter(|e| e["step"]["step"] == "ddc_push" || e["step"]["step"] == "detach")
        .all(|e| e["ok"] == false));
    assert_eq!(
        rt.request("display.readInput", json!({"edid_id":SECOND}))
            .unwrap()["value"],
        7
    );
    assert!(topo.operations.lock().unwrap().is_empty());
}
#[test]
fn e2_builtin_allows_one_external_and_e3_lid_close_restores_last_screen() {
    let dir = tempfile::tempdir().unwrap();
    let topo = FakeDisplayTopology::new(vec![
        TopologyDisplay {
            fingerprint: SECOND.into(),
            builtin: false,
            connected: true,
        },
        TopologyDisplay {
            fingerprint: "builtin".into(),
            builtin: true,
            connected: true,
        },
    ]);
    let (rt, rx) = start(dir.path(), "idle", &config(), Box::new(topo.clone()));
    assert_eq!(
        rt.request(
            "mixed.assign",
            json!({"assignment":[{"fingerprint":"builtin","device_id":"mac"}]})
        )
        .unwrap_err()
        .code,
        "E_CONFIG_INVALID"
    );
    let r = rt.request("mixed.set", json!({"enabled":true})).unwrap();
    assert_eq!(r["mixed"]["validation"]["ok"], true);
    assert_eq!(r["mixed"]["detached"], json!([SECOND]));
    topo.displays.lock().unwrap().retain(|d| !d.builtin);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let s = rt.request("state.get", json!({})).unwrap();
        if s["mixed"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["code"] == "last_local_screen")
        {
            assert_eq!(s["mixed"]["detached"], json!([]));
            assert_eq!(s["mixed"]["validation"]["ok"], false);
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        rx.try_iter()
            .filter(|(kind, data)| kind == "mixed.warning"
                && data["code"] == "last_local_screen"
                && data["fingerprint"] == SECOND)
            .count(),
        1
    );
}

#[test]
fn persistence_failure_never_executes_or_changes_mode_revision() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    let (rt, _) = start(dir.path(), "idle", &config(), Box::new(topo.clone()));
    // Create a directory at the atomic temp-file path to force a real IO failure.
    std::fs::create_dir(dir.path().join("config.json.tmp")).unwrap();
    let r = rt
        .request("mixed.set", json!({"enabled":true}))
        .unwrap_err();
    assert_eq!(r.code, "E_CONFIG_IO");
    let s = rt.request("state.get", json!({})).unwrap();
    assert_eq!(s["mixed"]["enabled"], false);
    assert_eq!(s["mixed"]["revision"], 4);
    assert!(topo.operations.lock().unwrap().is_empty());
}

#[test]
fn review_config_set_cannot_replace_or_reset_live_mixed_section() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    let (rt, _) = start(dir.path(), "idle", &config(), Box::new(topo));
    let stale = rt.request("config.get", json!({})).unwrap();
    rt.request("mixed.set", json!({"enabled":true})).unwrap();
    let expected = rt.request("config.get", json!({})).unwrap()["switch_group"]["mixed"].clone();
    for omit in [true, false] {
        let mut edit = stale.clone();
        edit["switch_group"]["devices"][0]["name"] = json!("Renamed");
        if omit {
            edit["switch_group"]
                .as_object_mut()
                .unwrap()
                .remove("mixed");
        }
        rt.request("config.set", json!({"config":edit})).unwrap();
        let cfg = rt.request("config.get", json!({})).unwrap();
        assert_eq!(cfg["switch_group"]["devices"][0]["name"], "Renamed");
        assert_eq!(
            cfg["switch_group"]["mixed"], expected,
            "config.set must preserve current mixed regardless of omitted or stale fields"
        );
    }
}
#[test]
fn review_builtin_availability_is_not_persisted_as_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, _) = start(dir.path(), "idle", &config(), Box::new(topology()));
    let cfg = rt.request("config.get", json!({})).unwrap();
    assert_eq!(
        cfg["switch_group"]["devices"][0]["available_builtin_screens"],
        0
    );
    rt.request("config.set", json!({"config":cfg})).unwrap();
    let disk: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("config.json")).unwrap()).unwrap();
    assert!(disk["switch_group"]["devices"]
        .as_array()
        .unwrap()
        .iter()
        .all(|d| d.get("available_builtin_screens").is_none()));
}

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
#[derive(Clone)]
struct FaultTopology {
    inner: FakeDisplayTopology,
    attach_failures: Arc<AtomicUsize>,
    wait_failures: Arc<AtomicUsize>,
    list_failed: Arc<AtomicBool>,
}
impl FaultTopology {
    fn new(inner: FakeDisplayTopology) -> Self {
        Self {
            inner,
            attach_failures: Arc::new(AtomicUsize::new(0)),
            wait_failures: Arc::new(AtomicUsize::new(0)),
            list_failed: Arc::new(AtomicBool::new(false)),
        }
    }
}
fn consume(n: &AtomicUsize) -> bool {
    n.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok()
}
impl DisplayTopology for FaultTopology {
    fn simulation_link(&self) -> Option<kvmflow_sidecar::topology::SimulationTopologyLink> {
        self.inner.simulation_link()
    }
    fn list(&mut self) -> Result<Vec<TopologyDisplay>, kvmflow_core::errors::CoreError> {
        if self.list_failed.load(Ordering::SeqCst) {
            Err(kvmflow_core::errors::CoreError::new(
                "E_TOPOLOGY_LIST",
                "injected enumeration error",
            ))
        } else {
            self.inner.list()
        }
    }
    fn attach(&mut self, fp: &str) -> Result<(), kvmflow_core::errors::CoreError> {
        if consume(&self.attach_failures) {
            Err(kvmflow_core::errors::CoreError::new(
                "E_TOPOLOGY_ATTACH",
                "injected transient attach failure",
            ))
        } else {
            self.inner.attach(fp)
        }
    }
    fn detach(&mut self, fp: &str) -> Result<(), kvmflow_core::errors::CoreError> {
        self.inner.detach(fp)
    }
    fn wait_enumeration(&mut self) -> Result<(), kvmflow_core::errors::CoreError> {
        if consume(&self.wait_failures) {
            Err(kvmflow_core::errors::CoreError::new(
                "E_TOPOLOGY_ENUMERATION",
                "injected transient wait failure",
            ))
        } else {
            self.inner.wait_enumeration()
        }
    }
    fn restore_all(&mut self) -> Result<(), kvmflow_core::errors::CoreError> {
        self.inner.restore_all()
    }
}
fn await_state(rt: &RuntimeHandle, mut f: impl FnMut(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let s = rt.request("state.get", json!({})).unwrap();
        if f(&s) {
            return s;
        }
        assert!(Instant::now() < deadline, "state predicate timed out: {s}");
        std::thread::sleep(Duration::from_millis(30));
    }
}
fn await_holder(rt: &RuntimeHandle) {
    await_state(rt, |s| s["mixed"]["keyboard_holder_device_id"] == "mac");
}
fn remote(revision: u64, enabled: bool, holder: Option<&str>) -> Value {
    json!({"group_id":"desk","revision":revision,"enabled":enabled,"assignment":config()["switch_group"]["mixed"]["assignment"],"keyboard_holder_device_id":holder})
}
#[test]
fn review_remote_entry_is_trusted_versioned_persisted_and_does_not_echo() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, rx) = start(dir.path(), "idle", &config(), Box::new(topology()));
    await_holder(&rt);
    while rx.try_recv().is_ok() {}
    assert_eq!(
        rt.request("mixed.apply_remote", remote(8, true, Some("mac")))
            .unwrap_err()
            .code,
        "E_UNKNOWN_METHOD"
    );
    assert_eq!(
        rt.apply_remote_mixed(remote(8, true, Some("mac"))).unwrap()["applied"],
        true
    );
    let current = rt.request("config.get", json!({})).unwrap();
    assert_eq!(current["switch_group"]["revision"], 1);
    assert_eq!(current["switch_group"]["mixed"]["revision"], 8);
    for revision in [7, 8] {
        assert_eq!(
            rt.apply_remote_mixed(remote(revision, false, Some("mac")))
                .unwrap()["applied"],
            false
        );
    }
    assert_eq!(
        rt.request("state.get", json!({})).unwrap()["mixed"]["enabled"],
        true
    );
    let wrong = json!({"group_id":"wrong","revision":9,"enabled":false,"assignment":[],"keyboard_holder_device_id":"mac"});
    assert!(rt.apply_remote_mixed(wrong).is_err());
    let notices: Vec<_> = rx.try_iter().collect();
    assert!(notices.iter().any(|(k, _)| k == "state"));
    assert!(notices.iter().all(|(k, _)| k != "mixed.changed"));
    let disk: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("config.json")).unwrap()).unwrap();
    assert_eq!(disk["switch_group"]["mixed"]["revision"], 8);
}
#[test]
fn review_section_meta_orders_equal_revisions_and_settings_cannot_overwrite_it() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, _) = start(dir.path(), "idle", &config(), Box::new(topology()));
    let mut first = remote(8, false, None);
    first["meta"] = json!({"updated_at_ms":100,"updated_by":"pc"});
    assert_eq!(
        rt.apply_remote_mixed(first.clone()).unwrap()["applied"],
        true
    );
    let mut next = first.clone();
    next["meta"]["updated_at_ms"] = json!(101);
    next["assignment"] = json!([]);
    assert_eq!(
        rt.apply_remote_mixed(next.clone()).unwrap()["applied"],
        true
    );
    assert_eq!(rt.apply_remote_mixed(first).unwrap()["applied"], false);
    let mut cfg = rt.request("config.get", json!({})).unwrap();
    cfg["switch_group"]["sync"]["mixed"] = json!({"updated_at_ms":0,"updated_by":"stale"});
    rt.request("config.set", json!({"config":cfg})).unwrap();
    assert_eq!(
        rt.request("config.get", json!({})).unwrap()["switch_group"]["sync"]["mixed"],
        next["meta"]
    );
}
#[test]
fn review_noop_set_is_idempotent_and_does_not_write_or_advance_revision() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    let (rt, _) = start(dir.path(), "idle", &config(), Box::new(topo.clone()));
    let disk = std::fs::read(dir.path().join("config.json")).unwrap();
    let r = rt.request("mixed.set", json!({"enabled":false})).unwrap();
    assert_eq!(r["saved"], false);
    assert_eq!(r["mixed"]["revision"], 4);
    assert_eq!(std::fs::read(dir.path().join("config.json")).unwrap(), disk);
    assert!(topo.operations.lock().unwrap().is_empty());
    let r = rt.request("mixed.set", json!({"enabled":true})).unwrap();
    assert_eq!(r["mixed"]["revision"], 5);
    let writes = topo.operations.lock().unwrap().clone();
    let again = rt.request("mixed.set", json!({"enabled":true})).unwrap();
    assert_eq!(again["mixed"]["revision"], 5);
    assert_eq!(*topo.operations.lock().unwrap(), writes);
}
#[test]
fn review_normal_mode_start_settings_and_hotplug_never_queue_pushes() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    let (rt, _) = start(
        dir.path(),
        "manual_layout",
        &config(),
        Box::new(topo.clone()),
    );
    await_holder(&rt);
    let cfg = rt.request("config.get", json!({})).unwrap();
    rt.request("config.set", json!({"config":cfg})).unwrap();
    assert!(
        rt.request("state.get", json!({})).unwrap()["mixed"]["pending_remote"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    topo.displays.lock().unwrap().push(TopologyDisplay {
        fingerprint: "new".into(),
        builtin: false,
        connected: true,
    });
    let s = await_state(&rt, |s| {
        s["mixed"]["destinations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["fingerprint"] == "new")
    });
    assert!(s["mixed"]["execution"].as_array().unwrap().is_empty());
    assert!(s["mixed"]["pending_remote"].as_array().unwrap().is_empty());
    assert!(topo.operations.lock().unwrap().is_empty());
}
#[test]
fn review_disabled_crash_leftovers_attach_and_wait_without_ddc_or_remote_push() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    topo.displays.lock().unwrap()[1].connected = false;
    topo.enumerated.lock().unwrap().remove(SECOND);
    let (rt, _) = start(dir.path(), "idle", &config(), Box::new(topo.clone()));
    let s = rt.request("state.get", json!({})).unwrap();
    assert_eq!(s["mixed"]["detached"], json!([]));
    assert_eq!(s["mixed"]["pending_remote"], json!([]));
    assert_eq!(
        *topo.operations.lock().unwrap(),
        vec![format!("attach:{SECOND}"), "wait_enumeration".into()]
    );
    assert_eq!(
        rt.request("display.readInput", json!({"edid_id":SECOND}))
            .unwrap()["value"],
        7
    );
}
#[test]
fn review_real_simulated_ddc_fails_detached_and_enable_pushes_correct_value_first() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    let (rt, _) = start(dir.path(), "idle", &config(), Box::new(topo.clone()));
    let r = rt.request("mixed.set", json!({"enabled":true})).unwrap();
    assert_eq!(
        *topo.operations.lock().unwrap(),
        vec![format!("ddc_push:{SECOND}:8"), format!("detach:{SECOND}")]
    );
    assert_eq!(r["mixed"]["execution"][0]["step"]["input"], 8);
    let result = rt
        .request("display.writeInput", json!({"edid_id":SECOND,"value":7}))
        .unwrap();
    assert_eq!(result["commanded"], false);
    assert!(result["last_error"]
        .as_str()
        .unwrap()
        .starts_with("E_TOPOLOGY_NOT_CONNECTED"));
}
#[test]
fn review_close_to_remote_holder_attaches_waits_then_pushes_local_screen() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    let (rt, rx) = start(dir.path(), "idle", &config(), Box::new(topo.clone()));
    await_holder(&rt);
    rt.request("mixed.set", json!({"enabled":true})).unwrap();
    topo.operations.lock().unwrap().clear();
    let r = rt
        .apply_remote_mixed(remote(6, false, Some("windows")))
        .unwrap();
    let ops = topo.operations.lock().unwrap().clone();
    let attach = ops
        .iter()
        .position(|s| s == &format!("attach:{SECOND}"))
        .unwrap();
    let wait = ops.iter().position(|s| s == "wait_enumeration").unwrap();
    let push = ops
        .iter()
        .position(|s| s == &format!("ddc_push:{MAIN}:16"))
        .unwrap();
    assert!(attach < wait && wait < push, "{ops:?}");
    assert_eq!(r["mixed"]["holder_confidence"], "known");
    assert!(rx
        .try_iter()
        .any(|(k, d)| k == "state" && d["mixed"]["transition"]["phase"] == "disabling"));
}
#[test]
fn review_failed_attach_and_wait_retry_without_inventory_changes_with_backoff() {
    for fail_wait in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let topo = topology();
        topo.displays.lock().unwrap()[1].connected = false;
        topo.enumerated.lock().unwrap().remove(SECOND);
        let fault = FaultTopology::new(topo.clone());
        if fail_wait {
            fault.wait_failures.store(1, Ordering::SeqCst);
        } else {
            fault.attach_failures.store(1, Ordering::SeqCst);
        }
        let (rt, rx) = start(dir.path(), "idle", &config(), Box::new(fault));
        let first = rt.request("state.get", json!({})).unwrap();
        assert!(
            first["mixed"]["retry"]["next_retry_ms"].as_u64().unwrap()
                >= first["mixed"]["transition"]["started_ms"].as_u64().unwrap() + 500
        );
        assert!(first["mixed"]["execution"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["error"]["code"]
                == if fail_wait {
                    "E_TOPOLOGY_ENUMERATION"
                } else {
                    "E_TOPOLOGY_ATTACH"
                }));
        let done = await_state(&rt, |s| {
            s["mixed"]["detached"] == json!([]) && s["mixed"]["retry"]["next_retry_ms"].is_null()
        });
        assert_eq!(done["mixed"]["transition"]["outcome"], "ok");
        assert_eq!(
            rt.request("display.readInput", json!({"edid_id":SECOND}))
                .unwrap()["value"],
            7,
            "retry must actually complete enumeration, not only clear the error"
        );
        assert!(rx.try_iter().any(|(k, d)| k == "mixed.warning"
            && d["code"]
                == if fail_wait {
                    "E_TOPOLOGY_ENUMERATION"
                } else {
                    "E_TOPOLOGY_ATTACH"
                }));
        assert!(topo
            .operations
            .lock()
            .unwrap()
            .iter()
            .all(|s| !s.starts_with("ddc_push")));
    }
}
#[test]
fn review_ddc_list_failure_is_closed_and_recovers_on_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    let fault = FaultTopology::new(topo.clone());
    let control = fault.clone();
    let (rt, rx) = start(dir.path(), "idle", &config(), Box::new(fault));
    control.list_failed.store(true, Ordering::SeqCst);
    let failed = rt.request("mixed.set", json!({"enabled":true})).unwrap();
    assert_eq!(
        failed["mixed"]["execution"][0]["error"]["code"],
        "E_TOPOLOGY_LIST"
    );
    assert!(topo.operations.lock().unwrap().is_empty());
    assert!(failed["mixed"]["retry"]["next_retry_ms"].is_number());
    control.list_failed.store(false, Ordering::SeqCst);
    let done = await_state(&rt, |s| s["mixed"]["detached"] == json!([SECOND]));
    assert_eq!(done["mixed"]["transition"]["outcome"], "ok");
    assert!(rx
        .try_iter()
        .any(|(k, d)| k == "mixed.warning" && d["code"] == "E_TOPOLOGY_LIST"));
}
#[test]
fn review_new_warning_is_emitted_once_and_topology_noop_is_visible_in_validation() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    let (rt, rx) = start(dir.path(), "idle", &config(), Box::new(topo.clone()));
    rt.request("mixed.set", json!({"enabled":true})).unwrap();
    while rx.try_recv().is_ok() {}
    topo.displays.lock().unwrap().push(TopologyDisplay {
        fingerprint: "new".into(),
        builtin: false,
        connected: true,
    });
    await_state(&rt, |s| {
        s["mixed"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "unassigned_monitor")
    });
    let cfg = rt.request("config.get", json!({})).unwrap();
    rt.request("config.set", json!({"config":cfg})).unwrap();
    assert_eq!(
        rx.try_iter()
            .filter(|(k, d)| k == "mixed.warning" && d["code"] == "unassigned_monitor")
            .count(),
        1
    );
    let dir = tempfile::tempdir().unwrap();
    let (rt, _) = start(
        dir.path(),
        "idle",
        &config(),
        Box::new(kvmflow_sidecar::topology::NoopDisplayTopology),
    );
    let s = rt.request("state.get", json!({})).unwrap();
    assert_eq!(s["mixed"]["validation"]["ok"], true);
    assert!(s["mixed"]["validation"]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w["code"] == "topology_unavailable" && w["severity"] == "warning"));
}
#[test]
fn review_pending_ids_stable_across_replans_and_superseded_by_new_revision() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    let (rt, _) = start(dir.path(), "manual_layout", &config(), Box::new(topo));
    await_holder(&rt);
    rt.request("mixed.set", json!({"enabled":true})).unwrap();
    let off = rt.request("mixed.set", json!({"enabled":false})).unwrap();
    let id = off["mixed"]["pending_remote"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(id, expected_pending_id(&off["mixed"], SECOND, "mac"));
    let cfg = rt.request("config.get", json!({})).unwrap();
    rt.request("config.set", json!({"config":cfg})).unwrap();
    assert_eq!(
        rt.request("state.get", json!({})).unwrap()["mixed"]["pending_remote"][0]["id"],
        id
    );

    let newer = rt
        .apply_remote_mixed(remote(7, false, Some("windows")))
        .unwrap();
    assert!(newer["mixed"]["superseded_remote"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["id"] == id && p["status"] == "superseded"));
    assert!(newer["mixed"]["pending_remote"]
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["id"] != id));
}
#[test]
fn review_status_labels_target_evidence_and_builtin_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    topo.displays.lock().unwrap().push(TopologyDisplay {
        fingerprint: "builtin".into(),
        builtin: true,
        connected: true,
    });
    let mut cfg = config();
    cfg["local_device"]["monitors"][0]["label"] = json!("Desk main");
    let (rt, _) = start(dir.path(), "idle", &cfg, Box::new(topo.clone()));
    let on = rt.request("mixed.set", json!({"enabled":true})).unwrap();
    let d = on["mixed"]["destinations"].as_array().unwrap();
    assert_eq!(d[0]["display_name"], "Desk main");
    assert_eq!(d[1]["display_name"], "显示器 2");
    assert_eq!(d[1]["target_device_id"], "windows");
    assert_eq!(d[1]["evidence"], "commanded_unconfirmed");
    assert!(d[1]["current_device_id"].is_null());
    topo.displays.lock().unwrap().retain(|d| !d.builtin);
    let s = await_state(&rt, |s| {
        s["mixed"]["destinations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| {
                d["fingerprint"] == "builtin"
                    && d["unavailable_reason"] == "builtin_closed_or_unavailable"
            })
    });
    assert!(s["mixed"]["destinations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["fingerprint"] == "builtin" && d["available"] == false));
}
#[test]
fn review_failed_ddc_retries_correct_value_then_detaches_without_inventory_change() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    topo.ddc_failures.lock().unwrap().insert(SECOND.into(), 2);
    let (rt, _) = start(dir.path(), "idle", &config(), Box::new(topo.clone()));
    let first = rt.request("mixed.set", json!({"enabled":true})).unwrap();
    assert_eq!(
        first["mixed"]["execution"][0]["error"]["code"],
        "E_DDC_FAILED"
    );
    assert_eq!(first["mixed"]["detached"], json!([]));
    assert_eq!(first["mixed"]["transition"]["outcome"], "failed");
    let s = await_state(&rt, |s| s["mixed"]["detached"] == json!([SECOND]));
    assert_eq!(s["mixed"]["transition"]["outcome"], "ok");
    assert_eq!(
        *topo.operations.lock().unwrap(),
        vec![
            format!("ddc_failed:{SECOND}:8"),
            format!("ddc_failed:{SECOND}:8"),
            format!("ddc_push:{SECOND}:8"),
            format!("detach:{SECOND}")
        ]
    );
}
#[test]
fn review_enabled_usb_departure_has_zero_additional_ddc_writes() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    let mut cfg = config();
    cfg["switch_group"]["mixed"]["enabled"] = json!(true);
    let (rt, _) = start(
        dir.path(),
        "fast_clean_roundtrip",
        &cfg,
        Box::new(topo.clone()),
    );
    await_holder(&rt);
    let initial = topo.operations.lock().unwrap().clone();
    let s = await_state(&rt, |s| {
        s["mixed"]["keyboard_holder_device_id"] == "windows"
    });
    assert_eq!(*topo.operations.lock().unwrap(), initial);
    assert_eq!(s["mixed"]["holder_confidence"], "known");
    // Positive ordinary-mode pair proves this fixture does observe USB writes.
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    let (rt, _) = start(
        dir.path(),
        "fast_clean_roundtrip",
        &config(),
        Box::new(topo.clone()),
    );
    await_state(&rt, |s| {
        s["mixed"]["keyboard_holder_device_id"] == "windows"
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    while topo
        .operations
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.starts_with("ddc_push"))
        .count()
        < 2
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(topo
        .operations
        .lock()
        .unwrap()
        .contains(&format!("ddc_push:{MAIN}:16")));
    assert!(topo
        .operations
        .lock()
        .unwrap()
        .contains(&format!("ddc_push:{SECOND}:8")));
}
#[test]
fn review_three_computer_inferred_holder_cannot_push_on_close() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    let mut cfg = config();
    cfg["switch_group"]["mixed"]["enabled"] = json!(true);
    cfg["switch_group"]["devices"].as_array_mut().unwrap().push(json!({"device_id":"third","name":"Third","port_index":3,"monitors":[{"fingerprint":MAIN,"local_input":17},{"fingerprint":SECOND,"local_input":9}]}));
    let (rt, _) = start(
        dir.path(),
        "fast_clean_roundtrip",
        &cfg,
        Box::new(topo.clone()),
    );
    let state = await_state(&rt, |s| {
        s["mixed"]["keyboard_holder_device_id"] == "windows"
    });
    assert_eq!(state["mixed"]["holder_confidence"], "inferred");
    topo.operations.lock().unwrap().clear();
    let off = rt.request("mixed.set", json!({"enabled":false})).unwrap();
    assert!(topo
        .operations
        .lock()
        .unwrap()
        .iter()
        .all(|s| !s.starts_with("ddc_push")));
    assert_eq!(off["mixed"]["pending_remote"], json!([]));
    assert_eq!(off["mixed"]["transition"]["outcome"], "partial");
    assert!(off["mixed"]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["code"] == "holder_inferred"));
}
#[test]
fn review_config_set_bootstrap_cannot_enable_mixed_or_choose_its_revision() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = RuntimeOptions::new(dir.path().join("config.json"), dir.path().join("logs"));
    opts.backend = "simulated".into();
    opts.scenario = "idle".into();
    opts.topology = Some(Box::new(topology()));
    let rt = RuntimeHandle::start(opts, |_, _| {}).unwrap();
    let mut cfg = config();
    cfg["switch_group"]["mixed"]["enabled"] = json!(true);
    cfg["switch_group"]["mixed"]["revision"] = json!(99);
    rt.request("config.set", json!({"config":cfg})).unwrap();
    let state = rt.request("state.get", json!({})).unwrap();
    assert_eq!(state["mixed"]["enabled"], false);
    assert_eq!(state["mixed"]["revision"], 0);
}
#[test]
fn review_persistent_failure_increases_backoff_without_repeating_same_warning() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    let (rt, rx) = start(
        dir.path(),
        "idle_ddc_fail",
        &config(),
        Box::new(topo.clone()),
    );
    let first = rt.request("mixed.set", json!({"enabled":true})).unwrap();
    assert_eq!(
        first["mixed"]["execution"][0]["error"]["code"],
        "E_DDC_FAILED"
    );
    let second = await_state(&rt, |s| s["mixed"]["retry"]["attempt"] == 2);
    assert!(
        second["mixed"]["retry"]["next_retry_ms"].as_u64().unwrap()
            >= second["mixed"]["transition"]["started_ms"]
                .as_u64()
                .unwrap()
                + 1000
    );
    assert!(topo.operations.lock().unwrap().is_empty());
    assert_eq!(
        rx.try_iter()
            .filter(|(k, d)| k == "mixed.warning" && d["code"] == "E_DDC_FAILED")
            .count(),
        1
    );
}

#[test]
fn combination_disabled_assign_preserves_unfinished_enumeration_retry() {
    let dir = tempfile::tempdir().unwrap();
    let topo = topology();
    let fault = FaultTopology::new(topo.clone());
    let control = fault.clone();
    let mut cfg = config();
    cfg["switch_group"]["mixed"]["enabled"] = json!(true);
    let (rt, _) = start(dir.path(), "idle", &cfg, Box::new(fault));
    await_holder(&rt);
    control.wait_failures.store(1, Ordering::SeqCst);
    let first = rt.request("mixed.set", json!({"enabled":false})).unwrap();
    assert!(first["mixed"]["retry"]["next_retry_ms"].is_number());
    assert_eq!(first["mixed"]["detached"], json!([]));
    let assignment = config()["switch_group"]["mixed"]["assignment"].clone();
    let saved = rt
        .request("mixed.assign", json!({"assignment":assignment}))
        .unwrap();
    assert_eq!(
        saved["mixed"]["retry"], first["mixed"]["retry"],
        "saving disabled assignment must preserve the unfinished retry deadline and backoff"
    );
    await_state(&rt, |s| s["mixed"]["retry"]["next_retry_ms"].is_null());
    assert!(topo.enumerated.lock().unwrap().contains(SECOND));
    assert_eq!(
        rt.request("display.readInput", json!({"edid_id":SECOND}))
            .unwrap_err()
            .code,
        "E_DDC_NOT_READABLE",
        "after enumeration the simulator must reach its picture-ownership check"
    );
}
#[test]
fn combination_initial_stable_with_same_holder_preserves_close_pending() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config();
    cfg["switch_group"]["mixed"]["enabled"] = json!(true);
    cfg["trigger"]["debounce"]["t_stable_ms"] = json!(1000);
    let (rt, _) = start(dir.path(), "idle", &cfg, Box::new(topology()));
    await_holder(&rt);
    let closed = rt.request("mixed.set", json!({"enabled":false})).unwrap();
    assert_eq!(
        closed["mixed"]["pending_remote"].as_array().unwrap().len(),
        1
    );
    let pending_id = closed["mixed"]["pending_remote"][0]["id"].clone();
    std::thread::sleep(Duration::from_millis(1700));
    let stable = rt.request("state.get", json!({})).unwrap();
    assert_eq!(stable["groupStable"], true);
    assert_eq!(closed["mixed"]["keyboard_holder_device_id"], "mac");
    assert_eq!(stable["mixed"]["keyboard_holder_device_id"], "mac");
    assert!(
        stable["mixed"]["pending_remote"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == pending_id),
        "initial stability with unchanged H must not supersede close work"
    );
    assert!(stable["mixed"]["superseded_remote"]
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["id"] != pending_id));
}
#[test]
fn combination_actual_handoff_still_supersedes_close_pending() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config();
    cfg["switch_group"]["mixed"]["enabled"] = json!(true);
    cfg["trigger"]["debounce"]["t_stable_ms"] = json!(1000);
    let (rt, _) = start(
        dir.path(),
        "fast_clean_roundtrip",
        &cfg,
        Box::new(topology()),
    );
    await_holder(&rt);
    let closed = rt.request("mixed.set", json!({"enabled":false})).unwrap();
    let id = closed["mixed"]["pending_remote"][0]["id"].clone();
    assert!(id.is_string());
    std::thread::sleep(Duration::from_millis(2100));
    let departed = rt.request("state.get", json!({})).unwrap();
    assert_eq!(departed["mixed"]["keyboard_holder_device_id"], "windows");
    assert!(departed["mixed"]["pending_remote"]
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["id"] != id));
    assert!(departed["mixed"]["superseded_remote"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["id"] == id && p["status"] == "superseded"));
}
#[test]
fn combination_newer_section_meta_does_not_reactivate_superseded_id() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, _) = start(dir.path(), "manual_layout", &config(), Box::new(topology()));
    await_holder(&rt);
    let mut params = remote(8, true, Some("mac"));
    params["meta"] = json!({"updated_at_ms":100,"updated_by":"pc"});
    let first = rt.apply_remote_mixed(params.clone()).unwrap();
    let previous = first["mixed"]["pending_remote"][0]["id"].clone();
    assert!(previous.is_string());
    params["meta"]["updated_at_ms"] = json!(101);
    let next = rt.apply_remote_mixed(params).unwrap();
    let active = next["mixed"]["pending_remote"].as_array().unwrap();
    let retired = next["mixed"]["superseded_remote"].as_array().unwrap();
    assert!(!active.is_empty());
    assert!(
        active
            .iter()
            .all(|p| retired.iter().all(|q| p["id"] != q["id"])),
        "an ID must never be both active and superseded"
    );
    assert!(retired.iter().any(|p| p["id"] == previous));
}

#[test]
fn lan_internal_registers_are_hidden_protected_and_preserved_by_settings() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config();
    cfg["switch_group"]["sync"] = json!({"members":[{"device_id":"retired-slot","public_key":"key-retired"}],"profile_cache":{"retired-slot":{"profile_rev":7,"monitors":[{"fingerprint":MAIN,"local_input":20}],"available_builtin_screens":2}},"archived_arrivals":{"retired-slot":"archived-policy"}});
    let (rt, _) = start(dir.path(), "idle", &cfg, Box::new(topology()));
    let mut view = rt.request("config.get", json!({})).unwrap();
    assert!(view["switch_group"]["sync"].get("profile_cache").is_none());
    assert!(view["switch_group"]["sync"]
        .get("archived_arrivals")
        .is_none());
    view["local_device"]["host_label"] = json!("Updated Mac");
    rt.request("config.set", json!({"config":view})).unwrap();
    let saved: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("config.json")).unwrap()).unwrap();
    assert_eq!(
        saved["switch_group"]["sync"]["profile_cache"]["retired-slot"]["profile_rev"],
        7
    );
    assert_eq!(
        saved["switch_group"]["sync"]["archived_arrivals"]["retired-slot"],
        "archived-policy"
    );
    assert!(
        saved["switch_group"]["sync"]["profile_cache"]["retired-slot"]
            .get("available_builtin_screens")
            .is_none()
    );
    let mut omitted = rt.request("config.get", json!({})).unwrap();
    omitted["switch_group"]["sync"] = Value::Null;
    rt.request("config.set", json!({"config":omitted})).unwrap();
    let preserved: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("config.json")).unwrap()).unwrap();
    assert_eq!(
        preserved["switch_group"]["sync"]["profile_cache"]["retired-slot"]["profile_rev"],
        7
    );
    let before = std::fs::read(dir.path().join("config.json")).unwrap();
    for field in ["profile_cache", "archived_arrivals"] {
        let mut forged = rt.request("config.get", json!({})).unwrap();
        forged["switch_group"]["sync"][field] = json!({});
        assert_eq!(
            rt.request("config.set", json!({"config":forged}))
                .unwrap_err()
                .code,
            "E_NOT_OWNER"
        );
        assert_eq!(
            std::fs::read(dir.path().join("config.json")).unwrap(),
            before
        );
    }
    rt.shutdown_and_join().unwrap();
}
