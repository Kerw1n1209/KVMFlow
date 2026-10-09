//! Session log - JSONL event stream with size rotation. Every event also
//! flows to the Electron shell as an `event` notification, so the file and
//! the UI cannot diverge.

use kvmflow_core::events::{EventRecord, Level};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

const ROTATE_BYTES: u64 = 8 * 1024 * 1024;

pub struct SessionLog {
    path: PathBuf,
    file: Mutex<Option<File>>,
}

impl SessionLog {
    pub fn open(path: PathBuf) -> Self {
        let s = Self {
            path,
            file: Mutex::new(None),
        };
        s.write(&EventRecord {
            ts_ms: now_ms(),
            kind: kvmflow_core::events::kinds::SIDECAR_START.into(),
            level: Level::Info,
            fields: serde_json::Map::new(),
        });
        s
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn write(&self, record: &EventRecord) {
        let mut guard = self.file.lock().unwrap();
        // rotate when over cap
        if let Ok(meta) = std::fs::metadata(&self.path) {
            if meta.len() > ROTATE_BYTES {
                let rotated = self.path.with_extension("jsonl.1");
                let _ = std::fs::rename(&self.path, rotated);
                *guard = None;
            }
        }
        if guard.is_none() {
            if let Some(parent) = self.path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            *guard = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .ok();
        }
        if let Some(f) = guard.as_mut() {
            let mut line = serde_json::to_string(record).unwrap_or_else(|_| "{}".into());
            line.push('\n');
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// Read the last `n` events (diagnostics.collect).
    pub fn tail(&self, n: usize) -> Vec<EventRecord> {
        let content = std::fs::read_to_string(&self.path).unwrap_or_default();
        let mut records = Vec::new();
        for line in content.lines().rev() {
            if records.len() >= n {
                break;
            }
            if let Ok(r) = serde_json::from_str::<EventRecord>(line.trim()) {
                records.push(r);
            }
        }
        records.reverse();
        records
    }
}

pub fn now_ms() -> u64 {
    use kvmflow_core::clock::Clock;
    kvmflow_core::clock::SystemClock.now_ms()
}
