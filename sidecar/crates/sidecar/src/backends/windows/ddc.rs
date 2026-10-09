//! Windows display/DDC adapter - dxva2. KVM-1 validated this path on real
//! hardware (DisplayProbe 0.1.4+); quirks preserved here:
//! - a PHYSICAL_MONITOR handle value of 0 can be valid - never treat 0 as
//!   "no monitor";
//! - only the active input reads/writes reliably (push-away model);
//! - dxva2 Get/SetVCP return C BOOL: nonzero = success, 0 = failure (the
//!   KVM-1 probe marshals them as C# bool; KVM-7 fixed an inverted check);
//! - VCP codes come only from `backends::vcp_codes` (hex table; KVM-8 fixed
//!   a decimal 60 = 0x3C misreading of the 0x60 Input Select code).
//!
//! KVM-7: enumeration previously built the EDID registry path straight from
//! the EnumDisplayDevices DeviceID (`MONITOR\<hwid>\<inst>`), but the real
//! key lives under `Enum\DISPLAY\<hwid>\<inst>` (KVM-1 probe + KVM-6
//! forensic B4) - every monitor was silently dropped and display.list
//! returned 0 displays on real hardware. Resolution now goes through
//! `backends::edid_registry` (candidate walk + sibling scan), and every
//! dropped attached monitor leaves a diagnostic trace.

use crate::backends::edid_registry::{self, EdidIdentity};
use crate::backends::vcp_codes;
use crate::backends::{Backend, DdcCapability, DisplayInfo, VcpRead, VcpWriteOutcome};
use kvmflow_core::edid_identity;
use kvmflow_core::errors::{CoreError, E_DDC_FAILED, E_DDC_NOT_READABLE, E_DISPLAY_NOT_FOUND};
use serde_json::json;
use std::sync::Mutex;
use windows::core::{BOOL, PCWSTR};
use windows::Win32::Devices::Display::{
    DestroyPhysicalMonitor, GetPhysicalMonitorsFromHMONITOR, GetVCPFeatureAndVCPFeatureReply,
    SetVCPFeature, PHYSICAL_MONITOR,
};
use windows::Win32::Foundation::{GetLastError, HANDLE, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, DISPLAY_DEVICEW, HDC, HMONITOR,
    MONITORINFOEXW,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
    REG_BINARY, REG_VALUE_TYPE,
};

// VCP codes come from the shared hex-canonical table (KVM-8: this file
// once carried `VCP_INPUT = 60` decimal = 0x3C instead of 0x60).
use vcp_codes::INPUT_SOURCE as VCP_INPUT;

struct WinDisplay {
    edid_id: String,
    manufacturer: String,
    product_code: u16,
    model_name: String,
    serial_string: String,
    device_name: String, // "\\.\DISPLAY1" - the GDI device name
}

/// Enumeration diagnostics for monitors that were attached but could not be
/// turned into managed displays (EDID unreadable, no monitor entry...).
/// Drained by the display.list handler so drop reasons reach the session
/// log (KVM-7 remediation: the defect shipped invisibly). Requests are
/// handled on one thread, so one slot is enough.
static ENUM_DIAG: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn take_enumeration_diag() -> Vec<String> {
    match ENUM_DIAG.lock() {
        Ok(mut v) => std::mem::take(&mut *v),
        Err(poisoned) => std::mem::take(&mut poisoned.into_inner()),
    }
}

fn push_diag(line: String) {
    match ENUM_DIAG.lock() {
        Ok(mut v) => v.push(line),
        Err(poisoned) => {
            let mut v = poisoned.into_inner();
            v.push(line);
        }
    }
}

pub struct WindowsDdcBackend {
    cache: Vec<WinDisplay>,
}

impl WindowsDdcBackend {
    pub fn new() -> Self {
        Self { cache: Vec::new() }
    }

    fn refresh_cache(&mut self) {
        self.cache = enumerate();
    }

    fn find(&self, edid_id: &str) -> Result<&WinDisplay, CoreError> {
        self.cache
            .iter()
            .find(|d| d.edid_id == edid_id)
            .ok_or_else(|| {
                CoreError::new(
                    E_DISPLAY_NOT_FOUND,
                    format!("no display with edid_id {edid_id}"),
                )
            })
    }

    /// Open the physical-monitor handle for a GDI device name.
    /// KVM-1 quirk: handle 0 CAN be valid - the success of the open call is
    /// the only validity signal, never the handle value.
    fn physical_handle(&self, device_name: &str) -> Result<HANDLE, CoreError> {
        let mut ctx = CbCtx {
            want: device_name.to_string(),
            found: None,
        };
        let raw = &mut ctx as *mut CbCtx;
        unsafe {
            if !EnumDisplayMonitors(None, None, Some(monitor_cb), LPARAM(raw as isize)).as_bool() {
                return Err(CoreError::new(E_DDC_FAILED, "EnumDisplayMonitors failed"));
            }
        }
        ctx.found.ok_or_else(|| {
            CoreError::new(
                E_DISPLAY_NOT_FOUND,
                format!("no monitor for device {device_name}"),
            )
        })
    }
}

struct CbCtx {
    want: String,
    found: Option<HANDLE>,
}

unsafe extern "system" fn monitor_cb(
    hmon: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut CbCtx);
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if GetMonitorInfoW(hmon, &mut info as *mut MONITORINFOEXW as *mut _).as_bool() {
        let name = String::from_utf16_lossy(&info.szDevice)
            .trim_end_matches('\0')
            .to_string();
        if name == ctx.want {
            let mut arr = [PHYSICAL_MONITOR::default(); 1];
            if GetPhysicalMonitorsFromHMONITOR(hmon, &mut arr).is_ok() {
                ctx.found = Some(arr[0].hPhysicalMonitor);
            }
        }
    }
    BOOL(1) // continue enumeration
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    String::from_utf16_lossy(buf)
        .trim_end_matches('\0')
        .to_string()
}

/// Enumerate monitors: GDI device -> monitor DeviceID -> registry EDID
/// (via candidate walk) -> identity. Attached monitors that cannot be
/// resolved leave a diag trace instead of vanishing silently.
fn enumerate() -> Vec<WinDisplay> {
    // Each refresh replaces the previous run's diag: read/write paths also
    // refresh when the cache is empty, and diag must never accumulate.
    let _ = take_enumeration_diag();
    let mut out = Vec::new();
    let mut used_edids: Vec<[u8; 128]> = Vec::new();
    let mut i: u32 = 0;
    loop {
        let mut dd = DISPLAY_DEVICEW::default();
        dd.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
        if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), i, &mut dd, 0) }.as_bool() {
            if i == 0 {
                push_diag(format!(
                    "EnumDisplayDevicesW returned FALSE at index 0 (GetLastError={})",
                    unsafe { GetLastError().0 }
                ));
            }
            break;
        }
        i += 1;
        if dd.StateFlags.0 & 1 /* DISPLAY_DEVICE_ATTACHED_TO_DESKTOP */ == 0 {
            continue;
        }
        // Second-level enum gets the MONITOR device with its DeviceID.
        // KVM-1 probe: try indices 0..3 and keep the MONITOR\ (or DISPLAY\)
        // entry - index 0 is not guaranteed to be the monitor record.
        let dev_name = from_wide(&dd.DeviceName);
        let dev_name_w = wide(&dev_name);
        let mut device_id = String::new();
        for j in 0..4u32 {
            let mut md = DISPLAY_DEVICEW::default();
            md.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
            if !unsafe { EnumDisplayDevicesW(PCWSTR(dev_name_w.as_ptr()), j, &mut md, 0) }.as_bool()
            {
                break;
            }
            let id = from_wide(&md.DeviceID);
            if id.starts_with("MONITOR\\") || id.starts_with("DISPLAY\\") {
                device_id = id;
                break;
            }
        }
        if device_id.is_empty() {
            push_diag(format!(
                "{dev_name}: attached, but no MONITOR/DISPLAY entry in second-level enum"
            ));
            continue;
        }
        match resolve_identity(&device_id, &mut used_edids) {
            Ok(EdidIdentity {
                manufacturer,
                product_code,
                serial_number,
                serial_string,
                model_name,
            }) => {
                out.push(WinDisplay {
                    edid_id: edid_identity(
                        &manufacturer,
                        product_code,
                        serial_number,
                        &serial_string,
                    ),
                    manufacturer,
                    product_code,
                    model_name,
                    serial_string,
                    device_name: dev_name,
                });
            }
            Err(reason) => {
                push_diag(format!(
                    "{dev_name}: attached ({device_id}) dropped - {reason}"
                ));
            }
        }
    }
    out
}

/// EDID resolution: registry candidate walk + sibling scan, all path logic
/// in the pure (cross-testable) edid_registry module.
fn resolve_identity(device_id: &str, used: &mut Vec<[u8; 128]>) -> Result<EdidIdentity, String> {
    edid_registry::resolve_edid(device_id, &read_edid_value, &registry_child_keys, used)
}

/// Read `Device Parameters\EDID` for a device-instance subkey under HKLM.
fn read_edid_value(device_key: &str) -> Option<Vec<u8>> {
    let subkey = format!(r"{device_key}\Device Parameters");
    let subkey_w = wide(&subkey);
    let mut hkey = HKEY::default();
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey_w.as_ptr()),
            None,
            KEY_READ,
            &mut hkey,
        )
    };
    if opened.is_err() {
        return None;
    }
    let name_w = wide("EDID");
    let mut ty = REG_VALUE_TYPE(0);
    let mut buf = [0u8; 512];
    let mut len: u32 = buf.len() as u32;
    let res = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name_w.as_ptr()),
            None,
            Some(&mut ty),
            Some(buf.as_mut_ptr()),
            Some(&mut len),
        )
    };
    unsafe {
        let _ = RegCloseKey(hkey);
    };
    if res.is_err() || ty != REG_BINARY || len as usize > buf.len() {
        return None;
    }
    Some(buf[..len as usize].to_vec())
}

/// List child subkey names of a registry key (for the sibling-instance scan).
fn registry_child_keys(key: &str) -> Vec<String> {
    let key_w = wide(key);
    let mut hkey = HKEY::default();
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key_w.as_ptr()),
            None,
            KEY_READ,
            &mut hkey,
        )
    };
    if opened.is_err() {
        return Vec::new();
    }
    let mut names = Vec::new();
    let mut index: u32 = 0;
    loop {
        let mut buf = [0u16; 256];
        // RegEnumKeyW returns ERROR_SUCCESS(0) with the name in `buf`,
        // ERROR_NO_MORE_ITEMS(259) at the end.
        let res = unsafe { RegEnumKeyW(hkey, index, Some(&mut buf)) };
        if res.0 != 0 {
            break;
        }
        let nul = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        names.push(String::from_utf16_lossy(&buf[..nul]));
        index += 1;
    }
    unsafe {
        let _ = RegCloseKey(hkey);
    };
    names
}

impl Backend for WindowsDdcBackend {
    fn name(&self) -> &'static str {
        "windows_dxva2"
    }

    fn refresh_displays(&mut self) -> Result<(), CoreError> {
        self.refresh_cache();
        Ok(())
    }

    fn list_displays(&mut self) -> Result<Vec<DisplayInfo>, String> {
        self.refresh_cache();
        Ok(self
            .cache
            .iter()
            .enumerate()
            .map(|(i, d)| DisplayInfo {
                edid_id: d.edid_id.clone(),
                manufacturer: d.manufacturer.clone(),
                product_code: d.product_code,
                model_name: d.model_name.clone(),
                serial_string: d.serial_string.clone(),
                display_index: (i + 1) as u32,
                builtin: false, // laptop panels expose no separate monitor enum here
                is_main: i == 0,
                ddc: DdcCapability::Unknown,
            })
            .collect())
    }

    fn read_input(&mut self, edid_id: &str) -> Result<VcpRead, CoreError> {
        if self.cache.is_empty() {
            self.refresh_cache();
        }
        let d = self.find(edid_id)?;
        let handle = self.physical_handle(&d.device_name)?;
        let mut cur = 0u32;
        let mut max = 0u32;
        let res = unsafe {
            GetVCPFeatureAndVCPFeatureReply(
                handle,
                VCP_INPUT,
                None,
                &mut cur as *mut u32,
                Some(&mut max as *mut u32),
            )
        };
        // Capture before any other Win32 call (Destroy overwrites it).
        let gle = unsafe { GetLastError().0 };
        unsafe {
            let _ = DestroyPhysicalMonitor(handle);
        };
        // dxva2 BOOL: nonzero = success (KVM-1 probe marshals as bool).
        if res != 0 {
            Ok(VcpRead {
                value: cur as u16,
                max: max as u16,
            })
        } else {
            Err(CoreError::new(
                E_DDC_NOT_READABLE,
                format!(
                    "GetVCPFeatureAndVCPFeatureReply failed (GetLastError=0x{gle:08X}; 0xC0262589-class failures = monitor not on this input, per KVM-1 control matrix)"
                ),
            ))
        }
    }

    fn write_input(&mut self, edid_id: &str, value: u16) -> Result<VcpWriteOutcome, CoreError> {
        if self.cache.is_empty() {
            self.refresh_cache();
        }
        let device_name = self.find(edid_id)?.device_name.clone();
        // Do not delay the SetVCPFeature command with an off-input read.
        let previous = None;
        let handle = self.physical_handle(&device_name)?;
        let res = unsafe { SetVCPFeature(handle, VCP_INPUT, value as u32) };
        // Capture before any other Win32 call (Destroy overwrites it).
        let gle = unsafe { GetLastError().0 };
        unsafe {
            let _ = DestroyPhysicalMonitor(handle);
        };
        // dxva2 BOOL: nonzero = success (KVM-1 probe marshals as bool).
        let commanded = res != 0;
        let error = if commanded {
            None
        } else {
            Some(format!("SetVCPFeature failed (GetLastError=0x{gle:08X})"))
        };
        Ok(VcpWriteOutcome {
            commanded,
            previous,
            error,
            evidence: json!({ "api": "dxva2 SetVCPFeature", "vcp": VCP_INPUT, "value": value }),
        })
    }
}
