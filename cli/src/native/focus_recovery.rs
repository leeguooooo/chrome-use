//! Per-invocation authority for native focus recovery.
//!
//! A nested script, confirmed command, or reused daemon cannot grant itself
//! desktop input permission. Only the current root request can grant one permit.

use std::{
    cell::{Cell, RefCell},
    future::Future,
    sync::Arc,
};

use serde_json::{json, Value};

mod authority;
mod documents;
mod host;
#[cfg(target_os = "macos")]
mod macos_probe;
pub(crate) use host::NativeFocusHost;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
mod windows_desktop;
#[cfg(windows)]
mod windows_parent;

#[derive(Clone, Copy)]
enum Budget {
    Disabled,
    Available,
    Consumed,
}

tokio::task_local! {
    static BUDGET: Cell<Budget>;
    static LEASE: RefCell<Option<ClientLease>>;
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
    BUDGET.scope(Cell::new(budget), LEASE.scope(RefCell::new(None), async {
        let mut reply = operation.await;
        let lease = LEASE.with(|slot| slot.borrow_mut().take());
        if let Some(lease) = lease {
            let input = lease.input.clone();
            let finished = lease.finish().await;
            let restored = finished.as_ref().ok().and_then(|value| value.pointer("/restore/restored")).and_then(Value::as_bool) == Some(true);
            let note = if finished.as_ref().ok().and_then(|value| value.get("focusRequested")).and_then(Value::as_bool) == Some(false) {
                "Native recovery stopped before requesting focus."
            } else if restored { "Native focus recovery was attempted; the original foreground was verified." }
                else { "Native focus recovery was attempted; original foreground restoration is unconfirmed. No native input was repeated." };
            if let Some(object) = reply.as_object_mut() {
                let warning = object.get("warning").and_then(Value::as_str)
                    .map(|existing| format!("{existing}\n{note}")).unwrap_or_else(|| note.to_string());
                object.insert("warning".into(), json!(warning));
                object.insert("nativeFocus".into(), json!({"input":input,"restore":finished.ok(),"originalOperationReplayed":false}));
            }
        }
        reply
    })).await
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

struct ClientLease {
    client: Arc<super::cdp::client::CdpClient>,
    token: String,
    input: Value,
    armed: bool,
}
impl ClientLease {
    async fn finish(mut self) -> Result<Value, String> {
        self.armed = false;
        self.client
            .send_command(
                "ABExt.nativeFocusFinish",
                Some(json!({"token":self.token})),
                None,
            )
            .await
    }
}
impl Drop for ClientLease {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Cancellation gets one cleanup request, never a new Escape attempt.
        // If the runtime/transport is gone the native host's lease expiry is
        // the independent bounded fallback.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let client = self.client.clone();
            let token = self.token.clone();
            runtime.spawn(async move {
                let _ = client
                    .send_command(
                        "ABExt.nativeFocusFinish",
                        Some(json!({"token":token})),
                        None,
                    )
                    .await;
            });
        }
    }
}

pub(super) fn enabled() -> bool {
    BUDGET
        .try_with(|budget| !matches!(budget.get(), Budget::Disabled))
        .unwrap_or(false)
}

pub(super) fn register(
    client: Arc<super::cdp::client::CdpClient>,
    token: String,
    input: Value,
) -> Result<(), String> {
    LEASE
        .try_with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_some() {
                return Err("native focus lease already exists".to_string());
            }
            *slot = Some(ClientLease {
                client,
                token,
                input,
                armed: true,
            });
            Ok(())
        })
        .map_err(|_| "native focus recovery has no invocation scope".to_string())?
}

pub(super) fn record_input(input: Value) -> Result<(), String> {
    LEASE
        .try_with(|slot| {
            let mut slot = slot.borrow_mut();
            let lease = slot
                .as_mut()
                .ok_or("native focus lease is absent".to_string())?;
            lease.input = input;
            Ok(())
        })
        .map_err(|_| "native focus recovery has no invocation scope".to_string())?
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
