//! Quarantined pairing channel (S6). It can never carry authenticated
//! application work: only `pair.*` and `profile.report` messages pass, and
//! the peer stays untrusted until both numeric confirmations commit keys
//! through the runtime's MemberStore. A single-flight gate in the supervisor
//! keeps at most one pairing active per transport.
use super::{
    messages::Envelope,
    noise::{self, SecureChannel},
    LanError, Result,
};
use std::{
    net::{Shutdown, SocketAddr},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Releases the supervisor's pairing gate when this attempt ends.
pub(super) struct Permit(pub(super) Arc<AtomicBool>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
/// Handoff from the handshake worker: gate permit, deadline and the
/// already-computed compare value.
pub(super) struct PairStart {
    pub(super) permit: Permit,
    pub(super) deadline: Instant,
    pub(super) sas: String,
}

/// The untrusted pairing link. All access happens on the runtime thread.
pub struct PairLink {
    pub device_id: String,
    /// Authenticated host name from the peer's `hello`, when it sent one.
    pub peer_name: Option<String>,
    pub public_key: [u8; 32],
    /// The peer's socket address; recorded into the transaction record so a
    /// later E_LAN_NOT_MEMBER rejection can be matched to this pairing.
    pub peer_addr: SocketAddr,
    /// Full Noise handshake hash; identifies this pairing attempt.
    pub attempt: [u8; 32],
    pub initiator: bool,
    /// Six-digit compare value both users must confirm.
    pub sas: String,
    pub deadline: Instant,
    writer: SyncSender<Envelope>,
    messages: Mutex<Receiver<Envelope>>,
    stop: Arc<AtomicBool>,
    joins: Mutex<Vec<JoinHandle<()>>>,
    permit: Mutex<Option<Permit>>,
}
impl std::fmt::Debug for PairLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairLink")
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}
const PAIR_ALLOWED: [&str; 2] = ["pair.", "profile.report"];
fn pairing_message(t: &str) -> bool {
    PAIR_ALLOWED.iter().any(|prefix| t.starts_with(prefix))
}

impl PairLink {
    pub(super) fn start(
        mut channel: SecureChannel,
        device_id: String,
        peer_name: Option<String>,
        initiator: bool,
        start: PairStart,
    ) -> Result<Arc<Self>> {
        let sas = start.sas;
        let peer_addr = channel.stream.peer_addr()?;
        let public_key = channel.remote_key();
        let attempt = channel.handshake_hash();
        let mut write_socket = channel.stream.try_clone()?;
        let transport = channel.transport.clone();
        let stop = Arc::new(AtomicBool::new(false));
        channel.set_cancellation(stop.clone());
        let deadline = start.deadline;
        let (writer, writer_rx) = mpsc::sync_channel::<Envelope>(32);
        let (message_tx, messages) = mpsc::sync_channel(32);
        let reader_stop = stop.clone();
        let reader = thread::spawn(move || {
            while !reader_stop.load(Ordering::Acquire) {
                let timeout = deadline.saturating_duration_since(Instant::now());
                if timeout.is_zero() {
                    break;
                }
                match channel.receive(timeout) {
                    Ok(m) if pairing_message(&m.t) => {
                        if message_tx.try_send(m).is_err() {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            reader_stop.store(true, Ordering::Release);
            channel.close();
        });
        let writer_stop = stop.clone();
        let write = thread::spawn(move || {
            while Instant::now() < deadline {
                let stopped = writer_stop.load(Ordering::Acquire);
                match writer_rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(m) => {
                        if noise::send_message(&mut write_socket, &transport, &m).is_err() {
                            break;
                        }
                    }
                    // Drain queued messages even after close() so a final
                    // accept/ack is not lost, then exit.
                    Err(mpsc::RecvTimeoutError::Timeout) if stopped => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => break,
                }
            }
            writer_stop.store(true, Ordering::Release);
            let _ = write_socket.shutdown(Shutdown::Both);
        });
        Ok(Arc::new(Self {
            device_id,
            peer_name,
            public_key,
            peer_addr,
            attempt,
            initiator,
            sas,
            deadline,
            writer,
            messages: Mutex::new(messages),
            stop,
            joins: Mutex::new(vec![reader, write]),
            permit: Mutex::new(Some(start.permit)),
        }))
    }
    /// Release the supervisor's single-flight gate while keeping the frame
    /// channel open. Called when the local commit has landed: the pairing is
    /// past admission control and the next pairing attempt may proceed even
    /// though this session still waits for the peer's final acknowledgement.
    pub fn release_gate(&self) {
        drop(self.permit.lock().unwrap().take());
    }
    pub fn send(&self, t: &str, body: serde_json::Value) -> Result<()> {
        if !pairing_message(t) {
            return Err(LanError::new(
                "E_LAN_NOT_MEMBER",
                "pairing quarantine: only pair.*/profile.report allowed",
            ));
        }
        if self.closed() {
            return Err(LanError::new("E_LAN_CLOSED", "pairing session closed"));
        }
        self.writer
            .try_send(Envelope::new(t, body))
            .map_err(|_| LanError::new("E_LAN_BUSY", "pairing send queue full"))
    }
    pub fn try_message(&self) -> Option<Envelope> {
        self.messages.lock().unwrap().try_recv().ok()
    }
    pub fn closed(&self) -> bool {
        self.stop.load(Ordering::Acquire) || Instant::now() >= self.deadline
    }
    pub fn close(&self) {
        self.stop.store(true, Ordering::Release);
    }
}
impl Drop for PairLink {
    fn drop(&mut self) {
        self.close();
        for join in self.joins.get_mut().unwrap().drain(..) {
            let _ = join.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::identity_store::Identity;
    use super::super::noise::Role;
    use super::*;
    use std::net::TcpStream;

    #[test]
    fn pairing_link_exchanges_pair_messages_and_quarantines_others() {
        let a = Identity::generate().unwrap();
        let b = Identity::generate().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let gate = Arc::new(AtomicBool::new(false));
        let server = thread::spawn(move || -> Arc<PairLink> {
            let accepted = listener.accept().unwrap().0;
            let mut channel = SecureChannel::handshake(accepted, &b, Role::Responder).unwrap();
            channel
                .send(
                    &super::super::messages::Hello::new("dev-b")
                        .envelope()
                        .unwrap(),
                )
                .unwrap();
            // Consume the initiator hello before the pairing transcript.
            channel.receive(Duration::from_secs(2)).unwrap();
            let sas = channel.compare_code(Role::Responder).unwrap();
            let start = PairStart {
                permit: Permit(gate),
                deadline: Instant::now() + Duration::from_secs(5),
                sas,
            };
            PairLink::start(channel, "dev-b".into(), None, false, start).unwrap()
        });
        let mut client =
            SecureChannel::handshake(TcpStream::connect(addr).unwrap(), &a, Role::Initiator)
                .unwrap();
        client
            .send(
                &super::super::messages::Hello::new("dev-a")
                    .envelope()
                    .unwrap(),
            )
            .unwrap();
        // Read the responder hello, then run the pairing transcript.
        client.receive(Duration::from_secs(2)).unwrap();
        let sas = client.compare_code(Role::Initiator).unwrap();
        let link = server.join().unwrap();
        assert_eq!(link.sas, sas);
        link.send("pair.confirm", serde_json::json!({})).unwrap();
        let got = client.receive(Duration::from_secs(2)).unwrap();
        assert_eq!(got.t, "pair.confirm");
        assert_eq!(
            link.send("mixed.set", serde_json::json!({}))
                .unwrap_err()
                .code,
            "E_LAN_NOT_MEMBER"
        );
        link.close();
        client.close();
    }
}
