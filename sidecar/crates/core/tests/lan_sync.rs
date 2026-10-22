use kvmflow_core::{
    config::{example_config, LegacyMigration, MultiDeviceConfig},
    identity::rename_device_id,
    multidevice::SwitchGroupSnapshot,
    sync::*,
};
use serde_json::json;

fn group(n: usize) -> SwitchGroupSnapshot {
    let LegacyMigration::Runnable(c) = MultiDeviceConfig::migrate_legacy(example_config()) else {
        panic!()
    };
    let mut g = c.switch_group.unwrap();
    rename_device_id(&mut g, "legacy-local", "A");
    rename_device_id(&mut g, "legacy-remote", "B");
    if n == 3 {
        let mut c = g.devices[1].clone();
        c.device_id = "C".into();
        c.port_index = 3;
        g.devices.push(c);
    }
    for (i, d) in g.devices.iter_mut().enumerate() {
        d.monitors[0].local_input = 10 + i as u16 * 20;
        d.profile_rev = 1;
    }
    g.set_group_sync(GroupSync {
        ports_confirmed: true,
        members: g
            .devices
            .iter()
            .map(|d| Member {
                device_id: d.device_id.clone(),
                public_key: format!("key-{}", d.device_id),
                revision: 1,
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    });
    normalize_snapshot(&mut g);
    g
}
fn join(a: &SwitchGroupSnapshot, b: &SwitchGroupSnapshot) -> SwitchGroupSnapshot {
    let (g, c) = merge_sections(a, b).unwrap();
    assert!(c.validation_error.is_none(), "{:?}", c);
    g
}
fn random(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

/// Replay random local edit sequences and out-of-order exchanges with 2/3
/// independent owners. Seed is stable and printed for reproducible failures.
#[test]
fn randomized_two_and_three_owner_edits_obey_join_laws_and_version_monotonicity() {
    for n in [2, 3] {
        for initial in 1..=32 {
            let mut seed = initial;
            let mut replicas = vec![group(n); n];
            for step in 0..100 {
                let owner = (random(&mut seed) as usize) % n;
                let self_id = replicas[owner].devices[owner].device_id.clone();
                let old = replicas[owner].clone();
                let mut edit = old.clone();
                match random(&mut seed) % 5 {
                    0 => edit.devices[owner].name = format!("label-{step}"),
                    1 => {
                        edit.devices[owner].monitors[0].local_input =
                            10 + owner as u16 * 20 + (random(&mut seed) % 10) as u16
                    }
                    2 => {
                        edit.mixed.as_mut().unwrap().enabled =
                            !edit.mixed.as_ref().unwrap().enabled;
                    }
                    3 => {
                        for d in &mut edit.devices {
                            d.port_index = d.port_index % n as u16 + 1;
                        }
                    }
                    _ => {
                        let mut s = edit.group_sync();
                        s.members[owner].removed = !s.members[owner].removed;
                        edit.set_group_sync(s);
                    }
                }
                replicas[owner] = stamp_local_edit(&old, &edit, &self_id, random(&mut seed) % 1000)
                    .unwrap()
                    .0;
                let a = &replicas[0];
                let b = &replicas[1];
                let c = &replicas[n - 1];
                let ab = join(a, b);
                assert_eq!(ab, join(b, a), "seed={initial} step={step}");
                assert_eq!(join(a, a), *a);
                assert_eq!(
                    join(&ab, c),
                    join(a, &join(b, c)),
                    "seed={initial} step={step}"
                );
                let summary = SyncSummary::from_snapshot(&ab);
                for input in [a, b] {
                    let s = SyncSummary::from_snapshot(input);
                    assert!(summary.topology >= s.topology);
                    assert!(summary.mixed >= s.mixed);
                    for (id, rev) in s.profiles {
                        assert!(summary.profiles[&id] >= rev);
                    }
                }
                let dest = random(&mut seed) as usize % n;
                let src = random(&mut seed) as usize % n;
                replicas[dest] = join(&replicas[dest], &replicas[src]);
            }
        }
    }
}

#[test]
fn receiver_accepts_only_owners_profile_and_pushes_back_its_own() {
    let a = group(3);
    let mut b = a.clone();
    for d in &mut b.devices {
        d.profile_rev += 100;
        d.monitors[0].label = "forged".into();
    }
    let (merged, c) = merge_group(&a, &b, "A", "B").unwrap();
    assert_eq!(merged.devices[0].monitors, a.devices[0].monitors);
    assert_eq!(merged.devices[1].monitors, b.devices[1].monitors);
    assert_eq!(merged.devices[2].monitors, a.devices[2].monitors);
    assert_eq!(c.rejected_profiles, vec!["A", "C"]);
}

#[test]
fn offline_edits_use_timestamp_then_author_and_keep_sections_independent() {
    let base = group(2);
    let mut edit = base.clone();
    edit.devices[0].name = "first".into();
    let a = stamp_local_edit(&base, &edit, "A", 100).unwrap().0;
    edit.devices[0].name = "later".into();
    let b = stamp_local_edit(&base, &edit, "B", 200).unwrap().0;
    assert_eq!(join(&a, &b).devices[0].name, "later");
    let b = stamp_local_edit(&base, &edit, "B", 100).unwrap().0;
    assert_eq!(join(&a, &b).devices[0].name, "later");
    let mut edit = base.clone();
    edit.mixed.as_mut().unwrap().enabled = true;
    let m = stamp_local_edit(&base, &edit, "A", 50).unwrap().0;
    let merged = join(&m, &b);
    assert!(merged.mixed.as_ref().unwrap().enabled);
    assert_eq!(merged.devices[0].name, "later");
}

#[test]
fn divergent_equal_keys_use_blake2s_including_member_tombstones() {
    let a = group(2);
    let mut b = a.clone();
    b.devices[0].name = "divergent".into();
    b.sync["members"][0]["removed"] = json!(true);
    let (ab, c) = merge_sections(&a, &b).unwrap();
    let (ba, _) = merge_sections(&b, &a).unwrap();
    assert_eq!(ab, ba);
    assert!(c.divergent.contains(&"topology".into()));
    assert!(c.divergent.contains(&"member:A".into()));
    assert_eq!(digest(&json!({"b":2,"a":1})), digest(&json!({"a":1,"b":2})));
    assert_eq!(digest(&json!(null)).len(), 64);
}

#[test]
fn old_snapshots_cannot_resurrect_a_removed_member() {
    let a = group(2);
    let mut b = a.clone();
    b.sync["members"][1]["removed"] = json!(true);
    let b = stamp_local_edit(&a, &b, "A", 100).unwrap().0;
    assert!(join(&a, &b).group_sync().members[1].removed);
    assert!(b.revision > a.revision);
    assert_eq!(join(&a, &b), join(&b, &a));
}

#[test]
fn invalid_merge_keeps_original_and_non_owner_local_edit_is_rejected() {
    let a = group(2);
    let mut invalid = a.clone();
    invalid.devices[1].monitors[0].local_input = invalid.devices[0].monitors[0].local_input;
    invalid.devices[1].profile_rev += 1;
    let (g, c) = merge_group(&a, &invalid, "A", "B").unwrap();
    assert_eq!(g, a);
    assert!(c.validation_error.is_some());
    assert_eq!(
        stamp_local_edit(&a, &invalid, "A", 1).unwrap_err().code,
        "E_NOT_OWNER"
    );
    invalid = a.clone();
    invalid.devices[1].port_index = 1;
    invalid.revision += 1;
    let (g, c) = merge_sections(&a, &invalid).unwrap();
    assert_eq!(g, a);
    assert!(c.validation_error.is_some());
    invalid.group_id = "other".into();
    assert_eq!(
        merge_sections(&a, &invalid).unwrap_err().code,
        "E_LAN_GROUP_CONFLICT"
    );
}

#[test]
fn no_op_stamping_ignores_injected_revisions_and_checks_overflow() {
    let a = group(2);
    let mut b = a.clone();
    b.revision = 999;
    b.devices[0].profile_rev = 999;
    b.mixed.as_mut().unwrap().revision = 999;
    let (b, c) = stamp_local_edit(&a, &b, "A", 1).unwrap();
    assert_eq!(b, a);
    assert!(!c.topology && !c.mixed && c.profiles.is_empty());
    let mut a = a;
    a.revision = u64::MAX;
    let mut b = a.clone();
    b.devices[0].name = "changed".into();
    assert_eq!(
        stamp_local_edit(&a, &b, "A", 1).unwrap_err().code,
        "E_CONFIG_INVALID"
    );
}

#[test]
fn trust_forwarding_requires_pinned_active_source_and_cannot_silently_rotate_keys() {
    let g = group(2);
    let local = g.group_sync().members;
    let source = AuthenticatedMember {
        device_id: "A".into(),
        public_key: "key-A".into(),
    };
    let mut remote = local.clone();
    remote.push(Member {
        device_id: "C".into(),
        public_key: "key-C".into(),
        introduced_by: "A".into(),
        revision: 1,
        ..Default::default()
    });
    let (members, _) = merge_members(&local, &remote, &source).unwrap();
    assert_eq!(members.len(), 3);
    let unknown = AuthenticatedMember {
        device_id: "C".into(),
        public_key: "key-C".into(),
    };
    assert_eq!(
        merge_members(&local, &remote, &unknown).unwrap_err().code,
        "E_LAN_NOT_MEMBER"
    );
    let wrong = AuthenticatedMember {
        device_id: "A".into(),
        public_key: "attacker".into(),
    };
    assert_eq!(
        merge_members(&local, &remote, &wrong).unwrap_err().code,
        "E_LAN_KEY_CHANGED"
    );
    remote[1].public_key = "rotated-B".into();
    remote[1].revision += 1;
    assert_eq!(
        merge_members(&local, &remote, &source).unwrap_err().code,
        "E_LAN_KEY_CHANGED"
    );
    let mut removed = local.clone();
    removed[1].removed = true;
    let (ab, _) = merge_members(&local, &removed, &source).unwrap();
    let (ba, _) = merge_members(&removed, &local, &source).unwrap();
    assert_eq!(ab, ba);
    assert!(ab[1].removed);
    let mut newer = ab.clone();
    newer[1].revision += 1;
    let (merged, _) = merge_members(&newer, &local, &source).unwrap();
    assert!(merged[1].removed);
    let revoked_source = AuthenticatedMember {
        device_id: "B".into(),
        public_key: "key-B".into(),
    };
    assert_eq!(
        merge_members(&newer, &local, &revoked_source)
            .unwrap_err()
            .code,
        "E_LAN_NOT_MEMBER"
    );
    // Old snapshots containing a prior key cannot undo an explicit local repair.
    let mut repaired = local.clone();
    repaired[1].public_key = "repaired".into();
    repaired[1].revision += 1;
    assert_eq!(
        merge_members(&repaired, &local, &source).unwrap().0[1].public_key,
        "repaired"
    );
    let mut edited = g.clone();
    edited.sync["members"][1]["public_key"] = json!("changed");
    assert_eq!(
        stamp_local_edit(&g, &edited, "A", 2).unwrap_err().code,
        "E_LAN_KEY_CHANGED"
    );
}

#[test]
fn new_relayed_profiles_wait_for_owner_but_trust_can_be_joined_first() {
    let local = group(2);
    let remote = group(3);
    let (g, c) = merge_group(&local, &remote, "B", "A").unwrap();
    assert_eq!(g, local);
    assert!(c.rejected_profiles.contains(&"C".into()));
    let source = AuthenticatedMember {
        device_id: "A".into(),
        public_key: "key-A".into(),
    };
    let (members, _) = merge_members(
        &local.group_sync().members,
        &remote.group_sync().members,
        &source,
    )
    .unwrap();
    assert!(members.iter().any(|m| m.device_id == "C"));
    let mut ready = local.clone();
    let mut s = ready.group_sync();
    s.members = members;
    ready.set_group_sync(s);
    let mut owner = remote.clone();
    owner.revision += 1;
    let (merged, c) = merge_group(&ready, &owner, "B", "C").unwrap();
    assert!(c.validation_error.is_none());
    assert_eq!(merged.devices.len(), 3);
}

#[test]
fn unknown_metadata_ties_are_deterministic_and_owner_removal_is_rejected() {
    let mut a = group(3);
    let mut b = a.clone();
    a.sync["future"] = json!({"a":1});
    b.sync["future"] = json!({"b":2});
    assert_eq!(join(&a, &b), join(&b, &a));
    b = a.clone();
    b.devices.retain(|d| d.device_id != "A");
    b.revision += 1;
    let (g, c) = merge_group(&a, &b, "A", "B").unwrap();
    assert_eq!(g, a);
    assert!(c.validation_error.is_some());
}

#[test]
fn topology_removal_and_return_keep_independent_owner_and_arrival_registers() {
    use kvmflow_core::layouts::*;
    let mut a = group(3);
    a.devices[2].profile_rev = 9;
    a.layouts.push(Layout {
        id: "C-policy".into(),
        name: String::new(),
        source: LayoutSource::MigratedFollow {
            device_id: "C".into(),
        },
        monitors: vec![],
    });
    a.devices[2].arrival_layout_id = Some("C-policy".into());
    let mut removed = a.clone();
    removed.devices.pop();
    let removed = stamp_local_edit(&a, &removed, "A", 10).unwrap().0;
    assert_eq!(removed.group_sync().profile_cache["C"].profile_rev, 9);
    assert_eq!(removed.group_sync().archived_arrivals["C"], "C-policy");
    assert_eq!(removed.mixed, a.mixed);
    assert_eq!(removed.group_sync().mixed, a.group_sync().mixed);
    let mut returned = a.clone();
    returned.revision = removed.revision + 1;
    returned.devices[2].profile_rev = 1;
    let result = join(&removed, &returned);
    assert_eq!(result.devices[2].profile_rev, 9);
    assert_eq!(
        result.devices[2].arrival_layout_id.as_deref(),
        Some("C-policy")
    );
    assert_eq!(
        join(&join(&a, &removed), &returned),
        join(&a, &join(&removed, &returned))
    );
    assert!(result.group_sync().profile_cache.is_empty());
    assert_eq!(SyncSummary::from_snapshot(&removed).profiles["C"], 9);
}

#[test]
fn randomized_topology_removal_return_and_owner_edits_still_obey_join_laws() {
    for n in [2, 3] {
        for initial in 1..=12 {
            let mut seed = initial;
            let mut replicas = vec![group(3); n];
            for step in 0..60 {
                let index = random(&mut seed) as usize % n;
                let old = replicas[index].clone();
                let mut new = old.clone();
                if random(&mut seed) % 3 == 0 {
                    if new.devices.len() == 3 {
                        new.devices.retain(|d| d.device_id != "C");
                    } else {
                        let p = new.group_sync().profile_cache["C"].clone();
                        let mut c = group(3).devices[2].clone();
                        c.profile_rev = p.profile_rev;
                        c.monitors = p.monitors;
                        c.available_builtin_screens = p.available_builtin_screens;
                        new.devices.push(c);
                    }
                } else {
                    new.devices[0].monitors[0].label = format!("edit-{step}");
                }
                replicas[index] = stamp_local_edit(&old, &new, "A", random(&mut seed) % 1000)
                    .unwrap()
                    .0;
                let a = &replicas[0];
                let b = &replicas[1];
                let c = &replicas[n - 1];
                assert_eq!(join(a, b), join(b, a), "seed {initial}, step {step}");
                assert_eq!(
                    join(&join(a, b), c),
                    join(a, &join(b, c)),
                    "seed {initial}, step {step}"
                );
                let combined = join(a, b);
                assert_eq!(join(&combined, &combined), combined);
                for s in [a, b] {
                    for (id, rev) in SyncSummary::from_snapshot(s).profiles {
                        assert!(SyncSummary::from_snapshot(&combined).profiles[&id] >= rev);
                    }
                }
                let dest = random(&mut seed) as usize % n;
                let source = random(&mut seed) as usize % n;
                replicas[dest] = join(&replicas[dest], &replicas[source]);
            }
        }
    }
}

#[test]
fn revocation_purges_retained_profiles_old_snapshots_do_not_restore_them_and_cache_is_bounded() {
    let a = group(3);
    let mut b = a.clone();
    b.devices.pop();
    let b = stamp_local_edit(&a, &b, "A", 1).unwrap().0;
    assert!(b.group_sync().profile_cache.contains_key("C"));
    let mut removal = b.clone();
    let mut sync = removal.group_sync();
    sync.members
        .iter_mut()
        .find(|m| m.device_id == "C")
        .unwrap()
        .removed = true;
    removal.set_group_sync(sync);
    let removed = stamp_local_edit(&b, &removal, "A", 2).unwrap().0;
    assert!(removed.group_sync().profile_cache.is_empty());
    assert!(join(&removed, &b).group_sync().profile_cache.is_empty());
    assert!(join(&b, &removed)
        .group_sync()
        .members
        .iter()
        .any(|m| m.device_id == "C" && m.removed));
    let mut over = b.clone();
    let mut sync = over.group_sync();
    let p = sync.profile_cache["C"].clone();
    for i in 0..MAX_PROFILE_REGISTERS {
        sync.profile_cache.insert(format!("old-{i}"), p.clone());
        sync.members.push(Member {
            device_id: format!("old-{i}"),
            public_key: format!("key-{i}"),
            ..Default::default()
        });
    }
    over.set_group_sync(sync);
    assert!(over.validate().is_err());
    over.revision += 1;
    let (kept, changed) = merge_sections(&b, &over).unwrap();
    assert_eq!(kept, b);
    assert!(changed.validation_error.is_some());
}
