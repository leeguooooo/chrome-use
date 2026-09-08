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
fn rules_paths() -> Vec<PathBuf> {
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
    #[serde(default, rename = "createdAt")]
    created_at: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
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
            .then(a.created_at.cmp(&b.created_at))
    });

    let winner = candidates.first()?;
    let key = parse_chrome_profile_key(winner.action.bundle_identifier.as_deref()?)?;
    Some(ProfileChoice {
        key,
        rule_id: winner.rule_id.clone(),
    })
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
pub fn resolve_profile_directory(local_state_json: &str, key: &str) -> Option<String> {
    let state: serde_json::Value = serde_json::from_str(local_state_json).ok()?;
    let cache = state.get("profile")?.get("info_cache")?.as_object()?;
    let key = key.trim();

    for field in ["gaia_id", "user_name", "gaia_name", "name", "shortcut_name"] {
        for (dir, info) in cache {
            if info
                .get(field)
                .and_then(|v| v.as_str())
                .is_some_and(|v| v.trim().eq_ignore_ascii_case(key))
            {
                return Some(dir.clone());
            }
        }
    }
    // Last resort: the key may itself be a directory name, which the writer
    // falls back to when a profile has no identifying fields at all.
    cache
        .keys()
        .find(|dir| dir.eq_ignore_ascii_case(key))
        .cloned()
}

/// The whole lookup, against the real files. `None` for every ordinary reason:
/// ChooseBrowser is not installed, the format is newer than we understand, no
/// rule covers this url, or the profile it names is not on this machine.
pub fn profile_directory_for_url(url: &str) -> Option<(String, ProfileChoice)> {
    // First path that both exists and yields a decision. A file that parses to
    // "no rule covers this url" is a real answer, so keep looking only while
    // nothing has answered at all.
    let choice = rules_paths().into_iter().find_map(|p| {
        let body = std::fs::read_to_string(p).ok()?;
        choose_for_url(&body, url)
    })?;
    let local_state = dirs::home_dir()
        .map(|h| h.join("Library/Application Support/Google/Chrome/Local State"))
        .and_then(|p| std::fs::read_to_string(p).ok())?;
    // A key that resolves to nothing means launching with no profile argument.
    // Falling back to *some other* profile would open the link as the wrong
    // identity, which is worse than opening it as the default one.
    let dir = resolve_profile_directory(&local_state, &choice.key)?;
    Some((dir, choice))
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
            resolve_profile_directory(&s, "103695396640962395023").as_deref(),
            Some("Default")
        );
        assert_eq!(
            resolve_profile_directory(&s, "leo@gmail.com").as_deref(),
            Some("Default")
        );
        assert_eq!(
            resolve_profile_directory(&s, "Work").as_deref(),
            Some("Profile 14")
        );
        // A profile with no account at all falls back to its directory name.
        assert_eq!(
            resolve_profile_directory(&s, "Profile 7").as_deref(),
            Some("Profile 7")
        );
    }

    /// Emails and names arrive with whatever case the user typed.
    #[test]
    fn key_matching_ignores_case_and_surrounding_space() {
        let s = local_state();
        assert_eq!(
            resolve_profile_directory(&s, " work@example.com ").as_deref(),
            Some("Profile 14")
        );
        assert_eq!(
            resolve_profile_directory(&s, "LEO@GMAIL.COM").as_deref(),
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
}
