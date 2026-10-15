//! USB snapshots: the platform source, wizard candidates and the trigger
//! tracker that turns bus state into departures and arrivals.

use super::*;

impl Runtime {
    pub(crate) fn handle_usb_snapshot(&mut self, devices: Vec<UsbDevice>) {
        let first_snapshot = !self.usb_snapshot_initialized;
        self.usb_snapshot_initialized = true;
        let changed = devices != self.last_snapshot;
        self.last_snapshot = devices.clone();
        if !changed {
            return;
        }
        self.emit_event(
            kinds::USB_CHANGED,
            Level::Debug,
            json!({ "count": devices.len(), "keys": devices.iter().map(|d| d.key.clone()).collect::<Vec<_>>() }),
        );

        // Loading a persisted config happens before the platform USB poller
        // delivers its first snapshot. Treat that first observation as the
        // baseline, never as a device arrival: launching KVMFlow while the
        // keyboard is already on this host must not write any monitor input.
        if first_snapshot {
            if self.wizard_active && self.learning_baseline.as_ref().is_none_or(Vec::is_empty) {
                self.learning_baseline = Some(devices.clone());
            }
            if let Some(trigger) = self.active_trigger() {
                let mut tracker =
                    GroupTracker::new(trigger.anchor, trigger.members, trigger.debounce);
                let keys: Vec<DeviceKey> = devices
                    .iter()
                    .map(|device| DeviceKey {
                        vid_pid: device.vid_pid.clone(),
                        serial: device.serial.clone(),
                    })
                    .collect();
                tracker.update(now_ms(), &keys);
                self.tracker = Some(tracker);
            }
            return;
        }

        // Wizard trigger-group candidate detection: devices that were present
        // at wizard start and have now disappeared.
        if self.wizard_active {
            if let Some(baseline) = &self.learning_baseline {
                let gone: Vec<&UsbDevice> = baseline
                    .iter()
                    .filter(|b| !devices.iter().any(|d| d.key == b.key))
                    .collect();
                let staying = baseline.len() - gone.len();
                if !gone.is_empty() {
                    let candidates = json!({
                        "disappeared": gone,
                        "staying_count": staying,
                        "note": "devices that left this host while the wizard listened - confirm to make them the trigger group",
                    });
                    self.last_candidates = Some(candidates.clone());
                    self.emit_event(kinds::WIZARD_CANDIDATES, Level::Info, candidates.clone());
                    self.notify("wizard.candidates", candidates);
                }
            }
        }

        self.pump_tracker();
    }

    /// Feed the current bus state + current time into the debounce tracker
    /// and dispatch resulting FSM actions. Called on every snapshot change
    /// AND on every idle tick: stability/absence windows are time-based, so
    /// a quiet bus must still advance them (KVM-1: the group leaves once and
    /// the bus stays silent while T_absent elapses).
    pub(crate) fn pump_tracker(&mut self) {
        let Some(tracker) = self.tracker.as_mut() else {
            return;
        };
        let keys: Vec<DeviceKey> = self
            .last_snapshot
            .iter()
            .map(|d| DeviceKey {
                vid_pid: d.vid_pid.clone(),
                serial: d.serial.clone(),
            })
            .collect();
        let evs = tracker.update(now_ms(), &keys);
        for ev in evs {
            self.handle_multi_device_tracker_event(&ev);
            let actions = self.fsm.handle(Input::Tracker(ev), now_ms());
            self.run_actions(actions);
        }
    }
}

pub(crate) fn spawn_usb_source(
    backend_kind: &str,
    poll_ms: u64,
    tx: Sender<Msg>,
    stopped: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    let kind = backend_kind.to_string();
    thread::spawn(move || match kind.as_str() {
        "simulated" => {
            // The simulated source is driven by the scenario player
            // (spawned separately); nothing to poll here.
        }
        #[cfg(target_os = "macos")]
        "macos_m1ddc" => {
            let mut last: Option<Vec<UsbDevice>> = None;
            while !stopped.load(Ordering::Acquire) {
                let snap = backends::macos::usb::usb_snapshot();
                if last.as_ref() != Some(&snap) {
                    last = Some(snap.clone());
                    if tx.send(Msg::Usb(snap)).is_err() {
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(poll_ms.max(100)));
            }
        }
        #[cfg(target_os = "windows")]
        "windows_dxva2" => {
            let mut last: Option<Vec<UsbDevice>> = None;
            while !stopped.load(Ordering::Acquire) {
                let snap = backends::windows::usb::usb_snapshot();
                let probes = backends::windows::usb::take_hub_probe_changes();
                if !probes.is_empty() && tx.send(Msg::HubProbe(probes)).is_err() {
                    break;
                }
                if last.as_ref() != Some(&snap) {
                    last = Some(snap.clone());
                    if tx.send(Msg::Usb(snap)).is_err() {
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(poll_ms.max(100)));
            }
        }
        _ => {}
    })
}
