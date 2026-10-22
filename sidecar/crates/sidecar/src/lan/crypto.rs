//! Only the primitives covered by the local snow secret-lifetime patch may be
//! selected. Hash choices outside BLAKE2s are intentionally not a LAN fallback.
use snow::{
    params::{CipherChoice, DHChoice, HashChoice},
    resolvers::{CryptoResolver, DefaultResolver},
    types::{Cipher, Dh, Hash, Random},
};
pub(super) struct LanResolver;
impl CryptoResolver for LanResolver {
    fn resolve_rng(&self) -> Option<Box<dyn Random>> {
        DefaultResolver.resolve_rng()
    }
    fn resolve_dh(&self, choice: &DHChoice) -> Option<Box<dyn Dh>> {
        matches!(choice, DHChoice::Curve25519)
            .then(|| DefaultResolver.resolve_dh(choice))
            .flatten()
    }
    fn resolve_cipher(&self, choice: &CipherChoice) -> Option<Box<dyn Cipher>> {
        matches!(choice, CipherChoice::ChaChaPoly)
            .then(|| DefaultResolver.resolve_cipher(choice))
            .flatten()
    }
    fn resolve_hash(&self, choice: &HashChoice) -> Option<Box<dyn Hash>> {
        matches!(choice, HashChoice::Blake2s)
            .then(|| DefaultResolver.resolve_hash(choice))
            .flatten()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_secret_lifetime_audited_primitives_are_available() {
        assert!(LanResolver.resolve_dh(&DHChoice::Curve25519).is_some());
        assert!(LanResolver
            .resolve_cipher(&CipherChoice::ChaChaPoly)
            .is_some());
        assert!(LanResolver.resolve_hash(&HashChoice::Blake2s).is_some());
        assert!(LanResolver.resolve_hash(&HashChoice::Blake2b).is_none());
        assert!(LanResolver.resolve_hash(&HashChoice::SHA256).is_none());
        assert!(LanResolver.resolve_cipher(&CipherChoice::AESGCM).is_none());
    }
}
