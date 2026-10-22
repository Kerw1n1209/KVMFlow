//! Opt-in LAN building blocks owned by the sidecar runtime. No config or hardware
//! operations occur here. Runtime integration, pairing approval and persistence
//! are deliberately left to the caller (S6–S8).
pub mod address;
pub mod conn;
mod crypto;
pub mod discovery;
pub mod identity_store;
pub mod messages;
pub mod noise;
pub mod pair_link;
pub mod trust;

use std::{fmt, io};

pub type Result<T> = std::result::Result<T, LanError>;

#[derive(Debug)]
pub struct LanError {
    pub code: &'static str,
    pub message: String,
}
impl LanError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl fmt::Display for LanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for LanError {}
impl From<io::Error> for LanError {
    fn from(e: io::Error) -> Self {
        Self::new("E_LAN_IO", e.to_string())
    }
}
impl From<snow::Error> for LanError {
    fn from(e: snow::Error) -> Self {
        Self::new("E_LAN_NOISE", e.to_string())
    }
}
impl From<serde_json::Error> for LanError {
    fn from(e: serde_json::Error) -> Self {
        Self::new("E_LAN_PROTO", e.to_string())
    }
}
