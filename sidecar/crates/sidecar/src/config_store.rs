//! Loading, migrating, applying and persisting the switch group config.

use super::*;

/// Local public identity, adjacent to S4's private identity.key/identity.dpapi.
/// It is deliberately independent of the user config and never replaces an
/// existing malformed ID. A hard-link publish makes concurrent first starts
/// choose one ID without overwriting another process's identity.
pub(crate) fn load_or_create_device_id(config_path: &Path) -> std::io::Result<String> {
    fn read(path: &Path) -> std::io::Result<String> {
        let id = std::fs::read_to_string(path)?;
        let alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
        if id.len() != 30
            || !id.starts_with("dev-")
            || !id[4..].chars().all(|c| alphabet.contains(c))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid local device identity; file preserved",
            ));
        }
        Ok(id)
    }
    let parent = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("lan");
    std::fs::create_dir_all(&parent)?;
    let path = parent.join("device-id");
    match read(&path) {
        Ok(id) => return Ok(id),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(std::io::Error::other)?;
    let id = kvmflow_core::identity::new_device_id(random);
    let tmp = parent.join(format!(
        ".device-id-{:032x}.tmp",
        u128::from_be_bytes(random)
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(id.as_bytes())?;
        file.sync_all()?;
        match std::fs::hard_link(&tmp, &path) {
            Ok(()) => Ok(id),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => read(&path),
            Err(e) => Err(e),
        }
    })();
    let _ = std::fs::remove_file(tmp);
    result
}

impl Runtime {
    pub(crate) fn load_config(&mut self) {
        let path = self.config_path.clone();
        match std::fs::read_to_string(&path) {
            Ok(text) => match ConfigDocument::parse_json(&text) {
                Ok(ConfigDocument::LegacyV1(cfg)) => self.migrate_loaded_legacy_config(cfg, &text),
                Ok(ConfigDocument::MultiDeviceV2(cfg)) => {
                    let cfg = self.bootstrap_loaded_identity(cfg, &text, true);
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
    /// incomplete one only converts in memory, preserving the original v1 file.
    /// A separate local identity file keeps its device ID stable across restarts.
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
                let text = persisted_config_text(&config);
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
                        "error": persisted.as_ref().err().map(|error| error.to_string()),
                    }),
                );
                let current = if persisted.is_ok() {
                    persisted_config_text(&config)
                } else {
                    original.into()
                };
                let config = if persisted.is_ok() {
                    self.bootstrap_loaded_identity(config, &current, true)
                } else {
                    config
                };
                self.apply_config(config);
            }
            LegacyMigration::NeedsSetup(config) => {
                self.emit_event(
                    kinds::CONFIG_SAVED,
                    Level::Warn,
                    json!({"loaded": "migrated_from_v1", "runnable": false}),
                );
                let config = self.bootstrap_loaded_identity(config, original, false);
                self.apply_config(config);
            }
        }
    }

    fn bootstrap_loaded_identity(
        &mut self,
        config: MultiDeviceConfig,
        original: &str,
        persist: bool,
    ) -> MultiDeviceConfig {
        if config.local_device.device_id != kvmflow_core::config::LEGACY_LOCAL_DEVICE_ID {
            return config;
        }
        let mut candidate = config.clone();
        let result = (|| {
            let stable_id = load_or_create_device_id(&self.config_path)?;
            bootstrap_device_identity(
                &self.config_path,
                &mut candidate,
                original,
                &stable_id,
                now_ms(),
                persist,
            )
        })();
        match result {
            Ok(_) => candidate,
            Err(error) => {
                self.emit_event(
                    "lan.identity.bootstrap_failed",
                    Level::Warn,
                    json!({"error":error.to_string()}),
                );
                config
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
        self.observe_mixed_holder_baseline();
        self.reconcile_mixed();
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
        json!({"state": self.fsm.state(), "mode": self.config_mode(), "enabled": self.enabled,
            "lan":self.lan_state(), "mixed":self.mixed_state(), "lastLayoutApplication": self.last_layout_application, "effectiveLayouts": self.effective_layouts_value()})
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
        let mut value = self
            .config
            .as_ref()
            .and_then(|config| serde_json::to_value(config).ok())
            .unwrap_or(Value::Null);
        if let Some(sync) = value
            .pointer_mut("/switch_group/sync")
            .and_then(Value::as_object_mut)
        {
            sync.remove("profile_cache");
            sync.remove("archived_arrivals");
        }
        value
    }

    pub(crate) fn config_mode(&self) -> &'static str {
        match &self.config {
            Some(config) if config.ready_for_group_switch() => "multi_device_armed",
            Some(_) => "multi_device_setup",
            None => "none",
        }
    }

    pub(crate) fn persist_and_apply_config(&mut self, id: u64, mut config: MultiDeviceConfig) {
        if config.local_device.device_id == kvmflow_core::config::LEGACY_LOCAL_DEVICE_ID {
            let stable = load_or_create_device_id(&self.config_path);
            match stable.and_then(|stable| {
                bootstrap_device_identity(
                    &self.config_path,
                    &mut config,
                    "",
                    &stable,
                    now_ms(),
                    false,
                )
            }) {
                Ok(_) => {}
                Err(error) => {
                    self.respond(ResponseLine::err(id, E_CONFIG_IO, error.to_string()));
                    return;
                }
            }
        }
        let mut config = match self.prepare_mixed_config(config) {
            Ok(config) => config,
            Err(error) => {
                self.respond(ResponseLine::err(id, error.code, error.message));
                return;
            }
        };
        // A local settings save is a section edit: stamp changed sections so
        // LAN peers can order it against their own replicas (S7). The caller's
        // literal form (device/assignment order) is preserved: only counters
        // and section metadata are copied from the stamped result.
        if let Some(old) = self.config.as_ref().and_then(|c| c.switch_group.as_ref()) {
            if let Some(new) = config.switch_group.as_mut() {
                let section_unchanged = {
                    let mut a = old.clone();
                    let mut b = new.clone();
                    kvmflow_core::sync::normalize_snapshot(&mut a);
                    kvmflow_core::sync::normalize_snapshot(&mut b);
                    a == b
                };
                if !section_unchanged {
                    match kvmflow_core::sync::stamp_local_edit(
                        old,
                        new,
                        &config.local_device.device_id,
                        now_ms(),
                    ) {
                        Ok((stamped, _)) => {
                            new.revision = stamped.revision;
                            new.sync = stamped.sync;
                            for (device, stamped_device) in
                                new.devices.iter_mut().zip(stamped.devices.iter())
                            {
                                device.profile_rev = stamped_device.profile_rev;
                            }
                            if let Some(mixed) = stamped.mixed.as_ref() {
                                if let Some(target) = new.mixed.as_mut() {
                                    target.revision = mixed.revision;
                                }
                            }
                        }
                        Err(e) => {
                            self.respond(ResponseLine::err(id, e.code, e.message));
                            return;
                        }
                    }
                }
            }
        }
        let text = persisted_config_text(&config);
        let mode = if config.ready_for_group_switch() {
            "multi_device_armed"
        } else {
            "multi_device_setup"
        };
        self.persist_config_text(id, text, mode, move |runtime| {
            runtime.apply_config(config);
            runtime.lan_config_changed();
        });
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
        let persisted = (|| {
            // Incomplete v1 stays byte-identical until the first explicit save.
            if let Ok(original) = std::fs::read_to_string(&self.config_path) {
                if matches!(
                    ConfigDocument::parse_json(&original),
                    Ok(ConfigDocument::LegacyV1(_))
                ) {
                    let backup = self.config_path.with_file_name(format!(
                        "{}.pre-identity-{}",
                        self.config_path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy(),
                        now_ms()
                    ));
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(backup)?;
                    file.write_all(original.as_bytes())?;
                    file.sync_all()?;
                }
            }
            write_atomically(&self.config_path, text.as_bytes())
        })();
        match persisted {
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

/// Backup before replacing the placeholder, and change memory only after a
/// successful atomic save. Key generation/storage is owned by LAN S4.
fn bootstrap_device_identity(
    path: &Path,
    config: &mut MultiDeviceConfig,
    original: &str,
    stable_id: &str,
    now: u64,
    persist: bool,
) -> std::io::Result<bool> {
    use kvmflow_core::{config::LEGACY_LOCAL_DEVICE_ID, identity::rename_device_id};
    if config.local_device.device_id != LEGACY_LOCAL_DEVICE_ID {
        return Ok(false);
    }
    let mut candidate = config.clone();
    let id = stable_id.to_owned();
    if let Some(group) = &mut candidate.switch_group {
        rename_device_id(group, LEGACY_LOCAL_DEVICE_ID, &id);
    }
    candidate.local_device.device_id = id;
    if !persist {
        *config = candidate;
        return Ok(true);
    }
    let backup = path.with_file_name(format!(
        "{}.pre-identity-{now}",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    // create_new never overwrites an earlier backup, even after a failed save.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(backup)?;
    file.write_all(original.as_bytes())?;
    file.sync_all()?;
    write_atomically(path, persisted_config_text(&candidate).as_bytes())?;
    *config = candidate;
    Ok(true)
}

/// Write to a sibling temp file, flush it to disk, then rename over the
/// target, so a crash or power loss leaves either the old or the new config,
/// never a truncated one. `rename` replaces an existing file on Windows too.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
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

/// Availability is observation, not configuration. T2 refreshes peers on hello;
/// never resurrect a stale lid-open count from a saved file after restart.
pub(crate) fn persisted_config_text(config: &MultiDeviceConfig) -> String {
    let mut value = serde_json::to_value(config).expect("serializable config");
    if let Some(devices) = value
        .pointer_mut("/switch_group/devices")
        .and_then(Value::as_array_mut)
    {
        for device in devices {
            if let Some(device) = device.as_object_mut() {
                device.remove("available_builtin_screens");
            }
        }
    }
    if let Some(cache) = value
        .pointer_mut("/switch_group/sync/profile_cache")
        .and_then(Value::as_object_mut)
    {
        for profile in cache.values_mut() {
            if let Some(profile) = profile.as_object_mut() {
                profile.remove("available_builtin_screens");
            }
        }
    }
    serde_json::to_string_pretty(&value).expect("serializable config")
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn public_identity_is_persistent_concurrent_and_never_replaces_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || load_or_create_device_id(&path).unwrap())
            })
            .collect();
        let ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(ids.iter().all(|id| id == &ids[0]));
        assert_eq!(load_or_create_device_id(&path).unwrap(), ids[0]);
        assert!(!path.exists());
        std::fs::write(dir.path().join("lan/device-id"), "damaged").unwrap();
        assert!(load_or_create_device_id(&path).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("lan/device-id")).unwrap(),
            "damaged"
        );
    }

    #[test]
    fn bootstrap_backs_up_is_idempotent_and_preserves_routes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let LegacyMigration::Runnable(mut c) =
            MultiDeviceConfig::migrate_legacy(kvmflow_core::config::example_config())
        else {
            panic!()
        };
        let before = c
            .switch_group
            .as_ref()
            .unwrap()
            .route_after_departure("legacy-local")
            .unwrap()
            .writes;
        let original = persisted_config_text(&c);
        std::fs::write(&path, &original).unwrap();
        assert!(
            bootstrap_device_identity(&path, &mut c, &original, "dev-test", 123, true).unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config.json.pre-identity-123")).unwrap(),
            original
        );
        assert_eq!(
            c.switch_group
                .as_ref()
                .unwrap()
                .route_after_departure(&c.local_device.device_id)
                .unwrap()
                .writes,
            before
        );
        assert!(c
            .switch_group
            .as_ref()
            .unwrap()
            .devices
            .iter()
            .any(|d| d.device_id == "legacy-remote"));
        let once = std::fs::read(&path).unwrap();
        assert!(
            !bootstrap_device_identity(&path, &mut c, &original, "dev-other", 124, true).unwrap()
        );
        assert_eq!(std::fs::read(&path).unwrap(), once);
        assert!(!dir.path().join("config.json.pre-identity-124").exists());
        assert_eq!(
            serde_json::from_slice::<MultiDeviceConfig>(&once).unwrap(),
            c
        );
    }

    #[test]
    fn backup_failure_keeps_identity_and_original_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut c = MultiDeviceConfig::from_legacy(kvmflow_core::config::example_config());
        let original = persisted_config_text(&c);
        std::fs::write(&path, &original).unwrap();
        std::fs::write(dir.path().join("config.json.pre-identity-1"), "preserved").unwrap();
        let before = c.clone();
        assert!(bootstrap_device_identity(&path, &mut c, &original, "dev-test", 1, true).is_err());
        assert_eq!(c, before);
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }
}
