//! Merge full and partial runtime snapshots without losing newer fields.
use serde_json::{json, Value};
use std::collections::HashMap;

pub struct Snapshot {
    value: Value,
    generation: u64,
    field_sequences: HashMap<String, u64>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            value: json!({}),
            generation: 0,
            field_sequences: HashMap::new(),
        }
    }
}

impl Snapshot {
    pub fn value(&self) -> &Value {
        &self.value
    }

    // runtime.error is an unsequenced event, not a state snapshot. Preserve
    // field cursors so a delayed snapshot cannot clear this error.
    pub fn mark_error(&mut self, incoming: &Value) -> bool {
        if !self.accept_generation(incoming) {
            return false;
        }
        self.value["state"] = json!("error");
        true
    }

    fn accept_generation(&mut self, incoming: &Value) -> bool {
        let generation = incoming["runtime_generation"]
            .as_u64()
            .or_else(|| incoming["mixed"]["runtime_generation"].as_u64())
            .unwrap_or(0);
        if generation < self.generation {
            return false;
        }
        if generation > self.generation {
            self.generation = generation;
            self.value = json!({});
            self.field_sequences.clear();
        }
        true
    }
}

pub fn merge_snapshot(current: &mut Snapshot, incoming: &Value) -> bool {
    let Some(fields) = incoming.as_object() else {
        return false;
    };
    if !current.accept_generation(incoming) {
        return false;
    }
    let incoming_seq = incoming["runtime_seq"]
        .as_u64()
        .or_else(|| incoming["mixed"]["runtime_seq"].as_u64());
    let mut accepted = false;
    for (field, value) in fields {
        if matches!(
            field.as_str(),
            "runtime_seq" | "runtime_generation" | "saved" | "applied"
        ) {
            continue;
        }
        if let Some(latest) = current.field_sequences.get(field) {
            if incoming_seq.is_none_or(|seq| seq < *latest) {
                continue;
            }
        }
        current.value[field] = value.clone();
        if let Some(seq) = incoming_seq {
            current.field_sequences.insert(field.clone(), seq);
        }
        accepted = true;
    }
    accepted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mixed_menu;
    use serde_json::json;
    fn snapshot(value: Value) -> Snapshot {
        let mut state = Snapshot::default();
        merge_snapshot(&mut state, &value);
        state
    }
    #[test]
    fn pausing_keeps_mixed_checkbox_assignment_and_enablement() {
        let mut state = snapshot(
            json!({"state":"idle","enabled":true,"mixed":{"enabled":true,"validation":{"ok":true},"assignment":[{"fingerprint":"a","device_id":"pc"}]}}),
        );
        merge_snapshot(&mut state, &json!({"state":"disabled","enabled":false}));
        assert_eq!(state.value()["enabled"], false);
        assert_eq!(state.value()["mixed"]["enabled"], true);
        assert!(mixed_menu::can_toggle(&state.value()["mixed"]));
        assert!(
            mixed_menu::assignment(&json!({}), &state.value()["mixed"], &str::to_owned).is_some()
        );
    }
    #[test]
    fn stale_full_snapshot_cannot_revert_mode_or_emit_an_extra_notice() {
        let mut state = snapshot(
            json!({"runtime_seq":30,"state":"idle","enabled":true,"mixed":{"enabled":true,"keyboard_holder_device_id":"mac"}}),
        );
        let stale = serde_json::json!({"runtime_seq":20,"state":"idle","enabled":true,"mixed":{"enabled":false,"keyboard_holder_device_id":"pc"}});
        let mut notice_mode = Some(true);
        if merge_snapshot(&mut state, &stale) {
            assert!(!crate::mixed_menu::should_notify(
                &mut notice_mode,
                &state.value()["mixed"]
            ));
        }
        assert_eq!(state.value()["mixed"]["enabled"], true);
        assert_eq!(state.value()["mixed"]["keyboard_holder_device_id"], "mac");
        assert_eq!(notice_mode, Some(true));
        assert!(merge_snapshot(
            &mut state,
            &serde_json::json!({"runtime_seq":31,"enabled":false})
        ));
        assert_eq!(state.value()["mixed"]["enabled"], true);
        assert_eq!(state.value()["enabled"], false);
    }

    #[test]
    fn mixed_response_does_not_hide_delayed_pause_fields() {
        let mut state = snapshot(
            json!({"runtime_seq":1,"state":"idle","enabled":true,"mixed":{"enabled":false}}),
        );
        merge_snapshot(
            &mut state,
            &json!({"runtime_seq":5,"mixed":{"enabled":true,"keyboard_holder_device_id":"mac"}}),
        );
        merge_snapshot(
            &mut state,
            &json!({"runtime_seq":4,"state":"disabled","enabled":false,"mixed":{"enabled":false}}),
        );
        assert_eq!(state.value()["enabled"], false);
        assert_eq!(state.value()["mixed"]["enabled"], true);
        assert_eq!(state.value()["mixed"]["keyboard_holder_device_id"], "mac");
    }

    #[test]
    fn filling_mixed_fields_does_not_undo_a_newer_pause() {
        let mut state = snapshot(
            json!({"runtime_seq":1,"state":"idle","enabled":true,"mixed":{"enabled":false}}),
        );
        merge_snapshot(
            &mut state,
            &json!({"runtime_seq":6,"state":"disabled","enabled":false}),
        );
        merge_snapshot(
            &mut state,
            &json!({"runtime_seq":5,"state":"idle","enabled":true,"mixed":{"enabled":true}}),
        );
        assert_eq!(state.value()["mixed"]["enabled"], true);
        assert_eq!(state.value()["enabled"], false);
    }

    #[test]
    fn new_generation_resets_sequences_and_rejects_old_worker_fields() {
        let mut state =
            snapshot(json!({"runtime_seq":90,"enabled":true,"mixed":{"enabled":false}}));
        assert!(merge_snapshot(
            &mut state,
            &json!({"runtime_generation":1,"runtime_seq":2,"mixed":{"enabled":true}})
        ));
        assert!(state.value().get("enabled").is_none());
        merge_snapshot(
            &mut state,
            &json!({"runtime_generation":1,"runtime_seq":1,"enabled":false,"mixed":{"enabled":false}}),
        );
        assert_eq!(state.value()["enabled"], false);
        assert_eq!(state.value()["mixed"]["enabled"], true);
        assert!(!merge_snapshot(
            &mut state,
            &json!({"runtime_generation":0,"runtime_seq":999,"enabled":true,"lan":{}})
        ));
        assert!(!merge_snapshot(
            &mut state,
            &json!({"runtime_seq":1000,"mixed":{"enabled":false}})
        ));
        assert!(state.value().get("lan").is_none());
        assert_eq!(state.value()["enabled"], false);
        assert_eq!(state.value()["mixed"]["enabled"], true);
    }

    #[test]
    fn nested_generation_starts_a_new_sequence_domain() {
        let mut state =
            snapshot(json!({"runtime_generation":3,"runtime_seq":90,"mixed":{"enabled":false}}));
        assert!(merge_snapshot(
            &mut state,
            &json!({"mixed":{"runtime_generation":4,"runtime_seq":1,"enabled":true}})
        ));
        assert_eq!(state.value()["mixed"]["enabled"], true);
        assert!(!merge_snapshot(
            &mut state,
            &json!({"mixed":{"runtime_generation":3,"runtime_seq":999,"enabled":false}})
        ));
        assert_eq!(state.value()["mixed"]["enabled"], true);
    }

    #[test]
    fn unsequenced_runtime_error_remains_visible_after_a_snapshot() {
        let mut state = snapshot(
            json!({"runtime_seq":5,"state":"idle","enabled":true,"mixed":{"enabled":true}}),
        );
        assert!(state.mark_error(&json!({})));
        assert_eq!(state.value()["state"], "error");
        assert_eq!(state.value()["mixed"]["enabled"], true);
        merge_snapshot(&mut state, &json!({"runtime_seq":4,"state":"idle"}));
        assert_eq!(state.value()["state"], "error");
        merge_snapshot(&mut state, &json!({"runtime_seq":6,"state":"idle"}));
        assert_eq!(state.value()["state"], "idle");
        merge_snapshot(
            &mut state,
            &json!({"runtime_generation":1,"runtime_seq":1,"state":"idle"}),
        );
        assert!(!state.mark_error(&json!({"runtime_generation":0})));
        assert_eq!(state.value()["state"], "idle");
        assert!(state.mark_error(&json!({"runtime_generation":1})));
        assert_eq!(state.value()["state"], "error");
    }
}
