use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

const FEEDBACK_URL: &str = "https://api.kvmflow.net/feedback";
const MAX_BODY: usize = 64 * 1024;
const MAX_DIAGNOSTICS: usize = 48 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackError {
    code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after: Option<u64>,
}
impl FeedbackError {
    fn new(code: &'static str) -> Self {
        Self {
            code,
            retry_after: None,
        }
    }
}
#[derive(Debug, Serialize, PartialEq)]
pub struct Receipt {
    ok: bool,
    id: String,
}

fn object(value: &Value, keys: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|map| map.len() == keys.len() && keys.iter().all(|key| map.contains_key(*key)))
}
fn text(value: &Value, max: usize, controls: bool) -> bool {
    value.as_str().is_some_and(|s| {
        s.chars().count() <= max && (!controls || !s.chars().any(char::is_control))
    })
}
fn nullable_text(value: &Value, max: usize) -> bool {
    value.is_null() || text(value, max, true)
}
fn input(value: &Value) -> bool {
    value.as_u64().is_some_and(|value| value <= 65535)
}
fn one_of(value: &Value, values: &[&str]) -> bool {
    value.as_str().is_some_and(|value| values.contains(&value))
}
fn hex_id(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| s.len() == 4 && s.bytes().all(|c| c.is_ascii_hexdigit()))
}

// Validate before constructing a client. Unknown fields at every level are
// rejected locally, so an address or unreviewed diagnostic field cannot be added.
fn encode_payload(payload: &Value) -> Result<Vec<u8>, FeedbackError> {
    let bytes = serde_json::to_vec(payload).map_err(|_| FeedbackError::new("invalid_feedback"))?;
    if bytes.len() > MAX_BODY {
        return Err(FeedbackError::new("too_large"));
    }
    let r = &payload["report"];
    let valid = object(
        payload,
        &[
            "schema",
            "kind",
            "message",
            "contact",
            "report",
            "diagnostics",
        ],
    ) && payload["schema"].as_u64() == Some(1)
        && one_of(&payload["kind"], &["works", "problem"])
        && text(&payload["message"], 2000, false)
        && text(&payload["contact"], 200, true)
        && (payload["diagnostics"].is_null()
            || payload["diagnostics"]
                .as_str()
                .is_some_and(|s| s.len() <= MAX_DIAGNOSTICS))
        && object(r, &["app", "os", "mode", "monitors", "usb", "lastSwitch"])
        && object(&r["app"], &["version", "channel"])
        && text(&r["app"]["version"], MAX_BODY, true)
        && one_of(&r["app"]["channel"], &["stable", "beta"])
        && object(&r["os"], &["platform", "version", "arch"])
        && one_of(&r["os"]["platform"], &["macos", "windows"])
        && text(&r["os"]["version"], MAX_BODY, true)
        && text(&r["os"]["arch"], MAX_BODY, true)
        && one_of(&r["mode"], &["normal", "mixed"])
        && r["monitors"].as_array().is_some_and(|monitors| {
            monitors.len() <= 8
                && monitors.iter().all(|m| {
                    object(m, &["vendor", "model", "ddc", "vcp60", "inputs"])
                        && nullable_text(&m["vendor"], 64)
                        && nullable_text(&m["model"], 64)
                        && (m["ddc"].is_null() || m["ddc"].is_boolean())
                        && (m["vcp60"].is_null() || input(&m["vcp60"]))
                        && m["inputs"].as_array().is_some_and(|inputs| {
                            inputs.len() <= 4
                                && inputs.iter().all(|i| {
                                    object(i, &["computer", "value", "source"])
                                        && matches!(i["computer"].as_u64(), Some(1 | 2))
                                        && input(&i["value"])
                                        && one_of(&i["source"], &["read", "manual"])
                                })
                        })
                })
        })
        && (r["usb"].is_null()
            || (object(&r["usb"], &["vendor", "product", "vid", "pid"])
                && text(&r["usb"]["vendor"], MAX_BODY, true)
                && text(&r["usb"]["product"], MAX_BODY, true)
                && hex_id(&r["usb"]["vid"])
                && hex_id(&r["usb"]["pid"])))
        && (r["lastSwitch"].is_null()
            || (object(&r["lastSwitch"], &["accepted", "readback"])
                && r["lastSwitch"]["accepted"].is_boolean()
                && input(&r["lastSwitch"]["readback"])));
    if !valid {
        return Err(FeedbackError::new("invalid_feedback"));
    }
    Ok(bytes)
}

fn client(timeout: Duration) -> Result<reqwest::Client, FeedbackError> {
    // Same reqwest 0.13 / rustls 0.23 ring provider as tauri-plugin-updater.
    // Feedback can be the first HTTPS operation, before updater initialization.
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    let builder = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none());
    // Local fixtures must never route through a machine's system HTTP proxy.
    #[cfg(test)]
    let builder = builder.no_proxy();
    builder.build().map_err(|_| FeedbackError::new("network"))
}

async fn send(
    client: reqwest::Client,
    endpoint: &str,
    body: Vec<u8>,
    version: &str,
) -> Result<Receipt, FeedbackError> {
    let mut response = client
        .post(endpoint)
        .header("content-type", "application/json")
        .header("x-kvmflow-client", format!("desktop/{version}"))
        .body(body)
        .send()
        .await
        .map_err(|_| FeedbackError::new("network"))?;
    let status = response.status().as_u16();
    // Bound even an unexpected response, and never propagate server text to UI.
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| FeedbackError::new("network"))?
    {
        if bytes.len() + chunk.len() > 4096 {
            return Err(FeedbackError::new("invalid_response"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    match status {
        201 => {
            let id = value["id"].as_str().unwrap_or("");
            if value["ok"] == true
                && id.len() == 29
                && id.starts_with("fb_")
                && id[3..].bytes().all(|c| c.is_ascii_alphanumeric())
            {
                Ok(Receipt {
                    ok: true,
                    id: id.into(),
                })
            } else {
                Err(FeedbackError::new("invalid_response"))
            }
        }
        400 => Err(FeedbackError::new("invalid_feedback")),
        413 => Err(FeedbackError::new("too_large")),
        429 => Err(FeedbackError {
            code: "rate_limited",
            retry_after: value["retryAfter"].as_u64(),
        }),
        _ => Err(FeedbackError::new("unavailable")),
    }
}

#[tauri::command]
pub async fn feedback_send(
    app: tauri::AppHandle,
    payload: Value,
) -> Result<Receipt, FeedbackError> {
    let bytes = encode_payload(&payload)?;
    send(
        client(TIMEOUT)?,
        FEEDBACK_URL,
        bytes,
        &app.package_info().version.to_string(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    fn payload() -> Value {
        json!({"schema":1,"kind":"works","message":"","contact":"","diagnostics":null,
            "report":{"app":{"version":"0.4.0-beta.1","channel":"beta"},"os":{"platform":"macos","version":"15.7.1","arch":"aarch64"},"mode":"normal",
            "monitors":[{"vendor":"DEL","model":"U2723QE","ddc":true,"vcp60":15,"inputs":[{"computer":1,"value":15,"source":"read"}]}],
            "usb":{"vendor":"UGREEN","product":"USB Switch","vid":"1234","pid":"5678"},"lastSwitch":{"accepted":true,"readback":15}}})
    }

    #[test]
    fn schema_and_utf8_limits_are_enforced_before_transport() {
        let mut p = payload();
        p["message"] = json!("中".repeat(2000));
        p["contact"] = json!("not-an-email"); // Contract deliberately permits it.
        assert!(encode_payload(&p).is_ok());
        p["message"] = json!("🙂".repeat(2001));
        assert_eq!(encode_payload(&p).unwrap_err().code, "invalid_feedback");
        p = payload();
        p["contact"] = json!("hello\nworld");
        assert!(encode_payload(&p).is_err());
        p = payload();
        p["diagnostics"] = json!("中".repeat(MAX_DIAGNOSTICS / 3));
        assert!(encode_payload(&p).is_ok());
        p["diagnostics"] = json!("中".repeat(MAX_DIAGNOSTICS / 3 + 1));
        assert!(encode_payload(&p).is_err());
        p = payload();
        p["report"]["os"]["version"] = json!("");
        let padding = MAX_BODY - serde_json::to_vec(&p).unwrap().len();
        p["report"]["os"]["version"] = json!("a".repeat(padding));
        assert_eq!(encode_payload(&p).unwrap().len(), MAX_BODY);
        p["report"]["os"]["version"] = json!("a".repeat(padding + 1));
        assert_eq!(encode_payload(&p).unwrap_err().code, "too_large");
    }

    #[test]
    fn unknown_fields_and_invalid_nested_values_are_rejected() {
        for pointer in [
            "",
            "/report",
            "/report/app",
            "/report/os",
            "/report/usb",
            "/report/lastSwitch",
            "/report/monitors/0",
            "/report/monitors/0/inputs/0",
        ] {
            let mut p = payload();
            p.pointer_mut(pointer).unwrap()["private"] = json!("should never be posted");
            assert!(
                encode_payload(&p).is_err(),
                "accepted extra field at {pointer}"
            );
        }
        for (pointer, value) in [
            ("/url", json!("https://other.test")),
            ("/schema", json!(2)),
            ("/kind", json!("other")),
            ("/report/os/platform", json!("linux")),
            ("/report/app/channel", json!("dev")),
            ("/report/monitors/0/vendor", json!("a".repeat(65))),
            ("/report/monitors/0/vcp60", json!(65536)),
            ("/report/monitors/0/inputs/0/computer", json!(3)),
            ("/report/monitors/0/inputs/0/source", json!("guessed")),
            ("/report/usb/vid", json!("12345")),
        ] {
            let mut p = payload();
            if pointer == "/url" {
                p["url"] = value;
            } else {
                *p.pointer_mut(pointer).unwrap() = value;
            }
            assert!(encode_payload(&p).is_err(), "accepted invalid {pointer}");
        }
        let mut p = payload();
        p["report"]["monitors"] = json!(vec![p["report"]["monitors"][0].clone(); 9]);
        assert!(encode_payload(&p).is_err());
        let mut p = payload();
        p["report"]["monitors"][0]["inputs"] =
            json!(vec![p["report"]["monitors"][0]["inputs"][0].clone(); 5]);
        assert!(encode_payload(&p).is_err());
        let mut p = payload();
        p["report"]["usb"] = Value::Null;
        p["report"]["lastSwitch"] = Value::Null;
        for key in ["vendor", "model", "ddc", "vcp60"] {
            p["report"]["monitors"][0][key] = Value::Null;
        }
        assert!(encode_payload(&p).is_ok());
    }

    fn fixture(
        status: u16,
        body: String,
        delay: Duration,
    ) -> (String, mpsc::Receiver<Vec<u8>>, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/feedback", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0; 2048];
                let count = socket.read(&mut chunk).unwrap();
                assert!(count > 0);
                request.extend_from_slice(&chunk[..count]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    let len: usize = header
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if request.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            tx.send(request).unwrap();
            std::thread::sleep(delay);
            let response = format!("HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nLocation: https://never-follow.invalid/\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = socket.write_all(response.as_bytes());
        });
        (url, rx, worker)
    }

    #[test]
    fn mock_transport_posts_exact_payload_with_client_header_and_validates_receipt() {
        tauri::async_runtime::block_on(async {
            let p = payload();
            let (url, rx, worker) = fixture(
                201,
                r#"{"ok":true,"id":"fb_01JABCDEF0123456789ABCDEFG"}"#.into(),
                Duration::ZERO,
            );
            let result = send(
                client(TIMEOUT).unwrap(),
                &url,
                encode_payload(&p).unwrap(),
                "0.4.0-beta.1",
            )
            .await
            .unwrap();
            assert_eq!(result.id, "fb_01JABCDEF0123456789ABCDEFG");
            let request = rx.recv().unwrap();
            let end = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            let header = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
            assert!(header.starts_with("post /feedback http/1.1"));
            assert!(header.contains("content-type: application/json"));
            assert!(header.contains("x-kvmflow-client: desktop/0.4.0-beta.1"));
            assert_eq!(
                serde_json::from_slice::<Value>(&request[end + 4..]).unwrap(),
                p
            );
            worker.join().unwrap();
        });
    }

    #[test]
    fn mock_http_failures_do_not_follow_redirects_or_expose_response_text() {
        tauri::async_runtime::block_on(async {
            for (status, body, code) in [
                (
                    400,
                    r#"{"error":{"code":"invalid_feedback","field":"report"}}"#,
                    "invalid_feedback",
                ),
                (413, "{}", "too_large"),
                (429, r#"{"retryAfter":60}"#, "rate_limited"),
                (500, "/Users/private backend error", "unavailable"),
                (302, "{}", "unavailable"),
                (201, r#"{"ok":true,"id":"unsafe-id"}"#, "invalid_response"),
                (
                    201,
                    r#"{"ok":false,"id":"fb_01JABCDEF0123456789ABCDEFG"}"#,
                    "invalid_response",
                ),
            ] {
                let (url, _rx, worker) = fixture(status, body.into(), Duration::ZERO);
                let error = send(
                    client(TIMEOUT).unwrap(),
                    &url,
                    encode_payload(&payload()).unwrap(),
                    "0.4.0",
                )
                .await
                .unwrap_err();
                assert_eq!(error.code, code);
                assert_eq!(
                    error.retry_after,
                    if status == 429 { Some(60) } else { None }
                );
                worker.join().unwrap();
            }
        });
    }

    #[test]
    fn mock_timeout_and_connection_failure_remain_retryable() {
        assert_eq!(TIMEOUT, Duration::from_secs(10));
        assert_eq!(FEEDBACK_URL, "https://api.kvmflow.net/feedback");
        tauri::async_runtime::block_on(async {
            let (url, _rx, worker) = fixture(201, "{}".into(), Duration::from_millis(150));
            let error = send(
                client(Duration::from_millis(50)).unwrap(),
                &url,
                encode_payload(&payload()).unwrap(),
                "0.4.0",
            )
            .await
            .unwrap_err();
            assert_eq!(error.code, "network");
            worker.join().unwrap();
            // The fixture's listener is now closed; no external server is used.
            let error = send(
                client(TIMEOUT).unwrap(),
                &url,
                encode_payload(&payload()).unwrap(),
                "0.4.0",
            )
            .await
            .unwrap_err();
            assert_eq!(error.code, "network");
        });
    }
}
