//! Edge cases under the parameters the app ships with (immediate departure,
//! 1s cooldown). The older adversarial tests use the conservative 5/10/15s
//! values, which no longer match what users run.

use kvmflow_core::config::DevicePattern;
use kvmflow_core::debounce::{DebounceParams, DeviceKey, GroupTracker, TrackerEvent};

fn key(vid_pid: &str, serial: &str) -> DeviceKey {
    DeviceKey {
        vid_pid: vid_pid.into(),
        serial: serial.into(),
    }
}
fn hub() -> DeviceKey {
    key("1a40:0101", "")
}
fn keyboard() -> DeviceKey {
    key("3837:303c", "K")
}
fn receiver() -> DeviceKey {
    key("373b:10c9", "R")
}

fn shipped_params() -> DebounceParams {
    DebounceParams {
        t_stable_ms: 0,
        t_absent_ms: 0,
        t_cooldown_ms: 1_000,
        quorum_peripherals: 1,
    }
}

fn hub_anchored_tracker() -> GroupTracker {
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
                serial: Some("R".into()),
            },
        ],
        shipped_params(),
    )
}

fn count_left(events: &[TrackerEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, TrackerEvent::GroupLeft { .. }))
        .count()
}

fn all_present() -> Vec<DeviceKey> {
    vec![hub(), keyboard(), receiver()]
}

/// Feed `steps` of (time_ms, devices) and return GroupLeft timestamps.
fn run(tracker: &mut GroupTracker, steps: &[(u64, Vec<DeviceKey>)]) -> Vec<u64> {
    let mut lefts = Vec::new();
    for (t, devices) in steps {
        if count_left(&tracker.update(*t, devices)) > 0 {
            lefts.push(*t);
        }
    }
    lefts
}

#[test]
fn one_real_press_triggers_exactly_once() {
    let mut tracker = hub_anchored_tracker();
    let lefts = run(
        &mut tracker,
        &[
            (0, all_present()),
            (250, all_present()),
            (500, vec![]),
            (750, vec![]),
            (5_000, all_present()),
        ],
    );
    assert_eq!(lefts, vec![500]);
}

#[test]
fn second_departure_inside_cooldown_is_suppressed() {
    let mut tracker = hub_anchored_tracker();
    let lefts = run(
        &mut tracker,
        &[
            (0, all_present()),
            (500, vec![]),
            (700, all_present()),
            (900, vec![]),
            (1_100, all_present()),
        ],
    );
    assert_eq!(
        lefts,
        vec![500],
        "bounce within the 1s cooldown must not fire again"
    );
}

#[test]
fn departure_after_cooldown_fires_again() {
    let mut tracker = hub_anchored_tracker();
    let lefts = run(
        &mut tracker,
        &[
            (0, all_present()),
            (500, vec![]),
            (2_000, all_present()),
            (4_000, vec![]),
            (6_000, all_present()),
        ],
    );
    assert_eq!(lefts, vec![500, 4_000]);
}

#[test]
fn losing_the_hub_alone_never_triggers_while_peripherals_stay() {
    let mut tracker = hub_anchored_tracker();
    let lefts = run(
        &mut tracker,
        &[
            (0, all_present()),
            (500, vec![keyboard(), receiver()]),
            (1_000, vec![keyboard(), receiver()]),
            (1_500, all_present()),
        ],
    );
    assert!(
        lefts.is_empty(),
        "a stale or missing hub entry must not switch monitors"
    );
}

#[test]
fn unplugging_each_peripheral_separately_never_triggers() {
    for gone in [keyboard(), receiver()] {
        let mut tracker = hub_anchored_tracker();
        let without: Vec<DeviceKey> = all_present().into_iter().filter(|d| *d != gone).collect();
        let lefts = run(
            &mut tracker,
            &[
                (0, all_present()),
                (500, without.clone()),
                (3_000, without),
                (4_000, all_present()),
            ],
        );
        assert!(
            lefts.is_empty(),
            "removing only {gone:?} must not switch monitors"
        );
    }
}

#[test]
fn staggered_group_departure_fires_once() {
    let mut tracker = hub_anchored_tracker();
    let lefts = run(
        &mut tracker,
        &[
            (0, all_present()),
            (500, vec![hub(), receiver()]),
            (750, vec![hub()]),
            (1_000, vec![]),
            (3_000, all_present()),
        ],
    );
    assert_eq!(lefts.len(), 1);
}

#[test]
fn many_rapid_presses_never_fire_faster_than_the_cooldown() {
    let mut tracker = hub_anchored_tracker();
    let mut steps = vec![(0u64, all_present())];
    for i in 0..40u64 {
        let base = 1_000 + i * 300;
        steps.push((base, vec![]));
        steps.push((base + 150, all_present()));
    }
    let lefts = run(&mut tracker, &steps);
    assert!(!lefts.is_empty());
    for pair in lefts.windows(2) {
        assert!(
            pair[1] - pair[0] >= 1_000,
            "triggers {pair:?} violate the cooldown"
        );
    }
}

#[test]
fn a_single_empty_enumeration_while_nothing_was_unplugged_is_a_known_trigger() {
    // Characterisation, not a desired behaviour: with immediate departure a
    // one-tick empty snapshot (for example a failed enumeration) is
    // indistinguishable from a real unplug. Raising t_absent_ms is the knob.
    let mut tracker = hub_anchored_tracker();
    let lefts = run(
        &mut tracker,
        &[(0, all_present()), (250, vec![]), (500, all_present())],
    );
    assert_eq!(lefts.len(), 1);

    let mut patient = GroupTracker::new(
        DevicePattern {
            vid_pid: "1a40:0101".into(),
            serial: None,
        },
        vec![DevicePattern {
            vid_pid: "3837:303c".into(),
            serial: Some("K".into()),
        }],
        DebounceParams {
            t_absent_ms: 1_000,
            ..shipped_params()
        },
    );
    let lefts = run(
        &mut patient,
        &[(0, all_present()), (250, vec![]), (500, all_present())],
    );
    assert!(
        lefts.is_empty(),
        "a short t_absent_ms absorbs a one-tick glitch"
    );
}
