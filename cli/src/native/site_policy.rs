//! Per-site permissions: the site dimension of the action policy (#781 G2, G3,
//! G11).
//!
//! `--action-policy` and `--confirm-actions` decide by action name alone. This
//! module adds *where*: an action is classified into a category (navigation,
//! click, typing, upload, download) and, for a few cases that always deserve a
//! person's look, a kind: `payment_input` (card number, CVV, expiry),
//! `purchase` (buy / pay / place-order buttons), `password_input` (password
//! and one-time-code fields) and `download`. Only the first two ask by
//! default: agents routinely sign in (`auth login`, `fill --from-env`) and
//! run download automations unattended, so the other two are opt-in. The site is
//! the host the action lands on: the URL argument for a navigation, else the
//! intended command target (resolved without selecting another tab). Pending
//! approvals also verify its renderer origin, without evaluating page JavaScript.
//!
//! Rules come in layers, highest first:
//!
//! 1. **managed**: Chrome enterprise policy (`chrome.storage.managed`), read
//!    by the extension. Allowed / blocked / confirm domains, and settings the
//!    user cannot change.
//! 2. **user**: decisions made in the extension ("Always allow on this site",
//!    blocked sites, the settings on its options page).
//! 3. **policy file**: the `sites` block of the `--action-policy` JSON.
//! 4. **defaults**: the built-in sensitive-site list (`sensitive_sites.json`:
//!    banking, brokerage, crypto exchanges, government ID) and the
//!    always-ask kinds. These defaults apply only while chrome-use drives the
//!    user's own Chrome through the extension, where a person and the approval
//!    UI exist. A `--launch` / `--cdp` session gets them only from its policy
//!    file.
//!
//! Everything here runs in the daemon. Nothing is evaluated in the page, so
//! the gate leaves no trace a site can see (stealth first).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::OnceLock;

/// The maintained list of sites that ask first by default.
const SENSITIVE_SITES_JSON: &str = include_str!("sensitive_sites.json");

/// Kinds that ask every time by default (on the relay). `password_input` and
/// `download` are opt-in.
pub const DEFAULT_ASK: &[&str] = &["payment_input", "purchase"];

/// Lowercased `name` / `id` fragments of a payment field.
const PAYMENT_IDENTIFIERS: &[&str] = &[
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
    "exp-date",
    "expdate",
    "expiry",
    "expiration",
    "cc-exp",
    "cc_exp",
];
/// Lowercased accessible-name fragments of a payment field.
const PAYMENT_NAMES: &[&str] = &[
    "card number",
    "credit card",
    "debit card",
    "security code",
    "cvc",
    "cvv",
    "csc",
    "expiration",
    "expiry",
    "卡号",
    "信用卡",
    "安全码",
    "有效期",
];
/// Lowercased `name` / `id` fragments of a password or one-time-code field.
const PASSWORD_IDENTIFIERS: &[&str] = &["password", "passwd", "passcode", "otp", "one-time-code"];
/// Lowercased accessible-name fragments of a password or one-time-code field.
const PASSWORD_NAMES: &[&str] = &[
    "password",
    "passcode",
    "one-time code",
    "one time code",
    "verification code",
    "密码",
    "验证码",
    "口令",
];

fn has_word(haystack: &str, word: &str) -> bool {
    haystack
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w == word)
}

/// `payment_input` or `password_input` for a selector, label or accessible
/// name, judged by its words. Payment wins when both match.
pub fn field_kind_from_text(text: &str) -> Option<&'static str> {
    let t = text.to_lowercase();
    if t.trim().is_empty() {
        return None;
    }
    if PAYMENT_IDENTIFIERS
        .iter()
        .chain(PAYMENT_NAMES)
        .any(|p| t.contains(p))
    {
        return Some("payment_input");
    }
    if t.contains("type=password")
        || t.contains("type=\"password\"")
        || PASSWORD_IDENTIFIERS
            .iter()
            .chain(PASSWORD_NAMES)
            .any(|p| t.contains(p))
        || has_word(&t, "pin")
    {
        return Some("password_input");
    }
    None
}

/// `payment_input` or `password_input` from a field's DOM attributes (a flat
/// `[name, value, ...]` list, as CDP `DOM.describeNode` returns them).
pub fn field_kind_from_attributes(attrs: &[String]) -> Option<&'static str> {
    let mut password = false;
    for pair in attrs.chunks(2) {
        let [key, value] = pair else { continue };
        let value = value.to_lowercase();
        match key.as_str() {
            "autocomplete" => {
                for token in value.split_whitespace() {
                    if token.starts_with("cc-") {
                        return Some("payment_input");
                    }
                    if matches!(token, "current-password" | "new-password" | "one-time-code") {
                        password = true;
                    }
                }
            }
            "type" if value == "password" => password = true,
            "name"
            | "id"
            | "data-elements-stable-field-name"
            | "aria-label"
            | "placeholder"
            | "title" => match field_kind_from_text(&value) {
                Some("payment_input") => return Some("payment_input"),
                Some(_) => password = true,
                None => {}
            },
            _ => {}
        }
    }
    password.then_some("password_input")
}

/// The category of a daemon action, or `None` for actions that only read
/// (snapshot, get, screenshot ...), manage the session, or are not tied to a
/// page the agent acts on.
pub fn category_of(action: &str, cmd: &Value) -> Option<&'static str> {
    let has_url = cmd
        .get("url")
        .and_then(|v| v.as_str())
        .is_some_and(|u| !u.trim().is_empty());
    match action {
        "navigate" => Some("navigation"),
        "tab_new" | "window_new" | "a11y" if has_url => Some("navigation"),
        "click" | "dblclick" | "tap" | "check" | "uncheck" | "select" | "multiselect" | "drag"
        | "do" | "pick" => Some("click"),
        "fill" | "type" | "inserttext" | "keyboard" | "press" | "form_fill" | "setvalue"
        | "paste" | "select_text" => Some("typing"),
        "upload" => Some("upload"),
        "download" | "download_url" => Some("download"),
        _ => None,
    }
}

/// The URL whose host an action lands on, when the command names one.
pub fn target_url(action: &str, cmd: &Value) -> Option<String> {
    match action {
        "navigate" | "tab_new" | "window_new" | "a11y" | "download_url" => cmd
            .get("url")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

/// The lowercase host of an http(s) URL, or `None` for anything else
/// (`about:blank`, `chrome://`, `file:`, data URLs). A URL without a scheme
/// is read the way `navigate` reads it, as https.
pub fn host_of(url: &str) -> Option<String> {
    let url = url.trim();
    let candidate = if url.contains("://") {
        url.to_string()
    } else if url.starts_with("about:")
        || url.starts_with("data:")
        || url.starts_with("javascript:")
        || url.starts_with("chrome:")
        || url.starts_with("file:")
    {
        return None;
    } else {
        format!("https://{url}")
    };
    let parsed = url::Url::parse(&candidate).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str()?.trim_end_matches('.').to_lowercase();
    (!host.is_empty()).then_some(host)
}

/// Normalise a site pattern a person or a policy wrote: lowercase, no scheme,
/// path, port or leading `*.` / `.`.
pub fn normalize_pattern(pattern: &str) -> String {
    let p = pattern.trim().to_lowercase();
    let p = p.split("://").last().unwrap_or("").to_string();
    let p = p.split(['/', '?', '#']).next().unwrap_or("");
    let p = p.split(':').next().unwrap_or("");
    p.trim_start_matches("*.")
        .trim_start_matches('.')
        .trim_end_matches('.')
        .to_string()
}

/// Whether `pattern` covers `host`. A domain covers itself and every
/// subdomain (`example.com` covers `www.example.com`), and `*.example.com`
/// means the same. `*` alone covers every host.
pub fn host_matches(pattern: &str, host: &str) -> bool {
    if pattern.trim() == "*" {
        return true;
    }
    let p = normalize_pattern(pattern);
    if p.is_empty() {
        return false;
    }
    host == p || host.ends_with(&format!(".{p}"))
}

fn any_match(patterns: &[String], host: &str) -> bool {
    patterns.iter().any(|p| host_matches(p, host))
}

/// The built-in sensitive-site category covering `host`, if any.
pub fn sensitive_site_category(host: &str) -> Option<&'static str> {
    static LIST: OnceLock<Vec<(&'static str, Vec<String>)>> = OnceLock::new();
    let list = LIST.get_or_init(|| {
        let parsed: Value = serde_json::from_str(SENSITIVE_SITES_JSON).unwrap_or(Value::Null);
        let mut out = Vec::new();
        if let Some(categories) = parsed.get("categories").and_then(Value::as_object) {
            for (name, domains) in categories {
                let name: &'static str = match name.as_str() {
                    "banking" => "banking",
                    "brokerage" => "brokerage",
                    "crypto" => "crypto",
                    "government_id" => "government_id",
                    _ => continue,
                };
                let domains = domains
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|d| d.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                out.push((name, domains));
            }
        }
        out
    });
    list.iter()
        .find(|(_, domains)| any_match(domains, host))
        .map(|(name, _)| *name)
}

/// Words on a control that commit money. Matched against the accessible
/// name (or selector text) of what a click targets; lowercase.
const PURCHASE_PHRASES: &[&str] = &[
    "buy now",
    "place order",
    "place your order",
    "complete purchase",
    "complete order",
    "confirm purchase",
    "confirm order",
    "confirm payment",
    "submit order",
    "submit payment",
    "pay now",
    "make payment",
    "proceed to pay",
    "立即购买",
    "立即支付",
    "提交订单",
    "确认支付",
    "确认付款",
    "去支付",
    "去付款",
    "付款",
    "下单",
    "購入する",
    "注文を確定",
    "注文確定",
    "支払う",
];
/// Whole names that commit money ("Buy", "Pay $12.00" by prefix).
const PURCHASE_WORDS: &[&str] = &[
    "buy",
    "pay",
    "purchase",
    "subscribe",
    "购买",
    "支付",
    "購入",
];

/// Whether the name of a clicked control reads as a purchase.
pub fn is_purchase_name(name: &str) -> bool {
    let n = name.trim().to_lowercase();
    if n.is_empty() || n.chars().count() > 80 {
        return false;
    }
    PURCHASE_PHRASES.iter().any(|p| n.contains(p))
        || PURCHASE_WORDS
            .iter()
            .any(|w| n == *w || n.starts_with(&format!("{w} ")))
}

/// One layer of site rules: the policy file's `sites` block, the extension's
/// stored user decisions, or the managed policy. Unset fields defer to the
/// layer below.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SiteLayer {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default, alias = "deny")]
    pub block: Vec<String>,
    #[serde(default)]
    pub confirm: Vec<String>,
    /// The built-in sensitive-site list on (`true`) or off (`false`).
    #[serde(default)]
    pub sensitive_sites: Option<bool>,
    /// Categories that ask the first time on a site without a decision.
    #[serde(default)]
    pub first_visit: Option<Vec<String>>,
    /// Kinds that ask every time.
    #[serde(default)]
    pub ask: Option<Vec<String>>,
}

/// What an administrator pinned through `chrome.storage.managed`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ManagedLayer {
    /// When non-empty, the only sites an agent may act on.
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    #[serde(default)]
    pub blocked_domains: Vec<String>,
    #[serde(default)]
    pub confirm_domains: Vec<String>,
    #[serde(default)]
    pub sensitive_sites: Option<bool>,
    #[serde(default)]
    pub first_visit: Option<Vec<String>>,
    #[serde(default)]
    pub ask: Option<Vec<String>>,
    /// Same shape as an `--action-policy` file (default / allow / deny /
    /// confirm by action name); applied on top of the local one.
    #[serde(default)]
    pub action_policy: Option<Value>,
}

/// What the extension answers to `ABExt.sitePolicy`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionSitePolicy {
    #[serde(default)]
    pub user: SiteLayer,
    #[serde(default)]
    pub managed: ManagedLayer,
}

/// What the gate decided for one action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SiteGate {
    Allow,
    Deny(String),
    /// Needs a person's approval. `reason` is a stable code, `detail` the
    /// sentence shown to the person.
    Ask {
        reason: &'static str,
        detail: String,
    },
}

/// The layers in force for one session.
#[derive(Debug, Clone, Default)]
pub struct SitePolicy {
    pub managed: ManagedLayer,
    pub user: SiteLayer,
    pub file: SiteLayer,
    /// Whether the defaults (sensitive list, always-ask kinds) apply: true
    /// while driving the user's Chrome through the extension.
    pub defaults: bool,
    /// Sites allowed for the rest of this daemon's life (an "always" chosen
    /// at a terminal prompt when no extension could store it).
    pub session_allow: Vec<String>,
}

fn env_off(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            matches!(
                v.trim().to_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            )
        })
        .unwrap_or(false)
}

impl SitePolicy {
    /// Whether the built-in sensitive-site list is on.
    pub fn sensitive_sites(&self) -> bool {
        if let Some(v) = self.managed.sensitive_sites {
            return v;
        }
        if env_off("CHROME_USE_SENSITIVE_SITES") {
            return false;
        }
        self.user
            .sensitive_sites
            .or(self.file.sensitive_sites)
            .unwrap_or(self.defaults)
    }

    /// The kinds that ask every time.
    pub fn ask(&self) -> Vec<String> {
        self.managed
            .ask
            .clone()
            .or_else(|| self.user.ask.clone())
            .or_else(|| self.file.ask.clone())
            .unwrap_or_else(|| {
                if self.defaults {
                    DEFAULT_ASK.iter().map(|s| s.to_string()).collect()
                } else {
                    Vec::new()
                }
            })
    }

    /// The categories that ask on a site the person has not decided on.
    pub fn first_visit(&self) -> Vec<String> {
        self.managed
            .first_visit
            .clone()
            .or_else(|| self.user.first_visit.clone())
            .or_else(|| self.file.first_visit.clone())
            .unwrap_or_default()
    }

    /// Whether the policy can never ask or refuse anything, so the gate can
    /// skip classifying the action at all.
    pub fn is_inert(&self) -> bool {
        self.managed == ManagedLayer::default()
            && self.user == SiteLayer::default()
            && self.file == SiteLayer::default()
            && !self.defaults
    }

    /// Decide one action on `host`. `kind` is `download`, `payment_input`,
    /// `password_input` or `purchase` when the action is one of those.
    pub fn decide(&self, host: &str, category: &str, kind: Option<&str>) -> SiteGate {
        let m = &self.managed;
        if any_match(&m.blocked_domains, host) {
            return SiteGate::Deny(format!(
                "{host} is blocked by your organization's chrome-use policy"
            ));
        }
        if !m.allowed_domains.is_empty() && !any_match(&m.allowed_domains, host) {
            return SiteGate::Deny(format!(
                "{host} is not on your organization's list of sites chrome-use may act on"
            ));
        }
        if any_match(&self.user.block, host) || any_match(&self.file.block, host) {
            return SiteGate::Deny(format!(
                "you blocked chrome-use on {host} (unblock it in the chrome-use options page or the policy file)"
            ));
        }
        if any_match(&m.confirm_domains, host) {
            return SiteGate::Ask {
                reason: "managed_site",
                detail: format!(
                    "your organization asks for approval before chrome-use acts on {host}"
                ),
            };
        }
        if let Some(kind) = kind {
            if self.ask().iter().any(|k| k == kind) {
                let what = match kind {
                    "download" => "download a file",
                    "payment_input" => "type into a payment card field",
                    "password_input" => "type into a password or one-time-code field",
                    "purchase" => "press what looks like a purchase button",
                    _ => "act",
                };
                return SiteGate::Ask {
                    reason: match kind {
                        "download" => "download",
                        "payment_input" => "payment_input",
                        "password_input" => "password_input",
                        _ => "purchase",
                    },
                    detail: format!("chrome-use wants to {what} on {host}"),
                };
            }
        }
        if any_match(&self.user.allow, host)
            || any_match(&self.file.allow, host)
            || any_match(&self.session_allow, host)
        {
            return SiteGate::Allow;
        }
        if any_match(&self.user.confirm, host) || any_match(&self.file.confirm, host) {
            return SiteGate::Ask {
                reason: "confirm_site",
                detail: format!("{host} is on your ask-first list"),
            };
        }
        if self.sensitive_sites() {
            if let Some(cat) = sensitive_site_category(host) {
                let label = match cat {
                    "banking" => "a banking or payments site",
                    "brokerage" => "a brokerage site",
                    "crypto" => "a crypto exchange",
                    _ => "a government ID site",
                };
                return SiteGate::Ask {
                    reason: "sensitive_site",
                    detail: format!("{host} is {label}; chrome-use asks before an agent acts there ({category})"),
                };
            }
        }
        if self.first_visit().iter().any(|c| c == category) {
            return SiteGate::Ask {
                reason: "first_visit",
                detail: format!("first time chrome-use acts on {host} ({category})"),
            };
        }
        SiteGate::Allow
    }
}

/// The `confirmation_required` payload for a site gate.
pub fn confirmation_payload(
    confirmation_id: &str,
    action: &str,
    host: &str,
    category: &str,
    reason: &str,
    detail: &str,
    approval: &str,
) -> Value {
    json!({
        "confirmation_required": true,
        "confirmation_id": confirmation_id,
        "action": action,
        "category": category,
        "description": detail,
        "site": host,
        "reason": reason,
        // `user`: only the person can approve (extension UI or a terminal
        // prompt); `cli`: `chrome-use confirm <id>` approves.
        "approval": approval,
        "choices": ["once", "always", "decline"],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relay() -> SitePolicy {
        SitePolicy {
            defaults: true,
            ..SitePolicy::default()
        }
    }

    #[test]
    fn hosts_match_themselves_and_subdomains() {
        assert!(host_matches("example.com", "example.com"));
        assert!(host_matches("example.com", "www.example.com"));
        assert!(host_matches("*.example.com", "a.b.example.com"));
        assert!(host_matches("https://Example.com/path", "example.com"));
        assert!(!host_matches("example.com", "badexample.com"));
        assert!(!host_matches("", "example.com"));
        assert!(host_matches("*", "anything.test"));
    }

    #[test]
    fn host_of_reads_http_only() {
        assert_eq!(
            host_of("https://WWW.Chase.com/x").as_deref(),
            Some("www.chase.com")
        );
        assert_eq!(host_of("example.com/a").as_deref(), Some("example.com"));
        assert_eq!(host_of("about:blank"), None);
        assert_eq!(host_of("chrome://settings"), None);
        assert_eq!(host_of("file:///tmp/a.html"), None);
    }

    #[test]
    fn categories_cover_acting_commands_and_skip_reads() {
        let none = json!({});
        assert_eq!(category_of("navigate", &none), Some("navigation"));
        assert_eq!(
            category_of("tab_new", &json!({"url":"a.com"})),
            Some("navigation")
        );
        assert_eq!(category_of("tab_new", &none), None);
        assert_eq!(category_of("click", &none), Some("click"));
        assert_eq!(category_of("fill", &none), Some("typing"));
        assert_eq!(category_of("upload", &none), Some("upload"));
        assert_eq!(category_of("download_url", &none), Some("download"));
        for read in [
            "snapshot",
            "screenshot",
            "gettext",
            "url",
            "read",
            "confirm",
            "deny",
        ] {
            assert_eq!(category_of(read, &none), None, "{read}");
        }
    }

    #[test]
    fn the_sensitive_list_parses_and_covers_its_categories() {
        assert_eq!(sensitive_site_category("secure.chase.com"), Some("banking"));
        assert_eq!(sensitive_site_category("www.schwab.com"), Some("brokerage"));
        assert_eq!(sensitive_site_category("www.coinbase.com"), Some("crypto"));
        assert_eq!(
            sensitive_site_category("sa.www4.irs.gov"),
            Some("government_id")
        );
        assert_eq!(sensitive_site_category("example.com"), None);
    }

    #[test]
    fn purchase_names() {
        assert!(is_purchase_name("Place your order"));
        assert!(is_purchase_name("Pay $12.00"));
        assert!(is_purchase_name("Buy"));
        assert!(is_purchase_name("立即购买"));
        assert!(!is_purchase_name("Payment methods help"));
        assert!(!is_purchase_name("Buyer reviews"));
        assert!(!is_purchase_name("Search"));
    }

    #[test]
    fn relay_defaults_ask_on_sensitive_sites_and_kinds() {
        let p = relay();
        assert!(matches!(
            p.decide("www.chase.com", "click", None),
            SiteGate::Ask {
                reason: "sensitive_site",
                ..
            }
        ));
        assert_eq!(p.decide("example.com", "click", None), SiteGate::Allow);
        // Agents sign in and download unattended: both are opt-in.
        assert_eq!(
            p.decide("example.com", "download", Some("download")),
            SiteGate::Allow
        );
        assert_eq!(
            p.decide("example.com", "typing", Some("password_input")),
            SiteGate::Allow
        );
        assert!(matches!(
            p.decide("example.com", "typing", Some("payment_input")),
            SiteGate::Ask {
                reason: "payment_input",
                ..
            }
        ));
        assert!(matches!(
            p.decide("shop.test", "click", Some("purchase")),
            SiteGate::Ask {
                reason: "purchase",
                ..
            }
        ));
    }

    #[test]
    fn opting_in_to_password_fields_and_downloads() {
        let mut p = relay();
        p.user.ask = Some(vec!["password_input".into(), "download".into()]);
        assert!(matches!(
            p.decide("a.test", "typing", Some("password_input")),
            SiteGate::Ask {
                reason: "password_input",
                ..
            }
        ));
        assert!(matches!(
            p.decide("a.test", "download", Some("download")),
            SiteGate::Ask {
                reason: "download",
                ..
            }
        ));
        assert_eq!(
            p.decide("a.test", "typing", Some("payment_input")),
            SiteGate::Allow
        );
    }

    #[test]
    fn fields_are_told_apart_payment_first() {
        let attrs = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            field_kind_from_attributes(&attrs(&["type", "password", "name", "pw"])),
            Some("password_input")
        );
        assert_eq!(
            field_kind_from_attributes(&attrs(&["autocomplete", "one-time-code"])),
            Some("password_input")
        );
        assert_eq!(
            field_kind_from_attributes(&attrs(&["autocomplete", "billing cc-number"])),
            Some("payment_input")
        );
        assert_eq!(
            field_kind_from_attributes(&attrs(&["type", "password", "name", "cvv"])),
            Some("payment_input")
        );
        assert_eq!(
            field_kind_from_attributes(&attrs(&["type", "text", "name", "email"])),
            None
        );
        assert_eq!(field_kind_from_text("Card number"), Some("payment_input"));
        assert_eq!(field_kind_from_text("#password"), Some("password_input"));
        assert_eq!(field_kind_from_text("PIN"), Some("password_input"));
        assert_eq!(field_kind_from_text("Spinner"), None);
        assert_eq!(field_kind_from_text("Search"), None);
    }

    #[test]
    fn non_relay_sessions_get_no_defaults() {
        let p = SitePolicy::default();
        assert!(p.is_inert());
        assert_eq!(p.decide("www.chase.com", "click", None), SiteGate::Allow);
        assert_eq!(
            p.decide("a.test", "download", Some("download")),
            SiteGate::Allow
        );
    }

    #[test]
    fn always_allow_covers_a_sensitive_site_but_not_the_always_ask_kinds() {
        let mut p = relay();
        p.user.allow = vec!["chase.com".into()];
        assert_eq!(p.decide("secure.chase.com", "click", None), SiteGate::Allow);
        assert!(matches!(
            p.decide("secure.chase.com", "typing", Some("payment_input")),
            SiteGate::Ask { .. }
        ));
    }

    #[test]
    fn turning_off_the_list_and_kinds() {
        let mut p = relay();
        p.user.sensitive_sites = Some(false);
        p.user.ask = Some(vec![]);
        assert_eq!(p.decide("www.chase.com", "click", None), SiteGate::Allow);
        assert_eq!(
            p.decide("a.test", "download", Some("download")),
            SiteGate::Allow
        );
    }

    #[test]
    fn first_visit_asks_until_a_site_is_allowed() {
        let mut p = SitePolicy::default();
        p.file.first_visit = Some(vec!["navigation".into()]);
        assert!(matches!(
            p.decide("a.test", "navigation", None),
            SiteGate::Ask {
                reason: "first_visit",
                ..
            }
        ));
        assert_eq!(p.decide("a.test", "click", None), SiteGate::Allow);
        p.session_allow.push("a.test".into());
        assert_eq!(p.decide("a.test", "navigation", None), SiteGate::Allow);
    }

    #[test]
    fn blocks_win_over_allows() {
        let mut p = SitePolicy::default();
        p.user.allow = vec!["a.test".into()];
        p.file.block = vec!["a.test".into()];
        assert!(matches!(
            p.decide("a.test", "click", None),
            SiteGate::Deny(_)
        ));
    }

    #[test]
    fn managed_values_override_the_user() {
        let mut p = relay();
        p.user.sensitive_sites = Some(false);
        p.user.allow = vec!["evil.test".into()];
        p.managed.sensitive_sites = Some(true);
        p.managed.blocked_domains = vec!["evil.test".into()];
        assert_eq!(p.sensitive_sites(), true);
        assert!(matches!(
            p.decide("evil.test", "click", None),
            SiteGate::Deny(_)
        ));
        assert!(matches!(
            p.decide("www.chase.com", "click", None),
            SiteGate::Ask { .. }
        ));
        p.managed.allowed_domains = vec!["corp.test".into()];
        assert!(matches!(
            p.decide("other.test", "click", None),
            SiteGate::Deny(_)
        ));
        p.managed.confirm_domains = vec!["corp.test".into()];
        p.user.allow = vec!["corp.test".into()];
        assert!(matches!(
            p.decide("app.corp.test", "click", None),
            SiteGate::Ask {
                reason: "managed_site",
                ..
            }
        ));
    }

    #[test]
    fn the_extension_reply_parses() {
        let reply = json!({
            "user": {"allow": ["a.test"], "block": ["b.test"], "sensitiveSites": false, "ask": ["download"]},
            "managed": {"allowedDomains": ["a.test"], "actionPolicy": {"deny": ["upload"]}},
            "extra": 1
        });
        let parsed: ExtensionSitePolicy = serde_json::from_value(reply).unwrap();
        assert_eq!(parsed.user.allow, vec!["a.test"]);
        assert_eq!(parsed.user.sensitive_sites, Some(false));
        assert_eq!(parsed.managed.allowed_domains, vec!["a.test"]);
        assert!(parsed.managed.action_policy.is_some());
    }

    #[test]
    fn the_policy_file_block_parses_with_deny_alias() {
        let layer: SiteLayer = serde_json::from_value(
            json!({"allow": ["a.test"], "deny": ["b.test"], "firstVisit": ["navigation"]}),
        )
        .unwrap();
        assert_eq!(layer.block, vec!["b.test"]);
        assert_eq!(layer.first_visit, Some(vec!["navigation".to_string()]));
    }
}
