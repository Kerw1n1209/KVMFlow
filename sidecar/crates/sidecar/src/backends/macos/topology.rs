//! macOS display detach. Primary route: the private
//! `CGSConfigureDisplayEnabled`, resolved with dlsym at run time (a missing
//! symbol is a clean fallback, never a crash). Fallback: mirror the display to
//! one that stays, with public CoreGraphics. The main display is moved to the
//! kept display first. Every change is `forSession`, so logging out always
//! brings the displays back (verified on Darwin 25.6 arm64 by the
//! display-topology probe).
//!
//! Hardware facts (Darwin 25.6 arm64): a CGS-disabled display leaves
//! `CGGetOnlineDisplayList` even though its id stays configurable (a manual
//! `CGSConfigureDisplayEnabled(id, true)` on the recorded id re-enabled it),
//! and `CGDisplayIsActive` answers 0 for it, -1 for an unknown id. Whether an
//! offline disabled id still answers the vendor/model/serial query is not yet
//! observed; `enumerate` probes it and treats an unreadable id as absent.

use super::ddc::{edid_of, parse_display_list_detailed, resolve_m1ddc, run_m1ddc};
use crate::backends::topology::{
    pnp_id_from_vendor, DetachMethod, DetachRecord, FingerprintSource, OriginSave,
    PlatformTopology, SysDisplay,
};
use kvmflow_core::edid_identity;
use kvmflow_core::errors::{
    CoreError, E_TOPOLOGY_FAILED, E_TOPOLOGY_NOT_FOUND, E_TOPOLOGY_UNSUPPORTED,
};
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CString};
use std::path::PathBuf;
use std::time::{Duration, Instant};

type DisplayId = u32;
type ConfigRef = *mut c_void;
type ConfigureDisplayEnabled = unsafe extern "C" fn(ConfigRef, DisplayId, bool) -> i32;

const CONFIGURE_FOR_SESSION: u32 = 1;
const SETTLE_TIMEOUT: Duration = Duration::from_secs(8);

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGGetOnlineDisplayList(max: u32, displays: *mut DisplayId, count: *mut u32) -> i32;
    fn CGDisplayIsActive(id: DisplayId) -> u32;
    fn CGDisplayIsBuiltin(id: DisplayId) -> u32;
    fn CGDisplayIsMain(id: DisplayId) -> u32;
    fn CGDisplayMirrorsDisplay(id: DisplayId) -> DisplayId;
    fn CGDisplayVendorNumber(id: DisplayId) -> u32;
    fn CGDisplayModelNumber(id: DisplayId) -> u32;
    fn CGDisplaySerialNumber(id: DisplayId) -> u32;
    fn CGDisplayBounds(id: DisplayId) -> CGRect;
    fn CGBeginDisplayConfiguration(config: *mut ConfigRef) -> i32;
    fn CGCompleteDisplayConfiguration(config: ConfigRef, option: u32) -> i32;
    fn CGCancelDisplayConfiguration(config: ConfigRef) -> i32;
    fn CGConfigureDisplayMirrorOfDisplay(
        config: ConfigRef,
        display: DisplayId,
        master: DisplayId,
    ) -> i32;
    fn CGConfigureDisplayOrigin(config: ConfigRef, display: DisplayId, x: i32, y: i32) -> i32;
}

extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
}

const RTLD_LAZY: i32 = 1;
/// `RTLD_DEFAULT` on Darwin.
const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

/// CoreGraphics re-exports SkyLight, so the default lookup normally finds the
/// symbol; SkyLight is opened explicitly as a fallback.
fn resolve_configure_display_enabled() -> Option<ConfigureDisplayEnabled> {
    let name = CString::new("CGSConfigureDisplayEnabled").ok()?;
    let mut ptr = unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) };
    if ptr.is_null() {
        let path =
            CString::new("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight").ok()?;
        let handle = unsafe { dlopen(path.as_ptr(), RTLD_LAZY) };
        if !handle.is_null() {
            ptr = unsafe { dlsym(handle, name.as_ptr()) };
        }
    }
    // SAFETY: the symbol has this C signature (verified by the probe).
    (!ptr.is_null())
        .then(|| unsafe { std::mem::transmute::<*mut c_void, ConfigureDisplayEnabled>(ptr) })
}

fn key_of(id: DisplayId) -> String {
    format!("cg:{id}")
}

fn id_of(key: &str) -> Result<DisplayId, CoreError> {
    key.strip_prefix("cg:")
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| CoreError::new(E_TOPOLOGY_FAILED, format!("not a macOS display key: {key}")))
}

fn cg_error(what: &str, rc: i32) -> CoreError {
    CoreError::new(E_TOPOLOGY_FAILED, format!("{what}: CGError {rc}"))
}

fn online_ids() -> Result<Vec<DisplayId>, CoreError> {
    let mut ids = [0u32; 32];
    let mut count = 0u32;
    let rc = unsafe { CGGetOnlineDisplayList(32, ids.as_mut_ptr(), &mut count) };
    if rc != 0 {
        return Err(cg_error("CGGetOnlineDisplayList", rc));
    }
    Ok(ids[..(count as usize).min(32)].to_vec())
}

fn is_active(id: DisplayId) -> bool {
    // An unknown id answers -1; only 0 means a known, inactive display.
    active_from_raw(unsafe { CGDisplayIsActive(id) } as i32)
}

/// `CGDisplayIsActive` truthiness: 1 = active, 0 = known-inactive, -1 (and
/// other negatives) = the id is not a display this process can ask about.
pub(super) fn active_from_raw(raw: i32) -> bool {
    raw == 1
}

fn mirrors(id: DisplayId) -> DisplayId {
    unsafe { CGDisplayMirrorsDisplay(id) }
}

fn origin_of(id: DisplayId) -> (i32, i32) {
    let b = unsafe { CGDisplayBounds(id) };
    (b.origin.x as i32, b.origin.y as i32)
}

fn current_identity(id: DisplayId) -> String {
    unsafe {
        identity_from_cg(
            CGDisplayVendorNumber(id),
            CGDisplayModelNumber(id),
            CGDisplaySerialNumber(id),
        )
    }
}

/// The raw identity triple, or `None` when CoreGraphics cannot answer for this
/// id: never-assigned or freed ids report all-ones, some stale ids report all
/// zeros (both observed read-only on Darwin 25.6 arm64). Only a real triple
/// identifies hardware.
fn identity_triple(id: DisplayId) -> Option<(u32, u32, u32)> {
    unsafe {
        identity_triple_values(
            CGDisplayVendorNumber(id),
            CGDisplayModelNumber(id),
            CGDisplaySerialNumber(id),
        )
    }
}

pub(super) fn identity_triple_values(
    vendor: u32,
    model: u32,
    serial: u32,
) -> Option<(u32, u32, u32)> {
    (vendor != u32::MAX && !(vendor == 0 && model == 0 && serial == 0))
        .then_some((vendor, model, serial))
}

/// A disabled display keeps a CG id that CoreGraphics still knows: it answers
/// the identity query and reports inactive. An id with no readable identity
/// is treated as absent, so an unplugged monitor is never "restored" into a
/// stale id.
fn disabled_display(id: DisplayId) -> Option<(u32, u32, u32)> {
    (!is_active(id) && !online_ids().unwrap_or_default().contains(&id))
        .then_some(identity_triple(id)?)
}

/// Pure core of the `enumerate` record synthesis (unit-tested without
/// CoreGraphics): given the online ids and a probe for an offline id's
/// identity triple, decide whether a record's display is CGS-disabled but
/// still plugged in, and what `SysDisplay` represents it. `None` means the
/// id is not usable: unreadable identity (unplugged or freed) or identity
/// that belongs to other hardware than the record.
pub(super) fn disabled_hint_display(
    online: &[DisplayId],
    id: DisplayId,
    triple: Option<(u32, u32, u32)>,
    builtin: bool,
    record: &DetachRecord,
) -> Option<SysDisplay> {
    if online.contains(&id) {
        return None; // listed displays come from the online scan, not hints
    }
    let (v, m, s) = triple?;
    if !record.identity.is_empty() && identity_from_cg(v, m, s) != record.identity {
        return None; // id now belongs to different hardware; never resurface it
    }
    Some(SysDisplay {
        fingerprint: record.fingerprint.clone(),
        fingerprint_source: FingerprintSource::RecordHint,
        key: record.key.clone(),
        identity: identity_from_cg(v, m, s),
        present: true,
        builtin,
        active: false,
        is_main: false,
    })
}

/// Pure decision of `attach`'s identity gate: refuse only when the id is
/// offline (not in the online list) but still answers the identity query with
/// a triple that belongs to different hardware than the record. An offline id
/// with an unreadable identity is not evidence of a replacement.
pub(super) fn offline_identity_conflict(
    listed: bool,
    triple: Option<(u32, u32, u32)>,
    record_identity: &str,
) -> bool {
    match (listed, triple) {
        (false, Some((v, m, s))) => {
            !record_identity.is_empty() && identity_from_cg(v, m, s) != record_identity
        }
        _ => false,
    }
}

fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < SETTLE_TIMEOUT {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    cond()
}

/// One CoreGraphics configuration transaction, applied for this login session
/// only. `build` fills it; a failing step cancels it.
fn configure(build: impl FnOnce(ConfigRef) -> i32) -> Result<(), CoreError> {
    let mut cfg: ConfigRef = std::ptr::null_mut();
    let rc = unsafe { CGBeginDisplayConfiguration(&mut cfg) };
    if rc != 0 {
        return Err(cg_error("CGBeginDisplayConfiguration", rc));
    }
    let rc = build(cfg);
    if rc != 0 {
        unsafe { CGCancelDisplayConfiguration(cfg) };
        return Err(cg_error("display configuration step", rc));
    }
    let rc = unsafe { CGCompleteDisplayConfiguration(cfg, CONFIGURE_FOR_SESSION) };
    if rc != 0 {
        return Err(cg_error("CGCompleteDisplayConfiguration", rc));
    }
    Ok(())
}

fn set_enabled(f: ConfigureDisplayEnabled, id: DisplayId, enabled: bool) -> Result<(), CoreError> {
    configure(|cfg| unsafe { f(cfg, id, enabled) })
}

fn set_mirror(id: DisplayId, of: DisplayId) -> Result<(), CoreError> {
    configure(|cfg| unsafe { CGConfigureDisplayMirrorOfDisplay(cfg, id, of) })
}

fn set_origins(origins: &[(DisplayId, i32, i32)]) -> Result<(), CoreError> {
    configure(|cfg| {
        for &(id, x, y) in origins {
            let rc = unsafe { CGConfigureDisplayOrigin(cfg, id, x, y) };
            if rc != 0 {
                return rc;
            }
        }
        0
    })
}

/// New origins that make `new_main` sit at (0, 0) while the others keep their
/// relative layout. Pure; `origins` is (display, x, y).
pub(super) fn origins_with_main(
    origins: &[(DisplayId, i32, i32)],
    new_main: DisplayId,
) -> Option<Vec<(DisplayId, i32, i32)>> {
    let &(_, mx, my) = origins.iter().find(|(id, _, _)| *id == new_main)?;
    Some(
        origins
            .iter()
            .map(|&(id, x, y)| (id, x - mx, y - my))
            .collect(),
    )
}

/// `(cg display id, fingerprint)` from `m1ddc display list detailed` output.
pub(super) fn fingerprints_from_m1ddc(stdout: &str) -> HashMap<DisplayId, String> {
    parse_display_list_detailed(stdout)
        .into_iter()
        .filter(|d| d.cg_id != 0)
        .map(|d| (d.cg_id, edid_of(&d)))
        .collect()
}

/// Identity from CoreGraphics alone, for displays m1ddc does not list (inactive
/// ones) or when m1ddc is unavailable. The alphanumeric serial is not
/// available here, so a monitor that only has that kind of serial can differ
/// from its DDC fingerprint; the manager prefers the recorded fingerprint.
pub(super) fn fingerprint_from_cg(vendor: u32, model: u32, serial: u32) -> String {
    edid_identity(
        &pnp_id_from_vendor(vendor).unwrap_or_default(),
        model as u16,
        serial,
        "",
    )
}

/// Hardware identity independent of the session-local display id.
pub(super) fn identity_from_cg(vendor: u32, model: u32, serial: u32) -> String {
    format!("{vendor:04x}:{model:04x}:{serial:08x}")
}

pub struct MacTopology {
    m1ddc: Option<PathBuf>,
    configure_enabled: Option<ConfigureDisplayEnabled>,
}

impl MacTopology {
    pub fn new(explicit_m1ddc: Option<&str>) -> Self {
        Self {
            m1ddc: resolve_m1ddc(explicit_m1ddc),
            configure_enabled: resolve_configure_display_enabled(),
        }
    }

    fn m1ddc_fingerprints(&self, timeout: Duration) -> HashMap<DisplayId, String> {
        let Some(binary) = &self.m1ddc else {
            return HashMap::new();
        };
        match run_m1ddc(binary, &["display", "list", "detailed"], timeout) {
            Ok((0, out, _)) => fingerprints_from_m1ddc(&out),
            _ => HashMap::new(),
        }
    }

    fn sys_display(id: DisplayId, known: &HashMap<DisplayId, String>) -> SysDisplay {
        let (vendor, model, serial) = unsafe {
            (
                CGDisplayVendorNumber(id),
                CGDisplayModelNumber(id),
                CGDisplaySerialNumber(id),
            )
        };
        let fingerprint = known
            .get(&id)
            .cloned()
            .unwrap_or_else(|| fingerprint_from_cg(vendor, model, serial));
        SysDisplay {
            fingerprint_source: if known.contains_key(&id) {
                crate::backends::topology::FingerprintSource::Ddc
            } else {
                crate::backends::topology::FingerprintSource::Fallback
            },
            key: key_of(id),
            fingerprint,
            identity: identity_from_cg(vendor, model, serial),
            // Disabled and mirrored displays stay online; a closed-lid
            // built-in panel too (the manager filters it).
            present: true,
            builtin: unsafe { CGDisplayIsBuiltin(id) != 0 },
            // A display mirroring another one is not a screen of its own.
            active: independent_desktop(is_active(id), mirrors(id)),
            is_main: unsafe { CGDisplayIsMain(id) == 1 } && is_active(id),
        }
    }
}

fn independent_desktop(active: bool, mirror: DisplayId) -> bool {
    active && mirror == 0
}

impl PlatformTopology for MacTopology {
    fn enumerate(&mut self, hints: &[DetachRecord]) -> Result<Vec<SysDisplay>, CoreError> {
        let known = self.m1ddc_fingerprints(Duration::from_secs(3));
        let online = online_ids()?;
        let mut displays: Vec<SysDisplay> = online
            .iter()
            .copied()
            .map(|id| Self::sys_display(id, &known))
            .collect();
        // A CGS-disabled display leaves the online list even while its monitor
        // stays plugged in (observed on Darwin 25.6 arm64). Re-synthesize it
        // from the record's id so the manager can tell "disabled by us" from
        // "unplugged"; the identity is only trusted when the id still answers
        // the hardware query with the recorded triple.
        for record in hints {
            let Ok(id) = id_of(&record.key) else {
                continue;
            };
            if let Some(d) = disabled_hint_display(
                &online,
                id,
                disabled_display(id),
                unsafe { CGDisplayIsBuiltin(id) == 1 },
                record,
            ) {
                displays.push(d);
            }
        }
        Ok(displays)
    }

    fn capture(
        &mut self,
        target: &SysDisplay,
        all: &[SysDisplay],
    ) -> Result<DetachRecord, CoreError> {
        let origins = if target.is_main {
            all.iter()
                .filter(|d| d.active)
                .filter_map(|d| {
                    let id = id_of(&d.key).ok()?;
                    let (x, y) = origin_of(id);
                    Some(OriginSave {
                        key: d.key.clone(),
                        identity: d.identity.clone(),
                        x,
                        y,
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
        Ok(DetachRecord {
            fingerprint_source: target.fingerprint_source,
            fingerprint: target.fingerprint.clone(),
            key: target.key.clone(),
            identity: target.identity.clone(),
            method: DetachMethod::Pending,
            mirror_of: None,
            origins,
            topology: None,
            detached_at_ms: crate::session::now_ms(),
        })
    }

    fn detach(
        &mut self,
        record: &DetachRecord,
        reserve: &SysDisplay,
    ) -> Result<DetachMethod, CoreError> {
        let target = id_of(&record.key)?;
        if current_identity(target) != record.identity {
            return Err(CoreError::new(E_TOPOLOGY_FAILED, "target identity changed"));
        }
        let keep = id_of(&reserve.key)?;
        if current_identity(keep) != reserve.identity {
            return Err(CoreError::new(
                E_TOPOLOGY_FAILED,
                "reserve display identity changed",
            ));
        }
        if target == keep {
            return Err(CoreError::new(
                E_TOPOLOGY_FAILED,
                "reserve display is the target",
            ));
        }
        if !record.origins.is_empty() {
            let current: Vec<(DisplayId, i32, i32)> = record
                .origins
                .iter()
                .filter_map(|o| Some((id_of(&o.key).ok()?, o.x, o.y)))
                .collect();
            let moved = origins_with_main(&current, keep).ok_or_else(|| {
                CoreError::new(E_TOPOLOGY_FAILED, "reserve display has no saved origin")
            })?;
            set_origins(&moved)?;
        }

        let mut private_error = String::from("CGSConfigureDisplayEnabled not found");
        if let Some(disable) = self.configure_enabled {
            match set_enabled(disable, target, false) {
                Ok(()) if wait_until(|| !is_active(target)) => return Ok(DetachMethod::Disabled),
                Ok(()) => {
                    private_error = "display stayed active after disable".into();
                    let _ = set_enabled(disable, target, true);
                }
                Err(e) => private_error = e.message,
            }
        }
        set_mirror(target, keep).map_err(|e| {
            CoreError::new(
                E_TOPOLOGY_FAILED,
                format!(
                    "disable failed ({private_error}); mirror fallback failed: {}",
                    e.message
                ),
            )
        })?;
        if !wait_until(|| mirrors(target) == keep) {
            return Err(CoreError::new(
                E_TOPOLOGY_FAILED,
                format!("disable failed ({private_error}); display did not start mirroring"),
            ));
        }
        Ok(DetachMethod::Mirrored)
    }

    fn attach(&mut self, record: &DetachRecord, _allow_extend: bool) -> Result<(), CoreError> {
        let id = id_of(&record.key)?;
        let online = online_ids()?;
        let listed = online.contains(&id);
        // Refuse only on readable-but-conflicting identity (an id reused by a
        // replacement monitor). An unreadable id (never assigned or freed)
        // falls through: the enable outcome itself decides, and a wrong guess
        // cannot relabel another screen because resolution already matched
        // this id's live identity before `attach` was called.
        if offline_identity_conflict(listed, identity_triple(id), &record.identity) {
            return Err(CoreError::new(
                E_TOPOLOGY_FAILED,
                "display id now belongs to different hardware; record kept",
            ));
        }
        if listed && mirrors(id) != 0 {
            set_mirror(id, 0)?;
            if !wait_until(|| mirrors(id) == 0) {
                return Err(CoreError::new(
                    E_TOPOLOGY_FAILED,
                    "mirroring was not cancelled",
                ));
            }
        }
        if !(listed && is_active(id)) {
            // Disabled-but-plugged displays left the online list; enable even
            // if it is not listed, then judge by the outcome.
            let enabled = match self.configure_enabled {
                Some(f) => set_enabled(f, id, true),
                None => Err(CoreError::new(
                    E_TOPOLOGY_UNSUPPORTED,
                    "CGSConfigureDisplayEnabled not found",
                )),
            };
            match enabled {
                Ok(())
                    if wait_until(|| {
                        online_ids().is_ok_and(|o| o.contains(&id) && is_active(id))
                    }) => {}
                _ if !online_ids()?.contains(&id) => {
                    return Err(CoreError::new(
                        E_TOPOLOGY_NOT_FOUND,
                        format!("display {} is no longer present", record.key),
                    ))
                }
                Ok(()) => {
                    return Err(CoreError::new(
                        E_TOPOLOGY_FAILED,
                        "display did not come back",
                    ))
                }
                Err(e) => return Err(e),
            }
        }
        if !record.origins.is_empty() {
            // Only displays that still are the same hardware, so ids reused by
            // another monitor are left alone.
            let online = online_ids()?;
            let saved: Vec<(DisplayId, i32, i32)> = record
                .origins
                .iter()
                .filter_map(|o| {
                    if o.identity.is_empty() {
                        return None;
                    }
                    let same: Vec<_> = online
                        .iter()
                        .copied()
                        .filter(|id| current_identity(*id) == o.identity)
                        .collect();
                    match same.as_slice() {
                        [id] if is_active(*id) => Some((*id, o.x, o.y)),
                        _ => None,
                    }
                })
                .collect();
            // The monitor is back but the layout must be verifiable too: a
            // failed origin restore leaves the record for the next attempt.
            if !saved.is_empty() {
                set_origins(&saved)?;
            }
        }
        Ok(())
    }

    fn wait_ready(&mut self, fingerprints: &[String]) -> Result<(), CoreError> {
        self.wait_ready_bounded(fingerprints, SETTLE_TIMEOUT)
    }
}

impl MacTopology {
    fn wait_ready_bounded(
        &self,
        fingerprints: &[String],
        timeout: Duration,
    ) -> Result<(), CoreError> {
        if fingerprints.is_empty() {
            return Ok(());
        }
        if self.m1ddc.is_none() {
            return Err(CoreError::new(
                E_TOPOLOGY_FAILED,
                "m1ddc unavailable; cannot confirm DDC readiness",
            ));
        }
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(CoreError::new(
                    E_TOPOLOGY_FAILED,
                    "DDC enumeration timed out",
                ));
            }
            let known: Vec<String> = self
                .m1ddc_fingerprints(remaining.min(Duration::from_secs(2)))
                .into_values()
                .collect();
            if missing_fingerprints(fingerprints, &known).is_empty() {
                return Ok(());
            }
            std::thread::sleep(
                Duration::from_millis(100).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
}

/// Fingerprints from `wanted` that `listed` does not contain.
pub(super) fn missing_fingerprints(wanted: &[String], listed: &[String]) -> Vec<String> {
    wanted
        .iter()
        .filter(|fp| !listed.contains(fp))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // `m1ddc display list detailed` output (serials and UUIDs anonymized) of a MacBook with one external display.
    const SAMPLE: &str = "\
[1] DELL P2425H (00000000-0000-4000-8000-000000000002)
 - Product name:  DELL P2425H
 - Manufacturer:  DEL
 - AN Serial:     TESTSN1
 - Vendor:        4268 (0x10ac)
 - Model:         41569 (0xa261)
 - Serial:        48879 (0x0000beef)
 - Display ID:    2
[2] (null) (00000000-0000-4000-8000-000000000001)
 - Product name:  (null)
 - Manufacturer:  00-10-fa
 - AN Serial:     (null)
 - Vendor:        1552 (0x0610)
 - Model:         41049 (0xa059)
 - Serial:        51966 (0x0000cafe)
 - Display ID:    1
";

    #[test]
    fn maps_cg_ids_to_ddc_fingerprints() {
        let map = fingerprints_from_m1ddc(SAMPLE);
        assert_eq!(map[&2], "DEL-A261-0000BEEF");
        assert_eq!(map[&1], "UNK-A059-0000CAFE");
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn cg_fallback_matches_ddc_fingerprint_for_serial_numbered_monitors() {
        assert_eq!(
            fingerprint_from_cg(0x10ac, 0xa261, 0x0000beef),
            "DEL-A261-0000BEEF"
        );
        assert_eq!(fingerprint_from_cg(0x10ac, 0xa261, 0), "DEL-A261-NOSERIAL");
    }

    #[test]
    fn main_move_shifts_the_layout_and_keeps_relative_positions() {
        // [builtin 0,0 (main)] [external 1512,0]; external becomes main.
        let moved = origins_with_main(&[(1, 0, 0), (2, 1512, 0)], 2).unwrap();
        assert_eq!(moved, vec![(1, -1512, 0), (2, 0, 0)]);
        assert!(origins_with_main(&[(1, 0, 0)], 9).is_none());
    }

    #[test]
    fn identity_is_independent_of_the_session_display_id() {
        assert_eq!(
            identity_from_cg(0x10ac, 0xa261, 0x0000beef),
            "10ac:a261:0000beef"
        );
    }

    #[test]
    fn readiness_lists_what_is_still_missing() {
        let listed = vec!["A".to_string()];
        assert!(missing_fingerprints(&["A".into()], &listed).is_empty());
        assert_eq!(
            missing_fingerprints(&["A".into(), "B".into()], &listed),
            ["B"]
        );
    }

    #[test]
    fn mirror_fallback_is_not_a_connected_independent_desktop() {
        assert!(independent_desktop(true, 0));
        assert!(!independent_desktop(true, 2));
        assert!(!independent_desktop(false, 0));
    }

    #[test]
    fn ddc_enumeration_has_a_deadline_even_if_the_helper_hangs() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("fake-m1ddc");
        std::fs::write(&binary, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = MacTopology {
            m1ddc: Some(binary),
            configure_enabled: None,
        };
        let started = Instant::now();
        assert!(adapter
            .wait_ready_bounded(&["missing".into()], Duration::from_millis(60))
            .is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn display_keys_round_trip() {
        assert_eq!(id_of(&key_of(77)).unwrap(), 77);
        assert!(id_of("win:1").is_err());
    }

    fn del_record() -> DetachRecord {
        DetachRecord {
            fingerprint_source: FingerprintSource::Ddc,
            fingerprint: "DEL-A261-0000BEEF".into(),
            key: "cg:2".into(),
            identity: "10ac:a261:0000beef".into(),
            method: DetachMethod::Disabled,
            mirror_of: None,
            origins: vec![],
            topology: None,
            detached_at_ms: 0,
        }
    }

    // P0 regression: a CGS-disabled display vanishes from the online list but
    // its id still answers the identity query (observed via the incident's
    // successful manual re-enable on the recorded id).
    #[test]
    fn disabled_display_off_the_online_list_is_resynthesized_from_the_record() {
        let d = disabled_hint_display(
            &[1],
            2,
            Some((0x10ac, 0xa261, 0x0000beef)),
            false,
            &del_record(),
        )
        .expect("disabled but plugged in");
        assert_eq!(d.key, "cg:2");
        assert_eq!(d.identity, "10ac:a261:0000beef");
        assert_eq!(d.fingerprint, "DEL-A261-0000BEEF");
        assert!(d.present && !d.active && !d.builtin && !d.is_main);
    }

    #[test]
    fn disabled_hint_display_rejects_every_unusable_id() {
        let record = del_record();
        // Unplugged/freed id: CoreGraphics answers no readable triple.
        assert!(disabled_hint_display(&[1], 2, None, false, &record).is_none());
        // All-ones (never assigned) and all-zeros triples are not identities.
        assert!(disabled_hint_display(
            &[1],
            2,
            Some((u32::MAX, u32::MAX, u32::MAX)),
            false,
            &record
        )
        .is_none());
        assert!(disabled_hint_display(&[1], 2, Some((0, 0, 0)), false, &record).is_none());
        // Still online: the online scan owns it, hints must not duplicate it.
        assert!(
            disabled_hint_display(&[2], 2, Some((0x10ac, 0xa261, 0x0000beef)), false, &record)
                .is_none()
        );
    }

    // P0 regression: an id whose readable identity now belongs to a
    // replacement monitor must not be resurfaced or restored.
    #[test]
    fn conflicting_offline_identity_is_never_resynthesized() {
        let record = del_record();
        assert!(
            disabled_hint_display(&[1], 2, Some((0x4c2d, 0x0f17, 0x11223344)), false, &record)
                .is_none()
        );
        assert!(offline_identity_conflict(
            false,
            Some((0x4c2d, 0x0f17, 0x11223344)),
            &record.identity
        ));
        // Matching identity is fine, and an unreadable id is not evidence of
        // a replacement: no conflict is claimed.
        assert!(!offline_identity_conflict(
            false,
            Some((0x10ac, 0xa261, 0x0000beef)),
            &record.identity
        ));
        assert!(!offline_identity_conflict(false, None, &record.identity));
        assert!(!offline_identity_conflict(
            true,
            Some((0x4c2d, 0x0f17, 0x11223344)),
            &record.identity
        ));
    }

    #[test]
    fn identity_triple_rejects_null_values() {
        // Observed read-only on Darwin 25.6: unknown ids answer all-ones,
        // some stale ids all-zeros.
        assert_eq!(identity_triple_values(u32::MAX, u32::MAX, u32::MAX), None);
        assert_eq!(identity_triple_values(0, 0, 0), None);
        assert_eq!(
            identity_triple_values(0x10ac, 0xa261, 0x0000beef),
            Some((0x10ac, 0xa261, 0x0000beef))
        );
        // Vendor with a zero serial alone is still readable hardware data.
        assert_eq!(identity_triple_values(0x10ac, 0, 0), Some((0x10ac, 0, 0)));
    }

    #[test]
    fn is_active_rejects_the_unknown_id_sentinel() {
        // Unknown ids answer -1; the truthy interpretation was a hazard.
        assert!(!active_from_raw(-1));
        assert!(!active_from_raw(0));
        assert!(active_from_raw(1));
    }

    /// Read-only: lists the real displays through the adapter. Safe to run.
    /// `cargo test -p kvmflow-sidecar -- --ignored macos_enumerate_real_displays --nocapture`
    #[test]
    #[ignore = "queries the real display system"]
    fn macos_enumerate_real_displays() {
        let mut t = MacTopology::new(None);
        let displays = t.enumerate(&[]).unwrap();
        println!(
            "symbol found: {}",
            resolve_configure_display_enabled().is_some()
        );
        for d in &displays {
            println!("{d:?}");
        }
        assert!(displays.iter().any(|d| d.active));
    }

    /// DESTRUCTIVE: really detaches `KVMFLOW_TEST_DETACH_FINGERPRINT` for ~5 s
    /// and restores it. Run only from the manual steps in the task report.
    #[test]
    #[ignore = "detaches a real display"]
    fn macos_detach_and_restore_real_display() {
        use crate::detach::TopologyManager;
        use crate::topology::DisplayTopology;
        let fp = std::env::var("KVMFLOW_TEST_DETACH_FINGERPRINT")
            .expect("set KVMFLOW_TEST_DETACH_FINGERPRINT to a fingerprint from the enumerate test");
        let dir = tempfile::tempdir().unwrap();
        let records = dir.path().join(crate::detach::RECORDS_FILE);
        println!("Recovery record: {}", records.display());
        let mut platform = MacTopology::new(None);
        // Only this explicitly ignored hardware test supports forced fallback.
        if std::env::var("KVMFLOW_TEST_MIRROR").as_deref() == Ok("1") {
            platform.configure_enabled = None;
        }
        let mut m = TopologyManager::new(platform, records.clone(), true);
        let outcome = m.detach(&fp);
        println!("detach: {outcome:?}");
        outcome.unwrap();
        std::thread::sleep(Duration::from_secs(5));
        let restored = m.restore_all();
        println!("restore: {restored:?}");
        if restored.is_err() {
            // Full diagnostics: every step the recovery depends on.
            let online = online_ids().unwrap_or_default();
            println!("diagnostics: online_ids={online:?}");
            for id in &online {
                println!(
                    "diagnostics: cg:{id} active={} identity={} mirrors={}",
                    is_active(*id),
                    current_identity(*id),
                    mirrors(*id)
                );
            }
            for r in m.pending_records() {
                let Ok(id) = id_of(&r.key) else {
                    println!("diagnostics: record key {} is not a cg id", r.key);
                    continue;
                };
                println!(
                    "diagnostics: record {} key cg:{id} recorded_identity={} live_identity={} \
                     identity_readable={} listed_online={} active={}",
                    r.fingerprint,
                    r.identity,
                    current_identity(id),
                    identity_triple(id).is_some(),
                    online.contains(&id),
                    is_active(id),
                );
            }
            println!(
                "diagnostics: manual recovery: re-enable with dlsym CGSConfigureDisplayEnabled \
                 (cfg, {key}, true) + CGCompleteDisplayConfiguration(.forSession), then \
                 CGConfigureDisplayOrigin; see sidecar/DISPLAY_DETACH.md. Last resort: log out.",
                key = m
                    .pending_records()
                    .first()
                    .map(|r| r.key.clone())
                    .unwrap_or_default(),
            );
        }
        if records.exists() {
            let kept = dir.keep();
            eprintln!(
                "Record retained. Restart with KVMFLOW_DATA_DIR={}",
                kept.display()
            );
        }
        restored.unwrap();
    }
}
