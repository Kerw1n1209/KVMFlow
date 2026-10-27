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
                    let cfg = self.repair_loaded_placeholder(cfg);
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

    pub(crate) fn bootstrap_loaded_identity(
        &mut self,
        config: MultiDeviceConfig,
        original: &str,
        persist: bool,
    ) -> MultiDeviceConfig {
        if config.local_device.device_id.starts_with("dev-") {
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
                self.lan_issue(
                    if legacy_identity_is_paired(&self.config_path, &config) {
                        "E_LAN_ID_MIGRATION_REQUIRED"
                    } else {
                        "E_LAN_IDENTITY_IO"
                    },
                    &error.to_string(),
                );
                self.emit_event(
                    "lan.identity.bootstrap_failed",
                    Level::Warn,
                    json!({"error":error.to_string()}),
                );
                config
            }
        }
    }

    fn repair_loaded_placeholder(&mut self, config: MultiDeviceConfig) -> MultiDeviceConfig {
        let mut candidate = config.clone();
        let report =
            kvmflow_core::pairing::ProfileReport::from_config(&config, std::env::consts::OS);
        let Some(group) = candidate.switch_group.as_mut() else {
            return config;
        };
        let slots = kvmflow_core::identity::legacy_placeholders(group, &report.device_id);
        if slots.is_empty() {
            return config;
        }
        self.emit_event(
            "lan.legacy.placeholders",
            Level::Info,
            json!({"count":slots.len(), "sync":"withheld_until_owner_profile"}),
        );
        // A beta.3 snapshot may already have displaced this machine. Its
        // local profile is authoritative and can repair that one legacy slot.
        let result = (|| -> std::io::Result<bool> {
            let changed = kvmflow_core::identity::replace_legacy_placeholder(
                candidate.switch_group.as_mut().unwrap(),
                &report.device_id,
                &report,
                now_ms(),
            )
            .map_err(std::io::Error::other)?;
            if !changed {
                return Ok(false);
            }
            candidate.validate().map_err(std::io::Error::other)?;
            let original = std::fs::read(&self.config_path)?;
            let backup = self.config_path.with_file_name(format!(
                "{}.pre-placeholder-{}",
                self.config_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
                now_ms()
            ));
            write_atomically(&backup, &original)?;
            write_atomically(
                &self.config_path,
                persisted_config_text(&candidate).as_bytes(),
            )?;
            Ok(true)
        })();
        match result {
            Ok(true) => candidate,
            Ok(false) => config,
            Err(e) => {
                self.lan_issue("E_LAN_TOPOLOGY_MISSING_SELF", &e.to_string());
                config
            }
        }
    }

    pub(crate) fn apply_config(&mut self, cfg: MultiDeviceConfig) {
        let t = &cfg.trigger;
        let unchanged = self
            .tracker
            .as_ref()
            .is_some_and(|tracker| tracker.watches(&t.anchor, &t.members, &t.debounce));
        if !unchanged {
            let mut tracker =
                GroupTracker::new(t.anchor.clone(), t.members.clone(), t.debounce.clone());
            let keys: Vec<DeviceKey> = self
                .last_snapshot
                .iter()
                .map(|device| DeviceKey {
                    vid_pid: device.vid_pid.clone(),
                    serial: device.serial.clone(),
                })
                .collect();
            // The bus has looked like this since its last change: a trigger
            // adopted while the switch already sits here is stable at once,
            // so pressing the switch right after setup still counts.
            let now = now_ms();
            tracker.update(self.usb_changed_ms.min(now), &keys);
            tracker.update(now, &keys);
            self.tracker = Some(tracker);
        }
        let ready = cfg.ready_for_group_switch();
        if let Some(handle) = self.lan_runtime.handle.as_ref() {
            handle.set_label(&cfg.local_device.host_label);
        }
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
        if !config.local_device.device_id.starts_with("dev-") {
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
        if !self.lan_runtime.device_id.is_empty()
            && config.local_device.device_id != self.lan_runtime.device_id
        {
            self.respond(ResponseLine::err(
                id,
                "E_LAN_IDENTITY",
                "local device ID must match runtime identity",
            ));
            return;
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

/// Trust references (including tombstones) or an unfinished pairing transaction
/// mean another machine may already pin this ID. Manual input presets in
/// `devices` are not LAN members and remain usable after local migration.
fn legacy_identity_is_paired(path: &Path, config: &MultiDeviceConfig) -> bool {
    path.with_file_name("pairing.pending.json").exists()
        || config.switch_group.as_ref().is_some_and(|group| {
            group.sync.get("members").is_some_and(|members| {
                !members.is_null() && members.as_array().is_none_or(|m| !m.is_empty())
            })
        })
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
    use kvmflow_core::identity::rename_device_id;
    if config.local_device.device_id.starts_with("dev-") {
        return Ok(false);
    }
    if legacy_identity_is_paired(path, config) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData,
            "Legacy device identity is already paired. Remove the old pairing on both computers and pair again."));
    }
    let old = config.local_device.device_id.clone();
    let mut candidate = config.clone();
    let id = stable_id.to_owned();
    if let Some(group) = &mut candidate.switch_group {
        if group.devices.iter().any(|device| device.device_id == id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "local identity already belongs to another device",
            ));
        }
        rename_device_id(group, &old, &id);
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

#[cfg(test)]
mod legacy_id_regressions {
    use super::*;
    fn legacy(old: &str) -> MultiDeviceConfig {
        let LegacyMigration::Runnable(mut config) =
            MultiDeviceConfig::migrate_legacy(kvmflow_core::config::example_config())
        else {
            panic!()
        };
        kvmflow_core::identity::rename_device_id(
            config.switch_group.as_mut().unwrap(),
            "legacy-local",
            old,
        );
        config.local_device.device_id = old.into();
        config
    }
    #[test]
    fn platform_ids_migrate_all_schema_references_atomically_and_only_once() {
        for old in ["mac", "win", "windows", "legacy-local"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.json");
            let id = load_or_create_device_id(&path).unwrap();
            let mut config = legacy(old);
            let group = config.switch_group.as_mut().unwrap();
            let fp = group.devices[0].monitors[0].fingerprint.clone();
            group.sync = json!({"topology":{"updated_by":old},"mixed":{"updated_by":old},
                "profile_cache":{old:{"profile_rev":1,"monitors":[]}},
                "archived_arrivals":{old:format!("builtin:{old}")},
                "monitor_aliases":{"local-fp":"group-fp"}, "unknown":"keep"});
            group.mixed = Some(serde_json::from_value(json!({"enabled":false,"revision":1,"assignment":[{"fingerprint":fp,"device_id":old}]})).unwrap());
            group.devices[0].arrival_layout_id = Some(format!("builtin:{old}"));
            group.layouts = serde_json::from_value(json!([{"id":format!("migrated:{old}:1"),"name":"saved","source":{"kind":"migrated_follow","device_id":old},"monitors":[{"fingerprint":fp,"target":{"device_id":old}}]}])).unwrap();
            let original = persisted_config_text(&config);
            std::fs::write(&path, &original).unwrap();
            assert!(
                bootstrap_device_identity(&path, &mut config, &original, &id, 1, true).unwrap()
            );
            let value = serde_json::to_value(&config).unwrap();
            for pointer in [
                "/local_device/device_id",
                "/switch_group/devices/0/device_id",
                "/switch_group/mixed/assignment/0/device_id",
                "/switch_group/layouts/0/source/device_id",
                "/switch_group/layouts/0/monitors/0/target/device_id",
                "/switch_group/sync/topology/updated_by",
                "/switch_group/sync/mixed/updated_by",
            ] {
                assert_eq!(value.pointer(pointer).unwrap(), &id, "{pointer}");
            }
            assert_eq!(
                value["switch_group"]["sync"]["profile_cache"][&id]["profile_rev"],
                1
            );
            assert_eq!(
                value["switch_group"]["sync"]["archived_arrivals"][&id],
                format!("builtin:{id}")
            );
            assert_eq!(
                value["switch_group"]["sync"]["monitor_aliases"]["local-fp"],
                "group-fp"
            );
            assert_eq!(value["switch_group"]["sync"]["unknown"], "keep");
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(
                serde_json::from_slice::<MultiDeviceConfig>(&bytes).unwrap(),
                config
            );
            assert!(!bootstrap_device_identity(
                &path,
                &mut config,
                &original,
                "dev-different",
                2,
                true
            )
            .unwrap());
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
    }
    #[test]
    fn paired_or_pending_legacy_identity_is_preserved_including_tombstones() {
        for kind in ["member", "removed", "pending", "malformed"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.json");
            let mut config = legacy("mac");
            if kind == "pending" {
                std::fs::write(dir.path().join("pairing.pending.json"), "{}").unwrap();
            } else {
                config.switch_group.as_mut().unwrap().sync = json!({"members":if kind=="malformed" {json!({})} else {json!([{"device_id":"peer","removed":kind=="removed"}])}});
            }
            let before = config.clone();
            let original = persisted_config_text(&config);
            std::fs::write(&path, &original).unwrap();
            assert!(
                bootstrap_device_identity(&path, &mut config, &original, "dev-new", 1, true)
                    .is_err()
            );
            assert_eq!(config, before);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
            assert!(!dir.path().join("config.json.pre-identity-1").exists());
        }
    }
}

#[cfg(test)]
mod identity_atomic_failure_tests {
    use super::*;
    #[test]
    fn failed_atomic_replace_does_not_publish_a_partial_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("preserved"), "original").unwrap();
        let mut config = MultiDeviceConfig::from_legacy(kvmflow_core::config::example_config());
        let before = config.clone();
        let original = persisted_config_text(&config);
        assert!(
            bootstrap_device_identity(&path, &mut config, &original, "dev-new", 1, true).is_err()
        );
        assert_eq!(config, before);
        assert_eq!(
            std::fs::read_to_string(path.join("preserved")).unwrap(),
            "original"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config.json.pre-identity-1")).unwrap(),
            original
        );
        assert!(!dir.path().join("config.json.tmp").exists());
    }
}
