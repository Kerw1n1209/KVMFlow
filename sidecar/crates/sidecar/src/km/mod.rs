//! Keyboard and mouse sharing over the LAN, driven by the bundled Deskflow
//! engine. Deskflow runs unmodified as a separate `deskflow-core` process; this
//! module owns its TLS identity, generated profile, installed runtime and
//! process lifetime. Peer fingerprints arrive over the authenticated LAN link
//! and are wired in by the runtime, not here.
pub mod activity;
pub mod engine;
pub mod identity;
pub mod permissions;
pub mod profile;
pub mod runtime;

use std::{fmt, fs, io, io::Write, path::Path};

pub type Result<T> = std::result::Result<T, KmError>;

#[derive(Debug)]
pub struct KmError {
    pub code: &'static str,
    pub message: String,
}
impl KmError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl fmt::Display for KmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for KmError {}
impl From<io::Error> for KmError {
    fn from(e: io::Error) -> Self {
        Self::new("E_KM_IO", e.to_string())
    }
}

/// Create `dir` readable by the current user only, refusing a symlinked one.
pub(crate) fn private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    if fs::symlink_metadata(dir)?.file_type().is_symlink() {
        return Err(KmError::new("E_KM_IO", "data directory is a symlink"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn random_suffix() -> Result<u128> {
    let mut random = [0; 16];
    getrandom::fill(&mut random)
        .map_err(|_| KmError::new("E_KM_IO", "random source unavailable"))?;
    Ok(u128::from_be_bytes(random))
}

/// Write an owner-only temp sibling, then publish it. With `replace` false an
/// existing file wins, so two processes creating the same file agree on one.
pub(crate) fn write_private(path: &Path, bytes: &[u8], replace: bool) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| KmError::new("E_KM_IO", "file needs a parent directory"))?;
    let temp = parent.join(format!(".km-{:032x}.tmp", random_suffix()?));
    let result = (|| -> io::Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        if replace {
            return fs::rename(&temp, path);
        }
        match fs::hard_link(&temp, path) {
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => Err(e),
            _ => Ok(()),
        }
    })();
    let _ = fs::remove_file(&temp);
    Ok(result?)
}
