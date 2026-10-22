//! Pairing adversarial tests (review P0-1/P1-1): a hostile peer must not be
//! able to drive a victim to commit, or extract group data, before the local
//! user confirmed the SAS. The attacker uses the real transport primitives
//! (Noise XX + the pairing quarantine), replaying the frame order a modified
//! client would send.

use kvmflow_core::mixed::TopologyDisplay;
use kvmflow_sidecar::{
    lan::{
        conn::LanOptions,
        discovery::StaticDiscovery,
        identity_store::Identity,
        messages::Hello,
        noise::{Role, SecureChannel},
        trust::public_key_string,
    },
    lan_runtime::RuntimeLanOptions,
    topology::{DisplayTopology, FakeDisplayTopology},
    RuntimeHandle, RuntimeOptions,
};
use serde_json::{json, Value};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const DELL: &str = "SAC-2763-S:0000000000001";
const SAC: &str = "SAC-2466-S:0000000000000";

fn victim_config() -> Value {
    json!({
        "schema_version": 2,
        "local_device": {"device_id": "dev-victim", "host_label": "Victim", "monitors": [
            {"fingerprint": DELL, "local_input": 15, "source": "learned_active_read"},
            {"fingerprint": SAC, "local_input": 7, "source": "learned_active_read"}]},
        "switch_group": {"group_id": "legacy-switch-group", "revision": 1, "devices": [
            {"device_id": "dev-victim", "name": "Victim", "port_index": 1, "monitors": [
                {"fingerprint": DELL, "local_input": 15}, {"fingerprint": SAC, "local_input": 7}]},
            {"device_id": "legacy-remote", "name": "other", "port_index": 2, "monitors": [
                {"fingerprint": DELL, "local_input": 17}, {"fingerprint": SAC, "local_input": 8}]}]},
        "trigger": {"anchor": {"vid_pid": "1a40:0101"}},
        "advanced": {"ddc_retry": {"attempts": 1, "delay_ms": 0}}
    })
}

fn start_victim(dir: &std::path::Path) -> (RuntimeHandle, mpsc::Receiver<(String, Value)>) {
    start_victim_with_config(dir, victim_config())
}

fn open_pair_window(victim: &RuntimeHandle) {
    // The victim's user is on the pairing screen (product flow).
    victim
        .request("lan.pair.window", json!({"open": true}))
        .unwrap();
}

fn start_victim_with_config(
    dir: &std::path::Path,
    config: Value,
) -> (RuntimeHandle, mpsc::Receiver<(String, Value)>) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("config.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    let mut options = RuntimeOptions::new(dir.join("config.json"), dir.join("logs"));
    options.backend = "simulated".into();
    options.scenario = "idle".into();
    options.topology = Some(Box::new(FakeDisplayTopology::new(vec![
        TopologyDisplay {
            fingerprint: DELL.into(),
            builtin: false,
            connected: true,
        },
        TopologyDisplay {
            fingerprint: SAC.into(),
            builtin: false,
            connected: true,
        },
    ])) as Box<dyn DisplayTopology>);
    options.lan = Some(RuntimeLanOptions {
        enabled: true,
        transport: LanOptions {
            listen_port: 0,
            ..LanOptions::default()
        },
        discovery: Some(Box::new(StaticDiscovery::default())),
        peers: vec![],
    });
    let (tx, rx) = mpsc::channel();
    let runtime = RuntimeHandle::start(options, move |kind, data| {
        let _ = tx.send((kind.to_string(), data));
    })
    .unwrap();
    (runtime, rx)
}

struct Attacker {
    channel: SecureChannel,
}

/// Complete a real pairing-channel handshake (SAS transcript included) and
/// return the raw channel. The attacker then sends whatever frames it wants.
fn attacker_dial(port: u16) -> Attacker {
    let identity = Identity::generate().unwrap();
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut channel = SecureChannel::handshake(
        TcpStream::connect(addr).unwrap(),
        &identity,
        Role::Initiator,
    )
    .unwrap();
    let mut hello = Hello::new("dev-attacker");
    hello.caps = vec!["pair.request.v1".into()];
    channel.send(&hello.envelope().unwrap()).unwrap();
    // Victim hello + the SAS commit-reveal transcript.
    channel.receive(Duration::from_secs(5)).unwrap();
    channel.compare_code(Role::Initiator).unwrap();
    Attacker { channel }
}

fn next(rx: &mpsc::Receiver<(String, Value)>, phase: &str) -> Option<Value> {
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok((kind, data)) if kind == "lan.pair" && data["phase"] == phase => return Some(data),
            Ok(_) => continue,
            Err(_) => continue,
        }
    }
    None
}

/// P0-1: a peer that pushes group data or the commit handshake before the
/// local user confirmed must fail, leave the config and the member list
/// untouched, and end the pairing session. The peer's own `pair.confirm`
/// arriving first is LEGAL (users press independently) and must not abort —
/// only data/commit frames do.
#[test]
fn attacker_cannot_commit_before_local_confirmation() {
    std::env::set_var("KVMFLOW_LAN_ALLOW_LOOPBACK", "1");
    let dir = tempfile::tempdir().unwrap();
    let (victim, rx) = start_victim(dir.path());
    open_pair_window(&victim);
    let port = victim.request("lan.status", json!({})).unwrap()["listen_port"]
        .as_u64()
        .unwrap() as u16;
    let mut attacker = attacker_dial(port);

    // The victim shows the SAS and waits. The attacker "confirms" first —
    // that alone is legal and must NOT abort the pairing (N1).
    assert!(next(&rx, "await_confirm").is_some());
    attacker
        .channel
        .send(&kvmflow_sidecar::lan::messages::Envelope::new(
            "pair.confirm",
            json!({}),
        ))
        .unwrap();
    // The victim records the peer's confirmation without failing.
    std::thread::sleep(Duration::from_millis(400));
    while let Ok((kind, data)) = rx.try_recv() {
        assert!(
            !(kind == "lan.pair" && data["phase"] == "failed"),
            "peer-first confirm must not abort the pairing: {data:?}"
        );
    }

    // …then the attacker pushes a plan and the commit handshake, still
    // without the victim's user confirming. Every data/commit step must
    // abort now.
    let zero_random: Vec<u8> = vec![0; 16];
    let options = json!({
        "initiator_device_id": "dev-attacker",
        "group_random": zero_random,
        "now_ms": 1,
    });
    for (t, body) in [
        (
            "pair.plan",
            json!({"options": options, "online_device_ids": []}),
        ),
        ("pair.commit_request", json!({"plan_digest": "x"})),
        ("pair.commit_go", json!({"plan_digest": "x"})),
    ] {
        attacker
            .channel
            .send(&kvmflow_sidecar::lan::messages::Envelope::new(t, body))
            .unwrap();
    }

    // The victim reports the attack as a failed pairing.
    let failed = next(&rx, "failed").expect("pairing must fail, not commit");
    assert_eq!(failed["code"], "E_LAN_PAIR_NOT_CONFIRMED");

    // Config and trust are untouched.
    let cfg = victim.request("config.get", json!({})).unwrap();
    let group = &cfg["switch_group"];
    assert_eq!(group["group_id"], "legacy-switch-group");
    let members = group["sync"]["members"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        members.iter().all(|m| m["device_id"] != "dev-attacker"),
        "attacker pinned as member: {members:?}"
    );
    assert!(!dir.path().join("pairing.pending.json").exists());
    victim.shutdown_and_join().unwrap();
}

/// N1: confirmations are asynchronous — each user presses independently.
/// Both orderings (peer first, local first) with a 3 s gap must progress the
/// pairing without aborting.
#[test]
fn staggered_confirmations_pair_successfully_both_orders() {
    for peer_first in [true, false] {
        std::env::set_var("KVMFLOW_LAN_ALLOW_LOOPBACK", "1");
        let dir = tempfile::tempdir().unwrap();
        let (victim, rx) = start_victim(dir.path());
        open_pair_window(&victim);
        let port = victim.request("lan.status", json!({})).unwrap()["listen_port"]
            .as_u64()
            .unwrap() as u16;
        let mut attacker = attacker_dial(port);
        assert!(next(&rx, "await_confirm").is_some());

        if peer_first {
            // Peer confirms; 3 s later the local user confirms.
            attacker
                .channel
                .send(&kvmflow_sidecar::lan::messages::Envelope::new(
                    "pair.confirm",
                    json!({}),
                ))
                .unwrap();
            std::thread::sleep(Duration::from_millis(3_000));
            victim
                .request("lan.pair.confirm", json!({"sas_ok": true}))
                .unwrap();
        } else {
            // Local user confirms; 3 s later the peer confirms.
            victim
                .request("lan.pair.confirm", json!({"sas_ok": true}))
                .unwrap();
            std::thread::sleep(Duration::from_millis(3_000));
            attacker
                .channel
                .send(&kvmflow_sidecar::lan::messages::Envelope::new(
                    "pair.confirm",
                    json!({}),
                ))
                .unwrap();
        }
        // The pairing did NOT abort, and progressed past the staggered
        // confirmation into negotiation (peer_confirmed is observable when
        // the peer confirmed; in the local-first order the negotiation
        // begins immediately after the peer's late confirm).
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut aborted = false;
        let mut progressed = false;
        while std::time::Instant::now() < deadline {
            while let Ok((kind, data)) = rx.try_recv() {
                if kind == "lan.pair" {
                    match data["phase"].as_str() {
                        Some("failed") => aborted = true,
                        Some("peer_confirmed")
                        | Some("ready_to_commit")
                        | Some("await_resolve") => progressed = true,
                        _ => {}
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            !aborted,
            "peer_first={peer_first}: staggered confirmations must not abort"
        );
        assert!(
            progressed,
            "peer_first={peer_first}: pairing must progress past both confirms"
        );
        victim.shutdown_and_join().unwrap();
    }
}

/// P1-1: no group data may cross the wire before the victim confirmed. The
/// attacker sends pair.confirm + hello_pair out of order; the victim must
/// abort rather than answer with its full PairSide.
#[test]
fn victim_never_sends_group_data_before_confirmation() {
    std::env::set_var("KVMFLOW_LAN_ALLOW_LOOPBACK", "1");
    let dir = tempfile::tempdir().unwrap();
    let (victim, rx) = start_victim(dir.path());
    open_pair_window(&victim);
    let port = victim.request("lan.status", json!({})).unwrap()["listen_port"]
        .as_u64()
        .unwrap() as u16;
    let mut attacker = attacker_dial(port);
    assert!(next(&rx, "await_confirm").is_some());

    // The attacker confirms and immediately asks for the side exchange. The
    // victim has NOT confirmed: it must fail the session and must never
    // transmit its side (the only legal reply, pair.hello_pair, would carry
    // member keys, device names, monitor fingerprints and input values).
    attacker
        .channel
        .send(&kvmflow_sidecar::lan::messages::Envelope::new(
            "pair.confirm",
            json!({}),
        ))
        .unwrap();
    attacker
        .channel
        .send(&kvmflow_sidecar::lan::messages::Envelope::new(
            "pair.hello_pair",
            json!({"side": Value::Null}),
        ))
        .unwrap();

    let failed = next(&rx, "failed").expect("unconfirmed side exchange must abort");
    assert_eq!(failed["code"], "E_LAN_PAIR_NOT_CONFIRMED");
    // No frame after the abort carries a side object: the channel is closed.
    let mut leaked = false;
    while let Ok((kind, data)) = rx.try_recv() {
        if kind == "lan.pair" && !data["issues"].is_null() {
            leaked = true;
        }
    }
    assert!(!leaked, "no plan/issues may surface pre-confirmation");
    victim.shutdown_and_join().unwrap();
}

/// P2-6: with the pairing window closed, an inbound pairing dial is refused
/// before the SAS transcript (no prompt, no gate occupation). The attacker
/// connects and starts the pairing transcript; the victim must reject.
#[test]
fn inbound_pairing_refused_outside_window() {
    std::env::set_var("KVMFLOW_LAN_ALLOW_LOOPBACK", "1");
    let dir = tempfile::tempdir().unwrap();
    let (victim, rx) = start_victim(dir.path());
    open_pair_window(&victim);
    let port = victim.request("lan.status", json!({})).unwrap()["listen_port"]
        .as_u64()
        .unwrap() as u16;

    // The UI closed the pairing window (first session ended / user left the
    // pairing screen).
    victim
        .request("lan.pair.window", json!({"open": false}))
        .unwrap();

    // A raw pairing dial: the victim refuses after the hello exchange (the
    // intent is only visible then), so the transcript never completes.
    let identity = Identity::generate().unwrap();
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut channel = SecureChannel::handshake(
        TcpStream::connect(addr).unwrap(),
        &identity,
        Role::Initiator,
    )
    .unwrap();
    let mut hello = Hello::new("dev-attacker");
    hello.caps = vec!["pair.request.v1".into()];
    channel.send(&hello.envelope().unwrap()).unwrap();
    // The victim answers with its hello, then refuses the pairing intent by
    // closing; no SAS prompt ever reaches the UI.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(std::time::Instant::now() < deadline);
        if let Ok((kind, data)) = rx.try_recv() {
            assert!(
                !(kind == "lan.pair" && data["phase"] == "await_confirm"),
                "closed pairing window must not show the SAS prompt"
            );
        }
        // The refusal shows up as the channel closing on the next read.
        if channel.receive(Duration::from_millis(200)).is_err() {
            break;
        }
    }
    let mut saw_prompt = false;
    while let Ok((kind, data)) = rx.try_recv() {
        if kind == "lan.pair" && data["phase"] == "await_confirm" {
            saw_prompt = true;
        }
    }
    assert!(!saw_prompt);
    victim.shutdown_and_join().unwrap();
}

/// N3 (true one-sided, transport-independent): a surviving record whose peer
/// refuses us with E_LAN_NOT_MEMBER — the definitive one-sided outcome —
/// tombstones the pin, clears the record, and surfaces
/// E_LAN_PAIR_ONESIDED. The mock peer answers our session attempt with the
/// refusal frame, exactly as a rolled-back peer does.
#[test]
fn one_sided_record_undone_on_member_refusal() {
    std::env::set_var("KVMFLOW_LAN_ALLOW_LOOPBACK", "1");
    let dir = tempfile::tempdir().unwrap();
    let (victim, _rx) = start_victim(dir.path());
    open_pair_window(&victim);

    // Bind the refusing mock first so its address can go into the record.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mock_addr = listener.local_addr().unwrap();

    // Plant the one-sided state: the victim's config carries the peer pin
    // AND a transaction record whose member list matches it (the durable
    // marker that this end committed), for a peer at the mock's address.
    let cfg = victim.request("config.get", json!({})).unwrap();
    let members = cfg["switch_group"]["sync"]["members"].clone();
    // The refusal is only one-sided evidence when the Noise key matches
    // the record pin (N9), so the mock's real identity key goes in the record.
    let mock_identity = Identity::generate().unwrap();
    let refused_key = public_key_string(&mock_identity.public_key());
    let mut planted = members.as_array().cloned().unwrap_or_default();
    planted.push(json!({
        "device_id": "dev-ghost", "public_key": refused_key, "platform": "macos",
        "introduced_by": "dev-victim", "revision": 2,
        "meta": {"updated_at_ms": 9, "updated_by": "dev-victim"}, "removed": false
    }));
    planted.sort_by(|x, y| x["device_id"].as_str().cmp(&y["device_id"].as_str()));
    let record = json!({
        "attempt": "0123456789abcdef0123456789abcdef",
        "peer_device_id": "dev-ghost",
        "peer_addr": mock_addr.to_string(),
        "initiator": true,
        "members": planted,
    });
    drop(victim);
    let mut disk = cfg.clone();
    disk["switch_group"]["sync"]["members"] = json!(planted);
    std::fs::write(
        dir.path().join("pairing.pending.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let (victim, rx) = start_victim_with_config(dir.path(), disk);

    // Mock peer: accept the victim's session attempt and refuse membership.
    let refuse = thread::spawn(move || {
        use kvmflow_sidecar::lan::messages::Envelope;
        for _ in 0..40 {
            if let Ok((stream, _)) = listener.accept() {
                let mut channel =
                    SecureChannel::handshake(stream, &mock_identity, Role::Responder).unwrap();
                // Receive the victim's hello, refuse as a non-member.
                let _ = channel.receive(Duration::from_secs(3));
                let mut refusal = Envelope::new(
                    "ack",
                    json!({"code": "E_LAN_NOT_MEMBER", "message": "not a member"}),
                );
                refusal.ok = Some(false);
                let _ = channel.send(&refusal);
                channel.close();
            }
        }
    });

    // The victim dials the refusing peer; the record's peer_addr matches the
    // refusal address, so the one-sided undo runs. The notification may
    // arrive slightly before or after this point, so poll the channel.
    victim
        .request("lan.connect.manual", json!({"addr": mock_addr.to_string()}))
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    let mut undone = false;
    while std::time::Instant::now() < deadline && !undone {
        while let Ok((kind, data)) = rx.try_recv() {
            if kind == "lan.pair"
                && data["phase"] == "failed"
                && data["code"] == "E_LAN_PAIR_ONESIDED"
            {
                undone = true;
            }
        }
        if !undone {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    assert!(undone, "member refusal must surface E_LAN_PAIR_ONESIDED");
    // The record is cleared and the pin tombstoned.
    assert!(!dir.path().join("pairing.pending.json").exists());
    let cfg = victim.request("config.get", json!({})).unwrap();
    let ghost = cfg["switch_group"]["sync"]["members"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|m| m["device_id"] == "dev-ghost")
        .expect("ghost pin retained");
    assert_eq!(
        ghost["removed"],
        json!(true),
        "one-sided pin must be tombstoned"
    );
    drop(refuse);
    victim.shutdown_and_join().unwrap();
}
