//! Fields whose value must not be printed by default (#372).
//!
//! `snapshot` and `get value` used to print whatever a field held, so a card
//! number or CVV typed into Stripe Elements landed verbatim in the agent's
//! transcript. A field counts as sensitive when its DOM says so
//! (`type=password`, an `autocomplete` token for card data, passwords or
//! one-time codes, or a telling `name`/`id`) or, when the DOM cannot be read,
//! when its accessible name does. Callers print [`masked`] instead of the value
//! unless the caller passed `--reveal-values`.

use serde_json::{json, Value};

use super::cdp::client::CdpClient;

/// `autocomplete` tokens for values that must stay out of the transcript.
const SENSITIVE_AUTOCOMPLETE: &[&str] = &[
    "cc-number",
    "cc-csc",
    "cc-exp",
    "cc-exp-month",
    "cc-exp-year",
    "current-password",
    "new-password",
    "one-time-code",
];

/// Lowercased substrings of a `name` / `id` attribute that mark a sensitive field.
const SENSITIVE_IDENTIFIERS: &[&str] = &[
    "cardnumber",
    "card-number",
    "card_number",
    "ccnumber",
    "cc-number",
    "cc_number",
    "cvc",
    "cvv",
    "csc",
    "securitycode",
    "security-code",
    "security_code",
    "password",
    "passwd",
    "passcode",
    "otp",
    "one-time-code",
    "exp-date",
    "expdate",
    "expiry",
    "expiration",
];

/// Lowercased substrings of an accessible name that mark a sensitive field.
const SENSITIVE_NAMES: &[&str] = &[
    "card number",
    "credit card",
    "debit card",
    "security code",
    "cvc",
    "cvv",
    "csc",
    "expiration",
    "expiry",
    "password",
    "passcode",
    "one-time code",
    "one time code",
    "verification code",
    "卡号",
    "信用卡",
    "安全码",
    "有效期",
    "密码",
    "验证码",
    "口令",
];

tokio::task_local! {
    /// Set for the whole of a command that carries a secret (`fill
    /// --from-env`): every field counts as sensitive while it runs, so the
    /// value cannot surface in a fill error, a read-back, or the `--observe`
    /// snapshot of a field that does not look like a password.
    pub static SECRET_COMMAND: bool;
}

/// Whether the running command carries a secret.
pub fn secret_command() -> bool {
    SECRET_COMMAND.try_with(|v| *v).unwrap_or(false)
}

/// What to print in place of a sensitive value.
pub fn masked(value: &str) -> String {
    format!("<filled {} chars>", value.chars().count())
}

/// Whether an accessible name marks the field as sensitive.
pub fn sensitive_by_name(name: &str) -> bool {
    if secret_command() {
        return true;
    }
    let n = name.to_lowercase();
    SENSITIVE_NAMES.iter().any(|s| n.contains(s)) || has_word(&n, "pin")
}

fn has_word(haystack: &str, word: &str) -> bool {
    haystack
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w == word)
}

/// Whether DOM attributes (a flat `[name, value, name, value, ...]` list, as
/// CDP returns them) mark the field as sensitive.
pub fn sensitive_by_attributes(attrs: &[String]) -> bool {
    for pair in attrs.chunks(2) {
        let [key, value] = pair else { continue };
        let value = value.to_lowercase();
        match key.as_str() {
            "type" if value == "password" => return true,
            "autocomplete" => {
                // The token list may carry section-/shipping prefixes.
                if value
                    .split_whitespace()
                    .any(|t| SENSITIVE_AUTOCOMPLETE.contains(&t))
                {
                    return true;
                }
            }
            "name" | "id" | "data-elements-stable-field-name" => {
                if SENSITIVE_IDENTIFIERS.iter().any(|s| value.contains(s)) {
                    return true;
                }
            }
            "aria-label" | "placeholder" | "title" if sensitive_by_name(&value) => return true,
            _ => {}
        }
    }
    false
}

/// Decide for one element: its DOM attributes when they can be read, plus its
/// accessible name.
pub async fn is_sensitive_node(
    client: &CdpClient,
    session_id: &str,
    backend_node_id: i64,
    accessible_name: &str,
) -> bool {
    if sensitive_by_name(accessible_name) {
        return true;
    }
    describe_is_sensitive(
        client,
        session_id,
        json!({ "backendNodeId": backend_node_id }),
    )
    .await
}

/// [`is_sensitive_node`] for an element already resolved to a remote object.
/// Judges the element a read or fill actually touches (the input inside a
/// wrapper, via [`EDITABLE_ATTRIBUTES_JS`]), and masks when that cannot be
/// read (deny by default).
pub async fn is_sensitive_object(client: &CdpClient, session_id: &str, object_id: &str) -> bool {
    if secret_command() {
        return true;
    }
    let result: Result<Value, String> = client
        .send_command(
            "Runtime.callFunctionOn",
            Some(json!({
                "functionDeclaration": EDITABLE_ATTRIBUTES_JS,
                "objectId": object_id,
                "returnByValue": true,
            })),
            Some(session_id),
        )
        .await;
    let attrs: Option<Vec<String>> = result.ok().and_then(|r| {
        if r.get("exceptionDetails").is_some() {
            return None;
        }
        r.pointer("/result/value")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
    });
    match attrs {
        Some(attrs) => sensitive_by_attributes(&attrs),
        None => true,
    }
}

async fn describe_is_sensitive(client: &CdpClient, session_id: &str, target: Value) -> bool {
    if secret_command() {
        return true;
    }
    let described: Result<Value, String> = client
        .send_command("DOM.describeNode", Some(target), Some(session_id))
        .await;
    // Deny by default: a node that cannot be inspected (it re-rendered away
    // between the AX read and now) might be the card field, so it is masked.
    let Ok(described) = described else {
        return true;
    };
    let attrs: Vec<String> = described
        .pointer("/node/attributes")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    sensitive_by_attributes(&attrs)
}

/// JS run on the element `get value` was given. It picks the same editable
/// element `READ_EDITABLE_VALUE_TEMPLATE` reads (the element itself, or the
/// first input/textarea/select/contenteditable inside it) and returns its
/// attributes as a flat `[name, value, ...]` list, so a password inside a
/// wrapper is judged by the password field, not by the wrapper (#372).
pub const EDITABLE_ATTRIBUTES_JS: &str = r#"function() {
    let el = this;
    const editable = n => n && (n.tagName === 'INPUT' || n.tagName === 'TEXTAREA'
        || n.tagName === 'SELECT' || n.isContentEditable);
    if (!editable(el) && el.querySelector) {
        const inner = el.querySelector('input, textarea, select, [contenteditable]');
        if (inner) el = inner;
    }
    const out = [];
    for (const k of ['type', 'autocomplete', 'name', 'id', 'aria-label', 'placeholder', 'title',
                     'data-elements-stable-field-name']) {
        const v = el.getAttribute && el.getAttribute(k);
        if (v != null) out.push(k, String(v));
    }
    return out;
}"#;

/// Replace `secret` in every string of `value` (not keys) with its masked
/// form, including the JSON- and Rust-escaped spellings an error may quote it
/// in. Works on the tree, so a short secret cannot corrupt the document.
pub fn scrub_value(value: &mut Value, secret: &str) {
    match value {
        Value::String(s) => *s = scrub_text(s, secret),
        Value::Array(items) => items.iter_mut().for_each(|v| scrub_value(v, secret)),
        Value::Object(map) => map.values_mut().for_each(|v| scrub_value(v, secret)),
        _ => {}
    }
}

/// [`scrub_value`] for a plain string.
pub fn scrub_text(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        return text.to_string();
    }
    let mask = masked(secret);
    let json = serde_json::to_string(secret).unwrap_or_default();
    let debug = format!("{secret:?}");
    let mut out = text.to_string();
    for form in [
        json.trim_matches('"').to_string(),
        debug.trim_matches('"').to_string(),
        secret.to_string(),
    ] {
        if !form.is_empty() {
            out = out.replace(&form, &mask);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs(pairs: &[(&str, &str)]) -> Vec<String> {
        pairs
            .iter()
            .flat_map(|(k, v)| [k.to_string(), v.to_string()])
            .collect()
    }

    #[test]
    fn stripe_elements_fields_are_sensitive() {
        // Attributes as Stripe's card iframe renders them.
        assert!(sensitive_by_attributes(&attrs(&[
            ("name", "cardnumber"),
            ("autocomplete", "cc-number")
        ])));
        assert!(sensitive_by_attributes(&attrs(&[
            ("name", "cvc"),
            ("autocomplete", "cc-csc")
        ])));
        assert!(sensitive_by_attributes(&attrs(&[
            ("name", "exp-date"),
            ("autocomplete", "cc-exp")
        ])));
        assert!(sensitive_by_attributes(&attrs(&[(
            "autocomplete",
            "section-pay billing cc-number"
        )])));
    }

    #[test]
    fn passwords_and_codes_are_sensitive() {
        assert!(sensitive_by_attributes(&attrs(&[("type", "password")])));
        assert!(sensitive_by_attributes(&attrs(&[(
            "autocomplete",
            "one-time-code"
        )])));
        assert!(sensitive_by_attributes(&attrs(&[("id", "login-passwd")])));
        assert!(sensitive_by_attributes(&attrs(&[(
            "placeholder",
            "MM / YY 有效期"
        )])));
        assert!(sensitive_by_attributes(&attrs(&[(
            "aria-label",
            "Credit or debit card number"
        )])));
    }

    #[test]
    fn ordinary_fields_are_not() {
        assert!(!sensitive_by_attributes(&attrs(&[
            ("type", "text"),
            ("name", "email"),
            ("autocomplete", "email")
        ])));
        assert!(!sensitive_by_attributes(&attrs(&[
            ("name", "postal"),
            ("autocomplete", "postal-code")
        ])));
        assert!(!sensitive_by_attributes(&attrs(&[(
            "autocomplete",
            "cc-name"
        )])));
    }

    #[test]
    fn names_decide_when_the_dom_cannot() {
        assert!(sensitive_by_name("Card number"));
        assert!(sensitive_by_name("Security code"));
        assert!(sensitive_by_name("Expiration date"));
        assert!(sensitive_by_name("请输入密码"));
        assert!(sensitive_by_name("短信验证码"));
        assert!(sensitive_by_name("PIN"));
        assert!(!sensitive_by_name("Pinterest handle"));
        assert!(!sensitive_by_name("Email"));
        assert!(!sensitive_by_name("Full name"));
    }

    #[test]
    fn masked_counts_characters_not_bytes() {
        assert_eq!(masked("4242 4242 4242 4242"), "<filled 19 chars>");
        assert_eq!(masked("密码"), "<filled 2 chars>");
    }

    #[test]
    fn scrubbing_catches_escaped_spellings_and_keeps_the_document_valid() {
        let secret = "pa\"ss\\word";
        let mut v = serde_json::json!({
            "error": format!("read back {:?} after writing {:?}", "x", secret),
            "n": 1,
            "list": [secret],
        });
        scrub_value(&mut v, secret);
        let text = v.to_string();
        assert!(!text.contains("pa\\\"ss"), "{text}");
        assert!(text.contains("<filled 10 chars>"), "{text}");
        assert_eq!(v["n"], 1);
        // A one-letter secret must not touch keys or break the document.
        let mut w = serde_json::json!({ "data": { "note": "n" } });
        scrub_value(&mut w, "n");
        assert!(w.get("data").is_some());
        assert_eq!(w["data"]["note"], "<filled 1 chars>");
    }

    #[tokio::test]
    async fn a_secret_command_makes_every_field_sensitive() {
        assert!(!sensitive_by_name("Email"));
        let inside = SECRET_COMMAND
            .scope(true, async { sensitive_by_name("Email") })
            .await;
        assert!(inside);
    }
}
