//! Advisory loop detection from complete, settled action observations.
//! Stores only a fingerprint, never selectors, field values or screen text.
//! An unchanged accessibility tree is not proof that a write failed: this
//! module warns the caller and never retries, rejects or changes success.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

#[derive(Default)]
pub struct ProgressTracker {
    last: Option<(u64, Instant, u32)>,
}

impl ProgressTracker {
    pub fn reset(&mut self) {
        self.last = None;
    }

    /// Count identical attempts only when the tree and URL were captured,
    /// settling was enabled, and there was no DOM, request or frame activity.
    pub fn observe(&mut self, command: &Value, observation: &Value, screen: &str) -> Option<Value> {
        self.observe_at(command, observation, screen, Instant::now())
    }

    fn observe_at(
        &mut self,
        command: &Value,
        observation: &Value,
        screen: &str,
        now: Instant,
    ) -> Option<Value> {
        let eligible = matches!(
            command["action"].as_str(),
            Some("click" | "dblclick" | "press")
        ) && observation["status"] == "complete"
            && observation["changed"] == false
            && observation["settle"]["quiet"] == true
            && observation["settle"]["sawChange"] == false
            && observation["settle"]["waitedMs"]
                .as_u64()
                .is_some_and(|ms| ms > 0)
            && observation["settle"]["pending"]
                .as_array()
                .is_some_and(|p| p.is_empty())
            && observation["target"]["targetId"].as_str().is_some()
            && !screen.trim().is_empty()
            && observation.get("humanCheck").is_none()
            && observation.get("newFrames").is_none()
            && observation["requestsTotal"].as_u64().unwrap_or(0) == 0
            && observation["resourcesTotal"].as_u64().unwrap_or(0) == 0;
        if !eligible {
            self.reset();
            return None;
        }
        let mut hash = DefaultHasher::new();
        // Exclude transport ids. Include every remaining command parameter and the actual
        // operation and target so distinct coordinates/keys are not one loop.
        let mut operation = command.clone();
        if let Some(fields) = operation.as_object_mut() {
            fields.remove("id");
        }
        operation.to_string().hash(&mut hash);
        observation["target"].to_string().hash(&mut hash);
        screen.hash(&mut hash);
        let fingerprint = hash.finish();
        let count = match self.last {
            Some((previous, at, count))
                if previous == fingerprint
                    && now.saturating_duration_since(at) <= Duration::from_secs(60) =>
            {
                count.saturating_add(1)
            }
            _ => 1,
        };
        self.last = Some((fingerprint, now, count));
        (count >= 3).then(|| json!({
            "kind": "repeated_unchanged_observation",
            "attempts": count,
            "retryAction": false,
            "hint": "Repeated action: no accessibility or request change was observed. Inspect the current state or wait for a task-specific condition; do not blindly replay a send, submit, purchase or delete. This does not prove the action failed."
        }))
    }
}

/// Preserve bounded action advisories through successful and failed nested
/// scripts. Only protocol fields are read, never a script's arbitrary return.
pub fn collect_advisories(response: &Value, advisories: &mut Vec<Value>) {
    if let Some(advisory) = response.pointer("/data/observed/noProgress") {
        if advisories.len() < 20 {
            advisories.push(advisory.clone());
        }
    }
    if let Some(nested) = response
        .pointer("/data/advisories")
        .and_then(Value::as_array)
    {
        advisories.extend(
            nested
                .iter()
                .take(20usize.saturating_sub(advisories.len()))
                .cloned(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet() -> Value {
        json!({"status":"complete", "changed":false,
            "settle":{"quiet":true,"sawChange":false,"waitedMs":100,"pending":[]},
            "target":{"targetId":"tab-1","url":"https://example.test"}})
    }

    #[test]
    fn nested_failure_preserves_only_protocol_advisories() {
        let mut collected = Vec::new();
        let response = json!({"success":true,"data":{"ok":false,"advisories":[
            {"hint":"Inspect state","attempts":3,"retryAction":false}],
            "return":{"advisories":[{"hint":"untrusted return text"}]}}});
        collect_advisories(&response, &mut collected);
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0]["attempts"], 3);
    }

    #[test]
    fn warns_on_third_identical_attempt_without_changing_success() {
        let mut tracker = ProgressTracker::default();
        let mut cmd = json!({"action":"click","selector":"@e1","id":"first"});
        let obs = quiet();
        assert!(tracker.observe(&cmd, &obs, "Save").is_none());
        cmd["id"] = json!("second");
        assert!(tracker.observe(&cmd, &obs, "Save").is_none());
        let warning = tracker.observe(&cmd, &obs, "Save").unwrap();
        assert_eq!(warning["attempts"], 3);
        assert_eq!(warning["retryAction"], false);
        assert!(warning.get("selector").is_none());
    }

    #[test]
    fn changed_action_parameters_do_not_share_a_streak() {
        for field in ["hold", "allowDom", "follow", "newTab", "x", "y"] {
            let mut tracker = ProgressTracker::default();
            let mut command = json!({"action":"press","key":"Enter"});
            command[field] = json!(100);
            tracker.observe(&command, &quiet(), "Save");
            tracker.observe(&command, &quiet(), "Save");
            command[field] = json!(300);
            assert!(tracker.observe(&command, &quiet(), "Save").is_none());
        }
    }

    #[test]
    fn activity_and_incomplete_evidence_break_the_streak() {
        let cmd = json!({"action":"click","selector":"@e1"});
        for (path, value) in [
            ("/status", json!("partial")),
            ("/changed", json!(true)),
            ("/settle/quiet", json!(false)),
            ("/settle/sawChange", json!(true)),
            ("/settle/waitedMs", json!(0)),
            ("/settle/pending", json!(["network"])),
            ("/target/targetId", Value::Null),
        ] {
            let mut tracker = ProgressTracker::default();
            tracker.observe(&cmd, &quiet(), "Save");
            tracker.observe(&cmd, &quiet(), "Save");
            let mut busy = quiet();
            *busy.pointer_mut(path).unwrap() = value;
            assert!(tracker.observe(&cmd, &busy, "Save").is_none());
            assert!(tracker.observe(&cmd, &quiet(), "Save").is_none());
        }
        for key in ["requestsTotal", "resourcesTotal", "humanCheck", "newFrames"] {
            let mut tracker = ProgressTracker::default();
            tracker.observe(&cmd, &quiet(), "Save");
            tracker.observe(&cmd, &quiet(), "Save");
            let mut busy = quiet();
            busy[key] = json!(1);
            assert!(tracker.observe(&cmd, &busy, "Save").is_none());
        }
    }

    #[test]
    fn distinct_targets_screens_operations_and_idle_time_are_not_a_loop() {
        let cmd = json!({"action":"click","selector":"@e1"});
        let start = Instant::now();
        let mut tracker = ProgressTracker::default();
        tracker.observe_at(&cmd, &quiet(), "Save", start);
        tracker.observe_at(&cmd, &quiet(), "Save", start);
        assert!(tracker
            .observe_at(&cmd, &quiet(), "Save", start + Duration::from_secs(61))
            .is_none());
        assert!(tracker.observe(&cmd, &quiet(), "Saved").is_none());
        let mut tab = quiet();
        tab["target"]["targetId"] = json!("tab-2");
        assert!(tracker.observe(&cmd, &tab, "Saved").is_none());
        assert!(tracker
            .observe(&json!({"action":"press","key":"Enter"}), &tab, "Saved")
            .is_none());
        assert!(tracker
            .observe(&json!({"action":"fill","selector":"@e1"}), &tab, "Saved")
            .is_none());
    }
}
