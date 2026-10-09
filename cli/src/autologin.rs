//! What to do at a login wall (#481): sign in from the vault without asking,
//! never, or ask.
//!
//! A login wall (a redirect to a sign-in page, #434, or a site adapter that
//! finds its site signed out, #479) is signed in with `auth login --bwu` only
//! when the user decided so. The decision is per host, stored in
//! `~/.chrome-use/autologin.json`:
//!
//! ```json
//! { "all": "always", "hosts": { "zentao.example.com": "always", "x.com": "never" } }
//! ```
//!
//! Per host, because agreeing to "sign in to this site from my vault whenever
//! it asks" is consent about one site and one vault login; `--all` exists for
//! someone who wants it everywhere. The order, most specific first:
//!
//! 1. `AGENT_BROWSER_AUTO_LOGIN`: `bwu` / `always` = always, `ask` = ask,
//!    anything else non-empty (`off`, `0`, `never`) = never. It overrides all
//!    stored decisions, for scripts and CI.
//! 2. The host's entry in `autologin.json`.
//! 3. `"all"` in `autologin.json`.
//! 4. `"auth": {"autoLogin": "bwu"}` in `~/.chrome-use/config.json` (the
//!    pre-#481 switch): always.
//! 5. Nothing decided: ask.
//!
//! Asking: a person at a terminal gets a prompt; an agent gets `loginWall.ask`
//! with a question to relay and one command per answer, and must not answer
//! it for the user.

use serde_json::{json, Value};
use std::io::IsTerminal;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    Always,
    Never,
    Ask,
}

/// An answer to the question at a login wall.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Sign in now; ask again next time.
    Once,
    /// Sign in now and from now on, without asking (stored).
    Always,
    /// Not now; ask again next time.
    NotNow,
    /// Do not sign in, and stop asking for this host (stored).
    Never,
}

/// What the caller of [`decide`] should do at this wall.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Sign in from the vault now. `source` says why (for the report).
    SignIn { source: String },
    /// Do not sign in; report the wall with its plain hint.
    Skip { source: String },
    /// Nobody could be asked here: report the wall with this `ask` object.
    Ask(Value),
}

pub fn store_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".chrome-use").join("autologin.json"))
}

pub fn load_store() -> Value {
    store_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| json!({}))
}

pub fn save_store(store: &Value) -> Result<PathBuf, String> {
    let path = store_path().ok_or("no home directory")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(store).unwrap_or_else(|_| "{}".into());
    std::fs::write(&tmp, body + "\n").map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

pub fn normalize_host(h: &str) -> String {
    let h = h.trim().trim_end_matches('.').to_ascii_lowercase();
    // Accept a URL too: `auth autologin always https://x.example.com/login`.
    url::Url::parse(&h)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or(h)
}

fn decision(v: Option<&Value>) -> Option<Policy> {
    match v.and_then(|v| v.as_str()) {
        Some("always") => Some(Policy::Always),
        Some("never") => Some(Policy::Never),
        _ => None,
    }
}

/// The policy for `host` and where it came from.
pub fn resolve(
    host: &str,
    env: Option<&str>,
    store: &Value,
    config: Option<&Value>,
) -> (Policy, String) {
    if let Some(v) = env.map(str::trim).filter(|v| !v.is_empty()) {
        let p = match v.to_ascii_lowercase().as_str() {
            "bwu" | "always" => Policy::Always,
            "ask" => Policy::Ask,
            _ => Policy::Never,
        };
        return (p, format!("AGENT_BROWSER_AUTO_LOGIN={v}"));
    }
    let host = normalize_host(host);
    if let Some(p) = decision(store.get("hosts").and_then(|h| h.get(&host))) {
        return (p, format!("autologin.json ({host})"));
    }
    if let Some(p) = decision(store.get("all")) {
        return (p, "autologin.json (all sites)".into());
    }
    if crate::bwu_login::auto_login_enabled(None, config) {
        return (Policy::Always, "config.json auth.autoLogin".into());
    }
    (Policy::Ask, "not decided".into())
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// The `ask` object for an agent: a question to relay to the user as is, and
/// one command per answer. `login_url` is the page `auth login --bwu` fills
/// (none when the tab is already on it); `rerun` says whether the original
/// command should be run again after signing in.
pub fn ask_payload(host: &str, session: &str, login_url: Option<&str>, rerun: bool) -> Value {
    let s = shell_quote(session);
    let once = match login_url {
        Some(u) => format!(
            "chrome-use --session {s} open {} && chrome-use --session {s} auth login --bwu",
            shell_quote(u)
        ),
        None => format!("chrome-use --session {s} auth login --bwu"),
    };
    let then = if rerun {
        "then run the command again"
    } else {
        "then continue"
    };
    json!({
        "question": format!(
            "{host} needs you to sign in. Should chrome-use sign in with your Bitwarden \
             login for {host}? (1) yes, this time  (2) yes, and always for {host} from now \
             on without asking  (3) no, and don't ask again for {host}"
        ),
        "options": [
            { "choice": "once", "label": "Sign in this time", "command": once, "then": then },
            { "choice": "always", "label": format!("Always sign in to {host} without asking"),
              "command": format!("chrome-use auth autologin always {}", shell_quote(host)), "then": then },
            { "choice": "never", "label": format!("Don't sign in, and don't ask again for {host}"),
              "command": format!("chrome-use auth autologin never {}", shell_quote(host)) },
        ],
        "instruction": "Ask the user this question and run the command for their answer. Do not \
                        choose for them, and never choose \"always\" on your own.",
    })
}

/// The `ask` object as lines for stderr and the error message. The last line
/// is the instruction, for agents that read errors through `tail -1`.
pub fn ask_text(host: &str, ask: &Value) -> String {
    let mut out = format!(
        "login wall: {host} needs a sign-in. Ask the user: \"{}\"",
        ask["question"].as_str().unwrap_or("")
    );
    for o in ask["options"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "\n  {:<7} {}{}",
            format!("{}:", o["choice"].as_str().unwrap_or("")),
            o["command"].as_str().unwrap_or(""),
            o["then"]
                .as_str()
                .map(|t| format!("  ({t})"))
                .unwrap_or_default()
        ));
    }
    out.push_str(
        "\nAsk the user which one; do not choose for them, and never choose \"always\" yourself.",
    );
    out
}

/// Parse a typed answer: 1/once/y, 2/always/a, 3/no/n (not now), 4/never.
pub fn parse_answer(s: &str) -> Option<Choice> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "y" | "yes" | "once" => Some(Choice::Once),
        "2" | "a" | "always" => Some(Choice::Always),
        "3" | "n" | "no" | "" => Some(Choice::NotNow),
        "4" | "never" => Some(Choice::Never),
        _ => None,
    }
}

/// Whether a person can be asked here: stdin and stderr are terminals and the
/// output is not `--json`.
pub fn can_prompt(json_mode: bool) -> bool {
    !json_mode && std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn prompt(host: &str) -> Choice {
    use std::io::Write;
    eprintln!(
        "{} {host} needs you to sign in. Sign in with your Bitwarden login for it?",
        crate::color::warning_indicator()
    );
    eprintln!("  1) yes, this time");
    eprintln!("  2) yes, and always for {host} from now on (no more asking)");
    eprintln!("  3) not now");
    eprintln!("  4) no, and don't ask again for {host}");
    for _ in 0..3 {
        eprint!("Choose [1-4, default 3]: ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
            return Choice::NotNow;
        }
        if let Some(c) = parse_answer(&line) {
            return c;
        }
    }
    Choice::NotNow
}

/// Store a decision for `host` (`None` = all sites); `None` decision forgets.
pub fn set_decision(store: &mut Value, host: Option<&str>, value: Option<&str>) {
    if !store.is_object() {
        *store = json!({});
    }
    match host {
        None => match value {
            Some(v) => store["all"] = json!(v),
            None => {
                if let Some(o) = store.as_object_mut() {
                    o.remove("all");
                }
            }
        },
        Some(h) => {
            let h = normalize_host(h);
            if !store.get("hosts").is_some_and(|v| v.is_object()) {
                store["hosts"] = json!({});
            }
            let hosts = store["hosts"].as_object_mut().unwrap();
            match value {
                Some(v) => {
                    hosts.insert(h, json!(v));
                }
                None => {
                    hosts.remove(&h);
                }
            }
        }
    }
}

/// Decide at a wall for `host`: from the stored policy, or by asking the
/// person at the terminal (and storing "always"/"never"), or by handing an
/// `ask` object back for an agent.
pub fn decide(
    host: &str,
    session: &str,
    login_url: Option<&str>,
    rerun: bool,
    json_mode: bool,
) -> Outcome {
    let store = load_store();
    let (policy, source) = resolve(
        host,
        std::env::var("AGENT_BROWSER_AUTO_LOGIN").ok().as_deref(),
        &store,
        crate::report::user_config().as_ref(),
    );
    match policy {
        Policy::Always => Outcome::SignIn { source },
        Policy::Never => Outcome::Skip { source },
        Policy::Ask if can_prompt(json_mode) => {
            let choice = prompt(host);
            let remember = |v: &str| {
                let mut s = load_store();
                set_decision(&mut s, Some(host), Some(v));
                match save_store(&s) {
                    Ok(p) => eprintln!("saved: {} = {v} in {}", normalize_host(host), p.display()),
                    Err(e) => eprintln!(
                        "{} couldn't save the choice: {e}",
                        crate::color::warning_indicator()
                    ),
                }
            };
            match choice {
                Choice::Once => Outcome::SignIn {
                    source: "asked: this time".into(),
                },
                Choice::Always => {
                    remember("always");
                    Outcome::SignIn {
                        source: "asked: always".into(),
                    }
                }
                Choice::NotNow => Outcome::Skip {
                    source: "asked: not now".into(),
                },
                Choice::Never => {
                    remember("never");
                    Outcome::Skip {
                        source: "asked: never".into(),
                    }
                }
            }
        }
        Policy::Ask => Outcome::Ask(ask_payload(host, session, login_url, rerun)),
    }
}

/// `auth autologin <always|never|off|status> [<host>|--all]`.
pub fn run_cli(args: &[String], json_mode: bool) -> i32 {
    let usage = "usage: chrome-use auth autologin always <host>|--all | never <host> | \
                 off <host>|--all | status";
    let sub = args.first().map(String::as_str).unwrap_or("status");
    let target = args.get(1).map(String::as_str);
    let all = target == Some("--all");
    let host = target.filter(|t| !t.starts_with("--")).map(normalize_host);
    let fail = |msg: &str| -> i32 {
        if json_mode {
            println!(
                "{}",
                json!({ "success": false, "data": null, "error": msg })
            );
        } else {
            eprintln!("{} {msg}", crate::color::error_indicator());
        }
        1
    };
    let mut store = load_store();
    match sub {
        "status" => {}
        "always" | "never" | "off" => {
            if sub == "never" && all {
                return fail("`never --all` is not supported; set AGENT_BROWSER_AUTO_LOGIN=off to never sign in");
            }
            if host.is_none() && !all {
                return fail(usage);
            }
            let value = (sub != "off").then_some(sub);
            set_decision(&mut store, if all { None } else { host.as_deref() }, value);
            if let Err(e) = save_store(&store) {
                return fail(&format!("couldn't save: {e}"));
            }
        }
        _ => return fail(usage),
    }
    let env = std::env::var("AGENT_BROWSER_AUTO_LOGIN")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let config_on =
        crate::bwu_login::auto_login_enabled(None, crate::report::user_config().as_ref());
    let path = store_path()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let mut notes: Vec<String> = Vec::new();
    if let Some(v) = &env {
        notes.push(format!(
            "AGENT_BROWSER_AUTO_LOGIN={v} is set and overrides every stored decision"
        ));
    }
    if config_on {
        notes.push(
            "\"auth\": {\"autoLogin\": \"bwu\"} in ~/.chrome-use/config.json signs in to every \
             site without a decision here; remove it there to be asked instead"
                .into(),
        );
    }
    let data = json!({
        "file": path,
        "all": store.get("all").cloned().unwrap_or(Value::Null),
        "hosts": store.get("hosts").cloned().unwrap_or_else(|| json!({})),
        "env": env,
        "configAutoLogin": config_on,
        "notes": notes,
    });
    if json_mode {
        println!(
            "{}",
            json!({ "success": true, "data": data, "error": null })
        );
        return 0;
    }
    match (sub, host.as_deref()) {
        ("always", Some(h)) => println!(
            "{} {h}: sign in from the vault at a login wall, without asking",
            crate::color::success_indicator()
        ),
        ("never", Some(h)) => println!(
            "{} {h}: never sign in and don't ask",
            crate::color::success_indicator()
        ),
        ("off", Some(h)) => println!(
            "{} {h}: decision removed; chrome-use will ask at its next login wall",
            crate::color::success_indicator()
        ),
        ("always", None) => println!(
            "{} all sites: sign in from the vault at a login wall, without asking",
            crate::color::success_indicator()
        ),
        ("off", None) => println!(
            "{} all sites: decision removed (per-site decisions kept)",
            crate::color::success_indicator()
        ),
        _ => {}
    }
    println!("auto-login decisions ({path}):");
    match store.get("all").and_then(|v| v.as_str()) {
        Some(v) => println!("  all sites: {v}"),
        None => println!("  all sites: ask"),
    }
    let hosts = store.get("hosts").and_then(|h| h.as_object());
    match hosts.filter(|h| !h.is_empty()) {
        Some(h) => {
            for (k, v) in h {
                println!("  {k}: {}", v.as_str().unwrap_or(""));
            }
        }
        None => println!("  (no per-site decisions)"),
    }
    for n in notes {
        println!("  note: {n}");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_order() {
        let empty = json!({});
        assert_eq!(resolve("a.com", None, &empty, None).0, Policy::Ask);
        let cfg = json!({"auth": {"autoLogin": "bwu"}});
        assert_eq!(resolve("a.com", None, &empty, Some(&cfg)).0, Policy::Always);
        let mut store = json!({});
        set_decision(&mut store, Some("A.com"), Some("never"));
        // A per-host decision beats the old config switch.
        assert_eq!(resolve("a.com", None, &store, Some(&cfg)).0, Policy::Never);
        assert_eq!(resolve("b.com", None, &store, Some(&cfg)).0, Policy::Always);
        set_decision(&mut store, None, Some("always"));
        assert_eq!(resolve("b.com", None, &store, None).0, Policy::Always);
        assert_eq!(resolve("a.com", None, &store, None).0, Policy::Never);
        // The env var beats everything.
        assert_eq!(
            resolve("a.com", Some("bwu"), &store, None).0,
            Policy::Always
        );
        assert_eq!(resolve("b.com", Some("off"), &store, None).0, Policy::Never);
        assert_eq!(resolve("b.com", Some("ask"), &store, None).0, Policy::Ask);
        assert_eq!(resolve("b.com", Some("  "), &empty, None).0, Policy::Ask);
        // Forget.
        set_decision(&mut store, Some("a.com"), None);
        set_decision(&mut store, None, None);
        assert_eq!(resolve("a.com", None, &store, None).0, Policy::Ask);
    }

    #[test]
    fn hosts_normalize() {
        assert_eq!(normalize_host("ZenTao.Example.com."), "zentao.example.com");
        assert_eq!(
            normalize_host("https://zentao.example.com/zentao/"),
            "zentao.example.com"
        );
    }

    #[test]
    fn ask_payload_gives_one_command_per_answer() {
        let a = ask_payload(
            "z.example.com",
            "zt 1",
            Some("https://z.example.com/login?x=1&y"),
            true,
        );
        let opts = a["options"].as_array().unwrap();
        assert_eq!(opts.len(), 3);
        assert_eq!(opts[0]["choice"], "once");
        assert_eq!(
            opts[0]["command"],
            "chrome-use --session 'zt 1' open 'https://z.example.com/login?x=1&y' && chrome-use --session 'zt 1' auth login --bwu"
        );
        assert_eq!(
            opts[1]["command"],
            "chrome-use auth autologin always z.example.com"
        );
        assert_eq!(
            opts[2]["command"],
            "chrome-use auth autologin never z.example.com"
        );
        assert_eq!(opts[0]["then"], "then run the command again");
        assert!(a["instruction"]
            .as_str()
            .unwrap()
            .contains("never choose \"always\""));
        let t = ask_text("z.example.com", &a);
        assert!(t.starts_with("login wall: z.example.com needs a sign-in. Ask the user"));
        assert!(t.lines().last().unwrap().contains("do not choose for them"));
        // Already on the sign-in page: just log in.
        let a = ask_payload("z.example.com", "default", None, false);
        assert_eq!(
            a["options"][0]["command"],
            "chrome-use --session default auth login --bwu"
        );
    }

    #[test]
    fn answers_parse() {
        assert_eq!(parse_answer("1\n"), Some(Choice::Once));
        assert_eq!(parse_answer("always"), Some(Choice::Always));
        assert_eq!(parse_answer(""), Some(Choice::NotNow));
        assert_eq!(parse_answer("4"), Some(Choice::Never));
        assert_eq!(parse_answer("x"), None);
    }
}
