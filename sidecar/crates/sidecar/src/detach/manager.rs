//! Safety, persistence and recovery around a platform adapter. Implements the
//! mixed-mode core's `DisplayTopology` for any `PlatformTopology`.

use super::plan::plan_detach;
use super::resolve::{resolve_record, Resolved};
use super::store::RecordStore;
use crate::backends::topology::{DetachMethod, DetachRecord, PlatformTopology, SysDisplay};
use crate::topology::DisplayTopology;
use kvmflow_core::errors::{
    CoreError, E_TOPOLOGY_ATTACH, E_TOPOLOGY_DETACH, E_TOPOLOGY_ENUMERATION, E_TOPOLOGY_FAILED,
    E_TOPOLOGY_LAST_ACTIVE, E_TOPOLOGY_LIST, E_TOPOLOGY_NOT_FOUND, E_TOPOLOGY_RESTORE,
    E_TOPOLOGY_UNSUPPORTED,
};
use kvmflow_core::mixed::TopologyDisplay;
use std::path::PathBuf;

pub struct TopologyManager<P: PlatformTopology> {
    platform: P,
    store: RecordStore,
    records: Vec<DetachRecord>,
    /// New detaches are allowed. Restoring recorded detaches
    /// is always allowed.
    enabled: bool,
    /// Fingerprints attached since the last `wait_enumeration`.
    pending_ready: Vec<String>,
    load_warning: Option<String>,
}

enum Attached {
    Restored,
    /// The monitor is no longer connected; its record is retained until it is present again.
    Gone,
}

impl<P: PlatformTopology> TopologyManager<P> {
    pub fn new(platform: P, store_path: PathBuf, enabled: bool) -> Self {
        let store = RecordStore::new(store_path);
        let (records, warning) = store.load();
        Self {
            platform,
            store,
            records,
            enabled,
            pending_ready: Vec::new(),
            load_warning: warning,
        }
    }

    #[cfg(test)]
    pub fn pending_records(&self) -> &[DetachRecord] {
        &self.records
    }

    fn require_enabled(&self) -> Result<(), CoreError> {
        if self.enabled {
            Ok(())
        } else {
            Err(CoreError::new(
                E_TOPOLOGY_UNSUPPORTED,
                "display detach is disabled on this platform",
            ))
        }
    }

    /// The system's displays with the records applied, plus what each record
    /// resolves to. A record never changes the identity of a display it does
    /// not resolve to, so a reused platform key cannot relabel another screen.
    fn view(&mut self) -> Result<(Vec<SysDisplay>, Vec<Resolved>), CoreError> {
        let mut displays = self.platform.enumerate(&self.records)?;
        let mut status = Vec::with_capacity(self.records.len());
        for record in &self.records {
            let resolved = resolve_record(record, &displays);
            if let Resolved::Found(key) = &resolved {
                if let Some(d) = displays.iter_mut().find(|d| &d.key == key) {
                    if !d.active {
                        d.fingerprint = record.fingerprint.clone();
                    }
                }
            }
            status.push(resolved);
        }
        Ok((displays, status))
    }

    /// The screens that count for safety and for the inventory: connected, and
    /// not a built-in panel that is unusable (closed lid).
    fn usable(displays: Vec<SysDisplay>) -> Vec<SysDisplay> {
        displays
            .into_iter()
            .filter(|d| d.present && !(d.builtin && !d.active))
            .collect()
    }

    fn persist(&self) -> Result<(), CoreError> {
        self.store.save(&self.records)
    }

    fn drop_record(&mut self, fingerprint: &str) {
        self.records.retain(|r| r.fingerprint != fingerprint);
    }

    /// Undo a detach that was just made, using the key captured a moment ago.
    /// The record is removed only when the platform confirms the restore.
    fn roll_back(&mut self, record: &DetachRecord, cause: CoreError) -> CoreError {
        let restored = self
            .platform
            .attach(record, false)
            .and_then(|()| self.confirm_attached(record));
        match restored {
            Ok(()) => {
                self.drop_record(&record.fingerprint);
                if let Err(error) = self.persist() {
                    self.records.push(record.clone());
                    return CoreError::new(
                        cause.code,
                        format!("{}; {}", cause.message, error.message),
                    );
                }
                cause
            }
            Err(restore) => CoreError::new(
                cause.code,
                format!(
                    "{}; restoring {} could not be confirmed ({}), record kept",
                    cause.message, record.fingerprint, restore.message
                ),
            ),
        }
    }

    fn confirm_attached(&mut self, record: &DetachRecord) -> Result<(), CoreError> {
        let displays = self.platform.enumerate(&self.records)?;
        match resolve_record(record, &displays) {
            Resolved::Found(key)
                if displays
                    .iter()
                    .any(|d| d.key == key && d.active && d.present) =>
            {
                Ok(())
            }
            _ => Err(CoreError::new(
                E_TOPOLOGY_FAILED,
                "restored monitor is not confirmed active; record kept",
            )),
        }
    }

    fn attach_record(&mut self, record: &DetachRecord) -> Result<Attached, CoreError> {
        let (displays, status) = self.view()?;
        let index = self
            .records
            .iter()
            .position(|r| r.fingerprint == record.fingerprint)
            .ok_or_else(|| CoreError::new(E_TOPOLOGY_NOT_FOUND, "record vanished"))?;
        let key = match &status[index] {
            Resolved::Found(key) => key.clone(),
            Resolved::Gone => {
                return Ok(Attached::Gone);
            }
            Resolved::Unconfirmed => {
                return Err(CoreError::new(
                    E_TOPOLOGY_FAILED,
                    "monitor identity is weak or its DDC fingerprint conflicts; record kept",
                ))
            }
            Resolved::Ambiguous => {
                return Err(CoreError::new(
                    E_TOPOLOGY_FAILED,
                    format!(
                        "cannot tell which monitor is {}: several identical monitors; record kept",
                        record.fingerprint
                    ),
                ))
            }
        };
        let mut target = record.clone();
        target.identity = displays
            .iter()
            .find(|d| d.key == key)
            .ok_or_else(|| CoreError::new(E_TOPOLOGY_FAILED, "resolved monitor disappeared"))?
            .identity
            .clone();
        target.key = key;
        self.platform.attach(&target, false)?;
        self.confirm_attached(&target)?;
        self.drop_record(&record.fingerprint);
        if let Err(error) = self.persist() {
            self.records.push(record.clone());
            return Err(error);
        }
        if !self.pending_ready.contains(&record.fingerprint) {
            self.pending_ready.push(record.fingerprint.clone());
        }
        Ok(Attached::Restored)
    }
}

fn operation_error(error: CoreError, operation: &'static str) -> CoreError {
    if error.code == E_TOPOLOGY_FAILED {
        CoreError::new(operation, error.message)
    } else {
        error
    }
}
impl<P: PlatformTopology> TopologyManager<P> {
    fn detach_display(&mut self, fingerprint: &str) -> Result<(), CoreError> {
        self.require_enabled()?;
        let (displays, _) = self.view()?;
        let displays = Self::usable(displays);
        let Some(plan) = plan_detach(&displays, fingerprint)? else {
            return Ok(());
        };
        let (target, reserve) = (plan.target.clone(), plan.reserve.clone());
        let mut record = self.platform.capture(&target, &displays)?;
        record.method = DetachMethod::Pending;
        record.fingerprint = fingerprint.into();
        record.identity = target.identity.clone();

        // Persist first: if the process dies mid-call, the next start restores.
        let previous = self
            .records
            .iter()
            .find(|r| r.fingerprint == fingerprint)
            .cloned();
        self.drop_record(fingerprint);
        self.records.push(record.clone());
        if let Err(e) = self.persist() {
            self.drop_record(fingerprint);
            if let Some(previous) = previous {
                self.records.push(previous);
            }
            return Err(e);
        }
        let method = match self.platform.detach(&record, &reserve) {
            Ok(method) => method,
            // The call may have changed part of the layout (main display).
            Err(e) => return Err(self.roll_back(&record, e)),
        };
        // Last line of defence: never leave the user without a screen, and
        // never assume safety when the system cannot be asked.
        match self.platform.enumerate(&self.records) {
            Err(e) => {
                let cause = CoreError::new(
                    E_TOPOLOGY_FAILED,
                    format!(
                        "could not verify the screens after detaching: {}",
                        e.message
                    ),
                );
                return Err(self.roll_back(&record, cause));
            }
            Ok(all) => {
                let others_active = all.iter().any(|d| {
                    d.active
                        && d.present
                        && d.key != target.key
                        && !(method == DetachMethod::Mirrored && d.key == record.key)
                });
                if !others_active {
                    let cause = CoreError::new(
                        E_TOPOLOGY_LAST_ACTIVE,
                        "no active display remained after detaching",
                    );
                    return Err(self.roll_back(&record, cause));
                }
            }
        }
        if let Some(r) = self
            .records
            .iter_mut()
            .find(|r| r.fingerprint == fingerprint)
        {
            r.method = method;
        }
        // The Pending record is already on disk; it restores just the same.
        let _ = self.persist();
        Ok(())
    }

    fn attach_display(&mut self, fingerprint: &str) -> Result<(), CoreError> {
        let Some(record) = self
            .records
            .iter()
            .find(|r| r.fingerprint == fingerprint)
            .cloned()
        else {
            let (displays, _) = self.view()?;
            let displays = Self::usable(displays);
            return match displays.iter().find(|d| d.fingerprint == fingerprint) {
                Some(d) if d.active => Ok(()),
                Some(_) => Err(CoreError::new(
                    E_TOPOLOGY_NOT_FOUND,
                    format!("display {fingerprint} is off, but not detached by this app"),
                )),
                None => Err(CoreError::new(
                    E_TOPOLOGY_NOT_FOUND,
                    format!("no display with fingerprint {fingerprint}"),
                )),
            };
        };
        match self.attach_record(&record)? {
            Attached::Restored => Ok(()),
            Attached::Gone => Err(CoreError::new(
                E_TOPOLOGY_NOT_FOUND,
                format!("monitor {fingerprint} is no longer connected"),
            )),
        }
    }
}

impl<P: PlatformTopology> DisplayTopology for TopologyManager<P> {
    fn supported(&self) -> bool {
        self.enabled
    }
    fn take_warning(&mut self) -> Option<String> {
        self.load_warning.take()
    }
    fn list(&mut self) -> Result<Vec<TopologyDisplay>, CoreError> {
        let (displays, _) = self
            .view()
            .map_err(|e| operation_error(e, E_TOPOLOGY_LIST))?;
        Ok(Self::usable(displays)
            .into_iter()
            .map(|d| TopologyDisplay {
                fingerprint: d.fingerprint,
                builtin: d.builtin,
                connected: d.active,
            })
            .collect())
    }

    fn detach(&mut self, fingerprint: &str) -> Result<(), CoreError> {
        self.detach_display(fingerprint)
            .map_err(|e| operation_error(e, E_TOPOLOGY_DETACH))
    }
    fn attach(&mut self, fingerprint: &str) -> Result<(), CoreError> {
        self.attach_display(fingerprint)
            .map_err(|e| operation_error(e, E_TOPOLOGY_ATTACH))
    }
    fn wait_enumeration(&mut self) -> Result<(), CoreError> {
        self.platform
            .wait_ready(&self.pending_ready)
            .map_err(|e| operation_error(e, E_TOPOLOGY_ENUMERATION))?;
        self.pending_ready.clear();
        Ok(())
    }

    fn restore_all(&mut self) -> Result<(), CoreError> {
        let mut failed: Vec<String> = Vec::new();
        // Undo in reverse detachment order; each platform still uses live
        // topology and restores only the requested monitor.
        let records: Vec<DetachRecord> = self.records.iter().rev().cloned().collect();
        for record in records {
            if let Err(e) = self.attach_record(&record) {
                failed.push(format!("{}: {}", record.fingerprint, e.message));
            }
        }
        if !self.pending_ready.is_empty() {
            if let Err(e) = self.wait_enumeration() {
                failed.push(format!("restored displays not ready: {}", e.message));
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(CoreError::new(E_TOPOLOGY_RESTORE, failed.join("; ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detach::plan::display;
    use kvmflow_core::errors::E_TOPOLOGY_ONLY_DISPLAY;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct World {
        displays: Vec<SysDisplay>,
        calls: Vec<String>,
        attach_identities: Vec<String>,
        fail_detach: bool,
        fail_attach: bool,
        fail_enumerate: bool,
        /// Fail `enumerate` only after the first `detach` call.
        fail_enumerate_after_detach: bool,
        detach_called: bool,
        /// Simulates a platform that blanks everything on detach.
        blank_all_on_detach: bool,
        not_ready: bool,
        attach_without_effect: bool,
        /// Keys removed from the raw enumeration; like a CGS-disabled id, they
        /// are only re-listed when a pending record claims them (hint path).
        offline_keys: std::collections::BTreeSet<String>,
    }

    #[derive(Clone)]
    struct Fake(Arc<Mutex<World>>);

    impl Fake {
        fn new(displays: Vec<SysDisplay>) -> Self {
            Fake(Arc::new(Mutex::new(World {
                displays,
                ..Default::default()
            })))
        }
        fn calls(&self) -> Vec<String> {
            self.0.lock().unwrap().calls.clone()
        }
        fn set_active(&self, key: &str, active: bool) {
            for d in &mut self.0.lock().unwrap().displays {
                if d.key == key {
                    d.active = active;
                }
            }
        }
        fn with(&self, f: impl FnOnce(&mut World)) {
            f(&mut self.0.lock().unwrap());
        }
    }

    impl PlatformTopology for Fake {
        fn enumerate(&mut self, hints: &[DetachRecord]) -> Result<Vec<SysDisplay>, CoreError> {
            let w = self.0.lock().unwrap();
            if w.fail_enumerate || (w.fail_enumerate_after_detach && w.detach_called) {
                return Err(CoreError::new(E_TOPOLOGY_FAILED, "query failed"));
            }
            // Raw list: offline keys are invisible, like CGS-disabled ids in
            // CGGetOnlineDisplayList.
            let mut displays: Vec<SysDisplay> = w
                .displays
                .clone()
                .into_iter()
                .filter(|d| !w.offline_keys.contains(&d.key))
                .collect();
            // Hint path mirrors macOS: a record's offline key whose recorded
            // identity still matches the id's answerable identity is
            // synthesized back (present, inactive); otherwise it stays absent
            // like an unplugged monitor.
            for record in hints {
                if displays.iter().any(|d| d.key == record.key) {
                    continue;
                }
                let Some(held) = w.displays.iter().find(|d| {
                    d.key == record.key
                        && !record.identity.is_empty()
                        && d.identity == record.identity
                }) else {
                    continue;
                };
                displays.push(SysDisplay {
                    key: held.key.clone(),
                    fingerprint: record.fingerprint.clone(),
                    fingerprint_source: crate::backends::topology::FingerprintSource::RecordHint,
                    identity: held.identity.clone(),
                    builtin: held.builtin,
                    active: false,
                    is_main: false,
                    present: true,
                });
            }
            Ok(displays)
        }
        fn capture(
            &mut self,
            target: &SysDisplay,
            _all: &[SysDisplay],
        ) -> Result<DetachRecord, CoreError> {
            Ok(DetachRecord {
                fingerprint_source: target.fingerprint_source,
                fingerprint: target.fingerprint.clone(),
                key: target.key.clone(),
                identity: target.identity.clone(),
                method: DetachMethod::Pending,
                mirror_of: None,
                origins: vec![],
                topology: None,
                detached_at_ms: 1,
            })
        }
        fn detach(
            &mut self,
            record: &DetachRecord,
            reserve: &SysDisplay,
        ) -> Result<DetachMethod, CoreError> {
            let mut w = self.0.lock().unwrap();
            w.detach_called = true;
            w.calls
                .push(format!("detach {} keep {}", record.key, reserve.key));
            if w.fail_detach {
                return Err(CoreError::new(E_TOPOLOGY_FAILED, "boom"));
            }
            if w.blank_all_on_detach {
                for d in &mut w.displays {
                    d.active = false;
                }
            }
            for d in &mut w.displays {
                if d.key == record.key {
                    d.active = false;
                }
            }
            Ok(DetachMethod::Disabled)
        }
        fn attach(&mut self, record: &DetachRecord, _allow_extend: bool) -> Result<(), CoreError> {
            let mut w = self.0.lock().unwrap();
            w.calls.push(format!("attach {}", record.key));
            w.attach_identities.push(record.identity.clone());
            if w.fail_attach {
                return Err(CoreError::new(E_TOPOLOGY_FAILED, "stuck"));
            }
            if w.attach_without_effect {
                return Ok(());
            }
            // The enable brings a disabled id back into the raw list.
            w.offline_keys.remove(&record.key);
            match w.displays.iter_mut().find(|d| d.key == record.key) {
                Some(d) => {
                    d.active = true;
                    Ok(())
                }
                None => Err(CoreError::new(E_TOPOLOGY_NOT_FOUND, "gone")),
            }
        }
        fn wait_ready(&mut self, fps: &[String]) -> Result<(), CoreError> {
            let mut w = self.0.lock().unwrap();
            w.calls.push(format!("wait_ready {}", fps.join(",")));
            if w.not_ready {
                return Err(CoreError::new(E_TOPOLOGY_FAILED, "not enumerated"));
            }
            Ok(())
        }
    }

    fn setup() -> (Fake, TopologyManager<Fake>, tempfile::TempDir) {
        let fake = Fake::new(vec![
            display("1", "EXT-A", false, true, true),
            display("2", "EXT-B", false, true, false),
            display("3", "BUILTIN", true, true, false),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let manager = TopologyManager::new(fake.clone(), dir.path().join("d.json"), true);
        (fake, manager, dir)
    }

    fn file(dir: &tempfile::TempDir) -> std::path::PathBuf {
        dir.path().join("d.json")
    }

    #[test]
    fn list_reports_connected_state() {
        let (_, mut m, _dir) = setup();
        let list = m.list().unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(
            list[2],
            TopologyDisplay {
                fingerprint: "BUILTIN".into(),
                builtin: true,
                connected: true
            }
        );
    }

    #[test]
    fn list_keeps_detached_but_drops_unplugged_and_closed_lid_builtin() {
        let (fake, mut m, _dir) = setup();
        m.detach("EXT-A").unwrap();
        // EXT-B is unplugged, the built-in lid is closed.
        fake.with(|w| {
            for d in &mut w.displays {
                if d.key == "2" {
                    d.present = false;
                    d.active = false;
                }
                if d.key == "3" {
                    d.active = false;
                }
            }
        });
        let list = m.list().unwrap();
        assert_eq!(
            list,
            vec![TopologyDisplay {
                fingerprint: "EXT-A".into(),
                builtin: false,
                connected: false
            }]
        );
    }

    #[test]
    fn detach_persists_then_attach_clears() {
        let (fake, mut m, dir) = setup();
        m.detach("EXT-A").unwrap();
        assert_eq!(fake.calls(), vec!["detach 1 keep 3"]);
        assert!(file(&dir).exists());
        let list = m.list().unwrap();
        assert!(
            !list
                .iter()
                .find(|d| d.fingerprint == "EXT-A")
                .unwrap()
                .connected
        );

        // Detaching again is a no-op, not a second system call.
        m.detach("EXT-A").unwrap();
        assert_eq!(fake.calls().len(), 1);

        m.attach("EXT-A").unwrap();
        assert!(!file(&dir).exists());
        m.attach("EXT-A").unwrap(); // already attached
    }

    #[test]
    fn last_screen_is_never_detached() {
        let (fake, mut m, _dir) = setup();
        m.detach("EXT-A").unwrap();
        m.detach("EXT-B").unwrap();
        // Only the built-in is left active; the app-detached EXT-A comes back
        // by itself (e.g. replug), the built-in lid closes.
        fake.set_active("3", false);
        fake.set_active("1", true);
        let calls_before = fake.calls().len();
        let err = m.detach("EXT-A").unwrap_err();
        assert_eq!(err.code, E_TOPOLOGY_LAST_ACTIVE);
        assert_eq!(fake.calls().len(), calls_before, "no system call was made");
    }

    #[test]
    fn only_display_is_refused() {
        let fake = Fake::new(vec![display("1", "EXT-A", false, true, true)]);
        let dir = tempfile::tempdir().unwrap();
        let mut m = TopologyManager::new(fake.clone(), file(&dir), true);
        assert_eq!(m.detach("EXT-A").unwrap_err().code, E_TOPOLOGY_ONLY_DISPLAY);
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn failed_detach_rolls_back_and_leaves_no_record() {
        let (fake, mut m, dir) = setup();
        fake.with(|w| w.fail_detach = true);
        assert_eq!(m.detach("EXT-A").unwrap_err().code, E_TOPOLOGY_DETACH);
        assert_eq!(fake.calls(), vec!["detach 1 keep 3", "attach 1"]);
        assert!(m.pending_records().is_empty());
        assert!(!file(&dir).exists());
    }

    // Review P1.1: a failed rollback must keep the record.
    #[test]
    fn failed_rollback_keeps_the_record_and_reports_both_errors() {
        let (fake, mut m, dir) = setup();
        fake.with(|w| {
            w.fail_detach = true;
            w.fail_attach = true;
        });
        let err = m.detach("EXT-A").unwrap_err();
        assert!(
            err.message.contains("boom") && err.message.contains("stuck"),
            "{err}"
        );
        assert_eq!(m.pending_records().len(), 1);
        assert!(file(&dir).exists());
        // The kept record lets a later restore succeed.
        fake.with(|w| w.fail_attach = false);
        m.restore_all().unwrap();
        assert!(!file(&dir).exists());
    }

    #[test]
    fn detach_that_blanks_everything_is_undone() {
        let (fake, mut m, dir) = setup();
        fake.with(|w| w.blank_all_on_detach = true);
        let err = m.detach("EXT-A").unwrap_err();
        assert_eq!(err.code, E_TOPOLOGY_LAST_ACTIVE);
        assert!(fake.calls().iter().any(|c| c == "attach 1"));
        assert!(m.pending_records().is_empty());
        assert!(!file(&dir).exists());
    }

    #[test]
    fn blanked_screens_and_failed_rollback_keep_the_record() {
        let (fake, mut m, dir) = setup();
        fake.with(|w| {
            w.blank_all_on_detach = true;
            w.fail_attach = true;
        });
        assert!(m.detach("EXT-A").is_err());
        assert_eq!(m.pending_records().len(), 1);
        assert!(file(&dir).exists());
    }

    // Review P1.2: a failing verification query is not "other screens are fine".
    #[test]
    fn unverifiable_detach_is_restored() {
        let (fake, mut m, dir) = setup();
        fake.with(|w| w.fail_enumerate_after_detach = true);
        let err = m.detach("EXT-A").unwrap_err();
        assert!(err.message.contains("could not verify"), "{err}");
        assert!(fake.calls().iter().any(|c| c == "attach 1"));
        assert_eq!(m.pending_records().len(), 1);
        assert!(file(&dir).exists());
        fake.with(|w| w.fail_enumerate_after_detach = false);
        m.restore_all().unwrap();
        assert!(!file(&dir).exists());
    }

    #[test]
    fn unverifiable_detach_with_failed_restore_keeps_the_record() {
        let (fake, mut m, dir) = setup();
        fake.with(|w| {
            w.fail_enumerate_after_detach = true;
            w.fail_attach = true;
        });
        assert!(m.detach("EXT-A").is_err());
        assert_eq!(m.pending_records().len(), 1);
        assert!(file(&dir).exists());
    }

    #[test]
    fn restore_all_reverses_order_waits_and_survives_restart() {
        let (fake, mut m, dir) = setup();
        m.detach("EXT-A").unwrap();
        m.detach("EXT-B").unwrap();

        // Crash: a new manager over the same file knows what to restore.
        let mut after_crash = TopologyManager::new(fake.clone(), file(&dir), true);
        assert_eq!(after_crash.pending_records().len(), 2);
        after_crash.restore_all().unwrap();
        let calls = fake.calls();
        let tail: Vec<&str> = calls.iter().skip(2).map(String::as_str).collect();
        assert_eq!(tail, ["attach 2", "attach 1", "wait_ready EXT-B,EXT-A"]);
        assert!(fake.0.lock().unwrap().displays.iter().all(|d| d.active));
        assert!(!file(&dir).exists());
    }

    #[test]
    fn restore_all_reports_displays_that_are_not_ready() {
        let (fake, mut m, _dir) = setup();
        m.detach("EXT-A").unwrap();
        fake.with(|w| w.not_ready = true);
        let err = m.restore_all().unwrap_err();
        assert!(err.message.contains("not ready"), "{err}");
    }

    #[test]
    fn failed_restore_keeps_the_record_for_the_next_try() {
        let (fake, mut m, dir) = setup();
        m.detach("EXT-A").unwrap();
        fake.with(|w| w.fail_attach = true);
        assert!(m.restore_all().is_err());
        assert!(file(&dir).exists());
        fake.with(|w| w.fail_attach = false);
        m.restore_all().unwrap();
        assert!(!file(&dir).exists());
    }

    #[test]
    fn restore_with_a_failing_query_keeps_the_record() {
        let (fake, mut m, dir) = setup();
        m.detach("EXT-A").unwrap();
        fake.with(|w| w.fail_enumerate = true);
        assert!(m.restore_all().is_err());
        assert!(file(&dir).exists());
    }

    #[test]
    fn unplugged_display_record_is_retained_until_replug() {
        let (fake, mut m, dir) = setup();
        m.detach("EXT-A").unwrap();
        fake.with(|w| w.displays.retain(|d| d.key != "1"));
        assert!(m.list().unwrap().iter().all(|d| d.fingerprint != "EXT-A"));
        m.restore_all().unwrap();
        assert!(file(&dir).exists());
        fake.with(|w| w.displays.push(display("1", "EXT-A", false, false, false)));
        m.restore_all().unwrap();
        assert!(!file(&dir).exists());
    }

    // P0 regression (Darwin 25.6 arm64): CGSConfigureDisplayEnabled takes the
    // display out of the online list entirely, so the platform's enumerate
    // must synthesize the record's display from its still-answerable id. The
    // fake models exactly that: after detach, key "1" is absent from the raw
    // list but the hint path re-lists it, restore re-attaches and clears the
    // record. Before the fix the manager saw "not enumerated" and silently
    // kept the record while the screen stayed dark.
    #[test]
    fn restore_recovers_a_display_that_left_the_enumeration_list() {
        let (fake, mut m, dir) = setup();
        m.detach("EXT-A").unwrap();
        // CGS-disable: the id answers identity queries but is no longer
        // enumerated (active=false, and the fake's raw list drops it).
        fake.with(|w| {
            if let Some(d) = w.displays.iter_mut().find(|d| d.key == "1") {
                d.active = false;
            }
            w.offline_keys.insert("1".into());
        });
        let list = m.list().unwrap();
        // Inventory still shows it as a detached-but-present monitor.
        assert_eq!(
            list.iter().find(|d| d.fingerprint == "EXT-A").unwrap(),
            &TopologyDisplay {
                fingerprint: "EXT-A".into(),
                builtin: false,
                connected: false
            }
        );
        m.restore_all().unwrap();
        assert!(fake.calls().iter().any(|c| c == "attach 1"));
        assert!(fake.0.lock().unwrap().displays.iter().all(|d| d.active));
        assert!(!file(&dir).exists());
    }

    // P0 regression companion: when the id is gone with no readable identity
    // (truly unplugged), the record stays and nothing is touched.
    #[test]
    fn restore_keeps_the_record_when_the_id_stops_answering() {
        let (fake, mut m, dir) = setup();
        m.detach("EXT-A").unwrap();
        fake.with(|w| {
            w.displays.retain(|d| d.key != "1");
            w.offline_keys.insert("1".into());
        });
        m.restore_all().unwrap();
        assert!(!fake.calls().iter().any(|c| c.starts_with("attach")));
        assert!(file(&dir).exists());
        assert_eq!(m.pending_records().len(), 1);
    }

    // Review P1.6: keys can be reassigned between runs.
    #[test]
    fn reused_key_never_restores_or_relabels_the_wrong_screen() {
        let (fake, mut m, dir) = setup();
        m.detach("EXT-A").unwrap();
        // Reboot: the old EXT-A (key 1) is gone, its key now belongs to a new
        // monitor "NEW" and EXT-A came back as key 9, still off.
        fake.with(|w| {
            w.displays.retain(|d| d.key != "1");
            let mut new = display("1", "NEW", false, true, true);
            new.identity = "hw-new".into();
            w.displays.push(new);
            let mut a = display("9", "ignored", false, false, false);
            a.fingerprint_source = crate::backends::topology::FingerprintSource::Fallback;
            a.identity = "1".into(); // same hardware identity as the old key-1 screen
            w.displays.push(a);
        });
        let list = m.list().unwrap();
        // The new monitor keeps its own fingerprint; the detached one is EXT-A.
        assert!(list.iter().any(|d| d.fingerprint == "NEW" && d.connected));
        assert!(list
            .iter()
            .any(|d| d.fingerprint == "EXT-A" && !d.connected));
        m.restore_all().unwrap();
        let calls = fake.calls();
        assert!(calls.iter().any(|c| c == "attach 9"));
        assert!(!calls.iter().any(|c| c == "attach 1"), "{calls:?}");
        assert!(!file(&dir).exists());
    }

    #[test]
    fn reused_key_with_the_monitor_unplugged_keeps_the_record_without_touching_the_new_screen() {
        let (fake, mut m, dir) = setup();
        m.detach("EXT-A").unwrap();
        fake.with(|w| {
            w.displays.retain(|d| d.key != "1");
            let mut new = display("1", "NEW", false, true, true);
            new.identity = "hw-new".into();
            w.displays.push(new);
        });
        m.restore_all().unwrap();
        assert!(!fake.calls().iter().any(|c| c == "attach 1"));
        assert!(file(&dir).exists());
    }

    #[test]
    fn legacy_fingerprint_resolution_supplies_live_identity_to_the_platform() {
        let (fake, mut m, _dir) = setup();
        m.detach("EXT-A").unwrap();
        // A v1 record only has the DDC fingerprint. The CG number changed.
        m.records[0].identity.clear();
        m.persist().unwrap();
        fake.with(|w| {
            let a = w.displays.iter_mut().find(|d| d.key == "1").unwrap();
            a.key = "9".into();
        });
        m.restore_all().unwrap();
        assert!(fake.calls().iter().any(|c| c == "attach 9"));
        assert_eq!(fake.0.lock().unwrap().attach_identities, ["1"]);
    }

    #[test]
    fn same_model_replacement_with_conflicting_ddc_fingerprint_is_never_restored() {
        let (fake, mut m, dir) = setup();
        m.detach("EXT-A").unwrap();
        fake.with(|w| {
            w.displays.retain(|d| d.key != "1");
            let mut replacement = display("9", "EXT-B-AN-SERIAL", false, true, false);
            replacement.identity = "1".into(); // Same model and zero numeric serial.
            w.displays.push(replacement);
        });
        assert!(m.restore_all().is_err());
        assert!(file(&dir).exists());
        assert_eq!(m.pending_records().len(), 1);
        assert!(!fake.calls().iter().any(|call| call.starts_with("attach")));
    }

    #[test]
    fn ambiguous_identity_refuses_and_keeps_the_record() {
        let (fake, mut m, dir) = setup();
        m.detach("EXT-A").unwrap();
        fake.with(|w| {
            w.displays.retain(|d| d.key != "1");
            for key in ["7", "8"] {
                let mut twin = display(key, "twin", false, key == "7", false);
                twin.fingerprint_source = crate::backends::topology::FingerprintSource::Fallback;
                twin.identity = "1".into();
                w.displays.push(twin);
            }
        });
        let err = m.restore_all().unwrap_err();
        assert!(err.message.contains("several identical"), "{err}");
        assert!(file(&dir).exists());
        assert!(!fake.calls().iter().any(|c| c.starts_with("attach")));
    }

    // Review P1.5: the disabled switch limits new detaches, not recovery.
    #[test]
    fn disabled_adapter_refuses_detach_but_still_restores_records() {
        let (fake, mut m, dir) = setup();
        m.detach("EXT-A").unwrap();
        let mut disabled = TopologyManager::new(fake.clone(), file(&dir), false);
        assert_eq!(disabled.pending_records().len(), 1);
        assert!(!disabled.supported());
        assert_eq!(
            disabled.detach("EXT-B").unwrap_err().code,
            E_TOPOLOGY_UNSUPPORTED
        );
        assert_eq!(disabled.list().unwrap().len(), 3);
        disabled.restore_all().unwrap();
        assert!(!file(&dir).exists());
        assert!(fake.0.lock().unwrap().displays.iter().all(|d| d.active));
    }

    #[test]
    fn successful_attach_call_without_active_confirmation_keeps_the_record() {
        let (fake, mut m, dir) = setup();
        m.detach("EXT-A").unwrap();
        fake.with(|w| w.attach_without_effect = true);
        assert!(m.attach("EXT-A").is_err());
        assert!(m.restore_all().is_err());
        assert_eq!(m.pending_records().len(), 1);
        assert!(file(&dir).exists());
    }

    #[test]
    fn load_failure_is_consumed_as_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(file(&dir), "not json").unwrap();
        let mut manager = TopologyManager::new(Fake::new(vec![]), file(&dir), true);
        assert!(manager.take_warning().unwrap().contains("unreadable"));
        assert!(manager.take_warning().is_none());
    }

    #[test]
    fn native_failures_preserve_operation_codes_for_mixed_execution() {
        let (fake, mut m, _dir) = setup();
        fake.with(|w| w.fail_enumerate = true);
        assert_eq!(m.list().unwrap_err().code, E_TOPOLOGY_LIST);
        assert_eq!(m.detach("EXT-A").unwrap_err().code, E_TOPOLOGY_DETACH);
        assert_eq!(m.attach("EXT-A").unwrap_err().code, E_TOPOLOGY_ATTACH);
        fake.with(|w| w.fail_enumerate = false);
        m.detach("EXT-A").unwrap();
        fake.with(|w| w.fail_attach = true);
        assert_eq!(m.attach("EXT-A").unwrap_err().code, E_TOPOLOGY_ATTACH);
        assert_eq!(m.restore_all().unwrap_err().code, E_TOPOLOGY_RESTORE);
        fake.with(|w| {
            w.fail_attach = false;
            w.not_ready = true;
        });
        m.attach("EXT-A").unwrap();
        assert_eq!(
            m.wait_enumeration().unwrap_err().code,
            E_TOPOLOGY_ENUMERATION
        );
    }

    #[test]
    fn attach_without_record_distinguishes_cases() {
        let (fake, mut m, _dir) = setup();
        m.attach("EXT-A").unwrap();
        fake.set_active("2", false); // off, but not by us
        assert_eq!(m.attach("EXT-B").unwrap_err().code, E_TOPOLOGY_NOT_FOUND);
        assert_eq!(m.attach("NOPE").unwrap_err().code, E_TOPOLOGY_NOT_FOUND);
    }

    #[test]
    fn failed_enumeration_keeps_the_restored_fingerprint_for_retry() {
        let (fake, mut m, _dir) = setup();
        m.detach("EXT-A").unwrap();
        m.attach("EXT-A").unwrap();
        fake.with(|w| w.not_ready = true);
        assert_eq!(
            m.wait_enumeration().unwrap_err().code,
            E_TOPOLOGY_ENUMERATION
        );
        fake.with(|w| w.not_ready = false);
        m.wait_enumeration().unwrap();
        m.wait_enumeration().unwrap();
        let waits: Vec<_> = fake
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("wait_ready"))
            .collect();
        assert_eq!(
            waits,
            ["wait_ready EXT-A", "wait_ready EXT-A", "wait_ready "]
        );
    }

    #[test]
    fn wait_enumeration_waits_for_attached_displays_then_clears() {
        let (fake, mut m, _dir) = setup();
        m.detach("EXT-A").unwrap();
        m.attach("EXT-A").unwrap();
        m.wait_enumeration().unwrap();
        m.wait_enumeration().unwrap();
        let waits: Vec<String> = fake
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("wait_ready"))
            .collect();
        assert_eq!(waits, ["wait_ready EXT-A", "wait_ready "]);
    }
}
