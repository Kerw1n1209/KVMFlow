//! Canonical request/notification contract for the embedded Tauri runtime
//! and standalone JSONL diagnostic server. Both consume the same catalog
//! and return the same structured errors; transport does not define behavior.
//!
//! Wire format (UTF-8, one JSON object per `\n`-terminated line):
//!
//! client -> sidecar (request):
//!   {"v":1,"id":7,"method":"display.list","params":{...}}
//! sidecar -> client:
//!   response:      {"v":1,"id":7,"ok":true,"result":{...}}
//!   response(err): {"v":1,"id":7,"ok":false,"error":{"code":"E_...","message":"..."}}
//!   notification:  {"v":1,"notification":"ready","data":{...}}

use crate::errors::{CoreError, E_MALFORMED_MESSAGE, E_PROTOCOL_VERSION, E_UNKNOWN_METHOD};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;

/// The complete request catalog (single copy). `protocol.describe` returns
/// this so clients can self-verify.
pub const REQUEST_METHODS: &[&str] = &[
    "hello",
    "protocol.describe",
    "backend.status",
    "config.get",
    "config.set",
    "display.list",
    "display.readInput",
    "display.writeInput",
    "usb.snapshot",
    "usb.watch.start",
    "usb.watch.stop",
    "app.setEnabled",
    "wizard.begin",
    "wizard.end",
    "wizard.candidates",
    "state.get",
    "switch.pushNow",
    "diagnostics.collect",
    "shutdown",
];

/// The complete notification catalog (single copy).
pub const NOTIFICATION_KINDS: &[&str] = &[
    "ready",
    "event",
    "state",
    "trigger",
    "switch.report",
    "wizard.candidates",
    "log",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestLine {
    pub v: u32,
    pub id: u64,
    pub method: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RpcErrorBody {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResponseLine {
    pub v: u32,
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcErrorBody>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NotificationLine {
    pub v: u32,
    pub notification: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub data: Value,
}

impl ResponseLine {
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: u64, code: &str, message: impl Into<String>) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            ok: false,
            result: None,
            error: Some(RpcErrorBody {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
}

impl NotificationLine {
    pub fn new(kind: &str, data: Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            notification: kind.into(),
            data,
        }
    }
}

/// Parse one client line. Version mismatches and unknown methods surface as
/// protocol errors carrying the request id when it could be recovered, so a
/// mismatched client gets a diagnosable response instead of silence.
pub fn parse_client_line(line: &str) -> Result<Result<RequestLine, ResponseLine>, CoreError> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(CoreError::new(E_MALFORMED_MESSAGE, "empty line"));
    }
    let v: Value = serde_json::from_str(trimmed)
        .map_err(|e| CoreError::new(E_MALFORMED_MESSAGE, format!("invalid JSON: {e}")))?;
    let Some(obj) = v.as_object() else {
        return Err(CoreError::new(
            E_MALFORMED_MESSAGE,
            "line is not a JSON object",
        ));
    };
    // Recover the id first so errors can be correlated.
    let id = obj.get("id").and_then(|x| x.as_u64()).unwrap_or(0);
    let version = obj.get("v").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    if version != PROTOCOL_VERSION {
        return Ok(Err(ResponseLine::err(
            id,
            E_PROTOCOL_VERSION,
            format!("protocol version {version} not supported (sidecar speaks {PROTOCOL_VERSION})"),
        )));
    }
    let method = match obj.get("method").and_then(|m| m.as_str()) {
        Some(m) => m.to_string(),
        None => {
            return Ok(Err(ResponseLine::err(
                id,
                E_MALFORMED_MESSAGE,
                "missing method",
            )));
        }
    };
    if !REQUEST_METHODS.contains(&method.as_str()) {
        return Ok(Err(ResponseLine::err(
            id,
            E_UNKNOWN_METHOD,
            format!("unknown method `{method}`; see protocol.describe"),
        )));
    }
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    Ok(Ok(RequestLine {
        v: version,
        id,
        method,
        params,
    }))
}

pub fn encode<T: Serialize>(msg: &T) -> String {
    let mut s = serde_json::to_string(msg).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_a_well_formed_request() {
        let r = parse_client_line(r#"{"v":1,"id":7,"method":"display.list"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(r.method, "display.list");
        assert_eq!(r.id, 7);
        assert!(r.params.is_null());
    }

    #[test]
    fn rejects_wrong_version_with_correlated_error() {
        let outcome = parse_client_line(r#"{"v":2,"id":9,"method":"hello"}"#).unwrap();
        let resp = outcome.unwrap_err();
        assert_eq!(resp.id, 9);
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().code, E_PROTOCOL_VERSION);
    }

    #[test]
    fn rejects_unknown_method() {
        let resp = parse_client_line(r#"{"v":1,"id":3,"method":"nope"}"#)
            .unwrap()
            .unwrap_err();
        assert_eq!(resp.error.unwrap().code, E_UNKNOWN_METHOD);
    }

    #[test]
    fn malformed_json_is_an_error_without_id() {
        let e = parse_client_line("{not json").unwrap_err();
        assert_eq!(e.code, E_MALFORMED_MESSAGE);
    }

    #[test]
    fn response_round_trips() {
        let r = ResponseLine::ok(5, json!({"a": 1}));
        let enc = encode(&r);
        assert!(enc.ends_with('\n'));
        let back: ResponseLine = serde_json::from_str(enc.trim()).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn notification_round_trips() {
        let n = NotificationLine::new("ready", json!({"pid": 42}));
        let back: NotificationLine = serde_json::from_str(&encode(&n)).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn catalog_lists_everything_the_sidecar_implements() {
        assert!(REQUEST_METHODS.contains(&"hello"));
        assert!(REQUEST_METHODS.contains(&"shutdown"));
        assert!(NOTIFICATION_KINDS.contains(&"ready"));
        assert!(NOTIFICATION_KINDS.contains(&"switch.report"));
    }
}
