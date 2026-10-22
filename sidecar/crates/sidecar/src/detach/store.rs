//! Persistent detach records in the sidecar state directory. A record is
//! written before the system is touched, so a crash always leaves enough to
//! restore the display on the next start.

use crate::backends::topology::DetachRecord;
use kvmflow_core::errors::{CoreError, E_CONFIG_IO};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

const FILE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    records: Vec<DetachRecord>,
}

pub struct RecordStore {
    path: PathBuf,
}

impl RecordStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A missing file is an empty store. An unreadable one is moved aside
    /// (never overwritten silently) and reported, so the caller can log it.
    pub fn load(&self) -> (Vec<DetachRecord>, Option<String>) {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Vec::new(), None),
            Err(e) => {
                return (
                    Vec::new(),
                    Some(format!("read {}: {e}", self.path.display())),
                )
            }
        };
        match serde_json::from_str::<StoreFile>(&text) {
            Ok(file) if file.version == FILE_VERSION => (file.records, None),
            parsed => {
                let why = match parsed {
                    Ok(file) => format!("unsupported version {}", file.version),
                    Err(e) => e.to_string(),
                };
                let aside = self
                    .path
                    .with_extension(format!("json.corrupt-{}", crate::session::now_ms()));
                let kept = if std::fs::rename(&self.path, &aside).is_ok() {
                    format!("kept as {}", aside.display())
                } else {
                    "could not move aside".to_string()
                };
                (
                    Vec::new(),
                    Some(format!("detach records unreadable ({why}); {kept}")),
                )
            }
        }
    }

    /// Atomic replace; an empty list removes the file.
    pub fn save(&self, records: &[DetachRecord]) -> Result<(), CoreError> {
        let io = |e: std::io::Error| {
            CoreError::new(
                E_CONFIG_IO,
                format!("detach records {}: {e}", self.path.display()),
            )
        };
        if records.is_empty() {
            return match std::fs::remove_file(&self.path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io(e)),
                _ => Ok(()),
            };
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let body = serde_json::to_string_pretty(&StoreFile {
            version: FILE_VERSION,
            records: records.to_vec(),
        })
        .map_err(|e| CoreError::new(E_CONFIG_IO, e.to_string()))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, body).map_err(io)?;
        std::fs::rename(&tmp, &self.path).map_err(io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::topology::DetachMethod;

    fn record(fp: &str) -> DetachRecord {
        DetachRecord {
            fingerprint_source: crate::backends::topology::FingerprintSource::Ddc,
            fingerprint: fp.into(),
            key: format!("k-{fp}"),
            identity: format!("id-{fp}"),
            method: DetachMethod::Disabled,
            mirror_of: None,
            origins: vec![],
            topology: Some("00ff".into()),
            detached_at_ms: 1,
        }
    }

    #[test]
    fn save_load_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let store = RecordStore::new(dir.path().join("display-detach.json"));
        assert_eq!(store.load(), (vec![], None));
        store.save(&[record("A"), record("B")]).unwrap();
        assert_eq!(store.load().0, vec![record("A"), record("B")]);
        store.save(&[]).unwrap();
        assert!(!store.path().exists());
        store.save(&[]).unwrap(); // removing nothing is fine
    }

    #[test]
    fn corrupt_file_is_kept_aside_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let store = RecordStore::new(dir.path().join("display-detach.json"));
        std::fs::write(store.path(), "{not json").unwrap();
        let (records, warning) = store.load();
        assert!(records.is_empty());
        assert!(warning.unwrap().contains("kept as"));
        assert!(!store.path().exists());
        let kept = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().contains("corrupt"));
        assert!(kept);
    }
}
