//! Versioned private identity, excluded from config and diagnostic serialization.
use super::{LanError, Result};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use zeroize::{Zeroize, Zeroizing};

const HEADER: &[u8] = b"KVMFLOW-LAN-KEY\x01";
pub const NOISE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

pub struct Identity {
    private: Zeroizing<[u8; 32]>,
    public: [u8; 32],
}
impl Identity {
    pub fn generate() -> Result<Self> {
        let mut pair = snow::Builder::with_resolver(
            NOISE_PATTERN.parse()?,
            Box::new(super::crypto::LanResolver),
        )
        .generate_keypair()?;
        let private = Zeroizing::new(
            pair.private
                .as_slice()
                .try_into()
                .map_err(|_| LanError::new("E_LAN_KEY", "invalid generated key"))?,
        );
        let public = pair
            .public
            .as_slice()
            .try_into()
            .map_err(|_| LanError::new("E_LAN_KEY", "invalid generated key"))?;
        pair.private.zeroize();
        Ok(Self { private, public })
    }
    pub fn public_key(&self) -> [u8; 32] {
        self.public
    }
    pub(super) fn private_key(&self) -> &[u8; 32] {
        &self.private
    }
    fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != HEADER.len() + 32 || !bytes.starts_with(HEADER) {
            return Err(LanError::new(
                "E_LAN_KEY",
                "invalid identity version or length; identity preserved",
            ));
        }
        let private: Zeroizing<[u8; 32]> =
            Zeroizing::new(bytes[HEADER.len()..].try_into().unwrap());
        // Derive X25519 public key using the same resolver as the handshake.
        use snow::resolvers::CryptoResolver;
        let mut dh = super::crypto::LanResolver
            .resolve_dh(&snow::params::DHChoice::Curve25519)
            .ok_or_else(|| LanError::new("E_LAN_KEY", "X25519 unavailable"))?;
        dh.set(&*private);
        let public = dh.pubkey().try_into().unwrap();
        Ok(Self { private, public })
    }
}

pub fn default_path() -> Result<PathBuf> {
    #[cfg(target_os = "windows")]
    let (root, suffix) = ("APPDATA", "KVMFlow/lan/identity.dpapi");
    #[cfg(not(target_os = "windows"))]
    let (root, suffix) = (
        "HOME",
        "Library/Application Support/KVMFlow/lan/identity.key",
    );
    Ok(PathBuf::from(
        std::env::var_os(root)
            .ok_or_else(|| LanError::new("E_LAN_KEY", "identity home unavailable"))?,
    )
    .join(suffix))
}

/// Never silently replace a damaged/unreadable identity. New files are published
/// atomically without replacing another process's key (same-directory hard link).
pub fn load_or_create(path: &Path) -> Result<Identity> {
    let parent = path
        .parent()
        .ok_or_else(|| LanError::new("E_LAN_KEY", "identity needs a parent directory"))?;
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::symlink_metadata(parent)?.file_type().is_symlink() {
            return Err(LanError::new(
                "E_LAN_KEY",
                "identity directory is a symlink",
            ));
        }
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    match read_identity(path) {
        Ok(identity) => return Ok(identity),
        Err(e) if e.code == "E_LAN_MISSING_KEY" => {}
        Err(e) => return Err(e),
    }
    let identity = Identity::generate()?;
    let mut bytes = Zeroizing::new(HEADER.to_vec());
    bytes.extend_from_slice(identity.private_key());
    let stored = protect(&bytes)?;
    let mut random = [0; 16];
    getrandom::fill(&mut random)
        .map_err(|_| LanError::new("E_LAN_KEY", "random source unavailable"))?;
    let temp = parent.join(format!(
        ".identity-{:032x}.tmp",
        u128::from_be_bytes(random)
    ));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(&stored)?;
        file.sync_all()?;
        match fs::hard_link(&temp, path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(e.into()),
        }
    })();
    let _ = fs::remove_file(temp);
    result?;
    read_identity(path)
}
fn read_identity(path: &Path) -> Result<Identity> {
    let meta = fs::symlink_metadata(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            LanError::new("E_LAN_MISSING_KEY", "identity missing")
        } else {
            e.into()
        }
    })?;
    if !meta.is_file() || meta.len() > 4096 {
        return Err(LanError::new("E_LAN_KEY", "invalid identity file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    let mut stored = Zeroizing::new(Vec::new());
    fs::File::open(path)?.take(4097).read_to_end(&mut stored)?;
    Identity::decode(&unprotect(&stored)?)
}
#[cfg(not(target_os = "windows"))]
fn protect(bytes: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    Ok(Zeroizing::new(bytes.to_vec()))
}
#[cfg(not(target_os = "windows"))]
fn unprotect(bytes: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    protect(bytes)
}

#[cfg(target_os = "windows")]
fn dpapi(bytes: &[u8], encrypt: bool) -> Result<Zeroizing<Vec<u8>>> {
    use windows::Win32::{
        Foundation::{LocalFree, HLOCAL},
        Security::Cryptography::{
            CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        },
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // Flags omit CRYPTPROTECT_LOCAL_MACHINE: current Windows user only.
    unsafe {
        let result = if encrypt {
            CryptProtectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        result.map_err(|_| LanError::new("E_LAN_KEY", "DPAPI operation failed"))?;
        let plain = Zeroizing::new(
            std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec(),
        );
        std::slice::from_raw_parts_mut(output.pbData, output.cbData as usize).zeroize();
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(plain)
    }
}
#[cfg(target_os = "windows")]
fn protect(bytes: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    dpapi(bytes, true)
}
#[cfg(target_os = "windows")]
fn unprotect(bytes: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    dpapi(bytes, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persistent_versioned_identity_and_corruption_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("lan/identity.key");
        let first = load_or_create(&path).unwrap();
        assert_eq!(
            first.public_key(),
            load_or_create(&path).unwrap().public_key()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        fs::write(&path, b"broken").unwrap();
        assert!(load_or_create(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"broken");
    }
    #[test]
    fn concurrent_creation_returns_the_same_key() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("lan/identity.key");
        std::thread::scope(|scope| {
            let jobs: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| load_or_create(&path).unwrap().public_key()))
                .collect();
            let keys: Vec<_> = jobs.into_iter().map(|job| job.join().unwrap()).collect();
            assert!(keys.iter().all(|key| *key == keys[0]));
        });
    }
}
