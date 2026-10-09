//! Error-code catalog - the single copy shared by both platforms and the
//! Electron shell (via protocol responses). Codes are stable strings.

pub const E_PROTOCOL_VERSION: &str = "E_PROTOCOL_VERSION";
pub const E_MALFORMED_MESSAGE: &str = "E_MALFORMED_MESSAGE";
pub const E_UNKNOWN_METHOD: &str = "E_UNKNOWN_METHOD";
pub const E_DISPLAY_NOT_FOUND: &str = "E_DISPLAY_NOT_FOUND";
pub const E_DDC_BINARY_MISSING: &str = "E_DDC_BINARY_MISSING";
pub const E_DDC_FAILED: &str = "E_DDC_FAILED";
pub const E_DDC_NOT_READABLE: &str = "E_DDC_NOT_READABLE";
pub const E_CONFIG_INVALID: &str = "E_CONFIG_INVALID";
pub const E_CONFIG_IO: &str = "E_CONFIG_IO";
pub const E_TRIGGER_NOT_ARMED: &str = "E_TRIGGER_NOT_ARMED";
pub const E_NOT_LEARNING: &str = "E_NOT_LEARNING";
pub const E_BACKEND: &str = "E_BACKEND";
pub const E_INTERNAL: &str = "E_INTERNAL";

#[derive(Debug, Clone)]
pub struct CoreError {
    pub code: &'static str,
    pub message: String,
}

impl CoreError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for CoreError {}
