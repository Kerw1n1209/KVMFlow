//! Deterministic pairing proposals. No disk, network, keys, or hardware effects.
//! All entropy, time, reachability and user choices are explicit inputs so both
//! peers can recompute the same plan byte for byte.
use crate::{
    config::{InputSource, LocalDeviceProfile, MultiDeviceConfig},
    errors::{CoreError, E_CONFIG_INVALID, E_LAN_GROUP_CONFLICT},
    identity::{new_group_id, rename_device_id},
    mixed::MixedConfig,
    multidevice::{GroupDevice, MonitorInput, SwitchGroupSnapshot},
    sync::{normalize_snapshot, GroupSync, Member, SectionMeta},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProfileInput {
    Value {
        value: u16,
        #[serde(default)]
        source: InputSource,
    },
    Pending,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileMonitor {
    pub fingerprint: String,
    pub name: String,
    pub builtin: bool,
    pub input: ProfileInput,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileReport {
    pub device_id: String,
    pub name: String,
    pub platform: String,
    pub profile_rev: u64,
    pub monitors: Vec<ProfileMonitor>,
}
impl ProfileReport {
    pub fn from_config(config: &MultiDeviceConfig, platform: &str) -> Self {
        Self {
            device_id: config.local_device.device_id.clone(),
            name: config.local_device.host_label.clone(),
            platform: platform.into(),
            profile_rev: config
                .switch_group
                .as_ref()
                .and_then(|g| {
                    g.devices
                        .iter()
                        .find(|d| d.device_id == config.local_device.device_id)
                })
                .map_or(0, |d| d.profile_rev),
            monitors: config
                .local_device
                .monitors
                .iter()
                .map(|m| ProfileMonitor {
                    fingerprint: m.fingerprint.clone(),
                    name: m.label.clone(),
                    builtin: false,
                    // An input never observed while this host owned the
                    // picture is a guess, not a value (§6.1: value | pending).
                    input: if m.source == InputSource::Unknown {
                        ProfileInput::Pending
                    } else {
                        ProfileInput::Value {
                            value: m.local_input,
                            source: m.source,
                        }
                    },
                })
                .collect(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PairSide {
    pub device_id: String,
    pub host_label: String,
    pub platform: String,
    pub public_key: String,
    pub local_device: LocalDeviceProfile,
    pub switch_group: Option<SwitchGroupSnapshot>,
    pub established: bool,
    pub profile: ProfileReport,
    /// Authenticated current reachability, supplied identically in pair.plan.
    #[serde(default)]
    pub online_device_ids: BTreeSet<String>,
}
impl PairSide {
    pub fn from_config(config: &MultiDeviceConfig, platform: &str, public_key: &str) -> Self {
        let established = config.switch_group.as_ref().is_some_and(|g| {
            g.group_sync()
                .members
                .iter()
                .any(|m| !m.removed && m.device_id != config.local_device.device_id)
        });
        Self {
            device_id: config.local_device.device_id.clone(),
            host_label: config.local_device.host_label.clone(),
            platform: platform.into(),
            public_key: public_key.into(),
            local_device: config.local_device.clone(),
            switch_group: config.switch_group.clone(),
            established,
            profile: ProfileReport::from_config(config, platform),
            online_device_ids: BTreeSet::new(),
        }
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MixedChoice {
    Basis,
    Joiner,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplacementChoice {
    pub old_device_id: String,
    pub replace: bool,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairOptions {
    pub initiator_device_id: String,
    /// Shared random token generated once by the initiator, never independently.
    pub group_random: [u8; 16],
    pub now_ms: u64,
    /// Joiner's backend fingerprint -> canonical group fingerprint, confirmed by user.
    pub monitor_aliases: BTreeMap<String, String>,
    /// Joiner fingerprints the user explicitly declared as a DIFFERENT
    /// physical display than the guessed group fingerprint. They stay
    /// unmatched (a blocking `monitor_unmatched`), so the plan is pin-only
    /// until the user resolves them; never guessed as the same display.
    #[serde(default)]
    pub monitor_alias_rejections: BTreeSet<String>,
    pub mixed: Option<MixedChoice>,
    pub replacement: Option<ReplacementChoice>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Blocking {
    MonitorUnmatched {
        basis: Vec<String>,
        joiner: Vec<String>,
    },
    InputPending {
        device_id: String,
        fingerprint: String,
    },
    SetupRequired {
        device_id: String,
    },
    DuplicateInput {
        fingerprint: String,
        value: u16,
        device_ids: Vec<String>,
    },
    InvalidSnapshot {
        message: String,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Choice {
    MonitorAlias {
        group_fingerprint: String,
        local_fingerprint: String,
        default_same: bool,
        resolved: bool,
    },
    Mixed {
        basis: MixedConfig,
        joiner: MixedConfig,
        default: MixedChoice,
        resolved: bool,
    },
    ReplaceDevice {
        old_device_id: String,
        new_device_id: String,
        default_replace: bool,
        resolved: bool,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Info {
    PeerGuessCorrected {
        recorded_by: String,
        device_id: String,
        fingerprint: String,
        guessed: u16,
        actual: u16,
    },
    PortsNeedConfirmation,
    RepairedKeys,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "severity", content = "detail", rename_all = "snake_case")]
pub enum PairIssue {
    Blocking(Blocking),
    Choice(Choice),
    Info(Info),
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairPlan {
    pub basis_device_id: String,
    /// Only Some when validation and all required user choices are complete.
    pub snapshot: Option<SwitchGroupSnapshot>,
    /// UI preview, never a commit instruction. May be incomplete when blocked.
    pub proposed_snapshot: Option<SwitchGroupSnapshot>,
    pub issues: Vec<PairIssue>,
    pub renames: Vec<(String, String)>,
    /// Pin these keys after numeric confirmation even if snapshot is blocked.
    pub members: Vec<Member>,
    pub re_pair: bool,
}
impl PairPlan {
    pub fn has_blocking(&self) -> bool {
        self.issues
            .iter()
            .any(|i| matches!(i, PairIssue::Blocking(_)))
    }
    pub fn needs_choice(&self) -> bool {
        self.issues.iter().any(|i| {
            matches!(
                i,
                PairIssue::Choice(
                    Choice::MonitorAlias {
                        resolved: false,
                        ..
                    } | Choice::Mixed {
                        resolved: false,
                        ..
                    } | Choice::ReplaceDevice {
                        resolved: false,
                        ..
                    }
                )
            )
        })
    }
}
fn invalid(s: impl Into<String>) -> CoreError {
    CoreError::new(E_CONFIG_INVALID, s)
}
fn bump(n: u64) -> Result<u64, CoreError> {
    n.checked_add(1)
        .ok_or_else(|| invalid("pair revision overflow"))
}
fn owner<'a>(g: &'a SwitchGroupSnapshot, id: &str) -> Option<&'a GroupDevice> {
    g.devices.iter().find(|d| d.device_id == id)
}
fn prefix(fp: &str) -> String {
    fp.split('-').take(2).collect::<Vec<_>>().join("-")
}
fn external(side: &PairSide) -> Vec<&ProfileMonitor> {
    side.profile
        .monitors
        .iter()
        .filter(|m| !m.builtin)
        .collect()
}
fn normalized_side(
    side: &PairSide,
    other: &str,
    renames: &mut Vec<(String, String)>,
) -> Option<SwitchGroupSnapshot> {
    let mut g = side.switch_group.clone()?;
    if g.devices.len() == 2 && g.devices.iter().any(|d| d.device_id == "legacy-remote") {
        rename_device_id(&mut g, "legacy-remote", other);
        renames.push(("legacy-remote".into(), other.into()));
    }
    normalize_snapshot(&mut g);
    Some(g)
}
fn remap_monitors(g: &mut SwitchGroupSnapshot, aliases: &BTreeMap<String, String>) {
    let map = |fp: &mut String| {
        if let Some(canonical) = aliases.get(fp) {
            *fp = canonical.clone();
        }
    };
    for d in &mut g.devices {
        for m in &mut d.monitors {
            map(&mut m.fingerprint);
        }
    }
    for l in &mut g.layouts {
        for m in &mut l.monitors {
            map(&mut m.fingerprint);
        }
    }
    if let Some(m) = &mut g.mixed {
        for a in &mut m.assignment {
            map(&mut a.fingerprint);
        }
    }
}
fn semantic_mixed(m: &MixedConfig) -> MixedConfig {
    let mut m = m.clone();
    m.revision = 0;
    m.assignment
        .sort_by(|a, b| a.fingerprint.cmp(&b.fingerprint));
    m
}
fn check_side(s: &PairSide) -> Result<(), CoreError> {
    if s.device_id.is_empty()
        || s.device_id.starts_with("legacy-")
        || s.public_key.is_empty()
        || s.local_device.device_id != s.device_id
        || s.profile.device_id != s.device_id
        || s.profile.platform != s.platform
    {
        return Err(invalid(
            "pair side identity, profile, or public key is inconsistent",
        ));
    }
    let mut seen = BTreeSet::new();
    if s.profile
        .monitors
        .iter()
        .any(|m| m.fingerprint.is_empty() || !seen.insert(m.fingerprint.clone()))
    {
        return Err(invalid("duplicate or empty reported display fingerprint"));
    }
    Ok(())
}

/// Argument order is irrelevant: the initiator and basis are selected from
/// explicit identities and established group state, not the receiver's view.
pub fn plan_pair(a: &PairSide, b: &PairSide, options: &PairOptions) -> Result<PairPlan, CoreError> {
    check_side(a)?;
    check_side(b)?;
    if a.device_id == b.device_id {
        return Err(invalid("cannot pair an identity with itself"));
    }
    if options.initiator_device_id != a.device_id && options.initiator_device_id != b.device_id {
        return Err(invalid("initiator must be one of the pair"));
    }
    let established = |s: &PairSide| {
        s.switch_group.as_ref().is_some_and(|g| {
            g.group_sync()
                .members
                .iter()
                .any(|m| !m.removed && m.device_id != s.device_id)
        })
    };
    if a.established != established(a) || b.established != established(b) {
        return Err(invalid("established flag disagrees with trust members"));
    }
    let same_group = a
        .switch_group
        .as_ref()
        .zip(b.switch_group.as_ref())
        .is_some_and(|(a, b)| {
            a.group_id == b.group_id
                && a.group_id != "legacy-switch-group"
                && !a.group_id.is_empty()
        });
    if a.established && b.established && !same_group {
        return Err(CoreError::new(
            E_LAN_GROUP_CONFLICT,
            "peer is already in another switch group",
        ));
    }
    let (basis, joiner) = if a.established != b.established {
        if a.established {
            (a, b)
        } else {
            (b, a)
        }
    } else if options.initiator_device_id == a.device_id {
        (a, b)
    } else {
        (b, a)
    };
    let mut plan = PairPlan {
        basis_device_id: basis.device_id.clone(),
        snapshot: None,
        proposed_snapshot: None,
        issues: Vec::new(),
        renames: Vec::new(),
        members: Vec::new(),
        re_pair: same_group,
    };
    let meta = SectionMeta {
        updated_at_ms: options.now_ms,
        updated_by: basis.device_id.clone(),
    };
    // Re-pair never touches display configuration, mode, counters or names.
    if same_group {
        let mut g = basis.switch_group.clone().unwrap();
        let mut sync = g.group_sync();
        update_members(&mut sync, basis, joiner, &meta, true)?;
        plan.members = sync.members.clone();
        g.set_group_sync(sync);
        if let Err(e) = g.validate() {
            plan.issues
                .push(PairIssue::Blocking(Blocking::InvalidSnapshot {
                    message: e.to_string(),
                }));
        } else {
            plan.snapshot = Some(g.clone());
        }
        plan.proposed_snapshot = Some(g);
        plan.issues.push(PairIssue::Info(Info::RepairedKeys));
        return Ok(plan);
    }
    let mut bg = normalized_side(basis, &joiner.device_id, &mut plan.renames);
    let mut jg = normalized_side(joiner, &basis.device_id, &mut plan.renames);
    // A lost installation can inherit an offline same-name/same-platform slot.
    if let Some(g) = &mut bg {
        if owner(g, &joiner.device_id).is_none() {
            let candidates: Vec<_> = g
                .devices
                .iter()
                .filter(|d| {
                    d.device_id != basis.device_id
                        && d.name == joiner.host_label
                        && !basis.online_device_ids.contains(&d.device_id)
                        && g.group_sync().members.iter().any(|m| {
                            !m.removed
                                && m.device_id == d.device_id
                                && m.platform == joiner.platform
                        })
                })
                .map(|d| d.device_id.clone())
                .collect();
            if let Some(choice) = &options.replacement {
                if !candidates.contains(&choice.old_device_id) {
                    return Err(invalid("replacement is not an offline matching device"));
                }
            }
            if candidates.len() == 1 || options.replacement.is_some() {
                let old = options
                    .replacement
                    .as_ref()
                    .map(|c| c.old_device_id.clone())
                    .unwrap_or_else(|| candidates[0].clone());
                let resolved = options.replacement.is_some();
                let replace = options.replacement.as_ref().is_none_or(|c| c.replace);
                plan.issues.push(PairIssue::Choice(Choice::ReplaceDevice {
                    old_device_id: old.clone(),
                    new_device_id: joiner.device_id.clone(),
                    default_replace: true,
                    resolved,
                }));
                if replace {
                    // Keep old key as a tombstone; rename only config references.
                    let mut tombstone = g
                        .group_sync()
                        .members
                        .into_iter()
                        .find(|m| m.device_id == old)
                        .unwrap();
                    tombstone.removed = true;
                    tombstone.revision = bump(tombstone.revision)?;
                    tombstone.meta = meta.clone();
                    rename_device_id(g, &old, &joiner.device_id);
                    let mut sync = g.group_sync();
                    sync.members.retain(|m| m.device_id != joiner.device_id);
                    sync.members.push(tombstone);
                    g.set_group_sync(sync);
                    plan.renames.push((old, joiner.device_id.clone()));
                }
            }
        }
    }
    // Canonical monitor names are the basis owner's group fingerprints.
    let canonical: Vec<String> = bg
        .as_ref()
        .and_then(|g| owner(g, &basis.device_id))
        .map(|d| d.monitors.iter().map(|m| m.fingerprint.clone()).collect())
        .unwrap_or_else(|| {
            external(basis)
                .iter()
                .map(|m| m.fingerprint.clone())
                .collect()
        });
    let mut aliases = options.monitor_aliases.clone();
    let joiner_monitors = external(joiner);
    if aliases.iter().any(|(local, group)| {
        !joiner_monitors.iter().any(|m| &m.fingerprint == local) || !canonical.contains(group)
    }) {
        return Err(invalid("monitor mapping references an unknown display"));
    }
    for rejected in &options.monitor_alias_rejections {
        if !joiner_monitors.iter().any(|m| &m.fingerprint == rejected) {
            return Err(invalid(
                "rejected monitor alias references an unknown display",
            ));
        }
        if aliases.contains_key(rejected) {
            return Err(invalid(
                "a monitor cannot be both aliased and rejected in one plan",
            ));
        }
    }
    // Existing local aliases in the joiner profile can already identify group fingerprints.
    if let Some(g) = &jg {
        if let Some(d) = owner(g, &joiner.device_id) {
            for m in &d.monitors {
                if canonical.contains(&m.fingerprint)
                    && joiner_monitors
                        .iter()
                        .any(|r| r.fingerprint == m.backend_fingerprint())
                {
                    aliases
                        .entry(m.backend_fingerprint().into())
                        .or_insert(m.fingerprint.clone());
                }
            }
        }
    }
    for m in &joiner_monitors {
        if canonical.contains(&m.fingerprint) {
            aliases
                .entry(m.fingerprint.clone())
                .or_insert(m.fingerprint.clone());
        }
    }
    let used: BTreeSet<_> = aliases.values().cloned().collect();
    let mut unmatched_basis: Vec<_> = canonical
        .iter()
        .filter(|f| !used.contains(*f))
        .cloned()
        .collect();
    let mut unmatched_joiner: Vec<_> = joiner_monitors
        .iter()
        .filter(|m| !aliases.contains_key(&m.fingerprint))
        .map(|m| m.fingerprint.clone())
        .collect();
    let initial_basis = unmatched_basis.clone();
    let initial_joiner = unmatched_joiner.clone();
    for local in initial_joiner {
        if options.monitor_alias_rejections.contains(&local) {
            continue; // explicit rejection: leave it unmatched
        }
        let p = prefix(&local);
        let left: Vec<_> = initial_basis
            .iter()
            .filter(|f| prefix(f) == p)
            .cloned()
            .collect();
        let right: Vec<_> = unmatched_joiner
            .iter()
            .filter(|f| prefix(f) == p)
            .cloned()
            .collect();
        if left.len() == 1
            && right.len() == 1
            && !local.contains("NOSERIAL")
            && !left[0].contains("NOSERIAL")
        {
            let group = left[0].clone();
            aliases.insert(local.clone(), group.clone());
            plan.issues.push(PairIssue::Choice(Choice::MonitorAlias {
                group_fingerprint: group.clone(),
                local_fingerprint: local.clone(),
                default_same: true,
                resolved: false,
            }));
            unmatched_basis.retain(|f| f != &group);
            unmatched_joiner.retain(|f| f != &local);
        }
    }
    // A rejection always wins over any implicit alias (profile memory or a
    // same-prefix guess), so the display stays unmatched/pending.
    for rejected in &options.monitor_alias_rejections {
        aliases.remove(rejected);
    }
    // Explicit mappings are confirmed; report them as resolved choices too.
    for (local, group) in &options.monitor_aliases {
        if local != group {
            plan.issues.push(PairIssue::Choice(Choice::MonitorAlias {
                group_fingerprint: group.clone(),
                local_fingerprint: local.clone(),
                default_same: true,
                resolved: true,
            }));
        }
    }
    if aliases.values().collect::<BTreeSet<_>>().len() != aliases.len() {
        return Err(invalid("monitor mapping must be one-to-one"));
    }
    if !unmatched_basis.is_empty() || !unmatched_joiner.is_empty() {
        plan.issues
            .push(PairIssue::Blocking(Blocking::MonitorUnmatched {
                basis: unmatched_basis,
                joiner: unmatched_joiner,
            }));
    }
    if let Some(g) = &mut jg {
        remap_monitors(g, &aliases);
    }
    let mut g = bg
        .clone()
        .or_else(|| jg.clone())
        .unwrap_or(SwitchGroupSnapshot {
            group_id: String::new(),
            revision: 0,
            devices: Vec::new(),
            layouts: Vec::new(),
            mixed: None,
            sync: serde_json::Value::Null,
        });
    // A basis without a snapshot starts a two-owner topology, using the peer's shared drafts.
    if bg.is_none() {
        g.devices
            .retain(|d| d.device_id == basis.device_id || d.device_id == joiner.device_id);
        g.devices.sort_by_key(|d| d.device_id != basis.device_id);
        for (i, d) in g.devices.iter_mut().enumerate() {
            d.port_index = (i + 1) as u16;
        }
    }
    let max_profile = |id: &str| {
        bg.iter()
            .chain(jg.iter())
            .map(|s| {
                owner(s, id).map_or(0, |d| d.profile_rev).max(
                    s.group_sync()
                        .profile_cache
                        .get(id)
                        .map_or(0, |p| p.profile_rev),
                )
            })
            .max()
            .unwrap_or(0)
    };
    for side in [basis, joiner] {
        let existing = owner(&g, &side.device_id).cloned();
        let old_profile = max_profile(&side.device_id).max(side.profile.profile_rev);
        let mut monitors = Vec::new();
        if external(side).is_empty() {
            plan.issues
                .push(PairIssue::Blocking(Blocking::SetupRequired {
                    device_id: side.device_id.clone(),
                }));
        }
        for m in external(side) {
            let fp = if side.device_id == joiner.device_id {
                aliases
                    .get(&m.fingerprint)
                    .cloned()
                    .unwrap_or_else(|| m.fingerprint.clone())
            } else {
                bg.as_ref()
                    .and_then(|s| owner(s, &side.device_id))
                    .and_then(|d| {
                        d.monitors
                            .iter()
                            .find(|v| v.backend_fingerprint() == m.fingerprint)
                    })
                    .map(|m| m.fingerprint.clone())
                    .unwrap_or_else(|| m.fingerprint.clone())
            };
            match m.input {
                ProfileInput::Pending => {
                    plan.issues
                        .push(PairIssue::Blocking(Blocking::InputPending {
                            device_id: side.device_id.clone(),
                            fingerprint: fp,
                        }));
                }
                ProfileInput::Value { value, .. } => {
                    monitors.push(MonitorInput {
                        local_fingerprint: (m.fingerprint != fp).then(|| m.fingerprint.clone()),
                        fingerprint: fp,
                        label: m.name.clone(),
                        local_input: value,
                        follow: true,
                    });
                }
            }
        }
        let port = if let Some(d) = &existing {
            d.port_index
        } else {
            (1..=u16::MAX)
                .find(|p| g.devices.iter().all(|d| d.port_index != *p))
                .ok_or_else(|| invalid("no available USB port"))?
        };
        let device = GroupDevice {
            device_id: side.device_id.clone(),
            name: existing
                .as_ref()
                .filter(|d| !d.name.is_empty())
                .map(|d| d.name.clone())
                .unwrap_or_else(|| side.host_label.clone()),
            port_index: port,
            monitors,
            profile_rev: bump(old_profile)?,
            available_builtin_screens: Some(
                side.profile.monitors.iter().filter(|m| m.builtin).count() as u32,
            ),
            arrival_layout_id: existing.and_then(|d| d.arrival_layout_id),
        };
        g.devices.retain(|d| d.device_id != side.device_id);
        g.devices.push(device);
    }
    // Any remaining placeholder cannot be trusted as a third owner profile.
    g.devices.retain(|d| d.device_id != "legacy-remote");
    if g.devices.len() == 2 {
        let basis_port = owner(&g, &basis.device_id).unwrap().port_index;
        let joiner_port = if basis_port == 1 { 2 } else { 1 };
        g.devices
            .iter_mut()
            .find(|d| d.device_id == joiner.device_id)
            .unwrap()
            .port_index = joiner_port;
    }
    for (recorded_by, observed) in [(basis, &bg), (joiner, &jg)] {
        for side in [basis, joiner] {
            if recorded_by.device_id == side.device_id {
                continue;
            }
            if let Some(guess) = observed.as_ref().and_then(|s| owner(s, &side.device_id)) {
                for actual in &owner(&g, &side.device_id).unwrap().monitors {
                    if let Some(old) = guess
                        .monitors
                        .iter()
                        .find(|m| m.fingerprint == actual.fingerprint)
                    {
                        if old.local_input != actual.local_input {
                            plan.issues.push(PairIssue::Info(Info::PeerGuessCorrected {
                                recorded_by: recorded_by.device_id.clone(),
                                device_id: side.device_id.clone(),
                                fingerprint: actual.fingerprint.clone(),
                                guessed: old.local_input,
                                actual: actual.local_input,
                            }));
                        }
                    }
                }
            } else {
                for m in &recorded_by.local_device.monitors {
                    if let Some(preset) = &m.legacy_remote_preset {
                        let fp = if recorded_by.device_id == joiner.device_id {
                            aliases.get(&m.fingerprint).unwrap_or(&m.fingerprint)
                        } else {
                            &m.fingerprint
                        };
                        if let Some(actual) = owner(&g, &side.device_id)
                            .unwrap()
                            .monitors
                            .iter()
                            .find(|a| &a.fingerprint == fp)
                        {
                            if preset.value != actual.local_input {
                                plan.issues.push(PairIssue::Info(Info::PeerGuessCorrected {
                                    recorded_by: recorded_by.device_id.clone(),
                                    device_id: side.device_id.clone(),
                                    fingerprint: fp.clone(),
                                    guessed: preset.value,
                                    actual: actual.local_input,
                                }));
                            }
                        }
                    }
                }
            }
        }
    }
    let bm = bg.as_ref().and_then(|g| g.mixed.clone());
    let jm = jg.as_ref().and_then(|g| g.mixed.clone());
    let choose_joiner = match (&bm, &jm) {
        (None, Some(_)) => true,
        (Some(b), Some(j)) if semantic_mixed(b) != semantic_mixed(j) => {
            plan.issues.push(PairIssue::Choice(Choice::Mixed {
                basis: b.clone(),
                joiner: j.clone(),
                default: MixedChoice::Basis,
                resolved: options.mixed.is_some(),
            }));
            options.mixed == Some(MixedChoice::Joiner)
        }
        _ => false,
    };
    let selected = if choose_joiner {
        jg.as_ref()
    } else {
        bg.as_ref()
    };
    if let Some(selected) = selected {
        g.mixed = selected.mixed.clone();
        g.layouts = selected.layouts.clone();
        for d in &mut g.devices {
            d.arrival_layout_id =
                owner(selected, &d.device_id).and_then(|d| d.arrival_layout_id.clone());
        }
    }
    let max_rev = bg
        .iter()
        .chain(jg.iter())
        .map(|s| s.revision)
        .max()
        .unwrap_or(0);
    let max_mixed = bg
        .iter()
        .chain(jg.iter())
        .filter_map(|s| s.mixed.as_ref())
        .map(|m| m.revision)
        .max()
        .unwrap_or(0);
    g.revision = bump(max_rev)?;
    g.mixed.get_or_insert_with(MixedConfig::default).revision = bump(max_mixed)?;
    if bg
        .as_ref()
        .is_none_or(|s| s.group_id.is_empty() || s.group_id == "legacy-switch-group")
    {
        g.group_id = new_group_id(options.group_random);
    }
    let mut sync = bg
        .as_ref()
        .or(jg.as_ref())
        .map(|s| s.group_sync())
        .unwrap_or_default();
    sync.topology = meta.clone();
    sync.mixed = meta.clone();
    sync.ports_confirmed = false;
    update_members(&mut sync, basis, joiner, &meta, false)?;
    sync.profile_cache
        .retain(|id, _| !g.devices.iter().any(|d| &d.device_id == id));
    sync.archived_arrivals
        .retain(|id, _| !g.devices.iter().any(|d| &d.device_id == id));
    plan.members = sync.members.clone();
    g.set_group_sync(sync);
    plan.issues
        .push(PairIssue::Info(Info::PortsNeedConfirmation));
    let mut inputs: BTreeMap<(String, u16), Vec<String>> = BTreeMap::new();
    for d in &g.devices {
        for m in &d.monitors {
            inputs
                .entry((m.fingerprint.clone(), m.local_input))
                .or_default()
                .push(d.device_id.clone());
        }
    }
    for ((fp, value), ids) in inputs {
        if ids.len() > 1 {
            plan.issues
                .push(PairIssue::Blocking(Blocking::DuplicateInput {
                    fingerprint: fp,
                    value,
                    device_ids: ids,
                }));
        }
    }
    normalize_snapshot(&mut g);
    if let Err(e) = g.validate() {
        if !plan.has_blocking() {
            plan.issues
                .push(PairIssue::Blocking(Blocking::InvalidSnapshot {
                    message: e.to_string(),
                }));
        }
    }
    plan.renames.sort();
    plan.renames.dedup();
    if !plan.has_blocking() && !plan.needs_choice() {
        plan.snapshot = Some(g.clone());
    }
    plan.proposed_snapshot = Some(g);
    Ok(plan)
}
fn update_members(
    sync: &mut GroupSync,
    basis: &PairSide,
    joiner: &PairSide,
    meta: &SectionMeta,
    re_pair: bool,
) -> Result<(), CoreError> {
    for side in [basis, joiner] {
        // Both authenticated peers confirmed these pins. Advance beyond every
        // pre-pair view, including a newer revision unknown to the basis.
        let max_revision = basis
            .switch_group
            .iter()
            .chain(joiner.switch_group.iter())
            .flat_map(|g| g.group_sync().members)
            .chain(sync.members.iter().cloned())
            .filter(|m| m.device_id == side.device_id)
            .map(|m| m.revision)
            .max()
            .unwrap_or(0);
        let existing = sync
            .members
            .iter()
            .position(|m| m.device_id == side.device_id);
        if let Some(index) = existing {
            let m = &mut sync.members[index];
            if re_pair || m.public_key != side.public_key || m.removed {
                let revision = bump(max_revision)?;
                m.public_key = side.public_key.clone();
                m.removed = false;
                m.revision = revision;
                m.meta = meta.clone();
            }
        } else {
            sync.members.push(Member {
                device_id: side.device_id.clone(),
                public_key: side.public_key.clone(),
                platform: side.platform.clone(),
                introduced_by: basis.device_id.clone(),
                revision: bump(max_revision)?,
                meta: meta.clone(),
                removed: false,
            });
        }
    }
    sync.members.sort_by(|a, b| a.device_id.cmp(&b.device_id));
    Ok(())
}
