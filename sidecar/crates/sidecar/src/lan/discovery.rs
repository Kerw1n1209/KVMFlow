//! Discovery supplies hints only; the connection layer authenticates the key.
use super::{address::AddressPolicy, LanError, Result};
use mdns_sd::{ScopedIp, ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    net::{SocketAddr, SocketAddrV6},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const SERVICE_TYPE: &str = "_kvmflow._tcp.local.";
#[derive(Clone, Debug)]
pub struct Advertisement {
    pub device_id: String,
    pub host_label: String,
    pub public_key: [u8; 32],
    pub group_id: Option<String>,
    pub port: u16,
    /// The inbound pairing window is open (TXT `w=1`). A setup wizard on
    /// another computer uses it to pick the computer to pair with.
    pub waiting: bool,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DiscoveredPeer {
    pub device_id: String,
    pub name: String,
    pub addresses: Vec<SocketAddr>,
    pub key_fp: String,
    pub group_fp: String,
    /// Untrusted hint: the peer says its pairing window is open.
    pub waiting: bool,
}
#[derive(Clone, Debug)]
pub enum DiscoveryEvent {
    Upsert(DiscoveredPeer),
    Removed(String),
}
/// Nonblocking poll on the transport supervisor. Implementations own shutdown.
pub trait Discovery: Send {
    fn announce(&mut self, advertisement: &Advertisement) -> Result<()>;
    fn poll(&mut self) -> Result<Vec<DiscoveryEvent>>;
    fn stop(&mut self) -> Result<()>;
}
#[derive(Default)]
struct StaticState {
    events: VecDeque<DiscoveryEvent>,
    advertisement: Option<Advertisement>,
}
/// Clone is a test/manual-address control handle for the same event source.
#[derive(Clone, Default)]
pub struct StaticDiscovery(Arc<Mutex<StaticState>>);
impl StaticDiscovery {
    pub fn new(peers: Vec<DiscoveredPeer>) -> Self {
        let result = Self::default();
        for peer in peers {
            result.upsert(peer);
        }
        result
    }
    pub fn upsert(&self, peer: DiscoveredPeer) {
        self.0
            .lock()
            .unwrap()
            .events
            .push_back(DiscoveryEvent::Upsert(peer));
    }
    pub fn remove(&self, id: impl Into<String>) {
        self.0
            .lock()
            .unwrap()
            .events
            .push_back(DiscoveryEvent::Removed(id.into()));
    }
    pub fn advertisement(&self) -> Option<Advertisement> {
        self.0.lock().unwrap().advertisement.clone()
    }
}
impl Discovery for StaticDiscovery {
    fn announce(&mut self, ad: &Advertisement) -> Result<()> {
        self.0.lock().unwrap().advertisement = Some(ad.clone());
        Ok(())
    }
    fn poll(&mut self) -> Result<Vec<DiscoveryEvent>> {
        Ok(self.0.lock().unwrap().events.drain(..).collect())
    }
    fn stop(&mut self) -> Result<()> {
        self.0.lock().unwrap().advertisement = None;
        Ok(())
    }
}

pub fn short_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn instance_name(label: &str) -> &str {
    let mut end = label.len().min(63);
    while !label.is_char_boundary(end) {
        end -= 1;
    }
    &label[..end]
}
const MAX_SERVICE_NAMES: usize = 128;
const MAX_NAMES_PER_DEVICE: usize = 4;
const SERVICE_HINT_TTL: Duration = Duration::from_secs(300);
/// mdns-sd only emits `ServiceResolved` when a record changes, so a peer that
/// keeps advertising unchanged records would never refresh its hint and would
/// expire after `SERVICE_HINT_TTL`. Browsing again replays every live cached
/// service; it must stay well under the hint TTL.
const REBROWSE_INTERVAL: Duration = Duration::from_secs(120);
struct ServiceHint {
    id: String,
    seen: Instant,
    order: u64,
}
#[derive(Default)]
struct MdnsCache {
    services: HashMap<String, ServiceHint>,
    sequence: u64,
}
impl MdnsCache {
    fn forget(&mut self, fullname: &str, events: &mut Vec<DiscoveryEvent>) {
        if let Some(hint) = self.services.remove(fullname) {
            if !self.services.values().any(|other| other.id == hint.id) {
                events.push(DiscoveryEvent::Removed(hint.id));
            }
        }
    }
    fn expire(&mut self, now: Instant) -> Vec<DiscoveryEvent> {
        let expired: Vec<_> = self
            .services
            .iter()
            .filter(|(_, hint)| now.saturating_duration_since(hint.seen) >= SERVICE_HINT_TTL)
            .map(|(name, _)| name.clone())
            .collect();
        let mut events = vec![];
        for name in expired {
            self.forget(&name, &mut events);
        }
        events
    }

    fn handle(
        &mut self,
        event: ServiceEvent,
        policy: AddressPolicy,
        now: Instant,
    ) -> Vec<DiscoveryEvent> {
        let mut events = self.expire(now);
        match event {
            ServiceEvent::ServiceResolved(info) => {
                if info.get_property_val_str("v") != Some("1") {
                    return events;
                }
                let Some(id) = info
                    .get_property_val_str("id")
                    .filter(|s| !s.is_empty() && s.len() <= 128)
                else {
                    return events;
                };
                let addresses: Vec<_> = info
                    .get_addresses()
                    .iter()
                    .filter_map(|ip| {
                        let addr = match ip {
                            ScopedIp::V6(v) => SocketAddr::V6(SocketAddrV6::new(
                                *v.addr(),
                                info.get_port(),
                                0,
                                v.scope_id().index,
                            )),
                            _ => SocketAddr::new(ip.to_ip_addr(), info.get_port()),
                        };
                        policy.check(addr).ok().map(|()| addr)
                    })
                    .collect();
                if addresses.is_empty() {
                    return events;
                }
                let fullname = info.get_fullname().to_ascii_lowercase();
                // A rename/reassignment must not leave the previous identity alive.
                if self
                    .services
                    .get(&fullname)
                    .is_some_and(|hint| hint.id != id)
                {
                    self.forget(&fullname, &mut events);
                }
                if !self.services.contains_key(&fullname) {
                    let aliases = self.services.values().filter(|hint| hint.id == id).count();
                    let victim = if aliases >= MAX_NAMES_PER_DEVICE {
                        self.services
                            .iter()
                            .filter(|(_, hint)| hint.id == id)
                            .min_by_key(|(_, hint)| hint.order)
                    } else if self.services.len() >= MAX_SERVICE_NAMES {
                        self.services.iter().min_by_key(|(_, hint)| hint.order)
                    } else {
                        None
                    }
                    .map(|(name, _)| name.clone());
                    if let Some(victim) = victim {
                        self.forget(&victim, &mut events);
                    }
                }
                self.sequence += 1;
                self.services.insert(
                    fullname,
                    ServiceHint {
                        id: id.into(),
                        seen: now,
                        order: self.sequence,
                    },
                );
                events.push(DiscoveryEvent::Upsert(DiscoveredPeer {
                    device_id: id.into(),
                    // Older peers name the instance after the label and send no `n`.
                    name: info
                        .get_property_val_str("n")
                        .filter(|n| !n.is_empty())
                        .map(instance_name)
                        .unwrap_or_else(|| {
                            info.get_fullname()
                                .strip_suffix(&format!(".{SERVICE_TYPE}"))
                                .unwrap_or(info.get_fullname())
                        })
                        .into(),
                    addresses,
                    key_fp: info.get_property_val_str("k").unwrap_or_default().into(),
                    group_fp: info.get_property_val_str("g").unwrap_or_default().into(),
                    waiting: info.get_property_val_str("w") == Some("1"),
                }));
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                self.forget(&fullname.to_ascii_lowercase(), &mut events);
            }
            _ => {}
        }
        events
    }
}
/// The instance name is derived from the device ID, never the label: two
/// unconfigured computers share the same (empty) label, and a shared instance
/// name makes each one's own service overwrite the other's in the browser's
/// per-name cache, so neither ever lists the other. The label travels in TXT.
///
/// `revision` makes every announcement's TXT unique. mdns-sd keeps a flushed
/// record cached for one more second and only refreshes the TTL of a record
/// it still holds, so flipping `w` back to a recent value would otherwise
/// never reach a running browser.
fn service_info(ad: &Advertisement, revision: u64) -> Result<ServiceInfo> {
    let key = short_hash(&ad.public_key);
    let group = ad
        .group_id
        .as_ref()
        .map(|g| short_hash(g.as_bytes()))
        .unwrap_or_default();
    let id_hash = short_hash(ad.device_id.as_bytes());
    let revision = revision.to_string();
    let txt = [
        ("v", "1"),
        ("id", ad.device_id.as_str()),
        ("k", key.as_str()),
        ("g", group.as_str()),
        ("n", instance_name(&ad.host_label)),
        ("w", if ad.waiting { "1" } else { "0" }),
        ("r", revision.as_str()),
    ];
    // Hostname never incorporates untrusted TXT or punctuation in a label.
    let hostname = format!("kvmflow-{id_hash}.local.");
    ServiceInfo::new(
        SERVICE_TYPE,
        &format!("KVMFlow-{id_hash}"),
        &hostname,
        "",
        ad.port,
        &txt[..],
    )
    .map_err(mdns_error)
    .map(ServiceInfo::enable_addr_auto)
}
/// Auto-address updates follow interfaces (including Wi-Fi changes). Call
/// announce again on runtime resume/network-change events to force publication.
pub struct MdnsDiscovery {
    daemon: ServiceDaemon,
    receiver: mdns_sd::Receiver<ServiceEvent>,
    fullname: Option<String>,
    revision: u64,
    cache: MdnsCache,
    policy: AddressPolicy,
    browsed_at: Instant,
    rebrowse_interval: Duration,
}
impl MdnsDiscovery {
    pub fn new(policy: AddressPolicy) -> Result<Self> {
        let daemon = ServiceDaemon::new().map_err(mdns_error)?;
        let receiver = match daemon.browse(SERVICE_TYPE) {
            Ok(receiver) => receiver,
            Err(e) => {
                let _ = daemon.shutdown();
                return Err(mdns_error(e));
            }
        };
        Ok(Self {
            daemon,
            receiver,
            fullname: None,
            revision: 0,
            cache: MdnsCache::default(),
            policy,
            browsed_at: Instant::now(),
            rebrowse_interval: REBROWSE_INTERVAL,
        })
    }
}
fn mdns_error(e: mdns_sd::Error) -> LanError {
    LanError::new("E_LAN_DISCOVERY", e.to_string())
}
impl Discovery for MdnsDiscovery {
    fn announce(&mut self, ad: &Advertisement) -> Result<()> {
        self.revision += 1;
        let info = service_info(ad, self.revision)?;
        let fullname = info.get_fullname().to_owned();
        if self.fullname.as_ref().is_some_and(|old| old != &fullname) {
            let _ = self.daemon.unregister(self.fullname.as_ref().unwrap());
        }
        self.daemon.register(info).map_err(mdns_error)?;
        self.fullname = Some(fullname);
        Ok(())
    }
    fn poll(&mut self) -> Result<Vec<DiscoveryEvent>> {
        let now = Instant::now();
        let mut events = vec![];
        for _ in 0..64 {
            let Ok(event) = self.receiver.try_recv() else {
                break;
            };
            events.extend(self.cache.handle(event, self.policy, now));
        }
        if now.saturating_duration_since(self.browsed_at) >= self.rebrowse_interval {
            // Replacing the listener replays the daemon's live cache into the
            // new receiver; the old one stops receiving. Keep the old receiver
            // on failure so discovery degrades to the previous behaviour.
            if let Ok(receiver) = self.daemon.browse(SERVICE_TYPE) {
                self.receiver = receiver;
            }
            self.browsed_at = now;
        }
        events.extend(self.cache.expire(now));
        Ok(events)
    }
    fn stop(&mut self) -> Result<()> {
        if let Some(name) = self.fullname.take() {
            let _ = self.daemon.unregister(&name);
        }
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        self.daemon.shutdown().map_err(mdns_error)?;
        Ok(())
    }
}
impl Drop for MdnsDiscovery {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn txt_hashes_and_utf8_instance_limit() {
        assert_eq!(short_hash(b"abc"), "ba7816bf");
        assert!(instance_name(&"电脑".repeat(30)).len() <= 63);
        assert_eq!(instance_name("Mac"), "Mac");
        let ad = Advertisement {
            device_id: "a".into(),
            host_label: "电脑".repeat(30),
            public_key: [1; 32],
            group_id: Some("group".into()),
            port: 47820,
            waiting: false,
        };
        let txt = [("v", "1"), ("id", ad.device_id.as_str())];
        let info = ServiceInfo::new(
            SERVICE_TYPE,
            instance_name(&ad.host_label),
            "kvmflow-a.local.",
            "192.168.1.1",
            ad.port,
            &txt[..],
        )
        .unwrap();
        assert_eq!(info.get_property_val_str("id"), Some("a"));
        assert_eq!(info.get_port(), 47820);
    }
    /// mdns-sd stays silent while an advertisement is unchanged, so without
    /// re-browsing an unpaired secondary that waits longer than the hint TTL
    /// silently drops out of the primary's list. Uses real daemons on this
    /// host's interfaces; skipped where multicast is unavailable.
    #[test]
    fn rebrowse_replays_unchanged_live_services() {
        let Ok(publisher) = ServiceDaemon::new() else {
            return;
        };
        let txt = [("v", "1"), ("id", "dev-rebrowse-fixture")];
        let info = ServiceInfo::new(
            SERVICE_TYPE,
            "rebrowse-fixture",
            "kvmflow-rebrowse-fixture.local.",
            "",
            47999,
            &txt[..],
        )
        .unwrap()
        .enable_addr_auto();
        publisher.register(info).unwrap();
        let mut browser = MdnsDiscovery::new(AddressPolicy::from_env()).unwrap();
        browser.rebrowse_interval = Duration::from_secs(3600);
        let upserts = |browser: &mut MdnsDiscovery, window: Duration| {
            let deadline = Instant::now() + window;
            let mut count = 0;
            while Instant::now() < deadline {
                count += browser
                    .poll()
                    .unwrap()
                    .iter()
                    .filter(|event| {
                        matches!(event, DiscoveryEvent::Upsert(peer) if peer.device_id == "dev-rebrowse-fixture")
                    })
                    .count();
                std::thread::sleep(Duration::from_millis(50));
            }
            count
        };
        if upserts(&mut browser, Duration::from_secs(6)) == 0 {
            // No usable private interface or multicast on this host.
            let _ = publisher.shutdown();
            return;
        }
        // Let the registration announcements settle: from here on the
        // advertisement is unchanged and mdns-sd emits nothing on its own.
        let mut quiet = false;
        for _ in 0..10 {
            if upserts(&mut browser, Duration::from_secs(3)) == 0 {
                quiet = true;
                break;
            }
        }
        assert!(quiet, "announcements never settled");
        browser.browsed_at = Instant::now() - browser.rebrowse_interval;
        assert!(
            upserts(&mut browser, Duration::from_secs(3)) > 0,
            "an unchanged live service must be replayed after re-browsing"
        );
        let _ = publisher.shutdown();
    }
    /// The wizard on the other computer reads `waiting` from a live update,
    /// so re-registering with a changed TXT must reach an existing browser.
    /// Real daemons; skipped where multicast is unavailable.
    #[test]
    fn toggled_pairing_window_reaches_a_running_browser() {
        let Ok(mut publisher) = MdnsDiscovery::new(AddressPolicy::from_env()) else {
            return;
        };
        let mut ad = Advertisement {
            device_id: "dev-waiting-fixture".into(),
            host_label: "Fixture".into(),
            public_key: [7; 32],
            group_id: None,
            port: 47998,
            waiting: false,
        };
        publisher.announce(&ad).unwrap();
        let mut browser = MdnsDiscovery::new(AddressPolicy::from_env()).unwrap();
        let last_waiting = |browser: &mut MdnsDiscovery, want: bool, window: Duration| {
            let deadline = Instant::now() + window;
            while Instant::now() < deadline {
                for event in browser.poll().unwrap() {
                    if let DiscoveryEvent::Upsert(peer) = event {
                        if peer.device_id == "dev-waiting-fixture" && peer.waiting == want {
                            return true;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            false
        };
        if !last_waiting(&mut browser, false, Duration::from_secs(6)) {
            return;
        }
        ad.waiting = true;
        publisher.announce(&ad).unwrap();
        assert!(last_waiting(&mut browser, true, Duration::from_secs(6)));
        ad.waiting = false;
        publisher.announce(&ad).unwrap();
        assert!(last_waiting(&mut browser, false, Duration::from_secs(6)));
        // The superseded record expires a second later; that must not
        // resurrect the old value.
        assert!(!last_waiting(&mut browser, true, Duration::from_secs(3)));
    }
    fn service_info_test(ad: &Advertisement) -> Result<ServiceInfo> {
        service_info(ad, 1)
    }
    #[test]
    fn open_pairing_window_is_published_in_txt() {
        let mut ad = Advertisement {
            device_id: "dev-a".into(),
            host_label: "Mac".into(),
            public_key: [1; 32],
            group_id: None,
            port: 47820,
            waiting: false,
        };
        let closed = service_info(&ad, 1).unwrap();
        assert_eq!(closed.get_property_val_str("w"), Some("0"));
        ad.waiting = true;
        let open = service_info(&ad, 2).unwrap();
        assert_eq!(open.get_property_val_str("w"), Some("1"));
        // Same instance: toggling the window updates the record in place.
        assert_eq!(closed.get_fullname(), open.get_fullname());
    }
    #[test]
    fn unlabelled_computers_advertise_distinct_instances() {
        let ad = |id: &str, label: &str| Advertisement {
            device_id: id.into(),
            host_label: label.into(),
            public_key: [1; 32],
            group_id: None,
            port: 47820,
            waiting: false,
        };
        let a = service_info_test(&ad("dev-a", "")).unwrap();
        let b = service_info_test(&ad("dev-b", "")).unwrap();
        assert_ne!(a.get_fullname(), b.get_fullname());
        // The same label on two computers still yields distinct instances.
        let c = service_info_test(&ad("dev-c", "MacBook Pro")).unwrap();
        let d = service_info_test(&ad("dev-d", "MacBook Pro")).unwrap();
        assert_ne!(c.get_fullname(), d.get_fullname());
        assert_eq!(c.get_property_val_str("n"), Some("MacBook Pro"));
    }
    #[test]
    fn peer_name_prefers_txt_label_and_falls_back_to_instance() {
        let name = |instance: &str, txt: &[(&str, &str)]| {
            let info = ServiceInfo::new(
                SERVICE_TYPE,
                instance,
                "fixture.local.",
                "192.168.1.9",
                47820,
                txt,
            )
            .unwrap();
            let event = ServiceEvent::ServiceResolved(Box::new(info.as_resolved_service()));
            match MdnsCache::default()
                .handle(event, AddressPolicy::default(), Instant::now())
                .pop()
            {
                Some(DiscoveryEvent::Upsert(peer)) => peer.name,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(
            name(
                "KVMFlow-1234abcd",
                &[("v", "1"), ("id", "dev-a"), ("n", "Studio Mac")]
            ),
            "Studio Mac"
        );
        assert_eq!(
            name(
                "KVMFlow-1234abcd",
                &[("v", "1"), ("id", "dev-a"), ("n", "")]
            ),
            "KVMFlow-1234abcd"
        );
        assert_eq!(name("Old PC", &[("v", "1"), ("id", "dev-old")]), "Old PC");
    }
    fn resolved(name: &str, id: &str) -> ServiceEvent {
        let txt = [("v", "1"), ("id", id)];
        let info = ServiceInfo::new(
            SERVICE_TYPE,
            name,
            "fixture.local.",
            "192.168.1.9",
            47820,
            &txt[..],
        )
        .unwrap();
        ServiceEvent::ServiceResolved(Box::new(info.as_resolved_service()))
    }
    #[test]
    fn real_device_is_discovered_after_128_fake_fullnames() {
        for shared_id in [true, false] {
            let mut cache = MdnsCache::default();
            let now = Instant::now();
            for n in 0..128 {
                let id = if shared_id {
                    "same-fake-id".to_owned()
                } else {
                    format!("fake-{n}")
                };
                cache.handle(
                    resolved(&format!("fake-name-{n}"), &id),
                    AddressPolicy::default(),
                    now,
                );
            }
            let result = cache.handle(
                resolved("real-machine", "real-id"),
                AddressPolicy::default(),
                now,
            );
            assert!(result.iter().any(|event|matches!(event,DiscoveryEvent::Upsert(peer) if peer.device_id=="real-id")),"full cache rejected a newly resolved real device");
        }
    }
    #[test]
    fn aliases_are_bounded_and_removal_waits_for_the_last_name() {
        let mut cache = MdnsCache::default();
        let now = Instant::now();
        for n in 0..128 {
            cache.handle(
                resolved(&format!("alias-{n}"), "same-id"),
                AddressPolicy::default(),
                now,
            );
        }
        assert_eq!(cache.services.len(), MAX_NAMES_PER_DEVICE);
        for n in 124..127 {
            let result = cache.handle(
                ServiceEvent::ServiceRemoved(
                    SERVICE_TYPE.into(),
                    format!("alias-{n}.{SERVICE_TYPE}"),
                ),
                AddressPolicy::default(),
                now,
            );
            assert!(!result
                .iter()
                .any(|e| matches!(e, DiscoveryEvent::Removed(_))));
        }
        let result = cache.handle(
            ServiceEvent::ServiceRemoved(SERVICE_TYPE.into(), format!("alias-127.{SERVICE_TYPE}")),
            AddressPolicy::default(),
            now,
        );
        assert!(matches!(&result[..],[DiscoveryEvent::Removed(id)] if id=="same-id"));
        assert!(cache.services.is_empty());
    }
    #[test]
    fn refresh_updates_lru_and_ttl_is_local_without_removed_events() {
        let mut cache = MdnsCache::default();
        let now = Instant::now();
        for n in 0..128 {
            cache.handle(
                resolved(&format!("name-{n}"), &format!("id-{n}")),
                AddressPolicy::default(),
                now,
            );
        }
        cache.handle(
            resolved("name-0", "id-0"),
            AddressPolicy::default(),
            now + Duration::from_secs(1),
        );
        let result = cache.handle(
            resolved("new-real", "new-id"),
            AddressPolicy::default(),
            now + Duration::from_secs(1),
        );
        assert!(result
            .iter()
            .any(|e| matches!(e,DiscoveryEvent::Removed(id) if id=="id-1")));
        assert!(cache
            .services
            .contains_key(&format!("name-0.{SERVICE_TYPE}")));
        assert_eq!(cache.services.len(), MAX_SERVICE_NAMES);
        let expired = cache.expire(now + SERVICE_HINT_TTL);
        assert_eq!(expired.len(), 126);
        assert_eq!(cache.services.len(), 2);
        assert_eq!(
            cache
                .expire(now + SERVICE_HINT_TTL + Duration::from_secs(1))
                .len(),
            2
        );
        assert!(cache.services.is_empty());
    }
    #[test]
    fn fullname_reassignment_and_case_variants_do_not_pin_old_ids() {
        let mut cache = MdnsCache::default();
        let now = Instant::now();
        cache.handle(resolved("SameName", "old"), AddressPolicy::default(), now);
        let result = cache.handle(resolved("samename", "new"), AddressPolicy::default(), now);
        assert!(result
            .iter()
            .any(|e| matches!(e,DiscoveryEvent::Removed(id) if id=="old")));
        assert!(result
            .iter()
            .any(|e| matches!(e,DiscoveryEvent::Upsert(peer) if peer.device_id=="new")));
        assert_eq!(cache.services.len(), 1);
    }
}
