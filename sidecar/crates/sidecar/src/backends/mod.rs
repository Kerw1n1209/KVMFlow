//! Backend contracts. Platform adapters implement `Backend` for DDC/display
//! work and provide a USB snapshot source for the poller. All decision logic
//! lives in kvmflow-core; adapters only perform system calls.

use kvmflow_core::errors::CoreError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsbDevice {
    /// "vid:pid:serial" - the same key shape the KVM-1 probes logged.
    pub key: String,
    pub vid_pid: String,
    pub serial: String,
    pub product: String,
    pub vendor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DisplayInfo {
    pub edid_id: String,
    pub manufacturer: String,
    pub product_code: u16,
    pub model_name: String,
    pub serial_string: String,
    /// Backend-local index used to address the display for DDC
    /// (m1ddc display number on macOS, monitor ordinal on Windows).
    pub display_index: u32,
    pub builtin: bool,
    pub is_main: bool,
    /// Result of a single read-only VCP 60 probe (1 try).
    pub ddc: DdcCapability,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DdcCapability {
    Unknown,
    Unavailable {
        reason: String,
    },
    /// VCP 60 read succeeded; carries the value at enumeration time.
    Available {
        input: u16,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcpRead {
    pub value: u16,
    pub max: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcpWriteOutcome {
    /// DDC write accepted (machine_observed only - never picture proof).
    pub commanded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Backend evidence (argv/exit codes, raw reply...) for the session log.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub evidence: Value,
}

/// Passthrough text fields (manufacturer / model_name / serial_string) are
/// rendered verbatim by the wizard, so backend garbage leaks straight into
/// the UI. Observed on macOS (KVM-5 session, fixed in KVM-7 T7): m1ddc
/// reports "(null)" Objective-C literals and OUI-style placeholders
/// ("00-10-fa") for displays without a real EDID identity (built-in
/// panels). Single copy - platform adapters call these on every text field
/// that crosses from OS enumeration into DisplayInfo. Windows derives its
/// fields from binary EDID descriptors (printable ASCII only), which cannot
/// produce these literals, so it has no call site.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn sanitize_text_field(s: &str) -> String {
    let t = s.trim();
    if t.eq_ignore_ascii_case("(null)")
        || t.eq_ignore_ascii_case("(nil)")
        || t.eq_ignore_ascii_case("(none)")
    {
        String::new()
    } else {
        t.to_string()
    }
}

/// EDID manufacturer IDs are exactly three letters ("SAC", "GSM", "SAM").
/// A "xx-xx-xx" hex triple is a MAC-address-style placeholder some pipelines
/// print instead - not an identity. Empty it so the identity falls back to
/// UNK instead of parading as a manufacturer.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn sanitize_manufacturer(s: &str) -> String {
    let t = sanitize_text_field(s);
    let b = t.as_bytes();
    let is_oui = t.len() == 8
        && b[2] == b'-'
        && b[5] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, &c)| i == 2 || i == 5 || c.is_ascii_hexdigit());
    if is_oui {
        String::new()
    } else {
        t
    }
}

pub trait Backend: Send {
    fn name(&self) -> &'static str;
    fn simulated(&self) -> bool {
        false
    }
    /// Refresh platform display handles after topology changes such as lid
    /// close/open, sleep/wake, docking and hot-plug. Backends without cached
    /// handles may keep the default no-op implementation.
    fn refresh_displays(&mut self) -> Result<(), CoreError> {
        Ok(())
    }
    /// Best-effort bounded refresh for latency-sensitive probes. Backends
    /// without a native timeout keep the normal behavior.
    fn refresh_displays_with_timeout(&mut self, _timeout: Duration) -> Result<(), CoreError> {
        self.refresh_displays()
    }
    fn list_displays(&mut self) -> Result<Vec<DisplayInfo>, String>;
    /// Read VCP 60. Only reliable while this host is the active input
    /// (KVM-1 control matrix); degraded reads must return E_DDC_NOT_READABLE.
    fn read_input(&mut self, edid_id: &str) -> Result<VcpRead, CoreError>;
    /// Best-effort bounded read for latency-sensitive probes. Backends without
    /// a native timeout keep the normal behavior.
    fn read_input_with_timeout(
        &mut self,
        edid_id: &str,
        _timeout: Duration,
    ) -> Result<VcpRead, CoreError> {
        self.read_input(edid_id)
    }
    /// Write VCP 60. One logical attempt (retry loop lives in the runtime).
    fn write_input(&mut self, edid_id: &str, value: u16) -> Result<VcpWriteOutcome, CoreError>;
}

/// Windows registry EDID resolution model - pure logic (no Win32), compiled
/// on every platform so the Windows enumeration path has a test seam; only
/// the Windows adapter links it in production builds.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub mod edid_registry;
pub mod simulated;
pub mod usb_port;
/// DDC/CI VCP code table - single copy, hex-canonical (KVM-8).
pub mod vcp_codes;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_field_clears_objc_null_literals() {
        assert_eq!(sanitize_text_field("(null)"), "");
        assert_eq!(sanitize_text_field("(NULL)"), "");
        assert_eq!(sanitize_text_field("(nil)"), "");
        assert_eq!(sanitize_text_field("(none)"), "");
        assert_eq!(sanitize_text_field("  (null)  "), "");
    }

    #[test]
    fn text_field_keeps_real_values_but_trims() {
        assert_eq!(sanitize_text_field("G73"), "G73");
        assert_eq!(sanitize_text_field("  Color LCD "), "Color LCD");
        assert_eq!(sanitize_text_field(""), "");
    }

    #[test]
    fn manufacturer_clears_oui_placeholder() {
        // KVM-5 observation: built-in panel reports manufacturer as an
        // OUI-style MAC placeholder instead of an EDID 3-letter ID.
        assert_eq!(sanitize_manufacturer("00-10-fa"), "");
        assert_eq!(sanitize_manufacturer("AA-BB-CC"), "");
    }

    #[test]
    fn manufacturer_keeps_real_edid_ids() {
        assert_eq!(sanitize_manufacturer("SAC"), "SAC");
        assert_eq!(sanitize_manufacturer("GSM"), "GSM");
        assert_eq!(sanitize_manufacturer("(null)"), "");
    }

    #[test]
    fn sanitized_builtin_identity_falls_back_to_unk() {
        // What the whole pipeline must produce for a built-in panel whose
        // m1ddc identity is pure placeholder: no fake manufacturer survives.
        let manufacturer = sanitize_manufacturer("00-10-fa");
        let model_name = sanitize_text_field("(null)");
        assert_eq!(manufacturer, "");
        assert_eq!(model_name, "");
        let edid_id = kvmflow_core::edid_identity(&manufacturer, 0x2763, 0, "");
        assert!(edid_id.starts_with("UNK-"), "got {edid_id}");
    }
}
