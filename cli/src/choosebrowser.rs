//! Read ChooseBrowser's site → profile rules so an `open` lands in the account
//! the user already decided that site belongs to.
//!
//! People with several Chrome profiles keep a mapping of "this site uses that
//! account" in their head, and pass `--browser` every time to tell us. Many of
//! them have already written that mapping down — in ChooseBrowser, a macOS
//! link router that stores it as readable JSON. Reading it means the mapping
//! does not have to be maintained twice.
//!
//! Three properties are load-bearing:
//!
//! * **Invisible when absent.** No file means no behaviour change and no
//!   message. Most users do not have ChooseBrowser, and they should never learn
//!   that this code exists.
//! * **Read only.** The rules file is written atomically with a backup by an
//!   app the user is actively using. A second writer would eventually destroy
//!   the rules they built; changing them belongs behind an interface agreed
//!   with that project, not in a file poke from here.
//! * **Never guess a profile.** A key that resolves to nothing degrades to
//!   launching with no profile argument. Opening a link in the *wrong* account
//!   is worse than opening it in the default one — it can post, purchase or
//!   send as the wrong identity.

use serde::Deserialize;
use std::cmp::Ordering;
use std::path::PathBuf;

/// The one format this code understands. A different number means the file was
/// written by a version whose shape we have not seen; guessing at it is how a
/// link ends up in the wrong account.
const SUPPORTED_VERSION: u32 = 2;

/// Where the rules can live, newest location first.
///
/// The App Group container is where both builds are converging, but the
/// currently shipped direct-sale DMG still writes its own sandboxed location
/// and the App Store build another. Reading only the new path would find
/// nothing for everyone using a released version today, so all three are
/// tried and the first that parses wins.
pub fn rules_paths() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    vec![
        // Shared App Group container — both builds, going forward.
        home.join("Library/Group Containers/6ZPXG4KVVS.com.choosebrowser/rules.json"),
        // Direct-sale build, currently shipped.
        home.join("Library/Application Support/ChooseBrowser/rules.json"),
        // App Store build, currently shipped (sandboxed).
        home.join(
            "Library/Containers/com.choosebrowser.app/Data/Library/Application Support/ChooseBrowser/rules.json",
        ),
    ]
}

#[derive(Debug, Deserialize)]
struct RulesFile {
    version: u32,
    #[serde(default)]
    rules: Vec<Rule>,
}

#[derive(Debug, Deserialize, Clone)]
struct Rule {
    #[serde(default)]
    priority: i64,
    /// Untyped because the real files carry a unix timestamp as a **number**
    /// while the written contract shows a string. Declaring it as either one
    /// made serde reject the whole document, and a document that fails to parse
    /// is indistinguishable from "no rules" — the feature stayed silent with
    /// every rule intact on disk.
    ///
    /// Only ever compared for ordering, so the concrete type does not matter as
    /// long as comparison is stable.
    #[serde(default, rename = "createdAt")]
    created_at: Option<serde_json::Value>,
    /// `ruleId` in the file. The provenance line names it so the user can find
    /// the rule that redirected them; without the rename it silently stayed
    /// empty and the message said "a rule" with no way to look it up.
    #[serde(default, rename = "ruleId")]
    rule_id: Option<String>,
    #[serde(default)]
    r#match: Match,
    action: Action,
}

/// Keys that appear must all match; keys that are absent are wildcards. An
/// empty `match` therefore matches every url, which is how a catch-all rule is
/// expressed.
#[derive(Debug, Deserialize, Default, Clone)]
struct Match {
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    scheme: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct Action {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default, rename = "bundleIdentifier")]
    bundle_identifier: Option<String>,
}

/// A profile the rule named, resolved against this machine.
///
/// Carries the email as well as the directory because they answer different
/// questions and only one of them works for each. The relay identifies a
/// connected profile by its own id or by email — a directory name is not a
/// dimension it knows — so selecting with the directory silently matched
/// nothing and the whole feature never fired. The directory is still what a
/// person recognises, so it is what the provenance line shows.
#[derive(Debug, PartialEq, Clone)]
pub struct ResolvedProfile {
    /// `Default`, `Profile 14` — this machine's on-disk name, for display.
    pub directory: String,
    /// The signed-in account, when there is one. This is what the relay can
    /// actually match on.
    pub email: Option<String>,
}

/// What a matched rule asks for: a Chrome profile identified by a key that
/// survives being moved between machines.
#[derive(Debug, PartialEq, Clone)]
pub struct ProfileChoice {
    /// The portable key from the rule — a gaia id, an email, a display name.
    /// Deliberately not a directory name; see [`resolve_profile_directory`].
    pub key: String,
    /// Which rule produced it, for the provenance line the user sees.
    pub rule_id: Option<String>,
}

/// `com.google.Chrome::profile::<key>` — anything else (a different browser, or
/// Chrome with no profile) is not a Chrome profile choice.
fn parse_chrome_profile_key(bundle_identifier: &str) -> Option<String> {
    let (bundle, key) = bundle_identifier.split_once("::profile::")?;
    if !bundle.eq_ignore_ascii_case("com.google.Chrome") {
        return None;
    }
    let key = key.trim();
    (!key.is_empty()).then(|| key.to_string())
}

/// `*.example.com` matches `example.com` and any subdomain; anything else is an
/// exact, case-insensitive host match.
fn domain_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.trim().to_ascii_lowercase();
    let host = host.trim().to_ascii_lowercase();
    match pattern.strip_prefix("*.") {
        Some(suffix) => host == suffix || host.ends_with(&format!(".{suffix}")),
        None => host == pattern,
    }
}

/// `/my-org*` matches `/my-org` itself as well as anything under it. Without
/// the star the whole path must be equal.
fn path_matches(pattern: &str, path: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => path.starts_with(prefix),
        None => path == pattern,
    }
}

impl Match {
    fn matches(&self, url: &url::Url) -> bool {
        if let Some(scheme) = &self.scheme {
            if !url.scheme().eq_ignore_ascii_case(scheme.trim()) {
                return false;
            }
        }
        if let Some(domain) = &self.domain {
            match url.host_str() {
                Some(host) if domain_matches(domain, host) => {}
                _ => return false,
            }
        }
        if let Some(path) = &self.path {
            if !path_matches(path, url.path()) {
                return false;
            }
        }
        true
    }

    /// How specific this rule is, used to break ties at equal priority: a rule
    /// naming more things should win over a broader one that happens to have
    /// been created first.
    fn specificity(&self) -> usize {
        [
            self.domain.as_ref(),
            self.path.as_ref(),
            self.scheme.as_ref(),
        ]
        .iter()
        .filter(|v| v.is_some())
        .count()
    }
}

/// Pick the rule that governs `url`: highest priority, then most specific, then
/// oldest. Returns `None` when nothing matches — which is not a failure, just
/// a url the user never wrote a rule for.
pub fn choose_for_url(rules_json: &str, url: &str) -> Option<ProfileChoice> {
    let parsed: RulesFile = serde_json::from_str(rules_json).ok()?;
    if parsed.version != SUPPORTED_VERSION {
        return None;
    }
    let url = url::Url::parse(url).ok()?;

    let mut candidates: Vec<&Rule> = parsed
        .rules
        .iter()
        .filter(|r| {
            // Only "always open in" expresses a routing decision; other action
            // types (ask, block) are not ours to act on.
            r.action
                .kind
                .as_deref()
                .is_none_or(|k| k.eq_ignore_ascii_case("always_open_in"))
                && r.r#match.matches(&url)
        })
        .collect();

    candidates.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then(b.r#match.specificity().cmp(&a.r#match.specificity()))
            .then(compare_created_at(&a.created_at, &b.created_at))
    });

    let winner = candidates.first()?;
    let key = parse_chrome_profile_key(winner.action.bundle_identifier.as_deref()?)?;
    Some(ProfileChoice {
        key,
        rule_id: winner.rule_id.clone(),
    })
}

/// Order two `createdAt` values without caring whether they are numbers or
/// strings. Numbers compare numerically, everything else by its text; a missing
/// value sorts last so a rule that records its age wins the tie over one that
/// does not.
fn compare_created_at(a: &Option<serde_json::Value>, b: &Option<serde_json::Value>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
            _ => x.to_string().cmp(&y.to_string()),
        },
    }
}

/// Turn a portable profile key into the `--profile-directory` value for *this*
/// machine.
///
/// The key is deliberately not a directory name: Chrome numbers profiles in
/// creation order, so the same account is `Profile 3` on one machine and
/// `Profile 11` on another. Chrome's own `Local State` carries the mapping.
///
/// Compared case-insensitively against gaia id, then email, then display name,
/// then the directory name itself — the same order the writer used when
/// choosing what to store.
pub fn resolve_profile_directory(local_state_json: &str, key: &str) -> Option<ResolvedProfile> {
    let state: serde_json::Value = serde_json::from_str(local_state_json).ok()?;
    let cache = state.get("profile")?.get("info_cache")?.as_object()?;
    let key = key.trim();
    let email_of = |info: &serde_json::Value| {
        info.get("user_name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };

    for field in ["gaia_id", "user_name", "gaia_name", "name", "shortcut_name"] {
        for (dir, info) in cache {
            if info
                .get(field)
                .and_then(|v| v.as_str())
                .is_some_and(|v| v.trim().eq_ignore_ascii_case(key))
            {
                return Some(ResolvedProfile {
                    directory: dir.clone(),
                    email: email_of(info),
                });
            }
        }
    }
    // Last resort: the key may itself be a directory name, which the writer
    // falls back to when a profile has no identifying fields at all.
    cache
        .iter()
        .find(|(dir, _)| dir.eq_ignore_ascii_case(key))
        .map(|(dir, info)| ResolvedProfile {
            directory: dir.clone(),
            email: email_of(info),
        })
}

/// What the lookup found, step by step, for `doctor` to print.
///
/// The feature degrades silently by design — a missing file must not produce a
/// message for the many people who do not use ChooseBrowser. That is right for
/// them and blinding for everyone else: "not installed", "wrong path", "format
/// not understood", "profile not connected" and "no rule matches" all present
/// as the same thing, which is nothing at all. Three separate ways of never
/// working shipped behind that sameness before a person ran six urls by hand
/// and reasoned backwards.
///
/// So the silent path gets one loud counterpart. Nothing here changes
/// behaviour; it only says out loud what the quiet path decided.
#[derive(Debug)]
pub struct Diagnosis {
    /// Every location probed, and whether a file was there. Reported even on
    /// success: "found it, but in the path you thought was retired" is its own
    /// failure mode.
    pub probed: Vec<(PathBuf, bool)>,
    /// The file that answered, if any.
    pub source: Option<PathBuf>,
    /// Rules parsed out of it. `Some(0)` and `None` are different: zero rules
    /// is an empty file, `None` is one we could not read.
    pub parsed: Option<usize>,
    /// Version found, when the file parsed far enough to carry one.
    pub version: Option<u32>,
}

/// Probe the rules files without consulting any url.
pub fn diagnose() -> Diagnosis {
    let mut d = Diagnosis {
        probed: Vec::new(),
        source: None,
        parsed: None,
        version: None,
    };
    for path in rules_paths() {
        let body = std::fs::read_to_string(&path).ok();
        d.probed.push((path.clone(), body.is_some()));
        let Some(body) = body else { continue };
        if d.source.is_some() {
            continue;
        }
        d.source = Some(path);
        // Read the version even when the rest fails to deserialize: "version 3"
        // and "malformed" are different problems with different fixes.
        if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&body) {
            d.version = raw
                .get("version")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32);
        }
        d.parsed = serde_json::from_str::<RulesFile>(&body)
            .ok()
            .filter(|f| f.version == SUPPORTED_VERSION)
            .map(|f| f.rules.len());
    }
    d
}

/// The whole lookup, against the real files. `None` for every ordinary reason:
/// ChooseBrowser is not installed, the format is newer than we understand, no
/// rule covers this url, or the profile it names is not on this machine.
pub fn profile_for_url(url: &str) -> Option<(ResolvedProfile, ProfileChoice)> {
    // First path that both exists and yields a decision. A file that parses to
    // "no rule covers this url" is a real answer, so keep looking only while
    // nothing has answered at all.
    let choice = rules_paths().into_iter().find_map(|p| {
        let body = std::fs::read_to_string(p).ok()?;
        choose_for_url(&body, url)
    })?;
    let local_state = read_local_state()?;
    // A key that resolves to nothing means launching with no profile argument.
    // Falling back to *some other* profile would open the link as the wrong
    // identity, which is worse than opening it as the default one.
    let profile = resolve_profile_directory(&local_state, &choice.key)?;
    Some((profile, choice))
}

/// Chrome's profile registry, as text. `None` when Chrome has never run here.
pub fn read_local_state() -> Option<String> {
    dirs::home_dir()
        .map(|h| h.join("Library/Application Support/Google/Chrome/Local State"))
        .and_then(|p| std::fs::read_to_string(p).ok())
}

// --- Writing a rule back (`--remember`) --------------------------------------
//
// The reverse of the read path, and deliberately not symmetric with it. Reading
// is an inference we make on the user's behalf; writing changes what every
// browser launch on this machine does from now on, so it happens only when the
// user says so, and ChooseBrowser itself — not us — takes the final consent.
//
// The hard requirement from ChooseBrowser's owner: **every save shows a dialog,
// and there is deliberately no success callback.** So this module can build and
// hand over a request and nothing more. It must never report a rule as saved,
// because it cannot know. A malformed request is dropped without a dialog, which
// means an unnoticed typo would look exactly like a user declining — hence the
// validation below happens before anything is sent, and refuses loudly.

/// The portable key to write into a rule for the account `email` is signed into.
///
/// Prefers the gaia id because that is what ChooseBrowser's own UI writes (the
/// rules on this machine carry 21-digit gaia ids, not emails) and because it
/// survives the account being renamed. Falls back to the email, which the
/// reader also accepts.
///
/// `None` when the account is not in Chrome's registry at all — better to
/// refuse than to write a key that resolves to nothing or, worse, to somebody
/// else.
///
/// One account can own several profile directories (this machine has three for
/// the same address). That is fine here precisely because the key is a gaia id:
/// all three carry the same one, so which entry is found first does not change
/// what gets written.
pub fn portable_key_for_email(local_state_json: &str, email: &str) -> Option<String> {
    let state: serde_json::Value = serde_json::from_str(local_state_json).ok()?;
    let cache = state.get("profile")?.get("info_cache")?.as_object()?;
    let email = email.trim();
    if email.is_empty() {
        return None;
    }
    let entry = cache.values().find(|info| {
        info.get("user_name")
            .and_then(|v| v.as_str())
            .is_some_and(|v| v.trim().eq_ignore_ascii_case(email))
    })?;
    let field = |name: &str| {
        entry
            .get(name)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    field("gaia_id").or_else(|| field("user_name"))
}

/// Percent-encode one query value.
///
/// Hand-rolled rather than `Url::query_pairs_mut`, which is form-urlencoded and
/// would turn `:` into `%3A` — but the target's `com.google.Chrome::profile::…`
/// shape requires literal colons, and a target ChooseBrowser cannot parse is
/// dropped silently. Everything that would end or split a query is escaped;
/// characters that are legal inside one are left as they are.
fn encode_query_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for b in v.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b':'
            | b'/'
            | b'*'
            | b'@' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Build the `choosebrowser://remember` request, or `None` when the inputs
/// could not produce one ChooseBrowser would accept.
///
/// Validating here rather than letting the app reject it matters because
/// rejection is invisible: a malformed request shows no dialog, which looks
/// identical to the user having dismissed one.
pub fn remember_url(host: &str, path: Option<&str>, key: &str) -> Option<String> {
    let host = host.trim();
    let key = key.trim();
    // A bare hostname, per the scheme's contract — no scheme, port, or path
    // smuggled in, and no wildcard: the app writes the rule verbatim.
    if host.is_empty()
        || key.is_empty()
        || host.contains(['/', ':', '?', '#', '@', ' '])
        || !host.contains('.')
    {
        return None;
    }
    let mut url = format!(
        "choosebrowser://remember?domain={}",
        encode_query_value(host)
    );
    if let Some(path) = path {
        // The contract requires a leading slash; anything else is malformed and
        // would be dropped without a word.
        if !path.starts_with('/') {
            return None;
        }
        url.push_str(&format!("&path={}", encode_query_value(path)));
    }
    url.push_str(&format!(
        "&target={}",
        encode_query_value(&format!("com.google.Chrome::profile::{key}"))
    ));
    Some(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(body: &str) -> String {
        format!(r#"{{"version":2,"rules":[{body}]}}"#)
    }

    /// A version we have not seen means a shape we have not seen. Guessing at
    /// it is how a link lands in the wrong account, so an unknown version is
    /// simply "no rules" rather than a best effort.
    #[test]
    fn an_unknown_version_yields_nothing() {
        let f = r#"{"version":3,"rules":[{"match":{"domain":"a.example"},
            "action":{"bundleIdentifier":"com.google.Chrome::profile::x"}}]}"#;
        assert_eq!(choose_for_url(f, "https://a.example/"), None);
    }

    /// Malformed or truncated JSON is the same story: no rules, no message,
    /// no behaviour change.
    #[test]
    fn unreadable_json_yields_nothing() {
        for body in ["", "{", "null", "[]", r#"{"rules":[]}"#] {
            assert_eq!(choose_for_url(body, "https://a.example/"), None, "{body:?}");
        }
    }

    /// Keys present in `match` must all hold; keys absent are wildcards.
    #[test]
    fn every_key_present_in_match_must_hold() {
        let f = rules(
            r#"{"match":{"domain":"a.example","path":"/x"},
                "action":{"bundleIdentifier":"com.google.Chrome::profile::k"}}"#,
        );
        assert!(choose_for_url(&f, "https://a.example/x").is_some());
        assert_eq!(
            choose_for_url(&f, "https://a.example/y"),
            None,
            "path must hold"
        );
        assert_eq!(
            choose_for_url(&f, "https://b.example/x"),
            None,
            "domain must hold"
        );
    }

    /// An empty `match` is every key wildcarded, which is how a catch-all is
    /// written.
    #[test]
    fn an_empty_match_is_a_catch_all() {
        let f =
            rules(r#"{"match":{},"action":{"bundleIdentifier":"com.google.Chrome::profile::k"}}"#);
        assert!(choose_for_url(&f, "https://anything.example/deep/path").is_some());
    }

    /// `*.example.com` covers the bare domain as well as subdomains — a user
    /// writing it means "this site", not "only its subdomains".
    #[test]
    fn a_leading_star_domain_covers_the_bare_domain_too() {
        assert!(domain_matches("*.example.com", "example.com"));
        assert!(domain_matches("*.example.com", "mail.example.com"));
        assert!(domain_matches("*.example.com", "a.b.example.com"));
        assert!(!domain_matches("*.example.com", "notexample.com"));
        assert!(!domain_matches("*.example.com", "example.com.evil.test"));
    }

    /// `/my-org*` matches `/my-org` itself, not only paths beneath it.
    #[test]
    fn a_trailing_star_path_covers_the_prefix_itself() {
        assert!(path_matches("/my-org*", "/my-org"));
        assert!(path_matches("/my-org*", "/my-org/repo"));
        assert!(!path_matches("/my-org*", "/other"));
        assert!(path_matches("/exact", "/exact"));
        assert!(!path_matches("/exact", "/exact/more"));
    }

    /// Priority first, then specificity, then age. Specificity matters because
    /// a rule naming domain *and* path is a more deliberate statement than a
    /// domain-only rule that happens to be older.
    #[test]
    fn the_winner_is_priority_then_specificity_then_age() {
        let f = rules(
            r#"{"priority":10,"createdAt":"2020-01-01","match":{"domain":"a.example"},
                "action":{"bundleIdentifier":"com.google.Chrome::profile::low"}},
               {"priority":100,"createdAt":"2024-01-01","match":{"domain":"a.example"},
                "action":{"bundleIdentifier":"com.google.Chrome::profile::high"}}"#,
        );
        assert_eq!(
            choose_for_url(&f, "https://a.example/").unwrap().key,
            "high"
        );

        let f = rules(
            r#"{"priority":50,"createdAt":"2020-01-01","match":{"domain":"a.example"},
                "action":{"bundleIdentifier":"com.google.Chrome::profile::broad"}},
               {"priority":50,"createdAt":"2024-01-01","match":{"domain":"a.example","path":"/x*"},
                "action":{"bundleIdentifier":"com.google.Chrome::profile::narrow"}}"#,
        );
        assert_eq!(
            choose_for_url(&f, "https://a.example/x").unwrap().key,
            "narrow"
        );

        let f = rules(
            r#"{"priority":50,"createdAt":"2024-01-01","match":{"domain":"a.example"},
                "action":{"bundleIdentifier":"com.google.Chrome::profile::newer"}},
               {"priority":50,"createdAt":"2020-01-01","match":{"domain":"a.example"},
                "action":{"bundleIdentifier":"com.google.Chrome::profile::older"}}"#,
        );
        assert_eq!(
            choose_for_url(&f, "https://a.example/").unwrap().key,
            "older"
        );
    }

    /// The provenance line names the rule so the user can find and edit it.
    /// The field is `ruleId` in the file; without the rename it deserialized to
    /// `None` and the message pointed at nothing.
    #[test]
    fn the_matched_rule_id_comes_back_for_the_provenance_line() {
        let f = rules(
            r#"{"ruleId":"github.com|/my-org*","match":{"domain":"github.com"},
                "action":{"bundleIdentifier":"com.google.Chrome::profile::k"}}"#,
        );
        let got = choose_for_url(&f, "https://github.com/my-org/repo").unwrap();
        assert_eq!(got.rule_id.as_deref(), Some("github.com|/my-org*"));
    }

    /// A rule pointing at another browser, or at Chrome without a profile, is
    /// not a Chrome profile choice and must not be forced into one.
    #[test]
    fn only_a_chrome_profile_bundle_id_is_a_profile_choice() {
        assert_eq!(
            parse_chrome_profile_key("com.google.Chrome::profile::abc").as_deref(),
            Some("abc")
        );
        assert_eq!(parse_chrome_profile_key("com.apple.Safari"), None);
        assert_eq!(parse_chrome_profile_key("com.google.Chrome"), None);
        assert_eq!(
            parse_chrome_profile_key("org.mozilla.firefox::profile::abc"),
            None
        );
        assert_eq!(
            parse_chrome_profile_key("com.google.Chrome::profile::   "),
            None
        );
    }

    fn local_state() -> String {
        r#"{"profile":{"info_cache":{
            "Default":     {"gaia_id":"103695396640962395023","user_name":"leo@gmail.com","name":"Leo"},
            "Profile 14":  {"gaia_id":"102211906995000000001","user_name":"Work@Example.COM","name":"Work"},
            "Profile 7":   {"name":"No Account"}
        }}}"#
            .to_string()
    }

    /// The stored key can be any of several identifying fields; each must
    /// resolve to the directory name this machine happens to use.
    #[test]
    fn a_portable_key_resolves_to_this_machines_directory() {
        let s = local_state();
        assert_eq!(
            resolve_profile_directory(&s, "103695396640962395023")
                .map(|p| p.directory)
                .as_deref(),
            Some("Default")
        );
        assert_eq!(
            resolve_profile_directory(&s, "leo@gmail.com")
                .map(|p| p.directory)
                .as_deref(),
            Some("Default")
        );
        assert_eq!(
            resolve_profile_directory(&s, "Work")
                .map(|p| p.directory)
                .as_deref(),
            Some("Profile 14")
        );
        // A profile with no account at all falls back to its directory name.
        assert_eq!(
            resolve_profile_directory(&s, "Profile 7")
                .map(|p| p.directory)
                .as_deref(),
            Some("Profile 7")
        );
    }

    /// Emails and names arrive with whatever case the user typed.
    #[test]
    fn key_matching_ignores_case_and_surrounding_space() {
        let s = local_state();
        assert_eq!(
            resolve_profile_directory(&s, " work@example.com ")
                .map(|p| p.directory)
                .as_deref(),
            Some("Profile 14")
        );
        assert_eq!(
            resolve_profile_directory(&s, "LEO@GMAIL.COM")
                .map(|p| p.directory)
                .as_deref(),
            Some("Default")
        );
    }

    /// The most important guarantee here: a key naming a profile this machine
    /// does not have resolves to nothing, so the caller launches with no
    /// profile argument. Substituting a different profile would open the link
    /// as the wrong identity — able to post, buy or send as someone else.
    #[test]
    fn an_unknown_key_resolves_to_nothing_rather_than_some_other_profile() {
        let s = local_state();
        assert_eq!(
            resolve_profile_directory(&s, "someone-else@example.com"),
            None
        );
        assert_eq!(resolve_profile_directory(&s, "999999999999"), None);
        assert_eq!(resolve_profile_directory(&s, ""), None);
    }

    /// The resolution must also carry something the **relay** can select on.
    ///
    /// This is the bug that made the whole feature dead on arrival: resolution
    /// returned only the on-disk directory name, and the relay identifies a
    /// connected profile by its own id or by the signed-in address — a
    /// directory is not a dimension it has. Every lookup therefore missed, and
    /// missed *silently*, because "that profile isn't running the extension" is
    /// a legitimate outcome. The rules parsed perfectly the entire time.
    ///
    /// Unit tests on either side stayed green because the break was between
    /// them, so this one asserts the property the caller actually needs.
    #[test]
    fn a_resolved_profile_carries_an_identity_the_relay_can_match() {
        let s = local_state();
        let signed_in = resolve_profile_directory(&s, "103695396640962395023").unwrap();
        assert_eq!(signed_in.directory, "Default");
        assert_eq!(
            signed_in.email.as_deref(),
            Some("leo@gmail.com"),
            "the relay selects by email; without it the caller has nothing to pass"
        );

        // Resolving by any of the other fields must carry the email too — the
        // key that matched says nothing about what the caller then needs.
        assert_eq!(
            resolve_profile_directory(&s, "Work")
                .unwrap()
                .email
                .as_deref(),
            Some("Work@Example.COM")
        );
    }

    /// A profile with no signed-in account has no email to offer. That is not a
    /// failure to report — the relay could not match it either — so the caller
    /// simply falls through to its existing behaviour.
    #[test]
    fn a_profile_with_no_account_resolves_without_an_email() {
        let resolved = resolve_profile_directory(&local_state(), "Profile 7").unwrap();
        assert_eq!(resolved.directory, "Profile 7");
        assert_eq!(resolved.email, None);
    }

    /// Verbatim from a real rules.json written by the shipped app.
    ///
    /// `createdAt` is a **number** here while the written contract shows a
    /// string. Declaring it as either concrete type made serde reject the whole
    /// document — and a document that fails to parse is indistinguishable from
    /// "no rules", so the feature stayed silent with every rule intact on disk.
    /// This fixture is the format as it actually ships, not as it is described.
    #[test]
    fn a_real_rules_file_from_the_shipped_app_parses() {
        let real = r#"{"version":2,"rules":[
          {"createdAt":1788593504,
           "action":{"type":"always_open_in",
                     "bundleIdentifier":"com.google.Chrome::profile::103695396640962395023"},
           "match":{"path":"/leeguooooo*","domain":"github.com"},
           "ruleId":"github.com|/leeguooooo*",
           "priority":100}]}"#;
        let got = choose_for_url(real, "https://github.com/leeguooooo/chrome-use")
            .expect("a rule the shipped app wrote must parse");
        assert_eq!(got.key, "103695396640962395023");
        assert_eq!(got.rule_id.as_deref(), Some("github.com|/leeguooooo*"));
    }

    /// Ordering must survive either representation, and must not throw away a
    /// rule just because its timestamp is typed differently.
    #[test]
    fn created_at_orders_across_numbers_and_strings() {
        use serde_json::json;
        let num = |n: i64| Some(json!(n));
        let text = |s: &str| Some(json!(s));
        assert_eq!(compare_created_at(&num(1), &num(2)), Ordering::Less);
        assert_eq!(compare_created_at(&num(2), &num(1)), Ordering::Greater);
        assert_eq!(
            compare_created_at(&text("2020"), &text("2024")),
            Ordering::Less
        );
        // A rule that records its age wins the tie over one that does not.
        assert_eq!(compare_created_at(&None, &num(1)), Ordering::Greater);
        assert_eq!(compare_created_at(&num(1), &None), Ordering::Less);
        assert_eq!(compare_created_at(&None, &None), Ordering::Equal);
    }

    /// `Some(0)` and `None` must stay distinguishable: an empty rules file and
    /// one we could not read need different messages, and collapsing them is
    /// exactly the sameness this diagnosis exists to break.
    #[test]
    fn zero_rules_and_unreadable_rules_are_different_answers() {
        assert_eq!(
            serde_json::from_str::<RulesFile>(r#"{"version":2,"rules":[]}"#)
                .ok()
                .map(|f| f.rules.len()),
            Some(0)
        );
        assert_eq!(
            serde_json::from_str::<RulesFile>("{not json")
                .ok()
                .map(|f| f.rules.len()),
            None
        );
    }

    /// A url no rule covers is the ordinary case, not an error.
    #[test]
    fn a_url_no_rule_covers_yields_nothing() {
        let f = rules(
            r#"{"match":{"domain":"a.example"},
                "action":{"bundleIdentifier":"com.google.Chrome::profile::k"}}"#,
        );
        assert_eq!(choose_for_url(&f, "https://elsewhere.example/"), None);
    }

    /// Action types other than "always open in" are decisions we are not being
    /// asked to make.
    #[test]
    fn only_always_open_in_routes() {
        let f = rules(
            r#"{"match":{"domain":"a.example"},
                "action":{"type":"ask","bundleIdentifier":"com.google.Chrome::profile::k"}}"#,
        );
        assert_eq!(choose_for_url(&f, "https://a.example/"), None);
    }
    // --- `--remember` write-back ---------------------------------------------

    /// The one test that would have caught all three of this feature's earlier
    /// "never worked" bugs, which were every time a mismatch between what one
    /// side writes and what the other side reads.
    ///
    /// So it does not check the url's shape against a spec. It feeds what we
    /// write straight back into the reader and requires it to land on the same
    /// profile — the two halves are only correct relative to each other.
    #[test]
    fn a_written_target_reads_back_as_the_same_profile() {
        let state = local_state();
        let key =
            portable_key_for_email(&state, "Work@Example.COM").expect("key for a known account");
        let url = remember_url("github.com", None, &key).expect("a valid request");

        // Pull the target back out the way ChooseBrowser would.
        let target = url
            .split("&target=")
            .nth(1)
            .expect("target parameter")
            .to_string();
        let read_key = parse_chrome_profile_key(&target).expect("a chrome profile target");
        assert_eq!(
            resolve_profile_directory(&state, &read_key).map(|p| p.directory),
            Some("Profile 14".to_string()),
            "the profile we wrote a rule for is not the one the reader finds"
        );
    }

    /// ChooseBrowser's own UI writes gaia ids (the four rules on a real machine
    /// all carry 21-digit ones), and a gaia id survives the account being
    /// renamed. The email is the fallback, not the preference.
    #[test]
    fn a_gaia_id_is_preferred_over_the_email() {
        assert_eq!(
            portable_key_for_email(&local_state(), "leo@gmail.com").as_deref(),
            Some("103695396640962395023")
        );
    }

    /// Matching an account is case-insensitive because Chrome stores whatever
    /// the user typed, but an account Chrome has never seen must produce
    /// nothing rather than a guess: a wrong key writes a rule pointing at
    /// somebody else's profile.
    #[test]
    fn an_unknown_account_yields_no_key() {
        let s = local_state();
        assert!(portable_key_for_email(&s, "work@example.com").is_some());
        assert_eq!(portable_key_for_email(&s, "nobody@example.com"), None);
        assert_eq!(portable_key_for_email(&s, "  "), None);
    }

    /// The colons in `com.google.Chrome::profile::…` are load-bearing, and a
    /// form-urlencoder would turn them into `%3A` — producing a request that is
    /// dropped with no dialog, which looks exactly like the user declining.
    #[test]
    fn the_targets_colons_survive_encoding() {
        let url = remember_url("github.com", Some("/my-org*"), "1036953966").unwrap();
        assert!(
            url.contains("target=com.google.Chrome::profile::1036953966"),
            "colons were escaped: {url}"
        );
        assert!(url.contains("path=/my-org*"), "path was escaped: {url}");
        assert!(url.starts_with("choosebrowser://remember?domain=github.com"));
    }

    /// A request ChooseBrowser cannot parse is discarded silently, so anything
    /// that would produce one has to be refused here, where we can still say
    /// why.
    #[test]
    fn a_request_the_app_would_drop_is_refused_here() {
        // A url, not a hostname.
        assert_eq!(remember_url("https://github.com/x", None, "k"), None);
        // Port and path smuggled into the host.
        assert_eq!(remember_url("github.com:443", None, "k"), None);
        assert_eq!(remember_url("github.com/x", None, "k"), None);
        // Not a hostname at all.
        assert_eq!(remember_url("localhost", None, "k"), None);
        assert_eq!(remember_url("", None, "k"), None);
        // The contract requires a leading slash on the path.
        assert_eq!(remember_url("github.com", Some("my-org"), "k"), None);
        // No key means no profile to point at.
        assert_eq!(remember_url("github.com", None, "  "), None);
    }

    /// Values that would end or split the query must be escaped even though
    /// the common ones are not.
    #[test]
    fn characters_that_would_break_the_query_are_escaped() {
        assert_eq!(encode_query_value("a b&c=d#e"), "a%20b%26c%3Dd%23e");
        assert_eq!(encode_query_value("a.b-c_d~e"), "a.b-c_d~e");
    }
}
