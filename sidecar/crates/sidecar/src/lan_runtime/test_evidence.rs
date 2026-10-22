//! 「试一下」evidence aggregation (S9).
//!
//! Both computers report their own view of one physical switch press:
//! the host that sent the DDC commands ("commands_sent") and the host that
//! observed the USB group move ("picture_arrived"/"display_left"). Every
//! report is tagged with the test window's `test_id` and a `press_id`; the
//! receiving host re-tags its own recent report with the sender's
//! `press_id` once it learns it, so both ends' evidence ends up under the
//! same key regardless of arrival order. A press id is `<device>:<n>`,
//! unique inside one test window.
use serde_json::{json, Value};

/// How long a local report stays eligible for re-tagging under a press id
/// learned from the other computer (covers the USB/DDC/network ordering
/// spread; deliberately much shorter than a user-driven second press).
pub(crate) const ATTACH_WINDOW_MS: u64 = 5_000;
const RECENT_LIMIT: usize = 8;

struct Recent {
    body: Value,
    target: String,
    press_id: String,
    ts_ms: u64,
    attached: bool,
}

pub(crate) struct TestEvidence {
    pub test_id: String,
    pub ends_ms: u64,
    press_seq: u64,
    pub events: Vec<Value>,
    last_remote_press: Option<String>,
    recent: Vec<Recent>,
    pending_observations: Vec<String>,
}

impl TestEvidence {
    pub fn start(test_id: impl Into<String>, now_ms: u64, duration_ms: u64) -> Self {
        Self {
            test_id: test_id.into(),
            ends_ms: now_ms.saturating_add(duration_ms),
            press_seq: 0,
            events: Vec::new(),
            last_remote_press: None,
            recent: Vec::new(),
            pending_observations: Vec::new(),
        }
    }

    pub fn summary(&self) -> Value {
        json!({"test_id": self.test_id, "events": self.events})
    }

    fn push_event(&mut self, device_id: &str, kind: &str, body: Value) {
        self.events.push(json!({
            "device_id": device_id,
            "kind": kind,
            "body": body,
        }));
        if self.events.len() > 512 {
            self.events.remove(0);
        }
    }

    /// Annotate one of THIS host's `switch.report`s with the active test's
    /// correlation fields. Returns the frame to broadcast to the peer.
    pub fn stamp_local_report(
        &mut self,
        mut report: Value,
        local_id: &str,
        now_ms: u64,
    ) -> Option<Value> {
        if report.get("test_id").is_some() {
            return None;
        }
        self.press_seq = self.press_seq.saturating_add(1);
        let press_id = format!("{local_id}:{}", self.press_seq);
        let target = report
            .get("target_device_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let kind = if target == local_id {
            "picture_arrived"
        } else {
            "commands_sent"
        };
        if let Some(obj) = report.as_object_mut() {
            obj.insert("test_id".into(), json!(self.test_id));
            obj.insert("press_id".into(), json!(press_id));
            obj.insert("reporting_device_id".into(), json!(local_id));
            obj.insert("test_kind".into(), json!(kind));
            obj.insert("ts_ms".into(), json!(now_ms));
        }
        self.push_event(local_id, "switch.report", report.clone());
        if target == local_id {
            self.recent.push(Recent {
                body: report.clone(),
                target,
                press_id,
                ts_ms: now_ms,
                attached: false,
            });
            if self.recent.len() > RECENT_LIMIT {
                self.recent.remove(0);
            }
        }
        Some(report)
    }

    /// Accept the peer's streamed evidence. When it names THIS host as the
    /// target, re-tag our own recent report (the arrival-correction one)
    /// under the sender's press id and return the frame to broadcast.
    pub fn accept_remote_event(
        &mut self,
        from: &str,
        body: Value,
        local_id: &str,
        now_ms: u64,
    ) -> Vec<Value> {
        let remote_press = body
            .get("press_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let targets_local = body.get("target_device_id").and_then(Value::as_str) == Some(local_id);
        let from_peer = body.get("reporting_device_id").and_then(Value::as_str) != Some(local_id);
        let mut out = Vec::new();
        if targets_local && from_peer {
            if let Some(press) = remote_press.clone() {
                self.last_remote_press = Some(press);
            }
        }
        self.push_event(from, "test.event", body);
        if targets_local && from_peer {
            if let Some(press) = remote_press {
                let mut tagged = Vec::new();
                for recent in self.recent.iter_mut() {
                    if recent.attached
                        || recent.target != local_id
                        || recent.press_id == press
                        || now_ms.saturating_sub(recent.ts_ms) > ATTACH_WINDOW_MS
                    {
                        continue;
                    }
                    recent.attached = true;
                    let mut body = recent.body.clone();
                    if let Some(obj) = body.as_object_mut() {
                        obj.insert("test_id".into(), json!(self.test_id));
                        obj.insert("press_id".into(), json!(press));
                        obj.insert("reporting_device_id".into(), json!(local_id));
                        obj.insert("test_kind".into(), json!("picture_arrived"));
                    }
                    tagged.push(body);
                }
                for body in tagged {
                    self.push_event(local_id, "switch.report", body.clone());
                    out.push(body);
                }
                // Observations that happened before we learned the press id.
                let pending = std::mem::take(&mut self.pending_observations);
                for kind in pending {
                    let tagged = self.observation_body(&kind, local_id, now_ms);
                    self.push_event(local_id, "test.event", tagged.clone());
                    out.push(tagged);
                }
            }
        }
        out
    }

    fn observation_body(&self, kind: &str, local_id: &str, now_ms: u64) -> Value {
        json!({
            "test_id": self.test_id,
            "press_id": self.last_remote_press,
            "target_device_id": local_id,
            "reporting_device_id": local_id,
            "test_kind": kind,
            "ts_ms": now_ms,
        })
    }

    /// Local USB observation (`picture_arrived` / `display_left`). Without a
    /// known press id it is queued and emitted when the peer's press arrives;
    /// it never invents a correlation.
    pub fn record_observation(&mut self, kind: &str, local_id: &str, now_ms: u64) -> Option<Value> {
        if self.last_remote_press.is_none() {
            self.pending_observations.push(kind.to_string());
            return None;
        }
        let body = self.observation_body(kind, local_id, now_ms);
        self.push_event(local_id, "test.event", body.clone());
        Some(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_ends_reports_land_under_the_senders_press_id_even_out_of_order() {
        // A (initiator) pushes to B: its own report is the commands evidence.
        let mut a = TestEvidence::start("t1", 0, 60_000);
        let a_frame = a
            .stamp_local_report(
                json!({"target_device_id": "dev-b", "per_monitor": [{"edid_id": "d1", "commanded": true}]}),
                "dev-a",
                10,
            )
            .expect("annotated");
        assert_eq!(a_frame["test_id"], "t1");
        assert_eq!(a_frame["press_id"], "dev-a:1");

        // B's arrival-correction report happened BEFORE A's frame arrived.
        let mut b = TestEvidence::start("t1", 0, 60_000);
        b.stamp_local_report(
            json!({"target_device_id": "dev-b", "per_monitor": [{"edid_id": "d1", "commanded": true}]}),
            "dev-b",
            9,
        );
        let resend = b.accept_remote_event("dev-a", a_frame.clone(), "dev-b", 11);
        assert_eq!(resend.len(), 1, "B re-tags its report under A's press");
        assert_eq!(resend[0]["press_id"], "dev-a:1");
        assert_eq!(resend[0]["reporting_device_id"], "dev-b");

        // A receives B's re-tagged evidence under its own press id.
        a.accept_remote_event("dev-b", resend[0].clone(), "dev-a", 12);
        let under_press: Vec<_> = a
            .events
            .iter()
            .filter(|e| e["body"]["press_id"] == "dev-a:1")
            .collect();
        let devices: std::collections::BTreeSet<_> = under_press
            .iter()
            .map(|e| e["device_id"].as_str().unwrap())
            .collect();
        assert_eq!(
            devices.into_iter().collect::<Vec<_>>(),
            vec!["dev-a", "dev-b"]
        );
    }

    #[test]
    fn arrival_observation_without_a_known_press_is_correlated_later() {
        let mut b = TestEvidence::start("t2", 0, 60_000);
        assert!(b
            .record_observation("picture_arrived", "dev-b", 5)
            .is_none());
        let frame = json!({"test_id": "t2", "press_id": "dev-a:7", "target_device_id": "dev-b", "reporting_device_id": "dev-a", "per_monitor": []});
        let out = b.accept_remote_event("dev-a", frame, "dev-b", 6);
        assert!(out
            .iter()
            .any(|f| f["test_kind"] == "picture_arrived" && f["press_id"] == "dev-a:7"));
    }

    #[test]
    fn an_old_report_is_not_retagged_as_a_new_press() {
        let mut b = TestEvidence::start("t3", 0, 60_000);
        b.stamp_local_report(json!({"target_device_id": "dev-b", "per_monitor": [{"edid_id": "d1", "commanded": true}]}), "dev-b", 0);
        let frame = json!({"test_id": "t3", "press_id": "dev-a:1", "target_device_id": "dev-b", "reporting_device_id": "dev-a", "per_monitor": []});
        let out = b.accept_remote_event("dev-a", frame, "dev-b", ATTACH_WINDOW_MS + 1);
        assert!(
            out.is_empty(),
            "stale local evidence must not be relabelled"
        );
    }
}
