//! macOS display/DDC adapter. Implements the KVM-1 vendoring verdict:
//! all DDC goes through a self-built stock m1ddc v1.2.0 (tag 2549fec,
//! MIT, vendored at probes/mac/experiments/m1ddc-stock/) spawned as a
//! subprocess. Display enumeration joins CoreGraphics' online display list
//! with `m1ddc display list detailed`, which exposes per-display EDID
//! identity fields - so the sidecar does NOT re-port the (defect-prone)
//! in-process discovery that KVM-1 falsified.

use crate::backends::{
    sanitize_manufacturer, sanitize_text_field, Backend, DdcCapability, DisplayInfo, VcpRead,
    VcpWriteOutcome,
};
use kvmflow_core::edid_identity;
use kvmflow_core::errors::{
    CoreError, E_DDC_BINARY_MISSING, E_DDC_FAILED, E_DDC_NOT_READABLE, E_DISPLAY_NOT_FOUND,
};
use serde_json::json;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGGetOnlineDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayIsBuiltin(id: u32) -> u32;
    fn CGDisplayIsMain(id: u32) -> u32;
    fn CGDisplayIsActive(id: u32) -> u32;
}

#[derive(Debug, Clone, Default)]
struct M1ddcDisplay {
    index: u32,
    cg_id: u32,
    product_name: String,
    manufacturer: String,
    an_serial: String,
    vendor: u32,
    model: u32,
    serial: u32,
}

pub struct MacDdcBackend {
    binary: PathBuf,
    cache: Vec<(M1ddcDisplay, bool, bool)>, // (info, builtin, is_main)
}

/// Binary resolution order (mirrors the KVM-1 stock-ddc wrapper):
/// explicit flag > $KVMFLOW_M1DDC > next to the sidecar binary >
/// ../Resources/m1ddc > Homebrew paths > PATH.
pub fn resolve_m1ddc(explicit: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        let pb = PathBuf::from(p);
        return pb.is_file().then_some(pb);
    }
    if let Ok(env) = std::env::var("KVMFLOW_M1DDC") {
        if !env.is_empty() {
            let pb = PathBuf::from(env);
            if pb.is_file() {
                return Some(pb);
            }
            return None; // authoritative override - no fallback
        }
    }
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let mut candidates: Vec<PathBuf> = vec![
        exe_dir.join("m1ddc"),
        exe_dir.join("../Resources/m1ddc"),
        exe_dir.join("../../../probes/mac/.build/m1ddc-stock/m1ddc-selfbuilt"),
    ];
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(PathBuf::from(home).join("Library/Application Support/KVMFlow/bin/m1ddc"));
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin/m1ddc"));
    candidates.push(PathBuf::from("/usr/local/bin/m1ddc"));
    candidates.into_iter().find(|c| c.is_file())
}

/// Run m1ddc capturing output, with a hard timeout so a stuck child cannot
/// wedge the sidecar loop.
fn run_m1ddc(
    binary: &PathBuf,
    args: &[&str],
    timeout: Duration,
) -> Result<(i32, String, String), CoreError> {
    let mut child = Command::new(binary)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CoreError::new(E_DDC_BINARY_MISSING, format!("spawn m1ddc: {e}")))?;

    let mut out = String::new();
    let mut err = String::new();
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if let Some(o) = &mut child.stdout {
                    use std::io::Read;
                    let _ = o.read_to_string(&mut out);
                }
                if let Some(e) = &mut child.stderr {
                    use std::io::Read;
                    let _ = e.read_to_string(&mut err);
                }
                return Ok((status.code().unwrap_or(-1), out, err));
            }
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(CoreError::new(
                        E_DDC_FAILED,
                        format!("m1ddc timed out after {}s", timeout.as_secs()),
                    ));
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(CoreError::new(E_DDC_FAILED, format!("wait m1ddc: {e}"))),
        }
    }
}

fn parse_display_list_detailed(stdout: &str) -> Vec<M1ddcDisplay> {
    let mut out: Vec<M1ddcDisplay> = Vec::new();
    for line in stdout.lines() {
        let line = line.trim_end();
        if let Some(rest) = line.strip_prefix('[') {
            // "[1] G73 (UUID)"
            if let Some((idx, _)) = rest.split_once(']') {
                if let Ok(index) = idx.trim().parse::<u32>() {
                    out.push(M1ddcDisplay {
                        index,
                        ..Default::default()
                    });
                }
            }
        } else if let Some(cur) = out.last_mut() {
            let get = |prefix: &str| -> Option<String> {
                let v = line.trim().strip_prefix(prefix)?.trim().to_string();
                (!v.is_empty()).then_some(v)
            };
            if let Some(v) = get("- Product name:") {
                cur.product_name = sanitize_text_field(&v);
            } else if let Some(v) = get("- Manufacturer:") {
                cur.manufacturer = sanitize_manufacturer(&v);
            } else if let Some(v) = get("- AN Serial:") {
                cur.an_serial = sanitize_text_field(&v);
            } else if let Some(v) = get("- Vendor:") {
                cur.vendor = v
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
            } else if let Some(v) = get("- Model:") {
                cur.model = v
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
            } else if let Some(v) = get("- Serial:") {
                cur.serial = v
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
            } else if let Some(v) = get("- Display ID:") {
                cur.cg_id = v.parse().unwrap_or(0);
            }
        }
    }
    out
}

fn online_display_ids() -> Vec<(u32, bool, bool)> {
    let mut ids = [0u32; 16];
    let mut count: u32 = 0;
    unsafe {
        CGGetOnlineDisplayList(16, ids.as_mut_ptr(), &mut count);
    }
    ids[..count as usize]
        .iter()
        .map(|&id| unsafe {
            (
                id,
                CGDisplayIsBuiltin(id) == 1,
                CGDisplayIsMain(id) == 1 && CGDisplayIsActive(id) == 1,
            )
        })
        .collect()
}

impl MacDdcBackend {
    pub fn new(explicit: Option<&str>) -> Result<Self, CoreError> {
        let binary = resolve_m1ddc(explicit).ok_or_else(|| {
            CoreError::new(
                E_DDC_BINARY_MISSING,
                "stock m1ddc binary not found; build it with probes/mac/experiments/m1ddc-stock/build.sh \
                 and place it next to the sidecar or set KVMFLOW_M1DDC",
            )
        })?;
        Ok(Self {
            binary,
            cache: Vec::new(),
        })
    }

    fn refresh_cache_with_timeout(&mut self, timeout: Duration) -> Result<(), CoreError> {
        let (code, out, err) = run_m1ddc(&self.binary, &["display", "list", "detailed"], timeout)?;
        if code != 0 {
            return Err(CoreError::new(
                E_DDC_FAILED,
                format!("m1ddc display list failed ({code}): {err}"),
            ));
        }
        let parsed = parse_display_list_detailed(&out);
        let cg = online_display_ids();
        self.cache = parsed
            .into_iter()
            .filter_map(|d| {
                // Join on the CG display id m1ddc reports. Zero = not
                // reported; keep only joinable entries.
                cg.iter()
                    .find(|(id, _, _)| *id == d.cg_id)
                    .map(|&(id, builtin, main)| (M1ddcDisplay { cg_id: id, ..d }, builtin, main))
            })
            .collect();
        Ok(())
    }

    fn refresh_cache(&mut self) -> Result<(), CoreError> {
        self.refresh_cache_with_timeout(Duration::from_secs(15))
    }

    fn find(&self, edid_id: &str) -> Result<&M1ddcDisplay, CoreError> {
        self.cache
            .iter()
            .map(|(d, _, _)| d)
            .find(|d| edid_of(d) == edid_id)
            .ok_or_else(|| {
                CoreError::new(
                    E_DISPLAY_NOT_FOUND,
                    format!("no display with edid_id {edid_id}"),
                )
            })
    }
}

fn edid_of(d: &M1ddcDisplay) -> String {
    edid_identity(&d.manufacturer, d.model as u16, d.serial, &d.an_serial)
}

impl Backend for MacDdcBackend {
    fn name(&self) -> &'static str {
        "macos_m1ddc"
    }

    fn refresh_displays(&mut self) -> Result<(), CoreError> {
        self.refresh_cache()
    }

    fn refresh_displays_with_timeout(&mut self, timeout: Duration) -> Result<(), CoreError> {
        self.refresh_cache_with_timeout(timeout)
    }

    fn list_displays(&mut self) -> Result<Vec<DisplayInfo>, String> {
        self.refresh_cache().map_err(|e| e.to_string())?;
        Ok(self
            .cache
            .iter()
            .map(|(d, builtin, main)| DisplayInfo {
                edid_id: edid_of(d),
                manufacturer: d.manufacturer.clone(),
                product_code: d.model as u16,
                model_name: d.product_name.clone(),
                serial_string: d.an_serial.clone(),
                display_index: d.index,
                builtin: *builtin,
                is_main: *main,
                ddc: DdcCapability::Unknown,
            })
            .collect())
    }

    fn read_input(&mut self, edid_id: &str) -> Result<VcpRead, CoreError> {
        self.read_input_with_timeout(edid_id, Duration::from_secs(15))
    }

    fn read_input_with_timeout(
        &mut self,
        edid_id: &str,
        timeout: Duration,
    ) -> Result<VcpRead, CoreError> {
        if self.cache.is_empty() {
            self.refresh_cache_with_timeout(timeout)?;
        }
        let idx = self.find(edid_id)?.index;
        let args = ["display", &idx.to_string(), "get", "input"];
        let (code, out, err) = run_m1ddc(&self.binary, &args, timeout)?;
        if code != 0 {
            // m1ddc exits non-zero when the DDC exchange fails - on this
            // hardware that includes the not-active-input degradation.
            return Err(CoreError::new(
                E_DDC_NOT_READABLE,
                format!("m1ddc get failed ({code}): {}", err.trim()),
            ));
        }
        let value = out
            .trim()
            .lines()
            .rev()
            .find_map(|l| l.trim().parse::<u16>().ok())
            .ok_or_else(|| {
                CoreError::new(
                    E_DDC_FAILED,
                    format!("m1ddc get: unparseable output {out:?}"),
                )
            })?;
        Ok(VcpRead { value, max: 0 })
    }

    fn write_input(&mut self, edid_id: &str, value: u16) -> Result<VcpWriteOutcome, CoreError> {
        if self.cache.is_empty() {
            self.refresh_cache()?;
        }
        let idx = self.find(edid_id)?.index;

        // Reading a sleeping/off-input monitor can stall for seconds. Writes
        // must not depend on a best-effort diagnostic read completing first.
        let previous = None;

        let args = [
            "display",
            &idx.to_string(),
            "set",
            "input",
            &value.to_string(),
        ];
        let (code, out, err) = run_m1ddc(&self.binary, &args, Duration::from_secs(2))?;
        let commanded = code == 0;
        let error = (!commanded).then(|| format!("m1ddc set failed ({code}): {}", err.trim()));
        Ok(VcpWriteOutcome {
            commanded,
            previous,
            error,
            evidence: json!({
                "argv": ["m1ddc", args],
                "exit_code": code,
                "stdout": out,
                "stderr": err,
                "stock_source": "m1ddc v1.2.0 tag 2549fec (vendored, self-built)",
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // KVM-5 session observation (fixed in KVM-7 T7): the built-in panel's
    // m1ddc identity lines are placeholders - manufacturer is an OUI-style
    // MAC string and the product name is a literal "(null)". They must not
    // reach DisplayInfo / the wizard table as-is.
    #[test]
    fn parses_builtin_placeholder_identity_as_clean() {
        let stdout = "\
[1] Color LCD (UUID)
- Product name: (null)
- Manufacturer: 00-10-fa
- Vendor: 16
- Model: 10099
- Serial: 0
- Display ID: 1234567
";
        let parsed = parse_display_list_detailed(stdout);
        assert_eq!(parsed.len(), 1);
        let d = &parsed[0];
        assert_eq!(d.product_name, "");
        assert_eq!(d.manufacturer, "");
        let edid = edid_of(d);
        assert!(edid.starts_with("UNK-"), "got {edid}");
        assert!(!edid.contains("00-10") && !edid.contains("(null)"));
    }

    #[test]
    fn parses_real_monitor_identity_unchanged() {
        let stdout = "\
[2] G73 (UUID)
- Product name: G73
- Manufacturer: SAC
- AN Serial: 0000000000001
- Vendor: 8660
- Model: 10083
- Serial: 0
- Display ID: 7654321
";
        let parsed = parse_display_list_detailed(stdout);
        assert_eq!(parsed.len(), 1);
        let d = &parsed[0];
        assert_eq!(d.product_name, "G73");
        assert_eq!(d.manufacturer, "SAC");
        assert_eq!(d.an_serial, "0000000000001");
        assert_eq!(edid_of(d), "SAC-2763-S:0000000000001");
    }
}
