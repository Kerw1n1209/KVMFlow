//! Exit cancellation and runtime replacement, separated from native dialogs.
use kvmflow_sidecar::{RuntimeHandle, RuntimeLifecycle};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub struct RuntimeSlot {
    inner: Arc<SlotInner>,
}
struct SlotInner {
    state: Mutex<SlotState>,
    ready: Condvar,
    restart: Box<dyn Fn(u64) -> Result<Arc<RuntimeHandle>, String> + Send + Sync>,
}
struct SlotState {
    current: Arc<RuntimeHandle>,
    restarting: bool,
    error: Option<String>,
    generation: u64,
    last_generation: u64,
}
pub const INITIAL_GENERATION: u64 = 1;
// Notifications are consumed as JavaScript numbers. Never wrap or reuse an ID.
const MAX_GENERATION: u64 = (1 << 53) - 1;

pub fn stamp_state(generation: u64, mut data: serde_json::Value) -> serde_json::Value {
    if let Some(state) = data.as_object_mut() {
        state.insert("runtime_generation".into(), generation.into());
    }
    data
}

pub fn notification(generation: u64, kind: &str, data: serde_json::Value) -> serde_json::Value {
    let data = stamp_state(generation, data);
    serde_json::json!({"kind": kind, "data": data, "runtime_generation": generation})
}
impl RuntimeSlot {
    pub fn new(
        current: Arc<RuntimeHandle>,
        restart: impl Fn(u64) -> Result<Arc<RuntimeHandle>, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner: Arc::new(SlotInner {
                state: Mutex::new(SlotState {
                    current,
                    restarting: false,
                    error: None,
                    generation: INITIAL_GENERATION,
                    last_generation: INITIAL_GENERATION,
                }),
                ready: Condvar::new(),
                restart: Box::new(restart),
            }),
        }
    }
    pub fn current(&self) -> Arc<RuntimeHandle> {
        self.snapshot().0
    }
    /// Capture together before dispatching work; late replies retain this ID.
    pub fn snapshot(&self) -> (Arc<RuntimeHandle>, u64) {
        let state = self.inner.state.lock().unwrap();
        (state.current.clone(), state.generation)
    }
    pub fn is_current(&self, generation: u64) -> bool {
        self.inner.state.lock().unwrap().generation == generation
    }
    pub fn resume_after_cancel(&self, timeout: Duration) -> Result<(), String> {
        let runtime = self.current();
        if runtime.lifecycle() == RuntimeLifecycle::Running {
            let resumed = runtime.resume_after_cancel(timeout);
            if runtime.lifecycle() == RuntimeLifecycle::Running {
                return resumed;
            }
        }
        let mut state = self.inner.state.lock().unwrap();
        if !Arc::ptr_eq(&state.current, &runtime) {
            return Ok(());
        }
        if !state.restarting {
            state.restarting = true;
            state.error = None;
            let inner = self.inner.clone();
            let old = runtime.clone();
            // Keep observing a stopping worker even after the caller's timeout.
            // Only this supervisor can replace it; concurrent callers wait on
            // the same condition variable instead of starting more workers.
            std::thread::spawn(move || {
                let _ = old.wait_until_finished();
                let mut state = inner.state.lock().unwrap();
                let next = state
                    .last_generation
                    .checked_add(1)
                    .filter(|n| *n <= MAX_GENERATION);
                let replacement = next
                    .ok_or_else(|| "runtime generation exhausted".to_string())
                    .and_then(|generation| {
                        // Failed startup attempts also consume their ID.
                        state.last_generation = generation;
                        (inner.restart)(generation).map(|runtime| (runtime, generation))
                    });
                match replacement {
                    Ok((replacement, generation)) => {
                        state.current = replacement;
                        state.generation = generation;
                    }
                    Err(error) => state.error = Some(error),
                }
                state.restarting = false;
                inner.ready.notify_all();
            });
        }
        let (state, _) = self
            .inner
            .ready
            .wait_timeout_while(state, timeout, |state| {
                state.restarting && Arc::ptr_eq(&state.current, &runtime)
            })
            .unwrap();
        if !Arc::ptr_eq(&state.current, &runtime) {
            Ok(())
        } else {
            Err(state
                .error
                .clone()
                .unwrap_or_else(|| "runtime restart pending; waiting for stopping worker".into()))
        }
    }
}

pub fn attempt_exit(runtime: &RuntimeHandle, timeout: Duration) -> Result<(), String> {
    // Do not stop the worker before the exit decision: failure must remain
    // cancellable, including a late successful release after the host deadline.
    runtime.release_displays_timeout(timeout)
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExitChoice {
    Retry,
    Continue,
    Force,
}
pub fn exit_choice(
    result: tauri_plugin_dialog::MessageDialogResult,
    retry: &str,
    force: &str,
) -> ExitChoice {
    use tauri_plugin_dialog::MessageDialogResult::*;
    match result {
        Yes => ExitChoice::Retry,
        No => ExitChoice::Force,
        Custom(label) if label == retry => ExitChoice::Retry,
        Custom(label) if label == force => ExitChoice::Force,
        _ => ExitChoice::Continue, // Cancel and closing the dialog are safe.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvmflow_core::{
        errors::{CoreError, E_BACKEND},
        mixed::TopologyDisplay,
    };
    use kvmflow_sidecar::{
        topology::{DisplayTopology, FakeDisplayTopology},
        RuntimeOptions,
    };
    struct Blocked {
        entered: std::sync::mpsc::Sender<()>,
        proceed: std::sync::mpsc::Receiver<()>,
    }
    impl DisplayTopology for Blocked {
        fn list(&mut self) -> Result<Vec<TopologyDisplay>, CoreError> {
            Ok(vec![])
        }
        fn detach(&mut self, _: &str) -> Result<(), CoreError> {
            Ok(())
        }
        fn attach(&mut self, _: &str) -> Result<(), CoreError> {
            Ok(())
        }
        fn wait_enumeration(&mut self) -> Result<(), CoreError> {
            Ok(())
        }
        fn restore_all(&mut self) -> Result<(), CoreError> {
            self.entered.send(()).unwrap();
            self.proceed.recv().unwrap();
            Ok(())
        }
    }
    fn start(topology: Box<dyn DisplayTopology>) -> (Arc<RuntimeHandle>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut options =
            RuntimeOptions::new(dir.path().join("config.json"), dir.path().join("logs"));
        options.backend = "simulated".into();
        options.topology = Some(topology);
        (
            Arc::new(RuntimeHandle::start(options, |_, _| {}).unwrap()),
            dir,
        )
    }
    #[test]
    fn cancelled_exit_queues_resume_after_late_success() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (proceed_tx, proceed_rx) = std::sync::mpsc::channel();
        let (runtime, _dir) = start(Box::new(Blocked {
            entered: entered_tx,
            proceed: proceed_rx,
        }));
        let slot = RuntimeSlot::new(runtime.clone(), |_| {
            panic!("running worker must not restart")
        });
        assert!(attempt_exit(&runtime, Duration::from_millis(1)).is_err());
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let _ = slot.resume_after_cancel(Duration::from_millis(1));
        proceed_tx.send(()).unwrap();
        runtime
            .request("app.setEnabled", serde_json::json!({"enabled":true}))
            .unwrap();
        assert_ne!(
            runtime
                .apply_remote_mixed(serde_json::json!({}))
                .unwrap_err()
                .code,
            E_BACKEND
        );
        runtime.abandon_for_exit();
        runtime.shutdown_and_join().unwrap();
    }
    #[test]
    fn cancelled_install_restarts_after_stopping_worker_finishes() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (proceed_tx, proceed_rx) = std::sync::mpsc::channel();
        let (runtime, _dir) = start(Box::new(Blocked {
            entered: entered_tx,
            proceed: proceed_rx,
        }));
        assert!(attempt_exit(&runtime, Duration::from_millis(1)).is_err());
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(runtime.lifecycle(), RuntimeLifecycle::Running);
        runtime.abandon_for_exit(); // Risky installer starts, then fails immediately.
        assert_eq!(runtime.lifecycle(), RuntimeLifecycle::Stopping);
        assert!(!runtime.is_finished());
        let (restarted_tx, restarted_rx) = std::sync::mpsc::channel();
        let restart_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = restart_count.clone();
        let replacement_dir = Arc::new(tempfile::tempdir().unwrap());
        let dir = replacement_dir.clone();
        let slot = Arc::new(RuntimeSlot::new(runtime, move |_| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut options =
                RuntimeOptions::new(dir.path().join("config.json"), dir.path().join("logs"));
            options.backend = "simulated".into();
            options.topology = Some(Box::new(FakeDisplayTopology::default()));
            let replacement = RuntimeHandle::start(options, |_, _| {}).map(Arc::new)?;
            restarted_tx.send(()).unwrap();
            Ok(replacement)
        }));
        let callers: Vec<_> = (0..4)
            .map(|_| {
                let slot = slot.clone();
                std::thread::spawn(move || slot.resume_after_cancel(Duration::from_millis(1)))
            })
            .collect();
        for caller in callers {
            assert!(caller.join().unwrap().is_err());
        }
        assert_eq!(restart_count.load(std::sync::atomic::Ordering::SeqCst), 0);
        proceed_tx.send(()).unwrap();
        restarted_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        slot.current()
            .request("app.setEnabled", serde_json::json!({"enabled":true}))
            .unwrap();
        assert_eq!(restart_count.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_ne!(
            slot.current()
                .apply_remote_mixed(serde_json::json!({}))
                .unwrap_err()
                .code,
            E_BACKEND
        );
        slot.current().shutdown_and_join().unwrap();
    }
    #[test]
    fn cancelled_exit_replaces_a_worker_that_already_stopped() {
        let (runtime, _dir) = start(Box::new(FakeDisplayTopology::default()));
        runtime.shutdown_and_join().unwrap();
        let replacement_dir = Arc::new(tempfile::tempdir().unwrap());
        let dir = replacement_dir.clone();
        let slot = RuntimeSlot::new(runtime, move |_| {
            let mut options =
                RuntimeOptions::new(dir.path().join("config.json"), dir.path().join("logs"));
            options.backend = "simulated".into();
            options.topology = Some(Box::new(FakeDisplayTopology::default()));
            RuntimeHandle::start(options, |_, _| {}).map(Arc::new)
        });
        slot.resume_after_cancel(Duration::from_secs(2)).unwrap();
        slot.current()
            .request("app.setEnabled", serde_json::json!({"enabled":true}))
            .unwrap();
        slot.current().shutdown_and_join().unwrap();
    }
    #[test]
    fn generations_increase_across_rebuilds_and_late_messages_keep_their_origin() {
        let (runtime, _dir) = start(Box::new(FakeDisplayTopology::default()));
        runtime.shutdown_and_join().unwrap();
        let dir = Arc::new(tempfile::tempdir().unwrap());
        let generations = Arc::new(Mutex::new(Vec::new()));
        let captured = generations.clone();
        let slot = RuntimeSlot::new(runtime, move |generation| {
            captured.lock().unwrap().push(generation);
            let mut options =
                RuntimeOptions::new(dir.path().join("config.json"), dir.path().join("logs"));
            options.backend = "simulated".into();
            options.topology = Some(Box::new(FakeDisplayTopology::default()));
            RuntimeHandle::start(options, |_, _| {}).map(Arc::new)
        });
        let (_, old_generation) = slot.snapshot();
        slot.resume_after_cancel(Duration::from_secs(2)).unwrap();
        let (runtime, generation) = slot.snapshot();
        assert_eq!((old_generation, generation), (1, 2));
        assert!(!slot.is_current(old_generation));
        assert!(slot.is_current(generation));
        let state = runtime.request("state.get", serde_json::json!({})).unwrap();
        let mut cached = crate::runtime_state::Snapshot::default();
        crate::runtime_state::merge_snapshot(&mut cached, &stamp_state(generation, state.clone()));
        let mut original_fields = stamp_state(generation, state.clone());
        original_fields
            .as_object_mut()
            .unwrap()
            .remove("runtime_generation");
        assert_eq!(original_fields, state);
        let late = notification(
            old_generation,
            "state",
            serde_json::json!({"state":"error"}),
        );
        assert_eq!(late["runtime_generation"], 1);
        assert_eq!(late["data"]["runtime_generation"], 1);
        assert!(!crate::runtime_state::merge_snapshot(
            &mut cached,
            &late["data"]
        ));
        assert_eq!(slot.snapshot().1, 2);
        let error = notification(
            old_generation,
            "mixed.warning",
            serde_json::json!({"code":"E_TOPOLOGY_ATTACH", "message":"failed"}),
        );
        assert_eq!(error["runtime_generation"], 1);
        assert_eq!(error["data"]["code"], "E_TOPOLOGY_ATTACH");
        runtime.shutdown_and_join().unwrap();
        slot.resume_after_cancel(Duration::from_secs(2)).unwrap();
        assert_eq!(slot.snapshot().1, 3);
        assert_eq!(*generations.lock().unwrap(), vec![2, 3]);
        slot.current().shutdown_and_join().unwrap();
    }

    #[test]
    fn failed_restart_never_reuses_generation_and_remains_retryable() {
        let (runtime, _dir) = start(Box::new(FakeDisplayTopology::default()));
        runtime.shutdown_and_join().unwrap();
        let dir = Arc::new(tempfile::tempdir().unwrap());
        let slot = RuntimeSlot::new(runtime, move |generation| {
            if generation == 2 {
                return Err("injected startup failure".into());
            }
            let mut options =
                RuntimeOptions::new(dir.path().join("config.json"), dir.path().join("logs"));
            options.backend = "simulated".into();
            options.topology = Some(Box::new(FakeDisplayTopology::default()));
            RuntimeHandle::start(options, |_, _| {}).map(Arc::new)
        });
        assert_eq!(
            slot.resume_after_cancel(Duration::from_secs(2))
                .unwrap_err(),
            "injected startup failure"
        );
        assert_eq!(slot.snapshot().1, 1);
        slot.resume_after_cancel(Duration::from_secs(2)).unwrap();
        assert_eq!(slot.snapshot().1, 3);
        slot.current().shutdown_and_join().unwrap();
        slot.inner.state.lock().unwrap().last_generation = MAX_GENERATION;
        assert_eq!(
            slot.resume_after_cancel(Duration::from_secs(2))
                .unwrap_err(),
            "runtime generation exhausted"
        );
        assert_eq!(slot.snapshot().1, 3);
    }

    #[test]
    fn exit_dialog_distinguishes_retry_continue_force_and_close_in_both_languages() {
        use tauri_plugin_dialog::MessageDialogResult::*;
        for (retry, force, stay) in [
            ("重试", "仍然退出", "继续运行"),
            ("Retry", "Exit anyway", "Keep running"),
        ] {
            assert_eq!(
                exit_choice(Custom(retry.into()), retry, force),
                ExitChoice::Retry
            );
            assert_eq!(
                exit_choice(Custom(force.into()), retry, force),
                ExitChoice::Force
            );
            assert_eq!(
                exit_choice(Custom(stay.into()), retry, force),
                ExitChoice::Continue
            );
            assert_eq!(exit_choice(Cancel, retry, force), ExitChoice::Continue);
        }
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use kvmflow_core::mixed::TopologyDisplay;
    use kvmflow_sidecar::{topology::FakeDisplayTopology, RuntimeOptions};
    use serde_json::{json, Value};
    use std::io::Write;
    fn start(dir: &std::path::Path) -> Arc<RuntimeHandle> {
        let mut options = RuntimeOptions::new(dir.join("config.json"), dir.join("logs"));
        options.backend = "simulated".into();
        options.topology = Some(Box::new(FakeDisplayTopology::new(
            ["SAC-2763-S:0000000000001", "SAC-2466-S:0000000000000"]
                .into_iter()
                .map(|fp| TopologyDisplay {
                    fingerprint: fp.into(),
                    builtin: false,
                    connected: true,
                })
                .collect(),
        )));
        Arc::new(RuntimeHandle::start(options, |_, _| {}).unwrap())
    }
    #[test]
    fn rebuilt_worker_is_accepted_by_native_and_renderer_before_late_old_work() {
        let dir = Arc::new(tempfile::tempdir().unwrap());
        let config: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/runtime-mixed.json")).unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let runtime = start(dir.path());
        let mut old = Value::Null;
        for _ in 0..30 {
            old = runtime.request("state.get", json!({})).unwrap();
        }
        let old = stamp_state(INITIAL_GENERATION, old);
        runtime
            .request("mixed.set", json!({"enabled":true}))
            .unwrap();
        runtime.shutdown_and_join().unwrap();
        let restart_dir = dir.clone();
        let slot = RuntimeSlot::new(runtime, move |_| Ok(start(restart_dir.path())));
        slot.resume_after_cancel(Duration::from_secs(2)).unwrap();
        let (replacement, generation) = slot.snapshot();
        let fresh = replacement.request("state.get", json!({})).unwrap();
        assert!(fresh["runtime_seq"].as_u64().unwrap() < old["runtime_seq"].as_u64().unwrap());
        assert_eq!(fresh["mixed"]["enabled"], true);
        // Exercise the same notification builder used by the desktop host,
        // including mixed.changed rather than only full state notifications.
        let changed = notification(generation, "mixed.changed", fresh.clone());
        let fresh = notification(generation, "state", fresh);
        let error = notification(
            INITIAL_GENERATION,
            "runtime.error",
            json!({"message":"late old worker"}),
        );
        let mut native = crate::runtime_state::Snapshot::default();
        assert!(crate::runtime_state::merge_snapshot(&mut native, &old));
        assert!(crate::runtime_state::merge_snapshot(
            &mut native,
            &changed["data"]
        ));
        assert_eq!(native.value()["mixed"]["enabled"], true);
        assert!(crate::runtime_state::merge_snapshot(
            &mut native,
            &fresh["data"]
        ));
        assert!(!crate::runtime_state::merge_snapshot(&mut native, &old));
        assert!(!native.mark_error(&error["data"]));
        assert_eq!(native.value()["mixed"]["enabled"], true);
        replacement.shutdown_and_join().unwrap();
        let payload =
            json!({"config":config,"old":old,"changed":changed,"fresh":fresh,"error":error});
        let mut child = std::process::Command::new("node")
            .arg(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../tests/helpers/runtime-rebuild.cjs"),
            )
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("node is required for simulated renderer integration");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(serde_json::to_string(&payload).unwrap().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "renderer integration failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
