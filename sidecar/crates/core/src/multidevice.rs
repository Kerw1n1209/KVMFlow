//! Platform-free model for a synchronized KVMFlow switch group.
//!
//! This model deliberately contains no account, HTTP, USB or DDC code. The
//! server provides a revisioned [`SwitchGroupSnapshot`]; the sidecar uses the
//! snapshot to turn a physical USB departure into an unambiguous target
//! device and the DDC values required to select it. Keeping that calculation
//! here guarantees macOS and Windows follow the same physical port order.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MonitorInput {
    /// Cross-platform stable display identity, normally the EDID identity.
    pub fingerprint: String,
    #[serde(default)]
    pub label: String,
    /// The VCP 0x60 value that selects this device on this physical monitor.
    pub local_input: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupDevice {
    pub device_id: String,
    pub name: String,
    /// Physical upstream port on the USB Switch. The next target is the next
    /// greater port in this group, wrapping at the end.
    pub port_index: u16,
    pub monitors: Vec<MonitorInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SwitchGroupSnapshot {
    pub group_id: String,
    /// Monotonically increasing server revision. Clients never apply an older
    /// revision over a locally cached newer snapshot.
    pub revision: u64,
    pub devices: Vec<GroupDevice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteWrite {
    pub fingerprint: String,
    pub value: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SwitchRoute {
    pub group_id: String,
    pub revision: u64,
    pub source_device_id: String,
    pub source_port_index: u16,
    pub target_device_id: String,
    pub target_port_index: u16,
    /// Values written by the source's fast path and repeated by the target's
    /// arrival correction path. Their equality makes retries idempotent.
    pub writes: Vec<RouteWrite>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupValidationError {
    EmptyGroupId,
    TooFewDevices,
    EmptyDeviceId,
    DuplicateDeviceId(String),
    InvalidPort {
        device_id: String,
    },
    DuplicatePort(u16),
    EmptyMonitorFingerprint {
        device_id: String,
    },
    DuplicateMonitor {
        device_id: String,
        fingerprint: String,
    },
    MonitorSetMismatch {
        device_id: String,
    },
    DuplicateInput {
        fingerprint: String,
        value: u16,
    },
}

impl std::fmt::Display for GroupValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyGroupId => write!(f, "group_id is empty"),
            Self::TooFewDevices => write!(f, "switch group requires at least two devices"),
            Self::EmptyDeviceId => write!(f, "device_id is empty"),
            Self::DuplicateDeviceId(id) => write!(f, "duplicate device_id `{id}`"),
            Self::InvalidPort { device_id } => write!(f, "device `{device_id}` has port_index 0"),
            Self::DuplicatePort(port) => write!(f, "duplicate physical USB port {port}"),
            Self::EmptyMonitorFingerprint { device_id } => {
                write!(f, "device `{device_id}` has an empty monitor fingerprint")
            }
            Self::DuplicateMonitor {
                device_id,
                fingerprint,
            } => write!(
                f,
                "device `{device_id}` has duplicate monitor `{fingerprint}`"
            ),
            Self::MonitorSetMismatch { device_id } => write!(
                f,
                "device `{device_id}` does not have the same monitor set as the group"
            ),
            Self::DuplicateInput { fingerprint, value } => write!(
                f,
                "monitor `{fingerprint}` maps more than one device to input {value}"
            ),
        }
    }
}

impl std::error::Error for GroupValidationError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    InvalidGroup(GroupValidationError),
    UnknownSourceDevice(String),
}

impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidGroup(error) => write!(f, "invalid switch group: {error}"),
            Self::UnknownSourceDevice(id) => {
                write!(f, "source device `{id}` is not in this switch group")
            }
        }
    }
}

impl std::error::Error for RouteError {}

impl SwitchGroupSnapshot {
    pub fn validate(&self) -> Result<(), GroupValidationError> {
        if self.group_id.trim().is_empty() {
            return Err(GroupValidationError::EmptyGroupId);
        }
        if self.devices.len() < 2 {
            return Err(GroupValidationError::TooFewDevices);
        }

        let mut device_ids = BTreeSet::new();
        let mut ports = BTreeSet::new();
        let mut expected_monitors: Option<BTreeSet<String>> = None;
        let mut inputs_by_monitor: BTreeMap<String, BTreeSet<u16>> = BTreeMap::new();

        for device in &self.devices {
            if device.device_id.trim().is_empty() {
                return Err(GroupValidationError::EmptyDeviceId);
            }
            if !device_ids.insert(device.device_id.clone()) {
                return Err(GroupValidationError::DuplicateDeviceId(
                    device.device_id.clone(),
                ));
            }
            if device.port_index == 0 {
                return Err(GroupValidationError::InvalidPort {
                    device_id: device.device_id.clone(),
                });
            }
            if !ports.insert(device.port_index) {
                return Err(GroupValidationError::DuplicatePort(device.port_index));
            }

            let mut monitor_set = BTreeSet::new();
            for monitor in &device.monitors {
                if monitor.fingerprint.trim().is_empty() {
                    return Err(GroupValidationError::EmptyMonitorFingerprint {
                        device_id: device.device_id.clone(),
                    });
                }
                if !monitor_set.insert(monitor.fingerprint.clone()) {
                    return Err(GroupValidationError::DuplicateMonitor {
                        device_id: device.device_id.clone(),
                        fingerprint: monitor.fingerprint.clone(),
                    });
                }
                let inputs = inputs_by_monitor
                    .entry(monitor.fingerprint.clone())
                    .or_default();
                if !inputs.insert(monitor.local_input) {
                    return Err(GroupValidationError::DuplicateInput {
                        fingerprint: monitor.fingerprint.clone(),
                        value: monitor.local_input,
                    });
                }
            }

            match &expected_monitors {
                Some(expected) if expected != &monitor_set => {
                    return Err(GroupValidationError::MonitorSetMismatch {
                        device_id: device.device_id.clone(),
                    });
                }
                None => expected_monitors = Some(monitor_set),
                _ => {}
            }
        }
        Ok(())
    }

    /// Builds the route selected by pressing a cycling USB Switch while the
    /// source device owns the current picture. The source can use `writes`
    /// immediately on departure; the target repeats them after USB arrival.
    pub fn route_after_departure(&self, source_device_id: &str) -> Result<SwitchRoute, RouteError> {
        self.validate().map_err(RouteError::InvalidGroup)?;
        let mut by_port: Vec<&GroupDevice> = self.devices.iter().collect();
        by_port.sort_by_key(|device| device.port_index);
        let source_position = by_port
            .iter()
            .position(|device| device.device_id == source_device_id)
            .ok_or_else(|| RouteError::UnknownSourceDevice(source_device_id.to_string()))?;
        let source = by_port[source_position];
        let target = by_port[(source_position + 1) % by_port.len()];
        let mut writes: Vec<RouteWrite> = target
            .monitors
            .iter()
            .map(|monitor| RouteWrite {
                fingerprint: monitor.fingerprint.clone(),
                value: monitor.local_input,
            })
            .collect();
        writes.sort_by(|a, b| a.fingerprint.cmp(&b.fingerprint));
        Ok(SwitchRoute {
            group_id: self.group_id.clone(),
            revision: self.revision,
            source_device_id: source.device_id.clone(),
            source_port_index: source.port_index,
            target_device_id: target.device_id.clone(),
            target_port_index: target.port_index,
            writes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(fingerprint: &str, value: u16) -> MonitorInput {
        MonitorInput {
            fingerprint: fingerprint.into(),
            label: fingerprint.into(),
            local_input: value,
        }
    }

    fn group() -> SwitchGroupSnapshot {
        SwitchGroupSnapshot {
            group_id: "desk".into(),
            revision: 12,
            devices: vec![
                GroupDevice {
                    device_id: "mac-work".into(),
                    name: "工作 Mac".into(),
                    port_index: 1,
                    monitors: vec![monitor("DELL-A", 15), monitor("LG-B", 17)],
                },
                GroupDevice {
                    device_id: "windows".into(),
                    name: "Windows".into(),
                    port_index: 2,
                    monitors: vec![monitor("DELL-A", 17), monitor("LG-B", 15)],
                },
                GroupDevice {
                    device_id: "mac-home".into(),
                    name: "家庭 Mac".into(),
                    port_index: 3,
                    monitors: vec![monitor("DELL-A", 27), monitor("LG-B", 18)],
                },
            ],
        }
    }

    #[test]
    fn next_physical_port_selects_target_and_inputs() {
        let route = group().route_after_departure("windows").unwrap();
        assert_eq!(route.source_port_index, 2);
        assert_eq!(route.target_device_id, "mac-home");
        assert_eq!(route.target_port_index, 3);
        assert_eq!(
            route.writes,
            vec![
                RouteWrite {
                    fingerprint: "DELL-A".into(),
                    value: 27
                },
                RouteWrite {
                    fingerprint: "LG-B".into(),
                    value: 18
                },
            ]
        );
    }

    #[test]
    fn last_physical_port_wraps_to_first() {
        let route = group().route_after_departure("mac-home").unwrap();
        assert_eq!(route.target_device_id, "mac-work");
        assert_eq!(route.target_port_index, 1);
    }

    #[test]
    fn physical_port_order_wins_over_declaration_order() {
        let mut snapshot = group();
        snapshot.devices.reverse();
        let route = snapshot.route_after_departure("mac-work").unwrap();
        assert_eq!(route.target_device_id, "windows");
    }

    #[test]
    fn rejects_incomplete_monitor_mapping() {
        let mut snapshot = group();
        snapshot.devices[1].monitors.pop();
        assert_eq!(
            snapshot.validate(),
            Err(GroupValidationError::MonitorSetMismatch {
                device_id: "windows".into()
            })
        );
    }

    #[test]
    fn rejects_duplicate_input_for_one_monitor() {
        let mut snapshot = group();
        snapshot.devices[1].monitors[0].local_input = 15;
        assert_eq!(
            snapshot.validate(),
            Err(GroupValidationError::DuplicateInput {
                fingerprint: "DELL-A".into(),
                value: 15
            })
        );
    }

    #[test]
    fn rejects_unknown_source() {
        assert_eq!(
            group().route_after_departure("unknown"),
            Err(RouteError::UnknownSourceDevice("unknown".into()))
        );
    }
}
