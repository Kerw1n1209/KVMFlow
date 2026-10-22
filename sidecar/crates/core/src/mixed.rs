//! Mixed-mode decisions. No OS, network or DDC effects live here.
use crate::layouts::LayoutTarget;
use crate::multidevice::{GroupValidationError, SwitchGroupSnapshot};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MixedConfig {
    pub enabled: bool,
    pub revision: u64,
    pub assignment: Vec<Assignment>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Assignment {
    pub fingerprint: String,
    pub device_id: String,
}
impl MixedConfig {
    /// Shape errors reject saves. Missing devices/monitors are retained as
    /// repairable references (unplugging must never delete an assignment).
    pub fn validate_shape(&self, _group: &SwitchGroupSnapshot) -> Result<(), GroupValidationError> {
        let mut seen = BTreeSet::new();
        for a in &self.assignment {
            if a.fingerprint.trim().is_empty()
                || a.device_id.trim().is_empty()
                || !seen.insert(&a.fingerprint)
            {
                return Err(GroupValidationError::InvalidLayout(
                    "invalid or duplicate mixed assignment".into(),
                ));
            }
        }
        Ok(())
    }
}
impl SwitchGroupSnapshot {
    /// Old per-arrival policies cannot all be represented by one fixed map.
    /// Choose the first non-default arrival policy, in physical port order;
    /// otherwise the first legacy non-follow policy, then the first device.
    /// Preserve old layouts/arrival IDs as an archive for lossless roundtrip.
    pub fn migrate_mixed(&mut self) {
        if self.mixed.is_some() {
            return;
        }
        let mut devices: Vec<_> = self.devices.iter().collect();
        devices.sort_by_key(|d| d.port_index);
        let chosen = devices
            .iter()
            .find(|d| {
                d.arrival_layout_id
                    .as_ref()
                    .is_some_and(|id| !id.starts_with("builtin:"))
            })
            .or_else(|| {
                devices
                    .iter()
                    .find(|d| d.monitors.iter().any(|m| !m.follow))
            })
            .or_else(|| devices.first());
        let mut assignment = Vec::new();
        if let Some(device) = chosen {
            // With 3+ devices, legacy keep does not identify a picture owner.
            // Leave it unassigned rather than inventing an owner. The archived
            // layout preserves the original policy for repair in settings.
            let previous = devices
                .iter()
                .position(|d| d.device_id == device.device_id)
                .filter(|_| devices.len() == 2)
                .map(|i| {
                    devices[(i + devices.len() - 1) % devices.len()]
                        .device_id
                        .clone()
                });
            let layout = device
                .arrival_layout_id
                .as_deref()
                .and_then(|id| self.layout(id));
            for monitor in &device.monitors {
                let target = if let Some(layout) = &layout {
                    match layout
                        .monitors
                        .iter()
                        .find(|m| m.fingerprint == monitor.fingerprint)
                        .map(|m| &m.target)
                    {
                        Some(LayoutTarget::Device { device_id }) => Some(device_id.clone()),
                        _ => previous.clone(),
                    }
                } else if monitor.follow {
                    Some(device.device_id.clone())
                } else {
                    previous.clone()
                };
                if let Some(device_id) = target {
                    assignment.push(Assignment {
                        fingerprint: monitor.fingerprint.clone(),
                        device_id,
                    });
                }
            }
        }
        self.migrate_follow_layouts();
        self.mixed = Some(MixedConfig {
            enabled: false,
            revision: 0,
            assignment,
        });
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reason {
    pub code: String,
    pub severity: Severity,
    pub device_id: Option<String>,
    pub fingerprint: Option<String>,
}
impl Reason {
    pub fn warning(code: &str, device: Option<&str>, fingerprint: Option<&str>) -> Self {
        Self {
            code: code.into(),
            severity: Severity::Warning,
            device_id: device.map(str::to_owned),
            fingerprint: fingerprint.map(str::to_owned),
        }
    }
    fn error(code: &str) -> Self {
        Self {
            severity: Severity::Error,
            ..Self::warning(code, None, None)
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Validation {
    pub ok: bool,
    pub reasons: Vec<Reason>,
}

/// `external` is the currently available external screen inventory, including
/// screens this app detached. Saved but physically unplugged screens do not count.
pub fn validate_enable(group: &SwitchGroupSnapshot, external: &BTreeSet<String>) -> Validation {
    let mut reasons = Vec::new();
    let mut counts: BTreeMap<&str, usize> = group
        .devices
        .iter()
        .map(|d| {
            (
                d.device_id.as_str(),
                d.available_builtin_screens.unwrap_or(0) as usize,
            )
        })
        .collect();
    let total = external.len() + counts.values().sum::<usize>();
    let assignment = group
        .mixed
        .as_ref()
        .map(|m| m.assignment.as_slice())
        .unwrap_or(&[]);
    for fp in external {
        match assignment.iter().find(|a| a.fingerprint == *fp) {
            Some(a) if counts.contains_key(a.device_id.as_str()) => {
                *counts.get_mut(a.device_id.as_str()).unwrap() += 1;
                if !group.devices.iter().any(|d| {
                    d.device_id == a.device_id && d.monitors.iter().any(|m| m.fingerprint == *fp)
                }) {
                    reasons.push(Reason {
                        severity: Severity::Error,
                        ..Reason::warning("missing_input", Some(&a.device_id), Some(fp))
                    });
                }
            }
            Some(a) => reasons.push(Reason {
                severity: Severity::Error,
                ..Reason::warning("unknown_device", Some(&a.device_id), Some(fp))
            }),
            None => reasons.push(Reason::warning("unassigned_monitor", None, Some(fp))),
        }
    }
    if total < 2 {
        reasons.push(Reason::error("not_enough_screens"));
    }
    let owners = counts.values().filter(|&&n| n > 0).count();
    if owners < 2 {
        reasons.push(Reason::error("all_to_one_computer"));
    }
    if group.devices.len() < 2 {
        reasons.push(Reason::error("not_enough_computers"));
    }
    for (id, count) in counts {
        if count == 0 {
            reasons.push(Reason::warning("computer_without_screen", Some(id), None));
        }
    }
    Validation {
        ok: !reasons.iter().any(|r| r.severity == Severity::Error),
        reasons,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TopologyDisplay {
    pub fingerprint: String,
    pub builtin: bool,
    /// false means app-detached, not physically unplugged. list must omit
    /// physically absent monitors and include restorable detached monitors.
    pub connected: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingRemote {
    pub id: String,
    /// None: picture owner could not be observed; T2 must resolve it first.
    pub device_id: Option<String>,
    pub fingerprint: String,
    pub target_device_id: String,
}
/// Compatibility helper for a section with no metadata. Runtime plans use
/// pending_remote_id_with_meta to identify the entire synchronized version.
pub fn pending_remote_id(group_id: &str, revision: u64, fingerprint: &str, target: &str) -> String {
    pending_remote_id_with_meta(group_id, revision, 0, "", fingerprint, target)
}
/// Canonical tuple includes the complete section key and avoids delimiter collisions.
pub fn pending_remote_id_with_meta(
    group_id: &str,
    revision: u64,
    updated_at_ms: u64,
    updated_by: &str,
    fingerprint: &str,
    target: &str,
) -> String {
    format!(
        "mixed:{}",
        serde_json::json!([
            group_id,
            revision,
            updated_at_ms,
            updated_by,
            fingerprint,
            target
        ])
    )
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    DdcPush {
        fingerprint: String,
        input: u16,
    },
    Detach {
        fingerprint: String,
    },
    Attach {
        fingerprint: String,
    },
    WaitEnumeration,
    NeedRemotePush {
        #[serde(flatten)]
        request: PendingRemote,
    },
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Plan {
    pub steps: Vec<Step>,
    pub warnings: Vec<Reason>,
}

/// One planner for transitions, startup reconciliation and hotplug/lid changes.
/// Never sends the last connected screen away, even before topology detach.
/// Unknown H must remain None; guessing a USB holder can black out the user.
pub fn plan(
    group: &SwitchGroupSnapshot,
    local: &str,
    holder: Option<&str>,
    current: &BTreeMap<String, Option<String>>,
    displays: &[TopologyDisplay],
) -> Plan {
    plan_with_context(
        group,
        local,
        holder,
        current,
        displays,
        ReconcileContext::Align,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileContext {
    Align,
    DisableTransition,
}

pub fn plan_with_context(
    group: &SwitchGroupSnapshot,
    local: &str,
    holder: Option<&str>,
    current: &BTreeMap<String, Option<String>>,
    displays: &[TopologyDisplay],
    context: ReconcileContext,
) -> Plan {
    let mut result = Plan::default();
    let Some(mixed) = &group.mixed else {
        return result;
    };
    let mut connected = displays.iter().filter(|d| d.connected).count();
    let mut restored = false;
    let mut attached = BTreeSet::new();
    // Attach all required screens before any DDC or detach.
    for d in displays.iter().filter(|d| !d.builtin && !d.connected) {
        let own = mixed
            .assignment
            .iter()
            .find(|a| a.fingerprint == d.fingerprint)
            .map(|a| a.device_id.as_str());
        let needs_local_push = current.get(&d.fingerprint).and_then(|o| o.as_deref())
            == Some(local)
            && own != Some(local);
        if !mixed.enabled || own.is_none() || own == Some(local) || needs_local_push {
            result.steps.push(Step::Attach {
                fingerprint: d.fingerprint.clone(),
            });
            connected += 1;
            attached.insert(d.fingerprint.clone());
            restored = true;
        }
    }
    // A previously safe detach may become the only restorable screen after
    // lid close / unplug. Recover one before doing anything else.
    if connected == 0 {
        if let Some(d) = displays
            .iter()
            .find(|d| !d.builtin && !d.connected && !attached.contains(&d.fingerprint))
        {
            result.steps.push(Step::Attach {
                fingerprint: d.fingerprint.clone(),
            });
            attached.insert(d.fingerprint.clone());
            connected += 1;
            restored = true;
            result.warnings.push(Reason::warning(
                "last_local_screen",
                Some(local),
                Some(&d.fingerprint),
            ));
        }
    }
    if restored {
        result.steps.push(Step::WaitEnumeration);
    }
    let mut fingerprints: BTreeSet<String> = displays
        .iter()
        .filter(|d| !d.builtin)
        .map(|d| d.fingerprint.clone())
        .collect();
    // Include nonlocal screens: the current picture owner can be on any peer.
    fingerprints.extend(current.keys().cloned());
    for fp in fingerprints {
        let display = displays.iter().find(|d| d.fingerprint == fp && !d.builtin);
        let assigned = mixed
            .assignment
            .iter()
            .find(|a| a.fingerprint == fp)
            .map(|a| a.device_id.as_str());
        if !mixed.enabled && context == ReconcileContext::Align {
            continue;
        }
        let target = if mixed.enabled { assigned } else { holder };
        let Some(target) = target else {
            result.warnings.push(Reason::warning(
                if mixed.enabled {
                    "unassigned_monitor"
                } else {
                    "unknown_keyboard_holder"
                },
                None,
                Some(&fp),
            ));
            continue;
        };
        let owner = current.get(&fp).and_then(|o| o.as_deref());
        let should_detach = mixed.enabled
            && target != local
            && display.is_some_and(|d| d.connected || attached.contains(&d.fingerprint));
        if should_detach && connected <= 1 {
            result
                .warnings
                .push(Reason::warning("last_local_screen", Some(local), Some(&fp)));
            continue;
        }
        if owner != Some(target) {
            if owner == Some(local) && display.is_some() {
                match group
                    .devices
                    .iter()
                    .find(|d| d.device_id == target)
                    .and_then(|d| d.monitors.iter().find(|m| m.fingerprint == fp))
                {
                    Some(m) => result.steps.push(Step::DdcPush {
                        fingerprint: fp.clone(),
                        input: m.local_input,
                    }),
                    None => {
                        result.warnings.push(Reason::warning(
                            "missing_input",
                            Some(target),
                            Some(&fp),
                        ));
                        continue;
                    }
                }
            } else {
                result.steps.push(Step::NeedRemotePush {
                    request: PendingRemote {
                        id: pending_remote_id_with_meta(
                            &group.group_id,
                            mixed.revision,
                            group
                                .sync
                                .pointer("/mixed/updated_at_ms")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0),
                            group
                                .sync
                                .pointer("/mixed/updated_by")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or(""),
                            &fp,
                            target,
                        ),
                        device_id: owner.map(str::to_owned),
                        fingerprint: fp.clone(),
                        target_device_id: target.into(),
                    },
                });
            }
        }
        if should_detach {
            result.steps.push(Step::Detach { fingerprint: fp });
            connected -= 1;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConfigDocument, LegacyMigration, MultiDeviceConfig};
    use crate::transaction::local_arrival_correction;
    use serde_json::json;

    fn group(screens: usize, computers: usize) -> SwitchGroupSnapshot {
        serde_json::from_value(json!({"group_id":"desk","revision":1,
            "mixed":{"enabled":false,"revision":3,"assignment":(0..screens).map(|m| json!({"fingerprint":format!("m{m}"),"device_id":format!("d{}",m%computers)})).collect::<Vec<_>>()},
            "devices":(0..computers).map(|i| json!({"device_id":format!("d{i}"),"name":"","port_index":i+1,"monitors":(0..screens).map(|m| json!({"fingerprint":format!("m{m}"),"local_input":i*10+m+1})).collect::<Vec<_>>()})).collect::<Vec<_>>()
        })).unwrap()
    }
    fn external(n: usize) -> BTreeSet<String> {
        (0..n).map(|i| format!("m{i}")).collect()
    }
    fn displays(n: usize) -> Vec<TopologyDisplay> {
        external(n)
            .into_iter()
            .map(|fingerprint| TopologyDisplay {
                fingerprint,
                builtin: false,
                connected: true,
            })
            .collect()
    }
    fn current(n: usize, device: Option<&str>) -> BTreeMap<String, Option<String>> {
        external(n)
            .into_iter()
            .map(|fp| (fp, device.map(str::to_owned)))
            .collect()
    }
    fn has(v: &Validation, code: &str) -> bool {
        v.reasons.iter().any(|r| r.code == code)
    }

    #[test]
    fn e1_e2_e3_builtin_availability_and_all_to_one() {
        let mut g = group(1, 2);
        let v = validate_enable(&g, &external(1));
        assert!(!v.ok);
        assert!(has(&v, "not_enough_screens"));
        g.devices[1].available_builtin_screens = Some(1);
        assert!(validate_enable(&g, &external(1)).ok); // E2
        g.devices[1].available_builtin_screens = Some(0); // lid closed E3
        assert!(!validate_enable(&g, &external(1)).ok);
        let mut g = group(2, 2);
        g.mixed.as_mut().unwrap().assignment[1].device_id = "d0".into();
        let v = validate_enable(&g, &external(2));
        assert!(!v.ok);
        assert!(has(&v, "all_to_one_computer"));
    }
    #[test]
    fn e4_e5_e6_two_or_three_owners_and_warning_only_computer() {
        for (n, c) in [(3, 2), (3, 3), (2, 3)] {
            let v = validate_enable(&group(n, c), &external(n));
            assert!(v.ok, "{v:?}");
            assert_eq!(has(&v, "computer_without_screen"), c > n);
        }
    }
    #[test]
    fn e7_unplug_retains_assignment_e8_new_screen_follows_e9_no_last_detach() {
        let mut g = group(2, 2);
        g.mixed.as_mut().unwrap().enabled = true;
        let before = g.mixed.clone();
        let p = plan(&g, "d0", Some("d1"), &current(1, Some("d0")), &displays(1));
        assert!(p.steps.is_empty());
        assert_eq!(g.mixed, before);
        // Local sole screen assigned away, E9.
        let p = plan(&g, "d1", Some("d0"), &current(1, Some("d1")), &displays(1));
        assert!(p.steps.is_empty());
        assert!(p.warnings.iter().any(|r| r.code == "last_local_screen"));
        for (i, d) in g.devices.iter_mut().enumerate() {
            d.monitors.push(crate::multidevice::MonitorInput {
                local_fingerprint: None,
                fingerprint: "new".into(),
                label: String::new(),
                local_input: 30 + i as u16,
                follow: true,
            });
        }
        let route = g.route_after_departure("d0").unwrap();
        assert_eq!(route.writes.len(), 1);
        assert_eq!(route.writes[0].fingerprint, "new");
        assert_eq!(route.writes[0].value, 31);
        let v = validate_enable(
            &g,
            &BTreeSet::from(["m0".into(), "m1".into(), "new".into()]),
        );
        assert!(v.ok);
        assert!(has(&v, "unassigned_monitor"));
    }
    #[test]
    fn scenarios_1_4_5_6_normal_and_mixed_usb_routes() {
        let mut g = group(2, 2);
        assert_eq!(g.route_after_departure("d0").unwrap().writes.len(), 2);
        g.mixed.as_mut().unwrap().enabled = true;
        for id in ["d0", "d1"] {
            assert!(g.route_after_departure(id).unwrap().writes.is_empty());
            assert!(local_arrival_correction(&g, id).unwrap().writes.is_empty());
        }
        g.mixed.as_mut().unwrap().enabled = false;
        assert_eq!(g.route_after_departure("d1").unwrap().writes.len(), 2);
        assert_eq!(local_arrival_correction(&g, "d0").unwrap().writes.len(), 2);
    }
    #[test]
    fn scenarios_2_3_7_enable_push_before_detach_and_remote_offline() {
        let mut g = group(2, 2);
        g.mixed.as_mut().unwrap().enabled = true;
        let p = plan(&g, "d0", Some("d0"), &current(2, Some("d0")), &displays(2));
        assert_eq!(
            p.steps,
            vec![
                Step::DdcPush {
                    fingerprint: "m1".into(),
                    input: 12
                },
                Step::Detach {
                    fingerprint: "m1".into()
                }
            ]
        );
        let p = plan(&g, "d0", Some("d1"), &current(2, Some("d1")), &displays(2));
        assert!(
            matches!(&p.steps[0],Step::NeedRemotePush {request} if request.device_id.as_deref()==Some("d1") && request.target_device_id=="d0")
        );
        // Assignment editing is data only; planners have no effects or revision mutations.
        assert_eq!(g.mixed.as_ref().unwrap().revision, 3);
    }
    #[test]
    fn scenario_6_close_attach_wait_then_ddc_and_remote_many_computers() {
        let g = group(3, 3);
        let mut d = displays(3);
        d[1].connected = false;
        let p = plan_with_context(
            &g,
            "d0",
            Some("d2"),
            &BTreeMap::from([
                ("m0".into(), Some("d0".into())),
                ("m1".into(), Some("d1".into())),
                ("m2".into(), None),
            ]),
            &d,
            ReconcileContext::DisableTransition,
        );
        assert_eq!(
            p.steps[0],
            Step::Attach {
                fingerprint: "m1".into()
            }
        );
        assert_eq!(p.steps[1], Step::WaitEnumeration);
        assert_eq!(
            p.steps[2],
            Step::DdcPush {
                fingerprint: "m0".into(),
                input: 21
            }
        );
        assert!(
            matches!(&p.steps[3],Step::NeedRemotePush {request} if request.device_id.as_deref()==Some("d1") && request.target_device_id=="d2")
        );
        assert!(
            matches!(&p.steps[4],Step::NeedRemotePush {request} if request.device_id.is_none())
        );
    }
    #[test]
    fn e3_runtime_lid_close_no_push_away_of_last_screen() {
        let mut g = group(1, 2);
        g.mixed.as_mut().unwrap().enabled = true;
        let p = plan(&g, "d1", Some("d1"), &current(1, Some("d1")), &displays(1));
        assert!(p.steps.is_empty());
        assert_eq!(p.warnings[0].code, "last_local_screen");
        let mut d = displays(1);
        d.push(TopologyDisplay {
            fingerprint: "builtin".into(),
            builtin: true,
            connected: true,
        });
        let p = plan(&g, "d1", None, &current(1, Some("d1")), &d);
        assert_eq!(p.steps.len(), 2);
        assert!(p
            .steps
            .iter()
            .all(|s| !matches!(s, Step::Detach {fingerprint} if fingerprint=="builtin")));
    }
    #[test]
    fn startup_reuses_same_plan_and_unknown_holder_only_restores() {
        let mut d = displays(2);
        d[0].connected = false;
        let p = plan(&group(2, 2), "d0", None, &current(2, None), &d);
        assert_eq!(
            p.steps,
            vec![
                Step::Attach {
                    fingerprint: "m0".into()
                },
                Step::WaitEnumeration
            ]
        );
        assert!(p.warnings.is_empty());
    }
    #[test]
    fn missing_device_or_input_and_duplicate_assignments_are_explicit() {
        let mut g = group(2, 2);
        g.mixed.as_mut().unwrap().assignment[1].device_id = "deleted".into();
        assert!(!validate_enable(&g, &external(2)).ok);
        g.mixed.as_mut().unwrap().enabled = true;
        let p = plan(&g, "d0", Some("d0"), &current(2, Some("d0")), &displays(2));
        assert!(p.steps.is_empty());
        assert!(p.warnings.iter().any(|r| r.code == "missing_input"));
        let a = g.mixed.as_ref().unwrap().assignment[0].clone();
        g.mixed.as_mut().unwrap().assignment.push(a);
        assert!(g.validate().is_err());
    }
    #[test]
    fn follow_and_layouts_migration_is_disabled_deterministic_and_lossless() {
        for use_layout in [false, true] {
            let mut g = group(2, 2);
            g.mixed = None;
            g.devices[1].monitors[1].follow = false;
            if use_layout {
                g.migrate_follow_layouts();
                // Also preserve unrelated user layouts.
                let mut l = g.layouts[0].clone();
                l.id = "custom".into();
                g.layouts.push(l);
            }
            g.migrate_mixed();
            let mixed = g.mixed.as_ref().unwrap();
            assert!(!mixed.enabled);
            assert_eq!(
                mixed.assignment,
                vec![
                    Assignment {
                        fingerprint: "m0".into(),
                        device_id: "d1".into()
                    },
                    Assignment {
                        fingerprint: "m1".into(),
                        device_id: "d0".into()
                    }
                ]
            );
            let once = g.clone();
            g.migrate_mixed();
            assert_eq!(g, once);
            let cfg = json!({"schema_version":2,"local_device":{"device_id":"d0","monitors":[{"fingerprint":"m0","local_input":1},{"fingerprint":"m1","local_input":2}]},"switch_group":g,"trigger":{"anchor":{"vid_pid":"1a40:0101"}}});
            let parsed = ConfigDocument::from_value(cfg).unwrap().into_v2();
            let serialized = serde_json::to_string(&parsed).unwrap();
            let reloaded = ConfigDocument::parse_json(&serialized).unwrap().into_v2();
            assert_eq!(parsed, reloaded);
            assert_eq!(reloaded.switch_group.unwrap(), once);
        }
    }
    #[test]
    fn runnable_v1_has_mixed_and_preserves_learned_inputs() {
        let cfg=serde_json::from_value(json!({"schema_version":1,"host_label":"Mac","monitors":[{"edid_id":"m0","here_input":15,"away_input":16,"away_input_confirmed":true,"away_input_source":"manual_entry","here_input_source":"learned_active_read"}],"trigger":{"anchor":{"vid_pid":"1a40:0101"},"members":[{"vid_pid":"3837:303c"}]}})).unwrap();
        let LegacyMigration::Runnable(cfg) = MultiDeviceConfig::migrate_legacy(cfg) else {
            panic!("runnable")
        };
        let g = cfg.switch_group.unwrap();
        assert!(!g.mixed.as_ref().unwrap().enabled);
        assert_eq!(g.devices[0].monitors[0].local_input, 15);
        assert_eq!(g.devices[1].monitors[0].local_input, 16);
    }
    #[test]
    fn scenario_8_pro_forwarding_is_outside_this_core_contract() {
        // Arbitrary holder changes cannot move a fixed monitor. Future LAN
        // keyboard forwarding uses the same invariant as hardware switching.
        let mut g = group(3, 3);
        g.mixed.as_mut().unwrap().enabled = true;
        for id in ["d0", "d1", "d2"] {
            assert!(g.route_after_departure(id).unwrap().writes.is_empty());
        }
    }
}

#[cfg(test)]
mod safety_edges {
    use super::*;
    use serde_json::json;
    #[test]
    fn ambiguous_three_computer_keep_is_retained_in_archive_not_guessed() {
        let mut g: SwitchGroupSnapshot=serde_json::from_value(json!({"group_id":"g","revision":1,"devices":[
            {"device_id":"a","name":"","port_index":1,"monitors":[{"fingerprint":"m","local_input":1}]},
            {"device_id":"b","name":"","port_index":2,"monitors":[{"fingerprint":"m","local_input":2,"follow":false}]},
            {"device_id":"c","name":"","port_index":3,"monitors":[{"fingerprint":"m","local_input":3}]}
        ]})).unwrap();
        g.migrate_mixed();
        assert!(g.mixed.as_ref().unwrap().assignment.is_empty());
        assert!(!g.layouts.is_empty());
        assert_eq!(g.layouts[0].monitors[0].target, LayoutTarget::keep());
        assert!(validate_enable(&g, &BTreeSet::from(["m".into()]))
            .reasons
            .iter()
            .any(|r| r.code == "unassigned_monitor"));
    }
    #[test]
    fn disconnected_local_picture_owner_is_attached_and_enumerated_before_push() {
        let g: SwitchGroupSnapshot=serde_json::from_value(json!({"group_id":"g","revision":1,"mixed":{"enabled":true,"revision":0,"assignment":[{"fingerprint":"m","device_id":"b"}]},"devices":[
            {"device_id":"a","name":"","port_index":1,"monitors":[{"fingerprint":"m","local_input":1}]},
            {"device_id":"b","name":"","port_index":2,"monitors":[{"fingerprint":"m","local_input":2}]}
        ]})).unwrap();
        let d = vec![
            TopologyDisplay {
                fingerprint: "m".into(),
                builtin: false,
                connected: false,
            },
            TopologyDisplay {
                fingerprint: "internal".into(),
                builtin: true,
                connected: true,
            },
        ];
        let p = plan(
            &g,
            "a",
            None,
            &BTreeMap::from([("m".into(), Some("a".into()))]),
            &d,
        );
        assert_eq!(
            p.steps,
            vec![
                Step::Attach {
                    fingerprint: "m".into()
                },
                Step::WaitEnumeration,
                Step::DdcPush {
                    fingerprint: "m".into(),
                    input: 2
                },
                Step::Detach {
                    fingerprint: "m".into()
                }
            ]
        );
    }
}

#[cfg(test)]
mod review_regressions {
    use super::*;
    #[test]
    fn default_alignment_cannot_push_even_with_known_remote_holder() {
        let group: SwitchGroupSnapshot=serde_json::from_value(serde_json::json!({"group_id":"desk","revision":1,"mixed":{"enabled":false,"revision":8,"assignment":[]},"devices":[
            {"device_id":"mac","name":"Mac","port_index":1,"monitors":[{"fingerprint":"m","local_input":15}]},
            {"device_id":"pc","name":"PC","port_index":2,"monitors":[{"fingerprint":"m","local_input":16}]}
        ]})).unwrap();
        let displays = [TopologyDisplay {
            fingerprint: "m".into(),
            builtin: false,
            connected: false,
        }];
        let current = BTreeMap::from([("m".into(), Some("mac".into()))]);
        assert_eq!(
            plan(&group, "mac", Some("pc"), &current, &displays).steps,
            vec![
                Step::Attach {
                    fingerprint: "m".into()
                },
                Step::WaitEnumeration
            ]
        );
        assert_eq!(
            plan_with_context(
                &group,
                "mac",
                Some("pc"),
                &current,
                &displays,
                ReconcileContext::DisableTransition
            )
            .steps,
            vec![
                Step::Attach {
                    fingerprint: "m".into()
                },
                Step::WaitEnumeration,
                Step::DdcPush {
                    fingerprint: "m".into(),
                    input: 16
                }
            ]
        );
    }
    #[test]
    fn remote_work_ids_are_stable_versioned_and_delimiter_safe() {
        let id = pending_remote_id("g:1", 8, "screen:左", "pc");
        assert_eq!(id, pending_remote_id("g:1", 8, "screen:左", "pc"));
        assert_ne!(id, pending_remote_id("g:1", 9, "screen:左", "pc"));
        assert_ne!(id, pending_remote_id("g:1", 8, "screen:左", "mac"));
        assert_ne!(
            pending_remote_id("a:1", 2, "b", "c"),
            pending_remote_id("a", 1, "2:b", "c")
        );
    }
}

#[cfg(test)]
mod section_identity_tests {
    use super::*;
    #[test]
    fn remote_ids_distinguish_every_section_version_component() {
        let id = pending_remote_id_with_meta("desk", 8, 100, "pc", "m", "mac");
        assert_eq!(
            id,
            pending_remote_id_with_meta("desk", 8, 100, "pc", "m", "mac")
        );
        assert_ne!(
            id,
            pending_remote_id_with_meta("desk", 8, 101, "pc", "m", "mac")
        );
        assert_ne!(
            id,
            pending_remote_id_with_meta("desk", 8, 100, "other", "m", "mac")
        );
        assert_ne!(
            id,
            pending_remote_id_with_meta("desk", 9, 100, "pc", "m", "mac")
        );
        assert_eq!(
            pending_remote_id("desk", 8, "m", "mac"),
            pending_remote_id_with_meta("desk", 8, 0, "", "m", "mac")
        );
    }
}
