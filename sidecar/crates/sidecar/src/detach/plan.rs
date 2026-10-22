//! Pure detach safety rules: which display may go and which one stays.

use crate::backends::topology::SysDisplay;
use kvmflow_core::errors::{
    CoreError, E_TOPOLOGY_FAILED, E_TOPOLOGY_LAST_ACTIVE, E_TOPOLOGY_NOT_FOUND,
    E_TOPOLOGY_ONLY_DISPLAY, E_TOPOLOGY_UNSUPPORTED,
};

pub struct DetachPlan<'a> {
    pub target: &'a SysDisplay,
    /// An active display that stays: mirror target and new main display.
    pub reserve: &'a SysDisplay,
}

/// `Ok(None)`: the display is already detached, nothing to do.
///
/// Refuses, with a distinct error each, to detach the only display the system
/// knows, the last active display (for example a closed laptop lid leaves one
/// external screen), the built-in panel, and a fingerprint that is not unique.
pub fn plan_detach<'a>(
    displays: &'a [SysDisplay],
    fingerprint: &str,
) -> Result<Option<DetachPlan<'a>>, CoreError> {
    let matches: Vec<&SysDisplay> = displays
        .iter()
        .filter(|d| d.fingerprint == fingerprint)
        .collect();
    if matches.is_empty() {
        return Err(CoreError::new(
            E_TOPOLOGY_NOT_FOUND,
            format!("no display with fingerprint {fingerprint}"),
        ));
    }
    if matches.len() > 1 {
        return Err(CoreError::new(
            E_TOPOLOGY_FAILED,
            "fingerprint matches multiple monitors; refusing ambiguous detach",
        ));
    }
    let active: Vec<&SysDisplay> = matches.into_iter().filter(|d| d.active).collect();
    let target = match active.as_slice() {
        [] => return Ok(None),
        [one] => *one,
        many => {
            return Err(CoreError::new(
                E_TOPOLOGY_FAILED,
                format!(
                    "fingerprint {fingerprint} matches {} active displays; it must be unique",
                    many.len()
                ),
            ))
        }
    };
    if target.builtin {
        return Err(CoreError::new(
            E_TOPOLOGY_UNSUPPORTED,
            "the built-in display always belongs to this computer and is never detached",
        ));
    }
    if displays.len() <= 1 {
        return Err(CoreError::new(
            E_TOPOLOGY_ONLY_DISPLAY,
            "refusing to detach the only display",
        ));
    }
    let reserve = displays
        .iter()
        .filter(|d| d.active && d.key != target.key)
        .min_by_key(|d| (!d.builtin, !d.is_main))
        .ok_or_else(|| {
            CoreError::new(
                E_TOPOLOGY_LAST_ACTIVE,
                "refusing to detach the last active display",
            )
        })?;
    Ok(Some(DetachPlan { target, reserve }))
}

#[cfg(test)]
pub(crate) fn display(key: &str, fp: &str, builtin: bool, active: bool, main: bool) -> SysDisplay {
    SysDisplay {
        key: key.into(),
        fingerprint_source: crate::backends::topology::FingerprintSource::Ddc,
        fingerprint: fp.into(),
        builtin,
        active,
        is_main: main,
        present: true,
        identity: key.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_builtin_as_reserve_then_main() {
        let displays = [
            display("1", "EXT-A", false, true, true),
            display("2", "EXT-B", false, true, false),
            display("3", "BUILTIN", true, true, false),
        ];
        let plan = plan_detach(&displays, "EXT-A").unwrap().unwrap();
        assert_eq!(plan.reserve.key, "3");

        let displays = [
            display("1", "EXT-A", false, true, false),
            display("2", "EXT-B", false, true, true),
            display("4", "EXT-C", false, true, false),
        ];
        let plan = plan_detach(&displays, "EXT-A").unwrap().unwrap();
        assert_eq!(plan.reserve.key, "2");
    }

    #[test]
    fn refuses_only_display() {
        let displays = [display("1", "EXT-A", false, true, true)];
        assert_eq!(
            plan_detach(&displays, "EXT-A").err().unwrap().code,
            E_TOPOLOGY_ONLY_DISPLAY
        );
    }

    #[test]
    fn refuses_last_active_display_when_others_are_off() {
        // Lid closed: the built-in panel is online but inactive.
        let displays = [
            display("1", "EXT-A", false, true, true),
            display("2", "BUILTIN", true, false, false),
        ];
        assert_eq!(
            plan_detach(&displays, "EXT-A").err().unwrap().code,
            E_TOPOLOGY_LAST_ACTIVE
        );
    }

    #[test]
    fn already_detached_is_a_no_op() {
        let displays = [
            display("1", "EXT-A", false, true, true),
            display("2", "EXT-B", false, false, false),
        ];
        assert!(plan_detach(&displays, "EXT-B").unwrap().is_none());
    }

    #[test]
    fn refuses_builtin_and_unknown_and_ambiguous() {
        let displays = [
            display("1", "EXT-A", false, true, true),
            display("2", "BUILTIN", true, true, false),
            display("3", "SAME", false, true, false),
            display("4", "SAME", false, true, false),
        ];
        assert_eq!(
            plan_detach(&displays, "BUILTIN").err().unwrap().code,
            E_TOPOLOGY_UNSUPPORTED
        );
        assert_eq!(
            plan_detach(&displays, "NOPE").err().unwrap().code,
            E_TOPOLOGY_NOT_FOUND
        );
        assert_eq!(
            plan_detach(&displays, "SAME").err().unwrap().code,
            E_TOPOLOGY_FAILED
        );
    }
}
