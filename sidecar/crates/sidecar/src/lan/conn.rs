//! Transport supervisor, independent of configuration, USB/DDC and the UI.
//! Each accepted session has a blocking reader and a bounded writer channel.
//! Only authenticated hello IDs enter the connection registry.
use super::{
    address::AddressPolicy,
    discovery::{short_hash, Advertisement, DiscoveredPeer, Discovery, DiscoveryEvent},
    identity_store::Identity,
    messages::{Envelope, Hello},
    noise::{self, Role, SecureChannel},
    trust::MemberStore,
    LanError, Result,
};
use serde::Serialize;
use serde_json::{json, Value};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    collections::HashMap,
    net::{Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const DEFAULT_PORT: u16 = 47820;
const MAX_PEERS: usize = 128;
const DISCOVERY_TTL: Duration = Duration::from_secs(300);
const PEER_NOTICE_INTERVAL: Duration = Duration::from_millis(100);
const MAX_CONNECTIONS: usize = 32;
const MAX_WORKERS: usize = 32;
const TICK: Duration = Duration::from_millis(20);

#[derive(Clone, Debug)]
pub struct Timing {
    pub connect: Duration,
    pub hello: Duration,
    pub ping: Duration,
    pub idle: Duration,
    pub retry_initial: Duration,
    pub retry_max: Duration,
    /// Whole-flow budget for one pairing attempt (S6 single flight).
    pub pair: Duration,
}
impl Default for Timing {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(3),
            hello: Duration::from_secs(5),
            ping: Duration::from_secs(15),
            idle: Duration::from_secs(45),
            retry_initial: Duration::from_secs(1),
            retry_max: Duration::from_secs(30),
            pair: Duration::from_secs(120),
        }
    }
}
#[derive(Clone, Debug)]
pub struct LanOptions {
    /// Zero requests an ephemeral port (tests); normal runtime uses 47820.
    pub listen_port: u16,
    pub address_policy: AddressPolicy,
    pub timing: Timing,
}
impl Default for LanOptions {
    fn default() -> Self {
        Self {
            listen_port: DEFAULT_PORT,
            address_policy: AddressPolicy::from_env(),
            timing: Timing::default(),
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct PeerStatus {
    pub device_id: String,
    pub name: String,
    pub online: bool,
    pub addr: Option<SocketAddr>,
    /// Untrusted discovery hint for starting quarantined pairing only.
    /// It never implies membership or an authenticated online session.
    pub pair_addr: Option<SocketAddr>,
    pub last_seen_ms: u64,
    /// Initiator ID makes connection selection inspectable in diagnostics.
    pub initiator: Option<String>,
    pub app_version: Option<String>,
    pub connection_id: Option<String>,
    pub key_fp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    pub last_error: Option<String>,
    /// Discovery hint: the peer's pairing window is open. Unauthenticated;
    /// only used to choose whom to pair with.
    pub waiting: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct LocalStatus {
    pub device_id: String,
    pub key_fp: String,
    /// This machine's LAN address (IP only; no port). The secondary's
    /// waiting screen shows it so the user can find the computer. Omitted
    /// when no non-loopback interface address exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct LanStatus {
    pub enabled: bool,
    pub listen_port: u16,
    #[serde(rename = "self")]
    pub local: LocalStatus,
    pub peers: Vec<PeerStatus>,
    pub issues: Vec<String>,
}
/// Opaque identity of one authenticated Noise session, derived from its full
/// handshake hash. Fresh ephemeral keys prevent reuse across handle restarts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionGeneration([u8; 32]);
#[cfg(test)]
impl SessionGeneration {
    pub(crate) fn for_test() -> Self {
        Self([0; 32])
    }
}
struct SessionContext {
    generation: SessionGeneration,
    public_key: [u8; 32],
    stopped: Arc<AtomicBool>,
}
type SessionRegistry = Arc<Mutex<HashMap<String, SessionContext>>>;
#[derive(Clone, Debug)]
pub enum LanEvent {
    Issue {
        code: String,
        message: String,
    },
    Peers(Vec<PeerStatus>),
    PairReady(Arc<super::pair_link::PairLink>),
    Message {
        device_id: String,
        authenticated_key: [u8; 32],
        generation: SessionGeneration,
        message: Envelope,
    },
    SendFailed {
        device_id: String,
        id: Option<u64>,
        code: String,
    },
    Rejected {
        addr: SocketAddr,
        code: String,
        /// Authenticated peer key when the refusal arrived on a completed
        /// Noise handshake. `None` for local/admission failures. The
        /// coordinator verifies this against a transaction record before
        /// treating the refusal as one-sided evidence (N9).
        authenticated_key: Option<[u8; 32]>,
    },
}
impl LanEvent {
    /// Runtime can pass this straight to its notification sink.
    pub fn notification(&self) -> Option<(&'static str, Value)> {
        match self {
            Self::Peers(peers) => Some(("lan.peers", json!({"peers": peers}))),
            _ => None,
        }
    }
}
enum Command {
    Send(String, Envelope),
    Manual(SocketAddr, bool),
    Pair(SocketAddr),
    Reannounce,
    SetWaiting(bool),
    Disconnect(String),
    Shutdown,
}
/// Dropping or shutting down stops listeners, discovery and connection threads.
/// Transport is opt-in: constructing the normal Runtime does not start LAN.
pub struct LanHandle {
    commands: SyncSender<Command>,
    events: Receiver<LanEvent>,
    status: Arc<Mutex<LanStatus>>,
    join: Option<JoinHandle<()>>,
    policy: AddressPolicy,
    sessions: SessionRegistry,
    members: Arc<dyn MemberStore>,
    /// Inbound pairing admission window (S6/P2-6): set while the user is on
    /// a pairing screen. Outbound dials are always allowed.
    pairing_window: Arc<AtomicBool>,
    /// Last waiting hint handed to discovery (see `set_waiting`).
    waiting: AtomicBool,
    /// Inbound pairing source-IP cooldowns (epoch ms of last attempt).
    pair_cooldowns: Arc<Mutex<HashMap<std::net::IpAddr, u64>>>,
}
impl LanHandle {
    pub fn start(
        identity: Arc<Identity>,
        hello: Hello,
        host_label: String,
        members: Arc<dyn MemberStore>,
        mut discovery: Box<dyn Discovery>,
        options: LanOptions,
    ) -> Result<Self> {
        if hello.proto != 1 || hello.device_id.is_empty() || hello.device_id.len() > 128 {
            return Err(LanError::new("E_LAN_PROTO", "invalid local identity"));
        }
        if options.timing.ping.is_zero()
            || options.timing.idle <= options.timing.ping
            || options.timing.retry_initial.is_zero()
            || options.timing.retry_max < options.timing.retry_initial
            || options.timing.connect.is_zero()
            || options.timing.hello.is_zero()
        {
            return Err(LanError::new(
                "E_LAN_OPTIONS",
                "invalid transport deadlines",
            ));
        }
        let policy = options.address_policy;
        let (listeners, port) = bind_listeners(options.listen_port)?;
        let ad = Advertisement {
            device_id: hello.device_id.clone(),
            host_label,
            public_key: identity.public_key(),
            group_id: hello.group_id.clone(),
            port,
            waiting: false,
        };
        discovery.announce(&ad)?;
        let status = Arc::new(Mutex::new(LanStatus {
            enabled: true,
            listen_port: port,
            local: LocalStatus {
                device_id: hello.device_id.clone(),
                key_fp: short_hash(&identity.public_key()),
                addr: local_lan_ip().map(|ip| ip.to_string()),
            },
            peers: vec![],
            issues: vec![],
        }));
        let (commands, command_rx) = mpsc::sync_channel(64);
        let (event_tx, events) = mpsc::sync_channel(256);
        let retained_members = members.clone();
        // Inbound pairing is closed by default (N4): only an explicit user
        // action opens the admission window.
        let pairing_window = Arc::new(AtomicBool::new(false));
        let pair_cooldowns = Arc::new(Mutex::new(HashMap::new()));
        let mut supervisor = Supervisor::new(
            identity,
            hello,
            members,
            discovery,
            options,
            listeners,
            ad,
            status.clone(),
            event_tx,
        );
        supervisor.pairing_window = pairing_window.clone();
        supervisor.pair_cooldowns = pair_cooldowns.clone();
        let sessions = supervisor.sessions.clone();
        let join = thread::Builder::new()
            .name("lan-supervisor".into())
            .spawn(move || supervisor.run(command_rx))?;
        Ok(Self {
            commands,
            events,
            status,
            join: Some(join),
            policy,
            sessions,
            members: retained_members,
            pairing_window,
            waiting: AtomicBool::new(false),
            pair_cooldowns,
        })
    }
    /// Open or close the inbound pairing admission window (P2-6). Outbound
    /// pairings (the user pressed 连接 on this machine) are unaffected.
    /// Closing also withdraws the waiting hint; opening does not set it,
    /// because a computer that is dialing opens the window too.
    pub fn set_pairing_window(&self, open: bool) {
        self.pairing_window.store(open, Ordering::Release);
        if !open {
            self.set_waiting(false);
            self.pair_cooldowns.lock().unwrap().clear();
        }
    }
    /// Advertise that this computer is waiting for another one to pair with
    /// it (TXT `w`). Only a computer idle on its waiting screen sets this;
    /// dialing or an active pairing session clears it, so two wizards never
    /// both treat each other as the one to dial.
    pub fn set_waiting(&self, waiting: bool) {
        if self.waiting.swap(waiting, Ordering::AcqRel) != waiting {
            // Best effort: a full queue only delays the discovery hint.
            let _ = self.command(Command::SetWaiting(waiting));
        }
    }
    /// Validate a queued message immediately before executing its action. This
    /// checks live session identity, connection liveness and CURRENT membership;
    /// the periodically published UI status is never an authorization source.
    pub fn is_current_session(&self, device_id: &str, generation: SessionGeneration) -> bool {
        if self.join.as_ref().is_none_or(|join| join.is_finished()) {
            return false;
        }
        self.sessions
            .lock()
            .unwrap()
            .get(device_id)
            .is_some_and(|current| {
                current.generation == generation
                    && !current.stopped.load(Ordering::Relaxed)
                    && self.members.check(device_id, &current.public_key).is_ok()
            })
    }
    pub fn status(&self) -> LanStatus {
        self.status.lock().unwrap().clone()
    }
    /// S6 should route the runtime's lan.status request here.
    pub fn status_json(&self) -> Value {
        serde_json::to_value(self.status()).unwrap()
    }
    pub fn try_event(&self) -> Option<LanEvent> {
        self.events.try_recv().ok()
    }
    /// Success means enqueued; transport delivery failure is a SendFailed event.
    /// Application ack/timeout belongs to the runtime.
    pub fn send(&self, device_id: &str, message: Envelope) -> Result<()> {
        message.validate()?;
        // Preserve the transport's limit before crossing a queue/thread boundary.
        if serde_json::to_vec(&message)?.len() > noise::MAX_PLAINTEXT {
            return Err(LanError::new("E_LAN_FRAME", "message exceeds frame limit"));
        }
        if !self
            .status()
            .peers
            .iter()
            .any(|p| p.device_id == device_id && p.online)
        {
            return Err(LanError::new(
                "E_LAN_OFFLINE",
                "peer offline; message not queued",
            ));
        }
        self.command(Command::Send(device_id.into(), message))
    }
    pub fn pair(&self, addr: SocketAddr) -> Result<()> {
        self.policy.check(addr)?;
        self.command(Command::Pair(addr))
    }
    pub fn connect_manual(&self, addr: SocketAddr) -> Result<()> {
        self.connect_manual_with_pairing(addr, false)
    }
    pub fn connect_manual_with_pairing(&self, addr: SocketAddr, pairing: bool) -> Result<()> {
        self.policy.check(addr)?;
        self.command(Command::Manual(addr, pairing))
    }
    /// Runtime calls after network changes/resume. Auto-address monitoring in
    /// mdns-sd also updates announcements as interfaces appear/disappear.
    pub fn reannounce(&self) -> Result<()> {
        self.command(Command::Reannounce)
    }
    pub fn disconnect(&self, device_id: &str) -> Result<()> {
        self.command(Command::Disconnect(device_id.into()))
    }
    fn command(&self, command: Command) -> Result<()> {
        self.commands
            .try_send(command)
            .map_err(|_| LanError::new("E_LAN_BUSY", "LAN command queue unavailable"))
    }
    pub fn shutdown(&mut self) {
        if let Some(join) = self.join.take() {
            let _ = self.commands.send(Command::Shutdown);
            let _ = join.join();
        }
    }
}
impl Drop for LanHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn listener(addr: SocketAddr) -> std::io::Result<TcpListener> {
    let socket = Socket::new(
        if addr.is_ipv6() {
            Domain::IPV6
        } else {
            Domain::IPV4
        },
        Type::STREAM,
        Some(Protocol::TCP),
    )?;
    if addr.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    socket.bind(&addr.into())?;
    socket.listen(32)?;
    socket.set_nonblocking(true)?;
    Ok(socket.into())
}
fn bind_port(port: u16) -> std::io::Result<(Vec<TcpListener>, u16)> {
    let v4 = listener((Ipv4Addr::UNSPECIFIED, port).into())?;
    let actual = v4.local_addr()?.port();
    let v6 = listener((Ipv6Addr::UNSPECIFIED, actual).into())?;
    Ok((vec![v4, v6], actual))
}
/// Best-effort LAN address for `lan.self.addr`: first non-loopback IPv4
/// private address (the same allow-list the transport uses). IPv6 is avoided
/// because link-local addresses are not interchangeable between machines.
fn local_lan_ip() -> Option<std::net::IpAddr> {
    let policy = AddressPolicy::default();
    let mut candidates: Vec<std::net::IpAddr> = if_addrs::get_if_addrs()
        .ok()?
        .into_iter()
        .filter(|iface| !iface.is_loopback())
        .filter(|iface| {
            !["utun", "tun", "tap", "zt", "tailscale", "wg"]
                .iter()
                .any(|prefix| iface.name.to_lowercase().starts_with(prefix))
        })
        .map(|iface| iface.addr.ip())
        .filter(|ip| matches!(ip, std::net::IpAddr::V4(_)) && policy.allows(*ip))
        .collect();
    candidates.sort();
    candidates.dedup();
    candidates.into_iter().next()
}

fn bind_listeners(port: u16) -> Result<(Vec<TcpListener>, u16)> {
    match bind_port(port) {
        Ok(pair) => Ok(pair),
        Err(e) if port != 0 && e.kind() == std::io::ErrorKind::AddrInUse => {
            // Retry to avoid the unlikely race where the selected ephemeral v4
            // port is already held by a v6 listener.
            for _ in 0..8 {
                match bind_port(0) {
                    Ok(pair) => return Ok(pair),
                    Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {}
                    Err(e) => return Err(e.into()),
                }
            }
            Err(LanError::new(
                "E_LAN_LISTEN",
                "no shared IPv4/IPv6 ephemeral port available",
            ))
        }
        Err(e) => Err(e.into()),
    }
}
#[derive(Debug)]
struct Retry {
    next: Instant,
    delay: Duration,
    cursor: usize,
}
impl Retry {
    fn new(timing: &Timing) -> Self {
        Self {
            next: Instant::now(),
            delay: timing.retry_initial,
            cursor: 0,
        }
    }
    fn failed(&mut self, timing: &Timing) {
        self.next = Instant::now() + self.delay;
        self.delay = self.delay.saturating_mul(2).min(timing.retry_max);
        self.cursor += 1;
    }
    fn failed_in_rotation(&mut self, timing: &Timing, count: usize) {
        if (self.cursor + 1) % count == 0 {
            self.failed(timing);
        } else {
            self.cursor += 1;
            self.next = Instant::now();
        }
    }
    fn reset(&mut self, timing: &Timing) {
        self.next = Instant::now();
        self.delay = timing.retry_initial;
    }
}
struct Candidate {
    addresses: Vec<SocketAddr>,
    retry: Retry,
}
struct Established {
    channel: SecureChannel,
    hello: Hello,
    addr: SocketAddr,
    outbound: bool,
    /// Quarantined pairing session: the channel stays untrusted and can only
    /// carry `pair.*`/`profile.report` until both numeric confirmations.
    pairing: Option<super::pair_link::PairStart>,
}
struct AttemptResult {
    token: u64,
    addr: SocketAddr,
    result: Result<Established>,
    /// Set when an authenticated first-frame refusal ended the attempt.
    refusal_key: Option<[u8; 32]>,
}
struct AttemptWorker {
    token: u64,
    addr: SocketAddr,
    join: JoinHandle<AttemptResult>,
    cancellation: Arc<AtomicBool>,
}
enum Internal {
    Received {
        id: String,
        token: u64,
        message: Envelope,
    },
    Activity {
        id: String,
        token: u64,
    },
    Closed {
        id: String,
        token: u64,
        code: String,
    },
}
struct Live {
    token: u64,
    // Same comparison on both endpoints: preferred initiator, then shared hash.
    rank: (bool, [u8; 32]),
    remote_key: [u8; 32],
    generation: SessionGeneration,
    writer: SyncSender<Envelope>,
    stream: TcpStream,
    stop: Arc<AtomicBool>,
    reader: JoinHandle<()>,
    write_thread: JoinHandle<()>,
}
impl Live {
    fn close(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.stream.shutdown(Shutdown::Both);
        let _ = self.reader.join();
        let _ = self.write_thread.join();
    }
}
struct Supervisor {
    pair_gate: Arc<AtomicBool>,
    pair_links: Vec<Arc<super::pair_link::PairLink>>,
    pairing_window: Arc<AtomicBool>,
    pair_cooldowns: Arc<Mutex<HashMap<std::net::IpAddr, u64>>>,
    identity: Arc<Identity>,
    hello: Hello,
    members: Arc<dyn MemberStore>,
    discovery: Box<dyn Discovery>,
    options: LanOptions,
    listeners: Vec<TcpListener>,
    ad: Advertisement,
    status: Arc<Mutex<LanStatus>>,
    events: SyncSender<LanEvent>,
    internal: SyncSender<Internal>,
    rx: Receiver<Internal>,
    candidates: HashMap<String, Candidate>,
    pending: HashMap<String, u64>,
    attempts: HashMap<u64, Option<String>>,
    next_attempt: u64,
    peers: HashMap<String, PeerStatus>,
    observed: HashMap<String, Instant>,
    peers_dirty: bool,
    next_publish: Instant,
    live: HashMap<String, Live>,
    sessions: SessionRegistry,
    workers: Vec<AttemptWorker>,
    next_token: u64,
}
impl Supervisor {
    #[allow(clippy::too_many_arguments)]
    fn new(
        identity: Arc<Identity>,
        hello: Hello,
        members: Arc<dyn MemberStore>,
        discovery: Box<dyn Discovery>,
        options: LanOptions,
        listeners: Vec<TcpListener>,
        ad: Advertisement,
        status: Arc<Mutex<LanStatus>>,
        events: SyncSender<LanEvent>,
    ) -> Self {
        let (internal, rx) = mpsc::sync_channel(256);
        Self {
            pair_gate: Arc::new(AtomicBool::new(false)),
            pair_links: vec![],
            pairing_window: Arc::new(AtomicBool::new(true)),
            pair_cooldowns: Arc::new(Mutex::new(HashMap::new())),
            identity,
            hello,
            members,
            discovery,
            options,
            listeners,
            ad,
            status,
            events,
            internal,
            rx,
            candidates: HashMap::new(),
            pending: HashMap::new(),
            attempts: HashMap::new(),
            next_attempt: 0,
            peers: HashMap::new(),
            observed: HashMap::new(),
            peers_dirty: false,
            next_publish: Instant::now(),
            live: HashMap::new(),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            workers: vec![],
            next_token: 0,
        }
    }
    fn publish(&mut self) {
        self.peers_dirty = true;
    }
    /// Cheap identity of the current trusted set; MemberStore has no change
    /// notification, so poll it on each tick.
    fn member_fingerprint(&self) -> usize {
        self.members.fingerprint()
    }
    fn flush_peers(&mut self, force: bool) {
        if !self.peers_dirty || (!force && Instant::now() < self.next_publish) {
            return;
        }
        let mut peers: Vec<_> = self.peers.values().cloned().collect();
        peers.sort_by(|a, b| a.device_id.cmp(&b.device_id));
        self.status.lock().unwrap().peers = peers.clone();
        self.peers_dirty = self.events.try_send(LanEvent::Peers(peers)).is_err();
        self.next_publish = Instant::now() + PEER_NOTICE_INTERVAL;
    }
    fn forget_peer(&mut self, id: &str) {
        self.candidates.remove(id);
        self.peers.remove(id);
        self.observed.remove(id);
        self.publish();
    }
    fn removed(&mut self, id: &str) {
        self.candidates.remove(id);
        if !self.live.contains_key(id) && !self.members.contains(id) {
            self.forget_peer(id);
        }
    }
    fn reserve_peer(&mut self, id: &str) -> bool {
        if self.peers.contains_key(id) || self.peers.len() < MAX_PEERS {
            return true;
        }
        let victim = self
            .peers
            .keys()
            .filter(|id| !self.live.contains_key(*id))
            .min_by_key(|id| (self.members.contains(id), self.observed.get(*id).copied()))
            .cloned();
        if let Some(victim) = victim {
            self.forget_peer(&victim);
            true
        } else {
            false
        }
    }
    fn prune_discovery(&mut self, now: Instant) {
        let expired: Vec<_> = self
            .observed
            .iter()
            .filter(|(id, seen)| {
                !self.live.contains_key(*id)
                    && !self.members.contains(id)
                    && now.saturating_duration_since(**seen) >= DISCOVERY_TTL
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.forget_peer(&id);
        }
    }
    fn issue(&self, error: impl Into<String>) {
        let mut status = self.status.lock().unwrap();
        if status.issues.len() >= 16 {
            status.issues.remove(0);
        }
        status.issues.push(error.into());
    }
    fn run(mut self, commands: Receiver<Command>) {
        let mut member_generation = self.member_fingerprint();
        'running: loop {
            self.pair_links.retain(|p| !p.closed());
            // A changed trust set (pairing commit, relayed member, unpair)
            // re-enables previously rejected candidates: reset their backoff
            // so reconnection does not wait out a stale 30s timer.
            let generation = self.member_fingerprint();
            if generation != member_generation {
                member_generation = generation;
                for candidate in self.candidates.values_mut() {
                    candidate.retry.reset(&self.options.timing);
                }
            }
            for _ in 0..64 {
                let Ok(command) = commands.try_recv() else {
                    break;
                };
                match command {
                    Command::Shutdown => break 'running,
                    Command::Send(id, message) => {
                        let message_id = message.id;
                        let result = match self.live.get(&id) {
                            Some(live) => {
                                self.members.check(&id, &live.remote_key).and_then(|()| {
                                    live.writer.try_send(message).map_err(|_| {
                                        LanError::new("E_LAN_BACKPRESSURE", "writer full")
                                    })
                                })
                            }
                            None => Err(LanError::new(
                                "E_LAN_OFFLINE",
                                "peer went offline before delivery",
                            )),
                        };
                        if let Err(e) = result {
                            let _ = self.events.try_send(LanEvent::SendFailed {
                                device_id: id.clone(),
                                id: message_id,
                                code: e.code.into(),
                            });
                            self.close(&id, e.code);
                        }
                    }
                    Command::Pair(addr) => {
                        if self.options.address_policy.check(addr).is_ok() {
                            self.spawn_pair(addr);
                        }
                    }
                    Command::Disconnect(id) => self.close(&id, "E_LAN_DISCONNECTED"),
                    Command::Manual(addr, pairing) => {
                        if let Err(e) = self.options.address_policy.check(addr) {
                            self.issue(e.to_string());
                        } else if pairing {
                            self.spawn_pair(addr);
                        } else {
                            self.upsert(DiscoveredPeer {
                                device_id: format!("@{addr}"),
                                name: addr.to_string(),
                                addresses: vec![addr],
                                key_fp: String::new(),
                                group_fp: String::new(),
                                waiting: false,
                            });
                        }
                    }
                    Command::SetWaiting(waiting) => {
                        self.ad.waiting = waiting;
                        if let Err(e) = self.discovery.announce(&self.ad) {
                            self.issue(e.to_string());
                        }
                    }
                    Command::Reannounce => {
                        if let Err(e) = self.discovery.announce(&self.ad) {
                            self.issue(e.to_string());
                        }
                        for c in self.candidates.values_mut() {
                            c.retry.reset(&self.options.timing);
                        }
                    }
                }
            }
            for _ in 0..256 {
                let Ok(event) = self.rx.try_recv() else {
                    break;
                };
                match event {
                    Internal::Received { id, token, message } => {
                        if self.live.get(&id).is_some_and(|live| live.token == token) {
                            if let Some(error) = self
                                .live
                                .get(&id)
                                .and_then(|live| self.members.check(&id, &live.remote_key).err())
                            {
                                self.close(&id, error.code);
                                continue;
                            }
                            if let Some(peer) = self.peers.get_mut(&id) {
                                peer.last_seen_ms = now_ms();
                            }
                            // Recheck membership for every incoming frame; unpair/key
                            // replacement cannot keep an old authenticated session alive.
                            if self
                                .events
                                .try_send(LanEvent::Message {
                                    device_id: id.clone(),
                                    authenticated_key: self.live[&id].remote_key,
                                    generation: self.live[&id].generation,
                                    message,
                                })
                                .is_err()
                            {
                                self.close(&id, "E_LAN_BACKPRESSURE");
                            }
                        }
                    }
                    Internal::Activity { id, token } => {
                        if self.live.get(&id).is_some_and(|live| live.token == token) {
                            if let Some(peer) = self.peers.get_mut(&id) {
                                peer.last_seen_ms = now_ms();
                            }
                            self.publish();
                        }
                    }
                    Internal::Closed { id, token, code } => {
                        if self.live.get(&id).is_some_and(|live| live.token == token) {
                            self.close(&id, &code);
                        }
                    }
                }
            }
            match self.discovery.poll() {
                Ok(events) => {
                    for event in events {
                        match event {
                            DiscoveryEvent::Upsert(peer) => self.upsert(peer),
                            DiscoveryEvent::Removed(id) => {
                                self.removed(&id);
                            }
                        }
                    }
                }
                Err(e) => self.issue(e.to_string()),
            }
            let mut accepted = vec![];
            for listener in &self.listeners {
                for _ in 0..8 {
                    match listener.accept() {
                        Ok((stream, addr)) => accepted.push((stream, addr)),
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(e) => {
                            self.issue(e.to_string());
                            break;
                        }
                    }
                }
            }
            for (stream, addr) in accepted {
                if let Err(e) = self.options.address_policy.check(addr) {
                    let _ = self.events.try_send(LanEvent::Rejected {
                        addr,
                        code: e.code.into(),
                        authenticated_key: None,
                    });
                    continue;
                }
                if self.workers.len() < MAX_WORKERS {
                    self.spawn(Some(stream), addr, None);
                }
            }
            // Both devices dial even if an inbound connection exists. Once the
            // preferred-direction session exists, stop all redundant attempts.
            let due: Vec<_> = self
                .candidates
                .iter()
                .filter(|(id, c)| {
                    c.retry.next <= Instant::now()
                        && !self.pending.contains_key(*id)
                        && !self.live.get(*id).is_some_and(|live| {
                            !live.rank.0 || self.hello.device_id.as_str() > id.as_str()
                        })
                })
                .map(|(id, c)| (id.clone(), c.addresses[c.retry.cursor % c.addresses.len()]))
                .collect();
            for (id, addr) in due {
                if self.workers.len() >= MAX_WORKERS {
                    break;
                }
                self.spawn(None, addr, Some(id));
            }
            let stopped: Vec<_> = self
                .live
                .iter()
                .filter(|(_, live)| live.stop.load(Ordering::Relaxed))
                .map(|(id, _)| id.clone())
                .collect();
            for id in stopped {
                self.close(&id, "E_LAN_CLOSED");
            }
            for result in self.reap_workers() {
                self.complete_attempt(result);
            }
            let revoked: Vec<_> = self
                .live
                .iter()
                .filter_map(|(id, live)| {
                    self.members
                        .check(id, &live.remote_key)
                        .err()
                        .map(|e| (id.clone(), e.code))
                })
                .collect();
            for (id, code) in revoked {
                self.close(&id, code);
            }
            self.prune_discovery(Instant::now());
            self.flush_peers(false);
            thread::sleep(TICK);
        }
        self.flush_peers(true);
        for link in self.pair_links.drain(..) {
            link.close();
        }
        let _ = self.discovery.stop();
        self.listeners.clear();
        self.sessions.lock().unwrap().clear();
        for (_, live) in self.live.drain() {
            live.close();
        }
        // Cancel every handshake/transcript before joining any of them.
        for worker in &self.workers {
            worker.cancellation.store(true, Ordering::Release);
        }
        for worker in self.workers {
            let _ = worker.join.join();
        }
        let mut status = self.status.lock().unwrap();
        status.enabled = false;
        for peer in &mut status.peers {
            peer.online = false;
            peer.initiator = None;
            peer.connection_id = None;
        }
    }
    fn begin_attempt(&mut self, candidate: Option<String>) -> u64 {
        self.next_attempt += 1;
        let token = self.next_attempt;
        if let Some(id) = &candidate {
            self.pending.insert(id.clone(), token);
        }
        self.attempts.insert(token, candidate);
        token
    }
    // Joining produces results; only complete_attempt consumes logical state.
    fn reap_workers(&mut self) -> Vec<AttemptResult> {
        let mut results = vec![];
        let mut index = 0;
        while index < self.workers.len() {
            if !self.workers[index].join.is_finished() {
                index += 1;
                continue;
            }
            let worker = self.workers.swap_remove(index);
            results.push(worker.join.join().unwrap_or_else(|_| AttemptResult {
                token: worker.token,
                addr: worker.addr,
                result: Err(LanError::new("E_LAN_WORKER", "connection worker panicked")),
                refusal_key: None,
            }));
        }
        results
    }
    fn complete_attempt(&mut self, result: AttemptResult) {
        let Some(candidate) = self.attempts.remove(&result.token) else {
            return;
        };
        if let Some(id) = &candidate {
            if self.pending.get(id) != Some(&result.token) {
                return;
            }
            self.pending.remove(id);
        }
        match result.result {
            Ok(established) => {
                if let Some(id) = candidate {
                    if let Some(candidate) = self.candidates.get_mut(&id) {
                        candidate.retry.reset(&self.options.timing);
                    }
                }
                self.established(established);
            }
            Err(error) => {
                if let Some(id) = candidate {
                    if let Some(candidate) = self.candidates.get_mut(&id) {
                        candidate
                            .retry
                            .failed_in_rotation(&self.options.timing, candidate.addresses.len());
                    }
                    if let Some(peer) = self.peers.get_mut(&id) {
                        peer.last_error = Some(error.code.into());
                        peer.pair_addr = if error.code == "E_LAN_NOT_MEMBER" {
                            Some(result.addr)
                        } else {
                            self.candidates
                                .get(&id)
                                .map(|c| c.addresses[c.retry.cursor % c.addresses.len()])
                        };
                    }
                }
                let _ = self.events.try_send(LanEvent::Rejected {
                    addr: result.addr,
                    code: error.code.into(),
                    authenticated_key: result.refusal_key,
                });
                self.publish();
            }
        }
    }
    fn upsert(&mut self, mut peer: DiscoveredPeer) {
        if peer.device_id == self.hello.device_id
            && !peer.key_fp.is_empty()
            && peer.key_fp != short_hash(&self.identity.public_key())
        {
            let code = "E_LAN_ID_CONFLICT";
            let mut status = self.status.lock().unwrap();
            if !status.issues.iter().any(|issue| issue == code) {
                status.issues.push(code.into());
                let _ = self.events.try_send(LanEvent::Issue {
                    code: code.into(), message: "Another computer advertises this device ID with a different key. Pair the computers again.".into(),
                });
            }
            return;
        }
        if peer.device_id.is_empty()
            || peer.device_id == self.hello.device_id
            || peer.device_id.len() > 128
        {
            return;
        }
        peer.addresses
            .retain(|addr| self.options.address_policy.check(*addr).is_ok());
        // IPv6 link-local targets are not dialable without a scope the peer
        // understands (and macOS fe80::1 is the router): keep them out of
        // the candidate rotation entirely — dialing them only produces
        // E_LAN_IO storms that starve the working IPv4 address.
        peer.addresses
            .retain(|addr| !matches!(addr.ip(), std::net::IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80));
        peer.addresses.sort();
        peer.addresses.dedup();
        peer.addresses.truncate(16);
        if peer.addresses.is_empty() || !self.reserve_peer(&peer.device_id) {
            return;
        }
        let retry = self
            .candidates
            .entry(peer.device_id.clone())
            .or_insert_with(|| Candidate {
                addresses: vec![],
                retry: Retry::new(&self.options.timing),
            });
        if retry.addresses != peer.addresses {
            retry.addresses = peer.addresses;
            retry.retry.reset(&self.options.timing);
        }
        let pair_addr = retry.addresses[retry.retry.cursor % retry.addresses.len()];
        let id = peer.device_id.clone();
        self.observed.insert(id.clone(), Instant::now());
        self.peers
            .entry(id.clone())
            .and_modify(|p| {
                p.waiting = peer.waiting;
                if !p.online {
                    p.name = peer.name.clone();
                    p.pair_addr = Some(pair_addr);
                }
            })
            .or_insert(PeerStatus {
                device_id: id,
                name: peer.name,
                online: false,
                addr: None,
                pair_addr: Some(pair_addr),
                last_seen_ms: 0,
                initiator: None,
                app_version: None,
                connection_id: None,
                key_fp: peer.key_fp,
                platform: None,
                last_error: None,
                waiting: peer.waiting,
            });
        self.publish();
    }
    fn spawn_pair(&mut self, addr: SocketAddr) {
        self.spawn_inner(None, addr, None, true)
    }
    fn spawn(&mut self, stream: Option<TcpStream>, addr: SocketAddr, candidate: Option<String>) {
        self.spawn_inner(stream, addr, candidate, false)
    }
    fn spawn_inner(
        &mut self,
        stream: Option<TcpStream>,
        addr: SocketAddr,
        candidate: Option<String>,
        pairing: bool,
    ) {
        let identity = self.identity.clone();
        let mut hello = self.hello.clone();
        if pairing {
            hello.caps.push("pair.request.v1".into());
        }
        let gate = self.pair_gate.clone();
        let members = self.members.clone();
        let timing = self.options.timing.clone();
        // Inbound pairing admission (P2-6), checked inside the worker so the
        // spawn signature stays uniform: only while the user is on a pairing
        // screen, and at most one attempt per source IP per 30 s — a hostile
        // peer must not be able to spam the SAS prompt and pin the
        // single-flight gate for its whole 120 s window.
        let pairing_window = self.pairing_window.clone();
        let pair_cooldowns = self.pair_cooldowns.clone();
        let token = self.begin_attempt(candidate.clone());
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancellation = cancellation.clone();
        self.workers.push(AttemptWorker {
            token,
            addr,
            cancellation,
            join: thread::spawn(move || {
                let outbound = stream.is_none();
                let mut refusal_key = None;
                let result = (|| -> Result<Established> {
                    let stream = match stream {
                        Some(s) => s,
                        None => TcpStream::connect_timeout(&addr, timing.connect)?,
                    };
                    let role = if outbound {
                        Role::Initiator
                    } else {
                        Role::Responder
                    };
                    let mut channel = SecureChannel::handshake_cancellable(
                        stream,
                        &identity,
                        role,
                        Some(worker_cancellation),
                    )?;
                    // Inbound attempts learn the pairing intent from the
                    // dialer's hello. Admission is checked BEFORE sending our
                    // own hello so a refusal is the dialer's first frame (an
                    // authenticated ack with E_LAN_PAIR_WINDOW); sending hello
                    // first turned refusals into ambiguous connection
                    // failures. Outbound dialers keep sending first.
                    let check_inbound = || -> Result<()> {
                        let now = now_ms();
                        let within_cooldown = {
                            let mut cooldowns = pair_cooldowns.lock().unwrap();
                            cooldowns.retain(|_, last| now.saturating_sub(*last) < 60_000);
                            let global = cooldowns.len();
                            let blocked = cooldowns
                                .get(&addr.ip())
                                .is_some_and(|last| now.saturating_sub(*last) < 30_000);
                            if !blocked && global < 5 {
                                cooldowns.insert(addr.ip(), now);
                            }
                            blocked || global >= 5
                        };
                        if !pairing_window.load(Ordering::Acquire) {
                            return Err(LanError::new(
                                "E_LAN_PAIR_WINDOW",
                                "pairing is not accepting connections right now",
                            ));
                        }
                        if within_cooldown {
                            return Err(LanError::new(
                                "E_LAN_BUSY",
                                "pairing attempt rate limit reached",
                            ));
                        }
                        Ok(())
                    };
                    let mut inbound_admission_checked = false;
                    let first = if outbound {
                        channel.send(&hello.envelope()?)?;
                        channel.receive(timing.hello.clone())?
                    } else {
                        let first = channel.receive(timing.hello.clone())?;
                        if first.t == "hello" {
                            if let Ok(remote) = Hello::parse(first.clone()) {
                                if remote.caps.iter().any(|c| c == "pair.request.v1") {
                                    if let Err(error) = check_inbound() {
                                        // Deliver the real refusal code before
                                        // closing (N4/N7).
                                        reject(&mut channel, &error);
                                        return Err(error);
                                    }
                                    inbound_admission_checked = true;
                                }
                            }
                        }
                        channel.send(&hello.envelope()?)?;
                        first
                    };
                    // A membership refusal arrives as an ok=false ack
                    // carrying the REFUSING side's code (e.g.
                    // E_LAN_NOT_MEMBER): surface that code instead of a
                    // generic parse error, so the dialer's coordinator can
                    // act on the actual reason (one-sided pairing undo).
                    if first.t == "ack" && first.ok == Some(false) {
                        // Map the refusing side's code back to our static
                        // catalog so the dialer sees the real reason.
                        let code = first.body.get("code").and_then(Value::as_str);
                        let known = match code {
                            Some("E_LAN_NOT_MEMBER") => Some("E_LAN_NOT_MEMBER"),
                            Some("E_LAN_KEY_CHANGED") => Some("E_LAN_KEY_CHANGED"),
                            Some("E_LAN_PAIR_WINDOW") => Some("E_LAN_PAIR_WINDOW"),
                            _ => None,
                        };
                        if let Some(static_code) = known {
                            // This ack arrived inside the authenticated Noise
                            // channel, so the key is the refusing peer's real
                            // identity. The coordinator only turns a
                            // NOT_MEMBER refusal into one-sided evidence when
                            // it matches the transaction record's pinned key.
                            refusal_key = Some(channel.remote_key());
                            return Err(LanError::new(static_code, "peer refused the session"));
                        }
                    }
                    let remote = match Hello::parse(first) {
                        Ok(remote) => remote,
                        Err(error) => {
                            reject(&mut channel, &error);
                            return Err(error);
                        }
                    };
                    if remote.device_id == hello.device_id
                        || candidate
                            .as_ref()
                            .is_some_and(|id| !id.starts_with('@') && id != &remote.device_id)
                    {
                        return Err(LanError::new("E_LAN_IDENTITY", "pair target mismatch"));
                    }
                    if pairing || remote.caps.iter().any(|c| c == "pair.request.v1") {
                        if outbound != pairing {
                            return Err(LanError::new("E_LAN_PROTO", "invalid pairing initiator"));
                        }
                        // Inbound admission was checked before our hello was
                        // sent (so the refusal reaches the dialer as an ack).
                        if !pairing && !inbound_admission_checked {
                            if let Err(error) = check_inbound() {
                                reject(&mut channel, &error);
                                return Err(error);
                            }
                        }
                        gate.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                            .map_err(|_| {
                                LanError::new("E_LAN_BUSY", "another pairing is active")
                            })?;
                        let sas = channel.compare_code(role)?;
                        let start = super::pair_link::PairStart {
                            permit: super::pair_link::Permit(gate),
                            deadline: Instant::now() + timing.pair,
                            sas,
                        };
                        return Ok(Established {
                            channel,
                            hello: remote,
                            addr,
                            outbound,
                            pairing: Some(start),
                        });
                    }
                    let check = members
                        .check(&remote.device_id, &channel.remote_key())
                        .and_then(|()| {
                            if remote.device_id == hello.device_id
                                || candidate.as_ref().is_some_and(|id| {
                                    !id.starts_with('@') && id != &remote.device_id
                                })
                            {
                                Err(LanError::new(
                                    "E_LAN_IDENTITY",
                                    "hello identity does not match dial target",
                                ))
                            } else {
                                Ok(())
                            }
                        });
                    if let Err(e) = check {
                        reject(&mut channel, &e);
                        return Err(e);
                    }
                    Ok(Established {
                        channel,
                        hello: remote,
                        addr,
                        outbound,
                        pairing: None,
                    })
                })();
                AttemptResult {
                    token,
                    addr,
                    result,
                    refusal_key,
                }
            }),
        });
    }
    fn established(&mut self, established: Established) {
        let Established {
            channel,
            hello,
            addr,
            outbound,
            pairing,
        } = established;
        if let Some(start) = pairing {
            // Remember the peer's address as a manual candidate so the normal
            // connection layer can re-dial it after pairing commits keys.
            // IPv6 link-local sources are skipped: their scope ids do not
            // survive as dial targets (and macOS fe80::1 is the router), so
            // redialing them only produces E_LAN_IO loops. The peer's real
            // addresses arrive through discovery or its own dial.
            let dialable = !(addr.is_ipv6()
                && matches!(addr.ip(), std::net::IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80));
            if dialable {
                self.upsert(super::discovery::DiscoveredPeer {
                    device_id: hello.device_id.clone(),
                    name: hello.device_id.clone(),
                    addresses: vec![addr],
                    key_fp: String::new(),
                    group_fp: String::new(),
                    waiting: false,
                });
            }
            match super::pair_link::PairLink::start(
                channel,
                hello.device_id,
                hello.name,
                outbound,
                start,
            ) {
                Ok(link) => {
                    self.pair_links.retain(|p| !p.closed());
                    if self
                        .events
                        .try_send(LanEvent::PairReady(link.clone()))
                        .is_ok()
                    {
                        self.pair_links.push(link);
                    } else {
                        link.close();
                    }
                }
                Err(error) => self.issue(error.to_string()),
            }
            return;
        }
        let id = hello.device_id.clone();
        if self.members.check(&id, &channel.remote_key()).is_err() {
            channel.close();
            return;
        }
        if let Some(manual) = self.candidates.remove(&format!("@{addr}")) {
            self.peers.remove(&format!("@{addr}"));
            self.observed.remove(&format!("@{addr}"));
            self.candidates.entry(id.clone()).or_insert(manual);
        }
        let initiator = if outbound {
            self.hello.device_id.clone()
        } else {
            id.clone()
        };
        let preferred = self.hello.device_id.as_str().min(id.as_str());
        let rank = (initiator != preferred, channel.handshake_hash());
        if self.live.get(&id).is_some_and(|old| {
            old.rank <= rank
                && !old.stop.load(Ordering::Relaxed)
                && self.members.check(&id, &old.remote_key).is_ok()
        }) {
            channel.close();
            return;
        }
        if let Some(old) = self.live.remove(&id) {
            self.sessions.lock().unwrap().remove(&id);
            old.close();
        }
        if self.live.len() >= MAX_CONNECTIONS {
            channel.close();
            return;
        }
        if !self.reserve_peer(&id) {
            channel.close();
            return;
        }
        self.observed.insert(id.clone(), Instant::now());
        self.next_token += 1;
        let token = self.next_token;
        let key = channel.remote_key();
        let generation = SessionGeneration(channel.handshake_hash());
        let stream = match channel.stream.try_clone() {
            Ok(s) => s,
            Err(_) => {
                channel.close();
                return;
            }
        };
        let writer_stream = match channel.stream.try_clone() {
            Ok(s) => s,
            Err(_) => {
                channel.close();
                return;
            }
        };
        let (writer, writer_rx) = mpsc::sync_channel(64);
        let stop = Arc::new(AtomicBool::new(false));
        let transport = channel.transport.clone();
        let reader = spawn_reader(
            channel,
            id.clone(),
            token,
            self.members.clone(),
            self.internal.clone(),
            writer.clone(),
            stop.clone(),
            self.options.timing.idle,
        );
        let write_thread = spawn_writer(
            writer_stream,
            transport,
            writer_rx,
            id.clone(),
            token,
            self.internal.clone(),
            stop.clone(),
            self.options.timing.ping,
            self.members.clone(),
            key,
        );
        self.sessions.lock().unwrap().insert(
            id.clone(),
            SessionContext {
                generation,
                public_key: key,
                stopped: stop.clone(),
            },
        );
        self.live.insert(
            id.clone(),
            Live {
                token,
                rank,
                remote_key: key,
                generation,
                writer,
                stream,
                stop,
                reader,
                write_thread,
            },
        );
        self.peers.entry(id.clone()).or_insert(PeerStatus {
            device_id: id.clone(),
            name: id.clone(),
            online: false,
            addr: None,
            pair_addr: None,
            last_seen_ms: 0,
            initiator: None,
            app_version: None,
            connection_id: None,
            key_fp: String::new(),
            platform: None,
            last_error: None,
            waiting: false,
        });
        let peer = self.peers.get_mut(&id).unwrap();
        peer.online = true;
        peer.addr = Some(addr);
        peer.last_seen_ms = now_ms();
        peer.initiator = Some(initiator);
        peer.app_version = Some(hello.app_version);
        peer.platform = hello.platform.clone();
        peer.key_fp = short_hash(&key);
        peer.connection_id = Some(short_hash(&rank.1));
        peer.last_error = None;
        self.publish();
    }
    fn close(&mut self, id: &str, code: &str) {
        self.sessions.lock().unwrap().remove(id);
        if let Some(live) = self.live.remove(id) {
            live.close();
        }
        if let Some(peer) = self.peers.get_mut(id) {
            peer.online = false;
            peer.initiator = None;
            peer.connection_id = None;
            peer.last_error = Some(code.into());
        }
        if let Some(candidate) = self.candidates.get_mut(id) {
            candidate.retry.reset(&self.options.timing);
            candidate
                .retry
                .failed_in_rotation(&self.options.timing, candidate.addresses.len());
        }
        self.publish();
    }
}
fn reject(channel: &mut SecureChannel, error: &LanError) {
    let mut reply = Envelope::new("ack", json!({"code": error.code, "message": error.message}));
    reply.ok = Some(false);
    let _ = channel.send(&reply);
    channel.close();
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[allow(clippy::too_many_arguments)]
fn spawn_reader(
    mut channel: SecureChannel,
    id: String,
    token: u64,
    members: Arc<dyn MemberStore>,
    tx: SyncSender<Internal>,
    writer: SyncSender<Envelope>,
    stop: Arc<AtomicBool>,
    idle: Duration,
) -> JoinHandle<()> {
    channel.set_cancellation(stop.clone());
    thread::spawn(move || {
        let result = (|| -> Result<()> {
            while !stop.load(Ordering::Relaxed) {
                let message = channel.receive(idle)?;
                members.check(&id, &channel.remote_key())?;
                match message.t.as_str() {
                    "ping" => {
                        writer
                            .try_send(Envelope::new("pong", json!({})))
                            .map_err(|_| LanError::new("E_LAN_BACKPRESSURE", "writer full"))?;
                    }
                    "pong" => {}
                    "hello" | "pair.hello_pair" => {
                        return Err(LanError::new(
                            "E_LAN_PROTO",
                            "identity cannot change on a live session",
                        ))
                    }
                    _ => {
                        tx.try_send(Internal::Received {
                            id: id.clone(),
                            token,
                            message,
                        })
                        .map_err(|_| {
                            LanError::new("E_LAN_BACKPRESSURE", "runtime event queue full")
                        })?;
                        continue;
                    }
                }
                tx.try_send(Internal::Activity {
                    id: id.clone(),
                    token,
                })
                .map_err(|_| LanError::new("E_LAN_BACKPRESSURE", "runtime event queue full"))?;
            }
            Ok(())
        })();
        stop.store(true, Ordering::Relaxed);
        channel.close();
        let code = result.err().map(|e| e.code).unwrap_or("E_LAN_CLOSED");
        let _ = tx.try_send(Internal::Closed {
            id,
            token,
            code: code.into(),
        });
    })
}
#[allow(clippy::too_many_arguments)]
fn spawn_writer(
    mut stream: TcpStream,
    transport: Arc<Mutex<snow::TransportState>>,
    rx: Receiver<Envelope>,
    id: String,
    token: u64,
    tx: SyncSender<Internal>,
    stop: Arc<AtomicBool>,
    ping: Duration,
    members: Arc<dyn MemberStore>,
    remote_key: [u8; 32],
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut next_ping = Instant::now() + ping;
        let result = (|| -> Result<()> {
            while !stop.load(Ordering::Relaxed) {
                members.check(&id, &remote_key)?;
                // Outbound traffic cannot extend the INBOUND liveness deadline.
                if Instant::now() >= next_ping {
                    noise::send_message(
                        &mut stream,
                        &transport,
                        &Envelope::new("ping", json!({})),
                    )?;
                    next_ping = Instant::now() + ping;
                }
                match rx.recv_timeout(TICK.min(next_ping.saturating_duration_since(Instant::now())))
                {
                    Ok(message) => {
                        // Queued-before-revocation is not authorization to write.
                        members.check(&id, &remote_key)?;
                        noise::send_message(&mut stream, &transport, &message)?;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            Ok(())
        })();
        stop.store(true, Ordering::Relaxed);
        let _ = stream.shutdown(Shutdown::Both);
        let code = result.err().map(|e| e.code).unwrap_or("E_LAN_CLOSED");
        let _ = tx.try_send(Internal::Closed {
            id,
            token,
            code: code.into(),
        });
    })
}

#[cfg(test)]
mod tests {
    use super::super::{discovery::StaticDiscovery, trust::MemoryMembers};
    use super::*;
    use std::sync::{Barrier, Once};
    fn options() -> LanOptions {
        static LOOPBACK: Once = Once::new();
        LOOPBACK.call_once(|| std::env::set_var("KVMFLOW_LAN_ALLOW_LOOPBACK", "1"));
        LanOptions {
            listen_port: 0,
            address_policy: AddressPolicy::from_env(),
            timing: Timing {
                ping: Duration::from_millis(80),
                idle: Duration::from_millis(400),
                retry_initial: Duration::from_millis(50),
                retry_max: Duration::from_millis(200),
                ..Timing::default()
            },
        }
    }
    fn addr(handle: &LanHandle) -> SocketAddr {
        (Ipv4Addr::LOCALHOST, handle.status().listen_port).into()
    }
    fn peer(id: &str, addr: SocketAddr) -> DiscoveredPeer {
        DiscoveredPeer {
            device_id: id.into(),
            name: id.into(),
            addresses: vec![addr],
            key_fp: String::new(),
            group_fp: String::new(),
            waiting: false,
        }
    }
    fn wait(timeout: Duration, mut condition: impl FnMut() -> bool) {
        let end = Instant::now() + timeout;
        while !condition() {
            assert!(Instant::now() < end, "condition timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn online(handle: &LanHandle) -> bool {
        handle.status().peers.iter().any(|p| p.online)
    }
    struct GatedDiscovery {
        inner: StaticDiscovery,
        barrier: Arc<Barrier>,
        passed: bool,
    }
    impl Discovery for GatedDiscovery {
        fn announce(&mut self, ad: &Advertisement) -> Result<()> {
            self.inner.announce(ad)
        }
        fn poll(&mut self) -> Result<Vec<DiscoveryEvent>> {
            let events = self.inner.poll()?;
            if !events.is_empty() && !self.passed {
                self.passed = true;
                self.barrier.wait();
            }
            Ok(events)
        }
        fn stop(&mut self) -> Result<()> {
            self.inner.stop()
        }
    }
    #[test]
    fn simultaneous_dials_converge_ping_and_reconnect() {
        let a = Arc::new(Identity::generate().unwrap());
        let b = Arc::new(Identity::generate().unwrap());
        let ma = Arc::new(MemoryMembers::default());
        let mb = Arc::new(MemoryMembers::default());
        ma.insert("b", b.public_key());
        mb.insert("a", a.public_key());
        let da = StaticDiscovery::default();
        let db = StaticDiscovery::default();
        let gate = Arc::new(Barrier::new(2));
        let mut ha = LanHandle::start(
            a.clone(),
            Hello::new("a"),
            "Mac".into(),
            ma,
            Box::new(GatedDiscovery {
                inner: da.clone(),
                barrier: gate.clone(),
                passed: false,
            }),
            options(),
        )
        .unwrap();
        let mut hb = LanHandle::start(
            b.clone(),
            Hello::new("b"),
            "PC".into(),
            mb.clone(),
            Box::new(GatedDiscovery {
                inner: db.clone(),
                barrier: gate,
                passed: false,
            }),
            options(),
        )
        .unwrap();
        da.upsert(peer("b", addr(&hb)));
        db.upsert(peer("a", addr(&ha)));
        wait(Duration::from_secs(5), || {
            let sa = ha.status();
            let sb = hb.status();
            match (sa.peers.first(), sb.peers.first()) {
                (Some(a), Some(b)) => {
                    a.online
                        && b.online
                        && a.initiator.as_deref() == Some("a")
                        && b.initiator.as_deref() == Some("a")
                        && a.connection_id == b.connection_id
                }
                _ => false,
            }
        });
        // Without ping/pong, the 400ms idle deadline would close both sockets.
        thread::sleep(Duration::from_millis(650));
        assert!(online(&ha) && online(&hb));
        assert_eq!(ha.status().peers.len(), 1);
        assert_eq!(hb.status().peers.len(), 1);
        ha.send("b", Envelope::new("test.payload", json!({"n":1})))
            .unwrap();
        wait(Duration::from_secs(2), || {
            while let Some(event) = hb.try_event() {
                if let LanEvent::Message {
                    device_id, message, ..
                } = event
                {
                    if device_id == "a" && message.t == "test.payload" {
                        return true;
                    }
                }
            }
            false
        });
        let old_port = hb.status().listen_port;
        hb.shutdown();
        wait(Duration::from_secs(2), || !online(&ha));
        assert_eq!(
            ha.send("b", Envelope::new("test", json!({})))
                .unwrap_err()
                .code,
            "E_LAN_OFFLINE"
        );
        let replacement_discovery = StaticDiscovery::new(vec![peer("a", addr(&ha))]);
        let mut replacement = LanHandle::start(
            b,
            Hello::new("b"),
            "PC".into(),
            mb,
            Box::new(replacement_discovery),
            options(),
        )
        .unwrap();
        da.upsert(peer("b", addr(&replacement)));
        wait(Duration::from_secs(5), || {
            online(&ha) && online(&replacement)
        });
        assert_ne!(old_port, 0);
        assert_eq!(ha.status_json()["self"]["device_id"], "a");
        assert_eq!(da.advertisement().unwrap().port, ha.status().listen_port);
        let mut saw_notification = false;
        while let Some(event) = ha.try_event() {
            if event
                .notification()
                .is_some_and(|(kind, _)| kind == "lan.peers")
            {
                saw_notification = true;
            }
        }
        assert!(saw_notification);
        ha.shutdown();
        replacement.shutdown();
        assert!(!ha.status().enabled);
        assert!(da.advertisement().is_none());
    }
    #[test]
    fn pairing_window_is_advertised_and_peer_waiting_hint_is_reported() {
        let discovery = StaticDiscovery::default();
        let mut handle = LanHandle::start(
            Arc::new(Identity::generate().unwrap()),
            Hello::new("a"),
            "PC".into(),
            Arc::new(MemoryMembers::default()),
            Box::new(discovery.clone()),
            options(),
        )
        .unwrap();
        assert!(!discovery.advertisement().unwrap().waiting);
        // Opening admission alone (a dialing computer does this) is no hint.
        handle.set_pairing_window(true);
        thread::sleep(Duration::from_millis(200));
        assert!(!discovery.advertisement().unwrap().waiting);
        handle.set_waiting(true);
        wait(Duration::from_secs(2), || {
            discovery.advertisement().unwrap().waiting
        });
        handle.set_pairing_window(false);
        wait(Duration::from_secs(2), || {
            !discovery.advertisement().unwrap().waiting
        });
        let mut waiting = peer("b", "192.168.1.9:47820".parse().unwrap());
        waiting.waiting = true;
        discovery.upsert(waiting.clone());
        wait(Duration::from_secs(2), || {
            handle
                .status()
                .peers
                .iter()
                .any(|p| p.device_id == "b" && p.waiting)
        });
        assert_eq!(handle.status_json()["peers"][0]["waiting"], json!(true));
        waiting.waiting = false;
        discovery.upsert(waiting);
        wait(Duration::from_secs(2), || {
            handle
                .status()
                .peers
                .iter()
                .any(|p| p.device_id == "b" && !p.waiting)
        });
        handle.shutdown();
    }
    #[test]
    fn default_port_and_busy_port_fallback_both_address_families() {
        assert_eq!(LanOptions::default().listen_port, 47820);
        let occupied = TcpListener::bind("0.0.0.0:0").unwrap();
        let port = occupied.local_addr().unwrap().port();
        let (listeners, actual) = bind_listeners(port).unwrap();
        assert_ne!(actual, port);
        assert_ne!(actual, 0);
        assert_eq!(listeners.len(), 2);
        assert!(listeners
            .iter()
            .all(|l| l.local_addr().unwrap().port() == actual));
        TcpStream::connect((Ipv4Addr::LOCALHOST, actual)).unwrap();
        TcpStream::connect((Ipv6Addr::LOCALHOST, actual)).unwrap();
    }
    #[test]
    fn retry_schedule_and_announcement_reset() {
        let t = Timing::default();
        let mut retry = Retry::new(&t);
        for secs in [1, 2, 4, 8, 16, 30, 30] {
            let before = Instant::now();
            retry.failed(&t);
            assert!(retry.next >= before + Duration::from_secs(secs));
            assert!(retry.next <= Instant::now() + Duration::from_secs(secs));
        }
        retry.reset(&t);
        assert!(retry.next <= Instant::now());
        assert_eq!(retry.delay, Duration::from_secs(1));
        assert_eq!(t.connect, Duration::from_secs(3));
        assert_eq!(t.hello, Duration::from_secs(5));
        assert_eq!(t.ping, Duration::from_secs(15));
        assert_eq!(t.idle, Duration::from_secs(45));
    }
    #[test]
    fn silent_authenticated_peer_goes_offline_at_45_seconds() {
        let local = Arc::new(Identity::generate().unwrap());
        let remote = Identity::generate().unwrap();
        let members = Arc::new(MemoryMembers::default());
        members.insert("silent", remote.public_key());
        let mut opts = options();
        opts.timing = Timing::default();
        let mut handle = LanHandle::start(
            local,
            Hello::new("local"),
            "Mac".into(),
            members,
            Box::new(StaticDiscovery::default()),
            opts,
        )
        .unwrap();
        let mut channel = SecureChannel::handshake(
            TcpStream::connect(addr(&handle)).unwrap(),
            &remote,
            Role::Initiator,
        )
        .unwrap();
        channel
            .send(&Hello::new("silent").envelope().unwrap())
            .unwrap();
        channel.receive(Duration::from_secs(1)).unwrap();
        wait(Duration::from_secs(2), || online(&handle));
        let began = Instant::now();
        wait(Duration::from_secs(46), || !online(&handle));
        assert!(began.elapsed() >= Duration::from_secs(44));
        handle.shutdown();
    }
    #[test]
    fn changed_key_nonmember_and_unpair_are_rejected_over_tcp() {
        for expected in ["E_LAN_KEY_CHANGED", "E_LAN_NOT_MEMBER"] {
            let identity = Arc::new(Identity::generate().unwrap());
            let remote = Identity::generate().unwrap();
            let members = Arc::new(MemoryMembers::default());
            if expected == "E_LAN_KEY_CHANGED" {
                members.insert("remote", [7; 32]);
            }
            let mut handle = LanHandle::start(
                identity,
                Hello::new("local"),
                "Mac".into(),
                members,
                Box::new(StaticDiscovery::default()),
                options(),
            )
            .unwrap();
            let mut channel = SecureChannel::handshake(
                TcpStream::connect(addr(&handle)).unwrap(),
                &remote,
                Role::Initiator,
            )
            .unwrap();
            channel
                .send(&Hello::new("remote").envelope().unwrap())
                .unwrap();
            assert_eq!(channel.receive(Duration::from_secs(2)).unwrap().t, "hello");
            let rejection = channel.receive(Duration::from_secs(2)).unwrap();
            assert_eq!(rejection.body["code"], expected);
            assert_eq!(rejection.ok, Some(false));
            assert!(!online(&handle));
            handle.shutdown();
        }
        let local = Arc::new(Identity::generate().unwrap());
        let remote = Identity::generate().unwrap();
        let members = Arc::new(MemoryMembers::default());
        members.insert("remote", remote.public_key());
        let mut handle = LanHandle::start(
            local,
            Hello::new("local"),
            "Mac".into(),
            members.clone(),
            Box::new(StaticDiscovery::default()),
            options(),
        )
        .unwrap();
        let mut channel = SecureChannel::handshake(
            TcpStream::connect(addr(&handle)).unwrap(),
            &remote,
            Role::Initiator,
        )
        .unwrap();
        channel
            .send(&Hello::new("remote").envelope().unwrap())
            .unwrap();
        channel.receive(Duration::from_secs(1)).unwrap();
        wait(Duration::from_secs(2), || online(&handle));
        members.remove("remote");
        wait(Duration::from_secs(2), || !online(&handle));
        handle.shutdown();
    }
    #[test]
    fn manual_address_filter_and_hello_timeout() {
        let local = Arc::new(Identity::generate().unwrap());
        let remote = Identity::generate().unwrap();
        let members = Arc::new(MemoryMembers::default());
        members.insert("remote", remote.public_key());
        let mut opts = options();
        opts.timing.hello = Duration::from_millis(100);
        let mut handle = LanHandle::start(
            local,
            Hello::new("local"),
            "Mac".into(),
            members,
            Box::new(StaticDiscovery::default()),
            opts,
        )
        .unwrap();
        assert_eq!(
            handle
                .connect_manual("198.18.0.1:47820".parse().unwrap())
                .unwrap_err()
                .code,
            "E_LAN_ADDRESS"
        );
        let _silent = SecureChannel::handshake(
            TcpStream::connect(addr(&handle)).unwrap(),
            &remote,
            Role::Initiator,
        )
        .unwrap();
        let mut timed_out = false;
        wait(Duration::from_secs(2), || {
            while let Some(event) = handle.try_event() {
                if let LanEvent::Rejected { .. } = event {
                    timed_out = true;
                }
            }
            timed_out
        });
        assert!(!online(&handle));
        handle.shutdown();
    }
}

#[cfg(test)]
mod fallback_tests {
    use super::super::{discovery::StaticDiscovery, trust::MemoryMembers};
    use super::*;
    fn wait(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn manual_connection_keeps_nonpreferred_direction_when_other_inbound_is_unavailable() {
        std::env::set_var("KVMFLOW_LAN_ALLOW_LOOPBACK", "1");
        let a = Arc::new(Identity::generate().unwrap());
        let b = Arc::new(Identity::generate().unwrap());
        let ma = Arc::new(MemoryMembers::default());
        let mb = Arc::new(MemoryMembers::default());
        ma.insert("b", b.public_key());
        mb.insert("a", a.public_key());
        let da = StaticDiscovery::default();
        let db = StaticDiscovery::default();
        let options = LanOptions {
            listen_port: 0,
            ..LanOptions::default()
        };
        let mut ha = LanHandle::start(
            a,
            Hello::new("a"),
            "Mac".into(),
            ma,
            Box::new(da.clone()),
            options.clone(),
        )
        .unwrap();
        let mut hb =
            LanHandle::start(b, Hello::new("b"), "PC".into(), mb, Box::new(db), options).unwrap();
        let addr_a = (Ipv4Addr::LOCALHOST, ha.status().listen_port).into();
        let addr_b = (Ipv4Addr::LOCALHOST, hb.status().listen_port).into();
        // Only B can dial A. Keeping a sole nonpreferred connection is crucial
        // when Windows declines an inbound firewall rule.
        hb.connect_manual(addr_a).unwrap();
        wait(|| {
            ha.status().peers.first().is_some_and(|p| p.online)
                && hb.status().peers.first().is_some_and(|p| p.online)
        });
        assert_eq!(ha.status().peers[0].initiator.as_deref(), Some("b"));
        assert_eq!(hb.status().peers[0].device_id, "a");
        assert_eq!(
            ha.status().peers[0].connection_id,
            hb.status().peers[0].connection_id
        );
        // Once A learns a reachable endpoint, it upgrades to the canonical dial.
        da.upsert(DiscoveredPeer {
            device_id: "b".into(),
            name: "PC".into(),
            addresses: vec![addr_b],
            key_fp: String::new(),
            group_fp: String::new(),
            waiting: false,
        });
        wait(|| {
            ha.status().peers[0].initiator.as_deref() == Some("a")
                && hb.status().peers[0].initiator.as_deref() == Some("a")
                && ha.status().peers[0].connection_id == hb.status().peers[0].connection_id
        });
        ha.shutdown();
        hb.shutdown();
    }
}

#[cfg(test)]
mod discovery_regression_tests {
    use super::super::{discovery::StaticDiscovery, trust::MemoryMembers};
    use super::*;
    use std::sync::atomic::AtomicUsize;
    pub(super) fn supervisor() -> (Supervisor, Arc<MemoryMembers>, Receiver<LanEvent>) {
        let identity = Arc::new(Identity::generate().unwrap());
        let hello = Hello::new("local");
        let members = Arc::new(MemoryMembers::default());
        let (tx, rx) = mpsc::sync_channel(256);
        let status = Arc::new(Mutex::new(LanStatus {
            enabled: true,
            listen_port: 0,
            local: LocalStatus {
                device_id: "local".into(),
                key_fp: String::new(),
                addr: None,
            },
            peers: vec![],
            issues: vec![],
        }));
        let ad = Advertisement {
            device_id: "local".into(),
            host_label: "local".into(),
            public_key: identity.public_key(),
            group_id: None,
            port: 0,
            waiting: false,
        };
        (
            Supervisor::new(
                identity,
                hello,
                members.clone(),
                Box::new(StaticDiscovery::default()),
                LanOptions::default(),
                vec![],
                ad,
                status,
                tx,
            ),
            members,
            rx,
        )
    }
    pub(super) fn discovered(id: &str) -> DiscoveredPeer {
        DiscoveredPeer {
            device_id: id.into(),
            name: id.into(),
            addresses: vec!["192.168.1.9:47820".parse().unwrap()],
            key_fp: String::new(),
            group_fp: String::new(),
            waiting: false,
        }
    }
    #[test]
    fn total_discovery_capacity_lru_ttl_and_trusted_retention() {
        let (mut s, members, _rx) = supervisor();
        members.insert("trusted", [1; 32]);
        s.upsert(discovered("trusted"));
        s.removed("trusted");
        assert!(s.peers.contains_key("trusted"));
        for n in 0..512 {
            s.upsert(discovered(&format!("peer-{n}")));
        }
        assert_eq!(s.peers.len(), MAX_PEERS);
        assert_eq!(s.observed.len(), MAX_PEERS);
        assert!(s.candidates.len() <= MAX_PEERS);
        assert!(s.peers.contains_key("trusted"));
        assert!(!s.peers.contains_key("peer-0"));
        assert!(s.peers.contains_key("peer-511"));
        s.prune_discovery(Instant::now() + DISCOVERY_TTL);
        assert_eq!(s.peers.len(), 1);
        assert!(s.peers.contains_key("trusted"));
        assert!(s.candidates.is_empty());
        assert_eq!(s.observed.len(), 1);
    }
    struct ObservedDiscovery {
        inner: StaticDiscovery,
        polls: Arc<AtomicUsize>,
    }
    impl Discovery for ObservedDiscovery {
        fn announce(&mut self, ad: &Advertisement) -> Result<()> {
            self.inner.announce(ad)
        }
        fn poll(&mut self) -> Result<Vec<DiscoveryEvent>> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            self.inner.poll()
        }
        fn stop(&mut self) -> Result<()> {
            self.inner.stop()
        }
    }
    #[test]
    fn withdrawn_untrusted_ids_cannot_accumulate_or_flood_notifications() {
        std::env::set_var("KVMFLOW_LAN_ALLOW_LOOPBACK", "1");
        let discovery = StaticDiscovery::default();
        for n in 0..512 {
            let id = format!("untrusted-{n}");
            discovery.upsert(DiscoveredPeer {
                device_id: id.clone(),
                name: id.clone(),
                addresses: vec!["127.0.0.1:9".parse().unwrap()],
                key_fp: String::new(),
                group_fp: String::new(),
                waiting: false,
            });
            discovery.remove(id);
        }
        let polls = Arc::new(AtomicUsize::new(0));
        let mut handle = LanHandle::start(
            Arc::new(Identity::generate().unwrap()),
            Hello::new("local"),
            "local".into(),
            Arc::new(MemoryMembers::default()),
            Box::new(ObservedDiscovery {
                inner: discovery,
                polls: polls.clone(),
            }),
            LanOptions {
                listen_port: 0,
                ..LanOptions::default()
            },
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while polls.load(Ordering::SeqCst) < 2 {
            assert!(Instant::now() < deadline);
            thread::sleep(TICK);
        }
        thread::sleep(Duration::from_millis(220));
        let count = handle.status().peers.len();
        let mut notices = 0;
        while let Some(event) = handle.try_event() {
            if matches!(event, LanEvent::Peers(_)) {
                notices += 1;
            }
        }
        handle.shutdown();
        assert_eq!(count, 0, "withdrawn discovery IDs were retained");
        assert!(notices <= 2, "one burst generated {notices} notifications");
    }
}

#[cfg(test)]
mod attempt_regression_tests {
    use super::discovery_regression_tests::{discovered, supervisor};
    use super::*;
    #[test]
    fn reaping_a_worker_must_not_consume_the_attempt_or_advance_backoff() {
        let (mut s, _, _) = supervisor();
        s.upsert(discovered("peer"));
        let token = s.begin_attempt(Some("peer".into()));
        let addr = "192.168.1.9:47820".parse().unwrap();
        let join = thread::spawn(move || AttemptResult {
            token,
            addr,
            result: Err(LanError::new("E_LAN_IO", "refused")),
            refusal_key: None,
        });
        while !join.is_finished() {
            thread::yield_now();
        }
        s.workers.push(AttemptWorker {
            token,
            addr,
            join,
            cancellation: Arc::new(AtomicBool::new(false)),
        });
        let results = s.reap_workers();
        assert_eq!(
            s.pending.get("peer"),
            Some(&token),
            "reaper consumed pending before its result"
        );
        assert_eq!(
            s.candidates["peer"].retry.delay, s.options.timing.retry_initial,
            "reaper advanced retry independently of the result"
        );
        assert_eq!(results.len(), 1);
        s.complete_attempt(results.into_iter().next().unwrap());
        assert!(!s.pending.contains_key("peer"));
        assert_eq!(s.candidates["peer"].retry.delay, Duration::from_secs(2));
        s.complete_attempt(AttemptResult {
            token,
            addr,
            result: Err(LanError::new("E_LAN_IO", "duplicate")),
            refusal_key: None,
        });
        assert_eq!(s.candidates["peer"].retry.delay, Duration::from_secs(2));
    }
    #[test]
    fn stale_completion_cannot_clear_a_newer_pending_attempt() {
        let (mut s, _, _) = supervisor();
        s.upsert(discovered("peer"));
        let old = s.begin_attempt(Some("peer".into()));
        let new = s.begin_attempt(Some("peer".into()));
        s.complete_attempt(AttemptResult {
            token: old,
            addr: "192.168.1.9:47820".parse().unwrap(),
            result: Err(LanError::new("E_LAN_IO", "old")),
            refusal_key: None,
        });
        assert_eq!(s.pending.get("peer"), Some(&new));
        assert_eq!(s.candidates["peer"].retry.delay, Duration::from_secs(1));
        s.complete_attempt(AttemptResult {
            token: new,
            addr: "192.168.1.9:47820".parse().unwrap(),
            result: Err(LanError::new("E_LAN_IO", "new")),
            refusal_key: None,
        });
        assert!(!s.pending.contains_key("peer"));
        assert_eq!(s.candidates["peer"].retry.delay, Duration::from_secs(2));
    }
    #[test]
    fn inbound_success_does_not_clear_an_outbound_attempt_for_the_same_device() {
        let (mut s, members, _) = supervisor();
        let remote = Identity::generate().unwrap();
        members.insert("peer", remote.public_key());
        s.upsert(discovered("peer"));
        let outbound = s.begin_attempt(Some("peer".into()));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let identity = s.identity.clone();
        let worker = thread::spawn(move || {
            SecureChannel::handshake(listener.accept().unwrap().0, &identity, Role::Responder)
                .unwrap()
        });
        let remote_channel =
            SecureChannel::handshake(TcpStream::connect(addr).unwrap(), &remote, Role::Initiator)
                .unwrap();
        let channel = worker.join().unwrap();
        let inbound = s.begin_attempt(None);
        s.complete_attempt(AttemptResult {
            token: inbound,
            addr,
            result: Ok(Established {
                channel,
                hello: Hello::new("peer"),
                addr,
                outbound: false,
                pairing: None,
            }),
            refusal_key: None,
        });
        assert_eq!(s.pending.get("peer"), Some(&outbound));
        assert!(s.attempts.contains_key(&outbound));
        assert!(s.live.contains_key("peer"));
        s.close("peer", "test complete");
        drop(remote_channel);
    }
}

#[cfg(test)]
mod revocation_queue_tests {
    use super::super::trust::MemoryMembers;
    use super::*;
    #[test]
    fn revocation_discards_commands_already_in_the_writer_queue() {
        for revoked in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let peer = Identity::generate().unwrap();
            let peer_key = peer.public_key();
            let worker = thread::spawn(move || {
                SecureChannel::handshake(listener.accept().unwrap().0, &peer, Role::Responder)
                    .unwrap()
            });
            let channel = SecureChannel::handshake(
                TcpStream::connect(addr).unwrap(),
                &Identity::generate().unwrap(),
                Role::Initiator,
            )
            .unwrap();
            let mut peer = worker.join().unwrap();
            let control = channel.stream.try_clone().unwrap();
            let members = Arc::new(MemoryMembers::default());
            members.insert("peer", peer_key);
            let (queued, rx) = mpsc::sync_channel(4);
            queued
                .send(Envelope::new("hardware.command", json!({"sequence":1})))
                .unwrap();
            queued
                .send(Envelope::new("hardware.command", json!({"sequence":2})))
                .unwrap();
            if revoked {
                members.remove("peer");
            }
            let (events, received) = mpsc::sync_channel(4);
            let stop = Arc::new(AtomicBool::new(false));
            let writer = spawn_writer(
                channel.stream,
                channel.transport,
                rx,
                "peer".into(),
                1,
                events,
                stop.clone(),
                Duration::from_secs(15),
                members,
                peer_key,
            );
            if revoked {
                assert!(
                    peer.receive(Duration::from_secs(2)).is_err(),
                    "revoked queued command reached peer"
                );
                assert!(
                    matches!(received.recv_timeout(Duration::from_secs(2)).unwrap(),Internal::Closed{code,..} if code=="E_LAN_NOT_MEMBER")
                );
            } else {
                assert_eq!(
                    peer.receive(Duration::from_secs(2)).unwrap().body["sequence"],
                    1
                );
                assert_eq!(
                    peer.receive(Duration::from_secs(2)).unwrap().body["sequence"],
                    2
                );
            }
            stop.store(true, Ordering::Relaxed);
            let _ = control.shutdown(Shutdown::Both);
            writer.join().unwrap();
        }
    }
}

#[cfg(test)]
mod session_source_tests {
    use super::super::{discovery::StaticDiscovery, trust::public_key_string};
    use super::*;
    use std::sync::RwLock;
    fn start(
        identity: Arc<Identity>,
        members: Arc<RwLock<Vec<kvmflow_core::sync::Member>>>,
    ) -> LanHandle {
        std::env::set_var("KVMFLOW_LAN_ALLOW_LOOPBACK", "1");
        LanHandle::start(
            identity,
            Hello::new("local"),
            "local".into(),
            members,
            Box::new(StaticDiscovery::default()),
            LanOptions {
                listen_port: 0,
                ..LanOptions::default()
            },
        )
        .unwrap()
    }
    fn peer(handle: &LanHandle, identity: &Identity) -> SecureChannel {
        let mut peer = SecureChannel::handshake(
            TcpStream::connect((Ipv4Addr::LOCALHOST, handle.status().listen_port)).unwrap(),
            identity,
            Role::Initiator,
        )
        .unwrap();
        peer.send(&Hello::new("peer").envelope().unwrap()).unwrap();
        assert_eq!(peer.receive(Duration::from_secs(2)).unwrap().t, "hello");
        peer
    }
    fn next_source(handle: &LanHandle) -> ([u8; 32], SessionGeneration) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(LanEvent::Message {
                device_id,
                authenticated_key,
                generation,
                message,
            }) = handle.try_event()
            {
                assert_eq!(device_id, "peer");
                assert_eq!(message.t, "hardware.request");
                return (authenticated_key, generation);
            }
            assert!(Instant::now() < deadline, "no authenticated message");
            thread::sleep(TICK);
        }
    }
    fn request(peer: &mut SecureChannel) {
        peer.send(&Envelope::new("hardware.request", json!({})))
            .unwrap();
    }
    #[test]
    fn queued_source_is_rejected_after_same_id_repair_and_handle_restart() {
        let identity = Arc::new(Identity::generate().unwrap());
        let old = Identity::generate().unwrap();
        let new = Identity::generate().unwrap();
        let members = Arc::new(RwLock::new(Vec::new()));
        members.write().unwrap().push(kvmflow_core::sync::Member {
            device_id: "peer".into(),
            public_key: public_key_string(&old.public_key()),
            ..Default::default()
        });
        let mut handle = start(identity.clone(), members.clone());
        let mut first = peer(&handle, &old);
        request(&mut first);
        let (old_key, old_generation) = next_source(&handle);
        assert_eq!(old_key, old.public_key());
        assert!(handle.is_current_session("peer", old_generation));
        // Held runtime work must fail immediately on key replacement, including
        // the interval before the supervisor's periodic revocation pass.
        members.write().unwrap()[0].public_key = public_key_string(&new.public_key());
        members.write().unwrap()[0].removed = false;
        assert!(!handle.is_current_session("peer", old_generation));
        first.close();
        let mut second = peer(&handle, &new);
        request(&mut second);
        let (new_key, new_generation) = next_source(&handle);
        assert_eq!(new_key, new.public_key());
        assert_ne!(old_generation, new_generation);
        assert!(!handle.is_current_session("peer", old_generation));
        assert!(handle.is_current_session("peer", new_generation));
        members.write().unwrap()[0].removed = true;
        assert!(!handle.is_current_session("peer", new_generation));
        second.close();
        handle.shutdown();
        assert!(!handle.is_current_session("peer", new_generation));
        members.write().unwrap()[0].public_key = public_key_string(&new.public_key());
        members.write().unwrap()[0].removed = false;
        let mut restarted = start(identity, members);
        let mut third = peer(&restarted, &new);
        request(&mut third);
        let (_, third_generation) = next_source(&restarted);
        assert_ne!(third_generation, new_generation);
        assert!(!restarted.is_current_session("peer", new_generation));
        assert!(restarted.is_current_session("peer", third_generation));
        third.close();
        let deadline = Instant::now() + Duration::from_secs(2);
        while restarted.is_current_session("peer", third_generation) {
            assert!(Instant::now() < deadline);
            thread::sleep(TICK);
        }
        restarted.shutdown();
    }
}

#[cfg(test)]
mod identity_conflict_regressions {
    use super::*;
    #[test]
    fn duplicate_id_with_a_different_key_reports_issue_but_own_echo_is_ignored() {
        let (mut supervisor, _, events) = super::discovery_regression_tests::supervisor();
        let mut ad = DiscoveredPeer {
            device_id: "local".into(),
            name: "PC".into(),
            addresses: vec!["192.168.1.3:47820".parse().unwrap()],
            key_fp: short_hash(&supervisor.identity.public_key()),
            group_fp: String::new(),
            waiting: false,
        };
        supervisor.upsert(ad.clone());
        assert!(events.try_recv().is_err());
        ad.key_fp = "different".into();
        supervisor.upsert(ad.clone());
        assert!(format!("{:?}", events.try_recv().unwrap()).contains("E_LAN_ID_CONFLICT"));
        supervisor.upsert(ad);
        assert!(events.try_recv().is_err());
        assert_eq!(
            supervisor.status.lock().unwrap().issues,
            vec!["E_LAN_ID_CONFLICT"]
        );
        assert!(supervisor.peers.is_empty());
    }
    #[test]
    fn unreachable_virtual_addresses_rotate_before_backoff_and_refresh_preserves_progress() {
        let (mut supervisor, _, _) = super::discovery_regression_tests::supervisor();
        let ad = DiscoveredPeer {
            device_id: "peer".into(),
            name: "PC".into(),
            addresses: vec![
                "10.242.163.87:47820".parse().unwrap(),
                "10.147.17.37:47820".parse().unwrap(),
                "192.168.1.23:47820".parse().unwrap(),
            ],
            key_fp: String::new(),
            group_fp: String::new(),
            waiting: false,
        };
        supervisor.upsert(ad.clone());
        for index in 0..2 {
            let token = supervisor.begin_attempt(Some("peer".into()));
            supervisor.complete_attempt(AttemptResult {
                token,
                addr: ad.addresses[index],
                result: Err(LanError::new("E_LAN_IO", "unreachable")),
                refusal_key: None,
            });
            let candidate = &supervisor.candidates["peer"];
            assert_eq!(candidate.retry.cursor, index + 1);
            assert!(candidate.retry.next <= Instant::now());
            supervisor.upsert(ad.clone());
            assert_eq!(supervisor.candidates["peer"].retry.cursor, index + 1);
        }
        assert_eq!(
            supervisor.candidates["peer"].addresses[2].ip().to_string(),
            "192.168.1.23"
        );
        let token = supervisor.begin_attempt(Some("peer".into()));
        supervisor.complete_attempt(AttemptResult {
            token,
            addr: ad.addresses[2],
            result: Err(LanError::new("E_LAN_IO", "unreachable")),
            refusal_key: None,
        });
        assert!(supervisor.candidates["peer"].retry.next > Instant::now());
    }
}

#[cfg(test)]
mod unpaired_discovery_tests {
    use super::*;
    #[test]
    fn untrusted_advertisement_exposes_pairing_hint_without_online_or_session_evidence() {
        let (mut supervisor, _, _) = super::discovery_regression_tests::supervisor();
        let ad = super::discovery_regression_tests::discovered("fresh");
        let addr = ad.addresses[0];
        supervisor.upsert(ad);
        let peer = &supervisor.peers["fresh"];
        assert_eq!(peer.pair_addr, Some(addr));
        assert!(!peer.online);
        assert!(peer.addr.is_none());
        assert!(peer.connection_id.is_none());
    }
}
