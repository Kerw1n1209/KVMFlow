//! The engine's TLS identity. Each computer generates one self-signed
//! certificate and keeps it; peers trust it by fingerprint, which travels over
//! the Noise-authenticated LAN link instead of being accepted on first use.
use super::{private_dir, write_private, KmError, Result};
use sha2::{Digest, Sha256};
use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

const FILE: &str = "identity.pem";
const MAX_LEN: u64 = 16 * 1024;

/// SHA-256 of the certificate DER, as Deskflow computes it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    pub fn of_der(der: &[u8]) -> Self {
        Self(Sha256::digest(der).into())
    }
    /// Accepts only Deskflow's own `v2:sha256:<64 lowercase hex>` form.
    pub fn parse(text: &str) -> Result<Self> {
        let invalid = || KmError::new("E_KM_FINGERPRINT", "invalid engine fingerprint");
        let hex = text.strip_prefix("v2:sha256:").ok_or_else(invalid)?;
        if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(invalid());
        }
        let mut bytes = [0; 32];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|_| invalid())?;
        }
        Ok(Self(bytes))
    }
}
impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("v2:sha256:")?;
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}
impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

pub struct EngineIdentity {
    /// Private key followed by the certificate, the layout Deskflow loads.
    pub pem_path: PathBuf,
    pub fingerprint: Fingerprint,
}

/// A damaged identity is reported, never silently regenerated: a new
/// certificate would quietly break trust with every paired computer.
pub fn load_or_create(dir: &Path) -> Result<EngineIdentity> {
    private_dir(dir)?;
    let path = dir.join(FILE);
    if !path.exists() {
        let pair = rcgen::generate_simple_self_signed(vec!["kvmflow.local".into()])
            .map_err(|e| KmError::new("E_KM_IDENTITY", e.to_string()))?;
        let text = format!("{}{}", pair.signing_key.serialize_pem(), pair.cert.pem());
        write_private(&path, text.as_bytes(), false)?;
    }
    let fingerprint = read(&path)?;
    Ok(EngineIdentity {
        pem_path: path,
        fingerprint,
    })
}

fn read(path: &Path) -> Result<Fingerprint> {
    let damaged = |why: &str| KmError::new("E_KM_IDENTITY", format!("engine identity {why}"));
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.len() > MAX_LEN {
        return Err(damaged("is not a regular file"));
    }
    let text = fs::read_to_string(path)?;
    let blocks = pem::parse_many(&text).map_err(|_| damaged("is unreadable"))?;
    let key = blocks.iter().find(|b| b.tag() == "PRIVATE KEY");
    let cert = blocks.iter().find(|b| b.tag() == "CERTIFICATE");
    let (Some(key), Some(cert)) = (key, cert) else {
        return Err(damaged("is incomplete"));
    };
    rcgen::KeyPair::try_from(key.contents()).map_err(|_| damaged("has an unusable key"))?;
    Ok(Fingerprint::of_der(cert.contents()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_persists_and_matches_its_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create(dir.path()).unwrap();
        let second = load_or_create(dir.path()).unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
        let text = fs::read_to_string(&first.pem_path).unwrap();
        let cert = pem::parse_many(&text)
            .unwrap()
            .into_iter()
            .find(|b| b.tag() == "CERTIFICATE")
            .unwrap();
        assert_eq!(first.fingerprint, Fingerprint::of_der(cert.contents()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&first.pem_path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn damaged_identity_is_kept_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(FILE), "not a certificate").unwrap();
        let error = load_or_create(dir.path()).err().unwrap();
        assert_eq!(error.code, "E_KM_IDENTITY");
        assert_eq!(
            fs::read_to_string(dir.path().join(FILE)).unwrap(),
            "not a certificate"
        );
    }

    #[test]
    fn fingerprint_round_trips_only_in_deskflow_form() {
        let fingerprint = Fingerprint::of_der(b"cert");
        let text = fingerprint.to_string();
        assert!(text.starts_with("v2:sha256:") && text.len() == 74);
        assert_eq!(Fingerprint::parse(&text).unwrap(), fingerprint);
        for bad in [
            &text[3..],
            &text.to_uppercase(),
            &text[..73],
            &format!("{text}\n"),
            "v2:sha256:",
        ] {
            assert!(Fingerprint::parse(bad).is_err(), "{bad:?}");
        }
    }
}
