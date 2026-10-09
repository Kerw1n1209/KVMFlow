//! Windows platform adapter module. Compile-checked via
//! `cargo check --target x86_64-pc-windows-gnu`; runtime behavior on real
//! Windows is NOT yet verified (honestly reported, per KVM-4 acceptance).
//!
//! DDC: dxva2 Get/SetVCPFeature (the path KVM-1 validated on real hardware,
//! including the quirk that a PHYSICAL_MONITOR handle of 0 can be valid).
//! Displays: EnumDisplayDevices + registry EDID -> same edid_identity rule.
//! USB: SetupAPI device-interface enumeration polled into snapshots.

pub mod ddc;
pub mod usb;
