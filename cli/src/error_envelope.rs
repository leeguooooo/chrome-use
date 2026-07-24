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
    use super::*;

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
