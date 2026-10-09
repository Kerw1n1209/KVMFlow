//! KVM-5 adversarial edge tests at the core (single-copy) level:
//! - same-model monitor identity (EDID collision surface)
//! - delayed / staggered / partial USB departure-and-return shapes
//! - debounce parameter boundary sweep
//!
//! These exercise only public core APIs - no product file is modified.

use kvmflow_core::config::DevicePattern;
use kvmflow_core::debounce::{DebounceParams, DeviceKey, GroupTracker, TrackerEvent};
use kvmflow_core::edid_identity;

fn tracker(quorum: u32) -> GroupTracker {
    GroupTracker::new(
        DevicePattern {
            vid_pid: "1a40:0101".into(),
            serial: None,
        },
        vec![
            DevicePattern {
                vid_pid: "3837:303c".into(),
                serial: Some("K".into()),
            },
            DevicePattern {
                vid_pid: "373b:10c9".into(),
                serial: Some("M".into()),
            },
        ],
        DebounceParams {
            t_stable_ms: 5_000,
            t_absent_ms: 10_000,
            t_cooldown_ms: 15_000,
            quorum_peripherals: quorum,
        },
    )
}

fn hub() -> DeviceKey {
    DeviceKey {
        vid_pid: "1a40:0101".into(),
        serial: String::new(),
    }
}
fn kb() -> DeviceKey {
    DeviceKey {
        vid_pid: "3837:303c".into(),
        serial: "K".into(),
    }
}
fn mouse() -> DeviceKey {
    DeviceKey {
        vid_pid: "373b:10c9".into(),
        serial: "M".into(),
    }
}

fn lefts(events: &[TrackerEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, TrackerEvent::GroupLeft { .. }))
        .count()
}
fn returns(events: &[TrackerEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, TrackerEvent::UnexpectedReturn { .. }))
        .count()
}

// ---- same-model monitor identity ----

#[test]
fn same_model_monitors_are_distinguished_by_serial() {
    // Two identical panels (same vendor + product code) must map to distinct
    // edid_ids via the serial, or per-monitor config addressing forks.
    let a = edid_identity("SAC", 0x2466, 1, "");
    let b = edid_identity("SAC", 0x2466, 2, "");
    assert_ne!(
        a, b,
        "distinct serial numbers must produce distinct identities"
    );
    assert_eq!(a, "SAC-2466-00000001");

    let c = edid_identity("SAC", 0x2466, 0, "SER-A");
    let d = edid_identity("SAC", 0x2466, 0, "SER-B");
    assert_ne!(
        c, d,
        "string-serial path must also distinguish same-model panels"
    );
}

#[test]
fn same_model_without_any_serial_collides_and_is_detectable() {
    // The documented failure shape: identical panels with no EDID serial
    // collapse onto NOSERIAL - the identity layer cannot tell them apart, so
    // this MUST surface as a config validation error (duplicate edid_id),
    // never as silent cross-wired writes.
    let a = edid_identity("SAC", 0x2466, 0, "");
    let b = edid_identity("SAC", 0x2466, 0, "");
    assert_eq!(a, b);
    assert!(a.ends_with("NOSERIAL"));

    let mut cfg = kvmflow_core::config::example_config();
    cfg.monitors.push(cfg.monitors[0].clone()); // duplicate edid_id
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string().contains("duplicates"),
        "duplicate edid_id must be rejected: {err}"
    );
}

// ---- delayed / staggered / lost USB event shapes ----

#[test]
fn staggered_departure_fires_only_after_full_group_absence() {
    // Downstream devices may disappear before the hub. That must not start
    // the departure timer; it starts only when the hub anchor disappears.
    let mut tr = tracker(1);
    let full = vec![hub(), kb(), mouse()];
    assert!(tr.update(0, &full).is_empty());
    tr.update(5_000, &full); // GroupStable

    // mouse leaves at 6s, then keyboard at 8s; the hub remains present.
    let mut evs = Vec::new();
    evs.extend(tr.update(6_000, &[hub(), kb()]));
    evs.extend(tr.update(8_000, &[hub()])); // peripherals gone, but hub remains
    assert_eq!(lefts(&evs), 0, "no GroupLeft until T_absent elapses");
    evs.extend(tr.update(17_999, &[hub()]));
    assert_eq!(
        lefts(&evs),
        0,
        "peripheral loss alone never starts T_absent"
    );
    evs.extend(tr.update(18_000, &[])); // hub now leaves; start T_absent
    assert_eq!(lefts(&evs), 0, "hub absence window has just started");
    evs.extend(tr.update(27_999, &[]));
    assert_eq!(lefts(&evs), 0, "T_absent not yet elapsed");
    evs.extend(tr.update(28_000, &[]));
    assert_eq!(
        lefts(&evs),
        1,
        "GroupLeft exactly once, after hub absence + T_absent"
    );
}

#[test]
fn partial_return_without_anchor_is_not_an_unexpected_return() {
    // Lost-connect shape: after the group left, a single peripheral
    // re-enumerates (stale hub enumeration lost). Anchor missing -> the
    // tracker must NOT treat it as the group coming back.
    let mut tr = tracker(1);
    let full = vec![hub(), kb(), mouse()];
    tr.update(0, &full);
    tr.update(5_000, &full);
    tr.update(8_000, &[]);
    let evs = tr.update(18_000, &[]); // GroupLeft
    assert_eq!(lefts(&evs), 1);

    // keyboard alone returns, hub does not
    let evs = tr.update(30_000, &[kb()]);
    assert_eq!(
        returns(&evs),
        0,
        "peripheral without anchor is not the group"
    );

    // now the full group returns -> UnexpectedReturn
    let evs = tr.update(60_000, &full);
    assert_eq!(returns(&evs), 1);
}

#[test]
fn group_never_seen_plus_long_silence_emits_nothing() {
    // Lost-disconnect shape / boot with the switch on the other host: the
    // tracker starts absent and a quiet bus must not fabricate events.
    let mut tr = tracker(1);
    for t in [0u64, 5_000, 60_000, 600_000] {
        assert!(
            tr.update(t, &[]).is_empty(),
            "silent absent bus emitted something at t={t}"
        );
    }
    assert!(!tr.group_present());
}

#[test]
fn individual_peripheral_flapping_is_absorbed_not_triggered() {
    // One peripheral bounces (cable/contact) while the rest of the group
    // stays: quorum holds, no GroupLeft may fire at any point.
    let mut tr = tracker(1);
    let full = vec![hub(), kb(), mouse()];
    tr.update(0, &full);
    tr.update(5_000, &full);
    let mut fired = 0;
    for cycle in 0..10u64 {
        let base = 6_000 + cycle * 6_000;
        // mouse gone 3s (< T_absent 10s), then back
        fired += lefts(&tr.update(base, &[hub(), kb()]));
        fired += lefts(&tr.update(base + 3_000, &full));
        tr.update(base + 3_100, &full);
    }
    assert_eq!(
        fired, 0,
        "peripheral flap below T_absent must never trigger"
    );
}

#[test]
fn unplugging_all_peripherals_while_hub_remains_never_triggers() {
    let mut tr = tracker(1);
    let full = vec![hub(), kb(), mouse()];
    tr.update(0, &full);
    tr.update(5_000, &full);

    // Even after much longer than T_absent, losing the receiver/devices while
    // the USB Switch hub is still enumerated must not switch the monitors.
    for t in [6_000, 16_000, 40_000] {
        assert_eq!(lefts(&tr.update(t, &[hub()])), 0);
    }
    assert_eq!(lefts(&tr.update(41_000, &[])), 0);
    assert_eq!(lefts(&tr.update(51_000, &[])), 1);
}

#[test]
fn higher_quorum_survives_single_peripheral_loss() {
    // quorum=2 is required to establish the group, but after it is stable a
    // peripheral loss must not count as the hub leaving.
    let mut tr = tracker(2);
    let full = vec![hub(), kb(), mouse()];
    tr.update(0, &full);
    tr.update(5_000, &full);
    assert!(tr.group_stable());
    assert!(!tr.group_present() || tr.group_stable());
    let mut evs = Vec::new();
    evs.extend(tr.update(8_000, &[hub(), mouse()]));
    evs.extend(tr.update(40_000, &[hub(), mouse()]));
    assert_eq!(
        lefts(&evs),
        0,
        "dropping below peripheral quorum while hub remains must not trigger"
    );
    evs.extend(tr.update(41_000, &[]));
    assert_eq!(lefts(&evs), 0, "hub departure starts the absence timer");
    evs.extend(tr.update(51_000, &[]));
    assert_eq!(lefts(&evs), 1, "hub departure triggers after T_absent");
}

// ---- parameter boundary sweep ----

#[test]
fn debounce_bounds_edge_values() {
    let ok = |t_stable, t_absent, t_cooldown, quorum| {
        DebounceParams {
            t_stable_ms: t_stable,
            t_absent_ms: t_absent,
            t_cooldown_ms: t_cooldown,
            quorum_peripherals: quorum,
        }
        .validate()
        .is_ok()
    };
    // Zero confirmation time is valid for immediate switching.
    assert!(ok(0, 0, 0, 1));
    // inclusive upper bounds accepted
    assert!(ok(120_000, 300_000, 600_000, 8));
    // one step outside each upper bound rejected (the unsigned fields have no
    // representable value below the zero lower bound).
    assert!(!ok(120_001, 10_000, 0, 1));
    assert!(!ok(5_000, 300_001, 0, 1));
    assert!(!ok(5_000, 10_000, 600_001, 1));
    assert!(!ok(5_000, 10_000, 0, 0));
    assert!(!ok(5_000, 10_000, 0, 9));
}
