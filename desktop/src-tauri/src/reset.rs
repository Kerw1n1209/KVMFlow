//! Beta-only configuration reset: moves the saved setup and any pending
//! pairing record aside so the next runtime starts unconfigured. The LAN
//! identity stays, so the other computer recognises this one when re-paired.
use std::path::{Path, PathBuf};

const FILES: [&str; 2] = ["config.json", "pairing.pending.json"];

pub fn allowed(version: &str) -> bool {
    version.contains("-beta")
}

/// Returns the backup folder, or None when there was nothing to move.
pub fn move_aside(data_dir: &Path, stamp: u64) -> Result<Option<PathBuf>, String> {
    let present: Vec<_> = FILES.iter().filter(|name| data_dir.join(name).is_file()).collect();
    if present.is_empty() {
        return Ok(None);
    }
    let backup = data_dir.join("backups").join(format!("reset-{stamp}"));
    std::fs::create_dir_all(&backup).map_err(|error| error.to_string())?;
    for name in present {
        std::fs::rename(data_dir.join(name), backup.join(name)).map_err(|error| error.to_string())?;
    }
    Ok(Some(backup))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_beta_builds_may_reset() {
        assert!(allowed("0.4.0-beta.9"));
        assert!(!allowed("0.4.0"));
    }

    #[test]
    fn reset_moves_setup_and_pending_pairing_but_keeps_identity() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), "{}").unwrap();
        std::fs::write(dir.path().join("pairing.pending.json"), "{}").unwrap();
        std::fs::create_dir(dir.path().join("lan")).unwrap();
        std::fs::write(dir.path().join("lan/identity.key"), "key").unwrap();
        let backup = move_aside(dir.path(), 7).unwrap().unwrap();
        assert_eq!(backup, dir.path().join("backups/reset-7"));
        assert!(!dir.path().join("config.json").exists());
        assert!(!dir.path().join("pairing.pending.json").exists());
        assert_eq!(std::fs::read_to_string(backup.join("config.json")).unwrap(), "{}");
        assert!(dir.path().join("lan/identity.key").exists());
        assert_eq!(move_aside(dir.path(), 8).unwrap(), None);
    }
}
