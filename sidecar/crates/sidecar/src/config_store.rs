//! Loading, migrating, applying and persisting the switch group config.

use super::*;

impl Runtime {
    pub(crate) fn load_config(&mut self) {
        let path = self.config_path.clone();
        match std::fs::read_to_string(&path) {
            Ok(text) => match ConfigDocument::parse_json(&text) {
                Ok(ConfigDocument::LegacyV1(cfg)) => self.migrate_loaded_legacy_config(cfg, &text),
                Ok(ConfigDocument::MultiDeviceV2(cfg)) => {
                    if let Err(e) = cfg.validate() {
                        self.emit_event(
                            kinds::CONFIG_SAVED,
                            Level::Warn,
                            json!({"loaded": "invalid_v2", "error": e.to_string()}),
                        );
                    }
                    self.apply_config(cfg);
                }
                Err(e) => {
                    // The next save would overwrite the only copy of the
                    // user's learned input values, so move it aside first.
                    let preserved = path.with_file_name(format!(
                        "{}.corrupt-{}",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        now_ms()
                    ));
                    let preserved_as = std::fs::rename(&path, &preserved)
                        .ok()
                        .map(|()| preserved.to_string_lossy().into_owned());
                    self.config_recovery = Some(json!({
                        "preservedAs": preserved_as.as_ref().and(preserved.file_name()).map(|name| name.to_string_lossy()),
                        "error": e.to_string(),
                    }));
                    self.emit_event(
                        kinds::CONFIG_SAVED,
                        Level::Warn,
                        json!({
                            "loaded": "parse_error",
                            "error": e.to_string(),
                            "preserved_as": preserved_as,
                        }),
                    );
                }
            },
            Err(_) => {
                self.emit_event(kinds::CONFIG_SAVED, Level::Debug, json!({"loaded": "none"}));
            }
        }
    }

    /// An armed v1 file becomes a runnable two-computer group and is
    /// rewritten as v2 (the original is kept as `<name>.v1-backup`). An
    /// incomplete one is only converted in memory, so nothing on disk is lost
    /// before the user completes setup.
    pub(crate) fn migrate_loaded_legacy_config(&mut self, cfg: Config, original: &str) {
        match MultiDeviceConfig::migrate_legacy(cfg) {
            LegacyMigration::Runnable(config) => {
                let backup = self.config_path.with_file_name(format!(
                    "{}.v1-backup",
                    self.config_path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                ));
                let text = serde_json::to_string_pretty(&config).unwrap_or_default();
                let persisted = write_atomically(&backup, original.as_bytes())
                    .and_then(|()| write_atomically(&self.config_path, text.as_bytes()));
                self.emit_event(
                    kinds::CONFIG_SAVED,
                    if persisted.is_ok() {
                        Level::Info
                    } else {
                        Level::Warn
                    },
                    json!({
                        "loaded": "migrated_from_v1",
                        "runnable": true,
                        "backup": backup.to_string_lossy(),
                        "error": persisted.err().map(|error| error.to_string()),
                    }),
                );
                self.apply_config(config);
            }
            LegacyMigration::NeedsSetup(config) => {
                self.emit_event(
                    kinds::CONFIG_SAVED,
                    Level::Warn,
                    json!({"loaded": "migrated_from_v1", "runnable": false}),
                );
                self.apply_config(config);
            }
        }
    }

    pub(crate) fn apply_config(&mut self, cfg: MultiDeviceConfig) {
        let mut tracker = GroupTracker::new(
            cfg.trigger.anchor.clone(),
            cfg.trigger.members.clone(),
            cfg.trigger.debounce.clone(),
        );
        let keys: Vec<DeviceKey> = self
            .last_snapshot
            .iter()
            .map(|device| DeviceKey {
                vid_pid: device.vid_pid.clone(),
                serial: device.serial.clone(),
            })
            .collect();
        tracker.update(now_ms(), &keys);
        self.tracker = Some(tracker);
        let ready = cfg.ready_for_group_switch();
        self.config = Some(cfg);
        self.enabled = true;
        self.last_multi_device_report = None;
        self.bounce_watch = None;
        let mut actions = self.fsm.handle(Input::SetDisabled(false), now_ms());
        actions.extend(self.fsm.handle(Input::ConfigReady(ready), now_ms()));
        let state_changed = actions
            .iter()
            .any(|action| matches!(action, Action::Notify { kind: "state", .. }));
        self.run_actions(actions);
        // The mode may change without a state change (setup -> armed while
        // learning); announce it once either way.
        if !state_changed {
            let data = self.state_notification();
            self.notify("state", data);
        }
    }

    pub(crate) fn state_notification(&self) -> Value {
        json!({"state": self.fsm.state(), "mode": self.config_mode(), "enabled": self.enabled})
    }

    pub(crate) fn active_trigger(&self) -> Option<TriggerConfig> {
        self.config.as_ref().map(|config| config.trigger.clone())
    }

    pub(crate) fn active_monitor_count(&self) -> usize {
        self.config
            .as_ref()
            .map(|config| config.local_device.monitors.len())
            .unwrap_or(0)
    }

    pub(crate) fn active_retry_policy(&self) -> kvmflow_core::retry::RetryPolicy {
        self.config
            .as_ref()
            .map(|config| config.advanced.ddc_retry.to_policy())
            .unwrap_or_default()
    }

    pub(crate) fn active_config_value(&self) -> Value {
        self.config
            .as_ref()
            .and_then(|config| serde_json::to_value(config).ok())
            .unwrap_or(Value::Null)
    }

    pub(crate) fn config_mode(&self) -> &'static str {
        match &self.config {
            Some(config) if config.ready_for_group_switch() => "multi_device_armed",
            Some(_) => "multi_device_setup",
            None => "none",
        }
    }

    pub(crate) fn persist_and_apply_config(&mut self, id: u64, config: MultiDeviceConfig) {
        let text = serde_json::to_string_pretty(&config).unwrap_or_default();
        let mode = if config.ready_for_group_switch() {
            "multi_device_armed"
        } else {
            "multi_device_setup"
        };
        self.persist_config_text(id, text, mode, move |runtime| runtime.apply_config(config));
    }

    pub(crate) fn persist_config_text<F>(
        &mut self,
        id: u64,
        text: String,
        mode: &'static str,
        apply: F,
    ) where
        F: FnOnce(&mut Self),
    {
        if let Some(parent) = self.config_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match write_atomically(&self.config_path, text.as_bytes()) {
            Ok(()) => {
                self.config_recovery = None;
                apply(self);
                self.emit_event(
                    kinds::CONFIG_SAVED,
                    Level::Info,
                    json!({"path": self.config_path.to_string_lossy(), "mode": mode}),
                );
                self.respond(ResponseLine::ok(
                    id,
                    json!({"saved": true, "path": self.config_path.to_string_lossy(), "mode": mode}),
                ));
            }
            Err(error) => self.respond(ResponseLine::err(id, E_CONFIG_IO, error.to_string())),
        }
    }
}

/// Write to a sibling temp file, flush it to disk, then rename over the
/// target, so a crash or power loss leaves either the old or the new config,
/// never a truncated one. `rename` replaces an existing file on Windows too.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_file_name(format!(
        "{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}
