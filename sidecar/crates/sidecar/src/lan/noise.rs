//! Noise XX on bounded frames. Socket deadlines cover the whole handshake/frame,
//! including trickled bytes, not just each individual read.
use super::{
    identity_store::{Identity, NOISE_PATTERN},
    messages::Envelope,
    LanError, Result,
};
use blake2::{Blake2s256, Digest};
use serde_json::json;
use std::{
    io::{Read, Write},
    net::{Shutdown, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;

/// Limit includes the 16-byte Noise authentication tag (60 * 1024 bytes).
pub const MAX_FRAME: usize = 60 * 1024;
pub const MAX_PLAINTEXT: usize = MAX_FRAME - 16;
pub const PROLOGUE: &[u8] = b"KVMFlow LAN control channel v1";
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Initiator,
    Responder,
}
pub struct SecureChannel {
    pub(super) stream: TcpStream,
    pub(super) transport: Arc<Mutex<snow::TransportState>>,
    handshake_hash: [u8; 32],
    remote_key: [u8; 32],
    cancellation: Option<Arc<AtomicBool>>,
}
impl SecureChannel {
    /// Unauthenticated until MemberStore::check OR both pairing approvals.
    /// This function never changes a trust store.
    pub fn handshake(stream: TcpStream, identity: &Identity, role: Role) -> Result<Self> {
        Self::handshake_cancellable(stream, identity, role, None)
    }
    pub(super) fn handshake_cancellable(
        mut stream: TcpStream,
        identity: &Identity,
        role: Role,
        cancellation: Option<Arc<AtomicBool>>,
    ) -> Result<Self> {
        // macOS accepted sockets can inherit the nonblocking listener flag.
        // Per-session readers use blocking I/O with explicit deadlines.
        stream.set_nonblocking(false)?;
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(Duration::from_secs(3)))?;
        let builder = snow::Builder::with_resolver(
            NOISE_PATTERN.parse()?,
            Box::new(super::crypto::LanResolver),
        )
        .prologue(PROLOGUE)?
        .local_private_key(identity.private_key())?;
        let mut noise = match role {
            Role::Initiator => builder.build_initiator()?,
            Role::Responder => builder.build_responder()?,
        };
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        let mut output = vec![0; MAX_FRAME];
        for index in 0..3 {
            let write = (index % 2 == 0) == (role == Role::Initiator);
            if write {
                stream.set_write_timeout(Some(remaining(deadline)?))?;
                let n = noise.write_message(&[], &mut output)?;
                write_frame(&mut stream, &output[..n])?;
            } else {
                let frame = read_frame_cancellable(&mut stream, deadline, cancellation.as_deref())?;
                let n = noise.read_message(&frame, &mut output)?;
                if n != 0 {
                    return Err(LanError::new("E_LAN_PROTO", "unexpected handshake payload"));
                }
            }
        }
        let handshake_hash = noise
            .get_handshake_hash()
            .try_into()
            .map_err(|_| LanError::new("E_LAN_NOISE", "invalid handshake hash"))?;
        let remote_key = noise
            .get_remote_static()
            .and_then(|k| k.try_into().ok())
            .ok_or_else(|| LanError::new("E_LAN_NOISE", "missing peer static key"))?;
        stream.set_write_timeout(Some(Duration::from_secs(3)))?;
        Ok(Self {
            stream,
            transport: Arc::new(Mutex::new(noise.into_transport_mode()?)),
            handshake_hash,
            remote_key,
            cancellation,
        })
    }
    pub(super) fn set_cancellation(&mut self, cancellation: Arc<AtomicBool>) {
        self.cancellation = Some(cancellation);
    }
    pub fn remote_key(&self) -> [u8; 32] {
        self.remote_key
    }
    /// Also used as the common tie-breaker for duplicate same-direction dials.
    pub fn handshake_hash(&self) -> [u8; 32] {
        self.handshake_hash
    }
    pub fn send(&mut self, message: &Envelope) -> Result<()> {
        send_message(&mut self.stream, &self.transport, message)
    }
    pub fn receive(&mut self, timeout: Duration) -> Result<Envelope> {
        let result = receive_message_cancellable(
            &mut self.stream,
            &self.transport,
            Instant::now() + timeout,
            self.cancellation.as_deref(),
        );
        if result.is_err() {
            self.close();
        }
        result
    }
    pub fn close(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }

    /// Strict commit -> nonce -> reveal transcript. Returns a display value only;
    /// caller must gate trust on both confirmations, single-flight and 120s (S6).
    pub fn compare_code(&mut self, role: Role) -> Result<String> {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce)
            .map_err(|_| LanError::new("E_LAN_NOISE", "random source unavailable"))?;
        let result = self.compare_code_with_nonce(role, nonce);
        if result.is_err() {
            self.close();
        }
        result
    }
    fn compare_code_with_nonce(&mut self, role: Role, nonce: [u8; 16]) -> Result<String> {
        if self.transport.lock().unwrap().is_initiator() != (role == Role::Initiator) {
            return Err(LanError::new(
                "E_LAN_PROTO",
                "pairing role must match Noise role",
            ));
        }
        let deadline = Instant::now() + Duration::from_secs(120);
        let hash = self.handshake_hash;
        let (na, nb) = match role {
            Role::Responder => {
                self.send(&Envelope::new(
                    "pair.commit",
                    json!({"c": commit(&hash, &nonce)}),
                ))?;
                let na = pair_value::<16>(self.receive(remaining(deadline)?)?, "pair.nonce", "na")?;
                self.send(&Envelope::new("pair.reveal", json!({"nb": nonce})))?;
                (na, nonce)
            }
            Role::Initiator => {
                let c = pair_value::<32>(self.receive(remaining(deadline)?)?, "pair.commit", "c")?;
                // Never expose na before the peer's commitment has arrived.
                self.send(&Envelope::new("pair.nonce", json!({"na": nonce})))?;
                let nb =
                    pair_value::<16>(self.receive(remaining(deadline)?)?, "pair.reveal", "nb")?;
                if !bool::from(c.ct_eq(&commit(&hash, &nb))) {
                    self.close();
                    return Err(LanError::new(
                        "E_LAN_PAIR_COMMIT",
                        "pairing commitment mismatch",
                    ));
                }
                (nonce, nb)
            }
        };
        Ok(sas(&hash, &na, &nb))
    }
}
fn pair_value<const N: usize>(message: Envelope, t: &str, field: &str) -> Result<[u8; N]> {
    if message.t != t {
        return Err(LanError::new("E_LAN_PROTO", "unexpected pairing sequence"));
    }
    let value: Vec<u8> =
        serde_json::from_value(message.body.get(field).cloned().unwrap_or_default())?;
    value
        .try_into()
        .map_err(|_| LanError::new("E_LAN_PROTO", "invalid pairing field length"))
}
fn commit(hash: &[u8; 32], nb: &[u8; 16]) -> [u8; 32] {
    let mut h = Blake2s256::new();
    h.update(b"kvmflow-sas-commit-v1");
    h.update(hash);
    h.update(nb);
    h.finalize().into()
}
fn sas(hash: &[u8; 32], na: &[u8; 16], nb: &[u8; 16]) -> String {
    let mut h = Blake2s256::new();
    h.update(b"kvmflow-sas-v1");
    h.update(hash);
    h.update(na);
    h.update(nb);
    let bytes = h.finalize();
    let n = u32::from_be_bytes(bytes[..4].try_into().unwrap()) % 1_000_000;
    format!("{:03} {:03}", n / 1000, n % 1000)
}
pub(super) fn send_message(
    stream: &mut TcpStream,
    transport: &Mutex<snow::TransportState>,
    message: &Envelope,
) -> Result<()> {
    message.validate()?;
    let plain = serde_json::to_vec(message)?;
    if plain.len() > MAX_PLAINTEXT {
        return Err(LanError::new(
            "E_LAN_FRAME",
            "plaintext exceeds 60 KB encrypted frame limit",
        ));
    }
    let mut encrypted = vec![0; plain.len() + 16];
    let n = transport
        .lock()
        .unwrap()
        .write_message(&plain, &mut encrypted)?;
    write_frame(stream, &encrypted[..n])
}
fn receive_message_cancellable(
    stream: &mut TcpStream,
    transport: &Mutex<snow::TransportState>,
    deadline: Instant,
    cancellation: Option<&AtomicBool>,
) -> Result<Envelope> {
    let encrypted = read_frame_cancellable(stream, deadline, cancellation)?;
    let mut plain = vec![0; encrypted.len()];
    let n = transport
        .lock()
        .unwrap()
        .read_message(&encrypted, &mut plain)?;
    let message: Envelope = serde_json::from_slice(&plain[..n])?;
    message.validate()?;
    Ok(message)
}
fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| LanError::new("E_LAN_TIMEOUT", "LAN deadline elapsed"))
}
fn read_exact_deadline(
    stream: &mut TcpStream,
    mut bytes: &mut [u8],
    deadline: Instant,
    cancellation: Option<&AtomicBool>,
) -> Result<()> {
    while !bytes.is_empty() {
        if cancellation.is_some_and(|stop| stop.load(Ordering::Acquire)) {
            return Err(LanError::new("E_LAN_CLOSED", "LAN read cancelled"));
        }
        let remaining = remaining(deadline)?;
        // A shutdown on a cloned Winsock handle need not wake an existing
        // blocking recv. Poll cancellation without losing partial frame bytes
        // or extending the original whole-frame deadline.
        let timeout = if cancellation.is_some() {
            remaining.min(Duration::from_millis(100))
        } else {
            remaining
        };
        stream.set_read_timeout(Some(timeout))?;
        match stream.read(bytes) {
            Ok(0) => return Err(LanError::new("E_LAN_IO", "peer closed connection")),
            Ok(n) => {
                bytes = &mut bytes[n..];
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e)
                if cancellation.is_some()
                    && matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
            {
                continue
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
#[cfg(test)]
fn read_frame(stream: &mut TcpStream, deadline: Instant) -> Result<Vec<u8>> {
    read_frame_cancellable(stream, deadline, None)
}
fn read_frame_cancellable(
    stream: &mut TcpStream,
    deadline: Instant,
    cancellation: Option<&AtomicBool>,
) -> Result<Vec<u8>> {
    let mut length = [0; 2];
    read_exact_deadline(stream, &mut length, deadline, cancellation)?;
    let n = u16::from_be_bytes(length) as usize;
    if n == 0 || n > MAX_FRAME {
        return Err(LanError::new("E_LAN_FRAME", "invalid or oversized frame"));
    }
    let mut bytes = vec![0; n];
    read_exact_deadline(stream, &mut bytes, deadline, cancellation)?;
    Ok(bytes)
}
fn write_frame(stream: &mut TcpStream, bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err(LanError::new("E_LAN_FRAME", "invalid or oversized frame"));
    }
    stream.write_all(&(bytes.len() as u16).to_be_bytes())?;
    stream.write_all(bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::trust::{MemberStore, MemoryMembers};
    use super::*;
    use std::net::TcpListener;
    fn pair() -> (SecureChannel, SecureChannel) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let job = std::thread::spawn(move || {
            SecureChannel::handshake(
                listener.accept().unwrap().0,
                &Identity::generate().unwrap(),
                Role::Responder,
            )
            .unwrap()
        });
        let a = SecureChannel::handshake(
            TcpStream::connect(addr).unwrap(),
            &Identity::generate().unwrap(),
            Role::Initiator,
        )
        .unwrap();
        (a, job.join().unwrap())
    }
    // Model Winsock's pending recv not waking after another handle shuts down:
    // cancel only the token, keep the peer and both sockets open throughout.
    #[test]
    fn cancellation_interrupts_partial_frame_without_socket_shutdown() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let mut stream = listener.accept().unwrap().0;
        peer.write_all(&[0]).unwrap(); // Incomplete frame-length prefix.
        let stop = Arc::new(AtomicBool::new(false));
        let reader_stop = stop.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            tx.send(read_frame_cancellable(
                &mut stream,
                Instant::now() + Duration::from_secs(3),
                Some(&reader_stop),
            ))
            .unwrap();
        });
        stop.store(true, Ordering::Release);
        let outcome = rx.recv_timeout(Duration::from_secs(1));
        // Always join, including on regression, with the finite frame deadline.
        reader.join().unwrap();
        assert_eq!(
            outcome
                .expect("read ignored cancellation")
                .unwrap_err()
                .code,
            "E_LAN_CLOSED"
        );
        drop(peer);
    }

    #[test]
    fn cancellation_poll_keeps_partial_bytes_and_the_whole_frame_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let mut stream = listener.accept().unwrap().0;
        let stop = AtomicBool::new(false);
        let writer = std::thread::spawn(move || {
            peer.write_all(&[0]).unwrap();
            std::thread::sleep(Duration::from_millis(250)); // Cross read polls.
            peer.write_all(&[2, 7, 8]).unwrap();
            peer
        });
        assert_eq!(
            read_frame_cancellable(
                &mut stream,
                Instant::now() + Duration::from_secs(2),
                Some(&stop)
            )
            .unwrap(),
            [7, 8]
        );
        let _peer = writer.join().unwrap();
        let start = Instant::now();
        assert_eq!(
            read_frame_cancellable(&mut stream, start + Duration::from_millis(250), Some(&stop))
                .unwrap_err()
                .code,
            "E_LAN_TIMEOUT"
        );
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn loopback_pairing_and_pinning() {
        let (mut a, mut b) = pair();
        assert_eq!(a.handshake_hash(), b.handshake_hash());
        let members = MemoryMembers::default();
        assert_eq!(
            members.check("b", &a.remote_key()).unwrap_err().code,
            "E_LAN_NOT_MEMBER"
        );
        members.insert("b", a.remote_key());
        members.check("b", &a.remote_key()).unwrap();
        assert_eq!(
            members.check("b", &[0; 32]).unwrap_err().code,
            "E_LAN_KEY_CHANGED"
        );
        let job = std::thread::spawn(move || b.compare_code(Role::Responder).unwrap());
        let sas = a.compare_code(Role::Initiator).unwrap();
        assert_eq!(sas, job.join().unwrap());
        assert_eq!(sas.len(), 7);
    }
    #[test]
    fn mitm_with_own_key_relaying_commit_is_detected() {
        // Two actual TCP/Noise sessions, terminated by a proxy with its OWN key.
        let (mut a, mut proxy_a) = pair();
        let (mut proxy_b, mut b) = pair();
        let victim = std::thread::spawn(move || {
            b.compare_code_with_nonce(Role::Responder, [2; 16]).unwrap()
        });
        let relay = std::thread::spawn(move || {
            let c = proxy_b.receive(Duration::from_secs(5)).unwrap();
            proxy_a.send(&c).unwrap();
            let na = proxy_a.receive(Duration::from_secs(5)).unwrap();
            proxy_b.send(&na).unwrap();
            let nb = proxy_b.receive(Duration::from_secs(5)).unwrap();
            proxy_a.send(&nb).unwrap();
        });
        assert_eq!(
            a.compare_code_with_nonce(Role::Initiator, [1; 16])
                .unwrap_err()
                .code,
            "E_LAN_PAIR_COMMIT"
        );
        relay.join().unwrap();
        victim.join().unwrap();
    }
    #[test]
    fn sas_transcript_vectors() {
        // Deterministic inputs avoid a flaky assertion: independent random
        // six-digit codes can coincide with probability 10^-6.
        assert_ne!(
            sas(&[1; 32], &[1; 16], &[2; 16]),
            sas(&[2; 32], &[1; 16], &[2; 16])
        );
        // Cross-checked using Python hashlib.blake2s, including domain tags.
        assert_eq!(sas(&[1; 32], &[1; 16], &[2; 16]), "155 639");
        assert_eq!(
            commit(&[1; 32], &[2; 16]),
            [
                161, 7, 219, 186, 142, 83, 158, 183, 15, 192, 157, 39, 97, 190, 150, 68, 66, 232,
                161, 107, 56, 88, 34, 8, 164, 193, 173, 65, 55, 148, 97, 27
            ]
        );
    }
    #[test]
    fn frame_limit_in_both_directions_and_bad_json() {
        let (mut a, mut b) = pair();
        let oversized = Envelope::new("test", json!({"data": "x".repeat(MAX_FRAME)}));
        assert_eq!(a.send(&oversized).unwrap_err().code, "E_LAN_FRAME");
        a.stream
            .write_all(&((MAX_FRAME + 1) as u16).to_be_bytes())
            .unwrap();
        assert_eq!(
            b.receive(Duration::from_secs(1)).unwrap_err().code,
            "E_LAN_FRAME"
        );
        let (mut a, mut b) = pair();
        let bytes = vec![0; MAX_FRAME];
        write_frame(&mut a.stream, &bytes).unwrap();
        assert!(b.receive(Duration::from_secs(1)).is_err());
    }
    #[test]
    fn exact_frame_limit_roundtrips_and_oversize_disconnects() {
        let (mut a, mut b) = pair();
        let empty = Envelope::new("data", json!({"text":""}));
        let overhead = serde_json::to_vec(&empty).unwrap().len();
        let maximum = Envelope::new("data", json!({"text":"x".repeat(MAX_PLAINTEXT-overhead)}));
        a.send(&maximum).unwrap();
        assert_eq!(b.receive(Duration::from_secs(1)).unwrap(), maximum);
        a.stream
            .write_all(&((MAX_FRAME + 1) as u16).to_be_bytes())
            .unwrap();
        assert_eq!(
            b.receive(Duration::from_secs(1)).unwrap_err().code,
            "E_LAN_FRAME"
        );
        let mut byte = [0];
        a.stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        assert_eq!(a.stream.read(&mut byte).unwrap(), 0);
    }
    #[test]
    fn out_of_order_pairing_rejected() {
        let (mut a, mut b) = pair();
        b.send(&Envelope::new("pair.reveal", json!({"nb": vec![0;16]})))
            .unwrap();
        assert!(a.compare_code(Role::Initiator).is_err());
    }
    #[test]
    fn prologue_protocol_and_version_are_bound_to_handshake() {
        for (prologue, compatible) in [
            (b"KVMFlow LAN control channel v1".as_slice(), true),
            (b"KVMFlow LAN control channel v2".as_slice(), false),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let remote = std::thread::spawn(move || -> Result<()> {
                let mut stream = listener.accept()?.0;
                let identity = Identity::generate()?;
                let mut responder = snow::Builder::with_resolver(
                    NOISE_PATTERN.parse()?,
                    Box::new(super::super::crypto::LanResolver),
                )
                .prologue(prologue)?
                .local_private_key(identity.private_key())?
                .build_responder()?;
                let deadline = Instant::now() + Duration::from_secs(2);
                let mut out = vec![0; MAX_FRAME];
                for index in 0..3 {
                    if index == 1 {
                        let n = responder.write_message(&[], &mut out)?;
                        write_frame(&mut stream, &out[..n])?;
                    } else {
                        let frame = read_frame(&mut stream, deadline)?;
                        responder.read_message(&frame, &mut out)?;
                    }
                }
                Ok(())
            });
            let result = SecureChannel::handshake(
                TcpStream::connect(addr).unwrap(),
                &Identity::generate().unwrap(),
                Role::Initiator,
            );
            assert_eq!(
                result.is_ok(),
                compatible,
                "application prologue does not match peer's protocol/version"
            );
            assert_eq!(remote.join().unwrap().is_ok(), compatible);
        }
    }

    #[test]
    fn commit_comparison_rejects_a_change_in_every_digest_byte() {
        for index in 0..32 {
            let (mut initiator, mut responder) = pair();
            let mut c = commit(&responder.handshake_hash(), &[2; 16]);
            c[index] ^= 1;
            let remote = std::thread::spawn(move || {
                responder
                    .send(&Envelope::new("pair.commit", json!({"c":c})))
                    .unwrap();
                assert_eq!(
                    responder.receive(Duration::from_secs(2)).unwrap().t,
                    "pair.nonce"
                );
                responder
                    .send(&Envelope::new("pair.reveal", json!({"nb":vec![2;16]})))
                    .unwrap();
            });
            assert_eq!(
                initiator.compare_code(Role::Initiator).unwrap_err().code,
                "E_LAN_PAIR_COMMIT"
            );
            remote.join().unwrap();
        }
    }
}
