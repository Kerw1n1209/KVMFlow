//! Log-event model - the single event catalog emitted to the session JSONL
//! file and mirrored to the Electron shell. Machine-readable kinds, not free
//! text, so diagnostics on both platforms have the same shape.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

/// One session-log record. `ts_ms` is UNIX epoch milliseconds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    pub ts_ms: u64,
    pub kind: String,
    pub level: Level,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub fields: Map<String, Value>,
}

/// Event kinds (single copy). See statemachine/debounce for emitters.
pub mod kinds {
    pub const SIDECAR_START: &str = "sidecar.start";
    pub const SIDECAR_READY: &str = "sidecar.ready";
    pub const SIDECAR_SHUTDOWN: &str = "sidecar.shutdown";
    pub const CONFIG_SAVED: &str = "config.saved";
    pub const USB_SNAPSHOT: &str = "usb.snapshot";
    pub const USB_CHANGED: &str = "usb.changed";
    pub const USB_HUB_PROBE: &str = "usb.hub_probe";
    pub const TRIGGER_GROUP_STABLE: &str = "trigger.group_stable";
    pub const TRIGGER_GROUP_LEFT: &str = "trigger.group_left";
    pub const TRIGGER_GROUP_ARRIVED: &str = "trigger.group_arrived";
    pub const TRIGGER_UNEXPECTED_RETURN: &str = "trigger.unexpected_return";
    pub const TRIGGER_BOUNCE_ABSORBED: &str = "trigger.bounce_absorbed";
    pub const WIZARD_BEGIN: &str = "wizard.begin";
    pub const WIZARD_END: &str = "wizard.end";
    pub const WIZARD_CANDIDATES: &str = "wizard.candidates";
    pub const STATE_CHANGE: &str = "state.change";
    pub const SWITCH_PUSH_BEGIN: &str = "switch.push.begin";
    pub const SWITCH_MONITOR_COMMANDED: &str = "switch.monitor.commanded";
    pub const SWITCH_MONITOR_FAILED: &str = "switch.monitor.failed";
    pub const SWITCH_PUSH_REPORT: &str = "switch.push.report";
    pub const DDC_GET: &str = "ddc.get";
    pub const DDC_SET: &str = "ddc.set";
    pub const DISPLAY_LIST: &str = "display.list";
    pub const BACKEND_ERROR: &str = "backend.error";
}
