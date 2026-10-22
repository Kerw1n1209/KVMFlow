use kvmflow_core::{
    config::{example_config, LegacyMigration, MultiDeviceConfig},
    identity::*,
    layouts::*,
    mixed::*,
    sync::*,
};
use serde_json::json;

fn migrated() -> MultiDeviceConfig {
    let LegacyMigration::Runnable(c) = MultiDeviceConfig::migrate_legacy(example_config()) else {
        panic!()
    };
    c
}

#[test]
fn ids_are_fixed_width_crockford_and_keep_all_entropy() {
    assert_eq!(new_device_id([0; 16]), "dev-00000000000000000000000000");
    assert_eq!(new_group_id([255; 16]), "grp-7ZZZZZZZZZZZZZZZZZZZZZZZZZ");
    let mut bytes = [0; 16];
    bytes[15] = 1;
    assert_ne!(new_device_id(bytes), new_device_id([0; 16]));
}

#[test]
fn rename_rewrites_all_layout_mixed_trust_and_pending_references() {
    let mut c = migrated();
    let g = c.switch_group.as_mut().unwrap();
    g.devices[0].arrival_layout_id = Some("migrated:legacy-local:1".into());
    g.layouts.push(Layout {
        id: "migrated:legacy-local:1".into(),
        name: "keep".into(),
        source: LayoutSource::MigratedFollow {
            device_id: "legacy-local".into(),
        },
        monitors: vec![LayoutMonitor {
            fingerprint: g.devices[0].monitors[0].fingerprint.clone(),
            target: LayoutTarget::device("legacy-local"),
        }],
    });
    g.sync = json!({"mixed":{"updated_at_ms":7,"updated_by":"legacy-local"},
        "members":[{"device_id":"legacy-local","introduced_by":"legacy-local","meta":{"updated_by":"legacy-local"}}],"future":{"keep":true}});
    let mut pending = vec![PendingRemote {
        id: String::new(),
        device_id: Some("legacy-local".into()),
        fingerprint: "screen".into(),
        target_device_id: "legacy-local".into(),
    }];
    let before = g.route_after_departure("legacy-local").unwrap().writes;
    rename_device_id(g, "legacy-local", "dev-A");
    rename_pending_remote(&mut pending, g, "legacy-local", "dev-A");
    assert_eq!(g.devices[0].device_id, "dev-A");
    assert_eq!(
        g.devices[0].arrival_layout_id.as_deref(),
        Some("migrated:dev-A:1")
    );
    assert_eq!(g.layouts[0].id, "migrated:dev-A:1");
    assert_eq!(
        g.layouts[0].source,
        LayoutSource::MigratedFollow {
            device_id: "dev-A".into()
        }
    );
    assert_eq!(
        g.layouts[0].monitors[0].target,
        LayoutTarget::device("dev-A")
    );
    assert!(g
        .mixed
        .as_ref()
        .unwrap()
        .assignment
        .iter()
        .any(|a| a.device_id == "dev-A"));
    assert_eq!(g.sync["members"][0]["device_id"], "dev-A");
    assert_eq!(g.sync["members"][0]["introduced_by"], "dev-A");
    assert_eq!(g.sync["future"], json!({"keep":true}));
    assert_eq!(pending[0].device_id.as_deref(), Some("dev-A"));
    assert_eq!(pending[0].target_device_id, "dev-A");
    assert!(pending[0].id.contains("dev-A"));
    assert_eq!(before, g.route_after_departure("dev-A").unwrap().writes);
}

#[test]
fn local_alias_validates_and_resolves_to_backend_only_on_local_owner() {
    let mut c = migrated();
    let fp = c.local_device.monitors[0].fingerprint.clone();
    c.local_device.monitors[0].fingerprint = "platform-local".into();
    c.switch_group.as_mut().unwrap().devices[0].monitors[0].local_fingerprint =
        Some("platform-local".into());
    assert!(c.validate().is_ok(), "{:?}", c.validate());
    assert_eq!(c.backend_fingerprint(&fp), "platform-local");
    c.switch_group.as_mut().unwrap().devices[0].monitors[0].local_fingerprint = None;
    assert!(c.validate().is_err());
}

#[test]
fn old_v2_with_follow_and_layouts_roundtrips_without_new_fields() {
    let mut c = migrated();
    let g = c.switch_group.as_mut().unwrap();
    g.devices[1].monitors[0].follow = false;
    g.migrate_follow_layouts();
    let old = serde_json::to_value(c).unwrap();
    assert!(old.get("lan").is_none());
    assert!(old["switch_group"]["devices"][0]
        .get("profile_rev")
        .is_none());
    let parsed: MultiDeviceConfig = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), old);
    let lan: kvmflow_core::config::LanConfig = serde_json::from_value(json!({})).unwrap();
    assert!(lan.enabled);
    assert_eq!(lan.listen_port, 47820);
    let meta: SectionMeta =
        serde_json::from_value(json!({"updated_at_ms":7,"updated_by":"A"})).unwrap();
    assert_eq!(meta.updated_by, "A");
}

#[test]
fn rename_rewrites_internal_profile_keys_and_archived_arrival_ids() {
    let mut c = migrated();
    let g = c.switch_group.as_mut().unwrap();
    let p = CachedProfile::from(&g.devices[0]);
    let mut sync = g.group_sync();
    sync.members.push(Member {
        device_id: "retired".into(),
        public_key: "key-retired".into(),
        ..Default::default()
    });
    sync.profile_cache.insert("retired".into(), p);
    sync.archived_arrivals
        .insert("retired".into(), "migrated:retired:1".into());
    g.set_group_sync(sync);
    rename_device_id(g, "retired", "dev-new");
    assert!(g.group_sync().profile_cache.contains_key("dev-new"));
    assert!(!g.group_sync().profile_cache.contains_key("retired"));
    assert_eq!(
        g.group_sync().archived_arrivals["dev-new"],
        "migrated:dev-new:1"
    );
}
