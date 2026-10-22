//! Shared section metadata; same wire format as T1's mixed metadata.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(default)]
pub struct SectionMeta {
    pub updated_at_ms: u64,
    pub updated_by: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Member {
    pub device_id: String,
    pub public_key: String,
    pub platform: String,
    pub introduced_by: String,
    pub revision: u64,
    pub meta: SectionMeta,
    pub removed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct GroupSync {
    pub topology: SectionMeta,
    pub mixed: SectionMeta,
    pub ports_confirmed: bool,
    pub members: Vec<Member>,
    /// Internal owner registers retained while topology omits the device.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profile_cache: BTreeMap<String, CachedProfile>,
    /// Internal mixed archive references for devices absent from topology.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub archived_arrivals: BTreeMap<String, String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// A switch group is a small physical desk. Refuse over 64 current+retained
/// identities rather than evicting a register and breaking version monotonicity.
pub const MAX_PROFILE_REGISTERS: usize = 64;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedProfile {
    pub profile_rev: u64,
    pub monitors: Vec<crate::multidevice::MonitorInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_builtin_screens: Option<u32>,
}
impl From<&GroupDevice> for CachedProfile {
    fn from(d: &GroupDevice) -> Self {
        Self {
            profile_rev: d.profile_rev,
            monitors: d.monitors.clone(),
            available_builtin_screens: d.available_builtin_screens,
        }
    }
}
fn profiles(s: &SwitchGroupSnapshot) -> BTreeMap<String, CachedProfile> {
    let mut result = s.group_sync().profile_cache;
    for d in &s.devices {
        result.insert(d.device_id.clone(), d.into());
    }
    result
}
fn arrivals(s: &SwitchGroupSnapshot) -> BTreeMap<String, String> {
    let mut result = s.group_sync().archived_arrivals;
    for d in &s.devices {
        if let Some(id) = &d.arrival_layout_id {
            result.insert(d.device_id.clone(), id.clone());
        } else {
            result.remove(&d.device_id);
        }
    }
    result
}

impl crate::multidevice::SwitchGroupSnapshot {
    pub fn group_sync(&self) -> GroupSync {
        let mut sync: GroupSync = serde_json::from_value(self.sync.clone()).unwrap_or_default();
        // Pre-LAN groups already had a usable order. Only an explicit false
        // from pairing requires the new port confirmation gate.
        if self.sync.get("ports_confirmed").is_none() {
            sync.ports_confirmed = true;
        }
        sync
    }
    pub fn set_group_sync(&mut self, mut sync: GroupSync) {
        // Applying a trust tombstone immediately purges retained owner data.
        // The tombstone stays forever; old snapshots cannot restore that cache.
        let revoked: BTreeSet<_> = sync
            .members
            .iter()
            .filter(|m| m.removed)
            .map(|m| m.device_id.clone())
            .collect();
        let trusted: BTreeSet<_> = sync
            .members
            .iter()
            .filter(|m| !m.removed)
            .map(|m| m.device_id.clone())
            .collect();
        sync.profile_cache.retain(|id, _| trusted.contains(id));
        sync.archived_arrivals.retain(|id, _| !revoked.contains(id));
        self.sync = serde_json::to_value(sync).expect("serializable sync");
    }
}

use crate::errors::{
    CoreError, E_CONFIG_INVALID, E_LAN_GROUP_CONFLICT, E_LAN_KEY_CHANGED, E_LAN_NOT_MEMBER,
    E_NOT_OWNER,
};
use crate::multidevice::{GroupDevice, SwitchGroupSnapshot};
use blake2::{Blake2s256, Digest};
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub type VersionKey = (u64, u64, String);

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangedSections {
    pub topology: bool,
    pub mixed: bool,
    pub profiles: Vec<String>,
    pub members: bool,
    /// Emit lan.sync.divergent for each section. This is data, not a core log sink.
    pub divergent: Vec<String>,
    /// Receiver must log and push its authoritative profile back to these peers.
    pub rejected_profiles: Vec<String>,
    /// Invalid cross-section combinations retain the original local snapshot.
    pub validation_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncSummary {
    pub topology: VersionKey,
    pub mixed: VersionKey,
    pub profiles: BTreeMap<String, u64>,
    pub members_digest: String,
    pub digest: String,
}

/// Canonical object keys, preserving array order (section projections sort sets).
pub fn canonical_json<T: Serialize>(value: &T) -> Vec<u8> {
    fn sorted(v: Value) -> Value {
        match v {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(k, v)| (k, sorted(v)))
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(a) => Value::Array(a.into_iter().map(sorted).collect()),
            v => v,
        }
    }
    serde_json::to_vec(&sorted(
        serde_json::to_value(value).expect("serializable section"),
    ))
    .unwrap()
}
pub fn digest<T: Serialize>(value: &T) -> String {
    Blake2s256::digest(canonical_json(value))
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn key(rev: u64, meta: &SectionMeta) -> VersionKey {
    (rev, meta.updated_at_ms, meta.updated_by.clone())
}
impl SyncSummary {
    pub fn from_snapshot(s: &SwitchGroupSnapshot) -> Self {
        let mut s = s.clone();
        normalize_snapshot(&mut s);
        let sync = s.group_sync();
        Self {
            topology: key(s.revision, &sync.topology),
            mixed: key(s.mixed.as_ref().map_or(0, |m| m.revision), &sync.mixed),
            profiles: profiles(&s)
                .into_iter()
                .map(|(id, p)| (id, p.profile_rev))
                .collect(),
            members_digest: digest(&sync.members),
            digest: digest(&s),
        }
    }
}

/// Set-like collections have a stable order on the wire and in section digests.
pub fn normalize_snapshot(s: &mut SwitchGroupSnapshot) {
    s.devices.sort_by(|a, b| a.device_id.cmp(&b.device_id));
    for d in &mut s.devices {
        d.monitors.sort_by(|a, b| a.fingerprint.cmp(&b.fingerprint));
    }
    s.layouts.sort_by(|a, b| a.id.cmp(&b.id));
    for l in &mut s.layouts {
        l.monitors.sort_by(|a, b| a.fingerprint.cmp(&b.fingerprint));
    }
    if let Some(m) = &mut s.mixed {
        m.assignment
            .sort_by(|a, b| a.fingerprint.cmp(&b.fingerprint));
    }
    if let Some(members) = s.sync.get_mut("members").and_then(Value::as_array_mut) {
        members.sort_by(|a, b| a["device_id"].as_str().cmp(&b["device_id"].as_str()));
    }
    if let Some(cache) = s
        .sync
        .get_mut("profile_cache")
        .and_then(Value::as_object_mut)
    {
        for p in cache.values_mut() {
            if let Some(monitors) = p.get_mut("monitors").and_then(Value::as_array_mut) {
                monitors.sort_by(|a, b| a["fingerprint"].as_str().cmp(&b["fingerprint"].as_str()));
            }
        }
    }
}
fn topology(s: &SwitchGroupSnapshot) -> Value {
    let mut devices: Vec<_> = s
        .devices
        .iter()
        .map(|d| json!({"device_id":d.device_id,"name":d.name,"port_index":d.port_index}))
        .collect();
    devices.sort_by(|a, b| a["device_id"].as_str().cmp(&b["device_id"].as_str()));
    json!({"devices":devices,"ports_confirmed":s.group_sync().ports_confirmed,"extra":s.group_sync().extra})
}
fn mixed(s: &SwitchGroupSnapshot, include_revision: bool) -> Value {
    let mut v = json!({"mixed":s.mixed,"layouts":s.layouts,"arrivals":arrivals(s)});
    if !include_revision {
        if let Some(m) = v["mixed"].as_object_mut() {
            m.remove("revision");
        }
    }
    v
}
fn choose_remote<K: Ord>(
    lk: K,
    rk: K,
    l: &Value,
    r: &Value,
    section: &str,
    changed: &mut ChangedSections,
) -> bool {
    match rk.cmp(&lk) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => {
            if l != r {
                changed.divergent.push(section.into());
                digest(r) > digest(l)
            } else {
                false
            }
        }
    }
}
fn changed_between(l: &SwitchGroupSnapshot, r: &SwitchGroupSnapshot) -> ChangedSections {
    ChangedSections {
        topology: topology(l) != topology(r)
            || l.revision != r.revision
            || l.group_sync().topology != r.group_sync().topology,
        mixed: mixed(l, true) != mixed(r, true) || l.group_sync().mixed != r.group_sync().mixed,
        profiles: profiles(r)
            .into_iter()
            .filter(|(id, p)| profiles(l).get(id) != Some(p))
            .map(|(id, _)| id)
            .collect(),
        members: l.group_sync().members != r.group_sync().members,
        ..Default::default()
    }
}

/// Pure join on owner-vetted snapshots. Call merge_group at the network boundary.
/// Algebraic laws hold on valid section combinations; invalid results explicitly
/// retain `local`, as required by the recovery contract (not a CRDT join).
pub fn merge_sections(
    local: &SwitchGroupSnapshot,
    remote: &SwitchGroupSnapshot,
) -> Result<(SwitchGroupSnapshot, ChangedSections), CoreError> {
    if local.group_id != remote.group_id {
        return Err(CoreError::new(
            E_LAN_GROUP_CONFLICT,
            "different switch groups",
        ));
    }
    if local == remote {
        return Ok((local.clone(), ChangedSections::default()));
    }
    let mut l = local.clone();
    let mut r = remote.clone();
    normalize_snapshot(&mut l);
    normalize_snapshot(&mut r);
    let ls = l.group_sync();
    let rs = r.group_sync();
    let mut diagnostics = ChangedSections::default();
    let rt = choose_remote(
        key(l.revision, &ls.topology),
        key(r.revision, &rs.topology),
        &topology(&l),
        &topology(&r),
        "topology",
        &mut diagnostics,
    );
    let rm = choose_remote(
        key(l.mixed.as_ref().map_or(0, |m| m.revision), &ls.mixed),
        key(r.mixed.as_ref().map_or(0, |m| m.revision), &rs.mixed),
        &mixed(&l, true),
        &mixed(&r, true),
        "mixed",
        &mut diagnostics,
    );
    let mut result = if rt { r.clone() } else { l.clone() };
    let chosen_mixed = if rm { &r } else { &l };
    result.mixed = chosen_mixed.mixed.clone();
    result.layouts = chosen_mixed.layouts.clone();
    let mut registers = profiles(&l);
    for (id, p) in profiles(&r) {
        if let Some(a) = registers.get(&id) {
            if !choose_remote(
                a.profile_rev,
                p.profile_rev,
                &json!({"monitors":a.monitors,"available_builtin_screens":a.available_builtin_screens}),
                &json!({"monitors":p.monitors,"available_builtin_screens":p.available_builtin_screens}),
                &format!("profile:{id}"),
                &mut diagnostics,
            ) {
                continue;
            }
        }
        registers.insert(id, p);
    }
    let arrival_register = arrivals(chosen_mixed);
    for d in &mut result.devices {
        let p = &registers[&d.device_id];
        d.monitors = p.monitors.clone();
        d.profile_rev = p.profile_rev;
        d.available_builtin_screens = p.available_builtin_screens;
        d.arrival_layout_id = arrival_register.get(&d.device_id).cloned();
    }
    let (members, divergent) = join_members(&ls.members, &rs.members);
    diagnostics.divergent.extend(divergent);
    let mut sync = if rt { rs.clone() } else { ls.clone() };
    sync.mixed = if rm { rs.mixed } else { ls.mixed };
    sync.members = members;
    let active: BTreeSet<_> = result
        .devices
        .iter()
        .map(|d| d.device_id.as_str())
        .collect();
    let revoked: BTreeSet<_> = sync
        .members
        .iter()
        .filter(|m| m.removed)
        .map(|m| m.device_id.as_str())
        .collect();
    sync.profile_cache = registers
        .into_iter()
        .filter(|(id, _)| !active.contains(id.as_str()) && !revoked.contains(id.as_str()))
        .collect();
    sync.archived_arrivals = arrival_register
        .into_iter()
        .filter(|(id, _)| !active.contains(id.as_str()) && !revoked.contains(id.as_str()))
        .collect();
    result.set_group_sync(sync);
    normalize_snapshot(&mut result);
    if let Err(e) = result.validate() {
        diagnostics.validation_error = Some(e.to_string());
        return Ok((local.clone(), diagnostics));
    }
    let mut changed = changed_between(local, &result);
    changed.divergent = diagnostics.divergent;
    Ok((result, changed))
}

fn join_members(local: &[Member], remote: &[Member]) -> (Vec<Member>, Vec<String>) {
    let mut members: BTreeMap<String, Member> = BTreeMap::new();
    let mut diagnostics = ChangedSections::default();
    for member in local.iter().chain(remote.iter()) {
        if let Some(a) = members.get(&member.device_id) {
            if key(a.revision, &a.meta) == key(member.revision, &member.meta)
                && a.removed != member.removed
            {
                diagnostics
                    .divergent
                    .push(format!("member:{}", member.device_id));
            }
            if !choose_remote(
                (key(a.revision, &a.meta), a.removed),
                (key(member.revision, &member.meta), member.removed),
                &json!(a),
                &json!(member),
                &format!("member:{}", member.device_id),
                &mut diagnostics,
            ) {
                continue;
            }
        }
        members.insert(member.device_id.clone(), member.clone());
    }
    (members.into_values().collect(), diagnostics.divergent)
}

/// Identity authenticated against the Noise session's static public key,
/// never copied from discovery TXT, the snapshot, or an unauthenticated hello.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthenticatedMember {
    pub device_id: String,
    pub public_key: String,
}

/// Independent trust-list join for S7. Authenticate the source against the
/// current active pinned member, then relay new identities before fetching
/// their owner reports. Caller must additionally check group identity.
/// Equal version keys prefer revocation over addition. Stale key records are
/// ignored; a same/newer version changing an existing pin requires re-pairing.
/// No mutation is returned on error, so a forwarded snapshot cannot rotate keys.
pub fn merge_members(
    local: &[Member],
    remote: &[Member],
    source: &AuthenticatedMember,
) -> Result<(Vec<Member>, Vec<String>), CoreError> {
    for list in [local, remote] {
        let mut ids = BTreeSet::new();
        if list
            .iter()
            .any(|m| m.device_id.is_empty() || m.public_key.is_empty() || !ids.insert(&m.device_id))
        {
            return Err(CoreError::new(
                E_CONFIG_INVALID,
                "invalid or duplicate trust member",
            ));
        }
    }
    let source_member = local
        .iter()
        .find(|m| m.device_id == source.device_id && !m.removed)
        .ok_or_else(|| CoreError::new(E_LAN_NOT_MEMBER, "source is not a current member"))?;
    if source_member.public_key != source.public_key {
        return Err(CoreError::new(
            E_LAN_KEY_CHANGED,
            "authenticated source key differs from pinned key",
        ));
    }
    let mut vetted = Vec::new();
    for incoming in remote {
        if let Some(pinned) = local.iter().find(|m| m.device_id == incoming.device_id) {
            if incoming.public_key != pinned.public_key {
                if key(incoming.revision, &incoming.meta) < key(pinned.revision, &pinned.meta) {
                    continue;
                }
                // Revocation never installs an incoming key, including a
                // tombstone with a stale key after a local re-pair.
                if incoming.removed {
                    let mut removal = incoming.clone();
                    removal.public_key = pinned.public_key.clone();
                    vetted.push(removal);
                    continue;
                }
                return Err(CoreError::new(
                    E_LAN_KEY_CHANGED,
                    format!(
                        "{} needs re-pairing before changing its key",
                        incoming.device_id
                    ),
                ));
            }
        }
        vetted.push(incoming.clone());
    }
    Ok(join_members(local, &vetted))
}

/// Authenticated receiver boundary. `source_id` must come from the pinned Noise
/// identity, never from the snapshot. Relayed profiles cannot raise revisions:
/// each peer must report its own profile directly. Topology/mixed/members relay.
pub fn merge_group(
    local: &SwitchGroupSnapshot,
    remote: &SwitchGroupSnapshot,
    self_id: &str,
    source_id: &str,
) -> Result<(SwitchGroupSnapshot, ChangedSections), CoreError> {
    if local.group_id != remote.group_id {
        return Err(CoreError::new(
            E_LAN_GROUP_CONFLICT,
            "different switch groups",
        ));
    }
    if !local.devices.iter().any(|d| d.device_id == self_id) {
        return Err(CoreError::new(E_NOT_OWNER, "local owner is missing"));
    }
    let mut vetted = remote.clone();
    let source = local
        .group_sync()
        .members
        .into_iter()
        .find(|m| m.device_id == source_id && !m.removed)
        .ok_or_else(|| CoreError::new(E_LAN_NOT_MEMBER, "source is not a current member"))?;
    // merge_group's source_id contract already requires the transport to pin
    // this session key. merge_members exposes the explicit key form for S7.
    let (members, member_divergence) = merge_members(
        &local.group_sync().members,
        &remote.group_sync().members,
        &AuthenticatedMember {
            device_id: source_id.into(),
            public_key: source.public_key,
        },
    )?;
    let mut vetted_sync = vetted.group_sync();
    vetted_sync.members = members;
    vetted.set_group_sync(vetted_sync);
    let mut rejected = Vec::new();
    let local_profiles = profiles(local);
    let mut remote_profiles = profiles(&vetted);
    for id in remote_profiles.keys().cloned().collect::<Vec<_>>() {
        if id == self_id || id != source_id {
            if let Some(own) = local_profiles.get(&id) {
                if remote_profiles[&id] != *own {
                    rejected.push(id.clone());
                }
                remote_profiles.insert(id, own.clone());
            } else if vetted.devices.iter().any(|d| d.device_id == id) {
                return Ok((
                    local.clone(),
                    ChangedSections {
                        rejected_profiles: vec![id],
                        validation_error: Some("new profile requires owner report".into()),
                        ..Default::default()
                    },
                ));
            } else {
                rejected.push(id.clone());
                remote_profiles.remove(&id);
            }
        }
    }
    for d in &mut vetted.devices {
        if let Some(p) = remote_profiles.get(&d.device_id) {
            d.monitors = p.monitors.clone();
            d.profile_rev = p.profile_rev;
            d.available_builtin_screens = p.available_builtin_screens;
        }
    }
    let mut sync = vetted.group_sync();
    sync.profile_cache = remote_profiles
        .into_iter()
        .filter(|(id, _)| !vetted.devices.iter().any(|d| &d.device_id == id))
        .collect();
    vetted.set_group_sync(sync);
    let (s, mut changed) = merge_sections(local, &vetted)?;
    changed.divergent.extend(member_divergence);
    if !s.devices.iter().any(|d| d.device_id == self_id) {
        changed = ChangedSections {
            validation_error: Some("merged topology removes the local owner".into()),
            ..Default::default()
        };
        changed.rejected_profiles = rejected;
        return Ok((local.clone(), changed));
    }
    changed.rejected_profiles = rejected;
    Ok((s, changed))
}

/// Stamp a local edit without accepting caller-supplied counters or metadata.
/// No partial mutation occurs on ownership, validation, or overflow failure.
pub fn stamp_local_edit(
    old: &SwitchGroupSnapshot,
    new: &SwitchGroupSnapshot,
    self_id: &str,
    now: u64,
) -> Result<(SwitchGroupSnapshot, ChangedSections), CoreError> {
    if old.group_id != new.group_id {
        return Err(CoreError::new(
            E_LAN_GROUP_CONFLICT,
            "local edit cannot replace group identity",
        ));
    }
    let mut o = old.clone();
    let mut n = new.clone();
    normalize_snapshot(&mut o);
    normalize_snapshot(&mut n);
    let mut internal = n.group_sync();
    let previous = o.group_sync();
    internal.profile_cache = previous.profile_cache;
    internal.archived_arrivals = previous.archived_arrivals;
    n.set_group_sync(internal);
    let old_profiles = profiles(&o);
    let old_arrivals = arrivals(&o);
    for d in &mut n.devices {
        if !o.devices.iter().any(|a| a.device_id == d.device_id) && d.arrival_layout_id.is_none() {
            d.arrival_layout_id = old_arrivals.get(&d.device_id).cloned();
        }
    }

    for d in &n.devices {
        if d.device_id != self_id
            && old_profiles.get(&d.device_id).is_none_or(|a| {
                a.monitors != d.monitors
                    || a.available_builtin_screens != d.available_builtin_screens
            })
        {
            return Err(CoreError::new(
                E_NOT_OWNER,
                format!("profile {} belongs to another device", d.device_id),
            ));
        }
    }
    let bump = |rev: u64| {
        rev.checked_add(1)
            .ok_or_else(|| CoreError::new(E_CONFIG_INVALID, "section revision overflow"))
    };
    let mut sync = o.group_sync();
    let meta = SectionMeta {
        updated_at_ms: now,
        updated_by: self_id.into(),
    };
    let topology_changed = topology(&o) != topology(&n);
    n.revision = if topology_changed {
        sync.topology = meta.clone();
        bump(o.revision)?
    } else {
        o.revision
    };
    let mut retained = n.group_sync();
    for (id, layout) in &old_arrivals {
        if !n.devices.iter().any(|d| &d.device_id == id) {
            retained
                .archived_arrivals
                .insert(id.clone(), layout.clone());
        }
    }
    n.set_group_sync(retained);
    let mixed_changed = mixed(&o, false) != mixed(&n, false);
    if mixed_changed {
        sync.mixed = meta.clone();
        let rev = bump(o.mixed.as_ref().map_or(0, |m| m.revision))?;
        if let Some(m) = &mut n.mixed {
            m.revision = rev;
        } else {
            n.mixed = Some(crate::mixed::MixedConfig {
                revision: rev,
                ..Default::default()
            });
        }
    } else {
        n.mixed = o.mixed.clone();
    }
    for d in &mut n.devices {
        let old = old_profiles.get(&d.device_id);
        let prev = old.map_or(0, |a| a.profile_rev);
        d.profile_rev = if old.is_none_or(|a| {
            a.monitors != d.monitors || a.available_builtin_screens != d.available_builtin_screens
        }) {
            bump(prev)?
        } else {
            prev
        };
    }
    let requested = n.group_sync();
    sync.ports_confirmed = requested.ports_confirmed;
    let mut members = Vec::new();
    let ids: BTreeSet<_> = sync
        .members
        .iter()
        .chain(requested.members.iter())
        .map(|m| m.device_id.clone())
        .collect();
    for id in ids {
        let before = sync.members.iter().find(|m| m.device_id == id);
        let after = requested.members.iter().find(|m| m.device_id == id);
        if before
            .zip(after)
            .is_some_and(|(a, b)| a.public_key != b.public_key)
        {
            return Err(CoreError::new(
                E_LAN_KEY_CHANGED,
                format!("{id} requires explicit re-pairing"),
            ));
        }
        let mut member = after.or(before).unwrap().clone();
        let mut a = before.cloned().unwrap_or_default();
        let mut b = member.clone();
        a.revision = 0;
        a.meta = SectionMeta::default();
        b.revision = 0;
        b.meta = SectionMeta::default();
        if after.is_none() {
            b.removed = true;
            member.removed = true;
        }
        if a != b {
            member.revision = bump(before.map_or(0, |m| m.revision))?;
            member.meta = meta.clone();
        } else if let Some(before) = before {
            member = before.clone();
        }
        members.push(member);
    }
    if sync.members != members && !topology_changed {
        n.revision = bump(o.revision)?;
        sync.topology = meta;
    }
    sync.members = members;
    let active: BTreeSet<_> = n.devices.iter().map(|d| d.device_id.as_str()).collect();
    let revoked: BTreeSet<_> = sync
        .members
        .iter()
        .filter(|m| m.removed)
        .map(|m| m.device_id.as_str())
        .collect();
    sync.profile_cache = old_profiles
        .into_iter()
        .filter(|(id, _)| !active.contains(id.as_str()) && !revoked.contains(id.as_str()))
        .collect();
    sync.archived_arrivals = old_arrivals
        .into_iter()
        .filter(|(id, _)| !active.contains(id.as_str()) && !revoked.contains(id.as_str()))
        .collect();
    n.set_group_sync(sync);
    normalize_snapshot(&mut n);
    n.validate()
        .map_err(|e| CoreError::new(E_CONFIG_INVALID, e.to_string()))?;
    let changes = changed_between(&o, &n);
    Ok((n, changes))
}
