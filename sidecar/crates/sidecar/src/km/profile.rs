//! Generates the engine profile: Deskflow's settings file, the server layout
//! and the single-entry trust database. Every value written here is validated
//! or derived, so nothing from the LAN can inject settings or layout lines.
use super::{identity::Fingerprint, private_dir, write_private, KmError, Result};
use std::{
    net::IpAddr,
    path::{Path, PathBuf},
};

pub const SETTINGS_FILE: &str = "Deskflow.conf";
const LAYOUT_FILE: &str = "server.conf";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Server,
    Client,
}

/// Where the client's screen sits relative to the server's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}
impl Side {
    /// (server edge leading to the client, client edge leading back).
    fn edges(self) -> (&'static str, &'static str) {
        match self {
            Side::Left => ("left", "right"),
            Side::Right => ("right", "left"),
            Side::Top => ("up", "down"),
            Side::Bottom => ("down", "up"),
        }
    }
}

/// A Deskflow computer name derived from a KVMFlow device id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenName(String);
impl ScreenName {
    pub fn from_device_id(id: &str) -> Result<Self> {
        let valid = !id.is_empty()
            && id.len() <= 64
            && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if !valid {
            return Err(KmError::new(
                "E_KM_PROFILE",
                "device id cannot name a screen",
            ));
        }
        Ok(Self(format!("kf-{}", id.to_ascii_lowercase())))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A Deskflow keystroke such as `control+alt+BackSpace`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keystroke(String);
impl Keystroke {
    pub fn parse(text: &str) -> Result<Self> {
        let valid = !text.is_empty()
            && text.len() <= 64
            && text
                .split('+')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric()));
        if !valid {
            return Err(KmError::new("E_KM_PROFILE", "invalid shortcut"));
        }
        Ok(Self(text.into()))
    }
}

pub struct Profile<'a> {
    pub role: Role,
    pub server: ScreenName,
    pub client: ScreenName,
    pub port: u16,
    /// The server's address as seen on the authenticated LAN link. Clients only.
    pub server_address: Option<IpAddr>,
    /// `None` leaves no shared edge; shortcuts still move input.
    pub side: Option<Side>,
    pub identity_pem: &'a Path,
    pub peer: Fingerprint,
    pub clipboard: bool,
    /// Moves input to the other computer and back.
    pub toggle: Option<Keystroke>,
    /// Always returns input to the server's own screen.
    pub home: Option<Keystroke>,
    /// The client turns the server's Super into Control and back.
    pub swap_command_control: bool,
    pub invert_scroll: bool,
    /// Both computers stay awake while the engine runs.
    pub keep_awake: bool,
}

/// Writes the profile into `dir` and returns the settings file to pass to the
/// engine. The trust database lists the current peer only, so a computer that
/// was unpaired or replaced cannot connect with an old certificate.
pub fn write(dir: &Path, profile: &Profile) -> Result<PathBuf> {
    if profile.server == profile.client {
        return Err(KmError::new("E_KM_PROFILE", "both computers have one name"));
    }
    if profile.port == 0 {
        return Err(KmError::new("E_KM_PROFILE", "no engine port"));
    }
    let tls = dir.join("tls");
    private_dir(dir)?;
    private_dir(&tls)?;
    let (trust, mode, local) = match profile.role {
        Role::Server => ("trusted-clients", 2, &profile.server),
        Role::Client => ("trusted-servers", 1, &profile.client),
    };
    write_private(
        &tls.join(trust),
        format!("{}\n", profile.peer).as_bytes(),
        true,
    )?;
    let layout = dir.join(LAYOUT_FILE);
    write_private(&layout, server_layout(profile).as_bytes(), true)?;
    let remote = match (profile.role, profile.server_address) {
        (Role::Client, Some(IpAddr::V6(address))) => format!("[{address}]"),
        (Role::Client, Some(address)) => address.to_string(),
        (Role::Client, None) => {
            return Err(KmError::new("E_KM_PROFILE", "no server address"));
        }
        (Role::Server, _) => String::new(),
    };
    let text = format!(
        "[core]\ncomputerName={local}\ncoreMode={mode}\nprocessMode=0\nport={port}\ninterface=\npreventSleep={keep_awake}\n\n\
         [security]\ntlsEnabled=true\ncheckPeerFingerprints=true\ncertificate={certificate}\n\n\
         [client]\nremoteHost={remote}\ninvertYScroll={invert}\ninvertXScroll={invert}\n\n\
         [server]\nexternalConfig=true\nexternalConfigFile={layout}\nenableClipboard={clipboard}\n\n\
         [log]\nlevel=INFO\ntoFile=false\n\n\
         [computer_{server}]\nname={server}\n\n[computer_{client}]\nname={client}\n{modifiers}",
        local = local.as_str(),
        port = profile.port,
        certificate = ini_path(profile.identity_pem)?,
        layout = ini_path(&layout)?,
        clipboard = profile.clipboard,
        keep_awake = profile.keep_awake,
        invert = profile.invert_scroll,
        modifiers = if profile.swap_command_control {
            "modifierCtrl=super\nmodifierSuper=ctrl\n"
        } else {
            ""
        },
        server = profile.server.as_str(),
        client = profile.client.as_str(),
    );
    let settings = dir.join(SETTINGS_FILE);
    write_private(&settings, text.as_bytes(), true)?;
    Ok(settings)
}

fn server_layout(profile: &Profile) -> String {
    let (server, client) = (profile.server.as_str(), profile.client.as_str());
    let mut text = format!("# Generated by KVMFlow.\nsection: links\n\t{server}:\n");
    if let Some((out, _)) = profile.side.map(Side::edges) {
        text += &format!("\t\t{out} = {client}\n");
    }
    text += &format!("\t{client}:\n");
    if let Some((_, back)) = profile.side.map(Side::edges) {
        text += &format!("\t\t{back} = {server}\n");
    }
    text += "end\nsection: options\n";
    if let Some(Keystroke(keys)) = &profile.toggle {
        text += &format!("\tkeystroke({keys}) = switchToNextScreen\n");
    }
    if let Some(Keystroke(keys)) = &profile.home {
        text += &format!("\tkeystroke({keys}) = switchToScreen({server})\n");
    }
    text + "end\n"
}

/// Qt reads forward slashes on every platform; quotes or line breaks in a
/// path would end the value, so such paths are refused rather than escaped.
fn ini_path(path: &Path) -> Result<String> {
    let text = path
        .to_str()
        .ok_or_else(|| KmError::new("E_KM_PROFILE", "path is not valid text"))?
        .replace('\\', "/");
    if text.contains(['"', '\r', '\n']) {
        return Err(KmError::new(
            "E_KM_PROFILE",
            "path cannot be written to settings",
        ));
    }
    Ok(format!("\"{text}\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn profile<'a>(role: Role, pem: &'a Path) -> Profile<'a> {
        Profile {
            role,
            server: ScreenName::from_device_id("Mac-1").unwrap(),
            client: ScreenName::from_device_id("win-2").unwrap(),
            port: 24800,
            server_address: Some("192.168.1.20".parse().unwrap()),
            side: Some(Side::Right),
            identity_pem: pem,
            peer: Fingerprint::of_der(b"peer"),
            clipboard: true,
            toggle: Some(Keystroke::parse("control+alt+Tab").unwrap()),
            home: Some(Keystroke::parse("control+alt+BackSpace").unwrap()),
            swap_command_control: false,
            invert_scroll: false,
            keep_awake: false,
        }
    }

    #[test]
    fn server_profile_trusts_only_the_client() {
        let dir = tempfile::tempdir().unwrap();
        let pem = dir.path().join("identity.pem");
        let settings = write(dir.path(), &profile(Role::Server, &pem)).unwrap();
        let text = fs::read_to_string(settings).unwrap();
        assert!(text.contains("computerName=kf-mac-1\ncoreMode=2\n"));
        assert!(text.contains("checkPeerFingerprints=true"));
        assert!(text.contains("[computer_kf-win-2]\nname=kf-win-2\n"));
        let trust = fs::read_to_string(dir.path().join("tls/trusted-clients")).unwrap();
        assert_eq!(trust, format!("{}\n", Fingerprint::of_der(b"peer")));
        assert!(!dir.path().join("tls/trusted-servers").exists());
        let layout = fs::read_to_string(dir.path().join(LAYOUT_FILE)).unwrap();
        assert_eq!(
            layout,
            "# Generated by KVMFlow.\nsection: links\n\tkf-mac-1:\n\t\tright = kf-win-2\n\
             \tkf-win-2:\n\t\tleft = kf-mac-1\nend\nsection: options\n\
             \tkeystroke(control+alt+Tab) = switchToNextScreen\n\
             \tkeystroke(control+alt+BackSpace) = switchToScreen(kf-mac-1)\nend\n"
        );
    }

    #[test]
    fn input_preferences_reach_the_engine_settings() {
        let dir = tempfile::tempdir().unwrap();
        let pem = dir.path().join("identity.pem");
        let mut p = profile(Role::Server, &pem);
        let text = fs::read_to_string(write(dir.path(), &p).unwrap()).unwrap();
        assert!(text.contains("preventSleep=false\n"));
        assert!(text.contains("invertYScroll=false\ninvertXScroll=false\n"));
        assert!(!text.contains("modifierCtrl"));

        p.swap_command_control = true;
        p.invert_scroll = true;
        p.keep_awake = true;
        let text = fs::read_to_string(write(dir.path(), &p).unwrap()).unwrap();
        assert!(text.contains("preventSleep=true\n"));
        assert!(text.contains("invertYScroll=true\ninvertXScroll=true\n"));
        assert!(text.ends_with(
            "[computer_kf-win-2]\nname=kf-win-2\nmodifierCtrl=super\nmodifierSuper=ctrl\n"
        ));
    }

    #[test]
    fn client_profile_names_the_server_address() {
        let dir = tempfile::tempdir().unwrap();
        let pem = dir.path().join("identity.pem");
        let mut client = profile(Role::Client, &pem);
        let text = fs::read_to_string(write(dir.path(), &client).unwrap()).unwrap();
        assert!(text.contains("computerName=kf-win-2\ncoreMode=1\n"));
        assert!(text.contains("remoteHost=192.168.1.20\n"));
        assert!(dir.path().join("tls/trusted-servers").exists());
        client.server_address = Some("fe80::1".parse().unwrap());
        let text = fs::read_to_string(write(dir.path(), &client).unwrap()).unwrap();
        assert!(text.contains("remoteHost=[fe80::1]\n"));
        client.server_address = None;
        assert!(write(dir.path(), &client).is_err());
    }

    #[test]
    fn no_side_leaves_only_shortcuts() {
        let pem = Path::new("/k/identity.pem");
        let mut p = profile(Role::Server, pem);
        p.side = None;
        let layout = server_layout(&p);
        assert!(layout.contains("\tkf-mac-1:\n\tkf-win-2:\nend\n"));
        assert!(layout.contains("switchToNextScreen"));
    }

    #[test]
    fn rejects_values_that_could_inject_lines() {
        for id in ["", "a b", "a\nb", "a=b", "a.b", "ab]", &"a".repeat(65)] {
            assert!(ScreenName::from_device_id(id).is_err(), "{id:?}");
        }
        for keys in [
            "",
            "control+",
            "+a",
            "a b",
            "a)\n\tkeystroke(b",
            "control+alt+Tab=",
        ] {
            assert!(Keystroke::parse(keys).is_err(), "{keys:?}");
        }
        assert!(ini_path(Path::new("/a\"b/identity.pem")).is_err());
        assert!(ini_path(Path::new("/a\nb/identity.pem")).is_err());
        assert_eq!(
            ini_path(Path::new(r"C:\Users\a\identity.pem")).unwrap(),
            "\"C:/Users/a/identity.pem\""
        );
    }

    #[test]
    fn one_computer_cannot_be_both_ends() {
        let pem = Path::new("/k/identity.pem");
        let mut p = profile(Role::Server, pem);
        p.client = p.server.clone();
        assert!(write(tempfile::tempdir().unwrap().path(), &p).is_err());
    }
}
