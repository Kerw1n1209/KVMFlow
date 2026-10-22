//! Executes core layout plans on the same worker as physical USB switching.
use super::*;
use kvmflow_core::layouts::{plan_apply_layout, LayoutAction, LayoutTarget};
use kvmflow_core::multidevice::SwitchGroupSnapshot;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum LayoutResult {
    Switched,
    NoChange,
    PullAttempted {
        write_succeeded: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    NeedsDevice {
        device_id: Option<String>,
    },
    Unavailable,
    Failed {
        error: String,
    },
}
#[derive(Debug, Clone, Serialize)]
struct LayoutMonitorResult {
    fingerprint: String,
    target: LayoutTarget,
    #[serde(flatten)]
    result: LayoutResult,
}

impl Runtime {
    pub(crate) fn effective_layouts_value(&self) -> Value {
        let Some(group) = self.config.as_ref().and_then(|c| c.switch_group.as_ref()) else {
            return json!([]);
        };
        // Include an explicit source even for ordinary custom layouts.
        let layouts: Vec<_> = group
            .effective_layouts()
            .into_iter()
            .map(|effective| {
                let mut value = serde_json::to_value(&effective).expect("serializable layout");
                value["source"] =
                    serde_json::to_value(&effective.layout.source).expect("serializable source");
                value
            })
            .collect();
        json!(layouts)
    }

    /// USB arrival still identifies an arrival layout when automatic pulling
    /// is disabled. Observe the source's result without issuing any writes.
    pub(crate) fn observe_arrival_layout(
        &mut self,
        group: &SwitchGroupSnapshot,
        local_device_id: &str,
    ) {
        let layout = group
            .arrival_layout(local_device_id)
            .expect("validated local device");
        let _ = self.backend.refresh_displays();
        let current: BTreeMap<String, Option<u16>> = match self.backend.list_displays() {
            Ok(displays) => displays
                .into_iter()
                .filter(|d| !d.builtin)
                .map(|d| {
                    let input = self
                        .backend
                        .read_input(&d.edid_id)
                        .ok()
                        .map(|read| read.value);
                    (d.edid_id, input)
                })
                .collect(),
            Err(error) => {
                self.emit_event(
                    kinds::BACKEND_ERROR,
                    Level::Warn,
                    json!({"phase":"target_arrival_observation", "error":error}),
                );
                BTreeMap::new()
            }
        };
        let plan = plan_apply_layout(group, local_device_id, &layout.id, &current)
            .expect("validated group and layout");
        let results = plan
            .into_iter()
            .map(|monitor| {
                let result = match monitor.action {
                    LayoutAction::NoChange => LayoutResult::NoChange,
                    LayoutAction::Unavailable => LayoutResult::Unavailable,
                    LayoutAction::NeedsDevice { device_id } => {
                        LayoutResult::NeedsDevice { device_id }
                    }
                    LayoutAction::Push { .. } => LayoutResult::NeedsDevice {
                        device_id: Some(local_device_id.into()),
                    },
                    LayoutAction::PullAttempt { .. } => LayoutResult::Failed {
                        error: "arrival correction is disabled; no pull was attempted".into(),
                    },
                };
                LayoutMonitorResult {
                    target: resolved_target(group, &layout, &monitor.fingerprint),
                    fingerprint: monitor.fingerprint,
                    result,
                }
            })
            .collect();
        self.record_layout_application(&layout.id, "target_arrival_observation", results);
    }

    /// Physical switching already executed its immutable write plan. Report
    /// those outcomes without issuing any additional commands or reads.
    pub(crate) fn record_physical_layout(
        &mut self,
        group: &SwitchGroupSnapshot,
        arrival_device_id: &str,
        local_device_id: &str,
        phase: &'static str,
        outcomes: &[MonitorOutcome],
    ) {
        let layout = group
            .arrival_layout(arrival_device_id)
            .expect("validated device");
        let local = group
            .devices
            .iter()
            .find(|d| d.device_id == local_device_id)
            .expect("validated local device");
        let results = local
            .monitors
            .iter()
            .map(|monitor| {
                let target = resolved_target(group, &layout, &monitor.fingerprint);
                let result = if let Some(outcome) =
                    outcomes.iter().find(|o| o.edid_id == monitor.fingerprint)
                {
                    write_result(outcome, phase == "target_arrival_correction")
                } else if phase == "target_arrival_correction"
                    && group
                        .layout_destination(&layout, &monitor.fingerprint)
                        .is_some_and(|d| d.device_id != local_device_id)
                {
                    LayoutResult::NeedsDevice { device_id: None }
                } else {
                    LayoutResult::NoChange
                };
                LayoutMonitorResult {
                    fingerprint: monitor.fingerprint.clone(),
                    target,
                    result,
                }
            })
            .collect();
        self.record_layout_application(&layout.id, phase, results);
    }

    fn record_layout_application(
        &mut self,
        layout_id: &str,
        reason: &'static str,
        results: Vec<LayoutMonitorResult>,
    ) -> Value {
        for result in &results {
            let level = match &result.result {
                LayoutResult::Failed { .. } => Level::Error,
                LayoutResult::NeedsDevice { .. }
                | LayoutResult::Unavailable
                | LayoutResult::PullAttempted {
                    write_succeeded: false,
                    ..
                } => Level::Warn,
                _ => Level::Info,
            };
            self.emit_event(
                kinds::LAYOUT_MONITOR_RESULT,
                level,
                json!({"layout_id": layout_id, "reason": reason, "monitor": result}),
            );
        }
        let report = json!({"layout_id": layout_id, "applied_at_ms": now_ms(), "reason": reason, "per_monitor": results, "note": REPORT_NOTE});
        self.last_layout_application = Some(report.clone());
        self.emit_event(kinds::LAYOUT_APPLY_REPORT, Level::Info, report.clone());
        self.notify("layout.report", report.clone());
        self.notify("state", self.state_notification());
        report
    }
}

fn write_result(outcome: &MonitorOutcome, pull: bool) -> LayoutResult {
    if pull {
        LayoutResult::PullAttempted {
            write_succeeded: outcome.commanded,
            error: outcome.last_error.clone(),
        }
    } else if outcome.commanded {
        LayoutResult::Switched
    } else {
        LayoutResult::Failed {
            error: outcome
                .last_error
                .clone()
                .unwrap_or_else(|| "DDC write did not return success".into()),
        }
    }
}
fn resolved_target(
    group: &SwitchGroupSnapshot,
    layout: &kvmflow_core::layouts::Layout,
    fingerprint: &str,
) -> LayoutTarget {
    group
        .layout_destination(layout, fingerprint)
        .map(|d| LayoutTarget::device(&d.device_id))
        .unwrap_or_else(LayoutTarget::keep)
}
