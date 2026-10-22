# KVMFlow secret-lifetime patch, based on snow 0.10.0

This is a local, scoped fork, version `0.10.0-kvmflow.1`. Original source hashes
are recorded in UPSTREAM.json. Upstream MIT and Apache-2.0 licenses are preserved.
No Noise transcript, cipher, DH, hash or KDF algorithm is replaced. Only lifetime
management and the BLAKE2s digest-core adapter change. KVMFlow uses LanResolver,
which permits only X25519 / ChaChaPoly / BLAKE2s and system randomness.

Why a fork: snow 0.10.0 does not expose SymmetricState's chaining key, checkpoint
or HKDF temporary buffers through CryptoResolver. A custom resolver alone cannot
wipe those objects. See upstream issue https://github.com/mcginty/snow/issues/203.

Changes to upstream:

- `resolvers/default.rs`: X25519 static/ephemeral private arrays and ChaChaPoly
  session-key arrays zeroize in Drop. DH set/generate/shared-secret temporaries
  use Zeroizing. AEAD construction borrows key slices instead of creating a plain
  GenericArray key temporary. The BLAKE2s resolver uses the adapter below.
- `resolvers/wiping_blake2s.rs`: use RustCrypto's existing VariableOutputCore API
  directly to avoid finalize_reset cloning the hash state. Wipe the core and
  buffered key input on reset/Drop, and guard digest output. This deliberately
  uses the existing BLAKE2s implementation, not a new compression implementation.
- `symmetricstate.rs`: remove Copy from checkpoint data; every cloned checkpoint
  and chaining state zeroizes on Drop. HKDF output and cipher-key temporaries
  use Zeroizing, including split. Failed handshakes release the same guards.
- `types.rs`: HMAC pads/intermediate digest, HKDF temporary key/input and rekey
  output/key are guarded through every early return.
- `handshakestate.rs`, `builder.rs`: guard DH results, the owned PSK store and
  generated Keypair private Vec. Both read/write `Token::Psk` paths borrow the
  guarded store directly; matching an Option by value must not create an
  unguarded `[u8; 32]` stack copy. Moving the transport ciphers out still drops all remaining
  handshake state (including private DH boxes, chaining state and PSKs).
- Manifest: restrict supported primitives, enable curve25519-dalek and Poly1305
  zeroize features; omit unused upstream benchmarks/examples/dev dependencies.

The one production unsafe block clears the flat RustCrypto hash core and block
buffer with zeroize_flat_type. Its safety proof depends on blake2 **0.10.6** and
block-buffer **0.10.4**, pinned exactly in Cargo.toml: both contain only integers,
arrays/vector words and PhantomData, no pointers/allocations/Drop fields; all-zero
is valid. Compile-time needs_drop assertions guard against added destructors.
Re-audit their source before changing either pin. Test-only raw probes inspect
initialized fixed arrays in still-allocated MaybeUninit storage after Drop, never
freed heap memory or uninitialized padding.

Verification: the three memory regression probes fail against the original
upstream Dh25519, CipherChaChaPoly and SymmetricStateData (nonzero bytes remain),
then pass with the patch. Existing X25519, BLAKE2 and ChaChaPoly known-answer tests
remain, plus hash chunk-boundary equivalence and secret-buffer wiping tests.
The application's loopback Noise/MITM suite checks the selected full protocol.
Read/write PSK address probes assert that HKDF receives the guarded store itself;
both fail against the former by-value matches. `tests/upstream_equivalence.rs`
uses the unmodified registry `snow =0.10.0` as a dev-only reference. Fixed static
and ephemeral keys compare every handshake frame/hash, remote static key and
bidirectional transport ciphertext for XX, NNpsk0, XXpsk0 and XXpsk2. These keys
are test-only. The reference implementation is not linked into KVMFlow.

## Upgrade review procedure

1. Obtain the original crate for the version in `UPSTREAM.json`, verify each
   recorded SHA-256 against that source, and diff the fork against it. Review
   every intentional lifetime change before rebasing onto a new upstream crate.
   Update the provenance file from the NEW unmodified source, not from this fork.
   Follow [mcginty/snow#203](https://github.com/mcginty/snow/issues/203) for upstream
   key-lifetime work; reassess whether each local change is still needed.
2. Re-audit the unsafe adapter's actual dependency source and all enabled
   features/targets before changing pins. Currently blake2 0.10.6 generates
   `Blake2sVarCore` with `h: [u32x4; 2]`, `t: u64`, and (with `reset`) another
   `h0: [u32x4; 2]`; inspect the vector-word representation too. block-buffer
   0.10.4's `Buffer<Blake2sVarCore>` is `BlockBuffer<U64, Lazy>` containing a
   `GenericArray<u8, U64>`, a `u8` position and `PhantomData<Lazy>`. Follow nested
   generic-array/vector types and feature unification, not just these names.
   Confirm no pointers, allocations, invalid all-zero values or Drop fields;
   `needs_drop == false` alone does NOT prove this (pointers also have no Drop).
   If this proof no longer holds, replace the adapter before upgrading.
3. Pin the test-only `upstream-snow` alias to the new upstream version, update
   lockfiles and run from the repository root (adjust the fork version selector
   if it changes):

   ```sh
   cargo test --locked --manifest-path sidecar/Cargo.toml -p snow@0.10.0-kvmflow.1 --test upstream_equivalence
   cargo test --locked --manifest-path sidecar/Cargo.toml -p snow@0.10.0-kvmflow.1 --lib
   cargo test --release --locked --manifest-path sidecar/Cargo.toml -p snow@0.10.0-kvmflow.1 --lib
   cargo test --locked --manifest-path sidecar/Cargo.toml --workspace
   cargo check --locked --manifest-path sidecar/Cargo.toml -p kvmflow-sidecar --target x86_64-pc-windows-msvc
   cargo metadata --locked --offline --manifest-path desktop/src-tauri/Cargo.toml --format-version 1
   ```

   Preserve the upstream output comparison test; investigate any divergence,
   rather than updating expected outputs to fit the fork. The lib suites include
   Drop-memory and PSK-borrow regression probes in debug and release. They check
   explicit owned buffers, not universal process-memory erasure. Also review the
   supported-algorithm whitelist and run the application's Noise/MITM tests.

Limits: this wipes the explicitly owned key/state objects and guarded temporary
buffers at their Rust lifetimes. It cannot promise erasure of compiler-created
moves/register spills, internal arithmetic temporaries inside RustCrypto/dalek,
OS swap/core dumps, copies made by callers, or termination via abort/power loss.
Those limits are not described as a guarantee that every historical byte in
process memory is cleared. Other upstream primitives are outside this audit and
are rejected by the application resolver; unsupported features fail compilation.
