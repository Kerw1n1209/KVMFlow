# LAN core contract (T2 S1–S3)

This branch owns identities, section merges and pairing proposals. It does not
start LAN transport, connect peers, expose frontend requests, store Noise keys,
or claim hardware picture verification. Schema remains v2.

## Identity and backward compatibility

`identity::{new_device_id,new_group_id}([u8;16])` encode all 128 bits as a
26-character uppercase Crockford base32 ID, prefixed `dev-` / `grp-`. Only the
sidecar obtains OS entropy; core has no random or crypto-key dependency.

`rename_device_id(&mut SwitchGroupSnapshot, old, new)` updates devices, generated
layout IDs and arrival references, layout targets/sources, mixed assignments,
section authors and trust references. Custom layout IDs stay intact. The caller
must reject an already occupied new ID. `rename_pending_remote` also rewrites
pending identities and regenerates their full-version T1 IDs; pending work is
normally created only after identity bootstrap.

Startup keeps its public stable ID in `<config parent>/lan/device-id`, adjacent
to S4's `identity.key` / `identity.dpapi`. Creation uses a flushed temporary file
and atomic hard-link publication, so simultaneous starts converge without
replacing an existing ID. A corrupt identity is preserved and reported. Keys
and `lan.public_key` generation remain S4 work.

An armed legacy configuration is backed up and migrated. A v2 placeholder is
backed up as `config.json.pre-identity-<ms>` before replacement. An **incomplete
v1 file stays byte-identical at startup**: only its in-memory v2 representation
gets the stable ID. The first explicit config save backs up its original bytes
before writing v2. Remote legacy presets remain until pairing is committed.
No ID-only migration changes a route's DDC fingerprint/value writes.

New optional/defaulted fields:

```json
{
  "lan": {"enabled": true, "listen_port": 47820, "public_key": "base64-public-key"},
  "switch_group": {
    "sync": {
      "topology": {"updated_at_ms": 100, "updated_by": "dev-A"},
      "mixed": {"updated_at_ms": 100, "updated_by": "dev-A"},
      "ports_confirmed": false,
      "members": [{"device_id":"dev-A", "public_key":"base64-public-key",
        "platform":"macos", "introduced_by":"dev-A", "revision":1,
        "meta":{"updated_at_ms":100,"updated_by":"dev-A"}, "removed":false}]
    },
    "devices": [{"device_id":"dev-B", "profile_rev":2,
      "monitors":[{"fingerprint":"DEL-A0F1-S:CN0ABC", "local_input":17,
        "local_fingerprint":"DEL-A0F1-4A3B2C1D"}]}]
  }
}
```

This is a field excerpt, not a complete runnable configuration. Missing `lan`
remains omitted; `{}` for LAN means enabled, port 47820, empty public key.
Zero profile revisions and absent aliases remain omitted for old v2 roundtrips.
A pre-LAN snapshot lacking `ports_confirmed` keeps its existing ready status;
an explicit false from pairing prevents arming until the user confirms order.

`GroupSync` is a typed accessor over T1's extensible `snapshot.sync` JSON,
preserving unknown sections. `sync::SectionMeta` is the same T1 wire type;
sidecar `mixed::SectionMeta` re-exports it. `config.backend_fingerprint(group_fp)`
resolves only the local owner's alias. Validation uses that alias when comparing
the local profile. Shared routing stays canonical; the common DDC retry executor
and bounce probes translate at the hardware boundary. S8's future `display.push`
handler must use this executor or the same resolver. T3/T7 inventory/topology
integration must likewise map backend identities before using canonical mixed
assignments; this branch does not implement network-driven reconciliation.

### Internal retained registers

Topology can temporarily omit a device while its owner profile or archived
arrival policy is still newer than another replica's copy. Two internal fields
retain those independent registers: `sync.profile_cache` maps absent device IDs
to `{profile_rev,monitors,available_builtin_screens?}`, and
`sync.archived_arrivals` maps absent device IDs to archived layout IDs. Both have
serde defaults and are omitted when empty. These extend the draft's data model
to preserve association/monotonicity during topology removal and re-addition;
they reuse the existing owner revision and mixed SectionMeta.

Only owner-vetted profile data for current trusted members enters this cache. When a member tombstone takes
effect, `set_group_sync` and the merge/stamp functions immediately clear that
device's retained profile and arrival registers. The member tombstone remains,
so older snapshots cannot refill revoked caches. Current `devices[]` profiles
remain available for local switching, as required for unpairing. Once that
device leaves topology too, its data is not retained. Profile monotonicity is
asserted for retained identities; explicit revocation removes that identity
from the retained-register domain.

At most **64 distinct current/retained identity keys** are accepted, including
arrival-only keys; over-limit results fail validation and preserve local state.
This is well above a physical desk's device count, bounds memory/storage, and
avoids eviction that would silently discard a higher version. Tombstones are
not evicted under this rule. Available built-in counts are stripped from cached
profiles on disk just as they are from current devices.

`config.get` and its UI/debug config view omit both internal fields. `config.set`
rejects any explicit attempt to supply them with `E_NOT_OWNER`, and preserves the
live internal registers when an ordinary UI settings save omits them. Direct
snapshot serialization for authenticated synchronization still includes them.

## Synchronization APIs

```rust
merge_group(local, remote, self_id, source_id)
    -> Result<(SwitchGroupSnapshot, ChangedSections), CoreError>;
stamp_local_edit(old, new, self_id, now_ms)
    -> Result<(SwitchGroupSnapshot, ChangedSections), CoreError>;
SyncSummary::from_snapshot(snapshot) -> SyncSummary;
merge_members(local_members, remote_members, &AuthenticatedMember { device_id, public_key })
    -> Result<(Vec<Member>, Vec<String> /* divergent section IDs */), CoreError>;
```

`merge_group` is the network boundary. Its source ID must already be authenticated
by Noise against the pinned key, never taken from discovery TXT or a snapshot.
It requires an active local trust member. It never accepts a peer's newer claims
about this receiver's own profile; other profiles can advance only when the
sender is that owner. `rejected_profiles` tells the coordinator to log and push
back/request authoritative profiles. Different groups return
`E_LAN_GROUP_CONFLICT`; failed validation returns the unchanged local snapshot
and `ChangedSections.validation_error`. A topology removing this local owner is
also rejected.

Topology and mixed use `(revision, timestamp, author)`, profiles use owner
revision, and trust entries use per-member version keys. Equal-key divergent
contents choose the larger canonical-JSON BLAKE2s digest and report section IDs
for `lan.sync.divergent`. Equal trust version keys with addition vs revocation
**prefer revocation**, before digest comparison. Tombstones cannot be undone by
older snapshots. `SyncSummary` contains tuple keys, profile revisions, a trust
list digest and a normalized whole-snapshot digest.

`stamp_local_edit` calculates content changes, ignores injected counters and
metadata, checks overflow, advances only changed sections and rejects remote
profile edits with `E_NOT_OWNER`. Trust edits also advance topology. Normal edits
cannot rotate an existing public-key pin (`E_LAN_KEY_CHANGED`). This function
does not persist, emit events or integrate `config.set`; S7 owns those hooks. Generic
settings still cannot own mixed (T1's dedicated setters retain that boundary).

### New member acquisition and key changes

A relayed new topology includes profiles that the receiver has not heard from
directly. Keep the live routing snapshot unchanged until those profiles arrive.
First call **authenticated `merge_members`** on the current pinned trust list.
It checks that the source is active and its actual authenticated key matches the
local pin (`E_LAN_NOT_MEMBER` / `E_LAN_KEY_CHANGED`). Never call it on discovery or
unconfirmed pairing data. Persist/retain its resulting trust list separately
from the uncommitted routing proposal, then connect to the new owner and obtain
its own profile. The trust list and routing proposal must belong to the same
group; the coordinator checks group ID before calling this list-only function.

A same/newer trust record changing an existing `device_id`'s key returns
`E_LAN_KEY_CHANGED`; it cannot silently rotate a pin through forwarding. Stale
records with an older key are ignored. Revocation can retain the existing pinned
key while marking it removed, so removal never installs an incoming key.
Explicit numeric-confirmed re-pairing is the only key replacement path.

`merge_sections` is the pure join for already owner/trust-vetted snapshots,
not an authentication boundary. Random two/three-owner editing and exchange
properties test exchange, association, idempotence and key monotonicity on valid
section combinations. Receiver-specific owner protection, pinned-key protection,
and invalid-merge fallback are deliberately separate: those operations cannot
mathematically be commutative while retaining the receiver's own original state.

## Pairing APIs and reports

```rust
PairSide::from_config(config, platform, public_key) -> PairSide;
ProfileReport::from_config(config, platform) -> ProfileReport;
plan_pair(a, b, &PairOptions) -> Result<PairPlan, CoreError>;
```

Arguments may be swapped: basis comes from established membership, then the
explicit initiator ID. Both peers must use identical sides, reachability,
`group_random`, `now_ms`, and user resolutions. Generate random/time once on the
initiator and transmit them; never independently generate them on the receiver.
The `established` flag is checked against the supplied membership list.

S6 must bind `initiator_device_id`, `group_random` and `now_ms` to the actual
authenticated initiator and the same numeric-confirmed pairing attempt on both
ends. The initiator ID must match that session's initiator, not a peer-supplied
claim. Do not reuse options from an earlier/cancelled attempt or generate fresh
random/time values while recomputing an already confirmed plan. A new attempt
requires new options and both user confirmations.

`PairOptions` carries confirmed monitor aliases (`local -> canonical`), mixed
choice (`basis|joiner`) and a replacement decision (`old_device_id`, `replace`).
Offline replacement requires same platform/name plus absence from the supplied
authenticated online set. Distinct existing established groups are refused.
Same-group re-pairing updates keys/member revisions only; topology/mixed/profile
configuration remains unchanged. Each confirmed participant's member revision
becomes the maximum seen by either side plus one, with overflow rejected. Older
pre-pair snapshots therefore cannot challenge or replace the newly pinned key.

`PairPlan.proposed_snapshot` is UI preview only. `snapshot` is **None** when
there is any `Blocking` or unresolved `Choice`; only a present `snapshot` may
be committed. `members` can still be pinned after both numeric confirmations
when configuration has blockers. No disk, member pinning or runtime commit is
performed by `plan_pair`. S6 must back up both config files before commit, clear
local `legacy_remote_preset` fields after a successful first merge, and retain
all local-only USB trigger/advanced/LAN fields. USB trigger sharing/verification
from the product flow belongs to the later runtime setup coordinator.

Issue families:

- Blocking: unmatched monitors, pending input, setup required, duplicate input,
  invalid final snapshot.
- Choice: confirm a unique same-model fingerprint alias, select mixed settings
  with explicit enabled flag, replace an offline device or add as new.
- Info: corrected legacy peer guess, USB port confirmation needed, repaired keys.

No-serial/ambiguous monitors require explicit manual mappings. Empty secondary
configuration adopts the primary's shared draft immediately in the preview,
then waits for all external owner inputs; a pending input never becomes a guessed
DDC value or an executable route. Built-ins are reported but excluded from the
shared external monitor set, contributing `available_builtin_screens` only.
Example `profile.report` body:

```json
{"device_id":"dev-B","name":"游戏 PC","platform":"windows","profile_rev":3,
 "monitors":[
   {"fingerprint":"DEL-A0F1-4A3B2C1D","name":"DELL","builtin":false,
    "input":{"state":"value","value":17,"source":"learned_active_read"}},
   {"fingerprint":"SAC-2763-S:screen2","name":"副屏","builtin":false,
    "input":{"state":"pending"}}
 ]}
```

## Verification and hardware boundary

Core tests use actual `migrate_legacy` inputs for examples 1–7 and recompute each
plan with reversed peer argument order. Additional cases cover empty secondary,
pending values, manual aliases, mixed enabled/disabled choice, established-group
conflict, and the port-confirmation arming gate. Local startup tests cover backups,
failed backup, simultaneous ID creation, corrupt ID preservation, repeated
startup, and first save after incomplete setup. Simulated DDC tests check alias
translation at the actual shared runtime write boundary.

Random properties also cover topology removal/re-addition and cached profiles;
targeted tests cover revoked-cache cleanup, bounds, internal-field injection,
settings preservation and non-persistence of cached availability telemetry.

Hardware still required: compare Mac/Windows fingerprints for the user's two
monitors; inspect preserved real v1/v2 inputs and names during upgrade/pairing;
verify actual monitor picture and local alias behavior on both machines. Noise,
mDNS, OS permissions/firewall, wake/reconnect, TUN behavior and end-to-end mixed
transitions remain S4–S9/T3 integration and hardware acceptance, not simulated
proof from this branch.

Local acceptance (2026-10-21): sidecar workspace `cargo test` 220/220; desktop
`npm test` 64/64; `npm run smoke` 7/7; targeted internal-register settings tests
also pass with an omitted/null sync object. Desktop Cargo.lock resolves offline
with the new core/runtime dependencies. `npm run test:rust` was not rerun: its
known iconutil preprocessing issue is owned by the other branch, so this result
does not claim the desktop Rust wrapper passed. The existing macOS
`vcp_codes::INPUT_SOURCE` dead-code warning is unchanged.
