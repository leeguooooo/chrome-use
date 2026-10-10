use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorMetadata {
    pub code: &'static str,
    pub retryable: bool,
}

/// Map human-readable command failures onto a small, stable machine taxonomy.
///
/// The message remains the primary diagnostic for humans. `code` and
/// `retryable` let CLI, MCP, and HTTP callers branch without parsing prose.
pub fn classify_error(message: &str) -> ErrorMetadata {
    let lower = message.to_ascii_lowercase();

    // First: a kept-ref refusal quotes its cause, which may itself read as a
    // denial, a timeout or a lost connection. It is none of those to act on:
    // nothing was done, and only a fresh `snapshot -i` helps.
    if lower.contains("kept_ref_unverified:") {
        return ErrorMetadata {
            code: "kept_ref_unverified",
            retryable: false,
        };
    }
    // An `open` whose wait ended without a usable page (#502). Its message
    // quotes the wait error ("Timeout waiting for Page.loadEventFired",
    // "Event stream closed") and the URL, either of which can read as a
    // timeout, a lost connection or a denial. None of those applies: the
    // navigation may still be loading, and repeating the open would start a
    // second one. Checked before every generic rule, so none can make it
    // retryable.
    if lower.contains(crate::native::browser::NAVIGATION_INCOMPLETE_PREFIX) {
        let code = if lower.contains(crate::native::browser::COMMIT_UNKNOWN_PHRASE) {
            "navigation_commit_unknown"
        } else {
            "navigation_incomplete"
        };
        return ErrorMetadata {
            code,
            retryable: false,
        };
    }
    // Before `timeout` and `connection`: these quote what failed, and each
    // has its own next step (#486). Only a setup failure whose tab Chrome
    // confirms gone is safe to simply rerun.
    for (needle, code, retryable) in [
        ("profile not open", "profile_not_open", false),
        (
            "profile window unavailable",
            "profile_window_unavailable",
            false,
        ),
        (
            "first tab outcome unknown",
            "first_tab_outcome_unknown",
            false,
        ),
        (
            "first tab cleanup incomplete",
            "first_tab_cleanup_incomplete",
            false,
        ),
        ("first tab setup failed", "first_tab_setup_failed", true),
    ] {
        if lower.contains(needle) {
            return ErrorMetadata { code, retryable };
        }
    }
    // The extension could not send a reply over Chrome's 64 MiB message
    // limit (#530). Asking again gets the same answer: ask for less.
    if lower.contains("reply_too_large:") {
        return ErrorMetadata {
            code: "reply_too_large",
            retryable: false,
        };
    }
    if lower.contains("action_outcome_unknown:") {
        return ErrorMetadata {
            code: "action_outcome_unknown",
            retryable: false,
        };
    }
    if lower.contains("debugger_access_denied:")
        || (lower.contains("cannot access a chrome-extension://")
            && lower.contains("different extension"))
    {
        return ErrorMetadata {
            code: "debugger_access_denied",
            retryable: false,
        };
    }
    if lower.contains("timeout") || lower.contains("timed out") {
        return ErrorMetadata {
            code: "timeout",
            retryable: true,
        };
    }
    if lower.contains("stale session")
        || lower.contains("stale target")
        || lower.contains("detached")
        || lower.contains("target closed")
        || lower.contains("target is gone")
    {
        return ErrorMetadata {
            code: "stale_target",
            retryable: true,
        };
    }
    if lower.contains("connection")
        || lower.contains("event stream closed")
        || lower.contains("failed to connect")
        || lower.contains("relay is not")
    {
        return ErrorMetadata {
            code: "connection_failed",
            retryable: true,
        };
    }
    if lower.contains("browser not launched") {
        return ErrorMetadata {
            code: "browser_not_launched",
            retryable: true,
        };
    }
    if lower.contains("element not found")
        || lower.contains("no element matches selector")
        || lower.contains("could not locate element")
        || lower.contains("unknown ref")
    {
        return ErrorMetadata {
            code: "element_not_found",
            retryable: false,
        };
    }
    if lower.contains("missing '")
        || lower.contains("invalid ")
        || lower.contains("unknown command")
        || lower.contains("unknown subcommand")
        || lower.contains("not yet implemented")
    {
        return ErrorMetadata {
            code: "invalid_request",
            retryable: false,
        };
    }
    if lower.contains("requires ab-connect")
        || lower.contains("not supported")
        || lower.contains("unsupported")
        || lower.contains("permission")
    {
        return ErrorMetadata {
            code: "unsupported",
            retryable: false,
        };
    }

    ErrorMetadata {
        code: "command_failed",
        retryable: false,
    }
}

pub fn error_value(message: &str) -> Value {
    let metadata = classify_error(message);
    json!({
        "success": false,
        "error": message,
        "code": metadata.code,
        "retryable": metadata.retryable,
    })
}

/// Add structured metadata to a daemon or transport error without changing
/// existing fields. Explicit codes already set by a parser are preserved.
pub fn enrich_error_value(value: &mut Value) {
    if value.get("success").and_then(Value::as_bool) != Some(false) {
        return;
    }
    let Some(message) = value.get("error").and_then(Value::as_str) else {
        return;
    };
    let metadata = classify_error(message);
    let Some(object) = value.as_object_mut() else {
        return;
    };
    object
        .entry("code".to_string())
        .or_insert_with(|| json!(metadata.code));
    object
        .entry("retryable".to_string())
        .or_insert_with(|| json!(metadata.retryable));
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_kept_ref_refusal_is_never_retryable_whatever_its_cause_says() {
        for cause in [
            "the page could not be read (CDP error (Page.getFrameTree): Request timed out)",
            "the page could not be read (WebSocket connection closed)",
            "the page could not be read (debugger_access_denied: blocked)",
            "its node is gone (target detached)",
        ] {
            let m = classify_error(&format!(
                "kept_ref_unverified: Ref e5 [textbox \"Name\"] ...: {cause}. Nothing was acted on."
            ));
            assert_eq!(m.code, "kept_ref_unverified", "{cause}");
            assert!(!m.retryable, "{cause}");
        }
    }

    use super::*;

    #[test]
    fn first_tab_failures_have_their_own_codes() {
        for (message, code, retryable) in [
            (
                crate::native::first_tab::PROFILE_NOT_OPEN.to_string(),
                "profile_not_open",
                false,
            ),
            (
                "Auto-launch failed: profile not open: no window".to_string(),
                "profile_not_open",
                false,
            ),
            (
                crate::native::first_tab::profile_window_verdict(Ok(serde_json::json!([null])))
                    .unwrap_err(),
                "profile_window_unavailable",
                false,
            ),
            (
                crate::native::first_tab::create_failure("CDP command timed out"),
                "first_tab_outcome_unknown",
                false,
            ),
            (
                crate::native::first_tab::cleanup_incomplete("T1", "x", "y", true),
                "first_tab_cleanup_incomplete",
                false,
            ),
            (
                crate::native::first_tab::setup_failure(
                    "T1",
                    "Page.enable timed out",
                    &crate::native::first_tab::Cleanup::Gone,
                    true,
                ),
                "first_tab_setup_failed",
                true,
            ),
        ] {
            let metadata = classify_error(&message);
            assert_eq!(metadata.code, code, "{message}");
            assert_eq!(metadata.retryable, retryable, "{message}");
        }
    }

    #[test]
    fn protected_extension_content_is_not_retryable() {
        for message in [
            "debugger_access_denied: blocked",
            "Cannot access a chrome-extension:// URL of different extension",
        ] {
            assert_eq!(
                classify_error(message),
                ErrorMetadata {
                    code: "debugger_access_denied",
                    retryable: false
                }
            );
        }
        assert_eq!(
            classify_error("action_outcome_unknown: original debugger_access_denied: blocked").code,
            "action_outcome_unknown"
        );
    }

    /// #502: an unfinished `open` quotes its wait error and its URL, which
    /// can read as a timeout, a lost connection, a stale target or a denial.
    /// Built with the real constructor, every combination keeps its own code
    /// and `retryable: false`, so nothing invites repeating the open.
    #[test]
    fn an_unfinished_open_is_never_a_retryable_timeout_connection_or_denial() {
        use crate::native::browser::{
            navigation_incomplete_error, to_ai_friendly_error, CommitEvidence, LoadProgress,
            WaitUntil,
        };
        let loading = LoadProgress::from_value(&json!({
            "readyState": "loading", "url": "https://a.test/x", "pending": [], "pendingTotal": 0
        }));
        let wait_errors = [
            "Timeout waiting for Page.loadEventFired",
            "Event stream closed",
            "CDP command timed out: Page.navigate",
        ];
        let targets = [
            "https://a.test/plain",
            "https://a.test/debugger_access_denied:/x",
            "https://a.test/Cannot access a chrome-extension:// URL of different extension",
            "https://a.test/connection-refused/failed to connect/relay is not up",
            "https://a.test/target closed/detached/stale session",
            "https://a.test/timed out",
        ];
        let commits = [
            (
                CommitEvidence::Unknown { url: None },
                "navigation_commit_unknown",
            ),
            (
                CommitEvidence::Unknown {
                    url: Some("https://a.test/debugger_access_denied:/connection".into()),
                },
                "navigation_commit_unknown",
            ),
            (
                CommitEvidence::Committed {
                    url: "https://a.test/x".into(),
                },
                "navigation_incomplete",
            ),
            (
                CommitEvidence::NotCommitted {
                    url: "https://a.test/old".into(),
                },
                "navigation_incomplete",
            ),
        ];
        for wait_error in wait_errors {
            for target in targets {
                for (commit, code) in &commits {
                    for progress in [Some(&loading), None] {
                        let message = navigation_incomplete_error(
                            target,
                            WaitUntil::Load,
                            25_000,
                            25_000,
                            wait_error,
                            progress,
                            commit,
                        );
                        let m = classify_error(&message);
                        assert_eq!(m.code, *code, "{message}");
                        assert!(!m.retryable, "{message}");
                        // The daemon's rewrite keeps it verbatim, and the
                        // #373 denial recovery never sees it as a denial.
                        assert_eq!(to_ai_friendly_error(&message), message);
                        assert!(
                            !crate::native::browser::is_debugger_access_denied(&message),
                            "{message}"
                        );
                        // What the CLI and the MCP tool print.
                        let mut v = json!({"success": false, "error": message});
                        enrich_error_value(&mut v);
                        assert_eq!(v["code"], *code);
                        assert_eq!(v["retryable"], false);
                    }
                }
            }
        }
    }

    #[test]
    fn an_unconfirmed_action_is_not_reclassified_as_retryable_transport_failure() {
        for cause in [
            "Detached while handling command",
            "timeout",
            "stale sessionId",
        ] {
            let message = format!("action_outcome_unknown: not replayed. Original error: {cause}");
            let value = error_value(&message);
            assert_eq!(value["code"], "action_outcome_unknown");
            assert_eq!(value["retryable"], false);
            assert_eq!(value["error"], message);
        }
    }

    #[test]
    fn classifies_retryable_runtime_failures() {
        assert_eq!(
            classify_error("Timeout waiting for download"),
            ErrorMetadata {
                code: "timeout",
                retryable: true
            }
        );
        assert_eq!(
            classify_error("stale sessionId cb-tab-1: target is gone"),
            ErrorMetadata {
                code: "stale_target",
                retryable: true
            }
        );
    }

    /// #530: the extension's size refusal mentions neither a timeout nor a
    /// connection, but its wording must never become retryable even if it did.
    #[test]
    fn a_reply_over_the_message_limit_is_not_retryable() {
        for message in [
            "CDP error (Runtime.evaluate): reply_too_large: the reply to Runtime.evaluate is 70.0 MiB, over Chrome's 64.0 MiB limit for one native-messaging message, so the extension cannot send it. Nothing is retried; ask for less (a smaller eval result, `snapshot -i` or a scoped selector, a smaller screenshot).",
            "reply_too_large: the reply is over Chrome's limit; connection kept, no timeout",
        ] {
            let value = error_value(message);
            assert_eq!(value["code"], "reply_too_large", "{message}");
            assert_eq!(value["retryable"], false, "{message}");
        }
    }

    #[test]
    fn classifies_scoped_a11y_selector_misses() {
        assert_eq!(
            classify_error("No element matches selector: #main"),
            ErrorMetadata {
                code: "element_not_found",
                retryable: false
            }
        );
    }

    #[test]
    fn enriches_without_overwriting_explicit_codes() {
        let mut value = json!({
            "success": false,
            "error": "Missing 'url' parameter",
            "code": "missing_url"
        });
        enrich_error_value(&mut value);
        assert_eq!(value["code"], "missing_url");
        assert_eq!(value["retryable"], false);
    }
}
