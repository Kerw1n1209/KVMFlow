use kvmflow_core::{config::*, identity::rename_device_id, mixed::Assignment, pairing::*};
use serde_json::json;

fn config(
    id: &str,
    name: &str,
    platform: &str,
    here: [u16; 2],
    away: [u16; 2],
    armed: bool,
) -> PairSide {
    let mut legacy = example_config();
    legacy.host_label = name.into();
    legacy.monitors[0].edid_id = "DEL-A0F1-S:CN0ABC".into();
    legacy.monitors[0].here_input = here[0];
    legacy.monitors[0].away_input = away[0];
    let mut second = legacy.monitors[0].clone();
    second.edid_id = "SAC-2763-S:screen2".into();
    second.here_input = here[1];
    second.away_input = away[1];
    legacy.monitors.push(second);
    if !armed {
        legacy.monitors[0].away_input_source = AwayInputSource::Unknown;
    }
    let mut c = match MultiDeviceConfig::migrate_legacy(legacy) {
        LegacyMigration::Runnable(c) | LegacyMigration::NeedsSetup(c) => c,
    };
    c.local_device.device_id = id.into();
    if let Some(g) = &mut c.switch_group {
        rename_device_id(g, "legacy-local", id);
    }
    PairSide::from_config(&c, platform, &format!("key-{id}"))
}
fn sides() -> (PairSide, PairSide) {
    let mut mac = config("dev-MAC", "Mac", "macos", [15, 15], [17, 16], true);
    let mut win = config("dev-WIN", "游戏 PC", "windows", [17, 17], [15, 15], true);
    mac.switch_group.as_mut().unwrap().devices[1].name = "游戏 PC".into();
    // Real migration drafts are then edited through the same model as mixed.assign.
    for side in [&mut mac, &mut win] {
        let id = if side.device_id == "dev-MAC" {
            "legacy-remote"
        } else {
            "dev-WIN"
        };
        side.switch_group
            .as_mut()
            .unwrap()
            .mixed
            .as_mut()
            .unwrap()
            .assignment = vec![Assignment {
            fingerprint: "SAC-2763-S:screen2".into(),
            device_id: id.into(),
        }];
    }
    (mac, win)
}
fn options() -> PairOptions {
    PairOptions {
        initiator_device_id: "dev-MAC".into(),
        group_random: [9; 16],
        now_ms: 100,
        ..Default::default()
    }
}
fn pair(a: &PairSide, b: &PairSide, o: &PairOptions) -> PairPlan {
    let p = plan_pair(a, b, o).unwrap();
    assert_eq!(
        p,
        plan_pair(b, a, o).unwrap(),
        "peers must recompute identical proposals"
    );
    p
}
fn established(a: &mut PairSide, b: &mut PairSide) {
    let g = pair(a, b, &options()).snapshot.unwrap();
    for s in [a, b] {
        s.switch_group = Some(g.clone());
        s.established = true;
    }
}

#[test]
fn seven_migration_examples_are_deterministic_and_never_commit_blocking_results() {
    for case in 1..=7 {
        let (mut mac, mut win) = sides();
        let mut o = options();
        match case {
            1 => {}
            2 => {
                mac = config("dev-MAC", "Mac", "macos", [15, 15], [17, 16], false);
            }
            3 => {
                win.profile.monitors[0].input = ProfileInput::Value {
                    value: 15,
                    source: InputSource::LearnedActiveRead,
                };
            }
            4 => {
                win.profile.monitors[0].fingerprint = "DEL-A0F1-4A3B2C1D".into();
                win.local_device.monitors[0].fingerprint = "DEL-A0F1-4A3B2C1D".into();
                for d in &mut win.switch_group.as_mut().unwrap().devices {
                    d.monitors
                        .iter_mut()
                        .find(|m| m.fingerprint.starts_with("DEL"))
                        .unwrap()
                        .fingerprint = "DEL-A0F1-4A3B2C1D".into();
                }
            }
            5 => {
                established(&mut mac, &mut win);
                win = config("dev-MINI", "Mac mini", "macos", [20, 20], [15, 15], false);
            }
            6 => {
                established(&mut mac, &mut win);
                win = config("dev-WIN2", "游戏 PC", "windows", [17, 17], [15, 15], false);
            }
            7 => {
                established(&mut mac, &mut win);
                win.public_key = "key-reinstalled".into();
            }
            _ => unreachable!(),
        }
        let p = pair(&mac, &win, &o);
        if p.has_blocking() {
            assert!(p.snapshot.is_none(), "example {case}");
        }
        match case {
            1 => {
                let g = p.snapshot.unwrap();
                assert!(g.group_id.starts_with("grp-"));
                assert_eq!(g.revision, 2);
                assert_eq!(
                    g.devices
                        .iter()
                        .find(|d| d.device_id == "dev-WIN")
                        .unwrap()
                        .monitors[1]
                        .local_input,
                    17
                );
                assert_eq!(
                    g.devices
                        .iter()
                        .find(|d| d.device_id == "dev-WIN")
                        .unwrap()
                        .name,
                    "游戏 PC"
                );
                assert!(p.issues.iter().any(|i| matches!(
                    i,
                    PairIssue::Info(Info::PeerGuessCorrected {
                        guessed: 16,
                        actual: 17,
                        ..
                    })
                )));
                assert!(!p
                    .issues
                    .iter()
                    .any(|i| matches!(i, PairIssue::Choice(Choice::Mixed { .. }))));
                assert!(g
                    .mixed
                    .as_ref()
                    .unwrap()
                    .assignment
                    .iter()
                    .all(|a| a.device_id == "dev-WIN"));
            }
            2 => {
                let g = p.snapshot.unwrap();
                assert_eq!(g.devices.len(), 2);
                assert_eq!(g.mixed.as_ref().unwrap().assignment[0].device_id, "dev-WIN");
            }
            3 => {
                assert!(p.has_blocking());
                assert_eq!(p.members.len(), 2);
                assert!(p.issues.iter().any(|i| matches!(
                    i,
                    PairIssue::Blocking(Blocking::DuplicateInput { value: 15, .. })
                )));
                win.profile.monitors[0].input = ProfileInput::Value {
                    value: 17,
                    source: InputSource::LearnedActiveRead,
                };
                win.profile.profile_rev += 1;
                let retry = pair(&mac, &win, &o);
                assert!(retry.snapshot.is_some());
            }
            4 => {
                assert!(p.needs_choice());
                assert!(p.snapshot.is_none());
                assert!(p.issues.iter().any(|i| matches!(
                    i,
                    PairIssue::Choice(Choice::MonitorAlias {
                        resolved: false,
                        ..
                    })
                )));
                o.monitor_aliases
                    .insert("DEL-A0F1-4A3B2C1D".into(), "DEL-A0F1-S:CN0ABC".into());
                let confirmed = pair(&mac, &win, &o);
                let g = confirmed.snapshot.unwrap();
                let d = g.devices.iter().find(|d| d.device_id == "dev-WIN").unwrap();
                assert_eq!(
                    d.monitors[0].local_fingerprint.as_deref(),
                    Some("DEL-A0F1-4A3B2C1D")
                );
            }
            5 => {
                let g = p.snapshot.unwrap();
                assert_eq!(g.devices.len(), 3);
                assert_eq!(
                    g.devices
                        .iter()
                        .find(|d| d.device_id == "dev-MINI")
                        .unwrap()
                        .port_index,
                    3
                );
                assert!(!g.group_sync().ports_confirmed);
                assert_eq!(g.group_sync().members.len(), 3);
                assert!(g
                    .group_sync()
                    .members
                    .iter()
                    .any(|m| m.device_id == "dev-MINI" && m.introduced_by == "dev-MAC"));
            }
            6 => {
                assert!(p.needs_choice());
                assert!(p.snapshot.is_none());
                o.replacement = Some(ReplacementChoice {
                    old_device_id: "dev-WIN".into(),
                    replace: true,
                });
                let fixed = pair(&mac, &win, &o);
                let g = fixed.snapshot.unwrap();
                assert_eq!(g.devices.len(), 2);
                assert!(g.devices.iter().all(|d| d.device_id != "dev-WIN"));
                assert!(g
                    .group_sync()
                    .members
                    .iter()
                    .any(|m| m.device_id == "dev-WIN" && m.removed));
                assert_eq!(
                    g.mixed.as_ref().unwrap().assignment[0].device_id,
                    "dev-WIN2"
                );
                o.replacement.as_mut().unwrap().replace = false;
                let added = pair(&mac, &win, &o);
                assert!(added.has_blocking()); // Same physical inputs conflict until owner calibrates.
                win.profile.monitors.clear();
                let pending = pair(&mac, &win, &o);
                assert!(pending
                    .issues
                    .iter()
                    .any(|i| matches!(i, PairIssue::Blocking(Blocking::SetupRequired { .. }))));
            }
            7 => {
                assert!(p.re_pair);
                let g = p.snapshot.unwrap();
                let old = mac.switch_group.as_ref().unwrap();
                assert_eq!(g.devices, old.devices);
                assert_eq!(g.mixed, old.mixed);
                assert_eq!(g.revision, old.revision);
                let m = g
                    .group_sync()
                    .members
                    .into_iter()
                    .find(|m| m.device_id == "dev-WIN")
                    .unwrap();
                assert_eq!(m.public_key, "key-reinstalled");
                assert_eq!(m.revision, 2);
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn empty_secondary_adopts_primary_shared_draft_and_reports_pending_without_guessing() {
    let (mac, mut win) = sides();
    win.switch_group = None;
    win.local_device.monitors.clear();
    win.profile.monitors[1].input = ProfileInput::Pending;
    win.profile.monitors.push(ProfileMonitor {
        fingerprint: "builtin".into(),
        name: "Laptop".into(),
        builtin: true,
        input: ProfileInput::Pending,
    });
    let p = pair(&mac, &win, &options());
    assert!(p.has_blocking());
    assert!(p.snapshot.is_none());
    assert_eq!(
        p.proposed_snapshot
            .as_ref()
            .unwrap()
            .mixed
            .as_ref()
            .unwrap()
            .assignment[0]
            .device_id,
        "dev-WIN"
    );
    assert!(p.issues.iter().any(|i|matches!(i,PairIssue::Blocking(Blocking::InputPending{device_id,..}) if device_id=="dev-WIN")));
    win.profile.monitors[1].input = ProfileInput::Value {
        value: 17,
        source: InputSource::LearnedActiveRead,
    };
    win.profile.profile_rev = 4;
    let ready = pair(&mac, &win, &options());
    let g = ready.snapshot.unwrap();
    let win = g.devices.iter().find(|d| d.device_id == "dev-WIN").unwrap();
    assert_eq!(win.profile_rev, 5);
    assert_eq!(win.available_builtin_screens, Some(1));
    assert_eq!(win.monitors.len(), 2);
    let wire = serde_json::to_value(&win).unwrap();
    assert_eq!(wire["profile_rev"], 5);
    let report:ProfileReport=serde_json::from_value(json!({"device_id":"new","name":"PC","platform":"windows","profile_rev":1,"monitors":[{"fingerprint":"DEL","name":"DELL","builtin":false,"input":{"state":"pending"}}]})).unwrap();
    assert_eq!(report.monitors[0].input, ProfileInput::Pending);
}

#[test]
fn mixed_conflict_requires_explicit_choice_and_keeps_selected_enabled_flag() {
    let (mac, mut win) = sides();
    let m = win.switch_group.as_mut().unwrap().mixed.as_mut().unwrap();
    m.assignment[0].device_id = "legacy-remote".into();
    m.enabled = true;
    let mut o = options();
    let p = pair(&mac, &win, &o);
    assert!(p.needs_choice());
    assert!(p.snapshot.is_none());
    assert!(p.issues.iter().any(|i| matches!(
        i,
        PairIssue::Choice(Choice::Mixed {
            default: MixedChoice::Basis,
            ..
        })
    )));
    o.mixed = Some(MixedChoice::Joiner);
    let chosen = pair(&mac, &win, &o);
    assert!(
        chosen
            .snapshot
            .as_ref()
            .unwrap()
            .mixed
            .as_ref()
            .unwrap()
            .enabled
    );
    o.mixed = Some(MixedChoice::Basis);
    let chosen = pair(&mac, &win, &o);
    assert!(
        !chosen
            .snapshot
            .as_ref()
            .unwrap()
            .mixed
            .as_ref()
            .unwrap()
            .enabled
    );
}

#[test]
fn ambiguous_noserial_requires_manual_mapping_and_conflicting_established_groups_reject() {
    let (mut mac, mut win) = sides();
    for (i, m) in win.profile.monitors.iter_mut().enumerate() {
        m.fingerprint = format!("DEL-A0F1-NOSERIAL-{i}");
    }
    let p = pair(&mac, &win, &options());
    assert!(p.has_blocking());
    assert!(!p
        .issues
        .iter()
        .any(|i| matches!(i, PairIssue::Choice(Choice::MonitorAlias { .. }))));
    let mut o = options();
    o.monitor_aliases
        .insert("DEL-A0F1-NOSERIAL-0".into(), "DEL-A0F1-S:CN0ABC".into());
    o.monitor_aliases
        .insert("DEL-A0F1-NOSERIAL-1".into(), "SAC-2763-S:screen2".into());
    assert!(pair(&mac, &win, &o).snapshot.is_some());
    let (a, b) = sides();
    mac = a;
    win = b;
    established(&mut mac, &mut win);
    win.switch_group.as_mut().unwrap().group_id = "another-group".into();
    assert_eq!(
        plan_pair(&mac, &win, &options()).unwrap_err().code,
        "E_LAN_GROUP_CONFLICT"
    );
}

#[test]
fn established_peer_is_basis_even_when_newcomer_initiates_and_ports_block_arming() {
    let (mut mac, mut win) = sides();
    established(&mut mac, &mut win);
    let newcomer = config("dev-C", "C", "macos", [22, 22], [15, 15], false);
    let mut o = options();
    o.initiator_device_id = "dev-C".into();
    let p = pair(&newcomer, &mac, &o);
    assert_eq!(p.basis_device_id, "dev-MAC");
    let g = p.snapshot.unwrap();
    let LegacyMigration::Runnable(mut c) = MultiDeviceConfig::migrate_legacy(example_config())
    else {
        panic!()
    };
    c.local_device = mac.local_device.clone();
    c.switch_group = Some(g);
    assert!(c.validate().is_ok());
    assert!(!c.ready_for_group_switch());
    c.switch_group.as_mut().unwrap().sync["ports_confirmed"] = json!(true);
    assert!(c.ready_for_group_switch());
}

#[test]
fn re_pair_advances_beyond_both_views_and_ignores_pre_pair_key_snapshots() {
    use kvmflow_core::sync::{merge_group, merge_members, AuthenticatedMember};
    for initiator in ["dev-MAC", "dev-WIN"] {
        let (mut mac, mut win) = sides();
        established(&mut mac, &mut win);
        for (side, mac_rev, win_rev) in [(&mut mac, 8, 1), (&mut win, 3, 9)] {
            let g = side.switch_group.as_mut().unwrap();
            let mut sync = g.group_sync();
            for member in &mut sync.members {
                member.revision = if member.device_id == "dev-MAC" {
                    mac_rev
                } else {
                    win_rev
                };
                // Even a future timestamp on an old key cannot defeat rev+1.
                member.meta.updated_at_ms = 1_000_000;
            }
            g.set_group_sync(sync);
        }
        let old_snapshots = [
            mac.switch_group.clone().unwrap(),
            win.switch_group.clone().unwrap(),
        ];
        win.public_key = "key-reinstalled".into();
        let mut o = options();
        o.initiator_device_id = initiator.into();
        let plan = pair(&mac, &win, &o);
        let repaired = plan.snapshot.unwrap();
        let members = repaired.group_sync().members;
        assert_eq!(
            members
                .iter()
                .find(|m| m.device_id == "dev-MAC")
                .unwrap()
                .revision,
            9
        );
        let win_member = members.iter().find(|m| m.device_id == "dev-WIN").unwrap();
        assert_eq!(win_member.revision, 10);
        assert_eq!(win_member.public_key, "key-reinstalled");
        let source = AuthenticatedMember {
            device_id: "dev-MAC".into(),
            public_key: mac.public_key.clone(),
        };
        for old in old_snapshots {
            assert_eq!(
                merge_members(&members, &old.group_sync().members, &source)
                    .unwrap()
                    .0,
                members
            );
            assert_eq!(
                merge_group(&repaired, &old, "dev-MAC", "dev-WIN")
                    .unwrap()
                    .0,
                repaired
            );
        }
    }
}

#[test]
fn re_pair_checks_member_revision_overflow_in_either_peer_view() {
    for observer in ["dev-MAC", "dev-WIN"] {
        for member_id in ["dev-MAC", "dev-WIN"] {
            let (mut mac, mut win) = sides();
            established(&mut mac, &mut win);
            let side = if observer == "dev-MAC" {
                &mut mac
            } else {
                &mut win
            };
            let g = side.switch_group.as_mut().unwrap();
            let mut sync = g.group_sync();
            sync.members
                .iter_mut()
                .find(|m| m.device_id == member_id)
                .unwrap()
                .revision = u64::MAX;
            g.set_group_sync(sync);
            win.public_key = "key-reinstalled".into();
            let before = (mac.clone(), win.clone());
            for (a, b) in [(&mac, &win), (&win, &mac)] {
                let error = plan_pair(a, b, &options()).unwrap_err();
                assert_eq!(error.code, "E_CONFIG_INVALID");
                assert!(error.message.contains("overflow"));
            }
            assert_eq!((mac, win), before);
        }
    }
}

#[test]
fn rejected_monitor_alias_stays_unmatched_instead_of_guessing() {
    let (mac, win) = sides();
    // Cell 4 in the migration matrix is the "backend fingerprint changed"
    // shape, which produces the auto-guessed alias choice.
    let mut win = win;
    win.profile.monitors[0].fingerprint = "DEL-A0F1-4A3B2C1D".into();
    win.local_device.monitors[0].fingerprint = "DEL-A0F1-4A3B2C1D".into();
    for d in &mut win.switch_group.as_mut().unwrap().devices {
        d.monitors
            .iter_mut()
            .find(|m| m.fingerprint.starts_with("DEL"))
            .unwrap()
            .fingerprint = "DEL-A0F1-4A3B2C1D".into();
    }
    let guessed = pair(&mac, &win, &options());
    let alias = guessed
        .issues
        .iter()
        .find_map(|issue| match issue {
            PairIssue::Choice(Choice::MonitorAlias {
                local_fingerprint,
                group_fingerprint,
                resolved: false,
                ..
            }) => Some((local_fingerprint.clone(), group_fingerprint.clone())),
            _ => None,
        })
        .expect("the same-prefix guess is offered as a choice");

    let mut reject = options();
    reject.monitor_alias_rejections.insert(alias.0.clone());
    let rejected = pair(&mac, &win, &reject);
    assert!(
        rejected.snapshot.is_none(),
        "rejected alias blocks the merge"
    );
    assert!(
        !rejected.issues.iter().any(|issue| matches!(
            issue,
            PairIssue::Choice(Choice::MonitorAlias { local_fingerprint, .. })
                if *local_fingerprint == alias.0
        )),
        "a rejected correspondence must not remain a positive alias"
    );
    assert!(
        rejected.issues.iter().any(|issue| matches!(
            issue,
            PairIssue::Blocking(Blocking::MonitorUnmatched { joiner, .. })
                if joiner.contains(&alias.0)
        )),
        "a rejected correspondence is pending/unmatched"
    );
    // The plan is still committable as pin-only (blockers only, no choice).
    assert!(!rejected.needs_choice());

    // Positive alias and rejection for the same display contradict each other.
    let mut both = reject.clone();
    both.monitor_aliases.insert(alias.0.clone(), alias.1);
    assert_eq!(
        plan_pair(&mac, &win, &both).unwrap_err().code,
        "E_CONFIG_INVALID"
    );

    // Unknown joins are rejected before any guess can be made.
    let mut unknown = options();
    unknown.monitor_alias_rejections.insert("UNKNOWN-1".into());
    assert_eq!(
        plan_pair(&mac, &win, &unknown).unwrap_err().code,
        "E_CONFIG_INVALID"
    );
}

// The 0.3 UI wrote the same group ID and mac/win draft IDs on both hosts.
fn old_ui_sides() -> (PairSide, PairSide, PairOptions) {
    let mut primary = config(
        "dev-MAC",
        "Windows workstation",
        "windows",
        [16, 8],
        [15, 7],
        true,
    );
    let mut secondary = config("dev-WIN", "Studio Mac", "macos", [15, 7], [16, 8], true);
    for side in [&mut primary, &mut secondary] {
        let group = side.switch_group.as_mut().unwrap();
        group.group_id = "local-switch-group".into();
        rename_device_id(group, "legacy-remote", "win");
        group
            .devices
            .iter_mut()
            .find(|d| d.device_id == "win")
            .unwrap()
            .name = "Old other computer".into();
    }
    let mut opts = options();
    opts.mixed = Some(MixedChoice::Basis);
    (primary, secondary, opts)
}

#[test]
fn old_ui_group_id_collision_replaces_unpinned_win_slot_with_real_owner() {
    let (a, b, opts) = old_ui_sides();
    let plan = pair(&a, &b, &opts);
    let group = plan
        .snapshot
        .expect("legacy group ID equality is not an authenticated re-pair");
    assert_eq!(group.devices.len(), 2);
    let other = group
        .devices
        .iter()
        .find(|d| d.device_id == b.device_id)
        .unwrap();
    assert_eq!(other.port_index, 2);
    assert_eq!(other.name, "Studio Mac");
    assert_eq!(
        other
            .monitors
            .iter()
            .map(|m| m.local_input)
            .collect::<Vec<_>>(),
        vec![15, 7]
    );
    assert!(!group.devices.iter().any(|d| d.device_id == "win"));
    assert!(group
        .group_sync()
        .members
        .iter()
        .any(|m| m.device_id == b.device_id));
}

#[test]
fn legacy_slot_keeps_physical_port_and_pending_input_only_offers_a_prefill() {
    let (a, mut b, opts) = old_ui_sides();
    let mut a = a;
    a.switch_group
        .as_mut()
        .unwrap()
        .devices
        .iter_mut()
        .find(|d| d.device_id == "win")
        .unwrap()
        .port_index = 7;
    b.profile.monitors[0].input = ProfileInput::Pending;
    let pending = pair(&a, &b, &opts);
    assert!(pending.snapshot.is_none());
    let data = serde_json::to_value(&pending).unwrap();
    assert!(data["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["detail"]["kind"] == "input_pending" && i["detail"]["prefill"] == 15));
    assert_eq!(
        pending
            .proposed_snapshot
            .unwrap()
            .devices
            .iter()
            .find(|d| d.device_id == b.device_id)
            .unwrap()
            .port_index,
        7
    );
    b.profile.monitors[0].input = ProfileInput::Value {
        value: 19,
        source: InputSource::LearnedActiveRead,
    };
    let actual = pair(&a, &b, &opts).snapshot.unwrap();
    let other = actual
        .devices
        .iter()
        .find(|d| d.device_id == b.device_id)
        .unwrap();
    assert_eq!(other.port_index, 7);
    assert_eq!(other.monitors[0].local_input, 19);
}

#[test]
fn a_pinned_old_id_is_never_claimed_as_an_unpaired_placeholder() {
    let (mut a, b, opts) = old_ui_sides();
    let g = a.switch_group.as_mut().unwrap();
    let mut sync = g.group_sync();
    sync.members.push(kvmflow_core::sync::Member {
        device_id: "win".into(),
        public_key: "other-pin".into(),
        platform: "windows".into(),
        ..Default::default()
    });
    g.set_group_sync(sync);
    a.established = true;
    let plan = pair(&a, &b, &opts);
    assert!(plan
        .proposed_snapshot
        .unwrap()
        .devices
        .iter()
        .any(|d| d.device_id == "win"));
}
