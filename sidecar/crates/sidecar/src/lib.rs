//! Shared runtime for the Tauri desktop host and the standalone JSONL server.
//! Hardware and state transitions execute on one worker, never the UI thread.

mod backends;
mod config_store;
mod detach;
pub mod lan;
pub mod lan_runtime;
mod layouts;
mod mixed;
mod requests;
mod session;
mod switching;
pub mod topology;
mod usb;

use switching::BounceWatch;
use usb::spawn_usb_source;

use backends::{Backend, UsbDevice};
use kvmflow_core::config::{
    Config, ConfigDocument, LegacyMigration, MultiDeviceConfig, TriggerConfig,
};
use kvmflow_core::debounce::{DeviceKey, GroupTracker, TrackerEvent};
use kvmflow_core::errors::*;
use kvmflow_core::events::kinds;
use kvmflow_core::events::{EventRecord, Level};
use kvmflow_core::protocol::{
    encode, parse_client_line, NotificationLine, ResponseLine, NOTIFICATION_KINDS,
    PROTOCOL_VERSION, REQUEST_METHODS,
};
use kvmflow_core::statemachine::{Action, Input, MonitorOutcome, SwitchFsm, REPORT_NOTE};
use kvmflow_core::transaction::local_arrival_correction;
use serde_json::{json, Value};
use session::{now_ms, SessionLog};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Condvar, Mutex,
};
use std::thread;
use std::time::Duration;

/// Host wait budget, including in-flight DDC work and display recovery.
/// If a large topology exceeds it, the worker continues and exit is cancelled.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(45);

enum Msg {
    Line(String),
    Request(kvmflow_core::protocol::RequestLine, Sender<ResponseLine>),
    RemoteMixed(Value, Sender<ResponseLine>),
    Usb(Vec<UsbDevice>),
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    HubProbe(Vec<(String, String)>),
    /// Restore every detached display and stop touching the topology until
    /// `ResumeDisplays` (used before an installer may end the process).
    ReleaseDisplays(Sender<Result<(), String>>),
    ResumeDisplays(Option<bool>, Sender<Result<(), String>>),
    Shutdown,
}

struct Runtime {
    runtime_seq: u64,
    lan_runtime: lan_runtime::LanRuntime,
    backend: Box<dyn Backend>,
    topology: Box<dyn topology::DisplayTopology>,
    /// The real adapter while `ReleaseDisplays` has swapped in the no-op one.
    released_topology: Option<Box<dyn topology::DisplayTopology>>,
    released_enabled: bool,
    mixed_runtime: mixed::MixedRuntime,
    tracker: Option<GroupTracker>,
    fsm: SwitchFsm,
    /// The switch group configuration. A schema v1 file is migrated into it
    /// when it is loaded or saved; there is no separate legacy runtime.
    config: Option<MultiDeviceConfig>,
    /// Tray pause. While false, USB triggers write nothing.
    enabled: bool,
    /// Last v2 write attempt. It is deliberately separate from the legacy
    /// state-machine report because v2 has source and arrival-correction
    /// phases rather than a single "push away" action.
    last_multi_device_report: Option<Value>,
    last_layout_application: Option<Value>,
    bounce_watch: Option<BounceWatch>,
    usb_snapshot_initialized: bool,
    config_path: PathBuf,
    log: SessionLog,
    stdout: std::io::Stdout,
    notification: Option<NotificationSink>,
    reply: Option<Sender<ResponseLine>>,
    last_snapshot: Vec<UsbDevice>,
    learning_baseline: Option<Vec<UsbDevice>>,
    last_candidates: Option<Value>,
    wizard_active: bool,
    /// Set when the saved config could not be parsed at startup, so the UI
    /// can explain why setup is required again. Cleared by the next save.
    config_recovery: Option<Value>,
}

impl Runtime {
    // The hardware worker serializes every snapshot before it enters an async
    // transport. Consumers can reject an older reply delivered after an event.
    fn stamp_snapshot(&mut self, data: &mut Value) {
        self.runtime_seq = self
            .runtime_seq
            .checked_add(1)
            .expect("runtime sequence exhausted");
        data["runtime_seq"] = json!(self.runtime_seq);
        if data["mixed"].is_object() {
            data["mixed"]["runtime_seq"] = json!(self.runtime_seq);
        }
    }

    fn notify(&mut self, kind: &str, mut data: Value) {
        if kind == "switch.report" {
            // 「试一下」evidence: tag this machine's report with the active
            // test/press identity before it reaches the UI or the peer.
            self.lan_test_report(&mut data);
        }
        if matches!(kind, "state" | "mixed.changed" | "mixed.pending_remote") {
            self.stamp_snapshot(&mut data);
        }
        if let Some(sink) = &self.notification {
            sink(kind, data);
            return;
        }
        let line = encode(&NotificationLine::new(kind, data));
        let _ = self.stdout.write_all(line.as_bytes());
        let _ = self.stdout.flush();
    }

    fn emit_event(&mut self, kind: &str, level: Level, fields: Value) {
        let mut map = serde_json::Map::new();
        if let Value::Object(o) = fields {
            map = o;
        }
        let record = EventRecord {
            ts_ms: now_ms(),
            kind: kind.to_string(),
            level,
            fields: map,
        };
        self.log.write(&record);
        let data = serde_json::to_value(&record).unwrap_or(Value::Null);
        self.notify("event", data);
    }

    fn respond(&mut self, mut resp: ResponseLine) {
        if let Some(data) = resp.result.as_mut() {
            if data.get("mixed").is_some()
                || (data.get("state").is_some() && data.get("enabled").is_some())
            {
                self.stamp_snapshot(data);
            }
        }
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(resp);
            return;
        }
        let line = encode(&resp);
        let _ = self.stdout.write_all(line.as_bytes());
        let _ = self.stdout.flush();
    }

    fn run_actions(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Event {
                    kind,
                    level,
                    fields,
                } => self.emit_event(kind, level, fields),
                // State changes always carry the config mode and pause flag.
                Action::Notify { kind: "state", .. } => {
                    let data = self.state_notification();
                    self.notify("state", data);
                }
                Action::Notify { kind, data } => self.notify(kind, data),
            }
        }
    }
}

impl Runtime {
    /// Restores every detached display and reports a failure; silent otherwise.
    fn restore_displays(&mut self, phase: &str) -> Result<(), String> {
        let restored = self.topology.restore_all();
        if let Err(error) = &restored {
            self.emit_event(
                kinds::DISPLAY_RESTORE,
                Level::Warn,
                json!({"phase": phase, "error": error.to_string()}),
            );
            self.notify(
                "mixed.warning",
                json!({"code":"restore_failed", "severity":"warning", "device_id":null, "fingerprint":null, "error": {"code":error.code, "message":error.message}}),
            );
        }
        restored.map_err(|error| error.to_string())
    }

    fn release_displays(&mut self) -> Result<(), String> {
        if self.released_topology.is_some() {
            return Ok(());
        }
        self.topology
            .restore_all()
            .map_err(|error| error.to_string())?;
        let real = std::mem::replace(&mut self.topology, Box::new(topology::NoopDisplayTopology));
        self.released_topology = Some(real);
        self.released_enabled = self.enabled;
        self.set_runtime_enabled(false);
        Ok(())
    }

    fn set_runtime_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.bounce_watch = None;
        }
        let actions = self.fsm.handle(Input::SetDisabled(!enabled), now_ms());
        self.run_actions(actions);
    }

    fn resume_displays(&mut self, enabled: Option<bool>) -> Result<(), String> {
        let was_released = self.released_topology.is_some();
        if let Some(real) = self.released_topology.take() {
            self.topology = real;
        }
        if was_released || enabled.is_some() {
            // Restore may have changed CG/DDC indices or GDI names while the
            // worker was frozen. Refresh before enabling any subsequent write.
            self.backend
                .refresh_displays_with_timeout(Duration::from_secs(3))
                .map_err(|error| error.to_string())?;
            self.set_runtime_enabled(enabled.unwrap_or(self.released_enabled));
        }
        Ok(())
    }
}

fn default_config_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join("Library/Application Support/KVMFlow/config.json")
    } else {
        let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
        PathBuf::from(appdata).join("KVMFlow").join("config.json")
    }
}

fn default_log_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join("Library/Logs/KVMFlow")
    } else {
        let appdata = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
        PathBuf::from(appdata).join("KVMFlow").join("logs")
    }
}

pub fn run_stdio() {
    let mut backend_name = String::from("auto");
    let mut scenario = String::from("idle");
    let mut fixture: Option<String> = None;
    let mut fixture_scale = 1.0f64;
    let mut config_path = default_config_path();
    let mut log_dir = default_log_dir();
    #[cfg(target_os = "macos")]
    let mut ddc_binary: Option<String> = None;

    let mut lan_options = lan_runtime::RuntimeLanOptions::disabled();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--lan" => lan_options.enabled = true,
            "--lan-port" => {
                lan_options.transport.listen_port =
                    args.next().and_then(|v| v.parse().ok()).unwrap_or(47820)
            }
            "--lan-peer" => {
                if let Some(addr) = args.next().and_then(|v| v.parse().ok()) {
                    lan_options.peers.push(addr);
                }
            }
            "--backend" => backend_name = args.next().unwrap_or_default(),
            "--scenario" => scenario = args.next().unwrap_or_default(),
            "--fixture" => fixture = args.next(),
            "--fixture-scale" => {
                fixture_scale = args.next().and_then(|s| s.parse().ok()).unwrap_or(1.0)
            }
            "--config" => config_path = PathBuf::from(args.next().unwrap_or_default()),
            "--log-dir" => log_dir = PathBuf::from(args.next().unwrap_or_default()),
            "--ddc-binary" => {
                #[cfg(target_os = "macos")]
                {
                    ddc_binary = args.next();
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = args.next();
                }
            }
            _ => {}
        }
    }
    let options = RuntimeOptions {
        backend: backend_name,
        lan: Some(lan_options),
        scenario,
        fixture,
        fixture_scale,
        config_path,
        log_dir,
        topology: None,
        ddc_binary: {
            #[cfg(target_os = "macos")]
            {
                ddc_binary
            }
            #[cfg(not(target_os = "macos"))]
            {
                None
            }
        },
    };
    let (tx, rx) = mpsc::channel();
    {
        let tx = tx.clone();
        thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                match line {
                    Ok(line) => {
                        if tx.send(Msg::Line(line)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.send(Msg::Shutdown);
        });
    }
    if let Err(error) = run_runtime(
        options,
        tx,
        rx,
        None,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    ) {
        eprintln!("kvmflow: {error}");
        std::process::exit(3);
    }
}

type NotificationSink = Arc<dyn Fn(&str, Value) + Send + Sync>;

pub struct RuntimeOptions {
    /// None: enabled for native embedded runtime, disabled for simulation.
    pub lan: Option<lan_runtime::RuntimeLanOptions>,
    pub backend: String,
    pub scenario: String,
    pub fixture: Option<String>,
    pub fixture_scale: f64,
    pub config_path: PathBuf,
    pub log_dir: PathBuf,
    pub ddc_binary: Option<String>,
    pub topology: Option<Box<dyn topology::DisplayTopology>>,
}

impl RuntimeOptions {
    pub fn new(config_path: PathBuf, log_dir: PathBuf) -> Self {
        Self {
            backend: std::env::var("KVMFLOW_BACKEND").unwrap_or_else(|_| "auto".into()),
            scenario: std::env::var("KVMFLOW_SCENARIO").unwrap_or_else(|_| "idle".into()),
            lan: None,
            fixture: None,
            fixture_scale: 1.0,
            config_path,
            log_dir,
            ddc_binary: None,
            topology: None,
        }
    }
}

/// Owns the in-process hardware worker. Requests keep the same canonical
/// contract as the JSONL server, including validation and structured errors.
pub struct RuntimeHandle {
    tx: Sender<Msg>,
    next_id: AtomicU64,
    stopped: Arc<AtomicBool>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
    completion: Arc<(Mutex<Option<Result<(), String>>>, Condvar)>,
    abandoned: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeLifecycle {
    Running,
    Stopping,
    Finished,
}

impl RuntimeHandle {
    pub fn start(
        options: RuntimeOptions,
        sink: impl Fn(&str, Value) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let completion = Arc::new((Mutex::new(None), Condvar::new()));
        let worker_completion = completion.clone();
        let abandoned = Arc::new(AtomicBool::new(false));
        let worker_abandoned = abandoned.clone();
        let worker_tx = tx.clone();
        let worker_stop = stopped.clone();
        let sink: NotificationSink = Arc::new(sink);
        let worker = thread::Builder::new()
            .name("kvmflow-runtime".into())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_runtime(
                        options,
                        worker_tx,
                        rx,
                        Some(sink.clone()),
                        worker_stop.clone(),
                        worker_abandoned,
                    )
                }))
                .unwrap_or_else(|_| {
                    Err("runtime worker panicked; display restore unconfirmed".into())
                });
                if let Err(error) = &outcome {
                    sink("runtime.error", json!({ "message": error }));
                }
                worker_stop.store(true, Ordering::Release);
                let (result, ready) = &*worker_completion;
                *result.lock().unwrap() = Some(outcome);
                ready.notify_all();
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            tx,
            next_id: AtomicU64::new(1),
            stopped,
            worker: Mutex::new(Some(worker)),
            completion,
            abandoned,
        })
    }

    /// Trusted T2 boundary only. Network authentication and peer/SectionMeta
    /// conflict resolution belong to T2. Never registered in JSONL/UI catalog.
    pub fn apply_remote_mixed(
        &self,
        params: Value,
    ) -> Result<Value, kvmflow_core::protocol::RpcErrorBody> {
        let fail = |message: &str| kvmflow_core::protocol::RpcErrorBody {
            code: E_BACKEND.into(),
            message: message.into(),
        };
        if self.stopped.load(Ordering::Acquire) {
            return Err(fail("Runtime stopped"));
        }
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Msg::RemoteMixed(params, tx))
            .map_err(|_| fail("Runtime stopped"))?;
        let response = rx
            .recv_timeout(Duration::from_secs(120))
            .map_err(|_| fail("Remote application timed out"))?;
        if response.ok {
            Ok(response.result.unwrap_or(Value::Null))
        } else {
            Err(response
                .error
                .unwrap_or_else(|| fail("Remote application failed")))
        }
    }

    pub fn request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, kvmflow_core::protocol::RpcErrorBody> {
        use kvmflow_core::protocol::RpcErrorBody;
        let fail = |message: &str| RpcErrorBody {
            code: E_BACKEND.into(),
            message: message.into(),
        };
        if self.stopped.load(Ordering::Acquire) {
            return Err(fail("后台组件不可用，请重启应用并查看诊断。"));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let line = encode(&kvmflow_core::protocol::RequestLine {
            v: PROTOCOL_VERSION,
            id,
            method: method.into(),
            params,
        });
        let req = match parse_client_line(&line).map_err(|error| fail(&error.to_string()))? {
            Ok(req) => req,
            Err(response) => return Err(response.error.unwrap()),
        };
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Msg::Request(req, tx))
            .map_err(|_| fail("后台组件已停止。"))?;
        // DDC retries can take substantially longer than a normal UI action.
        let response = rx
            .recv_timeout(Duration::from_secs(120))
            .map_err(|_| fail("后台操作超时或已停止，请查看诊断后重试。"))?;
        if response.ok {
            Ok(response.result.unwrap_or(Value::Null))
        } else {
            Err(response.error.unwrap_or_else(|| fail("后台操作失败。")))
        }
    }

    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::Release);
        let _ = self.tx.send(Msg::Shutdown);
    }

    /// Waits for the explicit final restore result. Timeout never discards
    /// the worker or a later result; a caller can wait again.
    pub fn shutdown_and_join(&self) -> Result<(), String> {
        self.shutdown_and_join_timeout(SHUTDOWN_WAIT)
    }

    pub fn shutdown_and_join_timeout(&self, timeout: Duration) -> Result<(), String> {
        self.shutdown();
        let (result, ready) = &*self.completion;
        let (result, _) = ready
            .wait_timeout_while(result.lock().unwrap(), timeout, |r| r.is_none())
            .unwrap();
        let outcome = result
            .clone()
            .ok_or_else(|| "display restore timed out; worker is still running".to_string())?;
        drop(result);
        if let Some(worker) = self.worker.lock().unwrap().take() {
            worker
                .join()
                .map_err(|_| "runtime worker panicked".to_string())?;
        }
        outcome
    }

    /// Restores all detached displays and keeps them that way until
    /// `resume_displays`. Ok means the restore finished; use it before
    /// anything that may end the process without running `shutdown`.
    pub fn release_displays(&self) -> Result<(), String> {
        self.release_displays_timeout(SHUTDOWN_WAIT)
    }

    pub fn release_displays_timeout(&self, timeout: Duration) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Msg::ReleaseDisplays(tx))
            .map_err(|_| "后台组件已停止。".to_string())?;
        rx.recv_timeout(timeout)
            .map_err(|_| "display restore timed out".to_string())?
    }

    pub fn resume_displays(&self) {
        let _ = self.resume_displays_to(None);
    }

    /// Queue resume and the prior transient USB pause state atomically. The
    /// bounded acknowledgement avoids a second 120-second RPC after timeout.
    pub fn resume_displays_with_enabled(&self, enabled: bool) -> Result<(), String> {
        self.resume_displays_to(Some(enabled))
    }

    pub fn is_finished(&self) -> bool {
        self.completion.0.lock().unwrap().is_some()
    }

    pub fn lifecycle(&self) -> RuntimeLifecycle {
        if self.is_finished() {
            RuntimeLifecycle::Finished
        } else if self.stopped.load(Ordering::Acquire) {
            RuntimeLifecycle::Stopping
        } else {
            RuntimeLifecycle::Running
        }
    }

    /// Supervisor-only wait: observes completion without requesting shutdown.
    /// May block indefinitely on a native call; never use on a UI thread.
    pub fn wait_until_finished(&self) -> Result<(), String> {
        let (result, ready) = &*self.completion;
        let result = ready
            .wait_while(result.lock().unwrap(), |r| r.is_none())
            .unwrap();
        result.clone().unwrap()
    }

    /// For a cancelled exit: enqueue even when a previous release is still
    /// running. A timed-out acknowledgement does not remove the queued resume.
    pub fn resume_after_cancel(&self, timeout: Duration) -> Result<(), String> {
        self.resume_displays_to_timeout(None, timeout)
    }

    /// Explicit user override only. Keep persisted recovery records and avoid
    /// starting another recovery or blocking Drop during forced exit/install.
    pub fn abandon_for_exit(&self) {
        self.abandoned.store(true, Ordering::Release);
        self.shutdown();
    }

    fn resume_displays_to(&self, enabled: Option<bool>) -> Result<(), String> {
        self.resume_displays_to_timeout(enabled, Duration::from_secs(5))
    }
    fn resume_displays_to_timeout(
        &self,
        enabled: Option<bool>,
        timeout: Duration,
    ) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Msg::ResumeDisplays(enabled, tx))
            .map_err(|_| "runtime stopped before resume".to_string())?;
        rx.recv_timeout(timeout)
            .map_err(|_| "resume acknowledgement timed out; resume remains queued".to_string())?
    }
}

impl Drop for RuntimeHandle {
    fn drop(&mut self) {
        if self.abandoned.load(Ordering::Acquire) {
            return;
        }
        let _ = self.shutdown_and_join();
    }
}

fn run_runtime(
    options: RuntimeOptions,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    notification: Option<NotificationSink>,
    stopped: Arc<AtomicBool>,
    abandoned: Arc<AtomicBool>,
) -> Result<(), String> {
    let RuntimeOptions {
        mut backend,
        lan,
        scenario,
        fixture,
        fixture_scale,
        config_path,
        log_dir,
        ddc_binary,
        topology,
    } = options;
    #[cfg(not(target_os = "macos"))]
    let _ = &ddc_binary;
    if backend == "auto" {
        backend = if cfg!(target_os = "macos") {
            "macos_m1ddc".into()
        } else {
            "windows_dxva2".into()
        };
    }

    std::fs::create_dir_all(&log_dir).ok();
    let log = SessionLog::open(log_dir.join("sidecar-session.jsonl"));

    let backend_name = backend;
    // Real platforms get the detach adapter unless the caller injected one;
    // the simulated backend keeps the caller's (or the no-op) topology.
    let injected_topology = topology.is_some();
    let mut topology = topology.or_else(|| {
        detach::platform_topology(
            &backend_name,
            config_path.with_file_name(detach::RECORDS_FILE),
            ddc_binary.as_deref(),
        )
    });
    // Recover the OS topology even when DDC backend initialization later
    // fails (for example a missing helper after an interrupted update).
    let startup_restore_error = if !injected_topology {
        topology
            .as_mut()
            .and_then(|adapter| adapter.restore_all().err())
    } else {
        None
    };
    let backend: Box<dyn Backend> = match backend_name.as_str() {
        "simulated" => {
            let mut b = backends::simulated::SimulatedBackend::new(&scenario);
            b.link_topology(topology.as_ref().and_then(|t| t.simulation_link()));
            Box::new(b)
        }
        #[cfg(target_os = "macos")]
        "macos_m1ddc" => match backends::macos::ddc::MacDdcBackend::new(ddc_binary.as_deref()) {
            Ok(b) => Box::new(b),
            Err(e) => return Err(e.to_string()),
        },
        #[cfg(target_os = "windows")]
        "windows_dxva2" => Box::new(backends::windows::ddc::WindowsDdcBackend::new()),
        other => return Err(format!("unknown backend {other}")),
    };

    // USB source
    if backend_name == "simulated" {
        // Scenario/fixture players emit plain device snapshots; forward them
        // into the runtime channel.
        let (stx, srx) = mpsc::channel::<Vec<UsbDevice>>();
        {
            let tx = tx.clone();
            thread::spawn(move || {
                for devices in srx {
                    let _ = tx.send(Msg::Usb(devices));
                }
            });
        }
        match fixture {
            Some(path) => {
                if let Err(e) = backends::simulated::spawn_fixture_replay(&path, fixture_scale, stx)
                {
                    eprintln!("kvmflow-sidecar: {e}");
                }
            }
            None => {
                let _ = backends::simulated::spawn_scenario_player(&scenario, stx);
            }
        }
    } else {
        let poll = 250u64;
        let _ = spawn_usb_source(&backend_name, poll, tx.clone(), stopped.clone());
    }

    let mut rt = Runtime {
        runtime_seq: 0,
        lan_runtime: lan_runtime::LanRuntime::default(),
        backend,
        topology: topology.unwrap_or_else(|| Box::new(topology::NoopDisplayTopology)),
        released_topology: None,
        released_enabled: false,
        mixed_runtime: mixed::MixedRuntime::default(),
        tracker: None,
        fsm: SwitchFsm::new(),
        config: None,
        enabled: true,
        last_multi_device_report: None,
        last_layout_application: None,
        bounce_watch: None,
        usb_snapshot_initialized: false,
        config_path,
        log,
        stdout: std::io::stdout(),
        notification,
        reply: None,
        last_snapshot: Vec::new(),
        learning_baseline: None,
        last_candidates: None,
        wizard_active: false,
        config_recovery: None,
    };

    rt.notify(
        "ready",
        json!({
            "sidecar_version": kvmflow_core::SIDECAR_VERSION,
            "protocol_version": PROTOCOL_VERSION,
            "platform": std::env::consts::OS,
            "backend": rt.backend.name(),
            "simulated": rt.backend.simulated(),
            "pid": std::process::id(),
        }),
    );
    rt.emit_event(
        kinds::SIDECAR_READY,
        Level::Info,
        json!({"backend": rt.backend.name(), "protocol": PROTOCOL_VERSION}),
    );
    if let Some(warning) = rt.topology.take_warning() {
        rt.notify("mixed.warning", json!({"code":"detach_records_unreadable", "severity":"warning", "device_id":null, "fingerprint":null, "error":{"code":"E_CONFIG_IO", "message":warning}}));
    }
    // Restore crash records before load_config can reconcile the saved mode.
    if let Some(error) = startup_restore_error {
        rt.notify("mixed.warning", json!({"code":"restore_failed", "severity":"warning", "device_id":null, "fingerprint":null, "error":{"code":error.code,"message":error.message}, "phase":"startup"}));
    }
    rt.load_config();
    rt.start_lan(lan);

    while !stopped.load(Ordering::Acquire) {
        rt.poll_lan();
        // Idle tick path: drain with timeout so time-based tracker windows advance.
        let msg = match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(m) => m,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Time-based tracker windows (T_stable / T_absent) advance on
                // a quiet bus too - pump before the FSM tick.
                if rt.released_topology.is_none() {
                    rt.pump_tracker();
                    rt.check_bounce_watch();
                    rt.poll_mixed_topology();
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match msg {
            Msg::RemoteMixed(params, reply) => {
                rt.reply = Some(reply);
                if rt.released_topology.is_some() {
                    rt.respond(ResponseLine::err(
                        0,
                        E_BACKEND,
                        "displays released for exit/update; remote update blocked until resume",
                    ));
                } else {
                    rt.handle_mixed_request(0, "mixed.apply_remote", &params);
                }
            }
            Msg::Request(req, reply) => {
                rt.reply = Some(reply);
                if !rt.handle_request(req) {
                    break;
                }
            }
            Msg::Line(line) => {
                match parse_client_line(&line) {
                    Ok(Ok(req)) => {
                        if !rt.handle_request(req) {
                            break;
                        }
                    }
                    Ok(Err(resp)) => rt.respond(resp),
                    Err(e) => {
                        rt.notify("log", json!({ "level": "warn", "msg": format!("dropping malformed line: {e}") }));
                    }
                }
            }
            Msg::Usb(devices) => rt.handle_usb_snapshot(devices),
            Msg::HubProbe(changes) => {
                for (key, outcome) in changes {
                    rt.emit_event(
                        kinds::USB_HUB_PROBE,
                        Level::Info,
                        json!({ "key": key, "outcome": outcome }),
                    );
                }
            }
            Msg::ReleaseDisplays(reply) => {
                let _ = reply.send(rt.release_displays());
            }
            Msg::ResumeDisplays(enabled, reply) => {
                let _ = reply.send(rt.resume_displays(enabled));
            }
            Msg::Shutdown => break,
        }
        // Busy request traffic can starve the idle tick, so the watch is also
        // advanced after every message.
        if rt.released_topology.is_none() {
            rt.check_bounce_watch();
            rt.poll_mixed_topology();
        }
    }

    // Display recovery must not wait for network teardown or peer deadlines.
    let restored = if abandoned.load(Ordering::Acquire) {
        Ok(())
    } else {
        rt.restore_displays("shutdown")
    };
    rt.stop_lan();
    rt.emit_event(kinds::SIDECAR_SHUTDOWN, Level::Info, json!({}));
    stopped.store(true, Ordering::Release);
    restored
}
