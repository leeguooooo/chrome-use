//! `auth login --bwu`: log in to the current page with an account from the
//! user's Bitwarden vault, through bitwarden-use (`bwu`, 0.7.0+).
//!
//! Two processes. Outside `bwu run` this CLI picks the account (masked:
//! `bwu login --domain <url> --list`, then the item's `_autotype` steps), and
//! runs the same command line again under `bwu run`, which asks the user once
//! for the item and puts the values in the child's environment only. The
//! child ([`parse`] with `CU_BWU_CHILD` set) reads them, drops them from the
//! environment before a daemon can inherit them, and sends one secret
//! command (`auth_login_bwu`) whose response is scrubbed of every value.
//! Values are never arguments, output or part of the transcript.

use std::process::Command;

use serde_json::{json, Value};

use crate::color;
use crate::commands::ParseError;
use crate::flags::Flags;

const USAGE: &str = "chrome-use auth login --bwu [--item <id|name>] [--passkey] [--no-submit]";

/// Set on the child so it fills instead of running `bwu` again.
const CHILD: &str = "CU_BWU_CHILD";
const USER: &str = "CU_BWU_USER";
const PASS: &str = "CU_BWU_PW";
const OTP: &str = "CU_BWU_OTP";
/// The item's passkeys as JSON (bwu `#passkeys`), private keys included.
const PASSKEYS: &str = "CU_BWU_PK";
/// JSON list of custom field names; their values are in `CU_BWU_F<i>`.
const FIELDS: &str = "CU_BWU_FIELDS";
const STEPS: &str = "CU_BWU_STEPS";
const ORIGIN: &str = "CU_BWU_ORIGIN";
const ITEM: &str = "CU_BWU_ITEM";

struct Opts {
    item: Option<String>,
    no_submit: bool,
    passkey: bool,
}

fn opts(args: &[&str]) -> Result<Opts, ParseError> {
    let mut o = Opts {
        item: None,
        no_submit: false,
        passkey: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "--bwu" => {}
            "--no-submit" => o.no_submit = true,
            "--passkey" => o.passkey = true,
            "--item" => {
                o.item = Some(
                    args.get(i + 1)
                        .ok_or_else(|| ParseError::MissingArguments {
                            context: "auth login --bwu --item".to_string(),
                            usage: USAGE,
                        })?
                        .to_string(),
                );
                i += 1;
            }
            other => {
                return Err(ParseError::InvalidValue {
                    message: format!(
                        "auth login --bwu takes no credential name ('{other}'): the account \
                         comes from your Bitwarden vault for the current page"
                    ),
                    usage: USAGE,
                })
            }
        }
        i += 1;
    }
    if o.passkey && o.no_submit {
        return Err(ParseError::InvalidValue {
            message: "--passkey has nothing to fill, so --no-submit would do nothing".to_string(),
            usage: USAGE,
        });
    }
    Ok(o)
}

/// Parse `auth login --bwu …` (the args after `login`).
pub fn parse(args: &[&str], id: &str) -> Result<Value, ParseError> {
    let o = opts(args)?;
    if std::env::var_os(CHILD).is_none() {
        return Ok(json!({
            "id": id, "action": "auth_login_bwu_probe",
            "item": o.item, "noSubmit": o.no_submit, "passkey": o.passkey,
        }));
    }
    // Under `bwu run`: take the values and drop them from the environment
    // before a daemon this command starts can inherit them.
    let take = |k: &str| {
        let v = std::env::var(k).ok().filter(|v| !v.is_empty());
        std::env::remove_var(k);
        v
    };
    let names: Vec<String> = take(FIELDS)
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default();
    let mut fields = serde_json::Map::new();
    for (i, name) in names.iter().enumerate() {
        if let Some(v) = take(&format!("CU_BWU_F{i}")) {
            fields.insert(name.clone(), Value::String(v));
        }
    }
    let username = take(USER);
    let password = take(PASS);
    let otp = take(OTP);
    let passkeys: Vec<Value> = take(PASSKEYS)
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default();
    let steps: Option<Value> = take(STEPS).and_then(|j| serde_json::from_str(&j).ok());
    let origin = take(ORIGIN);
    let item = take(ITEM);
    std::env::remove_var(CHILD);
    let origin = origin.ok_or_else(|| ParseError::InvalidValue {
        message: format!("auth login --bwu: {ORIGIN} is not set; run it without {CHILD}"),
        usage: USAGE,
    })?;
    let secrets: Vec<String> = [&username, &password, &otp]
        .into_iter()
        .flatten()
        .cloned()
        .chain(
            fields
                .values()
                .filter_map(Value::as_str)
                .map(str::to_string),
        )
        .chain(
            passkeys
                .iter()
                .filter_map(|p| p["privateKey"].as_str())
                .map(str::to_string),
        )
        .collect();
    Ok(json!({
        "id": id, "action": "auth_login_bwu", "secret": true, "secrets": secrets,
        "username": username, "password": password, "otp": otp, "fields": fields,
        "steps": steps, "origin": origin, "item": item, "noSubmit": o.no_submit,
        "passkeys": passkeys, "passkeyFirst": o.passkey,
    }))
}

fn fail(flags: &Flags, msg: &str) -> i32 {
    if flags.json {
        println!(
            "{}",
            json!({ "success": false, "data": null, "error": msg })
        );
    } else {
        eprintln!("{} {}", color::error_indicator(), msg);
    }
    1
}

/// `bwu`, else `bitwarden-use`, from PATH.
fn find_bwu() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for name in ["bwu", "bitwarden-use"] {
        for dir in std::env::split_paths(&path) {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Run `bwu` and parse its stdout as JSON. Its stderr (unlock prompts,
/// update notices) goes to ours.
fn bwu_json(bwu: &std::path::Path, args: &[&str]) -> Result<Value, String> {
    let out = Command::new(bwu)
        .args(args)
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("couldn't run {}: {e}", bwu.display()))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        if stderr.contains("unexpected argument '--list'") {
            return Err(
                "auth login --bwu needs bitwarden-use 0.7.0 or newer: run `bwu upgrade`"
                    .to_string(),
            );
        }
        return Err(format!(
            "`bwu {}` failed: {}",
            args.join(" "),
            stderr.trim()
        ));
    }
    eprint!("{stderr}");
    serde_json::from_slice(&out.stdout).map_err(|e| {
        format!(
            "`bwu {}` printed something that is not JSON: {e}",
            args.join(" ")
        )
    })
}

/// Choose the vault item: `--item` (id or exact name), else the only match,
/// else the only one *named* exactly the page's host (#479: bwu matches by
/// registrable domain, so `zentao.example.com` also lists every other
/// `*.example.com` login; an item named `zentao.example.com` is the one meant
/// for it). Otherwise several matches are listed for the caller to pick from,
/// most recently used first; picking one silently could log in as the wrong
/// account.
fn choose(candidates: &[Value], item: Option<&str>, host: &str) -> Result<Value, String> {
    let line = |c: &Value| {
        format!(
            "  --item {}  {}{}  (used {}×{})",
            c["id"].as_str().unwrap_or(""),
            c["name"].as_str().unwrap_or(""),
            c["folder"]
                .as_str()
                .map(|f| format!(" [{f}]"))
                .unwrap_or_default(),
            c["uses"],
            c["last_used"]
                .as_str()
                .map(|t| format!(", last {t}"))
                .unwrap_or_default(),
        )
    };
    let picked: Vec<&Value> = match item {
        Some(want) => candidates
            .iter()
            .filter(|c| c["id"] == want || c["name"] == want)
            .collect(),
        None => {
            let named: Vec<&Value> = candidates
                .iter()
                .filter(|c| {
                    !host.is_empty()
                        && c["name"]
                            .as_str()
                            .is_some_and(|n| n.trim().eq_ignore_ascii_case(host))
                })
                .collect();
            if candidates.len() > 1 && named.len() == 1 {
                named
            } else {
                candidates.iter().collect()
            }
        }
    };
    match picked.as_slice() {
        [one] => Ok((*one).clone()),
        [] if candidates.is_empty() => Err(format!(
            "no login in your Bitwarden vault matches {host}. Save one with this site's URL, or \
             fill the form by hand."
        )),
        [] => Err(format!(
            "no login for {host} has the id or name '{}'. Logins for this site:\n{}",
            item.unwrap_or(""),
            candidates.iter().map(line).collect::<Vec<_>>().join("\n")
        )),
        many => Err(format!(
            "{} logins in your vault match {host}; pick one with --item (most recently used \
             first):\n{}",
            many.len(),
            many.iter().map(|c| line(c)).collect::<Vec<_>>().join("\n")
        )),
    }
}

/// The parent half: choose the account, then run this command line again
/// under `bwu run`. Returns the exit code.
pub fn run(flags: &Flags, cmd: &Value) -> i32 {
    match run_inner(flags, cmd) {
        Ok(code) => code,
        Err(e) => fail(flags, &e),
    }
}

/// The account chosen for the tab's page and how `bwu run` hands its values
/// to the child.
struct Prepared {
    bwu: std::path::PathBuf,
    envs: Vec<String>,
    origin: String,
    host: String,
    id: String,
    name: String,
    custom: Vec<String>,
    steps: Option<Vec<String>>,
}

/// Choose the vault item for the tab's current page (masked), and which of
/// its values the login needs.
fn prepare(
    flags: &Flags,
    item_arg: Option<&str>,
    passkey_first: bool,
    no_submit: bool,
) -> Result<Prepared, String> {
    let bwu = find_bwu().ok_or(
        "auth login --bwu needs bitwarden-use (bwu) on PATH. Install it with: curl -fsSL \
         https://raw.githubusercontent.com/leeguooooo/bitwarden-use/main/install.sh | sh",
    )?;
    let resp = crate::connection::send_command(
        json!({ "id": crate::commands::gen_id(), "action": "url" }),
        &flags.session,
    )?;
    let url = resp
        .data
        .as_ref()
        .and_then(|d| d["url"].as_str())
        .filter(|_| resp.success)
        .ok_or_else(|| {
            resp.error
                .clone()
                .unwrap_or_else(|| "couldn't read the tab's URL".into())
        })?
        .to_string();
    let parsed =
        url::Url::parse(&url).map_err(|_| format!("the tab is on {url:?}, not a website"))?;
    if !matches!(parsed.scheme(), "http" | "https") || !parsed.origin().is_tuple() {
        return Err(format!(
            "the tab is on {url}, not a login page; open the site's login page first"
        ));
    }
    let origin = parsed.origin().ascii_serialization();
    let host = parsed.host_str().unwrap_or("").to_string();

    let listed = bwu_json(&bwu, &["login", "--domain", &url, "--list"])?;
    let candidates = listed["candidates"].as_array().cloned().unwrap_or_default();
    let chosen = choose(&candidates, item_arg, &host)?;
    let id = chosen["id"].as_str().unwrap_or_default().to_string();
    let name = chosen["name"].as_str().unwrap_or_default().to_string();
    // Masked: which values the item has, and its `_autotype` steps.
    let item = bwu_json(&bwu, &["login", "--domain", &url, "--name", &id])?;
    let steps: Option<Vec<String>> = item["autotype"].as_array().map(|a| {
        a.iter()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect()
    });
    let has = |k: &str| item[k].is_string();
    let wants = |step: &str| steps.as_ref().is_none_or(|s| s.iter().any(|x| x == step));

    let passkey_count = item["passkeys"].as_u64().unwrap_or(0);
    // --no-submit fills and presses nothing: no passkey (a ceremony signs and
    // submits) and no one-time code (sites submit on the last digit).
    if passkey_first && passkey_count == 0 {
        return Err(format!("the vault item '{name}' has no passkey"));
    }
    let mut envs: Vec<String> = Vec::new();
    if passkey_count > 0 && !no_submit {
        envs.push(format!("{PASSKEYS}=bw:{id}#passkeys"));
    }
    // Signing in with the passkey needs nothing else from the vault.
    let wants = |step: &str| !passkey_first && wants(step);
    if has("username") && wants("username") {
        envs.push(format!("{USER}=bw:{id}#username"));
    }
    if has("password") && wants("password") {
        envs.push(format!("{PASS}=bw:{id}#password"));
    }
    if has("code") && wants("totp") && !no_submit {
        envs.push(format!("{OTP}=bw:{id}#totp"));
    }
    let custom: Vec<String> = steps
        .iter()
        .flatten()
        .filter_map(|s| s.strip_prefix("custom:").map(str::to_string))
        .filter(|_| !passkey_first)
        .collect();
    for (i, field) in custom.iter().enumerate() {
        envs.push(format!("CU_BWU_F{i}=bw:{id}#custom:{field}"));
    }
    if envs.is_empty() {
        return Err(format!(
            "the vault item '{name}' has no username or password to log in with"
        ));
    }
    Ok(Prepared {
        bwu,
        envs,
        origin,
        host,
        id,
        name,
        custom,
        steps,
    })
}

/// `bwu run --env … -- chrome-use <child_args>`: the child fills the page.
fn child_command(p: &Prepared, child_args: &[std::ffi::OsString]) -> Result<Command, String> {
    let me =
        std::env::current_exe().map_err(|e| format!("couldn't find chrome-use itself: {e}"))?;
    let mut run = Command::new(&p.bwu);
    run.arg("run");
    for e in &p.envs {
        run.arg("--env").arg(e);
    }
    run.arg("--").arg(me).args(child_args);
    run.env(CHILD, "1")
        .env(ORIGIN, &p.origin)
        .env(ITEM, &p.name)
        .env(FIELDS, serde_json::to_string(&p.custom).unwrap_or_default());
    match &p.steps {
        Some(s) => run.env(STEPS, serde_json::to_string(s).unwrap_or_default()),
        None => run.env_remove(STEPS),
    };
    Ok(run)
}

fn run_inner(flags: &Flags, cmd: &Value) -> Result<i32, String> {
    let p = prepare(
        flags,
        cmd["item"].as_str(),
        cmd["passkey"].as_bool().unwrap_or(false),
        cmd["noSubmit"].as_bool().unwrap_or(false),
    )?;
    if !flags.json {
        eprintln!(
            "logging in to {} as vault item '{}' (bwu asks for Touch ID unless require_touch_id is off)",
            p.host, p.name
        );
    }
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let status = child_command(&p, &args)?
        .status()
        .map_err(|e| format!("couldn't run {}: {e}", p.bwu.display()))?;
    Ok(status.code().unwrap_or(1))
}

/// Whether login walls are signed in automatically (#434):
/// `AGENT_BROWSER_AUTO_LOGIN=bwu`, or `"auth": {"autoLogin": "bwu"}` (or
/// `"auth.autoLogin": "bwu"`) in `~/.chrome-use/config.json`. A set env var
/// wins either way.
pub fn auto_login_enabled(env: Option<&str>, config: Option<&Value>) -> bool {
    if let Some(v) = env.map(str::trim).filter(|v| !v.is_empty()) {
        return v.eq_ignore_ascii_case("bwu");
    }
    config.is_some_and(|c| {
        c.get("auth")
            .and_then(|a| a.get("autoLogin"))
            .or_else(|| c.get("auth.autoLogin"))
            .and_then(Value::as_str)
            .is_some_and(|v| v.eq_ignore_ascii_case("bwu"))
    })
}

/// Run `auth login --bwu` under `bwu run`; the child's error when it fails.
fn run_login_child(p: &Prepared, args: &[std::ffi::OsString]) -> Result<(), String> {
    let output = child_command(p, args).and_then(|mut c| {
        c.stdout(std::process::Stdio::piped())
            .output()
            .map_err(|e| format!("couldn't run {}: {e}", p.bwu.display()))
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: Option<Value> = stdout
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str(l.trim()).ok());
    let ok = output.status.success()
        && envelope
            .as_ref()
            .is_some_and(|e| e["success"].as_bool() == Some(true));
    if ok {
        return Ok(());
    }
    Err(envelope
        .as_ref()
        .and_then(|e| e["error"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("auth login --bwu exited with {}", output.status)))
}

/// Whether an `auth login --bwu` failure is worth one more try from the
/// sign-in page (#482): a step the tab detached under (its outcome unknown),
/// or a submit the site was not seen to take while the page showed no message
/// of its own. A page that says why it refused (a wrong password) is not
/// retried: the same login would be refused again and count as another failed
/// attempt.
fn retry_after_unknown_outcome(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("action_outcome_unknown")
        || (e.contains("the sign-in was not confirmed") && !e.contains("the page says"))
}

/// Sign in to the login wall the tab is on with the only vault login for it
/// (several, or none, is reported, never guessed), then go back to
/// `returnTo`. Returns `{ok, item, error}` plus `returnedTo` / `returnError`.
pub fn auto_login(flags: &Flags, wall: &Value) -> Value {
    let mut out = json!({ "ok": false, "item": null, "error": null });
    let p = match prepare(flags, None, false, false) {
        Ok(p) => p,
        Err(e) => {
            out["error"] = json!(e);
            return out;
        }
    };
    out["item"] = json!(p.name);
    eprintln!(
        "login wall: signing in to {} as vault item '{}' (auto-login)",
        p.host, p.name
    );
    let args: Vec<std::ffi::OsString> = [
        "--session",
        flags.session.as_str(),
        "--json",
        "auth",
        "login",
        "--bwu",
        "--item",
        p.id.as_str(),
    ]
    .iter()
    .map(Into::into)
    .collect();
    // The sign-in page, to come back to when a submit's outcome is unknown.
    let login_page = crate::connection::send_command(
        json!({ "id": crate::commands::gen_id(), "action": "url" }),
        &flags.session,
    )
    .ok()
    .and_then(|r| r.data.and_then(|d| d["url"].as_str().map(str::to_string)));
    let mut attempt = 0;
    loop {
        attempt += 1;
        match run_login_child(&p, &args) {
            Ok(()) => break,
            // #482: a step the tab detached under, or a submit the site was
            // not seen to take. Logging in again from the sign-in page is
            // safe: on a page that is already signed in, `auth login --bwu`
            // reports alreadySignedIn (a loaded page with no sign-in form) and
            // types nothing.
            Err(e) if attempt == 1 && retry_after_unknown_outcome(&e) && login_page.is_some() => {
                eprintln!(
                    "login wall: the sign-in submit's outcome is unknown ({e}); trying once more"
                );
                let _ = crate::connection::send_command(
                    json!({ "id": crate::commands::gen_id(), "action": "navigate", "url": login_page }),
                    &flags.session,
                );
                out["retried"] = json!(1);
            }
            Err(e) => {
                out["error"] = json!(e);
                return out;
            }
        }
    }
    out["ok"] = json!(true);
    if let Some(back) = wall["returnTo"].as_str() {
        let resp = crate::connection::send_command(
            json!({ "id": crate::commands::gen_id(), "action": "navigate", "url": back }),
            &flags.session,
        );
        match resp {
            Ok(r) if r.success => {
                out["returnedTo"] = r
                    .data
                    .as_ref()
                    .and_then(|d| d.get("url"))
                    .cloned()
                    .unwrap_or_else(|| json!(back));
            }
            Ok(r) => out["returnError"] = json!(r.error),
            Err(e) => out["returnError"] = json!(e),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: &str, name: &str) -> Value {
        json!({ "id": id, "name": name, "folder": null, "uses": 0, "last_used": null })
    }

    #[test]
    fn choose_needs_one_account() {
        let one = [c("1", "github")];
        assert_eq!(choose(&one, None, "github.com").unwrap()["id"], "1");
        let two = [c("1", "work"), c("2", "personal")];
        let e = choose(&two, None, "github.com").unwrap_err();
        assert!(e.contains("--item 1") && e.contains("--item 2"), "{e}");
        assert_eq!(choose(&two, Some("2"), "github.com").unwrap()["id"], "2");
        assert_eq!(choose(&two, Some("work"), "github.com").unwrap()["id"], "1");
        assert!(choose(&two, Some("x"), "github.com").is_err());
        assert!(choose(&[], None, "github.com")
            .unwrap_err()
            .contains("no login"));
    }

    #[test]
    fn retries_unknown_outcomes_and_unconfirmed_sign_ins_only() {
        assert!(retry_after_unknown_outcome("CDP error (Input.dispatchKeyEvent): action_outcome_unknown: Input.dispatchKeyEvent was not replayed because it may already have executed. Original error: Detached while handling command."));
        assert!(!retry_after_unknown_outcome(
            "2 logins in your vault match x.com"
        ));
        assert!(retry_after_unknown_outcome(
            "action_outcome_unknown: Runtime.evaluate was not replayed"
        ));
        assert!(retry_after_unknown_outcome(
            "auth login --bwu: the sign-in was not confirmed: the sign-in form is still on the page (https://x/login) 12s after it was submitted, so the site did not sign in."
        ));
        assert!(!retry_after_unknown_outcome(
            "auth login --bwu: the sign-in was not confirmed: the sign-in form is still on the page (https://x/login) 12s after it was submitted, so the site did not sign in. The page says: \"wrong password\"."
        ));
    }

    #[test]
    fn choose_prefers_the_one_item_named_for_the_host() {
        // bwu lists every login of the registrable domain.
        let many = [
            c("1", "dev-web.example.com"),
            c("2", "zentao.example.com"),
            c("3", "example.com"),
        ];
        assert_eq!(
            choose(&many, None, "zentao.example.com").unwrap()["id"],
            "2"
        );
        assert_eq!(
            choose(&many, None, "ZENTAO.example.com").unwrap()["id"],
            "2"
        );
        // No item named for the host, or two of them: still ask.
        assert!(choose(&many, None, "jira.example.com").is_err());
        let twins = [c("1", "zentao.example.com"), c("2", "zentao.example.com")];
        assert!(choose(&twins, None, "zentao.example.com").is_err());
        // --item still wins.
        assert_eq!(
            choose(&many, Some("1"), "zentao.example.com").unwrap()["id"],
            "1"
        );
    }

    #[test]
    fn auto_login_switch() {
        assert!(!auto_login_enabled(None, None));
        assert!(auto_login_enabled(Some("bwu"), None));
        assert!(!auto_login_enabled(
            Some("off"),
            Some(&json!({"auth": {"autoLogin": "bwu"}}))
        ));
        assert!(auto_login_enabled(
            None,
            Some(&json!({"auth": {"autoLogin": "bwu"}}))
        ));
        assert!(auto_login_enabled(
            None,
            Some(&json!({"auth.autoLogin": "BWU"}))
        ));
        assert!(!auto_login_enabled(
            None,
            Some(&json!({"auth": {"autoLogin": false}}))
        ));
        assert!(!auto_login_enabled(
            None,
            Some(&json!({"report": {"auto": true}}))
        ));
    }

    // One test: both halves set and clear the same process-wide variables.
    #[test]
    fn parse_probe_then_child() {
        std::env::remove_var(CHILD);
        let cmd = parse(&["--bwu", "--item", "abc", "--no-submit"], "1").unwrap();
        assert_eq!(cmd["action"], "auth_login_bwu_probe");
        assert_eq!(cmd["item"], "abc");
        assert_eq!(cmd["noSubmit"], true);
        assert!(parse(&["--bwu", "github"], "1").is_err());
        assert!(parse(&["--bwu", "--passkey", "--no-submit"], "1").is_err());

        for (k, v) in [
            (CHILD, "1"),
            (USER, "alice"),
            (PASS, "hunter2"),
            (FIELDS, r#"["PIN"]"#),
            ("CU_BWU_F0", "1234"),
            (
                STEPS,
                r#"["username","enter","password","custom:PIN","enter"]"#,
            ),
            (ORIGIN, "https://example.com"),
            (ITEM, "example"),
            (
                PASSKEYS,
                r#"[{"credentialId":"AQID","rpId":"example.com","privateKey":"c2VjcmV0a2V5","userHandle":"dQ==","signCount":0,"isResidentCredential":true}]"#,
            ),
        ] {
            std::env::set_var(k, v);
        }
        let cmd = parse(&["--bwu"], "2").unwrap();
        assert_eq!(cmd["action"], "auth_login_bwu");
        assert_eq!(cmd["secret"], true);
        assert_eq!(cmd["password"], "hunter2");
        assert_eq!(cmd["fields"]["PIN"], "1234");
        assert_eq!(cmd["steps"][3], "custom:PIN");
        assert_eq!(cmd["origin"], "https://example.com");
        let secrets: Vec<&str> = cmd["secrets"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(secrets, ["alice", "hunter2", "1234", "c2VjcmV0a2V5"]);
        assert_eq!(cmd["passkeys"][0]["rpId"], "example.com");
        // Nothing is left for a daemon to inherit.
        for k in [
            CHILD,
            USER,
            PASS,
            FIELDS,
            "CU_BWU_F0",
            STEPS,
            ORIGIN,
            ITEM,
            PASSKEYS,
        ] {
            assert!(std::env::var_os(k).is_none(), "{k} still set");
        }
    }
}
