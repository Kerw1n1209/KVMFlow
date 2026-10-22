//! Platform contract for removing a display from the desktop ("detach") and
//! putting it back. Adapters only perform system calls and capture the state
//! needed to undo them; the safety rules, persistence and crash recovery live
//! in `crate::topology`, so every platform gets them identically.

use kvmflow_core::errors::CoreError;
use serde::{Deserialize, Serialize};

/// Origin of an enumerated identity. Record hints and CG fallbacks are not
/// independent evidence that a monitor is the original DDC-addressed device.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FingerprintSource {
    Ddc,
    Fallback,
    RecordHint,
    #[default]
    Unknown,
}

/// One display as the operating system currently reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysDisplay {
    /// Platform handle, only meaningful on this host (`cg:2`, `win:0:1:4357`).
    pub key: String,
    /// The same identity the DDC enumeration produces (`edid_id`).
    pub fingerprint: String,
    pub fingerprint_source: FingerprintSource,
    pub builtin: bool,
    /// Part of the desktop right now: windows and the pointer can reach it.
    pub active: bool,
    pub is_main: bool,
    /// Physically connected and usable. Detached monitors are present but not
    /// active; unplugged ones are absent; a closed-lid built-in panel is
    /// reported present but inactive and is filtered out by the manager.
    pub present: bool,
    /// Hardware identity that does not depend on the session-local `key`
    /// (macOS vendor:model:serial, Windows monitor device path). Used to find
    /// the same monitor again when keys may have been reassigned. Empty when
    /// the platform cannot tell.
    pub identity: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetachMethod {
    /// Recorded before the system call; the adapter has not reported a result.
    Pending,
    /// The display was taken out of the desktop (macOS `CGSConfigureDisplayEnabled`,
    /// Windows path removal).
    Disabled,
    /// macOS fallback: the display mirrors a display that stays.
    Mirrored,
}

/// Position of a display in the global desktop, saved so a moved main display
/// can be put back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginSave {
    pub key: String,
    /// Identity of the display at that key when saved; the origin is only put
    /// back if the same display still holds the key.
    #[serde(default)]
    pub identity: String,
    pub x: i32,
    pub y: i32,
}

/// Everything needed to undo one detach, persisted before the system is touched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetachRecord {
    pub fingerprint: String,
    #[serde(default)]
    pub fingerprint_source: FingerprintSource,
    /// Platform handle at detach time. May be stale after a reboot or replug;
    /// resolve it against `identity` before use.
    pub key: String,
    #[serde(default)]
    pub identity: String,
    pub method: DetachMethod,
    /// macOS: the display this one mirrors (`Mirrored` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror_of: Option<String>,
    /// macOS: origins before the main display was moved. Non-empty only when
    /// the detached display was the main one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub origins: Vec<OriginSave>,
    /// Windows: the full active topology (hex of the PATH/MODE arrays).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topology: Option<String>,
    pub detached_at_ms: u64,
}

pub trait PlatformTopology: Send {
    /// Online displays. `hints` are the pending detach records: a platform
    /// that cannot see detached monitors by itself reports them (with their
    /// `present` state) so the manager can tell unplugged from detached.
    fn enumerate(&mut self, hints: &[DetachRecord]) -> Result<Vec<SysDisplay>, CoreError>;

    /// Read-only: capture what `detach` will change, without changing it.
    fn capture(
        &mut self,
        target: &SysDisplay,
        all: &[SysDisplay],
    ) -> Result<DetachRecord, CoreError>;

    /// Remove `record.key` from the desktop. When it is the main display the
    /// adapter first makes `reserve` the main one. Returns the method used.
    fn detach(
        &mut self,
        record: &DetachRecord,
        reserve: &SysDisplay,
    ) -> Result<DetachMethod, CoreError>;

    /// Undo `detach` for this display only. Must be idempotent: running it
    /// for a display that is already back is a success. `allow_extend` says no
    /// other display is detached, so an emergency "extend everything" is fine.
    fn attach(&mut self, record: &DetachRecord, allow_extend: bool) -> Result<(), CoreError>;

    /// Bounded wait until the restored displays can be addressed by DDC (they
    /// appear in the DDC enumeration with these fingerprints).
    fn wait_ready(&mut self, _fingerprints: &[String]) -> Result<(), CoreError> {
        Ok(())
    }
}

/// EDID manufacturer id as CoreGraphics reports it (`CGDisplayVendorNumber`):
/// three 5-bit letters, `A` = 1. `0x10ac` -> `DEL`. `None` when it does not
/// decode to three letters.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn pnp_id_from_vendor(vendor: u32) -> Option<String> {
    if vendor == 0 || vendor > 0xFFFF {
        return None;
    }
    let letter = |v: u32| match v & 0x1f {
        n @ 1..=26 => Some((b'A' + (n as u8) - 1) as char),
        _ => None,
    };
    Some(
        [letter(vendor >> 10)?, letter(vendor >> 5)?, letter(vendor)?]
            .iter()
            .collect(),
    )
}

/// Windows `DISPLAYCONFIG_OUTPUT_TECHNOLOGY` values that denote the machine's
/// own panel: INTERNAL, LVDS, DISPLAYPORT_EMBEDDED, UDI_EMBEDDED.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn is_internal_output_technology(tech: u32) -> bool {
    matches!(tech, 0x8000_0000 | 6 | 11 | 13)
}

/// `modeInfoIdx` value meaning "no mode entry".
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub const INVALID_MODE_INDEX: u32 = 0xFFFF_FFFF;

/// After dropping a path, keep only the mode entries the remaining paths still
/// reference. `refs` holds each remaining path's (source, target) mode index;
/// the result holds the rewritten indices and the old mode indices to keep, in
/// new order. Entries are shared between paths in a clone group, so a mode is
/// kept once.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn compact_mode_indices(refs: &[[u32; 2]], mode_count: usize) -> (Vec<[u32; 2]>, Vec<usize>) {
    let mut map: Vec<Option<u32>> = vec![None; mode_count];
    let mut keep: Vec<usize> = Vec::new();
    let mut out = Vec::with_capacity(refs.len());
    for pair in refs {
        let mut mapped = [INVALID_MODE_INDEX; 2];
        for (slot, &idx) in pair.iter().enumerate() {
            if idx == INVALID_MODE_INDEX || idx as usize >= mode_count {
                mapped[slot] = idx;
                continue;
            }
            let entry = &mut map[idx as usize];
            let new = *entry.get_or_insert_with(|| {
                keep.push(idx as usize);
                (keep.len() - 1) as u32
            });
            mapped[slot] = new;
        }
        out.push(mapped);
    }
    (out, keep)
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 15) as usize] as char);
    }
    s
}

#[cfg(test)]
pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    s.as_bytes()
        .chunks(2)
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16)?;
            let lo = (pair[1] as char).to_digit(16)?;
            Some((hi * 16 + lo) as u8)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pnp_id_decodes_cg_vendor_numbers() {
        assert_eq!(pnp_id_from_vendor(0x10ac).as_deref(), Some("DEL"));
        assert_eq!(pnp_id_from_vendor(0x0610).as_deref(), Some("APP"));
        assert_eq!(pnp_id_from_vendor(0x4c2d).as_deref(), Some("SAM"));
        assert_eq!(pnp_id_from_vendor(0), None);
        assert_eq!(pnp_id_from_vendor(0x1_0000), None);
    }

    #[test]
    fn internal_output_technologies() {
        assert!(is_internal_output_technology(0x8000_0000));
        assert!(is_internal_output_technology(6));
        assert!(is_internal_output_technology(11));
        assert!(is_internal_output_technology(13));
        // HDMI, DisplayPort external, UDI external, Miracast
        for external in [5, 10, 12, 15] {
            assert!(!is_internal_output_technology(external));
        }
    }

    #[test]
    fn compact_drops_unreferenced_modes_and_shares_entries() {
        // Paths: A (src 0, tgt 1), B (src 2, tgt 3), C (clone of A's source: src 0, tgt 4).
        // Removing B leaves modes 0, 1, 4 referenced.
        let refs = [[0, 1], [0, 4]];
        let (out, keep) = compact_mode_indices(&refs, 5);
        assert_eq!(keep, vec![0, 1, 4]);
        assert_eq!(out, vec![[0, 1], [0, 2]]);
    }

    #[test]
    fn compact_preserves_invalid_indices() {
        let refs = [[INVALID_MODE_INDEX, 7], [3, INVALID_MODE_INDEX]];
        let (out, keep) = compact_mode_indices(&refs, 4);
        // 7 is out of range, so it passes through untouched.
        assert_eq!(out, vec![[INVALID_MODE_INDEX, 7], [0, INVALID_MODE_INDEX]]);
        assert_eq!(keep, vec![3]);
    }

    #[test]
    fn hex_round_trips() {
        let bytes: Vec<u8> = (0..=255).collect();
        assert_eq!(hex_decode(&hex_encode(&bytes)).unwrap(), bytes);
        assert!(hex_decode("abc").is_none());
        assert!(hex_decode("zz").is_none());
    }

    #[test]
    fn record_json_round_trips_with_optional_fields_absent() {
        let record = DetachRecord {
            fingerprint_source: FingerprintSource::Ddc,
            fingerprint: "DEL-A261-0000BEEF".into(),
            key: "cg:2".into(),
            identity: "10ac:a261:0000beef".into(),
            method: DetachMethod::Mirrored,
            mirror_of: Some("cg:1".into()),
            origins: vec![],
            topology: None,
            detached_at_ms: 5,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("origins") && !json.contains("topology"));
        assert_eq!(serde_json::from_str::<DetachRecord>(&json).unwrap(), record);
    }
}

/// Restore exactly one target using current paths and an unused source.
/// Never replay saved paths: other monitors may still be intentionally off.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn restore_paths<T: Clone>(
    current: &[T],
    possible: &[T],
    target: &str,
    key: impl Fn(&T) -> String,
    source: impl Fn(&T) -> String,
) -> Result<Vec<T>, CoreError> {
    if current.iter().any(|p| key(p) == target) {
        return Ok(current.to_vec());
    }
    let candidate = possible
        .iter()
        .find(|p| key(p) == target && !current.iter().any(|active| source(active) == source(p)))
        .ok_or_else(|| {
            CoreError::new(
                kvmflow_core::errors::E_TOPOLOGY_FAILED,
                "no independent source for the requested target; record kept",
            )
        })?;
    let mut paths = current.to_vec();
    paths.push(candidate.clone());
    Ok(paths)
}

#[cfg(test)]
mod restore_tests {
    use super::*;
    #[test]
    fn restore_one_target_preserves_live_changes_and_other_detaches() {
        let current = vec![("C", "source-new")];
        // A and B were both detached. Only A is requested; C has since moved.
        let possible = vec![("A", "source-new"), ("A", "source-free"), ("B", "source-b")];
        let restored =
            restore_paths(&current, &possible, "A", |p| p.0.into(), |p| p.1.into()).unwrap();
        assert_eq!(restored, [("C", "source-new"), ("A", "source-free")]);
        assert!(restore_paths(
            &current,
            &possible[..1],
            "A",
            |p| p.0.into(),
            |p| p.1.into()
        )
        .is_err());
        assert_eq!(
            restore_paths(&restored, &possible, "A", |p| p.0.into(), |p| p.1.into()).unwrap(),
            restored
        );
    }
}
