# Display detach: recovery contract and hardware acceptance

T3 uses the public `kvmflow_sidecar::topology::DisplayTopology` boundary. The
platform implementation lives under `backends/{macos,windows}/topology.rs`;
`detach/manager.rs` owns safety, persistence and recovery. This document is an
acceptance checklist, not a claim that hardware switching has been tested.

## Fixes and regression coverage

| Review item | Behavior | Regression guard |
| --- | --- | --- |
| P1.1 rollback failure | Keep the disk record unless attach and a subsequent live query confirm the same monitor is active; report both errors. | `failed_rollback_keeps_the_record_and_reports_both_errors`, `successful_attach_call_without_active_confirmation_keeps_the_record` |
| P1.2 query failure | Return an error, attempt recovery, retain the record if recovery cannot be confirmed. | `unverifiable_detach_is_restored`, `unverifiable_detach_with_failed_restore_keeps_the_record`, `restore_with_a_failing_query_keeps_the_record` |
| P1.3 exit | `shutdown_and_join` returns the actual restore result; timeout preserves the eventual result. Tauri prevents exit while recovery runs, queues resume on cancellation (or rebuilds a stopped worker), and offers Retry / Keep running / Exit anyway. | `display_restore.rs`: `shutdown_timeout_keeps_the_later_explicit_result`, `failed_restore_is_reported_and_the_adapter_stays_active` |
| P1.4 updater | Pause USB writes, release/restore displays and freeze hardware operations **before** calling the installer. This covers Windows updater 2.13.2's direct `process::exit(0)` and macOS's install-return/restart path. A failed installer resumes the worker. Persistent recovery failure also offers the existing Releases download page or a native-confirmed install override that preserves/backups records. | `updates.rs`: `installer_is_not_entered_until_displays_are_restored`, `timed_out_recovery_cancels_install_and_unfreezes_the_worker`, existing installation failure test |
| P1.5 Windows gate | Feature/env switch gates new detach only. Inventory, attach and crash recovery remain available with the switch off. | `disabled_adapter_refuses_detach_but_still_restores_records` |
| P1.6 identity | Resolve hardware identity/unique fingerprint again; never use a recycled CG number to select or disambiguate a screen. Legacy records need independent fingerprint evidence. Ambiguity keeps the record and refuses the operation. | `resolve.rs` and manager reused-key/ambiguity tests |
| P2.7 Windows single attach | Use current active paths and add only the requested target through a currently unused source from QDC_ALL_PATHS. Preserve current modes; never replay the full saved snapshot or Extend-all. Failure retains the record. | host-runnable `restore_one_target_preserves_live_changes_and_other_detaches`; Windows-only `restoring_one_path_does_not_replay_other_detached_or_stale_paths` (cross-compiled here) |
| P2.8 physical presence | Inventory includes physically present detached monitors; excludes unplugged and closed-lid built-in panels. Unplugging retains the recovery record. | `list_keeps_detached_but_drops_unplugged_and_closed_lid_builtin`, `unplugged_display_record_is_retained_until_replug` |
| P0 disabled display off the online list | On Darwin 25.6 arm64 a `CGSConfigureDisplayEnabled`-disabled display leaves `CGGetOnlineDisplayList` while its id stays configurable and answers identity queries. `enumerate` synthesizes the record's display from that id (present, inactive) so resolution finds it; `attach` enables unlisted ids after an identity gate that refuses only readable conflicts; `CGDisplayIsActive`'s -1 unknown-id answer is not "active". | `disabled_display_off_the_online_list_is_resynthesized_from_the_record`, `disabled_hint_display_rejects_every_unusable_id`, `conflicting_offline_identity_is_never_resynthesized`, `identity_triple_rejects_null_values`, `is_active_rejects_the_unknown_id_sentinel`, `disabled_offline_candidate_resolves_for_attach`, `restore_recovers_a_display_that_left_the_enumeration_list`, `restore_keeps_the_record_when_the_id_stops_answering` |

Additional guards cover last/only screen safety, reverse-order restart recovery,
DDC enumeration failure, mirror fallback as `connected=false`, a hung m1ddc
helper with a finite deadline, load warnings forwarded as `mixed.warning`,
operation-specific `E_TOPOLOGY_*` errors, and retained enumeration intent on retry.
Nine isolated mutations reproducing the reviewed failure modes were observed to
fail their targeted tests (P1.3 has separate failure-result and timeout probes).
These were fault-injection runs on a temporary copy, not hardware operations.

## Injection and lifecycle

- Explicit `RuntimeOptions.topology = Some(Box::new(adapter))` wins over defaults.
- With no explicit injection, `auto` resolves the actual backend, then the
  runtime factory selects macOS or Windows topology. macOS is enabled by default.
- Windows always constructs the recovery manager. New detach requires cargo
  feature `windows-display-detach` **or** `KVMFLOW_WINDOWS_DISPLAY_DETACH=1`.
  Removing the switch does not prevent recovery of an earlier record. The
  adapter reports `supported=false` for new mixed detaches while gated off.
- Simulated tests inject T1's `FakeDisplayTopology`; no native topology is
  constructed for the simulated backend. Fake's connected/enumerated link
  exercises the DDC ordering contract.
- Startup restores crash records before loading/reconciling saved mixed mode,
  even if DDC backend initialization subsequently fails. Saved mixed mode may
  then detach monitors again according to the current plan.
- `wait_enumeration` is bounded (macOS 8 s; Windows polling 10 s). Mixed runtime
  refreshes the real DDC backend after enumeration before further writes. Windows
  readiness additionally opens/closes each requested physical DDC handle; a GDI
  or registry entry alone is insufficient.
- `release_displays` restores first, then freezes topology, automatic polling
  and mutating runtime requests (including trusted T2 remote application) until
  `resume_displays`. A cancelled installer atomically queues resume with the old
  USB pause state; its acknowledgement is bounded to 5 s, with no extra 120 s RPC.
  The host waits up to
  45 s; timeout reports unconfirmed restoration. Explicit native confirmation can
  allow Exit anyway or Install anyway while preserving records; ordinary retry
  still requires recovery. Cancelled exit queues resume after any late release.
- `restore_all` tries every record, aggregates failures and returns `Err` if any
  restoration/readiness attempt fails. A physically absent monitor is skipped
  with its record retained; it is not falsely reported as an active desktop.
- Records live beside `config.json`, in `display-detach.json`. Corrupt files are
  preserved separately and reported as `mixed.warning` with a structured error.

Default desktop data directories: macOS `~/Library/Application Support/KVMFlow`,
Windows `%APPDATA%\KVMFlow`. `KVMFLOW_DATA_DIR` selects a desktop test directory.
For standalone sidecar, use `--config <test-dir>/config.json`; the record file is
in that same directory. Preserve the config and record file when collecting
failure evidence.

## macOS manual acceptance (not run by this change)

Use a test profile with at least two usable screens and a known keyboard route.
Do not start an unrelated production profile for these checks.

1. **Read-only inventory.** From the repository root run:
   ```sh
   cargo test --manifest-path sidecar/Cargo.toml -p kvmflow-sidecar macos_enumerate_real_displays -- --ignored --nocapture
   ```
   Expected: external fingerprints agree with DDC; an open built-in is listed.
   Recovery: none needed; this only queries the system.
2. **Controlled detach/attach.** Choose an external fingerprint and run:
   ```sh
   KVMFLOW_TEST_DETACH_FINGERPRINT='YOUR_FINGERPRINT' cargo test --manifest-path sidecar/Cargo.toml -p kvmflow-sidecar macos_detach_and_restore_real_display -- --ignored --nocapture
   ```
   Expected: the target leaves the extended desktop for about five seconds, a
   usable reserve remains, then the target returns; both results are `Ok`.
   On a restore failure the test prints full diagnostics (online ids, each
   id's identity/active state, the record's key and recorded identity, and
   whether the recorded id still answers the identity query) plus the manual
   recovery hint below.
   Recovery: the test always attempts restore even after detach failure. If a
   record remains it prints a retained directory; restart the desktop with
   `KVMFLOW_DATA_DIR` set to it. If the API fails, use the manual re-enable
   procedure below, or log out/in to reset forSession changes. Do not delete
   an unconfirmed record.
3. **Mirror fallback.** Repeat step 2 with `KVMFLOW_TEST_MIRROR=1` added to that
   one ignored test command. Expected: mirroring removes the target's independent
   desktop; normal mixed inventory reports `connected=false`; restore cancels
   mirroring. Recovery: as step 2, or disable mirroring in System Settings.
   This override exists only in the ignored test, not the production factory.
4. **Mixed mode and lid safety.** With a separate test configuration, enable
   mixed mode and assign an external screen elsewhere. Expected: DDC push precedes
   detach, and disabling mixed restores/enumerates before DDC. Close the laptop
   lid when only one external is usable: the last screen must remain attached.
   Recovery: open the lid, disable mixed, or Quit; log out if native recovery fails.
5. **Hotplug and identity.** While detached, unplug the target. Expected: it
   disappears from runtime inventory but assignment/record remain. Replug/restart:
   restore only the same uniquely identified monitor. Identical ambiguous monitors
   must produce an error and retain the record. Recovery: connect one identifiable
   target at a time, retry recovery; use System Settings/log out if still ambiguous.
6. **Quit/crash/update.** In the test profile verify tray Quit and Cmd-Q restore
   before process exit; a crash/restart restores records before saved mode aligns.
   With a test signed update, verify restoration before install/restart. Expected:
   recovery failure/timeout shows retry/continue/exit-anyway choices. Closing the
   dialog keeps running. The update error offers retry recovery and manual
   Releases download; Install anyway requires a separate native risk confirmation.
   Recovery: retry after reconnecting the target, restart against the same data
   directory, or log out/in. Preserve records and diagnostics on failure.

## Windows manual acceptance (not run by this change)

Use a local console session (not RDP), a WDDM driver, and at least two usable
screens. New detach remains opt-in pending this acceptance.

1. **Gate off and read-only inventory.** With the environment variable unset and
   no feature enabled, run:
   ```powershell
   cargo test --manifest-path sidecar/Cargo.toml -p kvmflow-sidecar windows_enumerate_real_displays -- --ignored --nocapture
   ```
   Expected: active/present monitors have stable device-path identities and
   fingerprints; production new detach is unsupported. Recovery: none needed.
2. **Controlled detach/attach.** Set a target fingerprint and run only the
   specifically named ignored test (it explicitly opts in):
   ```powershell
   $env:KVMFLOW_TEST_DETACH_FINGERPRINT = 'YOUR_FINGERPRINT'
   cargo test --manifest-path sidecar/Cargo.toml -p kvmflow-sidecar windows_detach_and_restore_real_monitor -- --ignored --nocapture
   ```
   Expected: only that path disappears for about five seconds, the reserve
   remains usable, then the target returns. Recovery: the test attempts restore
   and prints/preserves any remaining record directory; restart against that
   directory. Emergency recovery: Win+P → Extend, sign out/in, or reboot.
3. **Current-topology single attach.** In a desktop test profile set
   `KVMFLOW_WINDOWS_DISPLAY_DETACH=1`, detach two targets while retaining a third,
   then change only one target's mixed assignment back to the local computer.
   Expected: the other target stays detached and any intervening desktop layout
   change remains; unavailable independent source yields an error/kept record.
   Recovery: disable mixed or Quit for restore-all; Win+P → Extend if needed.
4. **Switch-off crash recovery.** In the same disposable profile leave a recorded
   detach by terminating the test app, then unset the environment switch and
   restart a build without the feature. Expected: startup still restores the
   recorded monitor; the gate rejects only new detaches. Recovery: same data
   directory, Win+P → Extend, or sign out/in. Keep the record until confirmation.
5. **Unplug/replug and closed lid.** Unplug an app-detached target, then restart.
   Expected: no target in inventory, record retained, other paths untouched.
   Replug it: only the matched device path is restored; a closed internal panel
   never counts as an available reserve. Recovery: reconnect target/open lid,
   disable mixed or Quit; Win+P → Extend if needed.
6. **Quit/update/error handling.** Verify tray Quit waits for restoration. With a
   disposable signed update, check restoration completes **before** the Windows
   installer starts and the old process exits. Force recovery failure in a test
   adapter: ordinary retry must launch no installer; exit offers Retry / Keep
   running / Exit anyway. Updates offer retry, manual Releases download, and
   Install anyway with a native risk prompt. Decline must not launch installer.
   Confirmed override must keep `display-detach.json` and create
   `display-detach.before-update.json`; the new version retries startup recovery. Recovery: reconnect/retry, launch
   the old build against the same data directory, or use Win+P → Extend/reboot.

Do not use `cargo test -- --ignored` without a specific test filter: it would
also select destructive hardware tests. Default suites skip them.

### macOS manual re-enable when restore fails (verified on Darwin 25.6 arm64)

A disabled external display stays dark and is absent from
`CGGetOnlineDisplayList`, so System Settings may not show it. The recovery
procedure used successfully in the 2026-10 P0 incident, in order:

1. **dlsym re-enable (works while the session is stuck).** In a small C/Swift
   tool, resolve `CGSConfigureDisplayEnabled` via
   `dlsym(RTLD_DEFAULT, "CGSConfigureDisplayEnabled")` (CoreGraphics
   re-exports SkyLight; open SkyLight explicitly as fallback), then run one
   configuration: `CGBeginDisplayConfiguration`, call
   `CGSConfigureDisplayEnabled(cfg, <recorded cg id>, true)`,
   `CGCompleteDisplayConfiguration(cfg, kCGConfigureForSession)`. The
   recorded id is the `cg:N` key in `display-detach.json`.
2. **Reposition.** If the layout was not restored, run another configuration
   with `CGConfigureDisplayOrigin(cfg, id, x, y)` per display using the
   origins saved in the record.
3. **`kvmprobe topo-restore`.** The probe (branch `probe/display-topology`,
   fixed commit "re-enable disabled displays that left the online list") now
   also re-enables offline ids that still answer the identity query. Earlier
   builds reported `restored:true` without doing anything.
4. **Last resorts.** Unplug/replug the display cable, or log out and back in
   (all changes are session-scoped); reboot if the session is wedged.

7. **Measure readiness latency.** The named ignored Windows detach probe in
   step 2 now prints `attach_ms`, `readiness_ms`, readiness result and final
   restore duration. It starts the readiness timer immediately after the first
   successful attach, before the first `wait_enumeration`, not after restore-all.
   Repeat cold start / warm reattach / sleep-wake / dock-replug runs, record
   monitor/GPU/driver, success/failure, and p50/p95/max across at least 20 runs
   per relevant setup. Expected: successful readiness means requested physical
   DDC handles opened; failed attempts remain explicit. Recovery: final
   restore-all is still attempted on every probe path; retain printed records
   and use Win+P → Extend or sign out/in if necessary. **Not measured here.**

### Timeout budgets and cancellation

Windows's 10-second polling budget is an engineering default for topology/DDC
settling, not a measured hardware SLA. The host's 45-second wait leaves room for
in-flight DDC work (m1ddc can take 15 s), individual monitor restoration (macOS
settling up to 8 s per operation) and enumeration (macOS 8 s / Windows nominal
10 s). Several monitors, retries or Windows driver delays can exceed it.

A synchronous Win32 call, including physical-handle opening, cannot be interrupted
by the polling deadline. Thus neither 10 s nor the 45 s host deadline bounds the
native call itself. Host timeout returns control to the UI; it does not stop the
worker. Exit cancellation queues resume so a late success does not leave the app
frozen. An already completed worker is recreated over the same data directory.
If a confirmed risky installer fails while the old worker is still stopping,
a supervisor continues waiting after the caller's timeout and rebuilds only
after actual completion. Concurrent cancellation requests share one replacement.
An indefinitely blocked native call still delays replacement; the supervisor
does not start a second hardware worker against the same recovery records.
The measured latency distribution above must inform any later budget changes.

### Host generation and integration acceptance

The Host assigns `runtime_generation` starting at 1 and increasing on each
worker rebuild (failed startup attempts also consume an ID). It never reuses
or wraps an ID within the process. Desktop `state.get`, `app.setEnabled`,
`mixed.set` and `mixed.assign` replies, all runtime notification envelopes and
object payloads expose the ID.
Each sink/reply captures its originating worker's generation, so delayed old
messages cannot masquerade as the replacement. Native status ignores stale
callbacks/cache writes. Native last_state and renderer now enforce
`(runtime_generation, runtime_seq)` ordering; see MIXED_PROTOCOL.md for field
locations. Actual simulated worker replacement is tested through native Snapshot
and jsdom, including its first mixed.changed and delayed old state/error.
The `released_topology` request gate is retained and the tray enqueue race has
a regression guard. Hardware acceptance below remains outstanding.

On **both macOS and Windows**, add this non-hardware regression to update
acceptance using an injected blocking topology and a failing test installer:

1. Block release, choose the explicit risk-install override, fail the installer
   before release returns, and let cancellation time out. Expected: old worker
   remains stopping; no replacement runs concurrently and recovery records remain.
2. Unblock old release after that timeout. Expected: exactly one replacement
   starts, generation increases, `state.get` and local/remote RPC work again.
   Deliver a queued old state/error afterwards: it retains the old generation
   and must not overwrite the new native status. Recovery: unblock the fake;
   if the native driver is hung instead, relaunch using the same data directory
   after the old process exits. No real detach/mirror is needed for this check.

### Review follow-up regression guards

- Same-model replacement: `same_model_replacement_with_conflicting_ddc_fingerprint_is_never_restored`
  failed before the fix. Fingerprint provenance distinguishes live DDC, platform
  fallback and record hints. A weak zero-numeric-serial CG identity cannot confirm
  a monitor using fallback/hint data; conflicting reliable DDC evidence refuses
  matching and keeps the original record.
- Cancelled exit: native host tests cover a release that succeeds after timeout
  and a worker that already stopped, plus a stopping worker after quick risky
  install failure. Four concurrent cancellations still produce exactly one
  replacement after completion. These cases previously left RPCs unusable.
- Generation: tests cover repeated rebuilds, failed startup consuming its ID,
  safe-integer exhaustion refusing wraparound, old notification provenance and
  stale cache rejection without changing existing state/error data.
- Exit choices: native result mapping distinguishes Retry / Keep running / Exit
  anyway in both languages; closing the dialog means Keep running. Explicit force
  exit preserves records and skips another automatic recovery attempt.
- Update alternatives: renderer tests cover retry without redownload, the existing
  `https://github.com/Kerw1n1209/KVMFlow/releases` download entry, explicit risky
  install and declined confirmation. Rust tests prove decline/backup failure does
  not invoke installer, and confirmed installation retains/backs up records even
  when the old worker has stopped. Backup failure leaves manual download available.

## Verification commands and limits

```sh
cargo test --manifest-path sidecar/Cargo.toml
cargo test --manifest-path desktop/src-tauri/Cargo.toml
cargo check --manifest-path sidecar/Cargo.toml --target x86_64-pc-windows-gnu --tests --features windows-display-detach
cargo check --manifest-path sidecar/Cargo.toml --target x86_64-pc-windows-msvc --tests
cd desktop
npm run smoke
npm test
```

Real detach/mirroring, native Quit dialogs, signed update installation and visible
picture changes remain hardware acceptance items. Windows native-only topology
tests are compile-checked on macOS; the shared restoration selector is executed
on the host. An OS force-kill cannot run application exit recovery; persisted
records and the session-only native configuration provide the recovery route.

Windows path/mode behavior follows Microsoft's
[SetDisplayConfig contract](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setdisplayconfig):
invalid new-path mode indices request best-mode selection; existing supplied
paths/modes are retained; `SDC_SAVE_TO_DATABASE` is never used.
