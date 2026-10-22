//! The host-facing restore contract: release before an installer may end the
//! process, and a shutdown that reports whether the restore finished.
use kvmflow_core::errors::{CoreError, E_BACKEND};
use kvmflow_core::mixed::TopologyDisplay;
use kvmflow_sidecar::{
    topology::{DisplayTopology, FakeDisplayTopology},
    RuntimeHandle, RuntimeOptions,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn display(fp: &str) -> TopologyDisplay {
    TopologyDisplay {
        fingerprint: fp.into(),
        builtin: false,
        connected: true,
    }
}

fn start(topology: Box<dyn DisplayTopology>) -> (RuntimeHandle, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut options = RuntimeOptions::new(dir.path().join("config.json"), dir.path().join("logs"));
    options.backend = "simulated".into();
    options.topology = Some(topology);
    (RuntimeHandle::start(options, |_, _| {}).unwrap(), dir)
}

#[test]
fn release_restores_before_returning_and_resume_hands_the_adapter_back() {
    let fake = FakeDisplayTopology::new(vec![display("A"), display("B")]);
    let ops = fake.operations.clone();
    let (runtime, _dir) = start(Box::new(fake.clone()));

    assert_ne!(
        runtime
            .apply_remote_mixed(serde_json::json!({}))
            .unwrap_err()
            .code,
        E_BACKEND
    );
    runtime.release_displays().unwrap();
    assert_eq!(*ops.lock().unwrap(), ["restore_all"]);
    assert!(runtime
        .request("app.setEnabled", serde_json::json!({"enabled":true}))
        .is_err());
    assert_eq!(
        runtime
            .apply_remote_mixed(serde_json::json!({}))
            .unwrap_err()
            .code,
        E_BACKEND
    );
    assert_eq!(
        runtime.request("state.get", serde_json::json!({})).unwrap()["enabled"],
        false
    );
    // Released twice is still one restore.
    runtime.release_displays().unwrap();
    assert_eq!(ops.lock().unwrap().len(), 1);

    runtime.resume_displays();
    assert_eq!(
        runtime.request("state.get", serde_json::json!({})).unwrap()["enabled"],
        true
    );
    runtime.shutdown_and_join().unwrap();
    // The final shutdown restore went to the real adapter again.
    assert_eq!(*ops.lock().unwrap(), ["restore_all", "restore_all"]);
}

struct FailingRestore(Arc<Mutex<u32>>);
impl DisplayTopology for FailingRestore {
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
        *self.0.lock().unwrap() += 1;
        Err(CoreError::new(E_BACKEND, "still detached"))
    }
}

#[test]
fn failed_restore_is_reported_and_the_adapter_stays_active() {
    let calls = Arc::new(Mutex::new(0));
    let (runtime, _dir) = start(Box::new(FailingRestore(calls.clone())));
    let error = runtime.release_displays().unwrap_err();
    assert!(error.contains("still detached"), "{error}");
    // Not released: shutdown still tries again.
    assert!(runtime
        .shutdown_and_join()
        .unwrap_err()
        .contains("still detached"));
    assert!(runtime
        .shutdown_and_join()
        .unwrap_err()
        .contains("still detached"));
    assert_eq!(*calls.lock().unwrap(), 2);
}

struct BlockedRestore {
    entered: std::sync::mpsc::Sender<()>,
    proceed: std::sync::mpsc::Receiver<()>,
}
impl DisplayTopology for BlockedRestore {
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

#[test]
fn shutdown_timeout_keeps_the_later_explicit_result() {
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (proceed_tx, proceed_rx) = std::sync::mpsc::channel();
    let (runtime, _dir) = start(Box::new(BlockedRestore {
        entered: entered_tx,
        proceed: proceed_rx,
    }));
    runtime.shutdown();
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let result = runtime.shutdown_and_join_timeout(Duration::from_millis(5));
    // Unblock before asserting so even a failed assertion cannot hang Drop.
    proceed_tx.send(()).unwrap();
    assert!(result.unwrap_err().contains("timed out"));
    runtime.shutdown_and_join().unwrap();
    runtime.shutdown_and_join().unwrap();
}

struct WarningTopology;
impl DisplayTopology for WarningTopology {
    fn take_warning(&mut self) -> Option<String> {
        Some("corrupt detach record".into())
    }
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
        Ok(())
    }
}
#[test]
fn load_warning_reaches_the_host_as_mixed_warning() {
    let dir = tempfile::tempdir().unwrap();
    let mut options = RuntimeOptions::new(dir.path().join("config.json"), dir.path().join("logs"));
    options.backend = "simulated".into();
    options.topology = Some(Box::new(WarningTopology));
    let (tx, rx) = std::sync::mpsc::channel();
    let runtime = RuntimeHandle::start(options, move |kind, data| {
        if kind == "mixed.warning" {
            tx.send(data).unwrap();
        }
    })
    .unwrap();
    let warning = rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(warning["code"], "detach_records_unreadable");
    assert_eq!(warning["severity"], "warning");
    assert!(warning["device_id"].is_null() && warning["fingerprint"].is_null());
    assert_eq!(warning["error"]["code"], "E_CONFIG_IO");
    assert_eq!(warning["error"]["message"], "corrupt detach record");
    runtime.shutdown_and_join().unwrap();
}

#[test]
fn explicit_force_exit_keeps_the_record_without_retrying_restore() {
    let calls = Arc::new(Mutex::new(0));
    let (runtime, dir) = start(Box::new(FailingRestore(calls.clone())));
    let records = dir.path().join("display-detach.json");
    std::fs::write(&records, "unconfirmed recovery record").unwrap();
    assert!(runtime.release_displays().is_err());
    runtime.abandon_for_exit();
    runtime.shutdown_and_join().unwrap();
    assert_eq!(*calls.lock().unwrap(), 1);
    assert_eq!(
        std::fs::read_to_string(records).unwrap(),
        "unconfirmed recovery record"
    );
}
