//! Installs the bundled engine. The release ships Deskflow as one archive so
//! the macOS bundle keeps its upstream signature; it is unpacked once per
//! engine build into app data and verified against the bundled manifest.
use super::{random_suffix, KmError, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Component, Path, PathBuf},
    process::Command,
};

const MANIFEST: &str = "manifest.json";
const ARCHIVE: &str = "runtime.tar.gz";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    version: String,
    binary: String,
    binary_sha256: String,
    runtime_sha256: String,
}

#[derive(Debug)]
pub struct Installed {
    pub binary: PathBuf,
    pub version: String,
}

/// `resources` holds the manifest and archive; the engine is unpacked under
/// `root/engine`. Builds other than the bundled one are removed.
pub fn install(resources: &Path, root: &Path) -> Result<Installed> {
    let manifest = read_manifest(&resources.join(MANIFEST))?;
    let engines = root.join("engine");
    fs::create_dir_all(&engines)?;
    let name = &manifest.runtime_sha256[..16];
    let target = engines.join(name);
    let binary = target.join(&manifest.binary);
    let installed = Installed {
        binary: binary.clone(),
        version: manifest.version.clone(),
    };
    if digest(&binary).ok().as_deref() == Some(manifest.binary_sha256.as_str()) {
        return Ok(installed);
    }
    let archive = resources.join(ARCHIVE);
    if digest(&archive)? != manifest.runtime_sha256 {
        return Err(KmError::new(
            "E_KM_RUNTIME",
            "bundled engine failed verification",
        ));
    }
    let staging = engines.join(format!(".install-{:032x}", random_suffix()?));
    let result = (|| -> Result<()> {
        fs::create_dir(&staging)?;
        let status = Command::new(tar())
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&staging)
            .status()?;
        if !status.success() {
            return Err(KmError::new("E_KM_RUNTIME", "could not unpack the engine"));
        }
        if digest(&staging.join(&manifest.binary))? != manifest.binary_sha256 {
            return Err(KmError::new(
                "E_KM_RUNTIME",
                "unpacked engine failed verification",
            ));
        }
        if target.exists() {
            fs::remove_dir_all(&target)?;
        }
        fs::rename(&staging, &target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result?;
    for entry in fs::read_dir(&engines)?.flatten() {
        let other = entry.file_name();
        let other = other.to_string_lossy();
        if other != name && other.len() == 16 && other.bytes().all(|b| b.is_ascii_hexdigit()) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
    Ok(installed)
}

fn read_manifest(path: &Path) -> Result<Manifest> {
    let invalid = || KmError::new("E_KM_RUNTIME", "engine manifest is invalid");
    if fs::metadata(path)?.len() > 64 * 1024 {
        return Err(invalid());
    }
    let manifest: Manifest = serde_json::from_slice(&fs::read(path)?).map_err(|_| invalid())?;
    let hex = |s: &str| s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    let relative = Path::new(&manifest.binary)
        .components()
        .all(|c| matches!(c, Component::Normal(_)));
    if !hex(&manifest.binary_sha256) || !hex(&manifest.runtime_sha256) || !relative {
        return Err(invalid());
    }
    Ok(manifest)
}

/// The system tar: bsdtar on macOS and on Windows 10 1803 and later.
fn tar() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let system = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        PathBuf::from(system).join(r"System32\tar.exe")
    }
    #[cfg(not(target_os = "windows"))]
    PathBuf::from("/usr/bin/tar")
}

fn digest(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    io::copy(&mut fs::File::open(path)?, &mut hasher)?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Packs `bin/engine` the way prepare-deskflow.js does and writes a manifest.
    fn bundle(resources: &Path, body: &[u8]) -> String {
        let content = tempfile::tempdir().unwrap();
        fs::create_dir(content.path().join("bin")).unwrap();
        fs::write(content.path().join("bin/engine"), body).unwrap();
        fs::create_dir_all(resources).unwrap();
        let archive = resources.join(ARCHIVE);
        let ok = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(content.path())
            .arg(".")
            .env("COPYFILE_DISABLE", "1")
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let manifest = serde_json::json!({
            "version": "1.27.0",
            "binary": "bin/engine",
            "binarySha256": digest(&content.path().join("bin/engine")).unwrap(),
            "runtimeSha256": digest(&archive).unwrap(),
        });
        fs::write(resources.join(MANIFEST), manifest.to_string()).unwrap();
        digest(&archive).unwrap()
    }

    #[test]
    fn installs_once_and_replaces_old_builds() {
        let dir = tempfile::tempdir().unwrap();
        let (resources, root) = (dir.path().join("res"), dir.path().join("km"));
        let stale = root.join("engine/0123456789abcdef");
        fs::create_dir_all(&stale).unwrap();
        let sha = bundle(&resources, b"engine one");
        let installed = install(&resources, &root).unwrap();
        assert_eq!(
            installed.binary,
            root.join("engine").join(&sha[..16]).join("bin/engine")
        );
        assert_eq!(fs::read(&installed.binary).unwrap(), b"engine one");
        assert!(!stale.exists());
        // A verified install is reused without touching the archive.
        fs::remove_file(resources.join(ARCHIVE)).unwrap();
        assert_eq!(install(&resources, &root).unwrap().binary, installed.binary);
    }

    #[test]
    fn damaged_install_is_unpacked_again() {
        let dir = tempfile::tempdir().unwrap();
        let (resources, root) = (dir.path().join("res"), dir.path().join("km"));
        bundle(&resources, b"engine");
        let installed = install(&resources, &root).unwrap();
        fs::write(&installed.binary, b"tampered").unwrap();
        install(&resources, &root).unwrap();
        assert_eq!(fs::read(&installed.binary).unwrap(), b"engine");
    }

    #[test]
    fn refuses_an_archive_that_does_not_match() {
        let dir = tempfile::tempdir().unwrap();
        let (resources, root) = (dir.path().join("res"), dir.path().join("km"));
        bundle(&resources, b"engine");
        fs::write(resources.join(ARCHIVE), b"other").unwrap();
        assert_eq!(install(&resources, &root).unwrap_err().code, "E_KM_RUNTIME");
        assert_eq!(fs::read_dir(root.join("engine")).unwrap().count(), 0);
    }

    #[test]
    fn refuses_a_binary_path_outside_the_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let resources = dir.path().join("res");
        bundle(&resources, b"engine");
        let text = fs::read_to_string(resources.join(MANIFEST))
            .unwrap()
            .replace("bin/engine", "../engine");
        fs::write(resources.join(MANIFEST), text).unwrap();
        assert!(install(&resources, &dir.path().join("km")).is_err());
    }
}
