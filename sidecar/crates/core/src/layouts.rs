//! Display layout resolution and manual planning without hardware side effects.
use crate::multidevice::{GroupDevice, GroupValidationError, SwitchGroupSnapshot};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Layout {
    pub id: String,
    pub name: String,
    pub monitors: Vec<LayoutMonitor>,
    #[serde(default, skip_serializing_if = "LayoutSource::is_custom")]
    pub source: LayoutSource,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LayoutSource {
    #[default]
    Custom,
    Builtin {
        device_id: String,
    },
    MigratedFollow {
        device_id: String,
    },
}
impl LayoutSource {
    fn is_custom(&self) -> bool {
        matches!(self, Self::Custom)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LayoutMonitor {
    pub fingerprint: String,
    pub target: LayoutTarget,
}

/// The only string accepted on the wire is `keep`; device IDs are objects.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum LayoutTarget {
    Keep(Keep),
    Device { device_id: String },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Keep {
    #[serde(rename = "keep")]
    Keep,
}
impl LayoutTarget {
    pub fn keep() -> Self {
        Self::Keep(Keep::Keep)
    }
    pub fn device(id: &str) -> Self {
        Self::Device {
            device_id: id.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LayoutUpdate {
    pub fingerprint: String,
    pub missing_monitor: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_device_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectiveLayout {
    #[serde(flatten)]
    pub layout: Layout,
    pub needs_update: Vec<LayoutUpdate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum LayoutAction {
    Push { value: u16 },
    NoChange,
    PullAttempt { value: u16 },
    NeedsDevice { device_id: Option<String> },
    Unavailable,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LayoutPlanMonitor {
    pub fingerprint: String,
    #[serde(flatten)]
    pub action: LayoutAction,
}

impl SwitchGroupSnapshot {
    pub fn validate_layouts(&self) -> Result<(), GroupValidationError> {
        let invalid = |message| GroupValidationError::InvalidLayout(message);
        let mut ids = BTreeSet::new();
        for layout in &self.layouts {
            if layout.id.trim().is_empty()
                || layout.id.starts_with("builtin:")
                || matches!(layout.source, LayoutSource::Builtin { .. })
            {
                return Err(invalid(format!("invalid custom layout id `{}`", layout.id)));
            }
            if !ids.insert(&layout.id) {
                return Err(invalid(format!("duplicate layout id `{}`", layout.id)));
            }
            let mut monitors = BTreeSet::new();
            for monitor in &layout.monitors {
                if !monitors.insert(&monitor.fingerprint) {
                    return Err(invalid(format!(
                        "layout `{}` duplicates monitor `{}`",
                        layout.id, monitor.fingerprint
                    )));
                }
            }
        }
        Ok(())
    }

    /// Legacy follow flags are consumed once, then ignored by all routing.
    pub fn migrate_follow_layouts(&mut self) {
        for device in &mut self.devices {
            if device.arrival_layout_id.is_none() && device.monitors.iter().any(|m| !m.follow) {
                let base = format!("migrated:{}", device.device_id);
                let mut id = base.clone();
                let mut suffix = 1;
                while self.layouts.iter().any(|l| l.id == id) {
                    id = format!("{base}:{suffix}");
                    suffix += 1;
                }
                self.layouts.push(Layout {
                    id: id.clone(),
                    name: String::new(),
                    source: LayoutSource::MigratedFollow {
                        device_id: device.device_id.clone(),
                    },
                    monitors: device
                        .monitors
                        .iter()
                        .map(|m| LayoutMonitor {
                            fingerprint: m.fingerprint.clone(),
                            target: if m.follow {
                                LayoutTarget::device(&device.device_id)
                            } else {
                                LayoutTarget::keep()
                            },
                        })
                        .collect(),
                });
                device.arrival_layout_id = Some(id);
            }
            for monitor in &mut device.monitors {
                monitor.follow = true;
            }
        }
    }

    pub fn layout(&self, id: &str) -> Option<Layout> {
        if let Some(device_id) = id.strip_prefix("builtin:") {
            let device = self.devices.iter().find(|d| d.device_id == device_id)?;
            return Some(Layout {
                id: id.into(),
                name: String::new(),
                source: LayoutSource::Builtin {
                    device_id: device_id.into(),
                },
                monitors: device
                    .monitors
                    .iter()
                    .map(|m| LayoutMonitor {
                        fingerprint: m.fingerprint.clone(),
                        target: if self.mixed.as_ref().is_some_and(|mixed| {
                            mixed.enabled
                                && mixed
                                    .assignment
                                    .iter()
                                    .any(|a| a.fingerprint == m.fingerprint)
                        }) {
                            LayoutTarget::keep()
                        } else {
                            LayoutTarget::device(device_id)
                        },
                    })
                    .collect(),
            });
        }
        self.layouts.iter().find(|l| l.id == id).cloned()
    }

    pub fn arrival_layout(&self, device_id: &str) -> Option<Layout> {
        let device = self.devices.iter().find(|d| d.device_id == device_id)?;
        if self.mixed.is_some() {
            return self.layout(&format!("builtin:{device_id}"));
        }
        device
            .arrival_layout_id
            .as_deref()
            .and_then(|id| self.layout(id))
            .or_else(|| self.layout(&format!("builtin:{device_id}")))
    }

    /// Unknown references and omitted monitors resolve to keep.
    pub fn layout_destination(&self, layout: &Layout, fingerprint: &str) -> Option<&GroupDevice> {
        let entry = layout
            .monitors
            .iter()
            .find(|m| m.fingerprint == fingerprint)?;
        let LayoutTarget::Device { device_id } = &entry.target else {
            return None;
        };
        self.devices.iter().find(|d| {
            d.device_id == *device_id && d.monitors.iter().any(|m| m.fingerprint == fingerprint)
        })
    }

    pub fn effective_layouts(&self) -> Vec<EffectiveLayout> {
        self.devices
            .iter()
            .filter_map(|d| self.layout(&format!("builtin:{}", d.device_id)))
            .chain(self.layouts.iter().cloned())
            .map(|layout| {
                let needs_update = layout
                    .monitors
                    .iter()
                    .filter_map(|m| {
                        let missing_monitor = !self.devices.iter().any(|d| {
                            d.monitors
                                .iter()
                                .any(|known| known.fingerprint == m.fingerprint)
                        });
                        let missing_device_id = match &m.target {
                            LayoutTarget::Device { device_id }
                                if !self.devices.iter().any(|d| d.device_id == *device_id) =>
                            {
                                Some(device_id.clone())
                            }
                            _ => None,
                        };
                        (missing_monitor || missing_device_id.is_some()).then(|| LayoutUpdate {
                            fingerprint: m.fingerprint.clone(),
                            missing_monitor,
                            missing_device_id,
                        })
                    })
                    .collect();
                EffectiveLayout {
                    layout,
                    needs_update,
                }
            })
            .collect()
    }
}

/// A missing map key means not detected; a present None means a failed read.
pub fn plan_apply_layout(
    snapshot: &SwitchGroupSnapshot,
    local_device_id: &str,
    layout_id: &str,
    current: &BTreeMap<String, Option<u16>>,
) -> Result<Vec<LayoutPlanMonitor>, String> {
    snapshot.validate().map_err(|e| e.to_string())?;
    let local = snapshot
        .devices
        .iter()
        .find(|d| d.device_id == local_device_id)
        .ok_or("unknown local device")?;
    let layout = snapshot
        .layout(layout_id)
        .ok_or_else(|| format!("unknown layout `{layout_id}`"))?;
    Ok(local
        .monitors
        .iter()
        .map(|monitor| {
            let action = match current.get(&monitor.fingerprint) {
                None => LayoutAction::Unavailable,
                Some(input) => match snapshot.layout_destination(&layout, &monitor.fingerprint) {
                    None => LayoutAction::NoChange,
                    Some(target) => {
                        let value = target
                            .monitors
                            .iter()
                            .find(|m| m.fingerprint == monitor.fingerprint)
                            .expect("resolved monitor")
                            .local_input;
                        if *input == Some(value) {
                            LayoutAction::NoChange
                        } else if target.device_id == local_device_id {
                            LayoutAction::PullAttempt { value }
                        } else if *input == Some(monitor.local_input) {
                            LayoutAction::Push { value }
                        } else {
                            LayoutAction::NeedsDevice {
                                device_id: input.and_then(|value| {
                                    snapshot
                                        .devices
                                        .iter()
                                        .find(|d| {
                                            d.monitors.iter().any(|m| {
                                                m.fingerprint == monitor.fingerprint
                                                    && m.local_input == value
                                            })
                                        })
                                        .map(|d| d.device_id.clone())
                                }),
                            }
                        }
                    }
                },
            };
            LayoutPlanMonitor {
                fingerprint: monitor.fingerprint.clone(),
                action,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multidevice::MonitorInput;
    use crate::transaction::local_arrival_correction;
    use serde_json::json;

    fn group(count: usize) -> SwitchGroupSnapshot {
        SwitchGroupSnapshot {
            sync: serde_json::Value::Null,
            mixed: None,
            group_id: "desk".into(),
            revision: 1,
            layouts: vec![],
            devices: (0..count)
                .map(|i| GroupDevice {
                    profile_rev: 0,
                    available_builtin_screens: None,
                    arrival_layout_id: None,
                    device_id: format!("d{i}"),
                    name: format!("Computer {i}"),
                    port_index: i as u16 + 1,
                    monitors: (0..2)
                        .map(|j| MonitorInput {
                            local_fingerprint: None,
                            fingerprint: format!("m{j}"),
                            label: String::new(),
                            local_input: 10 * i as u16 + j + 1,
                            follow: true,
                        })
                        .collect(),
                })
                .collect(),
        }
    }
    fn custom(group: &mut SwitchGroupSnapshot, targets: Vec<LayoutTarget>) {
        group.layouts.push(Layout {
            id: "u:mixed".into(),
            name: "Mixed".into(),
            source: LayoutSource::Custom,
            monitors: targets
                .into_iter()
                .enumerate()
                .map(|(i, target)| LayoutMonitor {
                    fingerprint: format!("m{i}"),
                    target,
                })
                .collect(),
        });
    }
    fn plan(group: &SwitchGroupSnapshot, values: &[(&str, Option<u16>)]) -> Vec<LayoutAction> {
        plan_apply_layout(
            group,
            "d0",
            "u:mixed",
            &values.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        )
        .unwrap()
        .into_iter()
        .map(|m| m.action)
        .collect()
    }

    #[test]
    fn migration_preserves_all_two_and_three_device_follow_combinations() {
        for count in [2, 3] {
            for bits in 0..(1 << (count * 2)) {
                let mut snapshot = group(count);
                for (i, d) in snapshot.devices.iter_mut().enumerate() {
                    for (j, m) in d.monitors.iter_mut().enumerate() {
                        m.follow = bits & (1 << (i * 2 + j)) != 0;
                    }
                }
                let old = snapshot.clone();
                snapshot.migrate_follow_layouts();
                snapshot.validate().unwrap();
                for i in 0..count {
                    let mut expected: Vec<_> = old.devices[(i + 1) % count]
                        .monitors
                        .iter()
                        .filter(|m| m.follow)
                        .map(|m| (m.fingerprint.clone(), m.local_input))
                        .collect();
                    expected.sort();
                    let actual: Vec<_> = snapshot
                        .route_after_departure(&format!("d{i}"))
                        .unwrap()
                        .writes
                        .into_iter()
                        .map(|w| (w.fingerprint, w.value))
                        .collect();
                    assert_eq!(actual, expected, "devices={count}, bits={bits}, source={i}");
                }
                assert!(snapshot
                    .devices
                    .iter()
                    .flat_map(|d| &d.monitors)
                    .all(|m| m.follow));
                let once = snapshot.clone();
                snapshot.migrate_follow_layouts();
                assert_eq!(snapshot, once);
                let json = serde_json::to_value(&snapshot).unwrap();
                let mut reloaded: SwitchGroupSnapshot = serde_json::from_value(json).unwrap();
                reloaded.migrate_follow_layouts();
                assert_eq!(reloaded, once);
            }
        }
    }

    #[test]
    fn migration_preserves_explicit_arrival_and_user_layout_ids() {
        let mut snapshot = group(2);
        snapshot.devices[0].arrival_layout_id = Some("missing".into());
        snapshot.devices[0].monitors[0].follow = false;
        snapshot.devices[1].monitors[1].follow = false;
        custom(&mut snapshot, vec![]);
        snapshot.layouts[0].id = "migrated:d1".into();
        snapshot.migrate_follow_layouts();
        assert_eq!(
            snapshot.devices[0].arrival_layout_id.as_deref(),
            Some("missing")
        );
        assert_eq!(
            snapshot.devices[1].arrival_layout_id.as_deref(),
            Some("migrated:d1:1")
        );
        assert_eq!(snapshot.layouts[0].monitors.len(), 0);
        assert_eq!(snapshot.arrival_layout("d0").unwrap().id, "builtin:d0");
        assert_eq!(
            snapshot.layouts[1].source,
            LayoutSource::MigratedFollow {
                device_id: "d1".into()
            }
        );
    }

    #[test]
    fn target_wire_format_is_unambiguous_even_for_a_device_named_keep() {
        assert_eq!(
            serde_json::to_value(LayoutTarget::keep()).unwrap(),
            json!("keep")
        );
        assert_eq!(
            serde_json::to_value(LayoutTarget::device("keep")).unwrap(),
            json!({"device_id":"keep"})
        );
        assert!(serde_json::from_value::<LayoutTarget>(json!("d0")).is_err());
    }

    #[test]
    fn stale_references_are_reported_and_resolve_to_keep() {
        let mut snapshot = group(2);
        custom(
            &mut snapshot,
            vec![LayoutTarget::device("deleted"), LayoutTarget::device("d1")],
        );
        snapshot.layouts[0].monitors[1].fingerprint = "deleted-monitor".into();
        snapshot.validate().unwrap();
        let effective = snapshot.effective_layouts();
        assert_eq!(effective.len(), 3);
        assert_eq!(effective[0].layout.id, "builtin:d0");
        assert_eq!(effective[2].needs_update.len(), 2);
        assert_eq!(
            effective[2].needs_update[0].missing_device_id.as_deref(),
            Some("deleted")
        );
        assert!(effective[2].needs_update[1].missing_monitor);
        assert_eq!(
            plan(&snapshot, &[("m0", Some(1)), ("m1", Some(2))]),
            vec![LayoutAction::NoChange, LayoutAction::NoChange]
        );
    }

    #[test]
    fn validates_layout_ids_and_repeated_entries() {
        let mut snapshot = group(2);
        custom(&mut snapshot, vec![LayoutTarget::keep()]);
        snapshot.layouts.push(snapshot.layouts[0].clone());
        assert!(snapshot.validate().is_err());
        snapshot.layouts.pop();
        snapshot.layouts[0].id = "builtin:d0".into();
        assert!(snapshot.validate().is_err());
        snapshot.layouts[0].id = String::new();
        assert!(snapshot.validate().is_err());
        snapshot.layouts[0].id = "u:ok".into();
        let m = snapshot.layouts[0].monitors[0].clone();
        snapshot.layouts[0].monitors.push(m);
        assert!(snapshot.validate().is_err());
    }

    #[test]
    fn physical_route_skips_source_and_keep_but_writes_third_device() {
        let mut snapshot = group(3);
        custom(
            &mut snapshot,
            vec![LayoutTarget::device("d0"), LayoutTarget::device("d2")],
        );
        snapshot.devices[1].arrival_layout_id = Some("u:mixed".into());
        let route = snapshot.route_after_departure("d0").unwrap();
        assert_eq!(route.writes.len(), 1);
        assert_eq!(route.writes[0].value, 22);
        let tx =
            crate::transaction::SwitchTransaction::begin(&snapshot, "d0", "tx-layout", 1).unwrap();
        assert!(tx
            .target_arrival_correction_plan("d1")
            .unwrap()
            .writes
            .is_empty());
        assert!(local_arrival_correction(&snapshot, "d1")
            .unwrap()
            .writes
            .is_empty());
        snapshot.layouts[0].monitors[0].target = LayoutTarget::device("d1");
        let correction = local_arrival_correction(&snapshot, "d1").unwrap();
        assert_eq!(correction.writes.len(), 1);
        assert_eq!(correction.writes[0].value, 11);
        snapshot.layouts[0].monitors[1].target = LayoutTarget::keep();
        assert_eq!(
            snapshot.route_after_departure("d0").unwrap().writes.len(),
            1
        );
        snapshot.devices[1].arrival_layout_id = Some("gone".into());
        assert_eq!(
            snapshot.route_after_departure("d0").unwrap().writes.len(),
            2
        );
    }

    #[test]
    fn manual_plan_covers_push_no_change_pull_handoff_unknown_and_offline() {
        let mut snapshot = group(3);
        custom(
            &mut snapshot,
            vec![LayoutTarget::device("d1"), LayoutTarget::device("d0")],
        );
        assert_eq!(
            plan(&snapshot, &[("m0", Some(1)), ("m1", Some(12))]),
            vec![
                LayoutAction::Push { value: 11 },
                LayoutAction::PullAttempt { value: 2 }
            ]
        );
        assert_eq!(
            plan(&snapshot, &[("m0", Some(11)), ("m1", Some(2))]),
            vec![LayoutAction::NoChange, LayoutAction::NoChange]
        );
        assert_eq!(
            plan(&snapshot, &[("m0", Some(21)), ("m1", None)]),
            vec![
                LayoutAction::NeedsDevice {
                    device_id: Some("d2".into())
                },
                LayoutAction::PullAttempt { value: 2 }
            ]
        );
        for input in [None, Some(99)] {
            assert_eq!(
                plan(&snapshot, &[("m0", input)])[0],
                LayoutAction::NeedsDevice { device_id: None }
            );
        }
        assert_eq!(
            plan(&snapshot, &[]),
            vec![LayoutAction::Unavailable, LayoutAction::Unavailable]
        );
        snapshot.layouts[0].monitors.clear();
        assert_eq!(
            plan(&snapshot, &[("m0", None), ("m1", Some(42))]),
            vec![LayoutAction::NoChange, LayoutAction::NoChange]
        );
        assert!(plan_apply_layout(&snapshot, "unknown", "u:mixed", &BTreeMap::new()).is_err());
        assert!(plan_apply_layout(&snapshot, "d0", "unknown", &BTreeMap::new()).is_err());
    }

    #[test]
    fn builtins_are_generated_and_empty_fields_remain_backward_compatible() {
        let snapshot = group(2);
        let value = serde_json::to_value(&snapshot).unwrap();
        assert!(value.get("layouts").is_none());
        assert!(value["devices"][0].get("arrival_layout_id").is_none());
        assert_eq!(snapshot.layout("builtin:d0").unwrap().name, "");
        assert!(snapshot.layout("builtin:missing").is_none());
        let current = BTreeMap::from([("m0".into(), Some(1)), ("m1".into(), Some(2))]);
        let plan = plan_apply_layout(&snapshot, "d0", "builtin:d1", &current).unwrap();
        assert!(plan
            .iter()
            .all(|m| matches!(m.action, LayoutAction::Push { .. })));
    }
}
