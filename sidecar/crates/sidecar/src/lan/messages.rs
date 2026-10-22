use super::{LanError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Envelope {
    pub v: u8,
    pub t: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub re: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub ok: Option<bool>,
    pub body: Value,
}
impl Envelope {
    pub fn new(t: impl Into<String>, body: Value) -> Self {
        Self {
            v: 1,
            t: t.into(),
            id: None,
            re: None,
            ok: None,
            body,
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.v != 1 || self.t.is_empty() || !self.body.is_object() {
            return Err(LanError::new("E_LAN_PROTO", "unsupported envelope"));
        }
        Ok(())
    }
}

/// Summary is intentionally opaque to transport; S7 owns its schema.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hello {
    pub proto: u8,
    pub app_version: String,
    pub device_id: String,
    pub group_id: Option<String>,
    pub holds_usb: bool,
    pub summary: Value,
    pub caps: Vec<String>,
    /// Peer OS (`macos`/`windows`/`linux`), added in protocol v1.1. Older
    /// peers omit it; consumers must treat it as unknown, never guess.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// Human-readable host name, authenticated by the Noise handshake. The
    /// pairing UI prefers this over an mDNS name (which is unauthenticated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}
impl Hello {
    pub fn new(device_id: impl Into<String>) -> Self {
        Self {
            proto: 1,
            app_version: env!("CARGO_PKG_VERSION").into(),
            device_id: device_id.into(),
            group_id: None,
            holds_usb: false,
            summary: serde_json::json!({}),
            caps: vec![],
            platform: Some(std::env::consts::OS.to_string()),
            name: None,
        }
    }
    pub fn envelope(&self) -> Result<Envelope> {
        Ok(Envelope::new("hello", serde_json::to_value(self)?))
    }
    pub fn parse(message: Envelope) -> Result<Self> {
        message.validate()?;
        if message.t != "hello" {
            return Err(LanError::new("E_LAN_PROTO", "expected hello"));
        }
        let hello: Self = serde_json::from_value(message.body)?;
        if hello.proto != 1 || hello.device_id.is_empty() || hello.device_id.len() > 128 {
            return Err(LanError::new("E_LAN_PROTO", "unsupported hello"));
        }
        Ok(hello)
    }
}
