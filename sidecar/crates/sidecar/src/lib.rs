//! Shared runtime for the Tauri desktop host and the standalone JSONL server.
//! Hardware and state transitions execute on one worker, never the UI thread.

mod backends;
mod config_store;
mod requests;
mod session;
mod switching;
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
    Arc, Mutex,
};
use std::thread;
use std::time::Duration;

enum Msg {
    Line(String),
    Request(kvmflow_core::protocol::RequestLine, Sender<ResponseLine>),
    Usb(Vec<UsbDevice>),
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    HubProbe(Vec<(String, String)>),
    Shutdown,
}

struct Runtime {
    backend: Box<dyn Backend>,
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
    fn notify(&mut self, kind: &str, data: Value) {
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

    fn respond(&mut self, resp: ResponseLine) {
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

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
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
        scenario,
        fixture,
        fixture_scale,
        config_path,
        log_dir,
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
    if let Err(error) = run_runtime(options, tx, rx, None, Arc::new(AtomicBool::new(false))) {
        eprintln!("kvmflow: {error}");
        std::process::exit(3);
    }
}

type NotificationSink = Arc<dyn Fn(&str, Value) + Send + Sync>;

pub struct RuntimeOptions {
    pub backend: String,
    pub scenario: String,
    pub fixture: Option<String>,
    pub fixture_scale: f64,
    pub config_path: PathBuf,
    pub log_dir: PathBuf,
    pub ddc_binary: Option<String>,
}

impl RuntimeOptions {
    pub fn new(config_path: PathBuf, log_dir: PathBuf) -> Self {
        Self {
            backend: std::env::var("KVMFLOW_BACKEND").unwrap_or_else(|_| "auto".into()),
            scenario: std::env::var("KVMFLOW_SCENARIO").unwrap_or_else(|_| "idle".into()),
            fixture: None,
            fixture_scale: 1.0,
            config_path,
            log_dir,
            ddc_binary: None,
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
}

impl RuntimeHandle {
    pub fn start(
        options: RuntimeOptions,
        sink: impl Fn(&str, Value) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_tx = tx.clone();
        let worker_stop = stopped.clone();
        let sink: NotificationSink = Arc::new(sink);
        let worker = thread::Builder::new()
            .name("kvmflow-runtime".into())
            .spawn(move || {
                if let Err(error) = run_runtime(
                    options,
                    worker_tx,
                    rx,
                    Some(sink.clone()),
                    worker_stop.clone(),
                ) {
                    sink("runtime.error", json!({ "message": error }));
                }
                worker_stop.store(true, Ordering::Release);
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            tx,
            next_id: AtomicU64::new(1),
            stopped,
            worker: Mutex::new(Some(worker)),
        })
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

    pub fn shutdown_and_join(&self) {
        self.shutdown();
        let Some(worker) = self.worker.lock().unwrap().take() else {
            return;
        };
        // The worker only observes `stopped` between messages, so a DDC retry
        // loop in flight could otherwise hold up application exit.
        let (done_tx, done_rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = worker.join();
            let _ = done_tx.send(());
        });
        let _ = done_rx.recv_timeout(Duration::from_secs(5));
    }
}

impl Drop for RuntimeHandle {
    fn drop(&mut self) {
        self.shutdown_and_join();
    }
}

fn run_runtime(
    options: RuntimeOptions,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    notification: Option<NotificationSink>,
    stopped: Arc<AtomicBool>,
) -> Result<(), String> {
    let RuntimeOptions {
        mut backend,
        scenario,
        fixture,
        fixture_scale,
        config_path,
        log_dir,
        ddc_binary,
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
    let backend: Box<dyn Backend> = match backend_name.as_str() {
        "simulated" => Box::new(backends::simulated::SimulatedBackend::new(&scenario)),
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
        backend,
        tracker: None,
        fsm: SwitchFsm::new(),
        config: None,
        enabled: true,
        last_multi_device_report: None,
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
    rt.load_config();

    while !stopped.load(Ordering::Acquire) {
        // Idle tick path: drain with timeout so time-based tracker windows advance.
        let msg = match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(m) => m,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Time-based tracker windows (T_stable / T_absent) advance on
                // a quiet bus too - pump before the FSM tick.
                rt.pump_tracker();
                rt.check_bounce_watch();
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match msg {
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
            Msg::Shutdown => break,
        }
        // Busy request traffic can starve the idle tick, so the watch is also
        // advanced after every message.
        rt.check_bounce_watch();
    }

    rt.emit_event(kinds::SIDECAR_SHUTDOWN, Level::Info, json!({}));
    stopped.store(true, Ordering::Release);
    Ok(())
}
