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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_fingerprint: Option<String>,
    /// Cross-platform stable display identity, normally the EDID identity.
    pub fingerprint: String,
    #[serde(default)]
    pub label: String,
    /// The VCP 0x60 value that selects this device on this physical monitor.
    pub local_input: u16,
    /// Read-only legacy compatibility, consumed by layout migration.
    /// Routing uses arrival layouts and never reads this flag.
    #[serde(default = "follows", skip_serializing_if = "is_true")]
    pub follow: bool,
}

fn follows() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupDevice {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub profile_rev: u64,
    /// Available (lid-open) built-in screens. Local enumeration; peers via T2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_builtin_screens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrival_layout_id: Option<String>,
    pub device_id: String,
    pub name: String,
    /// Physical upstream port on the USB Switch. The next target is the next
    /// greater port in this group, wrapping at the end.
    pub port_index: u16,
    pub monitors: Vec<MonitorInput>,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

impl MonitorInput {
    pub fn backend_fingerprint(&self) -> &str {
        self.local_fingerprint
            .as_deref()
            .unwrap_or(&self.fingerprint)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SwitchGroupSnapshot {
    /// Per-section merge metadata; T2 owns other sections. Preserve unknown
    /// sections while mixed commands stamp only sync.mixed.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub sync: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mixed: Option<crate::mixed::MixedConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layouts: Vec<crate::layouts::Layout>,
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
    /// Departure writes, including destinations on a third computer. The
    /// arrival path separately corrects only monitors assigned to its host.
    pub writes: Vec<RouteWrite>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupValidationError {
    EmptyGroupId,
    InvalidLayout(String),
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
            Self::InvalidLayout(message) => write!(f, "{message}"),
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
        if self.devices.len() > crate::sync::MAX_PROFILE_REGISTERS {
            return Err(GroupValidationError::InvalidLayout(
                "sync register limit exceeded (64)".into(),
            ));
        }
        if !self.sync.is_null() {
            let sync: crate::sync::GroupSync =
                serde_json::from_value(self.sync.clone()).map_err(|e| {
                    GroupValidationError::InvalidLayout(format!("invalid sync metadata: {e}"))
                })?;
            let mut members = BTreeSet::new();
            if sync
                .members
                .iter()
                .any(|m| m.device_id.is_empty() || !members.insert(&m.device_id))
            {
                return Err(GroupValidationError::InvalidLayout(
                    "invalid or duplicate sync member".into(),
                ));
            }
            let ids: BTreeSet<_> = self
                .devices
                .iter()
                .map(|d| &d.device_id)
                .chain(sync.profile_cache.keys())
                .chain(sync.archived_arrivals.keys())
                .collect();
            if ids.len() > crate::sync::MAX_PROFILE_REGISTERS
                || sync.archived_arrivals.len() > crate::sync::MAX_PROFILE_REGISTERS
            {
                return Err(GroupValidationError::InvalidLayout(
                    "sync register limit exceeded (64)".into(),
                ));
            }
            for (id, p) in &sync.profile_cache {
                let mut monitors = BTreeSet::new();
                let mut local = BTreeSet::new();
                if id.trim().is_empty()
                    || p.monitors.iter().any(|m| {
                        m.fingerprint.trim().is_empty()
                            || m.backend_fingerprint().trim().is_empty()
                            || !monitors.insert(&m.fingerprint)
                            || !local.insert(m.backend_fingerprint())
                    })
                {
                    return Err(GroupValidationError::InvalidLayout(
                        "invalid retained profile".into(),
                    ));
                }
            }
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
            let mut local_monitor_set = BTreeSet::new();
            for monitor in &device.monitors {
                if monitor.backend_fingerprint().trim().is_empty()
                    || !local_monitor_set.insert(monitor.backend_fingerprint())
                {
                    return Err(GroupValidationError::InvalidLayout(format!(
                        "device `{}` has an empty or duplicate local monitor alias",
                        device.device_id
                    )));
                }
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
        self.validate_layouts()?;
        if let Some(mixed) = &self.mixed {
            mixed.validate_shape(self)?;
        }
        Ok(())
    }

    /// Builds the route selected by pressing a cycling USB Switch while the
    /// source device owns the current picture. The source can use `writes`
    /// immediately on departure; arrival corrects only local destinations.
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
        let layout = self
            .arrival_layout(&target.device_id)
            .expect("known device");
        let mut writes: Vec<RouteWrite> = target
            .monitors
            .iter()
            .filter_map(|monitor| {
                if self.mixed.as_ref().is_some_and(|m| {
                    m.enabled
                        && m.assignment
                            .iter()
                            .any(|a| a.fingerprint == monitor.fingerprint)
                }) {
                    return None;
                }
                let destination = self.layout_destination(&layout, &monitor.fingerprint)?;
                if destination.device_id == source.device_id {
                    return None;
                }
                let input = destination
                    .monitors
                    .iter()
                    .find(|m| m.fingerprint == monitor.fingerprint)?;
                Some(RouteWrite {
                    fingerprint: monitor.fingerprint.clone(),
                    value: input.local_input,
                })
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
            local_fingerprint: None,
            fingerprint: fingerprint.into(),
            label: fingerprint.into(),
            local_input: value,
            follow: true,
        }
    }

    fn group() -> SwitchGroupSnapshot {
        SwitchGroupSnapshot {
            sync: serde_json::Value::Null,
            mixed: None,
            layouts: Vec::new(),
            group_id: "desk".into(),
            revision: 12,
            devices: vec![
                GroupDevice {
                    profile_rev: 0,
                    available_builtin_screens: None,
                    arrival_layout_id: None,
                    device_id: "mac-work".into(),
                    name: "工作 Mac".into(),
                    port_index: 1,
                    monitors: vec![monitor("DELL-A", 15), monitor("LG-B", 17)],
                },
                GroupDevice {
                    profile_rev: 0,
                    available_builtin_screens: None,
                    arrival_layout_id: None,
                    device_id: "windows".into(),
                    name: "Windows".into(),
                    port_index: 2,
                    monitors: vec![monitor("DELL-A", 17), monitor("LG-B", 15)],
                },
                GroupDevice {
                    profile_rev: 0,
                    available_builtin_screens: None,
                    arrival_layout_id: None,
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
    fn monitors_that_do_not_follow_the_target_are_left_alone() {
        let mut snapshot = group();
        // Switching to Windows leaves LG-B on the previous computer.
        snapshot.devices[1].monitors[1].follow = false;
        snapshot.migrate_follow_layouts();
        let route = snapshot.route_after_departure("mac-work").unwrap();
        assert_eq!(route.target_device_id, "windows");
        assert_eq!(
            route.writes,
            vec![RouteWrite {
                fingerprint: "DELL-A".into(),
                value: 17
            }]
        );
        // The flag belongs to the target: leaving Windows still moves LG-B.
        let back = snapshot.route_after_departure("windows").unwrap();
        assert_eq!(back.writes.len(), 2);
    }

    #[test]
    fn follow_defaults_to_true_and_is_only_written_when_off() {
        let parsed: MonitorInput =
            serde_json::from_str(r#"{"fingerprint":"DELL-A","local_input":15}"#).unwrap();
        assert!(parsed.follow);
        let written = serde_json::to_value(&parsed).unwrap();
        assert!(written.get("follow").is_none(), "{written}");
        let off = MonitorInput {
            follow: false,
            ..parsed
        };
        assert_eq!(serde_json::to_value(&off).unwrap()["follow"], false);
    }

    #[test]
    fn rejects_unknown_source() {
        assert_eq!(
            group().route_after_departure("unknown"),
            Err(RouteError::UnknownSourceDevice("unknown".into()))
        );
    }
}
