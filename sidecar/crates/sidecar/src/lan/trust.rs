use super::{LanError, Result};
use std::{collections::HashMap, sync::RwLock};
use subtle::ConstantTimeEq;

/// S6/S7 may replace this with a runtime-owned membership snapshot. Never trust
/// mDNS TXT keys, names or IDs for authentication. Tombstones return NOT_MEMBER.
pub trait MemberStore: Send + Sync {
    /// Retention hint only; never authenticates a discovered identity.
    fn contains(&self, _device_id: &str) -> bool {
        false
    }
    /// Cheap change identity of the trusted set. The supervisor polls it to
    /// reset dial backoff after pairing commits or relayed trust updates.
    fn fingerprint(&self) -> usize {
        0
    }
    fn check(&self, device_id: &str, public_key: &[u8; 32]) -> Result<()>;
}
/// Canonical wire representation for core membership and pairing snapshots.
pub fn public_key_string(key: &[u8; 32]) -> String {
    key.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn parse_public_key(value: &str) -> Result<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(LanError::new(
            "E_LAN_KEY_CHANGED",
            "invalid pinned public key",
        ));
    }
    let mut key = [0; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| LanError::new("E_LAN_KEY_CHANGED", "invalid pinned public key"))?;
    }
    Ok(key)
}

/// Read the CURRENT core membership on every check, including queued work checks.
/// The runtime owner replaces this snapshot after a trusted membership edit.
impl MemberStore for RwLock<Vec<kvmflow_core::sync::Member>> {
    fn contains(&self, id: &str) -> bool {
        self.read()
            .unwrap()
            .iter()
            .any(|m| m.device_id == id && !m.removed)
    }
    fn fingerprint(&self) -> usize {
        use std::hash::Hash;
        let members = self.read().unwrap();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for m in members.iter() {
            m.device_id.hash(&mut hasher);
            m.public_key.hash(&mut hasher);
            m.removed.hash(&mut hasher);
        }
        std::hash::Hasher::finish(&hasher) as usize
    }
    fn check(&self, id: &str, key: &[u8; 32]) -> Result<()> {
        let members = self.read().unwrap();
        let mut matches = members.iter().filter(|m| m.device_id == id);
        let member = matches
            .next()
            .filter(|m| !m.removed)
            .ok_or_else(|| LanError::new("E_LAN_NOT_MEMBER", "peer is not a current member"))?;
        if matches.next().is_some() {
            return Err(LanError::new(
                "E_LAN_NOT_MEMBER",
                "ambiguous member identity",
            ));
        }
        let pinned = parse_public_key(&member.public_key)?;
        if !bool::from(pinned.ct_eq(key)) {
            return Err(LanError::new(
                "E_LAN_KEY_CHANGED",
                "peer security key changed; pair again",
            ));
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct MemoryMembers(RwLock<HashMap<String, [u8; 32]>>);
impl MemoryMembers {
    /// Call only after BOTH pairing approvals or a trusted membership update.
    pub fn insert(&self, id: impl Into<String>, key: [u8; 32]) {
        self.0.write().unwrap().insert(id.into(), key);
    }
    pub fn remove(&self, id: &str) {
        self.0.write().unwrap().remove(id);
    }
}
impl MemberStore for MemoryMembers {
    fn contains(&self, id: &str) -> bool {
        self.0.read().unwrap().contains_key(id)
    }
    fn check(&self, id: &str, key: &[u8; 32]) -> Result<()> {
        match self.0.read().unwrap().get(id) {
            None => Err(LanError::new(
                "E_LAN_NOT_MEMBER",
                "peer is not a current member",
            )),
            Some(expected) if !bool::from(expected.ct_eq(key)) => Err(LanError::new(
                "E_LAN_KEY_CHANGED",
                "peer security key changed; pair again",
            )),
            Some(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn core_member_pins_roundtrip_and_fail_closed() {
        use kvmflow_core::sync::Member;
        let key = [0xa5; 32];
        let encoded = public_key_string(&key);
        assert_eq!(parse_public_key(&encoded).unwrap(), key);
        assert_eq!(parse_public_key(&encoded.to_uppercase()).unwrap(), key);
        for invalid in [
            "".to_owned(),
            "a".repeat(63),
            "a".repeat(65),
            "é".repeat(32),
            "z".repeat(64),
        ] {
            assert!(parse_public_key(&invalid).is_err());
        }
        let members = RwLock::new(vec![Member {
            device_id: "peer".into(),
            public_key: encoded,
            ..Default::default()
        }]);
        members.check("peer", &key).unwrap();
        assert_eq!(
            members.check("peer", &[0; 32]).unwrap_err().code,
            "E_LAN_KEY_CHANGED"
        );
        members.write().unwrap()[0].public_key = "bad".into();
        assert!(members.check("peer", &key).is_err());
        members.write().unwrap()[0].public_key = public_key_string(&key);
        let duplicate = members.read().unwrap()[0].clone();
        members.write().unwrap().push(duplicate);
        assert!(members.check("peer", &key).is_err());
        members.write().unwrap().pop();
        members.write().unwrap()[0].removed = true;
        assert!(!members.contains("peer"));
        assert_eq!(
            members.check("peer", &key).unwrap_err().code,
            "E_LAN_NOT_MEMBER"
        );
    }

    #[test]
    fn pinned_key_comparison_rejects_every_changed_byte() {
        let members = MemoryMembers::default();
        let key = [0xa5; 32];
        members.insert("peer", key);
        members.check("peer", &key).unwrap();
        for index in 0..32 {
            let mut changed = key;
            changed[index] ^= 1;
            assert_eq!(
                members.check("peer", &changed).unwrap_err().code,
                "E_LAN_KEY_CHANGED"
            );
        }
    }
}
