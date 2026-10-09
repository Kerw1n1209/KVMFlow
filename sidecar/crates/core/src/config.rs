//! Config schema v1 - the single copy. The sidecar is the only reader/writer
//! of the config file; the Electron shell manipulates it exclusively through
//! protocol requests so the schema cannot fork per platform.

use crate::debounce::DebounceParams;
use crate::errors::{CoreError, E_CONFIG_INVALID};
use crate::multidevice::SwitchGroupSnapshot;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SCHEMA_VERSION: u32 = 1;
pub const MULTI_DEVICE_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub schema_version: u32,
    /// Human label for this host, shown in logs/diagnostics only.
    #[serde(default)]
    pub host_label: String,
    pub monitors: Vec<MonitorConfig>,
    pub trigger: TriggerConfig,
    #[serde(default)]
    pub advanced: AdvancedConfig,
}

/// Schema v2 stores only what this device knows authoritatively: its own
/// display inputs. A synchronized group snapshot supplies the destination
/// device inputs needed by the source-side fast path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MultiDeviceConfig {
    pub schema_version: u32,
    pub local_device: LocalDeviceProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch_group: Option<SwitchGroupSnapshot>,
    pub trigger: TriggerConfig,
    #[serde(default)]
    pub advanced: AdvancedConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LocalDeviceProfile {
    /// Server-issued after account/device registration. A v1 migration uses a
    /// local placeholder until the user joins a switch group.
    pub device_id: String,
    #[serde(default)]
    pub host_label: String,
    pub monitors: Vec<LocalMonitorConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LocalMonitorConfig {
    pub fingerprint: String,
    #[serde(default)]
    pub label: String,
    /// The VCP input value observed while *this* device owns the picture.
    pub local_input: u16,
    #[serde(default)]
    pub source: InputSource,
    /// Compatibility-only data imported from schema v1. It is local to this
    /// installation and is never treated as another device's source of truth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_remote_preset: Option<LegacyRemotePreset>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LegacyRemotePreset {
    pub value: u16,
    pub source: AwayInputSource,
    pub confirmed: bool,
}

/// Distinguishes a document read from disk during the migration window. The
/// sidecar can keep schema v1 alive in explicit legacy mode while new account
/// flows write schema v2.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigDocument {
    LegacyV1(Config),
    MultiDeviceV2(MultiDeviceConfig),
}

impl ConfigDocument {
    pub fn parse_json(text: &str) -> Result<Self, CoreError> {
        let value: Value = serde_json::from_str(text)
            .map_err(|error| CoreError::new(E_CONFIG_INVALID, format!("config parse: {error}")))?;
        Self::from_value(value)
    }

    pub fn from_value(value: Value) -> Result<Self, CoreError> {
        let schema_version = value
            .get("schema_version")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                CoreError::new(
                    E_CONFIG_INVALID,
                    "config schema_version is missing or invalid",
                )
            })?;
        match schema_version as u32 {
            SCHEMA_VERSION => serde_json::from_value::<Config>(value)
                .map(Self::LegacyV1)
                .map_err(|error| {
                    CoreError::new(E_CONFIG_INVALID, format!("schema v1 config parse: {error}"))
                }),
            MULTI_DEVICE_SCHEMA_VERSION => serde_json::from_value::<MultiDeviceConfig>(value)
                .map(Self::MultiDeviceV2)
                .map_err(|error| {
                    CoreError::new(E_CONFIG_INVALID, format!("schema v2 config parse: {error}"))
                }),
            version => Err(CoreError::new(
                E_CONFIG_INVALID,
                format!("unsupported config schema_version {version}"),
            )),
        }
    }

    pub fn into_v2(self) -> MultiDeviceConfig {
        match self {
            Self::LegacyV1(config) => MultiDeviceConfig::from_legacy(config),
            Self::MultiDeviceV2(config) => config,
        }
    }
}

impl MultiDeviceConfig {
    /// Lossless migration: no v1 `away_input` is assigned to an invented peer
    /// device. It remains an explicitly legacy, local-only preset.
    pub fn from_legacy(config: Config) -> Self {
        Self {
            schema_version: MULTI_DEVICE_SCHEMA_VERSION,
            local_device: LocalDeviceProfile {
                device_id: "legacy-local".into(),
                host_label: config.host_label,
                monitors: config
                    .monitors
                    .into_iter()
                    .map(|monitor| LocalMonitorConfig {
                        fingerprint: monitor.edid_id,
                        label: monitor.label,
                        local_input: monitor.here_input,
                        source: monitor.here_input_source,
                        legacy_remote_preset: Some(LegacyRemotePreset {
                            value: monitor.away_input,
                            source: monitor.away_input_source,
                            confirmed: monitor.away_input_confirmed,
                        }),
                    })
                    .collect(),
            },
            switch_group: None,
            trigger: config.trigger,
            advanced: config.advanced,
        }
    }

    pub fn validate(&self) -> Result<(), CoreError> {
        let mut problems: Vec<String> = Vec::new();
        if self.schema_version != MULTI_DEVICE_SCHEMA_VERSION {
            problems.push(format!(
                "schema_version must be {MULTI_DEVICE_SCHEMA_VERSION}, got {}",
                self.schema_version
            ));
        }
        if self.local_device.device_id.trim().is_empty() {
            problems.push("local_device.device_id is empty".into());
        }
        if self.local_device.monitors.is_empty() {
            problems.push("local_device.monitors is empty".into());
        }
        let mut fingerprints = std::collections::BTreeSet::new();
        for (index, monitor) in self.local_device.monitors.iter().enumerate() {
            if monitor.fingerprint.trim().is_empty() {
                problems.push(format!(
                    "local_device.monitors[{index}].fingerprint is empty"
                ));
            }
            if !fingerprints.insert(monitor.fingerprint.clone()) {
                problems.push(format!(
                    "local_device.monitors[{index}].fingerprint duplicates an earlier monitor"
                ));
            }
        }
        if let Some(group) = &self.switch_group {
            if let Err(error) = group.validate() {
                problems.push(format!("switch_group: {error}"));
            } else if let Some(device) = group
                .devices
                .iter()
                .find(|device| device.device_id == self.local_device.device_id)
            {
                let local_fingerprints: std::collections::BTreeSet<&str> = self
                    .local_device
                    .monitors
                    .iter()
                    .map(|monitor| monitor.fingerprint.as_str())
                    .collect();
                let group_fingerprints: std::collections::BTreeSet<&str> = device
                    .monitors
                    .iter()
                    .map(|monitor| monitor.fingerprint.as_str())
                    .collect();
                if local_fingerprints != group_fingerprints {
                    problems.push(
                        "switch_group local device monitor set differs from local_device".into(),
                    );
                }
                for monitor in &self.local_device.monitors {
                    match device
                        .monitors
                        .iter()
                        .find(|entry| entry.fingerprint == monitor.fingerprint)
                    {
                        Some(entry) if entry.local_input == monitor.local_input => {}
                        Some(_) => problems.push(format!(
                            "switch_group local input for `{}` differs from local_device",
                            monitor.fingerprint
                        )),
                        None => problems.push(format!(
                            "switch_group local device is missing monitor `{}`",
                            monitor.fingerprint
                        )),
                    }
                }
            } else {
                problems.push("switch_group does not contain local_device.device_id".into());
            }
        }
        // A local v2 profile may be saved before a physical USB Switch is
        // calibrated. The trigger becomes mandatory only when a complete
        // group is armed; otherwise the first-device setup flow would have
        // to invent a peer or ask the user to switch too early.
        validate_advanced(&self.advanced, &mut problems);
        if problems.is_empty() {
            Ok(())
        } else {
            Err(CoreError::new(E_CONFIG_INVALID, problems.join("; ")))
        }
    }

    pub fn ready_for_group_switch(&self) -> bool {
        self.validate().is_ok() && self.switch_group.is_some() && self.has_valid_trigger()
    }

    pub fn has_valid_trigger(&self) -> bool {
        let mut problems = Vec::new();
        validate_trigger(&self.trigger, &mut problems);
        problems.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MonitorConfig {
    /// Stable cross-platform identity from EDID (e.g. SAC-2763-S:0000000000001).
    pub edid_id: String,
    #[serde(default)]
    pub label: String,
    /// VCP 60 value that selects THIS host on this monitor (learned while this
    /// host holds the picture - the only reliable read per KVM-1 control matrix).
    pub here_input: u16,
    /// VCP 60 value that selects the OTHER host (heuristic prefill or manual
    /// entry from the other machine's wizard; must be confirmed by the final
    /// supervised switch test).
    pub away_input: u16,
    #[serde(default)]
    pub here_input_source: InputSource,
    #[serde(default)]
    pub away_input_source: AwayInputSource,
    /// Set once the user confirmed the away input actually moved the picture
    /// during the wizard's final test.
    #[serde(default)]
    pub away_input_confirmed: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputSource {
    #[default]
    Unknown,
    /// Read via DDC while this host was the active input.
    LearnedActiveRead,
    Manual,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AwayInputSource {
    #[default]
    Unknown,
    /// here_input + 1 prefill - both KVM-1 monitors happened to follow this
    /// (15/16 and 7/8), but it is a guess and MUST be confirmed.
    HeuristicPrefill,
    /// Typed in from the other machine's wizard readout.
    ManualEntry,
    /// Confirmed by the supervised final test.
    ConfirmedByTest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TriggerConfig {
    /// Anchor device (the switch's hub). Matched by VID:PID only - hubs have
    /// no stable serial, and Windows instance paths are not stable identifiers.
    pub anchor: DevicePattern,
    /// Peripheral members (keyboard/mouse dongles). Matched by VID:PID and,
    /// when a serial is known, additionally by serial.
    #[serde(default)]
    pub members: Vec<DevicePattern>,
    #[serde(default)]
    pub debounce: DebounceParams,
}

impl Default for TriggerConfig {
    fn default() -> Self {
        Self {
            anchor: DevicePattern {
                vid_pid: String::new(),
                serial: None,
            },
            members: Vec::new(),
            debounce: DebounceParams::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DevicePattern {
    /// "1a40:0101" - lowercase hex.
    pub vid_pid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AdvancedConfig {
    #[serde(default)]
    pub ddc_retry: DdcRetry,
    /// USB snapshot poll interval on polling backends.
    #[serde(default = "default_usb_poll_ms")]
    pub usb_poll_ms: u64,
    /// When this host observes the USB trigger group arriving, rewrite its
    /// own monitor inputs as a local fallback. The source-side departure path
    /// remains active regardless of this setting.
    #[serde(default = "default_arrival_correction_enabled")]
    pub arrival_correction_enabled: bool,
}

impl Default for AdvancedConfig {
    fn default() -> Self {
        Self {
            ddc_retry: DdcRetry::default(),
            usb_poll_ms: default_usb_poll_ms(),
            arrival_correction_enabled: default_arrival_correction_enabled(),
        }
    }
}

fn default_usb_poll_ms() -> u64 {
    250
}

fn default_arrival_correction_enabled() -> bool {
    false
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DdcRetry {
    #[serde(default = "default_attempts")]
    pub attempts: u32,
    #[serde(default = "default_delay_ms")]
    pub delay_ms: u64,
}

fn default_attempts() -> u32 {
    3
}
fn default_delay_ms() -> u64 {
    500
}

impl Default for DdcRetry {
    fn default() -> Self {
        Self {
            attempts: default_attempts(),
            delay_ms: default_delay_ms(),
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<(), CoreError> {
        let mut problems: Vec<String> = Vec::new();

        if self.schema_version != SCHEMA_VERSION {
            problems.push(format!(
                "schema_version must be {SCHEMA_VERSION}, got {}",
                self.schema_version
            ));
        }
        if self.monitors.is_empty() {
            problems.push("at least one monitor is required".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for (i, m) in self.monitors.iter().enumerate() {
            if m.edid_id.trim().is_empty() {
                problems.push(format!("monitors[{i}].edid_id is empty"));
            }
            if !seen.insert(m.edid_id.clone()) {
                problems.push(format!("monitors[{i}].edid_id duplicates an earlier entry"));
            }
            if m.here_input == m.away_input {
                problems.push(format!(
                    "monitors[{i}] here_input == away_input ({}); the two hosts must map to different inputs",
                    m.here_input
                ));
            }
        }
        validate_trigger_and_advanced(&self.trigger, &self.advanced, &mut problems);

        if problems.is_empty() {
            Ok(())
        } else {
            Err(CoreError::new(E_CONFIG_INVALID, problems.join("; ")))
        }
    }

    pub fn monitor(&self, edid_id: &str) -> Option<&MonitorConfig> {
        self.monitors.iter().find(|m| m.edid_id == edid_id)
    }

    /// Armed = everything needed for automatic push-away is present.
    pub fn armed_ready(&self) -> bool {
        is_vid_pid(&self.trigger.anchor.vid_pid)
            && !self.monitors.is_empty()
            && self
                .monitors
                .iter()
                .all(|m| m.away_input_source != AwayInputSource::Unknown)
    }
}

fn validate_trigger_and_advanced(
    trigger: &TriggerConfig,
    advanced: &AdvancedConfig,
    problems: &mut Vec<String>,
) {
    validate_trigger(trigger, problems);
    validate_advanced(advanced, problems);
}

fn validate_trigger(trigger: &TriggerConfig, problems: &mut Vec<String>) {
    if !is_vid_pid(&trigger.anchor.vid_pid) {
        problems.push(format!(
            "trigger.anchor.vid_pid `{}` is not xxxx:yyyy hex",
            trigger.anchor.vid_pid
        ));
    }
    if trigger.members.is_empty() {
        problems.push("at least one trigger member (peripheral) is required - quorum is anchor + >=1 peripheral (KVM-1 verified model)".to_string());
    }
    for (index, member) in trigger.members.iter().enumerate() {
        if !is_vid_pid(&member.vid_pid) {
            problems.push(format!(
                "trigger.members[{index}].vid_pid `{}` is not xxxx:yyyy hex",
                member.vid_pid
            ));
        }
    }
    if let Err(error) = trigger.debounce.validate() {
        problems.push(format!("trigger.debounce: {error}"));
    }
}

fn validate_advanced(advanced: &AdvancedConfig, problems: &mut Vec<String>) {
    if let Err(error) = advanced.ddc_retry.validate() {
        problems.push(format!("advanced.ddc_retry: {error}"));
    }
    if advanced.usb_poll_ms == 0 || advanced.usb_poll_ms > 5_000 {
        problems.push(format!(
            "advanced.usb_poll_ms {} outside 1..=5000",
            advanced.usb_poll_ms
        ));
    }
}

fn is_vid_pid(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() == 9
        && bytes[..4].iter().all(|b| b.is_ascii_hexdigit())
        && bytes[4] == b':'
        && bytes[5..].iter().all(|b| b.is_ascii_hexdigit())
        && s.chars().all(|c| !c.is_ascii_uppercase())
}

impl DdcRetry {
    pub fn validate(&self) -> Result<(), String> {
        if self.attempts == 0 || self.attempts > 10 {
            return Err(format!("attempts {} outside 1..=10", self.attempts));
        }
        if self.delay_ms > 10_000 {
            return Err(format!("delay_ms {} above 10000", self.delay_ms));
        }
        Ok(())
    }

    pub fn to_policy(&self) -> crate::retry::RetryPolicy {
        crate::retry::RetryPolicy {
            attempts: self.attempts,
            delay_ms: self.delay_ms,
        }
    }
}

pub fn example_config() -> Config {
    Config {
        schema_version: SCHEMA_VERSION,
        host_label: "example-host".into(),
        monitors: vec![MonitorConfig {
            edid_id: "SAC-2763-S:0000000000001".into(),
            label: "G73".into(),
            here_input: 15,
            away_input: 16,
            here_input_source: InputSource::LearnedActiveRead,
            away_input_source: AwayInputSource::HeuristicPrefill,
            away_input_confirmed: false,
        }],
        trigger: TriggerConfig {
            anchor: DevicePattern {
                vid_pid: "1a40:0101".into(),
                serial: None,
            },
            members: vec![
                DevicePattern {
                    vid_pid: "3837:303c".into(),
                    serial: Some("fixture-usb-3837-303c-1".into()),
                },
                DevicePattern {
                    vid_pid: "373b:10c9".into(),
                    serial: Some("Љ".into()),
                },
            ],
            debounce: DebounceParams::default(),
        },
        advanced: AdvancedConfig::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> Config {
        example_config()
    }

    #[test]
    fn example_config_validates() {
        assert!(valid().validate().is_ok());
    }

    #[test]
    fn arrival_correction_defaults_off_for_existing_configs() {
        let advanced: AdvancedConfig = serde_json::from_value(serde_json::json!({
            "ddc_retry": {"attempts": 2, "delay_ms": 100},
            "usb_poll_ms": 250
        }))
        .unwrap();
        assert!(!advanced.arrival_correction_enabled);
    }

    #[test]
    fn rejects_empty_monitors() {
        let mut c = valid();
        c.monitors.clear();
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_equal_inputs() {
        let mut c = valid();
        c.monitors[0].away_input = c.monitors[0].here_input;
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_uppercase_and_malformed_vidpid() {
        let mut c = valid();
        c.trigger.anchor.vid_pid = "1A40:0101".into();
        assert!(c.validate().is_err());
        c.trigger.anchor.vid_pid = "1a40010101".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_duplicate_edid_ids() {
        let mut c = valid();
        c.monitors.push(c.monitors[0].clone());
        assert!(c.validate().is_err());
    }

    #[test]
    fn round_trips_through_json() {
        let c = valid();
        let j = serde_json::to_string(&c).unwrap();
        let back: Config = serde_json::from_str(&j).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn armed_ready_requires_trigger_and_away_inputs() {
        assert!(valid().armed_ready());
        let mut c = valid();
        c.trigger.anchor.vid_pid = String::new();
        assert!(!c.armed_ready());
        let mut c = valid();
        c.monitors[0].away_input_source = AwayInputSource::Unknown;
        assert!(!c.armed_ready());
    }

    #[test]
    fn v1_migration_preserves_remote_value_as_local_legacy_preset() {
        let mut legacy = valid();
        legacy.host_label = "工作 Mac".into();
        legacy.monitors[0].away_input = 27;
        legacy.monitors[0].away_input_source = AwayInputSource::ConfirmedByTest;
        legacy.monitors[0].away_input_confirmed = true;

        let migrated = MultiDeviceConfig::from_legacy(legacy);
        assert_eq!(migrated.schema_version, MULTI_DEVICE_SCHEMA_VERSION);
        assert_eq!(migrated.local_device.device_id, "legacy-local");
        assert_eq!(migrated.local_device.host_label, "工作 Mac");
        assert_eq!(migrated.local_device.monitors[0].local_input, 15);
        assert_eq!(
            migrated.local_device.monitors[0].legacy_remote_preset,
            Some(LegacyRemotePreset {
                value: 27,
                source: AwayInputSource::ConfirmedByTest,
                confirmed: true,
            })
        );
        assert!(migrated.switch_group.is_none());
        assert!(migrated.validate().is_ok());
        assert!(!migrated.ready_for_group_switch());
    }

    #[test]
    fn config_document_selects_schema_and_rejects_unknown_versions() {
        let legacy_json = serde_json::to_string(&valid()).unwrap();
        assert!(matches!(
            ConfigDocument::parse_json(&legacy_json).unwrap(),
            ConfigDocument::LegacyV1(_)
        ));

        let v2 = MultiDeviceConfig::from_legacy(valid());
        let v2_json = serde_json::to_string(&v2).unwrap();
        assert!(matches!(
            ConfigDocument::parse_json(&v2_json).unwrap(),
            ConfigDocument::MultiDeviceV2(_)
        ));

        let error = ConfigDocument::parse_json(r#"{"schema_version":99}"#).unwrap_err();
        assert!(error
            .message
            .contains("unsupported config schema_version 99"));
    }

    #[test]
    fn v2_does_not_require_an_away_input_before_group_join() {
        let mut v2 = MultiDeviceConfig::from_legacy(valid());
        v2.local_device.monitors[0].legacy_remote_preset = None;
        assert!(v2.validate().is_ok());
        assert!(!v2.ready_for_group_switch());
    }

    #[test]
    fn v2_local_profile_can_wait_for_usb_calibration_without_being_armed() {
        let mut v2 = MultiDeviceConfig::from_legacy(valid());
        v2.trigger = TriggerConfig::default();
        assert!(v2.validate().is_ok());
        assert!(!v2.has_valid_trigger());
        assert!(!v2.ready_for_group_switch());
    }

    #[test]
    fn v2_rejects_group_snapshot_that_disagrees_about_local_monitors() {
        use crate::multidevice::{GroupDevice, MonitorInput, SwitchGroupSnapshot};

        let mut v2 = MultiDeviceConfig::from_legacy(valid());
        v2.switch_group = Some(SwitchGroupSnapshot {
            group_id: "desk".into(),
            revision: 1,
            devices: vec![
                GroupDevice {
                    device_id: "legacy-local".into(),
                    name: "工作 Mac".into(),
                    port_index: 1,
                    monitors: vec![
                        MonitorInput {
                            fingerprint: "SAC-2763-S:0000000000001".into(),
                            label: "G73".into(),
                            local_input: 15,
                        },
                        MonitorInput {
                            fingerprint: "SECOND".into(),
                            label: "第二台显示器".into(),
                            local_input: 17,
                        },
                    ],
                },
                GroupDevice {
                    device_id: "windows".into(),
                    name: "Windows".into(),
                    port_index: 2,
                    monitors: vec![
                        MonitorInput {
                            fingerprint: "SAC-2763-S:0000000000001".into(),
                            label: "G73".into(),
                            local_input: 16,
                        },
                        MonitorInput {
                            fingerprint: "SECOND".into(),
                            label: "第二台显示器".into(),
                            local_input: 18,
                        },
                    ],
                },
            ],
        });
        let error = v2.validate().unwrap_err();
        assert!(error
            .message
            .contains("monitor set differs from local_device"));
        assert!(!v2.ready_for_group_switch());
    }
}
