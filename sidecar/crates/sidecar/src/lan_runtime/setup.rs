//! Resume the SAS-confirmed setup when pending owner inputs become available.
use super::*;
use kvmflow_core::pairing::{plan_pair, PairOptions, PairSide, ProfileInput};

impl Runtime {
    pub(super) fn resume_pair_setup(&mut self, source: &Source, report: &ProfileReport) -> bool {
        let Some(mut cfg) = self.config.clone() else {
            return false;
        };
        let Some(group) = cfg.switch_group.as_ref() else {
            return false;
        };
        let Some(pending) = group.sync.get("setup_pending") else {
            return false;
        };
        let result = (|| -> Result<Option<SwitchGroupSnapshot>, CoreError> {
            let decode = |key: &str| pending.get(key).cloned().unwrap_or(Value::Null);
            let mut local: PairSide = serde_json::from_value(decode("side"))
                .map_err(|e| CoreError::new(E_CONFIG_INVALID, e.to_string()))?;
            let mut peer: PairSide = serde_json::from_value(decode("peer_side"))
                .map_err(|e| CoreError::new(E_CONFIG_INVALID, e.to_string()))?;
            let options: PairOptions = serde_json::from_value(decode("options"))
                .map_err(|e| CoreError::new(E_CONFIG_INVALID, e.to_string()))?;
            if local.device_id != cfg.local_device.device_id
                || peer.device_id != source.device_id
                || peer.public_key != public_key_string(&source.key)
                || !self.current_source(source)
            {
                return Err(CoreError::new(
                    "E_LAN_IDENTITY",
                    "pending setup does not match the authenticated owners",
                ));
            }
            local.profile = ProfileReport::from_config(&cfg, std::env::consts::OS);
            local.local_device = cfg.local_device.clone();
            peer.profile = report.clone();
            peer.host_label = report.name.clone();
            // The owner report is authoritative; pending values remain unknown.
            peer.local_device.monitors = report
                .monitors
                .iter()
                .filter(|m| !m.builtin)
                .map(|m| {
                    let (value, source) = match m.input {
                        ProfileInput::Value { value, source } => (value, source),
                        ProfileInput::Pending => (0, kvmflow_core::config::InputSource::Unknown),
                    };
                    kvmflow_core::config::LocalMonitorConfig {
                        fingerprint: m.fingerprint.clone(),
                        label: m.name.clone(),
                        local_input: value,
                        source,
                        legacy_remote_preset: None,
                    }
                })
                .collect();
            let plan = plan_pair(&local, &peer, &options)?;
            if plan.needs_choice() || plan.snapshot.is_none() {
                self.notify(
                    "lan.setup",
                    json!({"phase":"awaiting_profiles","issues":plan.issues}),
                );
                return Ok(None);
            }
            let mut ready = plan.snapshot.unwrap();
            let mut sync = ready.group_sync();
            sync.members = group.group_sync().members;
            sync.extra.remove("setup_pending");
            ready.set_group_sync(sync);
            Ok(Some(ready))
        })();
        match result {
            Ok(Some(ready)) => {
                cfg.switch_group = Some(ready);
                if let Err(error) = self.lan_save(cfg.clone()) {
                    self.lan_issue("E_LAN_SETUP_PENDING", &error.to_string());
                    return true;
                }
                self.apply_config(cfg);
                self.lan_config_changed();
                self.lan_runtime
                    .issues
                    .retain(|issue| issue["code"] != "E_LAN_SETUP_PENDING");
                self.notify(
                    "lan.setup",
                    json!({"phase":"topology_ready","device_id":source.device_id}),
                );
                self.pair_emit(
                    json!({"phase":"committed","device_id":source.device_id,"config_merged":true}),
                );
            }
            Ok(None) => {}
            Err(error) => self.lan_issue("E_LAN_SETUP_PENDING", &error.to_string()),
        }
        true
    }
}
