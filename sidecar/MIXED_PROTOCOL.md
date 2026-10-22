# Mixed-mode integration contract (T1, review corrections)

Protocol version is 1. This implements core decisions and local orchestration;
LAN transport/authentication, real OS detach and hardware picture acceptance
remain T2/T3 work. `saved:true` means persisted, not that all local/remote work
succeeded. Read transition, execution, warnings and pending state separately.

## Ownership and local requests (T4)

```json
{"v":1,"id":10,"method":"mixed.assign","params":{"assignment":[{"fingerprint":"screen-a","device_id":"mac"},{"fingerprint":"screen-b","device_id":"pc"}]}}
{"v":1,"id":11,"method":"mixed.set","params":{"enabled":true}}
{"v":1,"id":12,"method":"mixed.set","params":{"enabled":false}}
{"v":1,"id":13,"method":"state.get","params":{}}
```

- `mixed.assign` replaces the complete external assignment and advances
  `mixed.revision`. Disabled mode only saves; enabled mode immediately aligns.
- A **changed** `mixed.set` advances revision, saves atomically, then executes.
  An unchanged enabled value returns `{saved:false, applied:false, mixed:...}`;
  it neither advances revision nor writes the file or touches hardware. Existing
  failed work continues on its independent retry schedule.
- Both commands preserve the USB tracker and pause flag. `state.enabled` is the
  USB pause flag; `state.mixed.enabled` is mixed mode.
- `config.set` never owns mixed. Its mixed section is replaced by the live
  section **before parsing/migration**, whether omitted, stale, malformed or
  explicitly different. It also preserves `sync.mixed`. Existing group removal
  or replacement through settings is rejected. For first setup, caller-provided
  mixed is ignored and migration initializes disabled/revision 0; use dedicated
  commands afterward. Disk loading still respects a saved mixed section.
- IO failure (`E_CONFIG_IO`) leaves revision/mode unchanged and executes nothing.
  Duplicate assignment fingerprints, empty IDs, built-ins, invalid parameters
  and revision overflow return `E_CONFIG_INVALID`. Missing/disconnected entries
  may remain in a disabled draft. Enable validation failures are returned as a
  serialized `{ok,reasons}` in the error message and available structured in state.
- `apply_layout` is removed (`E_UNKNOWN_METHOD`). Legacy layouts/arrival IDs and
  layout reports remain migration/diagnostic archives, not UI switching controls.

Local edits stamp only `sync.mixed:{updated_at_ms,updated_by}`. Group
`snapshot.revision` is the **topology section** counter; `mixed.revision` is the
**mixed section** counter. Mixed operations do not advance the topology counter.
`SwitchGroupSnapshot.sync` preserves metadata for sections T1 does not own.

## Trusted remote application (T2)

`RuntimeHandle::apply_remote_mixed(Value)` is the internal `mixed.apply_remote`
entry. It is **absent from public JSONL, RuntimeHandle::request and
protocol.describe**. T2 must authenticate the peer and validate ownership/merge
rules before calling this in-process method. Example argument:

```json
{
  "group_id":"desk",
  "revision":9,
  "enabled":false,
  "assignment":[{"fingerprint":"screen-a","device_id":"mac"},{"fingerprint":"screen-b","device_id":"pc"}],
  "meta":{"updated_at_ms":1792530000000,"updated_by":"pc"},
  "keyboard_holder_device_id":"mac",
  "holder_confidence":"known"
}
```

Group ID must match. Compare the mixed section version lexicographically as
`(revision, meta.updated_at_ms, meta.updated_by)`, matching LAN plan §3.1.
Strictly newer section versions are atomically saved and reconciled without
incrementing revision or emitting `mixed.changed` (no state-broadcast echo).
Older/equal section versions return `{saved:false,applied:false,mixed:...}`.
Without meta, timestamps/author default to 0/empty; same numeric revision then
cannot overwrite newer metadata. Equal numeric revision with greater SectionMeta
**is** a newer section version and can be applied. This is needed for offline
concurrent edits; comparison is not against the topology revision.

T2 remains responsible for BLAKE2s convergence when the entire comparison key is
identical but contents differ; this entry ignores an identical key. Do not try
higher topology revision as a way to force mixed replacement. A future trusted
merged-snapshot integration must carry that resolved identical-key case explicitly.

Remote application still emits local `state`, warnings and directed pending work,
so UI and the coordinator can act. A supplied holder defaults to known; null or
`holder_confidence:"inferred"` blocks close-transition pushes. Supplied IDs must
exist in the group. Receiving a new disabled snapshot while already disabled
only aligns connections; it does not initiate another close push.

## State and progress (T4)

`state.get.result.mixed`, `state.data.mixed` and command responses use this
object. The example is abbreviated; destinations/execution can contain more rows:

```json
{
  "enabled":false,
  "revision":9,
  "section_meta":{"updated_at_ms":1792530000000,"updated_by":"pc"},
  "assignment":[{"fingerprint":"screen-a","device_id":"mac"},{"fingerprint":"screen-b","device_id":"pc"}],
  "validation":{"ok":true,"reasons":[]},
  "keyboard_holder_device_id":"mac",
  "holder_confidence":"known",
  "transition":{
    "phase":"idle",
    "started_ms":1792530000100,
    "outcome":"partial",
    "reasons":[{"code":"remote_push_pending","severity":"warning","device_id":null,"fingerprint":null}]
  },
  "destinations":[{
    "fingerprint":"screen-b",
    "label":"",
    "display_name":"显示器 2",
    "default_display_number":2,
    "builtin":false,
    "available":true,
    "connected":true,
    "availability":"available",
    "unavailable_reason":null,
    "target_device_id":"mac",
    "fixed":false,
    "current_device_id":null,
    "commanded_device_id":null,
    "evidence":"assignment_only"
  }],
  "detached":[],
  "pending_remote":[{"id":"mixed:[\"desk\",9,1792530000000,\"pc\",\"screen-b\",\"mac\"]","device_id":"pc","fingerprint":"screen-b","target_device_id":"mac"}],
  "superseded_remote":[],
  "execution":[{"step":{"step":"attach","fingerprint":"screen-b"},"ok":false,"error":{"code":"E_TOPOLOGY_ATTACH","message":"adapter detail"}}],
  "warnings":[],
  "retry":{"attempt":1,"next_retry_ms":1792530000600},
  "topology_error":null,
  "simulated":false
}
```

A real object also includes canonical `REPORT_NOTE`. Before a group is configured,
`mixed` is null.

### Runtime snapshot ordering (T4 review correction)

State payloads now include a session-scoped, strictly increasing `runtime_seq`.
The single hardware worker assigns it immediately before publishing a `state`,
`mixed.changed`, or `mixed.pending_remote` notification, and before returning
`state.get`, `app.setEnabled`, or a mixed-command result. A mixed object nested
in such a payload repeats the same `runtime_seq` for consumers that receive
only that object. Sequence values are never persisted or synchronized to peers.

The host integration may also stamp `runtime_generation` on these payloads
(and repeat it inside mixed). Missing generation means 0. A host that recreates
the hardware worker within its lifetime must increase this generation before
the new worker publishes any snapshots or command results, and preserve the
originating generation on delayed results from an old worker. Consumers compare
`(runtime_generation, runtime_seq)` lexicographically. A higher generation clears
all cached fields and sequence marks and accepts the new worker's lower sequence;
an older generation is rejected in its entirety, including previously unseen
fields. Top-level stamps take precedence over nested mixed stamps. The current
worker supplies runtime_seq; the desktop RuntimeSlot supplies generation on
all state-bearing replies and notifications, including mixed.changed, pending
work and runtime.error. Native and renderer consumers enforce this ordering. Generation is also local to this host lifetime and never persisted or synced.
`runtime.error` remains an event without a required sequence: it preserves field
sequence marks, and a later accepted state can clear the error. It obeys the same
generation check, so worker recreation must stamp the originating generation on
error events as well.

All full and partial snapshots use this one sequence domain, but consumers keep
a separate high-water mark for each top-level state field (`mixed` is one atomic
field). A partial mixed result at seq 5 must not hide a delayed full snapshot at
seq 4: accept its state/enabled fields if their last accepted sequence was lower,
while retaining mixed at seq 5. Conversely, filling mixed at seq 5 must not undo
enabled=false already accepted at seq 6. Consumers use the merged snapshot,
never the raw partially accepted message. Command acknowledgements (`saved` and
`applied`) are not state fields. Equal sequence copies are idempotent. Merge
partial `app.setEnabled` results without clearing mixed state.
Config/mixed revision orders saved settings, not runtime observations, and is
not a substitute for this sequence. Older unstamped backends remain usable
until a sequenced value is accepted for a field; do not then accept an unstamped
value for that field as newer. This field is additive; protocol version stays 1.

### Progress, retry and one-time warnings

`transition.phase` is `idle|enabling|disabling|restoring`. A `state` notification
with active phase is emitted **before** hardware execution. `started_ms` is UTC
Unix milliseconds for that attempt. During execution outcome is `partial`; after
an attempt, phase becomes idle and outcome is `ok|partial|failed`. Pending remote
work, uncertain H and last-screen protection produce partial results. Use
`disabling`/`restoring` to show “正在恢复同屏…”, and idle/partial plus reasons/pending
to explain that the next USB switch can restore normal routing. Remote ACK/timeout
handling and its final UI outcome remain T2 integration work.

Poll checks inventory changes **and unfinished invariants**: a disabled detached
screen, previous failed execution, or unfinished enumeration. Retry starts at
500ms, then 1s/2s/4s/8s/16s, capped at 30s; worker polling makes actual times no
sooner than those deadlines. It is independent of list changes. An enumeration
failure remains unfinished even if attach succeeded; retry waits/enumerates again.
No-op mode setters do not clear failed work. A new USB holder cancels obsolete
close-transition work and normal USB routing takes over.

`execution[].error` is null or **`{code,message}`**, preserving native adapter
`E_TOPOLOGY_*` and DDC codes. Failed prerequisites use `E_MIXED_PREREQUISITE`.
DDC acceptance only means commanded; `need_remote_push.ok:true` only means queued.
Live topology query failure blocks DDC and detach; it never authorizes them.

A newly active warning emits `mixed.warning` with stable
`{code,severity,device_id,fingerprint}`. Identical active warnings are suppressed
across retries/replans; after resolution, recurrence emits again. This covers
`last_local_screen`, `unassigned_monitor`, failed execution codes, inferred H,
remote pending and unavailable topology. UI can toast/banner once per event.

### Names, availability and picture evidence

Destinations use local `local_device.monitors[]` order, the same order settings
uses for default “显示器 1/2/3”. Local configured label wins, then a nonempty group
monitor label; absent labels use `default_display_number` and `display_name`.
UI can localize the number-based fallback. Remaining saved assignments and known
built-ins follow that order. Unplugged assignments are retained.

`availability` is `available|detached|unavailable`; `available:true,connected:false`
is app-detached. A previously seen built-in disappearing remains a row with
`unavailable_reason:"builtin_closed_or_unavailable"`; this does not claim lid
telemetry distinguishes lid-close from other disappearance. Other absent screens
use `not_in_local_inventory`.

**UI choice implemented:** show the planned destination from `target_device_id`
alongside `evidence`, instead of presenting it as observed current picture.
`current_device_id` is null after writing, unless a later readable input maps to
an owner. Evidence values are:

- `local_topology`: available built-in, always local.
- `input_observation_only`: readable mapped input, not human picture proof.
- `commanded_unconfirmed`: accepted DDC; `commanded_device_id` records its target.
- `assignment_only`: desired destination without fresh observation/accepted write.
- `unavailable`: screen absent from local inventory.

No delayed-read picture claim is fabricated. Target/commanded labels must be
rendered as destinations/commands, not “已确认正在显示”.

`holder_confidence` is known after local USB presence, an unambiguous two-device
departure, or trusted T2 holder input. Three-plus-device departure only infers the
next configured port. Inferred H disables close pushes and emits `holder_inferred`;
connections still restore. Unknown H is null with inferred confidence.

## Pending work and notifications (T2)

```json
{
  "v":1,
  "notification":"mixed.pending_remote",
  "data":{
    "group_id":"desk","revision":9,
    "section_meta":{"updated_at_ms":1792530000000,"updated_by":"pc"},
    "enabled":false,"local_device_id":"mac",
    "keyboard_holder_device_id":"mac","holder_confidence":"known",
    "pending_remote":[{"id":"mixed:[\"desk\",9,1792530000000,\"pc\",\"screen-b\",\"mac\"]","device_id":"pc","fingerprint":"screen-b","target_device_id":"mac"}],
    "superseded_remote":[{"id":"mixed:[\"desk\",8,1792529999000,\"pc\",\"screen-b\",\"pc\"]","device_id":null,"fingerprint":"screen-b","target_device_id":"pc","status":"superseded"}]
  }
}
```

IDs are `mixed:` plus the canonical JSON tuple
`[group_id,mixed.revision,sync.mixed.updated_at_ms,sync.mixed.updated_by,fingerprint,target_device_id]`.
Missing metadata uses timestamp 0 and empty author. IDs are stable for the complete
section version across retries/replans and safe for arbitrary delimiters. A newer
SectionMeta at the same numeric revision creates new IDs; it cannot reactivate
steps retired under the preceding section version. Active IDs are also excluded
from the bounded superseded history, so the two published sets remain disjoint. Null device_id
requires resolving the picture owner. Active work is only in pending_remote.
Replaced work moves to superseded_remote with status superseded (bounded last 32),
including revision/mode/target/USB-holder changes. Retired work must not execute.

**Immediately before each `display.push`, T2 must reread live H and confidence,
mode, mixed section version and active pending IDs.** During closing, require
known H equal to target, mixed disabled, receiver attached/enumerated and step ID
still active. Cancel/replace stale work; never dispatch a captured H blindly.
Enabling pending work instead checks the current assignment target. T1 does not
fabricate remote acknowledgements, timeouts or completion. T2 owns those.

`mixed.changed` contains `{mixed:<full state>,switch_group:<full snapshot>}` only
for local mixed edits; it is not emitted for remote apply or retry/alignment alone.
`state` updates remain available for all contexts, including hotplug and retry.

## Planner, topology and validation (T3)

`mixed::plan` is alignment-only; disabled mode restores connections without any
DDC/remote push. `plan_with_context(...,ReconcileContext::DisableTransition)` is
required only for an explicit enabled→disabled transition and its unfinished
retries. Enabled plans use fixed assignment; normal USB routing ignores fixed
monitors and only writes unassigned monitors (E8). Unknown target inputs cannot
be invented: normal routing still needs calibrated group inputs.

Plan steps remain `ddc_push{fingerprint,input}`, `detach{fingerprint}`,
`attach{fingerprint}`, `wait_enumeration`, and
`need_remote_push{id,device_id,fingerprint,target_device_id}`. Attach all required
screens, wait/refresh, then DDC; DDC before detach. Executor rereads and replans
once after enumeration. Failed restore/wait suppresses later local pushes/detach;
failed DDC suppresses that screen's detach. The last connected screen is retained,
and one is restored if a previous safe detach becomes the sole restorable screen.

```rust
pub trait DisplayTopology: Send {
    fn take_warning(&mut self) -> Option<String> { None }
    fn supported(&self) -> bool { true }
    fn simulation_link(&self) -> Option<SimulationTopologyLink> { None }
    fn list(&mut self) -> Result<Vec<TopologyDisplay>, CoreError>;
    fn detach(&mut self, fingerprint: &str) -> Result<(), CoreError>;
    fn attach(&mut self, fingerprint: &str) -> Result<(), CoreError>;
    fn wait_enumeration(&mut self) -> Result<(), CoreError>;
    fn restore_all(&mut self) -> Result<(), CoreError>;
}
```

Public module `kvmflow_sidecar::topology`; inject via `RuntimeOptions.topology`.
`TopologyDisplay{fingerprint,builtin,connected}` is unchanged. `list` includes
physically present app-detached screens; excludes unplugged/unavailable built-ins.
Wait must be bounded and make DDC handles usable. Crash restoration knowledge
belongs to T3. Startup aligns the saved enabled state; shutdown calls restore_all.

Production T3 adapters are selected after resolving the backend, unless
`RuntimeOptions.topology` was explicitly injected. macOS defaults on. Windows
always constructs its recovery adapter, while `windows-display-detach` or
`KVMFLOW_WINDOWS_DISPLAY_DETACH=1` enables new detach (`supported=false` otherwise).
The gate never disables inventory, attach or restore-all. Startup crash recovery
runs before saved-mode alignment; shutdown waits for an explicit recovery result.

Native query/operation failures use `E_TOPOLOGY_LIST`, `E_TOPOLOGY_ATTACH`,
`E_TOPOLOGY_DETACH`, `E_TOPOLOGY_ENUMERATION` and aggregate `E_TOPOLOGY_RESTORE`.
Specific safety/identity/persistence codes remain intact. Execution errors remain
`{code,message}`; platform diagnostic text is preserved. Failed enumeration keeps
pending fingerprints for the next wait/retry even after attach was confirmed.

`take_warning` consumes record-load diagnostics once. T3 lifecycle warnings use
stable codes `detach_records_unreadable` and `restore_failed`, with the standard
`severity:"warning", device_id:null, fingerprint:null` fields and an extra
`error:{code,message}` diagnostic. Exit/update timeouts are failures; the native
host cancels the action and reports them. See [hardware acceptance](DISPLAY_DETACH.md)
for adapter injection, recovery records, exact test commands and manual steps.
During release, both public mutations and trusted T2 apply requests return
`E_BACKEND` without executing; T2 must retry after resume. Installer cancellation
queues resume with the prior transient pause state and bounds its acknowledgement.

Noop reports supported=false, adds warning-level `topology_unavailable` to
validation, and never claims detach/attach success. Inventory fallback is only
for UI/validation; failed live list always blocks destructive actions.
Fake's simulation link makes DDC fail on detached or unenumerated screens;
operation logs record actual accepted input values with attach/wait/detach order.
Production adapters leave simulation_link as None.

Enable validation returns `{ok,reasons[]}`. Error codes:
`not_enough_screens`, `all_to_one_computer`, `not_enough_computers`,
`unknown_device`, `missing_input`. Warning codes include
`computer_without_screen`, `unassigned_monitor`, `topology_unavailable`.
Plan warnings include `last_local_screen`, `unknown_keyboard_holder`,
`holder_inferred`, `remote_push_pending`, plus execution failure codes.

`devices[].available_builtin_screens` is runtime observation: locally enumerated,
peers refreshed by T2; unknown counts conservatively as zero. All disk writes
remove it. Live snapshots may carry it; receiving peers must treat it as fresh
profile telemetry rather than resurrecting saved lid-open counts after restart.

## Migration boundaries

Schema remains v2. Armed v1 migration retains learned inputs and the v1 backup;
incomplete setup does not invent peers. Missing mixed migrates once to disabled,
revision 0. Layouts/arrival IDs remain archives; mixed is authoritative.

Choose the first non-default arrival policy in physical port order, otherwise the
first non-follow policy, otherwise the first device. Follow=true maps to that
device; keep/false maps to the other device for two computers. Three-plus old
computers with an unknown keep owner remain unassigned instead of guessed.

Review item 9 remains a recorded migration limitation: projection traverses the
chosen policy's monitor list. Another device's exclusive monitor remains
unassigned. Current group validation requires equal external monitor sets, so
supporting asymmetric per-device sets safely also needs profile/topology work;
T1 does not silently invent that missing mapping. Original policies remain in
the archive for assignment repair.

Runtime notification sinks execute on the hardware worker. T2 sinks should enqueue
work onto their coordinator rather than synchronously calling back into a blocking
RuntimeHandle method from the sink itself.

### Combined recovery and assignment saves

Saving an assignment while disabled does not complete an earlier close/recovery:
its enumeration flag, close intent, retry deadline and backoff remain active.
If close work has no scheduled local retry, the edit schedules reconciliation to
replace work invalidated by the new section version. The poller also checks
unfinished enumeration and failed execution when a deadline is missing; an
unchanged inventory cannot hide them. No recovery is scheduled for an ordinary
assignment-only save with no outstanding work.

Initial `GroupStable` only confirms USB baseline stability. If it reports the same
H already known when closing began, pending close work and its retry intent remain
active. Cancellation requires an actual `GroupLeft`/`GroupArrived` handoff or a
changed holder; a confidence/stability confirmation alone does not cancel work.


### T3 recovery override and fingerprint provenance

Native recovery records distinguish live DDC fingerprints from platform fallback
and record hints. A unique weak CG identity (zero numeric serial) is insufficient
without independent matching DDC evidence; reliable fingerprint conflict refuses
restore and retains the record. This is internal to T3, not a mixed config change.

After an exit timeout the host queues resume, including after a late release
success. A running worker consumes resume; a stopping worker cannot. A host
supervisor waits for actual completion and then recreates it over the same data
directory, even if cancellation's acknowledgement deadline has already expired.
Concurrent cancellation requests share one serialized replacement. Native
exit errors offer retry, keep running, or an explicit exit override. Update errors
also offer the existing Releases download page and a separately native-confirmed
install override. These overrides retain unconfirmed records for startup recovery;
install additionally atomically backs them up to `display-detach.before-update.json`.
Ordinary retries still require restoration. Closing/declining a prompt never
selects an override. Windows polling budgets do not interrupt synchronous Win32
calls; see DISPLAY_DETACH.md for the required readiness latency measurements.

### Desktop runtime generations

The desktop Host owns `runtime_generation`, a positive JavaScript-safe integer.
It starts at 1 for each Host process and strictly increases for every worker
rebuild. Failed startup attempts consume their number; IDs never wrap or get
reused within that Host. This is runtime metadata, not persisted mixed config or
a sidecar JSONL protocol change.

- Desktop `state.get`, `app.setEnabled`, `mixed.set` and `mixed.assign` success
  objects include `runtime_generation` alongside their existing fields.
- Every `runtime-notification` envelope includes `runtime_generation` alongside
  `kind` and `data`; every object payload also carries the same
  `data.runtime_generation`. Warning/error data preserve their existing fields.
- The host captures `(worker, generation)` together before RPC dispatch, and
  each notification sink captures its generation before worker startup. A late
  reply/event from an old worker keeps its **old** generation. The host ignores
  known stale callbacks for native status/notifications and rejects stale cache
  writes; events already queued for the renderer remain tagged with their origin.
- Native last_state and renderer compare `(runtime_generation, runtime_seq)`
  lexicographically per state field, accept a newer generation even when its
  sequence restarts at zero, reset cached fields, and discard older generations
  regardless of sequence. RuntimeSlot waits for the old worker to finish before
  replacement; every replacement gets a fresh generation and fresh seq domain.
  Generation ordering applies within one Host process; reconnecting to a new
  Host starts a new comparison session.

The `released_topology` request gate remains active while release is in effect,
including the install window: generation metadata grants no bypass for requests,
tray actions or trusted remote apply. An install failure waits for a stopping
worker's completion before replacement; it never overlaps two hardware workers.

Integration regressions use actual simulated workers and RuntimeSlot rebuilds,
then feed the same notification payloads through native Snapshot and the actual
renderer in jsdom. They cover immediate acceptance after rebuild and rejection
of delayed old state/error. The tray race regression schedules ReleaseDisplays
between the second installing check and mixed.set dispatch: the worker gate
rejects the queued mutation without persistence or simulated hardware writes;
resume restores normal operation. These are simulated tests, not OS display or
native tray stress acceptance.
