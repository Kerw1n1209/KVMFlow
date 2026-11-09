//! Keeps the keyboard and mouse engine running for the paired computer and
//! turns its switches into the monitor writes a USB switch would cause. Only
//! the server's engine sees a switch, so it tells the client over the LAN link.
use super::*;
use crate::km::{
    self,
    activity::{self, Holder},
    engine::{self, Engine, Event, Launch},
    identity::{self, EngineIdentity, Fingerprint},
    permissions::{self, Permission, Permissions},
    profile::{self, Keystroke, Role, ScreenName},
    runtime as engine_runtime,
};
use kvmflow_core::{config::KmConfig, debounce::TrackerEvent, statemachine::Input};
use std::{
    net::IpAddr,
    path::PathBuf,
    sync::mpsc::{channel, Sender},
    thread,
};

/// Away from Deskflow's default, so a standalone Deskflow can keep its port.
const ENGINE_PORT: u16 = 24810;
const RETRY_MS: u64 = 3_000;
const HELLO_RETRY_MS: u64 = 2_000;
const SWITCH_RETRY_MS: u64 = 1_000;
const PERMISSION_POLL_MS: u64 = 2_000;
const DEFAULT_HOTKEY: &str = "Control+Alt+k";
const DEFAULT_HOME_HOTKEY: &str = "Control+Alt+Escape";
/// Runs a stand-in engine, which needs no privacy grants.
const TEST_ENGINE: &str = "KVMFLOW_KM_ENGINE";
const UNCHECKED_PERMISSIONS: Permissions = Permissions {
    accessibility: false,
    input_monitoring: false,
};

#[derive(Default)]
pub(crate) struct KmState {
    /// Bundled manifest and archive; absent in builds without the engine.
    pub(crate) resources: Option<PathBuf>,
    pub(crate) wake: Option<Sender<Msg>>,
    engine: Option<Engine>,
    running: Option<Plan>,
    identity: Option<EngineIdentity>,
    /// The peer's certificate, heard over the authenticated LAN link.
    peer: Option<(String, Fingerprint)>,
    addresses: BTreeMap<String, IpAddr>,
    /// (peer, config revision) of the last hello and when it went out.
    hello_sent: Option<((String, u64), u64)>,
    hello_acked: Option<(String, u64)>,
    holder: Holder,
    switch_seq: u64,
    unconfirmed_switch: Option<SentSwitch>,
    error: Option<&'static str>,
    next_start_ms: u64,
    permissions: Option<(u64, Permissions)>,
    published: Option<Value>,
}

struct SentSwitch {
    client: String,
    holder: Holder,
    seq: u64,
    at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Plan {
    role: Role,
    server: String,
    client: String,
    server_address: Option<IpAddr>,
    peer: Fingerprint,
    hotkey: String,
    home_hotkey: String,
    clipboard: bool,
    swap_command_control: bool,
    invert_scroll: bool,
    keep_awake: bool,
}

/// The tracker event a new holder means for one side of the pair.
fn tracker_event(role: Role, holder: Holder) -> TrackerEvent {
    let here = matches!(
        (role, holder),
        (Role::Server, Holder::Server) | (Role::Client, Holder::Client)
    );
    if here {
        TrackerEvent::GroupArrived { stable_for_ms: 0 }
    } else {
        TrackerEvent::GroupLeft { absent_for_ms: 0 }
    }
}

/// True when `candidate` should replace `current`. Equal revisions resolve to
/// the lower device id so two simultaneous edits still converge.
fn adopts(current: Option<&KmConfig>, candidate: &KmConfig, source: &str, local: &str) -> bool {
    match current {
        None => true,
        Some(current) if current == candidate => false,
        Some(current) => {
            candidate.revision > current.revision
                || (candidate.revision == current.revision && source < local)
        }
    }
}

/// A shortcut needs a modifier, or it would swallow the key from typing.
fn valid_hotkey(hotkey: &str) -> bool {
    Keystroke::parse(hotkey).is_ok() && hotkey.contains('+')
}

/// One keystroke can only run one action.
fn valid_hotkeys(sharing: &KmConfig) -> bool {
    valid_hotkey(&sharing.hotkey)
        && valid_hotkey(&sharing.home_hotkey)
        && sharing.hotkey != sharing.home_hotkey
}

impl Runtime {
    pub(crate) fn km_sharing_enabled(&self) -> bool {
        self.config.as_ref().is_some_and(|c| c.km_enabled())
    }

    fn km_peer_device(&self) -> Option<String> {
        let config = self.config.as_ref()?;
        let group = config.switch_group.as_ref()?;
        match group.devices.as_slice() {
            [a, b] if a.device_id == config.local_device.device_id => Some(b.device_id.clone()),
            [a, b] if b.device_id == config.local_device.device_id => Some(a.device_id.clone()),
            _ => None,
        }
    }

    fn km_root(&self) -> PathBuf {
        self.config_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("km")
    }

    fn km_identity(&mut self) -> km::Result<Fingerprint> {
        if self.lan_runtime.km.identity.is_none() {
            let root = self.km_root();
            km::private_dir(&root)?;
            self.lan_runtime.km.identity = Some(identity::load_or_create(&root.join("identity"))?);
        }
        Ok(self
            .lan_runtime
            .km
            .identity
            .as_ref()
            .expect("identity was just loaded")
            .fingerprint)
    }

    fn km_permissions(&mut self) -> Permissions {
        if std::env::var_os(TEST_ENGINE).is_some() {
            return Permissions {
                accessibility: true,
                input_monitoring: true,
            };
        }
        let now = now_ms();
        match self.lan_runtime.km.permissions {
            Some((at, current)) if now.saturating_sub(at) < PERMISSION_POLL_MS => current,
            _ => {
                let current = permissions::current();
                self.lan_runtime.km.permissions = Some((now, current));
                current
            }
        }
    }

    /// macOS can take half a second to answer, so the main loop only asks
    /// while sharing depends on it. Otherwise it repeats the last answer the
    /// settings page asked for.
    fn km_cached_permissions(&mut self) -> Permissions {
        if self.km_sharing_enabled() {
            return self.km_permissions();
        }
        self.lan_runtime
            .km
            .permissions
            .map_or(UNCHECKED_PERMISSIONS, |(_, known)| known)
    }

    pub(super) fn km_note_peers(&mut self, peers: &[crate::lan::conn::PeerStatus]) {
        for peer in peers {
            if let Some(address) = peer.addr {
                self.lan_runtime
                    .km
                    .addresses
                    .insert(peer.device_id.clone(), address.ip());
            }
            if let Some(platform) = &peer.platform {
                self.lan_runtime
                    .peer_platforms
                    .insert(peer.device_id.clone(), platform.clone());
            }
        }
    }

    /// Repeats until acknowledged: a frame on a session the dial race just
    /// replaced is dropped without any error reaching the sender.
    fn km_send_hello(&mut self, ack: bool) {
        let Some(peer) = self.km_peer_device() else {
            return;
        };
        if !self.lan_runtime.online.contains(&peer) {
            self.lan_runtime.km.hello_sent = None;
            self.lan_runtime.km.hello_acked = None;
            return;
        }
        let config = self.config.as_ref().and_then(|c| c.km.clone());
        let revision = config.as_ref().map(|c| c.revision);
        let Some(revision) = revision.or(ack.then_some(0)) else {
            return;
        };
        let sent = (peer.clone(), revision);
        let now = now_ms();
        let waiting = self
            .lan_runtime
            .km
            .hello_sent
            .as_ref()
            .is_some_and(|(last, at)| *last == sent && now.saturating_sub(*at) < HELLO_RETRY_MS);
        if !ack && (waiting || self.lan_runtime.km.hello_acked.as_ref() == Some(&sent)) {
            return;
        }
        let Ok(fingerprint) = self.km_identity() else {
            return;
        };
        let body = json!({"fingerprint": fingerprint.to_string(), "ack": ack, "config": config});
        if self
            .lan_send(&peer, Envelope::new("km.hello", body))
            .is_ok()
            && !ack
        {
            self.lan_runtime.km.hello_sent = Some((sent, now));
        }
    }

    pub(super) fn km_receive_hello(&mut self, source: &Source, body: &Value) {
        if self.km_peer_device().as_deref() != Some(source.device_id.as_str()) {
            return;
        }
        let fingerprint = body
            .get("fingerprint")
            .and_then(Value::as_str)
            .and_then(|text| Fingerprint::parse(text).ok());
        let Some(fingerprint) = fingerprint else {
            return;
        };
        self.lan_runtime.km.peer = Some((source.device_id.clone(), fingerprint));
        let remote = body
            .get("config")
            .cloned()
            .and_then(|value| serde_json::from_value::<KmConfig>(value).ok());
        if let (Some(remote), Some(mut config)) = (remote, self.config.clone()) {
            let local = config.local_device.device_id.clone();
            let known = config.switch_group.as_ref().is_some_and(|group| {
                group
                    .devices
                    .iter()
                    .any(|d| d.device_id == remote.server_device_id)
            });
            if known
                && valid_hotkeys(&remote)
                && adopts(config.km.as_ref(), &remote, &source.device_id, &local)
            {
                config.km = Some(remote);
                if let Err(error) = self.lan_save(config) {
                    self.emit_event(
                        "km.config_rejected",
                        Level::Warn,
                        json!({"error": error.to_string()}),
                    );
                }
            }
        }
        if body.get("ack").and_then(Value::as_bool) == Some(true) {
            self.lan_runtime.km.hello_acked = self
                .config
                .as_ref()
                .and_then(|c| c.km.as_ref())
                .map(|km| (source.device_id.clone(), km.revision));
        } else {
            self.km_send_hello(true);
        }
        self.km_publish();
    }

    pub(super) fn km_receive_switch(&mut self, source: &Source, body: &Value) {
        let Some(plan) = self.lan_runtime.km.running.clone() else {
            return;
        };
        if plan.role != Role::Client || plan.server != source.device_id {
            return;
        }
        let holder = match body.get("holder").and_then(Value::as_str) {
            Some("client") => Holder::Client,
            Some("server") => Holder::Server,
            _ => return,
        };
        self.km_set_holder(Role::Client, holder);
        if let Some(seq) = body.get("seq").and_then(Value::as_u64) {
            let ack = Envelope::new("km.switch.ack", json!({"seq": seq}));
            let _ = self.lan_send(&source.device_id, ack);
        }
    }

    pub(super) fn km_receive_switch_ack(&mut self, source: &Source, body: &Value) {
        let seq = body.get("seq").and_then(Value::as_u64);
        let state = &mut self.lan_runtime.km;
        let confirmed = state
            .unconfirmed_switch
            .as_ref()
            .is_some_and(|sent| sent.client == source.device_id && Some(sent.seq) == seq);
        if confirmed {
            state.unconfirmed_switch = None;
        }
    }

    /// Moves the server to `holder` and tells the client, replacing any older
    /// switch it has not confirmed.
    fn km_server_switched(&mut self, client: &str, holder: Holder) {
        self.km_set_holder(Role::Server, holder);
        let state = &mut self.lan_runtime.km;
        state.switch_seq += 1;
        state.unconfirmed_switch = Some(SentSwitch {
            client: client.to_string(),
            holder,
            seq: state.switch_seq,
            at_ms: 0,
        });
        self.km_send_switch();
    }

    /// Repeats until confirmed, for the same reason the hello does.
    fn km_send_switch(&mut self) {
        let now = now_ms();
        let Some(sent) = self.lan_runtime.km.unconfirmed_switch.as_mut() else {
            return;
        };
        if now.saturating_sub(sent.at_ms) < SWITCH_RETRY_MS {
            return;
        }
        sent.at_ms = now;
        let holder = match sent.holder {
            Holder::Server => "server",
            Holder::Client => "client",
        };
        let body = json!({"holder": holder, "seq": sent.seq});
        let client = sent.client.clone();
        let _ = self.lan_send(&client, Envelope::new("km.switch", body));
    }

    /// Runs the same path a USB switch does, so routing, retries and the
    /// setup test behave identically.
    fn km_set_holder(&mut self, role: Role, holder: Holder) {
        if self.lan_runtime.km.holder == holder {
            return;
        }
        self.lan_runtime.km.holder = holder;
        let event = tracker_event(role, holder);
        self.handle_multi_device_tracker_event(&event);
        let actions = self.fsm.handle(Input::Tracker(event), now_ms());
        self.run_actions(actions);
        self.km_publish();
    }

    /// Input is back with the server once the engine or the link is gone. Only
    /// the server writes: a client cannot tell a blip from a real return.
    fn km_input_released(&mut self, previous: &Plan) {
        self.lan_runtime.km.unconfirmed_switch = None;
        if self.lan_runtime.km.holder == Holder::Server {
            return;
        }
        match previous.role {
            Role::Server => self.km_server_switched(&previous.client, Holder::Server),
            Role::Client => self.lan_runtime.km.holder = Holder::Server,
        }
    }

    /// The engine takes no command to move input, but the server sends input
    /// home when its client disconnects, and the client then reconnects.
    fn km_release(&mut self) {
        let is_client = self
            .lan_runtime
            .km
            .running
            .as_ref()
            .is_some_and(|plan| plan.role == Role::Client);
        if !is_client || self.lan_runtime.km.holder != Holder::Client {
            return;
        }
        self.km_set_holder(Role::Client, Holder::Server);
        self.lan_runtime.km.engine = None;
        self.lan_runtime.km.next_start_ms = 0;
    }

    fn km_plan(&mut self) -> std::result::Result<Option<Plan>, &'static str> {
        if self.released_topology.is_some() || !self.enabled {
            return Ok(None);
        }
        let Some(config) = self.config.as_ref() else {
            return Ok(None);
        };
        let Some(sharing) = config.km.clone().filter(|km| km.enabled) else {
            return Ok(None);
        };
        let local = config.local_device.device_id.clone();
        let Some(peer) = self.km_peer_device() else {
            return Err("E_KM_TWO_COMPUTERS");
        };
        let role = if sharing.server_device_id == local {
            Role::Server
        } else if sharing.server_device_id == peer {
            Role::Client
        } else {
            return Err("E_KM_NO_SERVER");
        };
        let granted = self.km_permissions();
        if !granted.accessibility || (role == Role::Server && !granted.input_monitoring) {
            return Err("E_KM_PERMISSION");
        }
        if !self.lan_runtime.online.contains(&peer) {
            return Ok(None);
        }
        let Some(fingerprint) = self
            .lan_runtime
            .km
            .peer
            .as_ref()
            .filter(|(device, _)| *device == peer)
            .map(|(_, fingerprint)| *fingerprint)
        else {
            return Ok(None);
        };
        let server_address = match role {
            Role::Server => None,
            Role::Client => match self.lan_runtime.km.addresses.get(&peer) {
                Some(address) => Some(*address),
                None => return Ok(None),
            },
        };
        // Two Macs or two PCs already agree on what these keys mean.
        let mixed_platforms = self
            .lan_runtime
            .peer_platforms
            .get(&peer)
            .is_some_and(|platform| platform != std::env::consts::OS);
        let (server, client) = match role {
            Role::Server => (local, peer),
            Role::Client => (peer, local),
        };
        Ok(Some(Plan {
            role,
            server,
            client,
            server_address,
            peer: fingerprint,
            hotkey: sharing.hotkey,
            home_hotkey: sharing.home_hotkey,
            clipboard: sharing.clipboard,
            swap_command_control: sharing.swap_command_control && mixed_platforms,
            invert_scroll: sharing.invert_scroll,
            keep_awake: sharing.keep_awake,
        }))
    }

    fn km_binary(&self) -> km::Result<PathBuf> {
        if let Some(path) = std::env::var_os(TEST_ENGINE) {
            return Ok(path.into());
        }
        let resources = self
            .lan_runtime
            .km
            .resources
            .as_ref()
            .ok_or_else(|| km::KmError::new("E_KM_RUNTIME", "the engine is not bundled"))?;
        Ok(engine_runtime::install(resources, &self.km_root())?.binary)
    }

    fn km_start(&mut self, plan: &Plan) -> km::Result<Engine> {
        self.km_identity()?;
        let binary = self.km_binary()?;
        let pem = self
            .lan_runtime
            .km
            .identity
            .as_ref()
            .expect("identity was just loaded")
            .pem_path
            .clone();
        let directory = self.km_root().join("profile");
        let settings = profile::write(
            &directory,
            &profile::Profile {
                role: plan.role,
                server: ScreenName::from_device_id(&plan.server)?,
                client: ScreenName::from_device_id(&plan.client)?,
                port: ENGINE_PORT,
                server_address: plan.server_address,
                side: None,
                identity_pem: &pem,
                peer: plan.peer,
                clipboard: plan.clipboard,
                toggle: Some(Keystroke::parse(&plan.hotkey)?),
                home: Some(Keystroke::parse(&plan.home_hotkey)?),
                swap_command_control: plan.swap_command_control,
                invert_scroll: plan.invert_scroll,
                keep_awake: plan.keep_awake,
            },
        )?;
        let (events, received) = channel();
        if let Some(wake) = self.lan_runtime.km.wake.clone() {
            thread::spawn(move || {
                for event in received {
                    if wake.send(Msg::Km(event)).is_err() {
                        break;
                    }
                }
            });
        }
        Engine::start(
            &Launch {
                binary: &binary,
                role: plan.role,
                settings: &settings,
                namespace: &engine::namespace(&directory),
                port: ENGINE_PORT,
            },
            events,
        )
    }

    /// Called every pass of the main loop.
    pub(crate) fn poll_km(&mut self) {
        self.km_send_hello(false);
        self.km_send_switch();
        let (plan, error) = match self.km_plan() {
            Ok(plan) => (plan, None),
            Err(code) => (None, Some(code)),
        };
        if self.lan_runtime.km.running != plan {
            self.lan_runtime.km.engine = None;
            if let Some(previous) = self.lan_runtime.km.running.take() {
                self.km_input_released(&previous);
            }
        }
        if plan.is_none() {
            self.lan_runtime.km.error = error;
            self.lan_runtime.km.unconfirmed_switch = None;
        }
        if let (Some(plan), None) = (plan, self.lan_runtime.km.engine.as_ref()) {
            if now_ms() >= self.lan_runtime.km.next_start_ms {
                self.lan_runtime.km.next_start_ms = now_ms() + RETRY_MS;
                match self.km_start(&plan) {
                    Ok(started) => {
                        self.lan_runtime.km.engine = Some(started);
                        self.lan_runtime.km.running = Some(plan);
                        self.lan_runtime.km.error = None;
                    }
                    Err(failure) => {
                        self.lan_runtime.km.error = Some(failure.code);
                        self.emit_event(
                            "km.start_failed",
                            Level::Warn,
                            json!({"code": failure.code, "message": failure.message}),
                        );
                    }
                }
            }
        }
        self.km_publish();
    }

    pub(crate) fn handle_km_event(&mut self, event: Event) {
        match event {
            Event::Output(line) => {
                let Some(plan) = self.lan_runtime.km.running.clone() else {
                    return;
                };
                if plan.role != Role::Server {
                    return;
                }
                let names = (
                    ScreenName::from_device_id(&plan.server),
                    ScreenName::from_device_id(&plan.client),
                );
                let (Ok(server), Ok(client)) = names else {
                    return;
                };
                let Some(holder) = activity::switched_to(&line, &server, &client) else {
                    return;
                };
                self.km_server_switched(&plan.client, holder);
            }
            Event::Exited {
                stopped: false,
                detail,
            } => {
                if self
                    .lan_runtime
                    .km
                    .engine
                    .as_ref()
                    .is_some_and(Engine::is_running)
                {
                    return;
                }
                self.lan_runtime.km.engine = None;
                self.lan_runtime.km.error = Some("E_KM_EXITED");
                self.lan_runtime.km.next_start_ms = now_ms() + RETRY_MS;
                self.emit_event("km.exited", Level::Warn, json!({"detail": detail}));
                if let Some(previous) = self.lan_runtime.km.running.take() {
                    self.km_input_released(&previous);
                }
                self.km_publish();
            }
            Event::Exited { .. } => {}
        }
    }

    pub(crate) fn stop_km(&mut self) {
        self.lan_runtime.km.engine = None;
        self.lan_runtime.km.running = None;
        self.lan_runtime.km.unconfirmed_switch = None;
    }

    fn km_status(&mut self, granted: Permissions) -> Value {
        let local = self
            .config
            .as_ref()
            .map(|c| c.local_device.device_id.clone());
        let sharing = self.config.as_ref().and_then(|c| c.km.clone());
        let peer = self.km_peer_device();
        let peer_platform = peer
            .as_ref()
            .and_then(|id| self.lan_runtime.peer_platforms.get(id))
            .cloned();
        let state = &self.lan_runtime.km;
        let role = sharing.as_ref().map(|km| {
            if Some(&km.server_device_id) == local.as_ref() {
                "server"
            } else {
                "client"
            }
        });
        let here = match (role, state.holder) {
            (Some("client"), Holder::Client) | (Some("server"), Holder::Server) => true,
            (Some("client"), Holder::Server) | (Some("server"), Holder::Client) => false,
            _ => true,
        };
        json!({
            "enabled": sharing.as_ref().is_some_and(|km| km.enabled),
            "configured": sharing.is_some(),
            "server_device_id": sharing.as_ref().map(|km| km.server_device_id.clone()),
            "hotkey": sharing.as_ref().map(|km| km.hotkey.clone()),
            "home_hotkey": sharing.as_ref().map(|km| km.home_hotkey.clone()),
            "clipboard": sharing.as_ref().is_none_or(|km| km.clipboard),
            "swap_command_control": sharing.as_ref().is_none_or(|km| km.swap_command_control),
            "invert_scroll": sharing.as_ref().is_some_and(|km| km.invert_scroll),
            "keep_awake": sharing.as_ref().is_some_and(|km| km.keep_awake),
            "peer_platform": peer_platform,
            "role": role,
            "input_here": here,
            "peer_device_id": peer,
            "local_name": self.config.as_ref().map(|c| c.local_device.host_label.clone()),
            "peer_online": peer.as_ref().is_some_and(|id| self.lan_runtime.online.contains(id)),
            "running": state.engine.as_ref().is_some_and(Engine::is_running),
            "error": state.error,
            "permissions": granted,
        })
    }

    fn km_publish(&mut self) {
        let granted = self.km_cached_permissions();
        let status = self.km_status(granted);
        if self.lan_runtime.km.published.as_ref() != Some(&status) {
            self.lan_runtime.km.published = Some(status.clone());
            self.notify("km.changed", status);
        }
    }

    fn km_configure(&mut self, params: &Value) -> std::result::Result<(), CoreError> {
        let mut config = self
            .config
            .clone()
            .ok_or_else(|| CoreError::new(E_CONFIG_INVALID, "no local configuration"))?;
        let local = config.local_device.device_id.clone();
        let mut sharing = config.km.clone().unwrap_or(KmConfig {
            enabled: false,
            revision: 0,
            server_device_id: local,
            hotkey: DEFAULT_HOTKEY.into(),
            clipboard: true,
            home_hotkey: DEFAULT_HOME_HOTKEY.into(),
            swap_command_control: true,
            invert_scroll: false,
            keep_awake: false,
        });
        if let Some(enabled) = params.get("enabled").and_then(Value::as_bool) {
            sharing.enabled = enabled;
        }
        for (name, field) in [
            ("clipboard", &mut sharing.clipboard),
            ("swap_command_control", &mut sharing.swap_command_control),
            ("invert_scroll", &mut sharing.invert_scroll),
            ("keep_awake", &mut sharing.keep_awake),
        ] {
            if let Some(value) = params.get(name).and_then(Value::as_bool) {
                *field = value;
            }
        }
        if let Some(hotkey) = params.get("hotkey").and_then(Value::as_str) {
            sharing.hotkey = hotkey.into();
        }
        if let Some(hotkey) = params.get("home_hotkey").and_then(Value::as_str) {
            sharing.home_hotkey = hotkey.into();
        }
        if !valid_hotkeys(&sharing) {
            return Err(CoreError::new(E_CONFIG_INVALID, "invalid shortcut"));
        }
        if let Some(server) = params.get("server_device_id").and_then(Value::as_str) {
            let known = config
                .switch_group
                .as_ref()
                .is_some_and(|group| group.devices.iter().any(|d| d.device_id == server));
            if !known {
                return Err(CoreError::new(E_CONFIG_INVALID, "unknown computer"));
            }
            sharing.server_device_id = server.into();
        }
        sharing.revision += 1;
        config.km = Some(sharing);
        self.lan_save(config)
    }

    pub(crate) fn handle_km_request(&mut self, id: u64, method: &str, params: &Value) {
        match method {
            "km.get" => {
                let granted = self.km_permissions();
                let status = self.km_status(granted);
                self.respond(ResponseLine::ok(id, status));
            }
            "km.configure" => match self.km_configure(params) {
                Ok(()) => {
                    self.poll_km();
                    let granted = self.km_permissions();
                    let status = self.km_status(granted);
                    self.respond(ResponseLine::ok(id, status));
                }
                Err(error) => self.respond(ResponseLine::err(id, error.code, error.message)),
            },
            "km.release" => {
                self.km_release();
                self.poll_km();
                let granted = self.km_permissions();
                let status = self.km_status(granted);
                self.respond(ResponseLine::ok(id, status));
            }
            "km.permissions.request" => {
                let permission = params
                    .get("permission")
                    .and_then(Value::as_str)
                    .and_then(Permission::parse);
                match permission {
                    Some(permission) => {
                        permissions::request(permission);
                        self.lan_runtime.km.permissions = None;
                        let granted = self.km_permissions();
                        let status = self.km_status(granted);
                        self.respond(ResponseLine::ok(id, status));
                    }
                    None => {
                        self.respond(ResponseLine::err(
                            id,
                            E_CONFIG_INVALID,
                            "unknown permission",
                        ));
                    }
                }
            }
            _ => self.respond(ResponseLine::err(id, E_CONFIG_INVALID, "unknown method")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(revision: u64, hotkey: &str) -> KmConfig {
        KmConfig {
            enabled: true,
            revision,
            server_device_id: "dev-a".into(),
            hotkey: hotkey.into(),
            clipboard: true,
            home_hotkey: DEFAULT_HOME_HOTKEY.into(),
            swap_command_control: true,
            invert_scroll: false,
            keep_awake: false,
        }
    }

    #[test]
    fn each_side_reads_a_switch_from_its_own_position() {
        for (role, holder, arrives) in [
            (Role::Server, Holder::Client, false),
            (Role::Server, Holder::Server, true),
            (Role::Client, Holder::Client, true),
            (Role::Client, Holder::Server, false),
        ] {
            assert_eq!(
                matches!(
                    tracker_event(role, holder),
                    TrackerEvent::GroupArrived { .. }
                ),
                arrives,
                "{role:?} {holder:?}"
            );
        }
    }

    #[test]
    fn newest_settings_win_and_ties_converge() {
        let current = config(2, "Control+Alt+k");
        let newer = config(3, "Control+Alt+j");
        let older = config(1, "Control+Alt+j");
        let tied = config(2, "Control+Alt+j");
        assert!(adopts(None, &older, "dev-b", "dev-a"));
        assert!(adopts(Some(&current), &newer, "dev-b", "dev-a"));
        assert!(!adopts(Some(&current), &older, "dev-b", "dev-a"));
        assert!(!adopts(Some(&current), &current, "dev-b", "dev-a"));
        assert!(adopts(Some(&current), &tied, "dev-a", "dev-b"));
        assert!(!adopts(Some(&current), &tied, "dev-b", "dev-a"));
    }

    #[test]
    fn a_shortcut_needs_a_modifier() {
        assert!(valid_hotkey("Control+Alt+k"));
        assert!(valid_hotkey("Super+F5"));
        assert!(!valid_hotkey("k"));
        assert!(!valid_hotkey("Control+"));
        assert!(!valid_hotkey("Control+a)\n\tkeystroke(b"));
    }

    #[test]
    fn the_two_shortcuts_must_differ() {
        assert!(valid_hotkeys(&config(1, "Control+Alt+k")));
        assert!(!valid_hotkeys(&config(1, DEFAULT_HOME_HOTKEY)));
    }

    #[test]
    fn settings_saved_before_these_options_keep_their_defaults() {
        let saved = serde_json::json!({
            "enabled": true, "revision": 1, "server_device_id": "dev-a",
            "hotkey": "Control+Alt+k", "clipboard": true,
        });
        let sharing: KmConfig = serde_json::from_value(saved).unwrap();
        assert_eq!(sharing.home_hotkey, DEFAULT_HOME_HOTKEY);
        assert!(sharing.swap_command_control);
        assert!(!sharing.invert_scroll);
        assert!(!sharing.keep_awake);
    }
}
