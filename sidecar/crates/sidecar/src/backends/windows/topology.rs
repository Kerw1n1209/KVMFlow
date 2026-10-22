//! Windows display detach with `SetDisplayConfig`: the monitor's path is left
//! out of the supplied configuration and the full topology is saved first, so
//! it can be applied again. NOT VERIFIED ON HARDWARE (the
//! `probes/windows/DisplayTopology.ps1` probe has not been run); the factory in
//! `crate::topology` keeps it off unless the `windows-display-detach` cargo
//! feature or `KVMFLOW_WINDOWS_DISPLAY_DETACH=1` is set.
//!
//! Never uses `SDC_SAVE_TO_DATABASE`: nothing is written to the display
//! database, so a crash is undone by the next start of the app, or by a
//! sign-out / reboot / Win+P "Extend".

use super::ddc::gdi_fingerprints;
use crate::backends::topology::{
    compact_mode_indices, hex_encode, is_internal_output_technology, DetachMethod, DetachRecord,
    PlatformTopology, SysDisplay,
};
use kvmflow_core::errors::{CoreError, E_TOPOLOGY_FAILED, E_TOPOLOGY_NOT_FOUND};
use std::time::{Duration, Instant};
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig, SetDisplayConfig,
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
    QUERY_DISPLAY_CONFIG_FLAGS, SDC_ALLOW_CHANGES, SDC_APPLY, SDC_USE_SUPPLIED_DISPLAY_CONFIG,
};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS};

const QDC_ALL_PATHS: QUERY_DISPLAY_CONFIG_FLAGS = QUERY_DISPLAY_CONFIG_FLAGS(1);
const SETTLE_TIMEOUT: Duration = Duration::from_secs(10);

struct Config {
    paths: Vec<DISPLAYCONFIG_PATH_INFO>,
    modes: Vec<DISPLAYCONFIG_MODE_INFO>,
}

fn win_error(what: &str, rc: i32) -> CoreError {
    CoreError::new(
        E_TOPOLOGY_FAILED,
        format!("{what} failed (Win32 {rc}; 87 = invalid parameter, 5 = access denied)"),
    )
}

/// `QueryDisplayConfig`; the topology can change between the size query and
/// the read, which reports ERROR_INSUFFICIENT_BUFFER, so retry.
fn query(flags: QUERY_DISPLAY_CONFIG_FLAGS) -> Result<Config, CoreError> {
    for _ in 0..5 {
        let (mut path_count, mut mode_count) = (0u32, 0u32);
        let rc = unsafe { GetDisplayConfigBufferSizes(flags, &mut path_count, &mut mode_count) };
        if rc != ERROR_SUCCESS {
            return Err(win_error("GetDisplayConfigBufferSizes", rc.0 as i32));
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        let rc = unsafe {
            QueryDisplayConfig(
                flags,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                None,
            )
        };
        if rc == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if rc != ERROR_SUCCESS {
            return Err(win_error("QueryDisplayConfig", rc.0 as i32));
        }
        paths.truncate(path_count as usize);
        modes.truncate(mode_count as usize);
        return Ok(Config { paths, modes });
    }
    Err(CoreError::new(
        E_TOPOLOGY_FAILED,
        "QueryDisplayConfig kept reporting a changing topology",
    ))
}

fn apply(config: &Config) -> Result<(), CoreError> {
    let flags = SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES;
    let rc = unsafe { SetDisplayConfig(Some(&config.paths), Some(&config.modes), flags) };
    if rc != 0 {
        return Err(win_error("SetDisplayConfig", rc));
    }
    Ok(())
}

fn path_key(p: &DISPLAYCONFIG_PATH_INFO) -> String {
    format!(
        "win:{}:{}:{}",
        p.targetInfo.adapterId.LowPart, p.targetInfo.adapterId.HighPart, p.targetInfo.id
    )
}

fn source_mode_idx(p: &DISPLAYCONFIG_PATH_INFO) -> u32 {
    unsafe { p.sourceInfo.Anonymous.modeInfoIdx }
}

fn target_mode_idx(p: &DISPLAYCONFIG_PATH_INFO) -> u32 {
    unsafe { p.targetInfo.Anonymous.modeInfoIdx }
}

/// Desktop position and size of a path's source, when it has a source mode.
fn source_geometry(config: &Config, p: &DISPLAYCONFIG_PATH_INFO) -> Option<(i32, i32, u32, u32)> {
    let mode = config.modes.get(source_mode_idx(p) as usize)?;
    if mode.infoType != DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE {
        return None;
    }
    let s = unsafe { mode.Anonymous.sourceMode };
    Some((s.position.x, s.position.y, s.width, s.height))
}

fn gdi_name(p: &DISPLAYCONFIG_PATH_INFO) -> String {
    let mut req = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
    req.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
    req.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
    req.header.adapterId = p.sourceInfo.adapterId;
    req.header.id = p.sourceInfo.id;
    if unsafe { DisplayConfigGetDeviceInfo(&mut req.header) } != 0 {
        return String::new();
    }
    let end = req
        .viewGdiDeviceName
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(32);
    String::from_utf16_lossy(&req.viewGdiDeviceName[..end])
}

/// Raw bytes of a plain-old-data array, as hex.
fn encode_array<T: Copy>(items: &[T]) -> String {
    // SAFETY: the display config structs are repr(C) plain data.
    let bytes = unsafe {
        std::slice::from_raw_parts(items.as_ptr() as *const u8, std::mem::size_of_val(items))
    };
    hex_encode(bytes)
}

#[cfg(test)]
fn decode_array<T: Copy + Default>(hex: &str) -> Option<Vec<T>> {
    let bytes = crate::backends::topology::hex_decode(hex)?;
    let size = std::mem::size_of::<T>();
    if size == 0 || bytes.len() % size != 0 {
        return None;
    }
    let mut out = vec![T::default(); bytes.len() / size];
    // SAFETY: the destination holds exactly `bytes.len()` bytes of plain data.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.as_mut_ptr() as *mut u8, bytes.len());
    }
    Some(out)
}

fn encode_topology(config: &Config) -> String {
    format!(
        "{}:{}",
        encode_array(&config.paths),
        encode_array(&config.modes)
    )
}

#[cfg(test)]
fn decode_topology(text: &str) -> Option<Config> {
    let (paths, modes) = text.split_once(':')?;
    Some(Config {
        paths: decode_array(paths)?,
        modes: decode_array(modes)?,
    })
}

/// The config without `remove_key`'s path. With `shift` the remaining source
/// positions move so the kept display `new_main_key` sits at (0, 0).
fn without_target(
    config: &Config,
    remove_key: &str,
    new_main_key: Option<&str>,
) -> Result<Config, CoreError> {
    let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> = config
        .paths
        .iter()
        .filter(|p| path_key(p) != remove_key)
        .copied()
        .collect();
    if paths.len() == config.paths.len() {
        return Err(CoreError::new(
            E_TOPOLOGY_NOT_FOUND,
            format!("{remove_key} is not an active path"),
        ));
    }
    let refs: Vec<[u32; 2]> = paths
        .iter()
        .map(|p| [source_mode_idx(p), target_mode_idx(p)])
        .collect();
    let (new_refs, keep) = compact_mode_indices(&refs, config.modes.len());
    for (p, r) in paths.iter_mut().zip(&new_refs) {
        p.sourceInfo.Anonymous.modeInfoIdx = r[0];
        p.targetInfo.Anonymous.modeInfoIdx = r[1];
    }
    let modes: Vec<DISPLAYCONFIG_MODE_INFO> = keep.iter().map(|&i| config.modes[i]).collect();
    let mut trimmed = Config { paths, modes };
    let origin = new_main_key.and_then(|key| {
        let p = trimmed.paths.iter().find(|p| path_key(p) == key)?;
        source_geometry(&trimmed, p).map(|(x, y, _, _)| (x, y))
    });
    if let Some((dx, dy)) = origin {
        // Clone groups share one source mode: shift each mode once.
        let indices: std::collections::BTreeSet<u32> =
            trimmed.paths.iter().map(source_mode_idx).collect();
        for idx in indices {
            if let Some(mode) = trimmed.modes.get_mut(idx as usize) {
                if mode.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE {
                    unsafe {
                        mode.Anonymous.sourceMode.position.x -= dx;
                        mode.Anonymous.sourceMode.position.y -= dy;
                    }
                }
            }
        }
    }
    Ok(trimmed)
}

fn wait_until(mut cond: impl FnMut() -> Result<bool, CoreError>) -> Result<(), CoreError> {
    let start = Instant::now();
    loop {
        if cond()? {
            return Ok(());
        }
        if start.elapsed() >= SETTLE_TIMEOUT {
            return Err(CoreError::new(
                E_TOPOLOGY_FAILED,
                "display topology did not settle before timeout",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn key_is_active(key: &str) -> Result<bool, CoreError> {
    Ok(query(QDC_ONLY_ACTIVE_PATHS)?
        .paths
        .iter()
        .any(|p| path_key(p) == key))
}

fn target_identity(p: &DISPLAYCONFIG_PATH_INFO) -> Result<String, CoreError> {
    let mut name = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
    name.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
    name.header.size = std::mem::size_of_val(&name) as u32;
    name.header.adapterId = p.targetInfo.adapterId;
    name.header.id = p.targetInfo.id;
    let rc = unsafe { DisplayConfigGetDeviceInfo(&mut name.header) };
    if rc != 0 {
        return Err(win_error("DisplayConfigGetDeviceInfo(target)", rc));
    }
    let end = name
        .monitorDevicePath
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(128);
    let identity = String::from_utf16_lossy(&name.monitorDevicePath[..end]).to_lowercase();
    if identity.is_empty() {
        return Err(CoreError::new(
            E_TOPOLOGY_FAILED,
            "monitor has no stable device path",
        ));
    }
    Ok(identity)
}

fn source_key(p: &DISPLAYCONFIG_PATH_INFO) -> String {
    format!(
        "{}:{}:{}",
        p.sourceInfo.adapterId.LowPart, p.sourceInfo.adapterId.HighPart, p.sourceInfo.id
    )
}

fn with_target(current: &Config, possible: &Config, key: &str) -> Result<Config, CoreError> {
    let available: Vec<_> = possible
        .paths
        .iter()
        .filter(|p| p.targetInfo.targetAvailable.as_bool())
        .copied()
        .collect();
    let mut paths = crate::backends::topology::restore_paths(
        &current.paths,
        &available,
        key,
        path_key,
        source_key,
    )?;
    if paths.len() > current.paths.len() {
        let target = paths.last_mut().unwrap();
        target.flags |= 1; // DISPLAYCONFIG_PATH_ACTIVE
                           // Ask best-mode logic for the new path; current paths retain their modes.
        target.sourceInfo.Anonymous.modeInfoIdx = u32::MAX;
        target.targetInfo.Anonymous.modeInfoIdx = u32::MAX;
    }
    Ok(Config {
        paths,
        modes: current.modes.clone(),
    })
}

#[derive(Default)]
pub struct WindowsTopology;
impl WindowsTopology {
    pub fn new() -> Self {
        Self
    }
}

impl PlatformTopology for WindowsTopology {
    fn enumerate(&mut self, hints: &[DetachRecord]) -> Result<Vec<SysDisplay>, CoreError> {
        let active = query(QDC_ONLY_ACTIVE_PATHS)?;
        let all = query(QDC_ALL_PATHS)?;
        let fingerprints = gdi_fingerprints();
        let mut displays = std::collections::BTreeMap::new();
        // Active paths first; QDC_ALL_PATHS contains alternate routes to one target.
        for p in active.paths.iter().chain(all.paths.iter()) {
            let key = path_key(p);
            if displays.contains_key(&key) || !p.targetInfo.targetAvailable.as_bool() {
                continue;
            }
            let identity = target_identity(p)?;
            let attached = active.paths.iter().any(|p| path_key(p) == key);
            let gdi = gdi_name(p);
            let fingerprint = if attached {
                fingerprints
                    .iter()
                    .find(|(name, _)| *name == gdi)
                    .map(|(_, fp)| fp.clone())
            } else {
                None
            }
            .or_else(|| {
                hints
                    .iter()
                    .find(|r| !r.identity.is_empty() && r.identity == identity)
                    .map(|r| r.fingerprint.clone())
            })
            .unwrap_or_else(|| format!("win-unresolved:{identity}"));
            displays.insert(
                key.clone(),
                SysDisplay {
                    key,
                    fingerprint_source: if fingerprints.iter().any(|(_, fp)| fp == &fingerprint)
                        && attached
                    {
                        crate::backends::topology::FingerprintSource::Ddc
                    } else {
                        crate::backends::topology::FingerprintSource::RecordHint
                    },
                    fingerprint,
                    identity,
                    present: true,
                    active: attached,
                    builtin: is_internal_output_technology(p.targetInfo.outputTechnology.0 as u32),
                    is_main: attached
                        && source_geometry(&active, p)
                            .is_some_and(|(x, y, w, _)| x == 0 && y == 0 && w > 0),
                },
            );
        }
        Ok(displays.into_values().collect())
    }

    fn capture(
        &mut self,
        target: &SysDisplay,
        _all: &[SysDisplay],
    ) -> Result<DetachRecord, CoreError> {
        Ok(DetachRecord {
            fingerprint_source: target.fingerprint_source,
            fingerprint: target.fingerprint.clone(),
            key: target.key.clone(),
            identity: target.identity.clone(),
            method: DetachMethod::Pending,
            mirror_of: None,
            origins: vec![],
            topology: Some(encode_topology(&query(QDC_ONLY_ACTIVE_PATHS)?)),
            detached_at_ms: crate::session::now_ms(),
        })
    }

    fn detach(
        &mut self,
        record: &DetachRecord,
        reserve: &SysDisplay,
    ) -> Result<DetachMethod, CoreError> {
        let config = query(QDC_ONLY_ACTIVE_PATHS)?;
        let target = config
            .paths
            .iter()
            .find(|p| path_key(p) == record.key)
            .ok_or_else(|| CoreError::new(E_TOPOLOGY_NOT_FOUND, "target path disappeared"))?;
        if target_identity(target)? != record.identity {
            return Err(CoreError::new(E_TOPOLOGY_FAILED, "target identity changed"));
        }
        let main =
            source_geometry(&config, target).is_some_and(|(x, y, w, _)| x == 0 && y == 0 && w > 0);
        let next = without_target(&config, &record.key, main.then_some(reserve.key.as_str()))?;
        if next.paths.is_empty() {
            return Err(CoreError::new(
                kvmflow_core::errors::E_TOPOLOGY_LAST_ACTIVE,
                "refusing empty topology",
            ));
        }
        apply(&next)?;
        wait_until(|| key_is_active(&record.key).map(|active| !active))?;
        Ok(DetachMethod::Disabled)
    }

    fn attach(&mut self, record: &DetachRecord, _allow_extend: bool) -> Result<(), CoreError> {
        let current = query(QDC_ONLY_ACTIVE_PATHS)?;
        let possible = query(QDC_ALL_PATHS)?;
        let target = possible
            .paths
            .iter()
            .find(|p| path_key(p) == record.key && p.targetInfo.targetAvailable.as_bool())
            .ok_or_else(|| {
                CoreError::new(
                    E_TOPOLOGY_NOT_FOUND,
                    "target is physically absent; record kept",
                )
            })?;
        if !record.identity.is_empty() && target_identity(target)? != record.identity {
            return Err(CoreError::new(
                E_TOPOLOGY_FAILED,
                "target identity changed; record kept",
            ));
        }
        if current.paths.iter().any(|p| path_key(p) == record.key) {
            return Ok(());
        }
        // No snapshot replay, no Extend-all fallback (even with one record).
        apply(&with_target(&current, &possible, &record.key)?)?;
        wait_until(|| key_is_active(&record.key))
    }

    fn wait_ready(&mut self, wanted: &[String]) -> Result<(), CoreError> {
        if wanted.is_empty() {
            return Ok(());
        }
        wait_until(|| {
            let listed = super::ddc::ready_fingerprints(wanted);
            Ok(wanted.iter().all(|fp| listed.contains(fp)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::LUID;

    fn path(target: u32, source_mode: u32, target_mode: u32) -> DISPLAYCONFIG_PATH_INFO {
        let mut p = DISPLAYCONFIG_PATH_INFO::default();
        p.sourceInfo.adapterId = LUID {
            LowPart: 1,
            HighPart: 0,
        };
        p.targetInfo.adapterId = LUID {
            LowPart: 1,
            HighPart: 0,
        };
        p.targetInfo.id = target;
        p.sourceInfo.id = target;
        p.targetInfo.targetAvailable = true.into();
        p.flags = 1;
        p.sourceInfo.Anonymous.modeInfoIdx = source_mode;
        p.targetInfo.Anonymous.modeInfoIdx = target_mode;
        p
    }

    fn source_mode(x: i32, y: i32) -> DISPLAYCONFIG_MODE_INFO {
        let mut m = DISPLAYCONFIG_MODE_INFO::default();
        m.infoType = DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE;
        m.Anonymous.sourceMode.width = 1920;
        m.Anonymous.sourceMode.height = 1080;
        m.Anonymous.sourceMode.position.x = x;
        m.Anonymous.sourceMode.position.y = y;
        m
    }

    /// Two monitors side by side: target 10 at (0,0) is main, target 20 at (1920,0).
    fn two_monitors() -> Config {
        Config {
            paths: vec![path(10, 0, 1), path(20, 2, 3)],
            modes: vec![
                source_mode(0, 0),
                DISPLAYCONFIG_MODE_INFO::default(),
                source_mode(1920, 0),
                DISPLAYCONFIG_MODE_INFO::default(),
            ],
        }
    }

    #[test]
    fn removing_a_path_compacts_modes() {
        let next = without_target(&two_monitors(), "win:1:0:20", None).unwrap();
        assert_eq!(next.paths.len(), 1);
        assert_eq!(next.modes.len(), 2);
        assert_eq!(source_mode_idx(&next.paths[0]), 0);
        assert_eq!(target_mode_idx(&next.paths[0]), 1);
        assert_eq!(
            source_geometry(&next, &next.paths[0]),
            Some((0, 0, 1920, 1080))
        );
    }

    #[test]
    fn removing_the_main_path_moves_the_kept_one_to_the_origin() {
        let next = without_target(&two_monitors(), "win:1:0:10", Some("win:1:0:20")).unwrap();
        assert_eq!(next.paths.len(), 1);
        assert_eq!(
            source_geometry(&next, &next.paths[0]),
            Some((0, 0, 1920, 1080))
        );
    }

    #[test]
    fn unknown_target_is_not_found() {
        let err = without_target(&two_monitors(), "win:1:0:99", None)
            .err()
            .unwrap();
        assert_eq!(err.code, E_TOPOLOGY_NOT_FOUND);
    }

    #[test]
    fn restoring_one_path_does_not_replay_other_detached_or_stale_paths() {
        let current = Config {
            paths: vec![path(30, 0, 1)],
            modes: vec![source_mode(444, 555), DISPLAYCONFIG_MODE_INFO::default()],
        };
        let restored = with_target(&current, &two_monitors(), "win:1:0:10").unwrap();
        assert_eq!(
            restored.paths.iter().map(path_key).collect::<Vec<_>>(),
            ["win:1:0:30", "win:1:0:10"]
        );
        assert_eq!(
            source_geometry(&restored, &restored.paths[0]),
            Some((444, 555, 1920, 1080))
        );
        assert_eq!(source_mode_idx(&restored.paths[1]), u32::MAX);
        assert_eq!(target_mode_idx(&restored.paths[1]), u32::MAX);
    }

    #[test]
    fn failed_queries_do_not_confirm_detach() {
        let error =
            wait_until(|| Err(CoreError::new(E_TOPOLOGY_FAILED, "query failed"))).unwrap_err();
        assert_eq!(error.message, "query failed");
    }

    #[test]
    fn saved_topology_round_trips() {
        let config = two_monitors();
        let back = decode_topology(&encode_topology(&config)).unwrap();
        assert_eq!(back.paths.len(), 2);
        assert_eq!(back.modes.len(), 4);
        assert_eq!(path_key(&back.paths[1]), "win:1:0:20");
        assert_eq!(
            source_geometry(&back, &back.paths[1]),
            Some((1920, 0, 1920, 1080))
        );
        assert!(decode_topology("zz:zz").is_none());
        assert!(decode_topology("no-separator").is_none());
    }

    /// Read-only: prints the active displays and their fingerprints.
    /// `cargo test -p kvmflow-sidecar -- --ignored windows_enumerate_real_displays --nocapture`
    #[test]
    #[ignore = "queries the real display system"]
    fn windows_enumerate_real_displays() {
        let displays = WindowsTopology::new().enumerate(&[]).unwrap();
        for d in &displays {
            println!("{d:?}");
        }
        assert!(!displays.is_empty());
    }

    /// DESTRUCTIVE: really detaches `KVMFLOW_TEST_DETACH_FINGERPRINT` for ~5 s
    /// and restores it. Run only from the manual steps in the task report.
    #[test]
    #[ignore = "detaches a real monitor"]
    fn windows_detach_and_restore_real_monitor() {
        use crate::detach::TopologyManager;
        use crate::topology::DisplayTopology;
        let fp = std::env::var("KVMFLOW_TEST_DETACH_FINGERPRINT")
            .expect("set KVMFLOW_TEST_DETACH_FINGERPRINT to a fingerprint from the enumerate test");
        let dir = tempfile::tempdir().unwrap();
        let records = dir.path().join(crate::detach::RECORDS_FILE);
        println!("Recovery record: {}", records.display());
        let mut m = TopologyManager::new(WindowsTopology::new(), records.clone(), true);
        let outcome = m.detach(&fp);
        std::thread::sleep(Duration::from_secs(5));
        let attach_started = Instant::now();
        let attached = m.attach(&fp);
        let attach_ms = attach_started.elapsed().as_millis();
        let readiness = if outcome.is_ok() && attached.is_ok() {
            let started = Instant::now();
            let ready = m.wait_enumeration();
            println!(
                "readiness_ms={} result={ready:?}",
                started.elapsed().as_millis()
            );
            Some(ready)
        } else {
            println!("readiness skipped: detach or attach did not succeed");
            None
        };
        let restore_started = Instant::now();
        let report = m.restore_all();
        println!(
            "attach_ms={attach_ms}; final_restore_ms={}",
            restore_started.elapsed().as_millis()
        );
        println!("detach: {outcome:?}\nattach: {attached:?}\nrestore: {report:?}");
        if records.exists() {
            let kept = dir.keep();
            eprintln!(
                "Record retained. Restart with KVMFLOW_DATA_DIR={}",
                kept.display()
            );
        }
        outcome.unwrap();
        attached.unwrap();
        if let Some(readiness) = readiness {
            readiness.unwrap();
        }
        report.unwrap();
    }
}
