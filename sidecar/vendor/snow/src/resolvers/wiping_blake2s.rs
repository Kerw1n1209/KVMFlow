//! BLAKE2s using RustCrypto's core API without finalize_reset's state clone.
//! The exact dependency versions below are part of the flat-state safety proof.
use blake2::{
    Blake2sVarCore,
    digest::{
        Output,
        core_api::{Buffer, UpdateCore, VariableOutputCore},
    },
};
use zeroize::Zeroizing;

pub(super) struct WipingBlake2s {
    core: Blake2sVarCore,
    buffer: Buffer<Blake2sVarCore>,
}
impl Default for WipingBlake2s {
    fn default() -> Self {
        Self {
            core: Blake2sVarCore::new(32).unwrap(),
            buffer: Buffer::<Blake2sVarCore>::default(),
        }
    }
}
impl WipingBlake2s {
    pub fn update(&mut self, data: &[u8]) {
        self.buffer
            .digest_blocks(data, |blocks| self.core.update_blocks(blocks));
    }
    pub fn finalize_reset(&mut self) -> Zeroizing<[u8; 32]> {
        let mut output = Zeroizing::new([0; 32]);
        self.core.finalize_variable_core(
            &mut self.buffer,
            Output::<Blake2sVarCore>::from_mut_slice(&mut *output),
        );
        self.reset();
        output
    }
    pub fn reset(&mut self) {
        self.wipe();
        self.core = Blake2sVarCore::new(32).unwrap();
        self.buffer = Buffer::<Blake2sVarCore>::default();
    }
    fn wipe(&mut self) {
        // SAFETY: pinned blake2 0.10.6 core has only integer/vector words and
        // a counter (optional reset adds more words). block-buffer 0.10.4 has
        // [u8;64], u8 position and PhantomData<Lazy>. No pointers, enums, Drop
        // fields or allocations. All-zero is valid. No computation uses the
        // zero state: reset replaces it, Drop only releases these flat fields.
        // Keep these pins and re-audit the source before upgrading either crate.
        const {
            assert!(!core::mem::needs_drop::<Blake2sVarCore>());
        }
        const {
            assert!(!core::mem::needs_drop::<Buffer<Blake2sVarCore>>());
        }
        unsafe {
            zeroize::zeroize_flat_type(&mut self.core);
            zeroize::zeroize_flat_type(&mut self.buffer);
        }
    }
}
impl Drop for WipingBlake2s {
    fn drop(&mut self) {
        self.wipe();
    }
}
impl zeroize::ZeroizeOnDrop for WipingBlake2s {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reset_and_drop_clear_buffered_secret_bytes() {
        let mut hash = WipingBlake2s::default();
        hash.update(&[0xa5; 31]);
        assert_eq!(hash.buffer.get_data(), &[0xa5; 31]);
        hash.reset();
        assert!(hash.buffer.get_data().is_empty());
        // Exercise the same wipe operation used by Drop, while fields are alive.
        hash.update(&[0xa5; 31]);
        hash.wipe();
        assert!(hash.buffer.get_data().is_empty());
        // wipe wrote every byte (including padding) before inspection.
        let raw = unsafe {
            core::slice::from_raw_parts(
                core::ptr::addr_of!(hash.buffer).cast::<u8>(),
                core::mem::size_of_val(&hash.buffer),
            )
        };
        assert!(raw.iter().all(|byte| *byte == 0));
    }
    #[test]
    fn core_hash_matches_reference_for_chunk_boundaries() {
        use blake2::{Blake2s256, Digest};
        for length in [0, 1, 31, 63, 64, 65, 127, 128, 129, 1024] {
            let data = alloc::vec![0xa5;length];
            let mut hash = WipingBlake2s::default();
            for chunk in data.chunks(7) {
                hash.update(chunk);
            }
            assert_eq!(
                &*hash.finalize_reset(),
                Blake2s256::digest(&data).as_slice()
            );
        }
    }
}
