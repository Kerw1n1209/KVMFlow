//! Tray presentation rules. Runtime observations, assignments and optional LAN
//! evidence stay distinct. This module performs no I/O or native menu calls.
use serde_json::Value;

pub type Tr<'a> = &'a dyn Fn(&str) -> String;

fn fill(text: String, values: &[(&str, String)]) -> String {
    values
        .iter()
        .fold(text, |text, (key, value)| text.replace(key, value))
}

fn named(value: &Value) -> Option<&str> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub fn computer_name(config: &Value, id: &str, tr: Tr) -> String {
    config["switch_group"]["devices"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|device| device["device_id"] == id)
        .and_then(|device| named(&device["name"]))
        .map(str::to_owned)
        .unwrap_or_else(|| tr("computer"))
}

pub fn busy(mixed: &Value) -> bool {
    matches!(
        mixed["transition"]["phase"].as_str(),
        Some("enabling" | "disabling" | "restoring")
    )
}

pub fn can_toggle(mixed: &Value) -> bool {
    mixed.is_object()
        && !busy(mixed)
        && (mixed["enabled"] == true || mixed["validation"]["ok"] == true)
}

pub fn interaction_allowed(mixed: &Value, installing: bool, busy: bool) -> bool {
    can_toggle(mixed) && !installing && !busy
}

pub fn perform_toggle<E>(
    installing: &std::sync::atomic::AtomicBool,
    mut request: impl FnMut(&str, Value) -> Result<Value, E>,
) -> Result<Option<Value>, E> {
    use std::sync::atomic::Ordering;
    if installing.load(Ordering::Acquire) {
        return Ok(None);
    }
    let state = request("state.get", serde_json::json!({}))?;
    if can_toggle(&state["mixed"]) && !installing.load(Ordering::Acquire) {
        request(
            "mixed.set",
            serde_json::json!({"enabled":state["mixed"]["enabled"] != true}),
        )?;
    }
    request("state.get", serde_json::json!({})).map(Some)
}

pub fn current(config: &Value, mixed: &Value, tr: Tr) -> Option<String> {
    let id = mixed["keyboard_holder_device_id"].as_str()?;
    let device = config["switch_group"]["devices"]
        .as_array()?
        .iter()
        .find(|device| device["device_id"] == id)?;
    let mut name = computer_name(config, id, tr);
    if mixed["holder_confidence"] == "inferred" {
        name = fill(tr("native.mixed.inferred"), &[("{p0}", name)]);
    }
    Some(fill(
        tr("native.mixed.current"),
        &[("{p0}", name), ("{p1}", device["port_index"].to_string())],
    ))
}

pub fn assignment(config: &Value, mixed: &Value, tr: Tr) -> Option<String> {
    if mixed["enabled"] != true {
        return None;
    }
    let mut fingerprints = Vec::<String>::new();
    for monitor in config["local_device"]["monitors"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if let Some(fp) = monitor["fingerprint"].as_str() {
            fingerprints.push(fp.to_string());
        }
    }
    // Match settings/runtime local order, then append retained and new screens.
    for device in config["switch_group"]["devices"]
        .as_array()
        .into_iter()
        .flatten()
    {
        for monitor in device["monitors"].as_array().into_iter().flatten() {
            if let Some(fp) = monitor["fingerprint"].as_str() {
                if !fingerprints.iter().any(|known| known == fp) {
                    fingerprints.push(fp.to_string());
                }
            }
        }
    }
    for destination in mixed["destinations"].as_array().into_iter().flatten() {
        if destination["builtin"] != true {
            if let Some(fp) = destination["fingerprint"].as_str() {
                if !fingerprints.iter().any(|known| known == fp) {
                    fingerprints.push(fp.to_string());
                }
            }
        }
    }
    let items: Vec<_> = fingerprints
        .iter()
        .enumerate()
        .filter_map(|(index, fp)| {
            let assigned = mixed["assignment"]
                .as_array()?
                .iter()
                .find(|item| item["fingerprint"] == *fp)?;
            let id = assigned["device_id"].as_str()?;
            let destination = mixed["destinations"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|item| item["fingerprint"] == *fp);
            let label = config["switch_group"]["devices"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|device| device["monitors"].as_array().into_iter().flatten())
                .find(|monitor| monitor["fingerprint"] == *fp && named(&monitor["label"]).is_some())
                .and_then(|monitor| named(&monitor["label"]))
                .or_else(|| destination.and_then(|item| named(&item["label"])))
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    fill(
                        tr("monitor.value"),
                        &[(
                            "{p0}",
                            destination
                                .and_then(|item| item["default_display_number"].as_u64())
                                .unwrap_or(index as u64 + 1)
                                .to_string(),
                        )],
                    )
                });
            Some(format!("{label} → {}", computer_name(config, id, tr)))
        })
        .collect();
    if items.is_empty() {
        return Some(fill(
            tr("native.mixed.assignment"),
            &[("{p0}", tr("mixed.unassigned"))],
        ));
    }
    Some(fill(
        tr("native.mixed.assignment"),
        &[("{p0}", items.join(&tr("mixed.list.separator")))],
    ))
}

pub fn should_notify(previous: &mut Option<bool>, mixed: &Value) -> bool {
    if busy(mixed) {
        return false;
    }
    let mode = mixed["enabled"].as_bool();
    if mode.is_none() {
        return false;
    }
    let changed = previous.is_some() && mode.is_some() && *previous != mode;
    *previous = mode;
    changed
}

pub fn summary(config: &Value, state: &Value, tr: Tr) -> String {
    let mixed = &state["mixed"];
    let enabled = mixed["enabled"] == true;
    let pending = mixed["pending_remote"]
        .as_array()
        .is_some_and(|items| !items.is_empty());
    let failed = mixed["execution"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|item| item["ok"] == false)
        || matches!(
            mixed["transition"]["outcome"].as_str(),
            Some("partial" | "failed")
        );
    let mut parts = vec![tr(if enabled {
        "native.mixed.enabled"
    } else {
        "native.mixed.disabled"
    })];
    if !enabled && (pending || failed) {
        parts.push(tr("native.mixed.restore.pending"));
    } else if failed {
        parts.push(tr("native.mixed.partial"));
    } else {
        parts.push(tr("native.mixed.local"));
    }
    if let Some(peers) = state["lan"]["peers"]
        .as_array()
        .filter(|peers| !peers.is_empty())
    {
        let names = |peers: Vec<&Value>| {
            peers
                .iter()
                .map(|peer| {
                    named(&peer["name"]).map(str::to_owned).unwrap_or_else(|| {
                        computer_name(config, peer["device_id"].as_str().unwrap_or(""), tr)
                    })
                })
                .collect::<Vec<_>>()
                .join(&tr("mixed.list.separator"))
        };
        let offline: Vec<_> = peers
            .iter()
            .filter(|peer| peer["online"] == false)
            .collect();
        if !offline.is_empty() {
            parts.push(fill(
                tr("native.mixed.offline"),
                &[("{p0}", names(offline))],
            ));
        } else if peers
            .iter()
            .all(|peer| peer["online"] == true && peer["synced"] == true)
        {
            parts.push(fill(
                tr("native.mixed.synced"),
                &[("{p0}", names(peers.iter().collect()))],
            ));
        }
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locale::translate;
    use serde_json::json;
    fn zh(key: &str) -> String {
        translate("zh-CN", key)
    }
    fn config() -> Value {
        json!({"switch_group":{"devices":[
            {"device_id":"mac","name":"Mac","port_index":1,"monitors":[{"fingerprint":"a"},{"fingerprint":"b","label":"竖屏"}]},
            {"device_id":"pc","name":"PC","port_index":2,"monitors":[]}
        ]}})
    }
    #[test]
    fn retries_and_assignment_changes_do_not_repeat_mode_notices() {
        let mut previous = None;
        assert!(!should_notify(&mut previous, &json!({"enabled":false})));
        assert!(!should_notify(
            &mut previous,
            &json!({"enabled":true,"transition":{"phase":"enabling"}})
        ));
        assert!(should_notify(
            &mut previous,
            &json!({"enabled":true,"transition":{"phase":"idle"}})
        ));
        assert!(!should_notify(
            &mut previous,
            &json!({"enabled":true,"revision":9})
        ));
        assert!(should_notify(
            &mut previous,
            &json!({"enabled":false,"transition":{"phase":"idle","outcome":"partial"}})
        ));
    }
    #[test]
    fn validation_never_blocks_disabling_but_busy_does() {
        assert!(!can_toggle(&Value::Null));
        assert!(!can_toggle(
            &json!({"enabled":false,"validation":{"ok":false}})
        ));
        assert!(can_toggle(
            &json!({"enabled":true,"validation":{"ok":false}})
        ));
        for phase in ["enabling", "disabling", "restoring"] {
            assert!(!can_toggle(
                &json!({"enabled":true,"transition":{"phase":phase}})
            ));
        }
    }
    #[test]
    fn assignments_are_named_and_only_shown_in_mixed_mode() {
        let mixed = json!({"enabled":true,"assignment":[{"fingerprint":"b","device_id":"mac"},{"fingerprint":"a","device_id":"pc"}]});
        assert_eq!(
            assignment(&config(), &mixed, &zh).unwrap(),
            "混屏中：显示器 1 → PC、竖屏 → Mac"
        );
        assert!(assignment(&config(), &json!({"enabled":false}), &zh).is_none());
        assert!(current(&config(), &json!({"keyboard_holder_device_id":null}), &zh).is_none());
    }
    #[test]
    fn saved_is_not_synced_or_visible_picture_proof() {
        let state = json!({"mixed":{"enabled":false,"pending_remote":[{}],"execution":[]}});
        let body = summary(&config(), &state, &zh);
        assert!(body.contains("下次按切换器时恢复"));
        assert!(!body.contains("已同步"));
        let state =
            json!({"mixed":{"enabled":true},"lan":{"peers":[{"device_id":"pc","online":true}]}});
        assert!(!summary(&config(), &state, &zh).contains("已与"));
        let state =
            json!({"mixed":{"enabled":true},"lan":{"peers":[{"device_id":"pc","online":false}]}});
        assert!(summary(&config(), &state, &zh).contains("PC 不在线"));
    }
    #[test]
    fn installation_disables_menu_and_blocks_already_queued_toggles() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let mixed = serde_json::json!({"enabled":true,"validation":{"ok":true}});
        assert!(interaction_allowed(&mixed, false, false));
        assert!(!interaction_allowed(&mixed, true, false));
        let installing = AtomicBool::new(true);
        let mut calls = Vec::new();
        let result: Result<_, ()> = perform_toggle(&installing, |method, _| {
            calls.push(method.to_string());
            Ok(serde_json::json!({"mixed":mixed}))
        });
        assert!(result.unwrap().is_none());
        assert!(calls.is_empty());
        installing.store(false, Ordering::Release);
        let result: Result<_, ()> = perform_toggle(&installing, |method, _| {
            calls.push(method.to_string());
            if method == "state.get" {
                installing.store(true, Ordering::Release);
            }
            Ok(serde_json::json!({"mixed":mixed}))
        });
        assert!(result.unwrap().is_some());
        assert!(!calls.iter().any(|method| method == "mixed.set"));
    }
}

#[cfg(test)]
mod install_race_tests {
    use super::*;
    use kvmflow_sidecar::{topology::FakeDisplayTopology, RuntimeHandle, RuntimeOptions};
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn worker_freezes_toggle_queued_after_install_check() {
        let dir = tempfile::tempdir().unwrap();
        let config = include_str!("../../tests/fixtures/runtime-mixed.json");
        std::fs::write(dir.path().join("config.json"), config).unwrap();
        let topology = FakeDisplayTopology::new(
            ["SAC-2763-S:0000000000001", "SAC-2466-S:0000000000000"]
                .into_iter()
                .map(|fp| kvmflow_core::mixed::TopologyDisplay {
                    fingerprint: fp.into(),
                    builtin: false,
                    connected: true,
                })
                .collect(),
        );
        let operations = topology.operations.clone();
        let mut options =
            RuntimeOptions::new(dir.path().join("config.json"), dir.path().join("logs"));
        options.backend = "simulated".into();
        options.topology = Some(Box::new(topology));
        let runtime = RuntimeHandle::start(options, |_, _| {}).unwrap();
        let before = std::fs::read(dir.path().join("config.json")).unwrap();
        assert_eq!(
            runtime.request("state.get", serde_json::json!({})).unwrap()["mixed"]["validation"]
                ["ok"],
            true
        );
        let installing = AtomicBool::new(false);
        let mut reached_enqueue = false;
        let result = perform_toggle(&installing, |method, params| {
            if method == "state.get" {
                return runtime.request(method, params);
            }
            // The tray passed BOTH atomic checks, then the installer acquired
            // its lock and completed ReleaseDisplays before mixed.set arrived.
            reached_enqueue = true;
            installing.store(true, Ordering::Release);
            runtime.release_displays().unwrap();
            runtime.request(method, params)
        });
        assert!(reached_enqueue);
        assert_eq!(result.unwrap_err().code, "E_BACKEND");
        assert_eq!(
            std::fs::read(dir.path().join("config.json")).unwrap(),
            before
        );
        assert_eq!(*operations.lock().unwrap(), ["restore_all"]);
        runtime
            .resume_after_cancel(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(
            runtime.request("state.get", serde_json::json!({})).unwrap()["enabled"],
            true
        );
        assert_eq!(
            runtime
                .request("mixed.set", serde_json::json!({"enabled":true}))
                .unwrap()["mixed"]["enabled"],
            true
        );
        runtime.shutdown_and_join().unwrap();
    }
}
