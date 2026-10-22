//! Mixed orchestration on the hardware worker. Local settings, mode edits and
//! authenticated remote section application have separate ownership boundaries.
use super::*;
use kvmflow_core::mixed::{self, PendingRemote, Reason, ReconcileContext, Step, TopologyDisplay};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(crate) use kvmflow_core::sync::SectionMeta;
#[derive(Serialize)]
struct Transition {
    phase: &'static str,
    started_ms: u64,
    outcome: &'static str,
    reasons: Vec<Reason>,
}
impl Default for Transition {
    fn default() -> Self {
        Self {
            phase: "idle",
            started_ms: 0,
            outcome: "ok",
            reasons: vec![],
        }
    }
}
#[derive(Default)]
pub(crate) struct MixedRuntime {
    pub(crate) displays: Vec<TopologyDisplay>,
    known: BTreeMap<String, bool>,
    pub(crate) current: BTreeMap<String, Option<String>>,
    commanded: BTreeMap<String, String>,
    pub(crate) pending_remote: Vec<PendingRemote>,
    pub(crate) superseded_remote: Vec<Value>,
    warnings: Vec<Reason>,
    warning_keys: BTreeSet<String>,
    execution: Vec<Value>,
    pub(crate) holder: Option<String>,
    pub(crate) holder_known: bool,
    topology_error: Option<String>,
    next_refresh_ms: u64,
    next_retry_ms: Option<u64>,
    retry_attempt: u32,
    enumeration_pending: bool,
    disable_transition: bool,
    announce_change: bool,
    transition: Transition,
}
fn failure(code: &str, message: impl Into<String>) -> Value {
    json!({"code":code,"message":message.into()})
}
fn core_failure(e: CoreError) -> Value {
    failure(e.code, e.message)
}
fn mixed_meta(group: &kvmflow_core::multidevice::SwitchGroupSnapshot) -> SectionMeta {
    serde_json::from_value(group.sync.get("mixed").cloned().unwrap_or(Value::Null))
        .unwrap_or_default()
}
fn set_meta(group: &mut kvmflow_core::multidevice::SwitchGroupSnapshot, meta: &SectionMeta) {
    if !group.sync.is_object() {
        group.sync = json!({});
    }
    group.sync["mixed"] = serde_json::to_value(meta).unwrap();
}
impl Runtime {
    pub(crate) fn refresh_mixed_inventory(&mut self) {
        self.mixed_runtime.next_refresh_ms = now_ms() + 2_000;
        let mut fresh = false;
        match self.topology.list() {
            Ok(displays) => {
                self.mixed_runtime.displays = displays;
                self.mixed_runtime.topology_error = None;
                fresh = true;
            }
            Err(e) => {
                self.mixed_runtime.topology_error = Some(e.to_string());
                // Fallback is for labels/validation only. It never authorizes
                // a destructive action; the executor must query live topology.
                if !self.topology.supported() {
                    if let Ok(displays) = self.backend.list_displays() {
                        self.mixed_runtime.displays = displays
                            .into_iter()
                            .map(|d| TopologyDisplay {
                                fingerprint: d.edid_id,
                                builtin: d.builtin,
                                connected: true,
                            })
                            .collect();
                        fresh = true;
                    }
                }
            }
        }
        if fresh {
            for d in &self.mixed_runtime.displays {
                self.mixed_runtime
                    .known
                    .insert(d.fingerprint.clone(), d.builtin);
            }
            let present: BTreeSet<_> = self
                .mixed_runtime
                .displays
                .iter()
                .map(|d| d.fingerprint.clone())
                .collect();
            self.mixed_runtime
                .current
                .retain(|fp, _| present.contains(fp));
            self.mixed_runtime
                .commanded
                .retain(|fp, _| present.contains(fp));
            if let Some(cfg) = &mut self.config {
                if let Some(local) = cfg.switch_group.as_mut().and_then(|g| {
                    g.devices
                        .iter_mut()
                        .find(|d| d.device_id == cfg.local_device.device_id)
                }) {
                    local.available_builtin_screens = Some(
                        self.mixed_runtime
                            .displays
                            .iter()
                            .filter(|d| d.builtin && d.connected)
                            .count() as u32,
                    );
                }
            }
        }
    }
    fn mixed_external(&self) -> BTreeSet<String> {
        self.mixed_runtime
            .displays
            .iter()
            .filter(|d| !d.builtin)
            .map(|d| d.fingerprint.clone())
            .collect()
    }
    fn mixed_validation(
        &self,
        group: &kvmflow_core::multidevice::SwitchGroupSnapshot,
    ) -> mixed::Validation {
        let mut v = mixed::validate_enable(group, &self.mixed_external());
        if !self.topology.supported() {
            v.reasons
                .push(Reason::warning("topology_unavailable", None, None));
        }
        v
    }
    pub(crate) fn mixed_state(&self) -> Value {
        let Some(cfg) = &self.config else {
            return Value::Null;
        };
        let Some(group) = &cfg.switch_group else {
            return Value::Null;
        };
        let Some(m) = &group.mixed else {
            return Value::Null;
        };
        // Order/default numbering comes from the same local monitor array as
        // settings. Do not use fingerprint sort order to name monitors.
        let mut order: Vec<String> = cfg
            .local_device
            .monitors
            .iter()
            .map(|m| m.fingerprint.clone())
            .collect();
        for fp in m
            .assignment
            .iter()
            .map(|a| &a.fingerprint)
            .chain(self.mixed_runtime.known.keys())
        {
            if !order.contains(fp) {
                order.push(fp.clone());
            }
        }
        let destinations:Vec<_>=order.iter().enumerate().map(|(index,fp)|{
            let d=self.mixed_runtime.displays.iter().find(|d|d.fingerprint==*fp);
            let builtin=d.map(|d|d.builtin).unwrap_or_else(||self.mixed_runtime.known.get(fp).copied().unwrap_or(false));
            let assigned=m.assignment.iter().find(|a|a.fingerprint==*fp);
            let label=cfg.local_device.monitors.iter().find(|m|m.fingerprint==*fp).map(|m|m.label.as_str()).filter(|s|!s.trim().is_empty())
                .or_else(||group.devices.iter().flat_map(|d|&d.monitors).find(|m|m.fingerprint==*fp&&!m.label.trim().is_empty()).map(|m|m.label.as_str()));
            let current=if builtin&&d.is_some() {Some(cfg.local_device.device_id.clone())}else{self.mixed_runtime.current.get(fp).cloned().flatten()};
            let evidence=if d.is_none(){"unavailable"}else if builtin{"local_topology"}else if current.is_some(){"input_observation_only"}else if self.mixed_runtime.commanded.contains_key(fp){"commanded_unconfirmed"}else{"assignment_only"};
            json!({"fingerprint":fp,"label":label.unwrap_or(""),"display_name":label.map(str::to_owned).unwrap_or_else(||format!("显示器 {}",index+1)),"default_display_number":index+1,
                "builtin":builtin,"available":d.is_some(),"connected":d.is_some_and(|d|d.connected),
                "availability":if d.is_none(){"unavailable"}else if d.is_some_and(|d|!d.connected){"detached"}else{"available"},
                "unavailable_reason":if d.is_none(){Some(if builtin{"builtin_closed_or_unavailable"}else{"not_in_local_inventory"})}else{None},
                "target_device_id":if builtin{Some(cfg.local_device.device_id.as_str())}else if m.enabled{assigned.map(|a|a.device_id.as_str()).or(self.mixed_runtime.holder.as_deref())}else{self.mixed_runtime.holder.as_deref()},
                "fixed":builtin||(m.enabled&&assigned.is_some()),"current_device_id":current,"evidence":evidence,
                "commanded_device_id":self.mixed_runtime.commanded.get(fp)})
        }).collect();
        json!({"enabled":m.enabled,"revision":m.revision,"section_meta":mixed_meta(group),"assignment":m.assignment,"validation":self.mixed_validation(group),
            "keyboard_holder_device_id":self.mixed_runtime.holder,"holder_confidence":if self.mixed_runtime.holder_known{"known"}else{"inferred"},
            "destinations":destinations,"detached":self.mixed_runtime.displays.iter().filter(|d|!d.connected).map(|d|&d.fingerprint).collect::<Vec<_>>(),
            "pending_remote":self.mixed_runtime.pending_remote,"superseded_remote":self.mixed_runtime.superseded_remote,
            "warnings":self.mixed_runtime.warnings,"execution":self.mixed_runtime.execution,"transition":self.mixed_runtime.transition,
            "retry":{"attempt":self.mixed_runtime.retry_attempt,"next_retry_ms":self.mixed_runtime.next_retry_ms},
            "topology_error":self.mixed_runtime.topology_error,"simulated":self.backend.simulated(),"note":REPORT_NOTE})
    }
    fn observe_mixed_inputs(&mut self) {
        let Some(group) = self.config.as_ref().and_then(|c| c.switch_group.clone()) else {
            return;
        };
        for d in self
            .mixed_runtime
            .displays
            .clone()
            .iter()
            .filter(|d| !d.builtin)
        {
            let owner = if d.connected {
                self.backend
                    .read_input_with_timeout(&d.fingerprint, Duration::from_millis(600))
                    .ok()
                    .and_then(|r| {
                        group
                            .devices
                            .iter()
                            .find(|device| {
                                device.monitors.iter().any(|m| {
                                    m.fingerprint == d.fingerprint && m.local_input == r.value
                                })
                            })
                            .map(|d| d.device_id.clone())
                    })
            } else {
                self.mixed_runtime
                    .current
                    .get(&d.fingerprint)
                    .cloned()
                    .flatten()
            };
            if owner.is_some() {
                self.mixed_runtime.commanded.remove(&d.fingerprint);
            }
            self.mixed_runtime
                .current
                .insert(d.fingerprint.clone(), owner);
        }
    }
    fn begin_mixed_transition(&mut self, phase: &'static str) {
        self.mixed_runtime.transition = Transition {
            phase,
            started_ms: now_ms(),
            outcome: "partial",
            reasons: vec![],
        };
        self.notify("state", self.state_notification());
    }
    fn reconcile_context(&self) -> ReconcileContext {
        if self.mixed_runtime.disable_transition {
            ReconcileContext::DisableTransition
        } else {
            ReconcileContext::Align
        }
    }
    fn retire_pending(&mut self, next: &[PendingRemote]) {
        for old in &self.mixed_runtime.pending_remote {
            if !next.iter().any(|p| p.id == old.id) {
                let mut retired = serde_json::to_value(old).unwrap();
                retired["status"] = json!("superseded");
                self.mixed_runtime.superseded_remote.push(retired);
            }
        }
        // Keep the published active/history views disjoint, including a
        // semantically unchanged task rediscovered by a later observation.
        self.mixed_runtime
            .superseded_remote
            .retain(|old| !next.iter().any(|active| old["id"] == active.id));
        let len = self.mixed_runtime.superseded_remote.len();
        if len > 32 {
            self.mixed_runtime.superseded_remote.drain(..len - 32);
        }
    }
    pub(crate) fn reconcile_mixed(&mut self) {
        if self.mixed_runtime.transition.phase == "idle" {
            self.begin_mixed_transition("restoring");
        }
        self.refresh_mixed_inventory();
        self.observe_mixed_inputs();
        let Some(cfg) = self.config.clone() else {
            return;
        };
        let Some(group) = &cfg.switch_group else {
            return;
        };
        let holder = if self.mixed_runtime.holder_known {
            self.mixed_runtime.holder.clone()
        } else {
            None
        };
        let mut plan = mixed::plan_with_context(
            group,
            &cfg.local_device.device_id,
            holder.as_deref(),
            &self.mixed_runtime.current,
            &self.mixed_runtime.displays,
            self.reconcile_context(),
        );
        if self.mixed_runtime.enumeration_pending
            && !plan
                .steps
                .iter()
                .any(|s| matches!(s, Step::WaitEnumeration))
        {
            plan.steps.insert(0, Step::WaitEnumeration);
        }
        if self.mixed_runtime.disable_transition && !self.mixed_runtime.holder_known {
            plan.warnings
                .push(Reason::warning("holder_inferred", None, None));
        }
        if !self.topology.supported() {
            plan.warnings
                .push(Reason::warning("topology_unavailable", None, None));
        }
        self.bounce_watch = None;
        self.mixed_runtime.warnings = plan.warnings;
        self.mixed_runtime.execution.clear();
        let mut pending = Vec::new();
        let mut failed_ddc = BTreeSet::new();
        let mut topology_failed = false;
        let mut steps: VecDeque<_> = plan.steps.into();
        let mut replanned = false;
        while let Some(step) = steps.pop_front() {
            let mut error = None;
            match &step {
                Step::Attach { fingerprint } => {
                    self.mixed_runtime.enumeration_pending = true;
                    if let Err(e) = self.topology.attach(fingerprint) {
                        error = Some(core_failure(e));
                        topology_failed = true;
                    }
                }
                Step::WaitEnumeration => {
                    if let Err(e) = self
                        .topology
                        .wait_enumeration()
                        .and_then(|()| self.backend.refresh_displays())
                    {
                        error = Some(core_failure(e));
                        topology_failed = true;
                    } else if !topology_failed && !replanned {
                        self.mixed_runtime.enumeration_pending = false;
                        replanned = true;
                        self.refresh_mixed_inventory();
                        self.observe_mixed_inputs();
                        let refreshed = mixed::plan_with_context(
                            group,
                            &cfg.local_device.device_id,
                            holder.as_deref(),
                            &self.mixed_runtime.current,
                            &self.mixed_runtime.displays,
                            self.reconcile_context(),
                        );
                        if refreshed
                            .steps
                            .iter()
                            .any(|s| matches!(s, Step::Attach { .. } | Step::WaitEnumeration))
                        {
                            topology_failed = true;
                            error = Some(failure(
                                "E_TOPOLOGY_ENUMERATION",
                                "Restored displays are not enumerated",
                            ));
                        } else {
                            steps = refreshed.steps.into();
                            for w in refreshed.warnings {
                                if !self.mixed_runtime.warnings.contains(&w) {
                                    self.mixed_runtime.warnings.push(w);
                                }
                            }
                        }
                    }
                }
                Step::DdcPush { fingerprint, input } => {
                    if topology_failed {
                        error = Some(failure(
                            "E_MIXED_PREREQUISITE",
                            "Restore or enumeration failed",
                        ));
                    } else {
                        // Fail closed even if enumeration succeeded during planning.
                        // In ordinary mode only an explicit close transition gets here.
                        match self.topology.list() {
                            Err(e) => error = Some(core_failure(e)),
                            Ok(d)
                                if group.mixed.as_ref().is_some_and(|m| m.enabled)
                                    && d.iter().filter(|d| d.connected).count() <= 1 =>
                            {
                                error = Some(failure(
                                    "last_local_screen",
                                    "Refusing to send away the last local screen",
                                ));
                            }
                            Ok(d)
                                if !d
                                    .iter()
                                    .any(|d| d.fingerprint == *fingerprint && d.connected) =>
                            {
                                error = Some(failure(
                                    "E_TOPOLOGY_NOT_CONNECTED",
                                    "Display is not connected",
                                ))
                            }
                            Ok(_) => {
                                let out = self.write_with_retry(
                                    fingerprint,
                                    *input,
                                    &self.active_retry_policy(),
                                    false,
                                );
                                self.emit_event(
                                    kinds::DDC_SET,
                                    if out.commanded {
                                        Level::Info
                                    } else {
                                        Level::Warn
                                    },
                                    json!({"reason":"mixed","outcome":out,"note":REPORT_NOTE}),
                                );
                                if !out.commanded {
                                    let message =
                                        out.last_error.unwrap_or_else(|| "DDC push failed".into());
                                    let code = message
                                        .split_once(": ")
                                        .map(|(c, _)| c)
                                        .filter(|c| c.starts_with("E_"))
                                        .unwrap_or("E_DDC_FAILED");
                                    error = Some(failure(code, message.clone()));
                                } else if let Some(target) = group.devices.iter().find(|d| {
                                    d.monitors.iter().any(|m| {
                                        m.fingerprint == *fingerprint && m.local_input == *input
                                    })
                                }) {
                                    self.mixed_runtime
                                        .commanded
                                        .insert(fingerprint.clone(), target.device_id.clone());
                                }
                                self.mixed_runtime.current.insert(fingerprint.clone(), None);
                            }
                        }
                    }
                    if error.is_some() {
                        failed_ddc.insert(fingerprint.clone());
                    }
                }
                Step::Detach { fingerprint } => {
                    if failed_ddc.contains(fingerprint) || topology_failed {
                        error = Some(failure(
                            "E_MIXED_PREREQUISITE",
                            "Previous required step failed",
                        ));
                    } else {
                        match self.topology.list() {
                            Err(e) => error = Some(core_failure(e)),
                            Ok(d) if d.iter().filter(|d| d.connected).count() <= 1 => {
                                error = Some(failure(
                                    "last_local_screen",
                                    "Refusing to detach the last local screen",
                                ))
                            }
                            Ok(_) => {
                                if let Err(e) = self.topology.detach(fingerprint) {
                                    error = Some(core_failure(e));
                                }
                            }
                        }
                    }
                }
                Step::NeedRemotePush { request } => pending.push(request.clone()),
            }
            if let Some(e) = &error {
                let fp = match &step {
                    Step::DdcPush { fingerprint, .. }
                    | Step::Detach { fingerprint }
                    | Step::Attach { fingerprint } => Some(fingerprint.as_str()),
                    _ => None,
                };
                self.mixed_runtime.warnings.push(Reason::warning(
                    e["code"].as_str().unwrap(),
                    Some(&cfg.local_device.device_id),
                    fp,
                ));
            }
            self.mixed_runtime
                .execution
                .push(json!({"step":step,"ok":error.is_none(),"error":error}));
        }
        self.retire_pending(&pending);
        if !pending.is_empty() {
            self.mixed_runtime
                .warnings
                .push(Reason::warning("remote_push_pending", None, None));
        }
        self.mixed_runtime.pending_remote = pending;
        self.refresh_mixed_inventory();
        let failed = self
            .mixed_runtime
            .execution
            .iter()
            .filter(|e| e["ok"] == false)
            .count();
        let restore_needed = group.mixed.as_ref().is_some_and(|m| !m.enabled)
            && self
                .mixed_runtime
                .displays
                .iter()
                .any(|d| !d.builtin && !d.connected);
        if failed > 0 || restore_needed || self.mixed_runtime.enumeration_pending {
            let delay = (500u64.saturating_mul(1u64 << self.mixed_runtime.retry_attempt.min(6)))
                .min(30_000);
            self.mixed_runtime.retry_attempt = self.mixed_runtime.retry_attempt.saturating_add(1);
            self.mixed_runtime.next_retry_ms = Some(now_ms() + delay);
        } else {
            self.mixed_runtime.retry_attempt = 0;
            self.mixed_runtime.next_retry_ms = None;
            if self.mixed_runtime.pending_remote.is_empty() {
                self.mixed_runtime.disable_transition = false;
            }
        }
        let active = self
            .mixed_runtime
            .execution
            .iter()
            .filter(|e| {
                !matches!(
                    e["step"]["step"].as_str(),
                    Some("wait_enumeration" | "need_remote_push")
                )
            })
            .count();
        self.mixed_runtime.transition.phase = "idle";
        self.mixed_runtime.transition.outcome = if failed > 0 && failed >= active.max(1) {
            "failed"
        } else if failed > 0
            || !self.mixed_runtime.pending_remote.is_empty()
            || self.mixed_runtime.warnings.iter().any(|r| {
                matches!(
                    r.code.as_str(),
                    "last_local_screen" | "holder_inferred" | "unknown_keyboard_holder"
                )
            })
        {
            "partial"
        } else {
            "ok"
        };
        self.mixed_runtime.transition.reasons = self.mixed_runtime.warnings.clone();
        // Emergency restore plus last-screen guard may report the same reason
        // in one plan. Deduplicate within this attempt as well as across polls.
        let mut seen = BTreeSet::new();
        self.mixed_runtime
            .warnings
            .retain(|r| seen.insert(serde_json::to_string(r).unwrap()));
        self.mixed_runtime.transition.reasons = self.mixed_runtime.warnings.clone();
        let keys: BTreeSet<_> = self
            .mixed_runtime
            .warnings
            .iter()
            .map(|r| serde_json::to_string(r).unwrap())
            .collect();
        for r in self.mixed_runtime.warnings.clone() {
            if !self
                .mixed_runtime
                .warning_keys
                .contains(&serde_json::to_string(&r).unwrap())
            {
                self.notify("mixed.warning", serde_json::to_value(r).unwrap());
            }
        }
        self.mixed_runtime.warning_keys = keys;
        if self.mixed_runtime.announce_change {
            self.notify("mixed.changed",json!({"mixed":self.mixed_state(),"switch_group":self.config.as_ref().and_then(|c|c.switch_group.as_ref())}));
            self.mixed_runtime.announce_change = false;
        }
        if !self.mixed_runtime.pending_remote.is_empty()
            || !self.mixed_runtime.superseded_remote.is_empty()
        {
            self.notify_pending(group);
        }
        self.notify("state", self.state_notification());
    }
    fn notify_pending(&mut self, group: &kvmflow_core::multidevice::SwitchGroupSnapshot) {
        self.notify("mixed.pending_remote",json!({"group_id":group.group_id,"revision":group.mixed.as_ref().map(|m|m.revision),"section_meta":mixed_meta(group),"enabled":group.mixed.as_ref().is_some_and(|m|m.enabled),"local_device_id":self.config.as_ref().map(|c|&c.local_device.device_id),"keyboard_holder_device_id":self.mixed_runtime.holder,"holder_confidence":if self.mixed_runtime.holder_known{"known"}else{"inferred"},"pending_remote":self.mixed_runtime.pending_remote,"superseded_remote":self.mixed_runtime.superseded_remote}));
    }
    pub(crate) fn poll_mixed_topology(&mut self) {
        let now = now_ms();
        if self.config.is_none() {
            return;
        }
        let retry = self.mixed_runtime.next_retry_ms.is_some_and(|t| now >= t);
        if now < self.mixed_runtime.next_refresh_ms && !retry {
            return;
        }
        let before = self.mixed_runtime.displays.clone();
        self.refresh_mixed_inventory();
        let restore_needed = self
            .config
            .as_ref()
            .and_then(|c| c.switch_group.as_ref())
            .and_then(|g| g.mixed.as_ref())
            .is_some_and(|m| !m.enabled)
            && self
                .mixed_runtime
                .displays
                .iter()
                .any(|d| !d.builtin && !d.connected);
        if before != self.mixed_runtime.displays
            || retry
            || ((restore_needed
                || self.mixed_runtime.enumeration_pending
                || self
                    .mixed_runtime
                    .execution
                    .iter()
                    .any(|e| e["ok"] == false))
                && self.mixed_runtime.next_retry_ms.is_none())
        {
            self.reconcile_mixed();
        }
    }
    pub(crate) fn observe_mixed_holder_baseline(&mut self) {
        if !self
            .config
            .as_ref()
            .and_then(|c| c.switch_group.as_ref())
            .is_some_and(|g| {
                g.devices
                    .iter()
                    .any(|d| Some(d.device_id.as_str()) == self.mixed_runtime.holder.as_deref())
            })
        {
            self.mixed_runtime.holder = None;
            self.mixed_runtime.holder_known = false;
        }
        if self.tracker.as_ref().is_some_and(|t| t.group_present()) {
            self.mixed_runtime.holder = self
                .config
                .as_ref()
                .map(|c| c.local_device.device_id.clone());
            self.mixed_runtime.holder_known = true;
        }
    }
    pub(crate) fn invalidate_mixed_observation(&mut self, fp: &str) {
        self.mixed_runtime.current.insert(fp.into(), None);
        self.mixed_runtime.commanded.remove(fp);
    }
    pub(crate) fn prepare_mixed_config(
        &mut self,
        mut cfg: MultiDeviceConfig,
    ) -> Result<MultiDeviceConfig, CoreError> {
        self.refresh_mixed_inventory();
        if let Some(current) = self.config.as_ref().and_then(|c| c.switch_group.as_ref()) {
            let incoming = cfg.switch_group.as_mut().ok_or_else(|| {
                CoreError::new(
                    E_CONFIG_INVALID,
                    "config.set cannot remove an existing switch group",
                )
            })?;
            if incoming.group_id != current.group_id {
                return Err(CoreError::new(
                    E_CONFIG_INVALID,
                    "config.set cannot replace an existing switch group",
                ));
            }
            incoming.mixed = current.mixed.clone();
            set_meta(incoming, &mixed_meta(current));
        }
        if let Some(group) = &mut cfg.switch_group {
            if let Some(local) = group
                .devices
                .iter_mut()
                .find(|d| d.device_id == cfg.local_device.device_id)
            {
                local.available_builtin_screens = self
                    .config
                    .as_ref()
                    .and_then(|c| c.switch_group.as_ref())
                    .and_then(|g| {
                        g.devices
                            .iter()
                            .find(|d| d.device_id == cfg.local_device.device_id)
                    })
                    .and_then(|d| d.available_builtin_screens)
                    .or(Some(
                        self.mixed_runtime
                            .displays
                            .iter()
                            .filter(|d| d.builtin && d.connected)
                            .count() as u32,
                    ));
            }
            if group.mixed.as_ref().is_some_and(|m| {
                m.assignment.iter().any(|a| {
                    self.mixed_runtime
                        .displays
                        .iter()
                        .any(|d| d.builtin && d.fingerprint == a.fingerprint)
                })
            }) {
                return Err(CoreError::new(
                    E_CONFIG_INVALID,
                    "builtin_monitor_in_assignment",
                ));
            }
            let was_enabled = self
                .config
                .as_ref()
                .and_then(|c| c.switch_group.as_ref())
                .and_then(|g| g.mixed.as_ref())
                .is_some_and(|m| m.enabled);
            if !was_enabled && group.mixed.as_ref().is_some_and(|m| m.enabled) {
                let validation = mixed::validate_enable(group, &self.mixed_external());
                if !validation.ok {
                    return Err(CoreError::new(
                        E_CONFIG_INVALID,
                        serde_json::to_string(&validation).unwrap(),
                    ));
                }
            }
        }
        Ok(cfg)
    }

    pub(crate) fn mixed_tracker_event(&mut self, event: &TrackerEvent) {
        let Some(cfg) = self.config.clone() else {
            return;
        };
        let previous_holder = self.mixed_runtime.holder.clone();
        let handoff = matches!(
            event,
            TrackerEvent::GroupLeft { .. } | TrackerEvent::GroupArrived { .. }
        );
        match event {
            TrackerEvent::GroupArrived { .. } | TrackerEvent::GroupStable { .. } => {
                self.mixed_runtime.holder = Some(cfg.local_device.device_id.clone());
                self.mixed_runtime.holder_known = true;
            }
            TrackerEvent::GroupLeft { .. } => {
                self.mixed_runtime.holder = cfg
                    .switch_group
                    .as_ref()
                    .and_then(|g| g.route_after_departure(&cfg.local_device.device_id).ok())
                    .map(|r| r.target_device_id);
                self.mixed_runtime.holder_known = cfg
                    .switch_group
                    .as_ref()
                    .is_some_and(|g| g.devices.len() == 2);
            }
            _ => return,
        }
        // Initial GroupStable confirms baseline presence, not a handoff.
        // Preserve pending close work if H did not change.
        if (handoff || previous_holder != self.mixed_runtime.holder)
            && cfg
                .switch_group
                .as_ref()
                .and_then(|g| g.mixed.as_ref())
                .is_some_and(|m| !m.enabled)
        {
            self.mixed_runtime.disable_transition = false;
            self.retire_pending(&[]);
            self.mixed_runtime.pending_remote.clear();
            if let Some(g) = &cfg.switch_group {
                if !self.mixed_runtime.superseded_remote.is_empty() {
                    self.notify_pending(g);
                }
            }
        }
        self.notify("state", self.state_notification());
    }
    pub(crate) fn handle_mixed_request(&mut self, id: u64, method: &str, params: &Value) {
        let remote = method == "mixed.apply_remote";
        self.refresh_mixed_inventory();
        let result = (|| -> Result<(MultiDeviceConfig, bool), CoreError> {
            let mut cfg = self
                .config
                .clone()
                .ok_or_else(|| CoreError::new(E_CONFIG_INVALID, "No configuration"))?;
            let group = cfg
                .switch_group
                .as_mut()
                .ok_or_else(|| CoreError::new(E_CONFIG_INVALID, "No switch group"))?;
            group.migrate_mixed();
            let old = group.mixed.clone().unwrap();
            let old_meta = mixed_meta(group);
            if remote {
                if params.get("group_id").and_then(Value::as_str) != Some(group.group_id.as_str()) {
                    return Err(CoreError::new(E_CONFIG_INVALID, "Remote group_id mismatch"));
                }
                let revision = params
                    .get("revision")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| CoreError::new(E_CONFIG_INVALID, "revision must be u64"))?;
                let meta = match params.get("meta") {
                    Some(v) => serde_json::from_value(v.clone())
                        .map_err(|_| CoreError::new(E_CONFIG_INVALID, "Invalid SectionMeta"))?,
                    None => SectionMeta::default(),
                };
                if (revision, &meta) <= (old.revision, &old_meta) {
                    return Ok((cfg, false));
                }
                group.mixed =
                    Some(mixed::MixedConfig {
                        enabled: params.get("enabled").and_then(Value::as_bool).ok_or_else(
                            || CoreError::new(E_CONFIG_INVALID, "enabled must be boolean"),
                        )?,
                        revision,
                        assignment: serde_json::from_value(
                            params.get("assignment").cloned().unwrap_or(Value::Null),
                        )
                        .map_err(|e| CoreError::new(E_CONFIG_INVALID, e.to_string()))?,
                    });
                if let Some(h) = params
                    .get("keyboard_holder_device_id")
                    .filter(|v| !v.is_null())
                {
                    if !h
                        .as_str()
                        .is_some_and(|id| group.devices.iter().any(|d| d.device_id == id))
                    {
                        return Err(CoreError::new(E_CONFIG_INVALID, "Unknown keyboard holder"));
                    }
                }
                if params
                    .get("holder_confidence")
                    .is_some_and(|v| v != "known" && v != "inferred")
                {
                    return Err(CoreError::new(
                        E_CONFIG_INVALID,
                        "Invalid holder_confidence",
                    ));
                }
                set_meta(group, &meta);
            } else {
                let m = group.mixed.as_mut().unwrap();
                if method == "mixed.set" {
                    let enabled =
                        params
                            .get("enabled")
                            .and_then(Value::as_bool)
                            .ok_or_else(|| {
                                CoreError::new(E_CONFIG_INVALID, "enabled must be boolean")
                            })?;
                    if enabled == m.enabled {
                        return Ok((cfg, false));
                    }
                    m.enabled = enabled;
                } else {
                    m.assignment = serde_json::from_value(
                        params.get("assignment").cloned().unwrap_or(Value::Null),
                    )
                    .map_err(|e| CoreError::new(E_CONFIG_INVALID, e.to_string()))?;
                }
                m.revision = m
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| CoreError::new(E_CONFIG_INVALID, "mixed revision overflow"))?;
                let timestamp = now_ms().max(old_meta.updated_at_ms.saturating_add(1));
                set_meta(
                    group,
                    &SectionMeta {
                        updated_at_ms: timestamp,
                        updated_by: cfg.local_device.device_id.clone(),
                    },
                );
            }
            if group
                .mixed
                .as_ref()
                .unwrap()
                .assignment
                .iter()
                .any(|a| self.mixed_runtime.known.get(&a.fingerprint) == Some(&true))
            {
                return Err(CoreError::new(
                    E_CONFIG_INVALID,
                    "builtin_monitor_in_assignment",
                ));
            }
            cfg.validate()?;
            if !remote
                && cfg
                    .switch_group
                    .as_ref()
                    .and_then(|g| g.mixed.as_ref())
                    .is_some_and(|m| m.enabled)
            {
                let validation = self.mixed_validation(cfg.switch_group.as_ref().unwrap());
                if !validation.ok {
                    return Err(CoreError::new(
                        E_CONFIG_INVALID,
                        serde_json::to_string(&validation).unwrap(),
                    ));
                }
            }
            Ok((cfg, true))
        })();
        match result {
            Err(e) => self.respond(ResponseLine::err(id, e.code, e.message)),
            Ok((_, false)) => self.respond(ResponseLine::ok(
                id,
                json!({"saved":false,"applied":false,"mixed":self.mixed_state()}),
            )),
            Ok((cfg, true)) => {
                if let Some(parent) = self.config_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match config_store::write_atomically(
                    &self.config_path,
                    config_store::persisted_config_text(&cfg).as_bytes(),
                ) {
                    Err(e) => self.respond(ResponseLine::err(id, E_CONFIG_IO, e.to_string())),
                    Ok(()) => {
                        let was = self
                            .config
                            .as_ref()
                            .and_then(|c| c.switch_group.as_ref())
                            .and_then(|g| g.mixed.as_ref())
                            .is_some_and(|m| m.enabled);
                        let enabled = cfg
                            .switch_group
                            .as_ref()
                            .and_then(|g| g.mixed.as_ref())
                            .unwrap()
                            .enabled;
                        self.config = Some(cfg);
                        if remote {
                            self.mixed_runtime.holder = params
                                .get("keyboard_holder_device_id")
                                .and_then(Value::as_str)
                                .map(str::to_owned);
                            self.mixed_runtime.holder_known = self.mixed_runtime.holder.is_some()
                                && params
                                    .get("holder_confidence")
                                    .and_then(Value::as_str)
                                    .unwrap_or("known")
                                    == "known";
                        }
                        // Saving a disabled assignment does not finish the
                        // previous close/recovery. Keep its intent and backoff.
                        let continuing_close = self.mixed_runtime.disable_transition && !enabled;
                        self.mixed_runtime.disable_transition =
                            (was && !enabled) || continuing_close;
                        let unfinished = continuing_close || self.mixed_runtime.enumeration_pending;
                        if unfinished {
                            self.mixed_runtime
                                .next_retry_ms
                                .get_or_insert_with(|| now_ms() + 500);
                        } else if self.mixed_runtime.next_retry_ms.is_none() {
                            self.mixed_runtime.retry_attempt = 0;
                        }
                        self.mixed_runtime.announce_change = !remote;
                        self.retire_pending(&[]);
                        self.mixed_runtime.pending_remote.clear();
                        if was != enabled || enabled || remote {
                            self.begin_mixed_transition(if enabled {
                                "enabling"
                            } else if was {
                                "disabling"
                            } else {
                                "restoring"
                            });
                            self.reconcile_mixed();
                        } else {
                            self.notify("mixed.changed",json!({"mixed":self.mixed_state(),"switch_group":self.config.as_ref().and_then(|c|c.switch_group.as_ref())}));
                            self.mixed_runtime.announce_change = false;
                        }
                        self.respond(ResponseLine::ok(
                            id,
                            json!({"saved":true,"applied":true,"mixed":self.mixed_state()}),
                        ));
                    }
                }
            }
        }
    }
}
