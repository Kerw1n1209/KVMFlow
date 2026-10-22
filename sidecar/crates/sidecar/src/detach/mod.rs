//! Display "detach": take a display out of this computer's desktop while its
//! monitor shows another computer, and bring it back later. `TopologyManager`
//! implements the mixed-mode core's `crate::topology::DisplayTopology` over a
//! platform adapter.
//!
//! Layers: `crate::backends::{macos,windows}::topology` perform system calls
//! (`PlatformTopology`); `plan` holds the pure safety rules; `resolve` re-finds
//! a recorded monitor by hardware identity; `store` persists detach records;
//! `manager` ties them together.

mod manager;
mod plan;
mod resolve;
mod store;

pub use manager::TopologyManager;

use crate::topology::DisplayTopology;
use std::path::PathBuf;

/// File name of the detach records inside the sidecar state directory.
pub const RECORDS_FILE: &str = "display-detach.json";

/// Environment switch for the (not yet verified on hardware) Windows detach.
/// The same adapter is also enabled by the cargo feature
/// `windows-display-detach`. The switch only gates *new* detaches: records
/// from an earlier run are always restored.
pub const WINDOWS_ENV_SWITCH: &str = "KVMFLOW_WINDOWS_DISPLAY_DETACH";

/// The platform adapter for `backend`, or `None` when this backend has no
/// display topology (simulated, unknown).
pub fn platform_topology(
    backend: &str,
    records_file: PathBuf,
    ddc_binary: Option<&str>,
) -> Option<Box<dyn DisplayTopology>> {
    let _ = (&records_file, ddc_binary);
    match backend {
        #[cfg(target_os = "macos")]
        "macos_m1ddc" => Some(Box::new(TopologyManager::new(
            crate::backends::macos::topology::MacTopology::new(ddc_binary),
            records_file,
            true,
        ))),
        #[cfg(target_os = "windows")]
        "windows_dxva2" => Some(Box::new(TopologyManager::new(
            crate::backends::windows::topology::WindowsTopology::new(),
            records_file,
            windows_detach_enabled(),
        ))),
        _ => None,
    }
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn windows_detach_enabled() -> bool {
    cfg!(feature = "windows-display-detach")
        || std::env::var(WINDOWS_ENV_SWITCH).is_ok_and(|v| v == "1")
}
