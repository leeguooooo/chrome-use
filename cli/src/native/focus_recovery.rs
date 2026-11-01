//! Per-invocation authority for native focus recovery.
//!
//! A nested script, confirmed command, or reused daemon cannot grant itself
//! desktop input permission. Only the current root request can grant one permit.

use std::{cell::Cell, future::Future};

use serde_json::{json, Value};

#[derive(Clone, Copy)]
enum Budget {
    Disabled,
    Available,
    Consumed,
}

tokio::task_local! {
    static BUDGET: Cell<Budget>;
}

/// Non-cloneable authority consumed before a native input attempt. A failed or
/// uncertain native acknowledgement must never mint another permit.
pub(super) struct Permit {
    _private: (),
}

/// Establish fresh authority for a root request, rejecting malformed wire input
/// before the future can perform browser or desktop effects.
pub(super) async fn scope(cmd: &Value, operation: impl Future<Output = Value>) -> Value {
    let enabled = match cmd.get("allowFocusRecovery") {
        None | Some(Value::Bool(false)) => false,
        Some(Value::Bool(true)) => true,
        Some(_) => {
            return json!({
                "id": cmd.get("id").cloned().unwrap_or(Value::Null),
                "success": false,
                "error": "allowFocusRecovery must be a boolean",
            });
        }
    };
    let budget = if enabled {
        Budget::Available
    } else {
        Budget::Disabled
    };
    BUDGET.scope(Cell::new(budget), operation).await
}

/// Take the invocation's sole native recovery attempt. Call only after checking
/// live target ownership and the exact blocked page; this permit alone does not
/// establish either fact.
pub(super) fn take_permit() -> Option<Permit> {
    BUDGET
        .try_with(|budget| match budget.get() {
            Budget::Available => {
                budget.set(Budget::Consumed);
                Some(Permit { _private: () })
            }
            Budget::Disabled | Budget::Consumed => None,
        })
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn only_current_root_can_grant_one_attempt() {
        assert!(take_permit().is_none());
        scope(&json!({"allowFocusRecovery": true}), async {
            assert!(take_permit().is_some());
            assert!(take_permit().is_none());
            Value::Null
        })
        .await;
        scope(&json!({}), async {
            assert!(take_permit().is_none());
            Value::Null
        })
        .await;
        assert!(take_permit().is_none());
    }

    #[tokio::test]
    async fn invalid_permission_does_not_poll_operation() {
        for invalid in [json!("true"), json!(1), Value::Null, json!({})] {
            let reply = scope(&json!({"allowFocusRecovery": invalid}), async {
                panic!("malformed authority must stop before browser effects")
            })
            .await;
            assert_eq!(reply["success"], false);
        }
    }

    #[tokio::test]
    async fn permission_is_not_inherited_by_spawned_tasks() {
        scope(&json!({"allowFocusRecovery": true}), async {
            assert!(tokio::spawn(async { take_permit().is_none() })
                .await
                .unwrap());
            assert!(take_permit().is_some());
            Value::Null
        })
        .await;
    }
    #[tokio::test]
    async fn cancellation_removes_authority() {
        let request = json!({"allowFocusRecovery": true});
        let mut operation = Box::pin(scope(&request, async {
            assert!(take_permit().is_some());
            std::future::pending::<Value>().await
        }));
        assert!(futures_util::poll!(&mut operation).is_pending());
        drop(operation);
        assert!(take_permit().is_none());
        scope(&json!({}), async {
            assert!(take_permit().is_none());
            Value::Null
        })
        .await;
    }
}
