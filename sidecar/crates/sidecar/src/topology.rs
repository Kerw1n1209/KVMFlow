//! T3 adapter boundary. A successful operation means OS topology changed;
//! DDC acceptance is never evidence that a monitor changed its picture.
use kvmflow_core::errors::{CoreError, E_BACKEND};
use kvmflow_core::mixed::TopologyDisplay;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct SimulationTopologyLink {
    pub ddc_failures: Arc<Mutex<std::collections::BTreeMap<String, usize>>>,
    pub displays: Arc<Mutex<Vec<TopologyDisplay>>>,
    pub enumerated: Arc<Mutex<std::collections::BTreeSet<String>>>,
    pub operations: Arc<Mutex<Vec<String>>>,
}
pub trait DisplayTopology: Send {
    /// Consume a persistent-record load warning for the host notification.
    fn take_warning(&mut self) -> Option<String> {
        None
    }
    fn supported(&self) -> bool {
        true
    }
    /// Test seam only; production adapters leave this as None.
    fn simulation_link(&self) -> Option<SimulationTopologyLink> {
        None
    }
    /// Include app-detached, physically present monitors (connected=false).
    /// Exclude unplugged screens and unavailable/closed-lid built-in panels.
    fn list(&mut self) -> Result<Vec<TopologyDisplay>, CoreError>;
    fn detach(&mut self, fingerprint: &str) -> Result<(), CoreError>;
    fn attach(&mut self, fingerprint: &str) -> Result<(), CoreError>;
    /// Bounded wait until restored handles can be used for DDC. T3 must
    /// refresh native DDC handles after this returns (runtime refreshes too).
    fn wait_enumeration(&mut self) -> Result<(), CoreError>;
    /// Idempotent session cleanup, including app-detached screens after restart.
    fn restore_all(&mut self) -> Result<(), CoreError>;
}

/// Production placeholder: never claims a topology change happened.
#[derive(Default)]
pub struct NoopDisplayTopology;
fn unavailable() -> CoreError {
    CoreError::new(E_BACKEND, "display_topology_unavailable")
}
impl DisplayTopology for NoopDisplayTopology {
    fn supported(&self) -> bool {
        false
    }
    fn list(&mut self) -> Result<Vec<TopologyDisplay>, CoreError> {
        Err(unavailable())
    }
    fn detach(&mut self, _: &str) -> Result<(), CoreError> {
        Err(unavailable())
    }
    fn attach(&mut self, _: &str) -> Result<(), CoreError> {
        Err(unavailable())
    }
    fn wait_enumeration(&mut self) -> Result<(), CoreError> {
        Err(unavailable())
    }
    fn restore_all(&mut self) -> Result<(), CoreError> {
        Ok(())
    }
}

/// Explicitly simulated, shareable topology for integration tests.
#[derive(Clone, Default)]
pub struct FakeDisplayTopology {
    pub ddc_failures: Arc<Mutex<std::collections::BTreeMap<String, usize>>>,
    pub enumerated: Arc<Mutex<std::collections::BTreeSet<String>>>,
    pub displays: Arc<Mutex<Vec<TopologyDisplay>>>,
    pub operations: Arc<Mutex<Vec<String>>>,
}
impl FakeDisplayTopology {
    pub fn new(displays: Vec<TopologyDisplay>) -> Self {
        let enumerated = Arc::new(Mutex::new(
            displays
                .iter()
                .filter(|d| d.connected)
                .map(|d| d.fingerprint.clone())
                .collect(),
        ));
        Self {
            enumerated,
            displays: Arc::new(Mutex::new(displays)),
            ..Self::default()
        }
    }
    fn set(&mut self, fp: &str, connected: bool) -> Result<(), CoreError> {
        let mut displays = self.displays.lock().unwrap();
        let d = displays
            .iter_mut()
            .find(|d| d.fingerprint == fp && !d.builtin)
            .ok_or_else(unavailable)?;
        d.connected = connected;
        self.enumerated.lock().unwrap().remove(fp);
        self.operations.lock().unwrap().push(format!(
            "{}:{fp}",
            if connected { "attach" } else { "detach" }
        ));
        Ok(())
    }
}
impl DisplayTopology for FakeDisplayTopology {
    fn simulation_link(&self) -> Option<SimulationTopologyLink> {
        Some(SimulationTopologyLink {
            ddc_failures: self.ddc_failures.clone(),
            displays: self.displays.clone(),
            enumerated: self.enumerated.clone(),
            operations: self.operations.clone(),
        })
    }
    fn list(&mut self) -> Result<Vec<TopologyDisplay>, CoreError> {
        Ok(self.displays.lock().unwrap().clone())
    }
    fn detach(&mut self, fp: &str) -> Result<(), CoreError> {
        self.set(fp, false)
    }
    fn attach(&mut self, fp: &str) -> Result<(), CoreError> {
        self.set(fp, true)
    }
    fn wait_enumeration(&mut self) -> Result<(), CoreError> {
        let enumerated = self
            .displays
            .lock()
            .unwrap()
            .iter()
            .filter(|d| d.connected)
            .map(|d| d.fingerprint.clone())
            .collect();
        *self.enumerated.lock().unwrap() = enumerated;
        self.operations
            .lock()
            .unwrap()
            .push("wait_enumeration".into());
        Ok(())
    }
    fn restore_all(&mut self) -> Result<(), CoreError> {
        for d in self.displays.lock().unwrap().iter_mut() {
            d.connected = true;
        }
        let enumerated = self
            .displays
            .lock()
            .unwrap()
            .iter()
            .map(|d| d.fingerprint.clone())
            .collect();
        *self.enumerated.lock().unwrap() = enumerated;
        self.operations.lock().unwrap().push("restore_all".into());
        Ok(())
    }
}
