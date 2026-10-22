//! Stable random identities. Entropy is supplied by the sidecar, never generated here.
use crate::layouts::{LayoutSource, LayoutTarget};
use crate::multidevice::SwitchGroupSnapshot;

fn encode(prefix: &str, random: [u8; 16]) -> String {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut n = u128::from_be_bytes(random);
    let mut result = [b'0'; 26];
    for c in result.iter_mut().rev() {
        *c = ALPHABET[(n & 31) as usize];
        n >>= 5;
    }
    format!("{prefix}-{}", std::str::from_utf8(&result).unwrap())
}
pub fn new_device_id(random: [u8; 16]) -> String {
    encode("dev", random)
}
pub fn new_group_id(random: [u8; 16]) -> String {
    encode("grp", random)
}

/// Rewrites identity references and generated layout IDs, preserving custom IDs.
/// Caller must ensure `new` is not already a different device in this group.
pub fn rename_device_id(s: &mut SwitchGroupSnapshot, old: &str, new: &str) {
    if old == new {
        return;
    }
    let rename = |id: &mut String| {
        if id == old {
            *id = new.into();
        }
    };
    let layout_id = |id: &mut String| {
        for prefix in ["builtin:", "migrated:"] {
            let base = format!("{prefix}{old}");
            if id == &base
                || id
                    .strip_prefix(&base)
                    .is_some_and(|tail| tail.starts_with(':'))
            {
                *id = format!("{prefix}{new}{}", &id[base.len()..]);
                break;
            }
        }
    };
    for d in &mut s.devices {
        rename(&mut d.device_id);
        if let Some(id) = &mut d.arrival_layout_id {
            layout_id(id);
        }
    }
    for l in &mut s.layouts {
        layout_id(&mut l.id);
        match &mut l.source {
            LayoutSource::MigratedFollow { device_id } | LayoutSource::Builtin { device_id } => {
                rename(device_id)
            }
            _ => {}
        }
        for m in &mut l.monitors {
            if let LayoutTarget::Device { device_id } = &mut m.target {
                rename(device_id);
            }
        }
    }
    if let Some(m) = &mut s.mixed {
        for a in &mut m.assignment {
            rename(&mut a.device_id);
        }
    }
    // Preserve unknown metadata verbatim; update known authors and trust references.
    for section in ["topology", "mixed"] {
        if let Some(by) = s
            .sync
            .get_mut(section)
            .and_then(|v| v.get_mut("updated_by"))
        {
            if by.as_str() == Some(old) {
                *by = new.into();
            }
        }
    }
    if let Some(members) = s.sync.get_mut("members").and_then(|v| v.as_array_mut()) {
        for member in members {
            for key in ["device_id", "introduced_by"] {
                if member[key].as_str() == Some(old) {
                    member[key] = new.into();
                }
            }
            if member["meta"]["updated_by"].as_str() == Some(old) {
                member["meta"]["updated_by"] = new.into();
            }
        }
    }
    for field in ["profile_cache", "archived_arrivals"] {
        if let Some(entries) = s.sync.get_mut(field).and_then(|v| v.as_object_mut()) {
            if let Some(value) = entries.remove(old) {
                entries.insert(new.into(), value);
            }
            if field == "archived_arrivals" {
                for value in entries.values_mut() {
                    if let Some(id) = value.as_str() {
                        let mut id = id.to_owned();
                        layout_id(&mut id);
                        *value = id.into();
                    }
                }
            }
        }
    }
}

/// Pending IDs encode the target and section author: regenerate after renaming.
/// Runtime normally bootstraps identity before it creates any pending work.
pub fn rename_pending_remote(
    pending: &mut [crate::mixed::PendingRemote],
    group: &SwitchGroupSnapshot,
    old: &str,
    new: &str,
) {
    let meta = group.group_sync().mixed;
    for p in pending {
        if p.device_id.as_deref() == Some(old) {
            p.device_id = Some(new.into());
        }
        if p.target_device_id == old {
            p.target_device_id = new.into();
        }
        p.id = crate::mixed::pending_remote_id_with_meta(
            &group.group_id,
            group.mixed.as_ref().map_or(0, |m| m.revision),
            meta.updated_at_ms,
            &meta.updated_by,
            &p.fingerprint,
            &p.target_device_id,
        );
    }
}
