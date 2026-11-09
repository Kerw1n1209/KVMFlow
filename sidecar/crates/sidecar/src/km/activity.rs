//! Reads who holds the keyboard and mouse from the server engine's log. The
//! line format is fixed by the bundled engine version.
use super::profile::ScreenName;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Holder {
    #[default]
    Server,
    Client,
}

/// The new holder, if `line` reports a switch between exactly these two screens.
pub fn switched_to(line: &str, server: &ScreenName, client: &ScreenName) -> Option<Holder> {
    let message = line.strip_prefix('[')?.split_once("] INFO: ")?.1;
    // "jump" is the engine sending input home after the client disconnected.
    let reports = |from: &ScreenName, to: &ScreenName| {
        ["switch", "jump"].iter().any(|verb| {
            let prefix = format!(
                "{verb} from \"{}\" to \"{}\" at ",
                from.as_str(),
                to.as_str()
            );
            message
                .strip_prefix(&prefix)
                .and_then(|at| at.trim_end().split_once(','))
                .is_some_and(|(x, y)| x.parse::<i32>().is_ok() && y.parse::<i32>().is_ok())
        })
    };
    if reports(server, client) {
        Some(Holder::Client)
    } else if reports(client, server) {
        Some(Holder::Server)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> (ScreenName, ScreenName) {
        (
            ScreenName::from_device_id("mac-1").unwrap(),
            ScreenName::from_device_id("win-2").unwrap(),
        )
    }

    #[test]
    fn reads_both_directions() {
        let (server, client) = names();
        let out = "[2026-11-09T03:13:00] INFO: switch from \"kf-mac-1\" to \"kf-win-2\" at 960,540";
        let back =
            "[2026-11-09T03:13:04] INFO: switch from \"kf-win-2\" to \"kf-mac-1\" at -12,0\n";
        assert_eq!(switched_to(out, &server, &client), Some(Holder::Client));
        assert_eq!(switched_to(back, &server, &client), Some(Holder::Server));
    }

    #[test]
    fn a_client_disconnect_sends_input_home() {
        let (server, client) = names();
        let line = "[2026-11-09T03:13:00] INFO: jump from \"kf-win-2\" to \"kf-mac-1\" at 960,540";
        assert_eq!(switched_to(line, &server, &client), Some(Holder::Server));
    }

    #[test]
    fn ignores_everything_else() {
        let (server, client) = names();
        for line in [
            "",
            "switch from \"kf-mac-1\" to \"kf-win-2\" at 1,2",
            "[t] DEBUG: switch from \"kf-mac-1\" to \"kf-win-2\" at 1,2",
            "[t] INFO: switch from \"kf-mac-1\" to \"kf-other\" at 1,2",
            "[t] INFO: switch from \"kf-mac-1\" to \"kf-mac-1\" at 1,2",
            "[t] INFO: switch from \"kf-mac-1\" to \"kf-win-2\" at 1",
            "[t] INFO: switch from \"kf-mac-1\" to \"kf-win-2\" at a,b",
            "[t] INFO: note switch from \"kf-mac-1\" to \"kf-win-2\" at 1,2",
        ] {
            assert_eq!(switched_to(line, &server, &client), None, "{line:?}");
        }
    }
}
