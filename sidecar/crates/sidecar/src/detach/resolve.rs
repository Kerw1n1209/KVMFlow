//! Re-finding a detached monitor from a persisted record. Platform keys
//! (CoreGraphics display ids, Windows path ids) can be reassigned after a
//! reboot, replug or sleep, so a key alone is never trusted: it must still
//! belong to the same hardware.

use crate::backends::topology::{DetachRecord, FingerprintSource, SysDisplay};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// The record's monitor is this display (key may differ from the record's).
    Found(String),
    /// No such monitor is connected any more.
    Gone,
    /// Several monitors look the same and the key does not settle which.
    Ambiguous,
    /// Hardware evidence is weak or conflicts with an independent DDC identity.
    Unconfirmed,
}

fn strong_identity(identity: &str) -> bool {
    let parts: Vec<_> = identity.split(':').collect();
    if parts.len() == 3 && parts.iter().all(|p| u32::from_str_radix(p, 16).is_ok()) {
        // CoreGraphics vendor:model:numeric-serial. Zero serial collides for
        // monitors whose only unique identity is an alphanumeric EDID serial.
        return u32::from_str_radix(parts[2], 16).is_ok_and(|serial| serial != 0);
    }
    !identity.is_empty() // Windows monitor device path.
}
fn ddc_evidence(source: FingerprintSource, fp: &str) -> bool {
    source == FingerprintSource::Ddc && !fp.ends_with("-NOSERIAL")
}
pub fn resolve_record(record: &DetachRecord, displays: &[SysDisplay]) -> Resolved {
    let candidates: Vec<_> = displays
        .iter()
        .filter(|d| {
            d.present
                && (if record.identity.is_empty() {
                    d.fingerprint == record.fingerprint
                } else {
                    d.identity == record.identity
                })
        })
        .collect();
    let recorded_ddc = matches!(
        record.fingerprint_source,
        FingerprintSource::Ddc | FingerprintSource::Unknown
    ) && !record.fingerprint.ends_with("-NOSERIAL");
    let exact: Vec<_> = candidates
        .iter()
        .filter(|d| {
            recorded_ddc
                && ddc_evidence(d.fingerprint_source, &d.fingerprint)
                && d.fingerprint == record.fingerprint
        })
        .collect();
    if exact.len() == 1 {
        return Resolved::Found(exact[0].key.clone());
    }
    if exact.len() > 1 {
        return Resolved::Ambiguous;
    }
    let strong: Vec<_> = candidates
        .iter()
        .filter(|d| {
            strong_identity(&record.identity)
                && !(recorded_ddc
                    && ddc_evidence(d.fingerprint_source, &d.fingerprint)
                    && d.fingerprint != record.fingerprint)
        })
        .collect();
    match strong.as_slice() {
        [one] => Resolved::Found(one.key.clone()),
        many if many.len() > 1 => Resolved::Ambiguous,
        _ if candidates.is_empty() && !record.identity.is_empty() => Resolved::Gone,
        _ => Resolved::Unconfirmed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::topology::DetachMethod;

    fn record(key: &str, identity: &str) -> DetachRecord {
        DetachRecord {
            fingerprint_source: crate::backends::topology::FingerprintSource::Ddc,
            fingerprint: format!("fp-{identity}"),
            key: key.into(),
            identity: identity.into(),
            method: DetachMethod::Disabled,
            mirror_of: None,
            origins: vec![],
            topology: None,
            detached_at_ms: 0,
        }
    }

    fn display(key: &str, identity: &str) -> SysDisplay {
        SysDisplay {
            key: key.into(),
            fingerprint_source: crate::backends::topology::FingerprintSource::Ddc,
            fingerprint: format!("fp-{identity}"),
            builtin: false,
            active: true,
            is_main: false,
            present: true,
            identity: identity.into(),
        }
    }

    #[test]
    fn key_reused_by_another_monitor_is_not_the_recorded_one() {
        // After a reboot cg:2 belongs to monitor B; the detached monitor A is now cg:5.
        let displays = [display("cg:2", "B"), display("cg:5", "A")];
        assert_eq!(
            resolve_record(&record("cg:2", "A"), &displays),
            Resolved::Found("cg:5".into())
        );
    }

    #[test]
    fn key_reused_and_monitor_unplugged_is_gone_not_the_wrong_screen() {
        let displays = [display("cg:2", "B")];
        assert_eq!(
            resolve_record(&record("cg:2", "A"), &displays),
            Resolved::Gone
        );
    }

    // P0 regression: the platform synthesizes a present-but-inactive display
    // for a record whose CG id is CGS-disabled off the online list. That
    // candidate must resolve, so the manager calls attach instead of
    // reporting the monitor as unplugged.
    #[test]
    fn disabled_offline_candidate_resolves_for_attach() {
        let mut synthesized = SysDisplay {
            key: "cg:2".into(),
            fingerprint: "fp-A".into(),
            fingerprint_source: FingerprintSource::RecordHint,
            builtin: false,
            active: false,
            is_main: false,
            present: true,
            identity: "A".into(),
        };
        let displays = [display("cg:1", "B"), synthesized.clone()];
        assert_eq!(
            resolve_record(&record("cg:2", "A"), &displays),
            Resolved::Found("cg:2".into())
        );
        // Still true when the record only has the hardware identity.
        let mut weak = record("cg:2", "A");
        weak.fingerprint = "fp-A".into();
        assert_eq!(
            resolve_record(&weak, &displays),
            Resolved::Found("cg:2".into())
        );
        // A synthesized display that is not present is not a candidate.
        synthesized.present = false;
        assert_eq!(
            resolve_record(&record("cg:2", "A"), &[display("cg:1", "B"), synthesized]),
            Resolved::Gone
        );
    }

    #[test]
    fn identical_twins_are_ambiguous_even_when_the_key_matches() {
        let twins = [display("cg:7", "T"), display("cg:8", "T")];
        assert_eq!(
            resolve_record(&record("cg:2", "T"), &twins),
            Resolved::Ambiguous
        );
        assert_eq!(
            resolve_record(&record("cg:8", "T"), &twins),
            Resolved::Ambiguous
        );
    }

    #[test]
    fn zero_serial_collision_requires_independent_matching_ddc_evidence() {
        let identity = "10ac:a261:00000000";
        let mut saved = record("cg:2", identity);
        saved.fingerprint = "DEL-A261-S:AN-A".into();
        let mut candidate = display("cg:9", identity);
        candidate.fingerprint = "DEL-A261-S:AN-B".into();
        assert_eq!(
            resolve_record(&saved, &[candidate.clone()]),
            Resolved::Unconfirmed
        );
        candidate.fingerprint_source = FingerprintSource::Fallback;
        candidate.fingerprint = "DEL-A261-NOSERIAL".into();
        assert_eq!(
            resolve_record(&saved, &[candidate.clone()]),
            Resolved::Unconfirmed
        );
        candidate.fingerprint_source = FingerprintSource::RecordHint;
        candidate.fingerprint = saved.fingerprint.clone();
        assert_eq!(
            resolve_record(&saved, &[candidate.clone()]),
            Resolved::Unconfirmed
        );
        candidate.fingerprint_source = FingerprintSource::Ddc;
        assert_eq!(
            resolve_record(&saved, &[candidate]),
            Resolved::Found("cg:9".into())
        );
    }

    #[test]
    fn legacy_record_never_trusts_the_key() {
        let displays = [display("cg:2", "B")];
        assert_eq!(
            resolve_record(&record("cg:2", ""), &displays),
            Resolved::Unconfirmed
        );
        assert_eq!(
            resolve_record(&record("cg:3", ""), &displays),
            Resolved::Unconfirmed
        );
    }
}
