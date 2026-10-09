//! KVMFlow core - the single shared implementation of everything that must NOT
//! diverge between macOS and Windows: the versioned protocol, the config
//! schema, the USB trigger debounce, the switch state machine, the retry
//! policy, the error-code catalog and the log-event model.
//!
//! Platform adapters (IOKit / m1ddc subprocess / dxva2 / SetupAPI) live in the
//! `kvmflow-sidecar` crate and only perform system calls; every decision is
//! made here.
//!
//! Design inputs are the KVM-1 verified conclusions (2026-09-13):
//! - trigger = device-group full round trip; same-end replug is not a switch
//! - debounce quorum + T_stable>=5s / T_absent>=10s / T_cooldown>=15s replayed
//!   with 0 false triggers on the real bounce fixture
//! - input codes are vendor dialects, learned per monitor, never from MCCS
//! - push-away model: only the side holding the picture can reliably write
//! - a DDC command succeeding is machine_observed only, never picture success

pub mod clock;
pub mod config;
pub mod debounce;
pub mod errors;
pub mod events;
pub mod multidevice;
pub mod protocol;
pub mod retry;
pub mod statemachine;
pub mod transaction;

/// Single rule for building the stable cross-platform display identity
/// `MANU-PPPP-SSSSSSSS` / `MANU-PPPP-S:<string>` / `MANU-PPPP-NOSERIAL`.
/// The macOS adapter derives the inputs from m1ddc's display fields, the
/// Windows adapter from raw EDID bytes; both funnel through this function so
/// the two platforms cannot drift apart.
pub fn edid_identity(
    manufacturer: &str,
    product_code: u16,
    serial_number: u32,
    serial_string: &str,
) -> String {
    let manu = if manufacturer.chars().all(|c| c.is_ascii_uppercase()) && manufacturer.len() == 3 {
        manufacturer.to_string()
    } else if !manufacturer.is_empty() {
        manufacturer.to_uppercase()
    } else {
        "UNK".to_string()
    };
    let product = format!("{product_code:04X}");
    if serial_number != 0 {
        format!("{manu}-{product}-{serial_number:08X}")
    } else if !serial_string.is_empty() {
        format!("{manu}-{product}-S:{serial_string}")
    } else {
        format!("{manu}-{product}-NOSERIAL")
    }
}

pub const SIDECAR_VERSION: &str = env!("CARGO_PKG_VERSION");
