# LAN transport integration (S4/S5)

This module is opt-in and belongs to the sidecar runtime. It does not load or
change configuration, run hardware operations, start with the normal runtime,
or introduce new JSONL methods automatically. The runtime should own one
`LanHandle`, drain its events, and shut it down before exiting.

## Public entry points

| Module | API | Contract |
| --- | --- | --- |
| `identity_store` | `default_path`, `load_or_create`, `Identity::generate/public_key` | Versioned persistent X25519 key; macOS directory 0700 / file 0600, Windows current-user DPAPI. Corrupt keys are preserved and reported, never regenerated silently. No private-key Debug/Serialize/export. |
| `address` | `AddressPolicy::default/from_env/check` | Allow-list LAN ranges only, including IPv6 ULA/link-local; excludes CGNAT/Tailscale, benchmark/fake-IP, IPv4-mapped IPv6, public and unspecified IPs. Loopback requires `KVMFLOW_LAN_ALLOW_LOOPBACK=1`. |
| `trust` | `MemberStore::check`, `MemoryMembers::insert/remove` | Return `E_LAN_NOT_MEMBER` for absent/removed IDs; `E_LAN_KEY_CHANGED` for a different authenticated static key. S6/S7 own changes to this store. |
| `noise` | `SecureChannel::handshake/send/receive/compare_code/close`, `Role` | Low-level, initially **unauthenticated** session. XX proves possession of the remote key; membership or two user approvals must establish trust. Caller applies AddressPolicy before opening/accepting sockets. |
| `messages` | `Envelope`, `Hello` | Version 1 envelopes; opaque `summary` leaves snapshot semantics to S7. Frames are limited to 60 KiB **including** the 16-byte Noise tag; max JSON plaintext is 61,424 bytes. |
| `discovery` | `Discovery::announce/poll/stop`, `MdnsDiscovery`, `StaticDiscovery` | Nonblocking discovery hints. StaticDiscovery clones can inject updates or inspect actual advertised ports. mDNS metadata is never an authentication source. IPv6 scope IDs are retained. |
| `conn` | `LanHandle::start/status/status_json/try_event/is_current_session/send/connect_manual/reannounce/disconnect/shutdown` | Supervisor plus per-session reader/writer threads, bounded queues and handshake concurrency. Both IP families listen on one port; 47820 by default, random on collision. |

`LanEvent::Peers(...).notification()` returns `("lan.peers", {"peers": [...]})`.
Route runtime `lan.status` to `status_json()`. Status contains the actual port,
authenticated online peers, public key fingerprints, last-seen time, selected
initiator/connection ID and transport errors. It has no private identity data.
The local version of `hello` is supplied at start; rebuild/restart the handle
when its identity or hello metadata must change. State-change messages use
`send` and are interpreted on the runtime thread, not in transport.

`send` returns immediately with `E_LAN_OFFLINE` when the snapshot is offline;
otherwise success means queued. `SendFailed` reports an intervening disconnect
or queue failure (with the message ID when present). Application ack/deadline,
retry, transactions and UI status belong to S7/S8. A saturated runtime event
queue closes sessions; status snapshots remain available. Draining events is
required during normal operation.

Connections verify the encrypted hello's device ID against the authenticated
static key and the intended discovered ID. Membership is checked again before
outbound enqueue, after every incoming message and on each supervisor tick;
revocation also closes existing sessions. Ordinary unknown or changed-key
connections receive an encrypted error and close. The first/only working
connection is retained even in the nonpreferred direction; once both directions
work, the smaller device ID's dial wins. Same-direction duplicates use the
shared handshake hash as a deterministic tie-breaker.

TCP connect / Noise handshake / hello deadlines are 3 / 5 / 5 seconds. Ping is
15 seconds, and a frame must arrive within 45 seconds. Retry waits after failure
are 1, 2, 4, 8, 16, 30 seconds; new announcements or `reannounce` reset the wait.
`Timing` is injectable to test the same state transitions faster. Shutdown joins
session threads; pending connection workers finish within their bounded connect,
handshake and hello deadlines (at most about 13 seconds).

## S6–S8 boundary

- S6 must add a single-flight, explicit pairing quarantine at the handshake/hello
  admission point in `Supervisor::spawn`. Normal `LanHandle` connections reject
  nonmembers and changed keys; they cannot implicitly authorize a pairing.
  Reuse `SecureChannel::compare_code` for the strict commit → nonce → reveal
  exchange (`c`, `na`, `nb` are JSON byte arrays). It only returns a display value. Two confirmations, the whole-flow
  120-second deadline, notification, cancel/reject handling, `PairPlan`, config
  backups and final trust insertion remain S6 work. Do not insert into
  `MemoryMembers` merely because the SAS values were calculated.
- S7 supplies an adapter implementing `MemberStore` from runtime-owned config,
  supplies hello summary/caps, handles `LanEvent::Message`, performs merges on
  the runtime thread, and calls `reannounce` after resume/network-change events.
  mdns-sd additionally monitors interface address changes itself.
- S8 consumes outbound delivery/ack outcomes and executes DDC/T3/T1 actions on
  the runtime thread. The transport never mutates mixed state.
- JSONL `--lan`, `--lan-port`, `--lan-peer` and `Msg::Lan` plumbing are deliberately
  not connected in S4/S5. Tests run isolated transport supervisors over TCP,
  not fully integrated simulated hardware runtimes.

## Verification boundary

Unit tests cover persisted identity and Unix permissions, authenticated Noise
and SAS, a key-terminating MITM forwarding commit/reveal, exact/oversized frames,
address boundaries, static discovery, simultaneous bidirectional dials, ping,
real 45-second silence detection, reconnect, manual/fallback direction, unpair,
changed keys/nonmembers, hello timeout, port collision and both IP families.
These are loopback tests; mDNS TXT construction is tested without publishing to
the real LAN. Windows is cross-compiled with `cargo check`, not executed here.

Before integrating/releasing, use two real Mac/Windows machines to check mDNS,
Wi-Fi changes/resume, local-network permission after an ad-hoc update, both
Windows firewall choices, DPAPI persistence/user scope and Clash/Surge TUN.
Fingerprint matching, config pairing/merge and mixed/DDC hardware verification
also remain in the later S6–S8 end-to-end checklist.

## Discovery resource limits

The supervisor keeps at most 128 peer records in total (including historical
records), with matching bounded candidate/observation maps. Removed offline
untrusted IDs are deleted immediately; unrefreshed untrusted hints expire after
five minutes. At capacity it evicts the least recently observed offline hint,
prioritizing untrusted hints. Active connections are preserved. `MemberStore`'s
optional `contains` method is only a retention hint, never authentication.
The mDNS adapter's service-name reverse lookup is independently capped at 128,
with a local five-minute TTL, refresh-aware LRU eviction and at most four names
per device ID. A full table evicts an older hint instead of rejecting a new
resolved device. Expiration runs even without ServiceRemoved; removing one alias
only withdraws the device hint when its last name disappears. DNS name case
variants share one entry. All of these are untrusted discovery hints.
`lan.peers` snapshots are coalesced at most every 100 ms; a full notification
queue keeps the latest state dirty for retry instead of enqueuing every change.

## Connection attempt ownership

Every incoming/outgoing handshake worker has a unique attempt token. Results are
returned by joining finished workers and consumed exactly once by the supervisor;
reaping never changes retry or pending state. Outbound pending entries carry the
specific token. An inbound success cannot clear an outstanding outbound attempt,
and a superseded/duplicate completion cannot clear a newer attempt or advance its
backoff. Worker panic is converted to one failed result. Results cannot disappear
because a shared traffic queue is full.

## Secret lifetime coverage

LAN uses a whitelisted resolver and a scoped local snow 0.10.0 patch in
`sidecar/vendor/snow` (`PATCHES.md` records provenance, changes and the audit).
This covers owned static/ephemeral DH private arrays, cipher session keys,
chaining keys/checkpoints, guarded HMAC/HKDF/DH/rekey buffers, PSKs, generated
keypair Vecs, and BLAKE2s state/buffer on reset/Drop. The resolver alone is not
sufficient: chaining state lives inside snow, so that code is patched too.
The BLAKE2s core adapter has one documented unsafe flat-state wipe, tied to exact
blake2/block-buffer version pins. It uses existing RustCrypto algorithms.
Compiler moves/register spills, arithmetic-library temporaries, swap/core dumps,
caller copies and abort/power-loss do not have a complete erasure guarantee.
No claim of clearing all historical process memory is made.

## S9 license packaging TODO

Generate complete redistributable third-party license material from the actual
locked macOS and Windows dependency graphs, including transitive dependencies
(e.g. curve25519-dalek's BSD-3-Clause), full texts, notices and source provenance.
The current direct-dependency table is not that complete distribution bundle.

## Protocol binding and revocation queues

Every XX handshake binds the prologue `KVMFlow LAN control channel v1` before
exchanging messages. A different protocol name/version fails the handshake.
Fixed-length pairing commitments and pinned public keys use subtle's constant-
time comparison; branch selection occurs only after comparing all bytes.

Revocation is checked again immediately before each writer send; unsent queued
commands for a revoked peer are dropped when that writer closes. A check/write
already in progress cannot be recalled. Messages already delivered/enqueued to
the runtime are NOT automatically canceled by revoking transport membership.
S7/S8 MUST check the CURRENT membership immediately before any hardware action,
including work received before revocation; transport admission is not a durable
execution authorization. Application ack/timeouts remain necessary.

## S7/S8 authenticated work contract

`LanEvent::Message` carries `device_id`, `authenticated_key` (the exact static
public key proved by this handshake), and an opaque `SessionGeneration`, as well
as the envelope. The generation is the full Noise handshake hash; fresh ephemeral
keys make a reconnected/restarted handle's session distinct even when its local
counter restarts. It is an in-memory work stamp, not persisted authorization.

Retain ALL source fields through runtime/pending-work queues. Immediately before
every hardware action (and again after async waits), call
`lan.is_current_session(&device_id, generation)` and discard work if false.
This consults the immediate session registry and current MemberStore, including
the session's authenticated key, not a delayed UI snapshot. Closed/replaced
sessions, changed/removed members, stopped readers/writers and shutdown handles
fail validation. A member ID that exists again is not sufficient authorization
for an old queued message. Membership edits and hardware execution must remain
serialized on the owning runtime thread; validation is a point-in-time check,
not permission to execute indefinitely later.

## Iteration 1 core compatibility

Core `Member.public_key` uses canonical 64-character lowercase hexadecimal;
`trust::public_key_string` and `parse_public_key` bridge the transport's 32-byte
keys. `RwLock<Vec<core::sync::Member>>` implements `MemberStore` and checks the
current snapshot on every admission, send and session-generation validation.
Removed, ambiguous and malformed pins fail closed. The owner must publish trusted
membership edits before executing queued work. This adapter does not implement
S6–S8 runtime wiring or authorize hardware execution.
