# LAN runtime protocol (T2 S6–S9)

Frontend contract for the LAN channel as owned by the sidecar runtime
(`lan_runtime.rs`). All requests go through the standard JSONL surface
(`RuntimeHandle::request`); notifications arrive on the normal sink. Wire
details of the Noise channel itself are in `lan/README.md`; merge semantics
are in `LAN_CORE.md`.

## Requests

### lan.status / lan.peers

Returns the full LAN state (same object as `state.get.lan`):

```json
{
  "enabled": true,
  "listen_port": 47820,
  "self": {
    "device_id": "dev-AK0HV2A4N77VQAJ517G377PKJ4", "key_fp": "1f22c9e5",
    "addr": "192.168.1.20"
  },
  "peers": [
    {
      "device_id": "dev-XYZ987HIJ...", "name": "游戏 PC", "online": true,
      "addr": "192.168.1.23:47820", "last_seen_ms": 1792585733129,
      "initiator": "dev-AK0HV2A4N77VQAJ517G377PKJ4", "app_version": "0.2.3",
      "connection_id": "fc3bbde6", "key_fp": "8624e75f", "platform": "windows",
      "last_error": null
    }
  ],
  "issues": [{"code": "E_LAN_NOT_MEMBER", "message": "…"}],
  "sync": { "dev-XYZ987HIJ...": {"device_id": "dev-XYZ987HIJ...", "status": "synced", "code": null} },
  "setup": { "secondary": false, "trigger_verified": false, "shared_trigger": null }
}
```

`lan.peers` is the same payload (kept as a separate method for symmetry with
the notification). `sync.<id>.status` is `synced | pending | conflict`.
`addr`/`initiator`/`connection_id` are null while offline.

`self.addr` is this machine's LAN IP (no port), shown on the secondary's
waiting screen; it is omitted when no non-loopback private interface exists.
`peers[].platform` is `macos | windows | linux`, taken from the peer's
authenticated `hello` (new peers) or its owner profile (older peers that
predate the field). The key is omitted when the platform is unknown — never
guess it from the name or address.

### lan.discover.start / lan.discover.stop

Re-announces mDNS and resets dial backoff. There is no persistent browse
switch — discovery always runs while LAN is enabled; these exist as UI hooks
for "refresh".

### lan.pair.start

```json
{"v":1,"id":10,"method":"lan.pair.start","params":{"addr":"192.168.1.23:47820"}}
→ {"phase":"connecting","addr":"192.168.1.23:47820",
   "session_id":"9a5f3c0e7b2d4a61b8c9d0e1f2a3b4c5","event_seq":0,
   "local_confirmed":false,"peer_confirmed":false}
```

Starts a quarantined pairing session dialed at the discovered peer's address
(`lan.peers[].addr`). Errors: `E_LAN_BUSY` (another pairing active anywhere
in the flow, single-flight), `E_LAN_ADDRESS` (non-LAN address),
`E_LAN_DISABLED`.

### lan.pair.confirm

```json
{"v":1,"id":11,"method":"lan.pair.confirm","params":{"sas_ok":true}}
→ {"phase":"await_peer_confirm"|"negotiating","sas":"123 456",
   "session_id":"9a5f3c0e7b2d4a61b8c9d0e1f2a3b4c5","event_seq":7,
   "local_confirmed":true,"peer_confirmed":false}
```

The response phase distinguishes the two confirmation orders (N1):
`await_peer_confirm` when the local user confirmed first (still waiting
for the peer's press), `negotiating` when the peer had already confirmed
(pairing advances immediately). The peer's confirmation may arrive at any
time — including before the local user presses — and never aborts the
pairing by itself; only data/commit frames (`hello_pair`/`plan`/
`commit_*`) arriving before the local confirmation abort with
`E_LAN_PAIR_NOT_CONFIRMED`.

`sas_ok:false` cancels this end (`{"phase":"cancelled"}`) and aborts the peer.
Both ends must confirm within the 120 s whole-flow deadline.

### lan.pair.resolve

Only valid on the initiating computer (the one that pressed 连接). Pass the
user's answers to the `choice` issues reported by the `lan.pair`
notification's `issues` array:

```json
{"v":1,"id":12,"method":"lan.pair.resolve","params":{
  "monitor_aliases": {
    "DEL-A0F1-4A3B2C1D": "DEL-A0F1-S:CN0ABC",
    "SAC-2763-S:0009": false
  },
  "mixed": "basis",
  "replacement": {"old_device_id": "dev-OLD...", "replace": true}
}}
→ {"phase":"ready_to_commit"|"await_resolve", "issues":[…], "proposed_snapshot":{…},
   "session_id":"9a5f3c0e7b2d4a61b8c9d0e1f2a3b4c5","event_seq":9,
   "local_confirmed":true,"peer_confirmed":true}
```

- `monitor_aliases` answers `monitor_alias` choices ("are these the same
  physical display?"). A string value is a positive alias (joiner backend
  fingerprint → canonical group fingerprint). `false` (or `null`) explicitly
  REJECTS the guessed correspondence: this display is a different physical
  screen, so it stays in the blocking `monitor_unmatched` list and the plan
  commits pin-only (`config_merged:false`) instead of guessing an alias.
  A key cannot be both aliased and rejected in one request, and unknown
  fingerprints are `E_CONFIG_INVALID`.
- `mixed` answers the `mixed` choice with `"basis"` (this computer) or
  `"joiner"` (the other one).
- `replacement` answers `replace_device` (an offline same-name machine was
  replaced by a fresh install).
- `phase:"ready_to_commit"` means all choices are resolved; commit then
  happens automatically (full merge, or key pinning while a `blocking` issue
  like duplicate inputs remains — see the `committed` notification's
  `config_merged`).

### lan.pair.window

```json
{"v":1,"id":16,"method":"lan.pair.window","params":{"open":true}}
→ {"open": true}
```

Opens/closes the inbound pairing admission window. `lan.pair.start` and
`lan.setup.secondary` open it automatically. It closes on failure, on
`lan.pair.cancel`, and at every commit completion: when the initiator's
local commit lands (before waiting for the responder), when the responder
sends `pair.committed`, and when the initiator receives it. A refused
inbound dial receives an authenticated `E_LAN_PAIR_WINDOW` ack as the first
frame (the responder checks admission before sending its hello), so the
dialer surfaces the real code. Even when open, attempts are rate-limited to
one per source IP per 30 s and five per 60 s globally.

### lan.pair.cancel

Cancels this end and aborts the peer. No state is left behind on either
machine.

### lan.unpair

```json
{"v":1,"id":13,"method":"lan.unpair","params":{"device_id":"dev-XYZ987HIJ..."}}
→ {"removed":"dev-XYZ987HIJ..."}
```

Marks the member removed (tombstone), closes its session; local switching
keeps working, only sync stops. The removed peer is rejected with
`E_LAN_NOT_MEMBER` on its next connection.

### lan.connect.manual

```json
{"v":1,"id":14,"method":"lan.connect.manual","params":{"addr":"192.168.1.23:47820"}}
→ {"connecting":"192.168.1.23:47820"}
```

The "按地址连接" fallback when mDNS is blocked.

### lan.setup.secondary

Marks this machine as the secondary (副机): it skips local initialization and
waits for the primary to pair. Emits `lan.setup {"secondary":true,
"phase":"waiting_for_primary"}`.

### lan.profile.refresh

Schedules the pending-input read poll immediately (see `lan.profile`).

### lan.test.start / lan.test.stop

「试一下」flow: the primary starts a bounded test window; both ends' USB
arrival/departure and DDC outcomes are aggregated into `lan.test` events.

```json
{"v":1,"id":15,"method":"lan.test.start","params":{"duration_ms":30000}}
→ {"test_id":"c1d2e3f4a5b60718","duration_ms":30000,"ends_ms":1792585763129}
```

`lan.test.stop` returns the current aggregate (same shape as the `lan.test`
notification):

```json
→ {"test_id":"c1d2e3f4a5b60718","events":[
  {"device_id":"dev-AK0H...","kind":"switch.report","body":{
    "test_id":"c1d2e3f4a5b60718","press_id":"dev-AK0H...:1",
    "reporting_device_id":"dev-AK0H...","target_device_id":"dev-XYZ...",
    "test_kind":"commands_sent","per_monitor":[…]
  }},
  {"device_id":"dev-XYZ...","kind":"test.event","body":{
    "test_id":"c1d2e3f4a5b60718","press_id":"dev-AK0H...:1",
    "reporting_device_id":"dev-XYZ...","target_device_id":"dev-XYZ...",
    "test_kind":"picture_arrived","ts_ms":1792585741000
  }}
]}
```

Evidence rules:

- `press_id` (`<device>:<n>`, unique inside the test) identifies one physical
  switch press. The host that runs the DDC writes tags its `switch.report`
  with `test_kind:"commands_sent"`; the host that observes the group arrive
  reports `test_kind:"picture_arrived"` (arrival-correction DDC writes keep
  their own `switch.report` and are re-tagged under the sender's `press_id`
  once it is learned, within 5 s).
- Both ends stream their evidence as `test.event` frames, so each end's
  `lan.test` aggregate contains both `device_id`s under the same `press_id`
  and `target_device_id`. `lan.test` notifications fire as evidence arrives
  (after `stop` too, for in-flight frames).
- Only a report that carries `test_id`, `press_id`, `target_device_id` and a
  non-empty `per_monitor` may be treated as linked evidence. A local-only
  report means "commands sent", never "both computers verified"; DDC success
  alone is not picture evidence.

## Notifications

### Correlation fields on `lan.pair`

Every `lan.pair` notification (including `failed`, `cancelled`,
`peer_committed`, and ones emitted after the live session ended, such as a
startup one-sided reconciliation) and every `lan.pair.*` RPC reply carries:

- `session_id` (string): unique per pairing attempt. A retry mints a new id;
  the id from `lan.pair.start` is reused by that attempt's
  `await_confirm`/`confirm`/… events. The responder has its own id.
  Transaction-record reconciliation events (record confirmation,
  one-sided undo/`E_LAN_PAIR_ONESIDED`) carry the RECORD's session id, not
  the live attempt's: a retry while an old record is reconciled keeps its
  own id, and the UI must ignore record events whose id is not the current
  session.
- `event_seq` (integer): strictly increasing inside one session. Notifications
  and the matching RPC reply for the same transition use the same sequence
  (the UI may drop the duplicate); later transitions are always greater.
- `local_confirmed` / `peer_confirmed` (bool): explicit flags, no longer
  inferred from the phase name. They are false on `await_confirm`, become
  true as each side presses, and stay true through `committed`.
- `peer_name` (string, optional): the peer's host name as authenticated by
  the Noise channel (the `name` field of its hello). Prefer it over an mDNS
  label; it is absent when the peer omits the name.

The UI should ignore an event whose `session_id` belongs to a retired session
or whose `event_seq` is not greater than the last accepted one.

### lan.peers

```json
{"notification":"lan.peers","data":{"peers":[ …same as lan.status.peers… ]}}
```

Fires on discovery changes, connect/disconnect (coalesced ≥100 ms).

### lan.pair

Phases: `await_confirm` → `await_peer_confirm` / `peer_confirmed` →
`await_resolve` (choices open) / `ready_to_commit` → `committing` →
`committed` | `peer_committed` | `failed`. Also `cancelled` after a local
cancel.

Security invariants:

- **No group data before both confirmations.** The `PairSide` exchange
  (member keys, device names, monitor fingerprints, input values) happens
  only after BOTH users confirmed the SAS. Any plan/side/commit frame
  arriving before the local confirmation aborts the pairing
  (`E_LAN_PAIR_NOT_CONFIRMED`); the peer's side is locked after the first
  frame.
- **Two-phase commit.** The initiator sends `pair.commit_request` with the
  plan digest; the responder verifies its own recomputation and answers
  `pair.commit_ready` WITHOUT committing; the initiator commits, sends
  `pair.commit_go`; the responder commits, answers `pair.committed`. The
  initiator's `committed` notification carries `peer_confirmed_commit:false`
  until the peer's acknowledgement (or the sync reconciliation) arrives —
  then a `peer_committed` notification follows.
- **One-sided commits are detected and undone.** The reconciliation
  criterion is "the peer pins US as an active member with the correct
  public key" — never a whole-member-list digest (a legitimately evolved
  peer list must not revoke a healthy pair). Confirmation is cheap and
  always available: any authenticated NON-REFUSAL frame whose key equals
  the public key stored in the transaction record proves the peer's live
  trust list accepts our identity (a different key never confirms or
  revokes it) and confirms the record immediately. A `E_LAN_NOT_MEMBER`
  refusal is evidence only when the authenticated key from the Noise
  channel equals the record's pinned peer key AND the address identifies
  the recorded listener; an impostor at the recorded address cannot undo
  the pin. Link-local IPv6 addresses are not interchangeable. The definitive one-sided outcome is the peer's
  `E_LAN_NOT_MEMBER` refusal — an ok=false ack with that code, honored
  even on a session the dial duel already replaced — which tombstones the
  peer pin, clears the record and surfaces `failed` with
  `E_LAN_PAIR_ONESIDED` and a 请重新配对 prompt. Refusals during the
  two-phase commit window (we committed, the peer not yet) are expected
  and never treated as one-sided. A record whose own commit landed is
  NEVER deleted at startup — only a record whose local commit never
  happened is consumed with `E_LAN_PAIR_INTERRUPTED`.
- **Pin-only commits.** When only blockers remain (duplicate inputs,
  pending inputs), the commit pins keys but does not merge configuration:
  `committed` carries `config_merged:false`.
- **Inbound pairing admission (default CLOSED).** The window opens only
  through an explicit user action — `lan.pair.start` (the initiator),
  `lan.setup.secondary` (the waiting secondary) or `lan.pair.window
  {open:true}` — and closes when a session fails, is cancelled or
  commits. While closed, inbound pairing dials are refused
  (`E_LAN_PAIR_WINDOW`) before the SAS transcript. Even when open, at
  most one attempt per source IP per 30 s and at most 5 inbound attempts
  per 60 s globally. Outbound dials are always allowed.

```json
{"notification":"lan.pair","data":{
  "phase":"await_confirm", "device_id":"dev-XYZ987HIJ...", "initiator":true, "sas":"123 456",
  "peer_name":"游戏 PC",
  "session_id":"9a5f3c0e7b2d4a61b8c9d0e1f2a3b4c5","event_seq":1,
  "local_confirmed":false,"peer_confirmed":false
}}
```

With issues (after both confirms):

```json
{"notification":"lan.pair","data":{
  "phase":"await_resolve", "device_id":"dev-XYZ987HIJ...", "initiator":true, "sas":"123 456",
  "issues":[
    {"severity":"choice","detail":{"kind":"monitor_alias","group_fingerprint":"DEL-A0F1-S:CN0ABC","local_fingerprint":"DEL-A0F1-4A3B2C1D","default_same":true,"resolved":false}},
    {"severity":"blocking","detail":{"kind":"monitor_unmatched","basis":["DEL-A0F1-S:CN0ABC"],"joiner":[]}},
    {"severity":"info","detail":{"kind":"peer_guess_corrected","recorded_by":"dev-…","device_id":"dev-…","fingerprint":"SAC-…","guessed":16,"actual":17}},
    {"severity":"info","detail":{"kind":"ports_need_confirmation"}}
  ],
  "proposed_snapshot":{ …preview, sync section stripped… }
}}
```

Commit result:

```json
{"notification":"lan.pair","data":{"phase":"committed","device_id":"dev-XYZ987HIJ...","config_merged":true,"peer_confirmed_commit":false,"renames":[["legacy-remote","dev-XYZ987HIJ..."]],"session_id":"9a5f3c0e7b2d4a61b8c9d0e1f2a3b4c5","event_seq":12,"local_confirmed":true,"peer_confirmed":true}}
```

`config_merged:false` means keys were pinned but a blocking issue remains
("已连接，切换组还有 N 个问题待处理"). `peer_confirmed_commit:false` means the
local commit is durable (the transaction record on disk) but the peer's
acknowledgement has not been observed yet; a later
`{"phase":"peer_committed"}` notification confirms both ends — or, if the
peer's snapshot never carries the record, `{"phase":"failed","code":"E_LAN_PAIR_ONESIDED"}`
undoes the one-sided pin and asks the user to pair again.

Failure:

```json
{"notification":"lan.pair","data":{"phase":"failed","code":"E_LAN_PAIR_COMMIT","message":"…","device_id":"dev-XYZ987HIJ...","session_id":"9a5f3c0e7b2d4a61b8c9d0e1f2a3b4c5","event_seq":13,"local_confirmed":true,"peer_confirmed":true}}
```

### lan.sync

- `{"status":"synced","device_id":"…"}` after a successful merge.
- `{"status":"pending","device_id":"…","code":"E_LAN_OFFLINE"}` while a known
  member is offline.
- `{"status":"conflict","issues":[…]}` for transport rejections
  (`E_LAN_KEY_CHANGED` etc.).
- `{"status":"mixed_closing","holder":"dev-…","steps":N}` when the close
  transaction starts (show "正在恢复同屏…").
- `{"status":"mixed_close_result","outcome":"ok|partial","holder":"dev-…","reason":"completed|holder_ack_timeout|superseded|holder_offline","steps":[{"step_id":"…","device_id":"…","fingerprint":"…","status":"acked|failed|offline|timed_out|superseded"}]}` —
  show "已恢复同屏" or "「电脑」上的显示器 N 下次按切换器时恢复" per step.

### lan.profile

- `{"device_id":"…","name":"游戏 PC","monitors":[…],"profile_rev":3}` when a
  peer's owner report arrives (live input values / pending). `name` is the
  authenticated owner name; prefer it over discovery labels.
- `{"learned":["SAC-…"],"phase":"inputs_learned"}` when this machine read a
  previously pending local input (§6.1: user switched the monitor over).

### lan.setup

- `{"secondary":true,"phase":"waiting_for_primary"}` after
  `lan.setup.secondary`.
- `{"shared_trigger":{…}}` when the primary's USB switch description arrives.
- `{"trigger_verified":true,…}` when the secondary actually observed the
  trigger device (or `{"by":"…"}` when the peer reported verification).

### lan.test

`{"test_id":"…","events":[…]}` aggregates both ends' USB/DDC evidence during
「试一下」. Fires while the test window runs and is also returned by
`lan.test.stop` (see above).

## state.get / diagnostics additions

`state.get.result.lan` and the `state` notification carry the `lan` object
from `lan.status`. Diagnostics include it too; no private key material is
ever serialized (only `self.key_fp` and peer key fingerprints).


## Record reconciliation and authenticated names (N8/N9/P3)

- Transaction records are written before the config and survive until the
  peer's commit is observed. Their `session_id` is used ONLY for events
  emitted while reconciling that record; it never overwrites the identity of
  a live pairing attempt. This lets a retry proceed while an old record is
  confirmed in the background.
- `E_LAN_NOT_MEMBER` is one-sided evidence only with the right key: the
  refusal carries the Noise-authenticated peer key (first-frame ack or an
  established-session ack), and the runtime compares it with the record's
  `members[peer_device_id].public_key`. A refusal from a different key (or
  from an unrelated link-local address) is a plain connection failure and
  never tombstones the pin.
- `hello.name` (optional) is the authenticated host name. The sidecar sets
  it from the owner config and includes it as `lan.pair.peer_name`; the
  pairing UI shows it instead of the mDNS name. Older peers that omit it
  keep the discovered label (documented UI fallback).
