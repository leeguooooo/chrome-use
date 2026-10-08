//! Chrome profiles the way the user names them (#437).
//!
//! The relay knows a connected profile only by a UUID the extension minted and,
//! when the profile granted `identity`, its email. People call profiles by the
//! name in Chrome's profile switcher ("Davian", "the d one") or its directory
//! ("Profile 14"). This module joins the two views — every profile in `Local
//! State`, plus which of them has a live relay — and builds the user-facing
//! pieces on top: the `browsers` table, `--browser` matching, `connect
//! --browser` (lazy, one-click per profile), `browsers --who <domain>`, the
//! per-session "profile:" line, and config-driven routing.

use crate::color;
use crate::connect::{self, ChromeProfileInfo};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// One Chrome profile, as `Local State` and the relay together describe it.
/// A relay with no matching `Local State` entry (e.g. Chrome's data root is in
/// an unusual place) still gets a row, carrying only what the relay knows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProfileRow {
    /// Name in Chrome's profile switcher.
    pub name: Option<String>,
    /// Profile directory: `Default`, `Profile 14`, …
    pub dir: Option<String>,
    /// Browser data root the directory lives under.
    pub root: Option<String>,
    /// Signed-in Google account (`Local State`'s `user_name`).
    pub email: Option<String>,
    /// The Google account's full name.
    pub gaia_name: Option<String>,
    /// Extension-minted profile id, when the profile is connected.
    pub relay_id: Option<String>,
    /// Email the extension reported (only with the `identity` permission).
    pub relay_email: Option<String>,
    /// The profile's relay endpoint, when connected.
    pub ws: Option<String>,
    pub has_extension: bool,
    /// Chrome lists disable reasons for the extension in this profile.
    pub extension_disabled: bool,
}

impl ProfileRow {
    pub fn connected(&self) -> bool {
        self.ws.is_some()
    }

    fn account(&self) -> Option<&str> {
        self.email.as_deref().or(self.relay_email.as_deref())
    }

    /// The shortest thing a person would call this profile.
    pub fn short(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.dir.clone())
            .or_else(|| self.account().map(ToString::to_string))
            .or_else(|| self.relay_id.clone())
            .unwrap_or_else(|| "?".to_string())
    }

    /// `Davian (Profile 14, someone@gmail.com)`.
    pub fn label(&self) -> String {
        let mut inner: Vec<String> = Vec::new();
        if self.name.is_some() {
            if let Some(d) = &self.dir {
                inner.push(d.clone());
            }
        }
        if self.name.is_some() || self.dir.is_some() {
            if let Some(a) = self.account() {
                inner.push(a.to_string());
            }
        } else if self.account().is_some() {
            // Relay-only row: headed by the email, so name the id beside it.
            if let Some(id) = &self.relay_id {
                inner.push(id.clone());
            }
        }
        let head = self.short();
        if inner.is_empty() {
            head
        } else {
            format!("{head} ({})", inner.join(", "))
        }
    }

    fn same_profile(&self, other: &ProfileRow) -> bool {
        match (&self.dir, &other.dir) {
            (Some(a), Some(b)) => a == b && self.root == other.root,
            _ => self.relay_id.is_some() && self.relay_id == other.relay_id,
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "dir": self.dir,
            "root": self.root,
            "email": self.account(),
            "gaiaName": self.gaia_name,
            "id": self.relay_id,
            "connected": self.connected(),
            "hasExtension": self.has_extension,
            "extensionDisabled": self.extension_disabled,
        })
    }
}

// --- Inventory ---------------------------------------------------------------

/// Join `Local State` profiles with live relays. `dir_of` maps a relay id to
/// the `(root, dir)` whose extension storage holds it; email is the fallback
/// (only when exactly one profile carries that email — several profiles can
/// be signed in to the same account).
pub fn join_rows(
    local: &[ChromeProfileInfo],
    relays: &[(String, Option<String>, String)],
    dir_of: &dyn Fn(&str) -> Option<(String, String)>,
) -> Vec<ProfileRow> {
    let mut rows: Vec<ProfileRow> = local
        .iter()
        .map(|p| ProfileRow {
            name: p.name.clone(),
            dir: Some(p.dir.clone()),
            root: Some(p.root.clone()),
            email: p.email.clone(),
            gaia_name: p.gaia_name.clone(),
            has_extension: p.extension.is_some(),
            extension_disabled: p
                .extension
                .as_ref()
                .is_some_and(|e| !e.disable_reasons.is_empty()),
            ..Default::default()
        })
        .collect();
    for (id, email, ws) in relays {
        let by_dir = dir_of(id).and_then(|(root, dir)| {
            rows.iter().position(|r| {
                r.relay_id.is_none()
                    && r.dir.as_deref() == Some(dir.as_str())
                    && r.root.as_deref() == Some(root.as_str())
            })
        });
        let by_email = || {
            let e = email.as_deref()?.to_lowercase();
            let hits: Vec<usize> = rows
                .iter()
                .enumerate()
                .filter(|(_, r)| {
                    r.relay_id.is_none()
                        && r.dir.is_some()
                        && r.email.as_deref().map(str::to_lowercase).as_deref() == Some(e.as_str())
                })
                .map(|(i, _)| i)
                .collect();
            if hits.len() == 1 {
                Some(hits[0])
            } else {
                None
            }
        };
        match by_dir.or_else(by_email) {
            Some(i) => {
                let r = &mut rows[i];
                r.relay_id = Some(id.clone());
                r.relay_email = email.clone();
                r.ws = Some(ws.clone());
                r.has_extension = true;
            }
            None => rows.push(ProfileRow {
                relay_id: Some(id.clone()),
                relay_email: email.clone(),
                ws: Some(ws.clone()),
                has_extension: true,
                ..Default::default()
            }),
        }
    }
    rows
}

/// Find which profile directory a relay id belongs to. The extension keeps
/// its id in `chrome.storage.local`, i.e. the profile's `Local Extension
/// Settings/<extension id>/` LevelDB — new writes land in the plain-text
/// `.log`, compacted ones in `.ldb` (short values stay literal under snappy).
fn relay_id_dir(id: &str, local: &[ChromeProfileInfo]) -> Option<(String, String)> {
    if id.len() < 8 {
        return None;
    }
    let needle = id.as_bytes();
    for p in local {
        for ext in [connect::STORE_EXTENSION_ID, connect::EXTENSION_ID] {
            let dir = Path::new(&p.root)
                .join(&p.dir)
                .join("Local Extension Settings")
                .join(ext);
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let ext_ok = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e == "log" || e == "ldb");
                if !ext_ok {
                    continue;
                }
                if let Ok(bytes) = std::fs::read(&path) {
                    if bytes.windows(needle.len()).any(|w| w == needle) {
                        return Some((p.root.clone(), p.dir.clone()));
                    }
                }
            }
        }
    }
    None
}

/// Every profile on this machine plus every live relay, joined.
pub fn load_rows() -> Vec<ProfileRow> {
    let local = connect::chrome_profiles();
    let relays = connect::list_relay_profiles();
    join_rows(&local, &relays, &|id| relay_id_dir(id, &local))
}

// --- Selector matching -------------------------------------------------------

#[derive(Debug, PartialEq)]
pub enum Match {
    One(usize),
    None,
    Ambiguous(Vec<usize>),
}

/// Resolve what a person typed to one profile. In order:
/// 1. exact (case-insensitive): relay id, directory, display name, email,
///    Google account name;
/// 2. a prefix of the display name (`d` → `Davian`);
/// 3. the pre-#437 forms: relay-id prefix, email substring.
///
/// Several hits on a prefix are always an error listing them. Several exact
/// or legacy hits resolve to the one connected profile among them, if there
/// is exactly one — the same email signed in to three profiles, one of which
/// runs the extension, kept working before this matcher knew the other two.
pub fn match_selector(rows: &[ProfileRow], selector: &str) -> Match {
    let sel = selector.trim();
    if sel.is_empty() {
        return Match::None;
    }
    let lc = sel.to_lowercase();
    let eq = |v: &Option<String>| v.as_deref().is_some_and(|x| x.to_lowercase() == lc);
    let hits = |f: &dyn Fn(&ProfileRow) -> bool| -> Vec<usize> {
        rows.iter()
            .enumerate()
            .filter(|(_, r)| f(r))
            .map(|(i, _)| i)
            .collect()
    };
    let decide = |c: Vec<usize>, prefer_connected: bool| -> Option<Match> {
        match c.len() {
            0 => None,
            1 => Some(Match::One(c[0])),
            _ => {
                if prefer_connected {
                    let live: Vec<usize> =
                        c.iter().copied().filter(|&i| rows[i].connected()).collect();
                    if live.len() == 1 {
                        return Some(Match::One(live[0]));
                    }
                }
                Some(Match::Ambiguous(c))
            }
        }
    };
    let exact = hits(&|r| {
        eq(&r.relay_id)
            || eq(&r.dir)
            || eq(&r.name)
            || eq(&r.email)
            || eq(&r.relay_email)
            || eq(&r.gaia_name)
    });
    if let Some(m) = decide(exact, true) {
        return m;
    }
    let prefix = hits(&|r| {
        r.name
            .as_deref()
            .is_some_and(|n| n.to_lowercase().starts_with(&lc))
    });
    if let Some(m) = decide(prefix, false) {
        return m;
    }
    let legacy = hits(&|r| {
        r.relay_id.as_deref().is_some_and(|id| id.starts_with(sel))
            || [&r.email, &r.relay_email]
                .iter()
                .any(|e| e.as_deref().is_some_and(|e| e.to_lowercase().contains(&lc)))
    });
    decide(legacy, true).unwrap_or(Match::None)
}

/// Quote a selector for a copy-pasteable command line.
fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | '+'))
    {
        s.to_string()
    } else {
        format!("\"{}\"", s.replace('"', "\\\""))
    }
}

/// The selector to suggest for a row: its display name when that alone
/// picks it, else its directory, else its id.
pub fn suggested_selector(rows: &[ProfileRow], i: usize) -> String {
    let r = &rows[i];
    for cand in [&r.name, &r.dir, &r.relay_id].into_iter().flatten() {
        if match_selector(rows, cand) == Match::One(i) {
            return cand.clone();
        }
    }
    r.short()
}

pub fn connect_command(selector: &str) -> String {
    format!("chrome-use connect --browser {}", shell_quote(selector))
}

fn list_lines(rows: &[ProfileRow], which: &[usize]) -> String {
    which
        .iter()
        .map(|&i| {
            let r = &rows[i];
            format!(
                "\n  {}{}",
                r.label(),
                if r.connected() { "  [connected]" } else { "" }
            )
        })
        .collect()
}

/// What to say about a profile that matched but has no live relay.
pub fn not_connected_message(selector: &str, row: &ProfileRow) -> String {
    let cmd = connect_command(selector);
    let ask = "It opens a window in the user's Chrome, so ask the user before running it.";
    if !row.has_extension {
        format!(
            "profile \"{selector}\" doesn't have the chrome-use extension yet — run `{cmd}` \
             (one click in Chrome), then repeat. {ask}"
        )
    } else if row.extension_disabled {
        format!(
            "profile \"{selector}\" ({}) has the chrome-use extension, but Chrome has it \
             disabled — run `{cmd}` (opens its extension page to re-enable it), then repeat. {ask}",
            row.dir.as_deref().unwrap_or("?")
        )
    } else {
        format!(
            "profile \"{selector}\" ({}) has the chrome-use extension but isn't open in Chrome \
             right now — run `{cmd}` (opens a window in that profile), then repeat. {ask}",
            row.dir.as_deref().unwrap_or("?")
        )
    }
}

/// Pure resolution to any profile (connected or not), with the user-facing
/// error for no match / several matches.
pub fn resolve_in(rows: &[ProfileRow], selector: &str) -> Result<usize, String> {
    let sel = selector.trim();
    match match_selector(rows, sel) {
        Match::One(i) => Ok(i),
        Match::Ambiguous(c) => Err(format!(
            "'{sel}' matches {} Chrome profiles — use the directory name or email:{}",
            c.len(),
            list_lines(rows, &c)
        )),
        Match::None => {
            let all: Vec<usize> = (0..rows.len()).collect();
            Err(if rows.is_empty() {
                format!(
                    "no Chrome profile matches '{sel}' (no Chrome profiles found and no \
                     extension connected — run `chrome-use doctor`)"
                )
            } else {
                format!(
                    "no Chrome profile matches '{sel}'. Profiles (see `chrome-use browsers`):{}",
                    list_lines(rows, &all)
                )
            })
        }
    }
}

/// Does `selector` name any Chrome profile on this machine (connected or not,
/// even ambiguously)? Lets `--profile`/AGENT_BROWSER_PROFILE act as a
/// selector without stealing its launch-mode meaning (a path, a new name).
pub fn names_a_profile(selector: &str) -> bool {
    // `auto` is `--profile`'s own keyword (the last-used profile), not a name
    // prefix.
    if selector.trim().eq_ignore_ascii_case("auto") {
        return false;
    }
    match_selector(&load_rows(), selector) != Match::None
}

/// Pure resolution to a CONNECTED profile.
pub fn resolve_connected_in(rows: &[ProfileRow], selector: &str) -> Result<usize, String> {
    let i = resolve_in(rows, selector)?;
    if rows[i].connected() {
        Ok(i)
    } else {
        Err(not_connected_message(selector.trim(), &rows[i]))
    }
}

pub fn resolve_connected(selector: &str) -> Result<ProfileRow, String> {
    let rows = load_rows();
    resolve_connected_in(&rows, selector).map(|i| rows[i].clone())
}

/// The row behind a relay endpoint (what a session is bound to).
pub fn row_for_ws(ws: &str) -> Option<ProfileRow> {
    load_rows()
        .into_iter()
        .find(|r| r.ws.as_deref() == Some(ws))
}

pub fn row_for_relay_id(id: &str) -> Option<ProfileRow> {
    load_rows()
        .into_iter()
        .find(|r| r.relay_id.as_deref() == Some(id))
}

// --- Routing config ----------------------------------------------------------

/// `"profiles"` in `~/.chrome-use/config.json`:
/// `{"default": "<selector>", "routes": [{"match": "github.com/acme/*", "profile": "Davian"}]}`.
#[derive(Debug, Default, Clone, PartialEq, serde::Deserialize)]
pub struct ProfilesConfig {
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub routes: Vec<Route>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct Route {
    #[serde(rename = "match")]
    pub pattern: String,
    pub profile: String,
}

fn config_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("AGENT_BROWSER_CONFIG").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    Some(dirs::home_dir()?.join(".chrome-use").join("config.json"))
}

pub fn parse_profiles_config(config: &Value) -> Option<ProfilesConfig> {
    serde_json::from_value(config.get("profiles")?.clone()).ok()
}

pub fn load_profiles_config() -> Option<ProfilesConfig> {
    let text = std::fs::read_to_string(config_path()?).ok()?;
    parse_profiles_config(&serde_json::from_str(&text).ok()?)
}

/// `*` matches any run of characters; everything else literally.
fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && p[pi] != '*' && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Does a route pattern (`host[/path]`) cover `url`? The host matches itself
/// and its subdomains (`*.` prefix optional); a path without `*` is a prefix
/// on segment boundaries (`github.com/acme` covers `/acme/x`, not `/acmeco`).
/// Case-insensitive.
pub fn route_matches(pattern: &str, url: &str) -> bool {
    let pattern = pattern.trim().to_lowercase();
    let pattern = pattern
        .strip_prefix("https://")
        .or_else(|| pattern.strip_prefix("http://"))
        .unwrap_or(&pattern);
    let normalized = if url.contains("://") {
        url.to_string()
    } else {
        format!("https://{url}")
    };
    let Ok(u) = url::Url::parse(&normalized) else {
        return false;
    };
    let Some(host) = u.host_str().map(str::to_lowercase) else {
        return false;
    };
    let (phost, ppath) = match pattern.split_once('/') {
        Some((h, p)) => (h, Some(p)),
        None => (pattern, None),
    };
    let phost = phost.strip_prefix("*.").unwrap_or(phost);
    if phost.is_empty() || !(host == phost || host.ends_with(&format!(".{phost}"))) {
        return false;
    }
    let Some(ppath) = ppath.map(|p| p.trim_matches('/')).filter(|p| !p.is_empty()) else {
        return true;
    };
    let path = u.path().trim_start_matches('/').to_lowercase();
    if ppath.contains('*') {
        glob(ppath, &path)
    } else {
        path == ppath || path.starts_with(&format!("{ppath}/"))
    }
}

/// The first configured route matching `url`, as `(selector, why)`.
pub fn choose_route(cfg: &ProfilesConfig, url: &str) -> Option<(String, String)> {
    cfg.routes
        .iter()
        .find(|r| route_matches(&r.pattern, url))
        .map(|r| (r.profile.clone(), format!("config route \"{}\"", r.pattern)))
}

/// The configured `profiles.default`, as `(selector, why)`.
pub fn configured_default(cfg: &ProfilesConfig) -> Option<(String, String)> {
    cfg.default
        .as_ref()
        .filter(|d| !d.trim().is_empty())
        .map(|d| (d.clone(), "config profiles.default".to_string()))
}

// --- ChooseBrowser rules vs. the session's profile ---------------------------
//
// A rule the user wrote in ChooseBrowser says which account a site belongs to.
// Opening that site in a different profile because the named one happened not
// to be connected — or because this session was bound to another one earlier —
// is the wrong account with no message, which is how claude.ai ended up in the
// wrong profile. So a matching rule is binding: use its profile or refuse with
// the fix. Explicit choices (`--browser`, `--cdp`, `--provider`, a config
// route, `--no-choosebrowser`) still win, because those were also stated by
// the user.
//
// Two places enforce it: the CLI when it picks a new session's profile (before
// any daemon exists), and the daemon right before it sends any navigation —
// whichever client asked for it (a direct command, a `batch` step, an MCP
// tool call, a script).

/// A ChooseBrowser rule that covers the url being opened, resolved against this
/// machine's `Local State` (so `dir` is this machine's directory name).
#[derive(Debug, Clone, PartialEq)]
pub struct RuleHit {
    pub host: String,
    pub rule_id: Option<String>,
    /// The portable key the rule stores (gaia id, email, name).
    pub key: String,
    /// Chrome data root the directory lives under.
    pub root: Option<String>,
    /// Empty when the key is ambiguous.
    pub dir: String,
    pub email: Option<String>,
    /// Directories the key matched when it matched more than one. Non-empty
    /// means the rule cannot be followed: which account it means is a guess.
    pub ambiguous: Vec<String>,
}

impl RuleHit {
    fn rule(&self) -> String {
        match &self.rule_id {
            Some(id) => format!("A ChooseBrowser rule ({id})"),
            None => "A ChooseBrowser rule".to_string(),
        }
    }

    /// The profile identity the rule resolves to, for comparing two hits.
    fn target(&self) -> (Option<&str>, &str, &str) {
        (self.root.as_deref(), self.dir.as_str(), self.key.as_str())
    }
}

/// What the rules say about one url being opened.
#[derive(Debug, Clone, PartialEq)]
pub enum RuleOutcome {
    None,
    Hit(RuleHit),
    /// The rule's profile no longer exists here: a warning, never a refusal.
    Stale(String),
}

/// The warning for a rule whose profile key matches nothing in `Local State`.
/// Not a refusal: a stale rule must not block every visit to its site, but the
/// user should learn that the rule they rely on is doing nothing.
pub fn stale_rule_warning(host: &str, choice: &crate::choosebrowser::ProfileChoice) -> String {
    let rule = match &choice.rule_id {
        Some(id) => format!("ChooseBrowser rule ({id})"),
        None => "ChooseBrowser rule".to_string(),
    };
    format!(
        "the {rule} for {host} names Chrome profile \"{}\", which no longer exists on this \
         machine — the rule is ignored and normal profile selection applies. Update or \
         delete it in ChooseBrowser.",
        choice.key
    )
}

/// Pure: turn a [`crate::choosebrowser::RuleLookup`] for `url` into what the
/// guard acts on. `chrome_root` is the data root `Local State` was read from.
pub fn rule_outcome(
    lookup: crate::choosebrowser::RuleLookup,
    url: &str,
    chrome_root: Option<&str>,
) -> RuleOutcome {
    use crate::choosebrowser::RuleLookup;
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| url.to_string());
    match lookup {
        RuleLookup::NoRule => RuleOutcome::None,
        RuleLookup::Stale(choice) => RuleOutcome::Stale(stale_rule_warning(&host, &choice)),
        RuleLookup::Resolved(profile, choice) => RuleOutcome::Hit(RuleHit {
            host,
            rule_id: choice.rule_id,
            key: choice.key,
            root: chrome_root.map(str::to_string),
            dir: profile.directory,
            email: profile.email,
            ambiguous: Vec::new(),
        }),
        RuleLookup::Ambiguous(choice, dirs) => RuleOutcome::Hit(RuleHit {
            host,
            rule_id: choice.rule_id,
            key: choice.key,
            root: chrome_root.map(str::to_string),
            dir: String::new(),
            email: None,
            ambiguous: dirs,
        }),
    }
}

/// The rule covering `url`, against the real files.
pub fn rule_outcome_for_url(url: &str) -> RuleOutcome {
    let root = crate::choosebrowser::chrome_root().map(|p| p.display().to_string());
    rule_outcome(crate::choosebrowser::lookup_url(url), url, root.as_deref())
}

/// The inventory row a rule's profile is: the same directory under the same
/// data root, nothing else. The directory is the identity `Local State` gave
/// us, and it is the only one that tells apart several profiles signed in to
/// the same account — so a known directory is never swapped for an email
/// match to another profile, and a directory under another root (Chrome Beta's
/// `Default`) is a different profile.
pub fn row_for_rule(rows: &[ProfileRow], hit: &RuleHit) -> Option<usize> {
    if !hit.ambiguous.is_empty() || hit.dir.is_empty() || hit.root.is_none() {
        return None;
    }
    rows.iter()
        .position(|r| r.dir.as_deref() == Some(hit.dir.as_str()) && r.root == hit.root)
}

/// Who a running session is bound to, from its live relay row or its record.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BoundProfile {
    pub relay_id: Option<String>,
    pub root: Option<String>,
    pub dir: Option<String>,
    pub label: String,
}

impl BoundProfile {
    pub fn from_row(row: &ProfileRow) -> Self {
        Self {
            relay_id: row.relay_id.clone(),
            root: row.root.clone(),
            dir: row.dir.clone(),
            label: row.label(),
        }
    }

    pub fn from_record(record: &Value) -> Self {
        let s = |k: &str| record.get(k).and_then(|v| v.as_str()).map(str::to_string);
        Self {
            relay_id: s("id"),
            root: s("root"),
            dir: s("dir"),
            label: s("label").unwrap_or_else(|| "another profile".to_string()),
        }
    }

    /// Is this `row`? `None` when there is not enough identity to tell — an
    /// old record without the data root, a relay-only row — and the caller
    /// refuses rather than guesses.
    fn is_row(&self, row: &ProfileRow) -> Option<bool> {
        if let (Some(a), Some(b)) = (self.relay_id.as_deref(), row.relay_id.as_deref()) {
            return Some(a == b);
        }
        match (&self.root, &self.dir, &row.root, &row.dir) {
            (Some(r1), Some(d1), Some(r2), Some(d2)) => Some(r1 == r2 && d1 == d2),
            _ => None,
        }
    }
}

/// Where the session stands when the rule is consulted.
#[derive(Debug, Clone, Copy)]
pub enum SessionBinding<'a> {
    /// No daemon yet: this command picks the session's profile.
    New,
    /// Running on the relay, bound to this profile. `None`: it is on the
    /// relay but which profile is unknown — refused, never treated as "not
    /// subject to rules". (Sessions off the relay never get here; see
    /// [`decide_bound`].)
    Bound(Option<&'a BoundProfile>),
}

#[derive(Debug, PartialEq)]
pub enum RuleDecision {
    /// No rule applies, or the user chose explicitly; carry on as before.
    NotApplicable,
    /// Bind the new session to this (connected) row.
    Use(usize),
    /// The running session is already on the rule's profile.
    AlreadyThere,
    /// Stop: the rule's profile cannot be used here, and using another one
    /// would be the wrong account.
    Refuse(String),
}

const OVERRIDE_HINT: &str = "To open it somewhere else on purpose, pass --browser <profile> \
                             or --no-choosebrowser.";

/// The decision for one navigation. `explicit` is true when the user already
/// named the profile some other way (`--browser`, `--profile`, `--cdp`,
/// `--provider`, a config route) — those win over the rule, as before.
pub fn decide_rule(
    rows: &[ProfileRow],
    hit: Option<&RuleHit>,
    no_choosebrowser: bool,
    explicit: bool,
    binding: SessionBinding,
    session: &str,
) -> RuleDecision {
    let Some(hit) = hit else {
        return RuleDecision::NotApplicable;
    };
    if no_choosebrowser || explicit {
        return RuleDecision::NotApplicable;
    }
    if !hit.ambiguous.is_empty() {
        return RuleDecision::Refuse(format!(
            "{} routes {} to Chrome profile key \"{}\", which matches {} profiles on this \
             machine ({}), so nothing was opened — picking one would be a guess about which \
             account the rule means. Point the rule at one profile in ChooseBrowser. \
             {OVERRIDE_HINT}",
            hit.rule(),
            hit.host,
            hit.key,
            hit.ambiguous.len(),
            hit.ambiguous.join(", "),
        ));
    }
    let Some(i) = row_for_rule(rows, hit) else {
        return RuleDecision::Refuse(format!(
            "{} routes {} to Chrome profile directory \"{}\", which chrome-use cannot find \
             among this machine's profiles, so nothing was opened (chrome-use does not \
             substitute a different profile for the one a rule names). Check \
             `chrome-use browsers`. {OVERRIDE_HINT}",
            hit.rule(),
            hit.host,
            hit.dir
        ));
    };
    let row = &rows[i];
    let selector = suggested_selector(rows, i);
    match binding {
        SessionBinding::New => {
            if row.connected() {
                RuleDecision::Use(i)
            } else {
                RuleDecision::Refuse(format!(
                    "{} routes {} to Chrome profile {}, which is not connected to chrome-use, \
                     so nothing was opened (chrome-use does not substitute a different profile \
                     for the one a rule names). Fix: {} {OVERRIDE_HINT}",
                    hit.rule(),
                    hit.host,
                    row.label(),
                    not_connected_message(&selector, row)
                ))
            }
        }
        SessionBinding::Bound(bound) => {
            let same = bound.and_then(|b| b.is_row(row));
            if same == Some(true) {
                return RuleDecision::AlreadyThere;
            }
            let connect_first = if row.connected() {
                String::new()
            } else {
                format!(
                    " That profile is not connected yet either: {}",
                    not_connected_message(&selector, row)
                )
            };
            let Some(bound) = bound.filter(|_| same.is_some()) else {
                return RuleDecision::Refuse(format!(
                    "{} routes {} to Chrome profile {}, but chrome-use cannot tell which \
                     profile session \"{session}\" is bound to, so nothing was opened. Open it \
                     in a new session, which picks the rule's profile: add --session \
                     <new-name>.{connect_first} Or pass --no-choosebrowser to open it in this \
                     session anyway.",
                    hit.rule(),
                    hit.host,
                    row.label(),
                ));
            };
            RuleDecision::Refuse(format!(
                "{} routes {} to Chrome profile {}, but session \"{session}\" is bound to {}, \
                 so nothing was opened (a running session does not switch profiles). Open it in \
                 a new session, which picks the rule's profile: add --session <new-name>.\
                 {connect_first} Or pass --no-choosebrowser to open it in {} anyway.",
                hit.rule(),
                hit.host,
                row.label(),
                bound.label,
                bound.label,
            ))
        }
    }
}

/// One command (a `batch`) whose navigations fall under rules naming
/// different profiles cannot run in one session without opening some of them
/// in the wrong account. Refused up front, before anything opens.
pub fn conflicting_rules(hits: &[RuleHit]) -> Option<String> {
    let first = hits.first()?;
    if hits.iter().all(|h| h.target() == first.target()) {
        return None;
    }
    let mut seen: Vec<String> = Vec::new();
    for h in hits {
        let target = if h.ambiguous.is_empty() {
            h.dir.clone()
        } else {
            format!("key \"{}\"", h.key)
        };
        let rule = h
            .rule_id
            .as_deref()
            .map(|r| format!(" (rule {r})"))
            .unwrap_or_default();
        let line = format!("{}{rule} → {target}", h.host);
        if !seen.contains(&line) {
            seen.push(line);
        }
    }
    Some(format!(
        "this command opens sites that ChooseBrowser rules send to different Chrome profiles \
         ({}), and one session is bound to one profile, so nothing was run. Split it into one \
         --session per profile, or pass --no-choosebrowser to run it all in one profile.",
        seen.join("; ")
    ))
}

/// Join a note onto a response's `warning` without dropping one already there
/// (the newline-joined shape the daemon uses).
pub fn merge_warning(existing: Option<&str>, note: &str) -> String {
    match existing.filter(|e| !e.is_empty()) {
        Some(e) => format!("{note}\n{e}"),
        None => note.to_string(),
    }
}

/// The http(s) url a navigation target means, for the rule lookup; `None` for
/// anything a site rule cannot be about (`about:`, `data:`, `file:`, …). A
/// bare host gets `https://`, the way `open` treats it, so `claude.ai`,
/// `localhost:8080` and `[::1]:3000` are all looked up as sites.
pub fn guard_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let lower = raw.to_ascii_lowercase();
    let candidate = if lower.starts_with("http://") || lower.starts_with("https://") {
        raw.to_string()
    } else if lower.contains("://")
        || [
            "about:",
            "data:",
            "file:",
            "javascript:",
            "blob:",
            "chrome:",
        ]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        return None;
    } else {
        format!("https://{raw}")
    };
    let u = url::Url::parse(&candidate).ok()?;
    u.host_str()?;
    Some(u.to_string())
}

/// What the daemon knows about the profile its session drives.
#[derive(Debug, Clone, PartialEq)]
pub enum BoundState {
    /// The live relay row for its endpoint, or the session record.
    Known(BoundProfile),
    /// It drives a relay endpoint, but no row or record says which profile.
    /// Rules still apply — and refuse, since the profile cannot be matched.
    UnknownRelay,
    /// Not a relay endpoint (a `--launch` or `--cdp` browser), or no browser
    /// yet. No ChooseBrowser rule is about it.
    NotRelay,
}

/// Pure: classify the session from its endpoint, the relay rows, its record
/// and every endpoint the relay has published.
pub fn bound_state(
    rows: &[ProfileRow],
    bound_ws: Option<&str>,
    record: Option<&Value>,
    relay_endpoints: &[String],
) -> BoundState {
    if let Some(row) = bound_ws.and_then(|ws| rows.iter().find(|r| r.ws.as_deref() == Some(ws))) {
        return BoundState::Known(BoundProfile::from_row(row));
    }
    if let Some(record) = record {
        return BoundState::Known(BoundProfile::from_record(record));
    }
    match bound_ws {
        Some(ws) if relay_endpoints.iter().any(|e| e == ws) => BoundState::UnknownRelay,
        _ => BoundState::NotRelay,
    }
}

/// The daemon-side guard: right before a navigation is sent, whatever asked
/// for it. `Err` refuses with the message; `Ok(Some)` is a warning to attach.
/// `bound_ws` is the endpoint the session's browser is connected to (`None`
/// before any browser).
pub fn guard_navigation(
    url: &str,
    skip: bool,
    bound_ws: Option<&str>,
    session: &str,
) -> Result<Option<String>, String> {
    if skip {
        return Ok(None);
    }
    check_bound_navigation(url, session, |rows| {
        bound_state(
            rows,
            bound_ws,
            session_profile(session).as_ref(),
            &crate::connect::relay_endpoints(),
        )
    })
}

/// The url a command sent to the daemon navigates to, if it navigates.
pub fn navigation_target(cmd: &Value) -> Option<&str> {
    match cmd.get("action")?.as_str()? {
        "navigate" | "tab_new" | "a11y" => cmd.get("url")?.as_str(),
        _ => None,
    }
}

/// The client-side guard, in `send_command`: the same check, run before the
/// command leaves the CLI, against the session's recorded profile. A session
/// with no record (a `--launch` or `--cdp` session, or one whose profile the
/// CLI could not place) is left to the daemon, which knows its live endpoint.
/// Stale-rule warnings are left to the daemon too, so they are said once.
pub fn guard_outgoing(cmd: &Value, session: &str, skip: bool) -> Result<(), String> {
    if skip {
        return Ok(());
    }
    let Some(url) = navigation_target(cmd) else {
        return Ok(());
    };
    let Some(record) = session_profile(session) else {
        return Ok(());
    };
    check_bound_navigation(url, session, |_| {
        BoundState::Known(BoundProfile::from_record(&record))
    })
    .map(|_| ())
}

/// Shared by both guards: does a running session's profile match the rule
/// covering `url`? `Err` refuses; `Ok(Some)` warns.
fn check_bound_navigation(
    url: &str,
    session: &str,
    bound: impl FnOnce(&[ProfileRow]) -> BoundState,
) -> Result<Option<String>, String> {
    let Some(url) = guard_url(url) else {
        return Ok(None);
    };
    if load_profiles_config()
        .and_then(|cfg| choose_route(&cfg, &url))
        .is_some()
    {
        return Ok(None);
    }
    let hit = match rule_outcome_for_url(&url) {
        RuleOutcome::None => return Ok(None),
        RuleOutcome::Stale(w) => return Ok(Some(w)),
        RuleOutcome::Hit(h) => h,
    };
    let rows = load_rows();
    match decide_bound(&rows, &hit, bound(&rows), session) {
        RuleDecision::Refuse(msg) => Err(msg),
        _ => Ok(None),
    }
}

/// Pure: the decision for a running session in `state`. A relay session
/// whose profile is unknown goes in as `Bound(None)` and is refused; only a
/// session that is not on the relay at all is outside the rules.
pub fn decide_bound(
    rows: &[ProfileRow],
    hit: &RuleHit,
    state: BoundState,
    session: &str,
) -> RuleDecision {
    let bound = match state {
        BoundState::NotRelay => return RuleDecision::NotApplicable,
        BoundState::UnknownRelay => None,
        BoundState::Known(b) => Some(b),
    };
    decide_rule(
        rows,
        Some(hit),
        false,
        false,
        SessionBinding::Bound(bound.as_ref()),
        session,
    )
}

// --- Per-session record ------------------------------------------------------

fn session_profile_path(session: &str) -> PathBuf {
    crate::connection::get_socket_dir().join(format!("{session}.browser-profile"))
}

/// Remember which profile a session bound to, and why, so later commands
/// (`open`, `browsers`) can say so without re-deciding.
pub fn record_session_profile(session: &str, row: &ProfileRow, reason: &str) {
    let mut v = row.to_json();
    v["reason"] = json!(reason);
    v["label"] = json!(row.label());
    let _ = std::fs::write(session_profile_path(session), v.to_string());
}

pub fn session_profile(session: &str) -> Option<Value> {
    let text = std::fs::read_to_string(session_profile_path(session)).ok()?;
    serde_json::from_str(&text).ok()
}

/// `profile: Davian (Profile 14, x@gmail.com) — config route "github.com/acme/*"`.
pub fn profile_line(record: &Value) -> String {
    let label = record.get("label").and_then(|v| v.as_str()).unwrap_or("?");
    match record.get("reason").and_then(|v| v.as_str()) {
        Some(r) if !r.is_empty() => format!("profile: {label} — {r}"),
        _ => format!("profile: {label}"),
    }
}

/// JSON shape of the per-session record for `--json` output.
pub fn profile_json(record: &Value) -> Value {
    json!({
        "name": record.get("name"),
        "dir": record.get("dir"),
        "email": record.get("email"),
        "id": record.get("id"),
        "reason": record.get("reason"),
    })
}

// --- `chrome-use browsers` ---------------------------------------------------

/// Terminal column width: CJK and other wide characters take two cells.
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| if (c as u32) >= 0x1100 { 2 } else { 1 })
        .sum()
}

fn pad(s: &str, w: usize) -> String {
    let n = display_width(s);
    format!("{s}{}", " ".repeat(w.saturating_sub(n)))
}

/// The profile `browsers` marks as default: the configured `profiles.default`
/// when it names a connected profile, else the one the CLI drives today.
fn default_index(rows: &[ProfileRow], cfg: Option<&ProfilesConfig>) -> Option<usize> {
    if let Some(sel) = cfg.and_then(|c| c.default.as_deref()) {
        if let Ok(i) = resolve_connected_in(rows, sel) {
            return Some(i);
        }
    }
    let (id, _) = connect::driving_profile()?;
    rows.iter()
        .position(|r| r.relay_id.as_deref() == Some(id.as_str()))
}

pub fn run_browsers(args: &[String], session: &str, json_out: bool) {
    if let Some(pos) = args.iter().position(|a| a == "--who") {
        match args.get(pos + 1).filter(|d| !d.starts_with("--")) {
            Some(domain) => run_who(domain, json_out),
            None => {
                fail(json_out, "usage: chrome-use browsers --who <domain>");
            }
        }
        return;
    }
    let rows = load_rows();
    let cfg = load_profiles_config();
    let default = default_index(&rows, cfg.as_ref());
    let session_id = crate::connection::daemon_ready(session)
        .then(|| session_profile(session))
        .flatten()
        .and_then(|v| {
            v.get("id")
                .and_then(|x| x.as_str())
                .map(ToString::to_string)
        });
    let in_session =
        |r: &ProfileRow| session_id.is_some() && r.relay_id.as_deref() == session_id.as_deref();

    if json_out {
        let arr: Vec<Value> = rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let mut v = r.to_json();
                v["wsUrl"] = json!(r.ws);
                v["default"] = json!(default == Some(i));
                v["session"] = json!(in_session(r));
                if !r.connected() {
                    v["connectCommand"] = json!(connect_command(&suggested_selector(&rows, i)));
                }
                v
            })
            .collect();
        println!("{}", json!({"success": true, "data": {"browsers": arr}}));
        return;
    }

    if rows.is_empty() {
        let host = connect::native_host_report();
        if !host.manifests.is_empty() && !host.is_healthy() {
            let bin = host.target_bin.as_deref().unwrap_or("<unresolved>");
            println!(
                "no Chrome profiles found and none connected.\n\
                 The native-messaging host is broken: its launcher points at a binary that is \
                 missing or not executable ({bin}).\n  \
                 chrome-use extension connect      (repoints the host at this binary)\n  \
                 chrome-use doctor                 (full diagnosis)"
            );
        } else {
            println!(
                "no Chrome profiles found and none connected.\n\
                 (if Chrome is running, try `chrome-use reconnect` or `chrome-use doctor`.)"
            );
        }
        return;
    }

    let header = [
        "PROFILE",
        "DIR",
        "ACCOUNT",
        "CONNECTED",
        "DEFAULT",
        "SESSION",
        "",
    ];
    let mut table: Vec<[String; 7]> = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let connected = if r.connected() {
            "yes".to_string()
        } else if r.extension_disabled {
            "no (disabled)".to_string()
        } else if r.has_extension {
            "no (not open)".to_string()
        } else {
            "no".to_string()
        };
        table.push([
            r.name.clone().unwrap_or_else(|| "-".into()),
            r.dir.clone().unwrap_or_else(|| "-".into()),
            r.account().unwrap_or("-").to_string(),
            connected,
            if default == Some(i) { "*" } else { "" }.to_string(),
            if in_session(r) { "*" } else { "" }.to_string(),
            if r.connected() {
                String::new()
            } else {
                connect_command(&suggested_selector(&rows, i))
            },
        ]);
    }
    let mut widths = [0usize; 7];
    for (c, h) in header.iter().enumerate() {
        widths[c] = display_width(h);
    }
    for row in &table {
        for (w, cell) in widths.iter_mut().zip(row.iter()) {
            *w = (*w).max(display_width(cell));
        }
    }
    let render = |cells: &[String]| -> String {
        let mut line = String::new();
        for (c, cell) in cells.iter().enumerate() {
            if c == cells.len() - 1 {
                line.push_str(cell);
            } else {
                line.push_str(&pad(cell, widths[c]));
                line.push_str("  ");
            }
        }
        line.trim_end().to_string()
    };
    let head: Vec<String> = header.iter().map(|s| s.to_string()).collect();
    println!("{}", render(&head));
    for row in &table {
        println!("{}", render(row));
    }
    println!(
        "\nDrive one: --browser <name|dir|email>; a unique prefix of the name works too{}.",
        rows.iter()
            .enumerate()
            .filter(|(_, r)| r.connected())
            .find_map(|(i, r)| {
                // The shortest prefix of a connected profile's name that
                // picks it alone — a live example, never an ambiguous one.
                let name = r.name.as_deref()?;
                (1..=name.chars().count())
                    .map(|n| name.chars().take(n).collect::<String>())
                    .find(|p| match_selector(&rows, p) == Match::One(i))
            })
            .map(|p| format!(" (e.g. --browser {})", shell_quote(&p)))
            .unwrap_or_default()
    );
    if rows.iter().any(|r| !r.connected()) {
        println!(
            "Not connected: the command on its row connects it — it opens a window in that \
             profile (one click if the extension isn't there yet). Agents: ask the user first."
        );
    }
}

fn fail(json_out: bool, msg: &str) {
    if json_out {
        println!("{}", json!({"success": false, "error": msg}));
    } else {
        eprintln!("{} {msg}", color::error_indicator());
    }
    std::process::exit(1);
}

// --- `browsers --who <domain>` -----------------------------------------------

/// Cookies that only exist while signed in, for sites where we know them. A
/// hit here is reported as "signed in"; elsewhere only as "session cookies".
const KNOWN_SESSION_COOKIES: &[(&str, &[&str])] = &[
    (
        "github.com",
        &["user_session", "__Host-user_session_same_site"],
    ),
    ("google.com", &["SID", "__Secure-1PSID", "__Secure-3PSID"]),
    ("x.com", &["auth_token"]),
    ("twitter.com", &["auth_token"]),
    ("reddit.com", &["reddit_session", "token_v2"]),
    ("gitlab.com", &["_gitlab_session"]),
    ("linkedin.com", &["li_at"]),
    ("facebook.com", &["c_user"]),
    ("instagram.com", &["sessionid"]),
    ("zhihu.com", &["z_c0"]),
    ("bilibili.com", &["SESSDATA"]),
    ("weibo.com", &["SUB"]),
    ("xiaohongshu.com", &["web_session"]),
];

/// Present even when signed out, or rotated by CDNs/bot defence: never
/// evidence of a session.
const NOT_SESSION: &[&str] = &[
    "logged_in",
    "__cf_bm",
    "cf_clearance",
    "_cfuvid",
    "__cflb",
    "_octo",
    "tz",
    "preferred_color_mode",
];

/// Names that look like a session/auth cookie when we have no site list.
pub fn looks_like_session_cookie(name: &str) -> bool {
    if NOT_SESSION.iter().any(|n| n.eq_ignore_ascii_case(name)) {
        return false;
    }
    let lc = name.to_lowercase();
    if lc.starts_with("_ga") || lc.starts_with("_gid") || lc.starts_with("_gcl") {
        return false;
    }
    ["sess", "auth", "token", "sid", "login", "jwt"]
        .iter()
        .any(|k| lc.contains(k))
}

#[derive(Debug, PartialEq)]
pub enum WhoState {
    SignedIn(Vec<String>),
    SessionCookies(Vec<String>),
    None,
}

/// Classify the (unexpired) cookie names a profile holds for `domain`.
pub fn classify_cookies(domain: &str, names: &[String]) -> WhoState {
    let domain = domain.trim_start_matches('.').to_lowercase();
    let known = KNOWN_SESSION_COOKIES
        .iter()
        .find(|(d, _)| domain == *d || domain.ends_with(&format!(".{d}")))
        .map(|(_, n)| *n);
    let mut uniq: Vec<String> = names.to_vec();
    uniq.sort();
    uniq.dedup();
    if let Some(known) = known {
        let hits: Vec<String> = uniq
            .iter()
            .filter(|n| known.iter().any(|k| k == n))
            .cloned()
            .collect();
        if !hits.is_empty() {
            return WhoState::SignedIn(hits);
        }
    }
    let generic: Vec<String> = uniq
        .into_iter()
        .filter(|n| looks_like_session_cookie(n))
        .collect();
    if generic.is_empty() {
        WhoState::None
    } else {
        WhoState::SessionCookies(generic)
    }
}

fn valid_domain(d: &str) -> bool {
    !d.is_empty()
        && d.len() <= 253
        && d.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && !d.starts_with('-')
}

/// Names (never values) of the unexpired cookies a profile's on-disk store
/// holds for `domain` and its subdomains. Reads a copy, so a running Chrome
/// is neither blocked nor disturbed.
fn cookie_names(root: &str, dir: &str, domain: &str) -> Result<Vec<String>, String> {
    let base = Path::new(root).join(dir);
    let db = [base.join("Network").join("Cookies"), base.join("Cookies")]
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| "no cookie store".to_string())?;
    let tmp = std::env::temp_dir().join(format!("chrome-use-who-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let result = query_cookie_names(&db, &tmp.join("Cookies"), domain);
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

fn query_cookie_names(db: &Path, tmp_db: &Path, domain: &str) -> Result<Vec<String>, String> {
    {
        std::fs::copy(db, tmp_db).map_err(|e| format!("copy failed: {e}"))?;
        for suffix in ["-wal", "-shm"] {
            let mut s = db.to_path_buf().into_os_string();
            s.push(suffix);
            let mut d = tmp_db.to_path_buf().into_os_string();
            d.push(suffix);
            let _ = std::fs::copy(PathBuf::from(s), PathBuf::from(d));
        }
        // Chrome stores expiry as microseconds since 1601-01-01.
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let now_chrome = (now_unix + 11_644_473_600) * 1_000_000;
        // The domain, its subdomains, and domain cookies of its parents
        // (`.cloudflare.com` cookies are sent to `dash.cloudflare.com`).
        let mut hosts = vec![
            format!("host_key = '{domain}'"),
            format!("host_key LIKE '%.{domain}'"),
        ];
        let labels: Vec<&str> = domain.split('.').collect();
        for i in 0..labels.len().saturating_sub(1) {
            hosts.push(format!("host_key = '.{}'", labels[i..].join(".")));
        }
        let sql = format!(
            "SELECT name FROM cookies WHERE ({}) AND (has_expires = 0 OR expires_utc > {now_chrome});",
            hosts.join(" OR ")
        );
        let out = std::process::Command::new("sqlite3")
            .arg(tmp_db)
            .arg(&sql)
            .output()
            .map_err(|e| format!("sqlite3 unavailable: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "sqlite3: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }
}

/// A local site adapter that reports the signed-in account, if one exists
/// for this domain (`github.com` → `github/me`).
fn whoami_adapter(domain: &str) -> Option<String> {
    let labels: Vec<&str> = domain.trim_start_matches('.').split('.').collect();
    let site = if labels.len() >= 2 {
        labels[labels.len() - 2]
    } else {
        labels.first()?
    };
    let dir = dirs::home_dir()?
        .join(".chrome-use")
        .join("sites")
        .join(site);
    ["me", "whoami"]
        .into_iter()
        .find(|n| dir.join(format!("{n}.js")).is_file())
        .map(|n| format!("{site}/{n}"))
}

fn run_who(domain: &str, json_out: bool) {
    let domain = domain
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("")
        .trim_start_matches('.')
        .to_lowercase();
    if !valid_domain(&domain) {
        fail(json_out, &format!("--who: '{domain}' is not a domain"));
        return;
    }
    let rows = load_rows();
    let adapter = whoami_adapter(&domain);
    let mut out: Vec<Value> = Vec::new();
    let mut lines: Vec<[String; 4]> = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let (Some(root), Some(dir)) = (&r.root, &r.dir) else {
            continue;
        };
        let (state, cookies, error) = match cookie_names(root, dir, &domain) {
            Ok(names) => match classify_cookies(&domain, &names) {
                WhoState::SignedIn(n) => ("signed in", n, None),
                WhoState::SessionCookies(n) => ("session cookies present", n, None),
                WhoState::None => ("none", Vec::new(), None),
            },
            Err(e) => ("unknown", Vec::new(), Some(e)),
        };
        let hint = match (&adapter, r.connected(), state) {
            (Some(a), true, "signed in" | "session cookies present") => Some(format!(
                "chrome-use --browser {} site {a}",
                shell_quote(&suggested_selector(&rows, i))
            )),
            _ => None,
        };
        out.push(json!({
            "profile": r.to_json(),
            "state": state,
            "cookieNames": cookies,
            "error": error,
            "whoamiCommand": hint,
        }));
        let mut detail = if cookies.is_empty() {
            error.unwrap_or_default()
        } else {
            let shown: Vec<&str> = cookies.iter().take(3).map(String::as_str).collect();
            format!(
                "{}{}",
                shown.join(", "),
                if cookies.len() > 3 { ", …" } else { "" }
            )
        };
        if let Some(h) = &hint {
            detail = format!("{detail}  → as whom: {h}");
        }
        lines.push([
            r.label(),
            if r.connected() { "yes" } else { "no" }.to_string(),
            state.to_string(),
            detail,
        ]);
    }
    if json_out {
        println!(
            "{}",
            json!({"success": true, "data": {"domain": domain, "profiles": out}})
        );
        return;
    }
    if lines.is_empty() {
        println!("no Chrome profiles found on this machine.");
        return;
    }
    let header = ["PROFILE", "CONNECTED", domain.as_str(), "COOKIE NAMES"];
    let mut w = [0usize; 3];
    for (c, wc) in w.iter_mut().enumerate() {
        *wc = lines
            .iter()
            .map(|l| display_width(&l[c]))
            .chain([display_width(header[c])])
            .max()
            .unwrap_or(0);
    }
    println!(
        "{}  {}  {}  {}",
        pad(header[0], w[0]),
        pad(header[1], w[1]),
        pad(header[2], w[2]),
        header[3]
    );
    for l in &lines {
        println!(
            "{}",
            format!(
                "{}  {}  {}  {}",
                pad(&l[0], w[0]),
                pad(&l[1], w[1]),
                pad(&l[2], w[2]),
                l[3]
            )
            .trim_end()
        );
    }
    println!(
        "\nFrom each profile's cookie store on disk (names only, never values; Chrome writes \
         new cookies to disk within ~30s). \"signed in\" = a cookie {domain} only sets for a \
         signed-in user; \"session cookies present\" = session-like names, not proof."
    );
}

// --- `chrome-use connect --browser <selector>` ---------------------------------

/// Connect one more profile, lazily: open the Web Store page in that profile
/// (one click: "Add to Chrome"), or — if the extension is already there —
/// open a window so its worker starts, then wait for its relay.
pub fn run_connect_profile(selector: &str, args: &[String], json_out: bool) {
    let wait_secs: u64 = args
        .iter()
        .position(|a| a == "--wait")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);
    let rows = load_rows();
    let i = match resolve_in(&rows, selector) {
        Ok(i) => i,
        Err(e) => {
            fail(json_out, &e);
            return;
        }
    };
    let row = rows[i].clone();
    let done = |row: &ProfileRow, already: bool| {
        if json_out {
            println!(
                "{}",
                json!({"success": true, "data": {"connected": true, "alreadyConnected": already, "profile": row.to_json()}})
            );
        } else if already {
            println!(
                "{} {} is already connected — use --browser {}",
                color::success_indicator(),
                row.label(),
                shell_quote(selector.trim())
            );
        } else {
            println!(
                "{} connected {} — use --browser {}",
                color::success_indicator(),
                row.label(),
                shell_quote(selector.trim())
            );
        }
    };
    if row.connected() {
        done(&row, true);
        return;
    }
    let local = connect::chrome_profiles()
        .into_iter()
        .find(|p| Some(&p.dir) == row.dir.as_ref() && Some(&p.root) == row.root.as_ref());
    let Some(local) = local else {
        fail(
            json_out,
            &format!(
                "can't find {} on disk — open it in Chrome yourself, then re-run.",
                row.label()
            ),
        );
        return;
    };
    // The relay needs the native-messaging host; registering it is idempotent
    // and touches no browser state, so do it rather than describe it.
    connect::ensure_host_installed();
    let host = connect::native_host_report();
    if host.manifests.is_empty() || !host.is_healthy() {
        fail(
            json_out,
            "the native-messaging host isn't registered correctly, so no profile can connect — \
             run `chrome-use extension connect` (or `chrome-use doctor`) first.",
        );
        return;
    }

    let (opened, action, told) = match &local.extension {
        Some(ext) if !ext.disable_reasons.is_empty() => {
            let url = format!("chrome://extensions/?id={}", ext.id);
            (
                connect::open_in_profile(&local, Some(&url)),
                "enable",
                format!(
                    "The chrome-use extension is installed in {} but Chrome has it disabled \
                     ({}). Turn it on (and accept any permission prompt) in the extensions \
                     page that just opened in that profile.",
                    row.label(),
                    ext.disable_reasons.join(", ")
                ),
            )
        }
        Some(_) => (
            connect::open_in_profile(&local, None),
            "open-window",
            format!(
                "The extension is installed in {} but that profile isn't open. Opened a window \
                 in it so the extension can start.",
                row.label()
            ),
        ),
        None => (
            connect::open_in_profile(&local, Some(connect::STORE_URL)),
            "store",
            format!(
                "Opened the chrome-use page of the Chrome Web Store in {}. Click \"Add to \
                 Chrome\" there — that's the only step; Chrome keeps it updated after.",
                row.label()
            ),
        ),
    };
    if !opened {
        let manual = match action {
            "store" => format!("open {} in that profile", connect::STORE_URL),
            _ => "open that profile from Chrome's profile menu".to_string(),
        };
        fail(
            json_out,
            &format!(
                "couldn't open a Chrome window in {} automatically — {manual}, then re-run.",
                row.label()
            ),
        );
        return;
    }
    if !json_out {
        eprintln!("{told}");
        eprint!("waiting up to {wait_secs}s for it to connect ");
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait_secs);
    loop {
        if let Some(now) = load_rows()
            .into_iter()
            .find(|r| r.same_profile(&row) && r.connected())
        {
            if !json_out {
                eprintln!();
            }
            done(&now, false);
            return;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        if !json_out {
            eprint!(".");
            let _ = std::io::Write::flush(&mut std::io::stderr());
        }
        std::thread::sleep(std::time::Duration::from_millis(1500));
    }
    if !json_out {
        eprintln!();
    }
    let msg = format!(
        "{} isn't connected yet ({}). Re-run `{}` to keep waiting.",
        row.label(),
        match action {
            "store" => "was \"Add to Chrome\" clicked?",
            "enable" => "is the extension switched on?",
            _ => "the extension's worker can take ~30s to wake",
        },
        connect_command(selector.trim())
    );
    if json_out {
        println!(
            "{}",
            json!({"success": false, "error": msg, "data": {"connected": false, "action": action, "profile": row.to_json()}})
        );
    } else {
        eprintln!("{} {msg}", color::error_indicator());
    }
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(dir: &str, name: &str, email: Option<&str>, _ext: bool) -> ChromeProfileInfo {
        let state = json!({"profile": {"info_cache": {dir: {
            "name": name, "user_name": email.unwrap_or(""), "gaia_name": "G"
        }}}});
        connect::parse_local_state_profiles(Path::new("/root"), &state)
            .pop()
            .unwrap()
    }

    #[test]
    fn parses_local_state_info_cache() {
        let state = json!({"profile": {"info_cache": {
            "Profile 14": {"name": "Davian", "user_name": "d@x.com", "gaia_name": "Davian P"},
            "Default": {"name": "Leo", "user_name": "", "gaia_name": ""},
            "Profile 2": {"name": "  "}
        }}});
        let ps = connect::parse_local_state_profiles(Path::new("/r"), &state);
        let dirs: Vec<&str> = ps.iter().map(|p| p.dir.as_str()).collect();
        assert_eq!(dirs, ["Default", "Profile 2", "Profile 14"]);
        assert_eq!(ps[0].name.as_deref(), Some("Leo"));
        assert_eq!(ps[0].email, None);
        assert_eq!(ps[1].name, None);
        assert_eq!(ps[2].email.as_deref(), Some("d@x.com"));
        assert_eq!(ps[2].gaia_name.as_deref(), Some("Davian P"));
        assert_eq!(ps[2].root, "/r");
        assert!(connect::parse_local_state_profiles(Path::new("/r"), &json!({})).is_empty());
    }

    fn rows() -> Vec<ProfileRow> {
        let locals = vec![
            local("Default", "Leo", Some("leo@gmail.com"), false),
            local("Profile 14", "Davian", Some("davian@gmail.com"), false),
            local("Profile 12", "wind", Some("wind@gmail.com"), false),
            local("Profile 7", "wind", Some("wind@gmail.com"), false),
            local("Profile 13", "dora", Some("dora@gmail.com"), false),
        ];
        let relays = vec![
            (
                "27ade1bc-0000".to_string(),
                Some("leo@gmail.com".to_string()),
                "ws://a".to_string(),
            ),
            ("9f00aa11-0000".to_string(), None, "ws://b".to_string()),
        ];
        // 9f00… has no email: only the storage scan places it.
        join_rows(&locals, &relays, &|id| {
            (id == "9f00aa11-0000").then(|| ("/root".to_string(), "Profile 14".to_string()))
        })
    }

    #[test]
    fn join_places_relays_by_storage_then_unique_email() {
        let r = rows();
        assert_eq!(r.len(), 5);
        assert_eq!(r[0].ws.as_deref(), Some("ws://a")); // by email
        assert_eq!(r[1].ws.as_deref(), Some("ws://b")); // by storage scan
        assert!(!r[2].connected());
        // A relay nobody can place still gets a row.
        let extra = join_rows(
            &[],
            &[("x-1".into(), Some("a@b".into()), "ws://c".into())],
            &|_| None,
        );
        assert_eq!(extra.len(), 1);
        assert_eq!(extra[0].label(), "a@b (x-1)");
        // Two profiles share an email: email alone must not pick one.
        let shared = join_rows(
            &[
                local("Profile 12", "wind", Some("w@x"), false),
                local("Profile 7", "wind", Some("w@x"), false),
            ],
            &[("id-wind-1".into(), Some("w@x".into()), "ws://w".into())],
            &|_| None,
        );
        assert_eq!(shared.len(), 3);
        assert!(shared[2].dir.is_none());
    }

    #[test]
    fn selector_exact_dir_email_name_and_id() {
        let r = rows();
        assert_eq!(match_selector(&r, "Profile 14"), Match::One(1));
        assert_eq!(match_selector(&r, "profile 14"), Match::One(1));
        assert_eq!(match_selector(&r, "DAVIAN"), Match::One(1));
        assert_eq!(match_selector(&r, "davian@gmail.com"), Match::One(1));
        assert_eq!(match_selector(&r, "27ade1bc-0000"), Match::One(0));
        assert_eq!(match_selector(&r, "Default"), Match::One(0));
    }

    #[test]
    fn selector_prefix_and_ambiguity() {
        let r = rows();
        // "da" → only Davian; "d" → Davian and dora: an error, never a guess,
        // even though only Davian is connected.
        assert_eq!(match_selector(&r, "da"), Match::One(1));
        assert_eq!(match_selector(&r, "d"), Match::Ambiguous(vec![1, 4]));
        assert_eq!(match_selector(&r, "l"), Match::One(0));
        // Two profiles named "wind", neither connected → ambiguous; the
        // directory still picks one.
        assert_eq!(match_selector(&r, "wind"), Match::Ambiguous(vec![2, 3]));
        assert_eq!(match_selector(&r, "Profile 7"), Match::One(3));
        let e = resolve_in(&r, "wind").unwrap_err();
        assert!(e.contains("matches 2") && e.contains("Profile 12") && e.contains("Profile 7"));
        assert_eq!(match_selector(&r, "nobody"), Match::None);
        assert_eq!(match_selector(&r, "  "), Match::None);
    }

    #[test]
    fn selector_legacy_forms_still_work() {
        let r = rows();
        assert_eq!(match_selector(&r, "9f00"), Match::One(1)); // id prefix
        assert_eq!(match_selector(&r, "LEO@gmail"), Match::One(0)); // email substring
                                                                    // an email substring hitting several profiles, two of them connected
        assert_eq!(
            match_selector(&r, "@gmail.com"),
            Match::Ambiguous(vec![0, 1, 2, 3, 4])
        );
    }

    #[test]
    fn unconnected_match_says_how_to_connect() {
        let r = rows();
        let e = resolve_connected_in(&r, "dora").unwrap_err();
        assert!(e.starts_with("profile \"dora\" doesn't have the chrome-use extension yet"));
        assert!(e.contains("`chrome-use connect --browser dora`"));
        assert!(e.contains("ask the user"));
        let mut with_ext = r.clone();
        with_ext[4].has_extension = true;
        let e = resolve_connected_in(&with_ext, "dora").unwrap_err();
        assert!(e.contains("isn't open in Chrome"));
        let e = resolve_connected_in(&r, "Profile 12").unwrap_err();
        assert!(e.contains("connect --browser \"Profile 12\""));
        assert_eq!(resolve_connected_in(&r, "dav"), Ok(1));
    }

    #[test]
    fn suggested_selector_prefers_unique_name() {
        let r = rows();
        assert_eq!(suggested_selector(&r, 1), "Davian");
        assert_eq!(suggested_selector(&r, 2), "Profile 12");
    }

    #[test]
    fn labels() {
        let r = rows();
        assert_eq!(r[1].label(), "Davian (Profile 14, davian@gmail.com)");
    }

    #[test]
    fn route_matching() {
        assert!(route_matches(
            "dash.cloudflare.com",
            "https://dash.cloudflare.com/x/y"
        ));
        assert!(!route_matches(
            "dash.cloudflare.com",
            "https://cloudflare.com/"
        ));
        assert!(route_matches(
            "cloudflare.com",
            "https://dash.cloudflare.com/"
        ));
        assert!(route_matches(
            "*.cloudflare.com",
            "https://dash.cloudflare.com/"
        ));
        assert!(!route_matches(
            "cloudflare.com",
            "https://notcloudflare.com/"
        ));
        assert!(route_matches(
            "github.com/acme/*",
            "https://github.com/acme/repo/pulls"
        ));
        assert!(route_matches("github.com/ACME/*", "github.com/acme/repo"));
        assert!(!route_matches(
            "github.com/acme/*",
            "https://github.com/other/repo"
        ));
        assert!(route_matches("github.com/acme", "https://github.com/acme"));
        assert!(route_matches(
            "github.com/acme",
            "https://github.com/acme/x"
        ));
        assert!(!route_matches(
            "github.com/acme",
            "https://github.com/acmeco"
        ));
        assert!(route_matches(
            "github.com/*/settings",
            "https://github.com/a/settings"
        ));
        assert!(!route_matches("", "https://github.com/"));
        assert!(!route_matches("github.com", "not a url at all"));
    }

    #[test]
    fn configured_choice_route_then_default() {
        let cfg = parse_profiles_config(&json!({"report": {"auto": true}, "profiles": {
            "default": "Leo",
            "routes": [
                {"match": "dash.cloudflare.com", "profile": "Leo"},
                {"match": "github.com/acme/*", "profile": "Davian"}
            ]
        }}))
        .unwrap();
        let choose_configured = |cfg: &ProfilesConfig, url: Option<&str>| {
            url.and_then(|u| choose_route(cfg, u))
                .or_else(|| configured_default(cfg))
        };
        assert_eq!(
            choose_configured(&cfg, Some("https://github.com/acme/x")),
            Some((
                "Davian".to_string(),
                "config route \"github.com/acme/*\"".to_string()
            ))
        );
        assert_eq!(
            choose_configured(&cfg, Some("https://example.com")),
            Some(("Leo".to_string(), "config profiles.default".to_string()))
        );
        assert_eq!(
            choose_configured(&cfg, None).map(|c| c.0),
            Some("Leo".to_string())
        );
        assert_eq!(
            choose_configured(&ProfilesConfig::default(), Some("https://x.com")),
            None
        );
        assert!(parse_profiles_config(&json!({"profile": "Default"})).is_none());
    }

    #[test]
    fn who_cookie_classification() {
        let n = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            classify_cookies("github.com", &n(&["_octo", "logged_in", "user_session"])),
            WhoState::SignedIn(n(&["user_session"]))
        );
        // Signed-out GitHub still carries `logged_in=no` and `_octo`.
        assert_eq!(
            classify_cookies("github.com", &n(&["_octo", "logged_in", "_gh_sess"])),
            WhoState::SessionCookies(n(&["_gh_sess"]))
        );
        assert_eq!(
            classify_cookies("example.com", &n(&["_ga", "__cf_bm", "tz"])),
            WhoState::None
        );
        assert_eq!(
            classify_cookies("dash.cloudflare.com", &n(&["vses2", "CF_Session"])),
            WhoState::SessionCookies(n(&["CF_Session"]))
        );
        assert!(valid_domain("github.com"));
        assert!(!valid_domain("x' OR 1=1"));
    }

    #[test]
    fn profile_line_format() {
        let r = rows();
        let mut v = r[1].to_json();
        v["label"] = json!(r[1].label());
        v["reason"] = json!("--browser");
        assert_eq!(
            profile_line(&v),
            "profile: Davian (Profile 14, davian@gmail.com) — --browser"
        );
    }

    // --- ChooseBrowser rule decisions --------------------------------------

    fn hit(dir: &str, email: Option<&str>) -> RuleHit {
        RuleHit {
            host: "claude.ai".to_string(),
            rule_id: Some("rule-7".to_string()),
            key: "gaia-x".to_string(),
            root: Some("/root".to_string()),
            dir: dir.to_string(),
            email: email.map(str::to_string),
            ambiguous: Vec::new(),
        }
    }

    fn at(rows: &[ProfileRow], dir: &str) -> usize {
        rows.iter()
            .position(|r| r.dir.as_deref() == Some(dir))
            .unwrap()
    }

    fn record(row: &ProfileRow) -> Value {
        let mut v = row.to_json();
        v["label"] = json!(row.label());
        v
    }

    fn refusal(d: RuleDecision) -> String {
        match d {
            RuleDecision::Refuse(msg) => msg,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_stale_rule_warning_names_the_rule_and_says_the_profile_is_gone() {
        let w = stale_rule_warning(
            "claude.ai",
            &crate::choosebrowser::ProfileChoice {
                key: "1234567890".to_string(),
                rule_id: Some("rule-7".to_string()),
            },
        );
        for want in [
            "ChooseBrowser rule (rule-7)",
            "claude.ai",
            "\"1234567890\"",
            "no longer exists on this machine",
            "ignored",
        ] {
            assert!(w.contains(want), "missing {want:?} in: {w}");
        }
    }

    #[test]
    fn a_rule_whose_profile_is_connected_binds_a_new_session_to_it() {
        let r = rows();
        let d = decide_rule(
            &r,
            Some(&hit("Profile 14", Some("davian@gmail.com"))),
            false,
            false,
            SessionBinding::New,
            "s",
        );
        assert_eq!(d, RuleDecision::Use(at(&r, "Profile 14")));
    }

    /// The reported bug: the rule's profile was not connected and the url
    /// quietly opened in another one. Now it refuses and names the fix.
    #[test]
    fn a_rule_whose_profile_is_not_connected_refuses_instead_of_substituting() {
        let r = rows();
        let msg = refusal(decide_rule(
            &r,
            Some(&hit("Profile 13", Some("dora@gmail.com"))),
            false,
            false,
            SessionBinding::New,
            "s",
        ));
        for want in [
            "rule-7",
            "claude.ai",
            "dora",
            "not connected",
            "chrome-use connect --browser dora",
            "ask the user",
            "--browser <profile>",
            "--no-choosebrowser",
        ] {
            assert!(msg.contains(want), "missing {want:?} in: {msg}");
        }
    }

    #[test]
    fn explicit_choices_and_no_choosebrowser_and_no_rule_leave_selection_alone() {
        let r = rows();
        let h = hit("Profile 13", Some("dora@gmail.com"));
        for (no_cb, explicit) in [(true, false), (false, true), (true, true)] {
            assert_eq!(
                decide_rule(&r, Some(&h), no_cb, explicit, SessionBinding::New, "s"),
                RuleDecision::NotApplicable
            );
        }
        let leo = BoundProfile::from_row(&r[at(&r, "Default")]);
        assert_eq!(
            decide_rule(
                &r,
                Some(&h),
                true,
                false,
                SessionBinding::Bound(Some(&leo)),
                "s"
            ),
            RuleDecision::NotApplicable
        );
        assert_eq!(
            decide_rule(&r, None, false, false, SessionBinding::New, "s"),
            RuleDecision::NotApplicable
        );
    }

    #[test]
    fn a_session_bound_to_another_profile_refuses_the_rules_site() {
        let r = rows();
        let leo = BoundProfile::from_row(&r[at(&r, "Default")]);
        let msg = refusal(decide_rule(
            &r,
            Some(&hit("Profile 14", Some("davian@gmail.com"))),
            false,
            false,
            SessionBinding::Bound(Some(&leo)),
            "work",
        ));
        for want in [
            "Davian",
            "session \"work\" is bound to Leo",
            "--session <new-name>",
            "--no-choosebrowser to open it in Leo",
        ] {
            assert!(msg.contains(want), "missing {want:?} in: {msg}");
        }
        // Davian is connected, so there is nothing to connect first.
        assert!(!msg.contains("chrome-use connect"), "{msg}");
    }

    #[test]
    fn a_bound_session_on_an_unconnected_rule_profile_also_says_to_connect_it() {
        let r = rows();
        let leo = BoundProfile::from_record(&record(&r[at(&r, "Default")]));
        let msg = refusal(decide_rule(
            &r,
            Some(&hit("Profile 13", None)),
            false,
            false,
            SessionBinding::Bound(Some(&leo)),
            "s",
        ));
        assert!(msg.contains("chrome-use connect --browser dora"), "{msg}");
        assert!(msg.contains("--session"), "{msg}");
    }

    #[test]
    fn a_session_already_on_the_rules_profile_proceeds() {
        let r = rows();
        let h = hit("Profile 14", None);
        let live = BoundProfile::from_row(&r[at(&r, "Profile 14")]);
        assert_eq!(
            decide_rule(
                &r,
                Some(&h),
                false,
                false,
                SessionBinding::Bound(Some(&live)),
                "s"
            ),
            RuleDecision::AlreadyThere
        );
        // The session record carries the data root, so it identifies the
        // profile even without a relay id.
        let rec = BoundProfile::from_record(&record(&r[at(&r, "Profile 14")]));
        assert_eq!(rec.root.as_deref(), Some("/root"));
        let by_dir = BoundProfile {
            relay_id: None,
            ..rec
        };
        assert_eq!(
            decide_rule(
                &r,
                Some(&h),
                false,
                false,
                SessionBinding::Bound(Some(&by_dir)),
                "s"
            ),
            RuleDecision::AlreadyThere
        );
    }

    /// Too little identity to tell is a refusal, never a guess: a pre-root
    /// record (directory only), or no record at all on a relay session.
    #[test]
    fn a_session_whose_profile_cannot_be_identified_refuses() {
        let r = rows();
        let h = hit("Profile 14", None);
        let old = BoundProfile::from_record(&json!({"dir": "Profile 14", "label": "Davian"}));
        // Davian's row has a relay id but the old record has none, and the
        // record has no root to compare by directory.
        let msg = refusal(decide_rule(
            &r,
            Some(&h),
            false,
            false,
            SessionBinding::Bound(Some(&old)),
            "s",
        ));
        assert!(msg.contains("cannot tell which profile"), "{msg}");
        let msg = refusal(decide_rule(
            &r,
            Some(&h),
            false,
            false,
            SessionBinding::Bound(None),
            "s",
        ));
        assert!(msg.contains("cannot tell which profile"), "{msg}");
    }

    /// The review's example: a session bound to Chrome Beta's `Default` must
    /// not count as being on Stable's `Default` just because the directory
    /// names agree.
    #[test]
    fn the_same_directory_under_another_data_root_is_another_profile() {
        let stable = ProfileRow {
            name: Some("Leo".into()),
            dir: Some("Default".into()),
            root: Some("/stable".into()),
            has_extension: true,
            ..Default::default()
        };
        let beta = ProfileRow {
            name: Some("Beta".into()),
            dir: Some("Default".into()),
            root: Some("/beta".into()),
            relay_id: Some("beta-id".into()),
            ws: Some("ws://beta".into()),
            has_extension: true,
            ..Default::default()
        };
        let rows = vec![stable, beta.clone()];
        let mut h = hit("Default", None);
        h.root = Some("/stable".into());
        assert_eq!(row_for_rule(&rows, &h), Some(0));
        for bound in [
            BoundProfile::from_row(&beta),
            BoundProfile::from_record(&record(&beta)),
        ] {
            let msg = refusal(decide_rule(
                &rows,
                Some(&h),
                false,
                false,
                SessionBinding::Bound(Some(&bound)),
                "s",
            ));
            assert!(msg.contains("is bound to Beta"), "{msg}");
        }
    }

    /// The rule names a directory under one data root; nothing else stands
    /// in for it — not an email match to another directory, not the same
    /// directory under another root.
    #[test]
    fn the_rules_profile_is_matched_by_directory_and_root_only() {
        let r = rows();
        assert_eq!(
            row_for_rule(&r, &hit("Profile 7", Some("wind@gmail.com"))),
            Some(at(&r, "Profile 7"))
        );
        assert_eq!(
            row_for_rule(&r, &hit("Profile 12", Some("wind@gmail.com"))),
            Some(at(&r, "Profile 12"))
        );
        // A unique email elsewhere does not substitute for a missing dir.
        let elsewhere = hit("Profile 99", Some("dora@gmail.com"));
        assert_eq!(row_for_rule(&r, &elsewhere), None);
        let msg = refusal(decide_rule(
            &r,
            Some(&elsewhere),
            false,
            false,
            SessionBinding::New,
            "s",
        ));
        assert!(msg.contains("cannot find"), "{msg}");
        let mut beta = hit("Profile 14", None);
        beta.root = Some("/beta-root".to_string());
        assert_eq!(row_for_rule(&r, &beta), None);
        let mut no_root = hit("Profile 14", None);
        no_root.root = None;
        assert_eq!(row_for_rule(&r, &no_root), None);
    }

    /// The whole chain — rules file → portable key → Local State → RuleHit →
    /// decision — with keys that fit one profile and keys that fit several.
    #[test]
    fn the_chain_from_portable_key_refuses_an_ambiguous_key() {
        use crate::choosebrowser::lookup_in;
        let rules = r#"{"version":2,"rules":[
            {"ruleId":"by-gaia","match":{"domain":"claude.ai"},
             "action":{"bundleIdentifier":"com.google.Chrome::profile::555"}},
            {"ruleId":"by-name","match":{"domain":"x.com"},
             "action":{"bundleIdentifier":"com.google.Chrome::profile::Twin"}},
            {"ruleId":"unique","match":{"domain":"github.com"},
             "action":{"bundleIdentifier":"com.google.Chrome::profile::777"}}]}"#;
        // One account signed in to two profiles (same gaia id), and two
        // profiles with the same display name.
        let ls = r#"{"profile":{"info_cache":{
            "Profile 1":{"gaia_id":"555","user_name":"a@x.com","name":"Twin"},
            "Profile 2":{"gaia_id":"555","user_name":"a@x.com","name":"Twin"},
            "Profile 3":{"gaia_id":"777","user_name":"b@x.com","name":"Solo"}}}}"#;
        let outcome =
            |url: &str| rule_outcome(lookup_in(Some(rules), Some(ls), url), url, Some("/root"));

        for url in ["https://claude.ai/new", "https://x.com/home"] {
            let RuleOutcome::Hit(h) = outcome(url) else {
                panic!("expected a hit for {url}");
            };
            assert_eq!(h.ambiguous, vec!["Profile 1", "Profile 2"], "{url}");
            let msg = refusal(decide_rule(
                &[],
                Some(&h),
                false,
                false,
                SessionBinding::New,
                "s",
            ));
            assert!(msg.contains("matches 2 profiles"), "{msg}");
            assert!(msg.contains("Profile 1, Profile 2"), "{msg}");
        }

        let RuleOutcome::Hit(h) = outcome("https://github.com/x") else {
            panic!("expected a hit");
        };
        assert!(h.ambiguous.is_empty());
        assert_eq!(h.dir, "Profile 3");
        assert_eq!(h.root.as_deref(), Some("/root"));
        assert_eq!(h.email.as_deref(), Some("b@x.com"));

        assert_eq!(outcome("https://example.org/"), RuleOutcome::None);
        let stale_rules = r#"{"version":2,"rules":[{"ruleId":"gone","match":{"domain":"a.io"},
             "action":{"bundleIdentifier":"com.google.Chrome::profile::999"}}]}"#;
        let url = "https://a.io/";
        assert!(matches!(
            rule_outcome(lookup_in(Some(stale_rules), Some(ls), url), url, Some("/root")),
            RuleOutcome::Stale(ref w) if w.contains("(gone)")
        ));
    }

    /// A known relay endpoint with no identity is not "off the relay": it is
    /// refused. Only an endpoint the relay never published is outside rules.
    #[test]
    fn a_known_relay_with_unknown_identity_is_refused_not_skipped() {
        let r = rows();
        let relays = vec!["ws://generic".to_string()];
        let h = hit("Profile 14", None);

        let state = bound_state(&r, Some("ws://generic"), None, &relays);
        assert_eq!(state, BoundState::UnknownRelay);
        let msg = refusal(decide_bound(&r, &h, state, "s"));
        assert!(msg.contains("cannot tell which profile"), "{msg}");
        let mut amb = hit("", None);
        amb.ambiguous = vec!["Profile 3".into(), "Profile 4".into()];
        let msg = refusal(decide_bound(&r, &amb, BoundState::UnknownRelay, "s"));
        assert!(msg.contains("matches 2 profiles"), "{msg}");

        // Not published by the relay (a --launch / --cdp browser), or no
        // browser yet: outside the rules.
        assert_eq!(
            bound_state(&r, Some("ws://launched"), None, &relays),
            BoundState::NotRelay
        );
        assert_eq!(bound_state(&r, None, None, &relays), BoundState::NotRelay);
        assert_eq!(
            decide_bound(&r, &h, BoundState::NotRelay, "s"),
            RuleDecision::NotApplicable
        );

        // A relay row for the endpoint identifies it; failing that, a record.
        let live = bound_state(&r, Some("ws://b"), None, &relays);
        assert_eq!(decide_bound(&r, &h, live, "s"), RuleDecision::AlreadyThere);
        let rec = record(&r[at(&r, "Default")]);
        let from_rec = bound_state(&r, Some("ws://generic"), Some(&rec), &relays);
        assert!(
            matches!(from_rec, BoundState::Known(ref b) if b.dir.as_deref() == Some("Default"))
        );
    }

    #[test]
    fn a_batch_whose_sites_need_different_profiles_is_refused() {
        let a = hit("Profile 14", None);
        let mut b = hit("Profile 13", None);
        b.host = "github.com".into();
        assert_eq!(conflicting_rules(&[]), None);
        assert_eq!(conflicting_rules(&[a.clone(), a.clone()]), None);
        let msg = conflicting_rules(&[a, b]).unwrap();
        assert!(
            msg.contains("claude.ai (rule rule-7) → Profile 14"),
            "{msg}"
        );
        assert!(
            msg.contains("github.com (rule rule-7) → Profile 13"),
            "{msg}"
        );
        assert!(msg.contains("--session"), "{msg}");
    }

    #[test]
    fn merge_warning_keeps_an_existing_warning() {
        assert_eq!(merge_warning(None, "a"), "a");
        assert_eq!(merge_warning(Some(""), "a"), "a");
        assert_eq!(merge_warning(Some("b"), "a"), "a\nb");
    }
}
