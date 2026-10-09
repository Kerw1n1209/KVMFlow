//! Standalone JSONL entry point, retained for protocol tests and diagnostics.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() {
    kvmflow_sidecar::run_stdio();
}
