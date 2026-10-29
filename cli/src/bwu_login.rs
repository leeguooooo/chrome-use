//! `auth login --bwu`: log in to the current page with an account from the
//! user's Bitwarden vault, through bitwarden-use (`bwu`, 0.7.0+).
//!
//! Outside `bwu run` this CLI picks the account (masked:
//! `bwu login --domain <url> --list`, then the item's `_autotype` steps), and
//! runs the same command line again under `bwu run`, which authorizes access
//! for the item and puts the values in the child's environment only. The
//! child ([`parse`] with `CU_BWU_CHILD` set) reads them, drops them from the
//! environment before a daemon can inherit them, and sends one secret
//! command (`auth_login_bwu`) whose response is scrubbed of every value.
//! Default TOTP login uses a second code-only child after observing the challenge;
//! each phase has its own vault authorization and audit, with no code retries.
//! Values are never arguments, output or part of the transcript.

use std::process::Command;

use serde_json::{json, Value};

use crate::color;
use crate::commands::ParseError;
use crate::flags::Flags;

const USAGE: &str =
    "chrome-use auth login --bwu [--item <id|name>] [--passkey | --totp] [--no-submit]";

/// Set on the child so it fills instead of running `bwu` again.
const CHILD: &str = "CU_BWU_CHILD";
const USER: &str = "CU_BWU_USER";
const PASS: &str = "CU_BWU_PW";
const OTP: &str = "CU_BWU_OTP";
/// Fetch the code in a second child only after the page asks for it.
const DEFER_OTP: &str = "CU_BWU_DEFER_OTP";
/// The item's passkeys as JSON (bwu `#passkeys`), private keys included.
const PASSKEYS: &str = "CU_BWU_PK";
/// JSON list of custom field names; their values are in `CU_BWU_F<i>`.
const FIELDS: &str = "CU_BWU_FIELDS";
const STEPS: &str = "CU_BWU_STEPS";
const ORIGIN: &str = "CU_BWU_ORIGIN";
const ITEM: &str = "CU_BWU_ITEM";
/// The item's id, for remembering it once the sign-in is confirmed.
const ITEM_ID: &str = "CU_BWU_ITEM_ID";

struct Opts {
    item: Option<String>,
    no_submit: bool,
    passkey: bool,
    totp: bool,
}

fn opts(args: &[&str]) -> Result<Opts, ParseError> {
    let mut o = Opts {
        item: None,
        no_submit: false,
        passkey: false,
        totp: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "--bwu" => {}
            "--no-submit" => o.no_submit = true,
            "--passkey" => o.passkey = true,
            "--totp" => o.totp = true,
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
    if o.totp && (o.passkey || o.no_submit) {
        return Err(ParseError::InvalidValue {
            message: "--totp cannot be combined with --passkey or --no-submit: code fields may submit automatically".to_string(),
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
            "item": o.item, "noSubmit": o.no_submit, "passkey": o.passkey, "totpOnly": o.totp,
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
    let defer_otp = take(DEFER_OTP).is_some();
    let passkeys: Vec<Value> = take(PASSKEYS)
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default();
    let steps: Option<Value> = take(STEPS).and_then(|j| serde_json::from_str(&j).ok());
    let origin = take(ORIGIN);
    let item = take(ITEM);
    let item_id = take(ITEM_ID);
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
        "steps": steps, "origin": origin, "item": item, "itemId": item_id, "noSubmit": o.no_submit,
        "passkeys": passkeys, "passkeyFirst": o.passkey, "totpOnly": o.totp, "deferOtp": defer_otp,
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

/// The hosts a login on the tab's page involves: the page's own, and those of
/// its same-site sign-in frames (App Store Connect signs in through an
/// idmsa.apple.com iframe).
struct LoginHosts {
    page: String,
    frames: Vec<String>,
}

impl LoginHosts {
    fn page(host: &str) -> Self {
        Self {
            page: host.to_string(),
            frames: Vec::new(),
        }
    }

    /// What an item's name says it is for, if it names one of these hosts or
    /// their site: the reason the error lists it first.
    fn named_for(&self, c: &Value) -> Option<String> {
        let name = c["name"].as_str()?.trim();
        let is = |h: &str| !h.is_empty() && name.eq_ignore_ascii_case(h);
        if is(&self.page) {
            return Some("named for this page".to_string());
        }
        if let Some(f) = self.frames.iter().find(|f| is(f)) {
            return Some(format!("named for the sign-in frame {f}"));
        }
        let site = crate::native::login_wall::site(&self.page);
        (site != self.page && is(&site)).then(|| format!("named for {site}"))
    }
}

/// Why an item was used without `--item`, for the "logging in as" line.
const PICKED_REMEMBERED: &str = "the login used for this site last time";

/// Choose the vault item: `--item` (id or exact name), else the only match,
/// else the one used for this page's host last time (`remembered`: an id
/// recorded after a login with it went through), else the only one *named*
/// exactly the page's host (#479: bwu matches by registrable domain, so
/// `zentao.example.com` also lists every other `*.example.com` login; an item
/// named `zentao.example.com` is the one meant for it), else the only one
/// named exactly a sign-in frame's host. Otherwise several matches are listed
/// for the caller to pick from, those named for the page, its frame or its
/// site first, then most recently used; picking one silently could log in as
/// the wrong account. Returns the item and why it was picked (`None`: it was
/// asked for, or the only one).
fn choose(
    candidates: &[Value],
    item: Option<&str>,
    hosts: &LoginHosts,
    remembered: Option<&str>,
) -> Result<(Value, Option<String>), String> {
    let host = hosts.page.as_str();
    let line = |c: &Value| {
        format!(
            "  --item {}  {}{}  (used {}×{}){}",
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
            hosts
                .named_for(c)
                .map(|why| format!("  ← {why}"))
                .unwrap_or_default(),
        )
    };
    // Named for the page, a frame or the site first; otherwise bwu's order.
    let listing = |many: &[&Value]| {
        let mut sorted = many.to_vec();
        sorted.sort_by_key(|c| hosts.named_for(c).is_none());
        sorted
            .iter()
            .map(|c| line(c))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let named = |h: &str| -> Vec<&Value> {
        candidates
            .iter()
            .filter(|c| {
                !h.is_empty()
                    && c["name"]
                        .as_str()
                        .is_some_and(|n| n.trim().eq_ignore_ascii_case(h))
            })
            .collect()
    };
    let mut why = None;
    let picked: Vec<&Value> = match item {
        Some(want) => {
            let exact: Vec<&Value> = candidates
                .iter()
                .filter(|c| c["id"] == want || c["name"] == want)
                .collect();
            // A name typed in another case still names one item, never two.
            if exact.is_empty() {
                named(want)
            } else {
                exact
            }
        }
        None if candidates.len() > 1 => {
            let last: Vec<&Value> = remembered
                .map(|id| candidates.iter().filter(|c| c["id"] == id).collect())
                .unwrap_or_default();
            if last.len() == 1 {
                why = Some(PICKED_REMEMBERED.to_string());
                last
            } else if let [one] = named(host).as_slice() {
                why = Some(format!("its name is {host}"));
                vec![*one]
            } else {
                // The sign-in frame's host, when exactly one item over all
                // the frames is named for one.
                let framed: Vec<(&String, &Value)> = hosts
                    .frames
                    .iter()
                    .flat_map(|f| named(f).into_iter().map(move |c| (f, c)))
                    .collect();
                match framed.as_slice() {
                    [(f, one)] if named(host).is_empty() => {
                        why = Some(format!("its name is the sign-in frame's host {f}"));
                        vec![*one]
                    }
                    _ => candidates.iter().collect(),
                }
            }
        }
        None => candidates.iter().collect(),
    };
    let frames = if hosts.frames.is_empty() {
        String::new()
    } else {
        format!(" (its sign-in frame: {})", hosts.frames.join(", "))
    };
    match picked.as_slice() {
        [one] => Ok(((*one).clone(), why)),
        [] if candidates.is_empty() => Err(format!(
            "no login in your Bitwarden vault matches {host}. Save one with this site's URL, or \
             fill the form by hand."
        )),
        [] => Err(format!(
            "no login for {host} has the id or name '{}'. Logins for this site:\n{}",
            item.unwrap_or(""),
            listing(&candidates.iter().collect::<Vec<_>>())
        )),
        many => Err(format!(
            "{} logins in your vault match {host}{frames}; pick one with --item (those named for \
             this page, its sign-in frame or its site first, then most recently used):\n{}",
            many.len(),
            listing(many)
        )),
    }
}

/// `~/.chrome-use/bwu-items.json`: the vault item last used to sign in to
/// each host, by id. Only ids and names, never a value.
fn remembered_path() -> Option<std::path::PathBuf> {
    Some(dirs::home_dir()?.join(".chrome-use").join("bwu-items.json"))
}

/// The item id last used to sign in to `host`.
fn remembered_item(host: &str) -> Option<String> {
    let store: Value = std::fs::read_to_string(remembered_path()?)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())?;
    store["hosts"][host.to_ascii_lowercase()]["id"]
        .as_str()
        .map(str::to_string)
}

/// The child half, after the daemon answered: a login the site was seen to
/// take (`signedIn`) remembers its item for the page's host. Nothing else
/// counts: not a page that was already signed in, nor one that still asks for
/// a code.
pub fn remember_if_signed_in(cmd: &Value, data: Option<&Value>) {
    if data.and_then(|d| d["signedIn"].as_bool()) != Some(true) {
        return;
    }
    let host = cmd["origin"]
        .as_str()
        .and_then(|o| url::Url::parse(o).ok())
        .and_then(|u| u.host_str().map(str::to_string));
    if let (Some(host), Some(id)) = (host, cmd["itemId"].as_str()) {
        remember_item(&host, id);
    }
}

/// Remember that `id` signed in to `host`. Best effort: a failure to write
/// only means the next login asks again.
fn remember_item(host: &str, id: &str) {
    let Some(path) = remembered_path() else {
        return;
    };
    let mut store: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    if !store["hosts"].is_object() {
        store["hosts"] = json!({});
    }
    store["hosts"][host.to_ascii_lowercase()] = json!({ "id": id });
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(&store).unwrap_or_default();
    if std::fs::write(&tmp, body + "\n").is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Hosts of the tab's same-site child frames (`frames`), where a sign-in
/// form may live: App Store Connect's is an idmsa.apple.com iframe. Empty when
/// they cannot be read; the page's own host is never listed.
fn sign_in_frame_hosts(flags: &Flags, page_host: &str) -> Vec<String> {
    let resp = crate::connection::send_command(
        json!({ "id": crate::commands::gen_id(), "action": "frames" }),
        &flags.session,
    );
    let frames = match resp {
        Ok(r) if r.success => r
            .data
            .and_then(|d| d["frames"].as_array().cloned())
            .unwrap_or_default(),
        _ => return Vec::new(),
    };
    frame_hosts(&frames, page_host)
}

fn frame_hosts(frames: &[Value], page_host: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in frames {
        let Some(u) = f["url"].as_str().and_then(|u| url::Url::parse(u).ok()) else {
            continue;
        };
        let Some(h) = u.host_str().map(str::to_ascii_lowercase) else {
            continue;
        };
        if matches!(u.scheme(), "http" | "https")
            && !h.eq_ignore_ascii_case(page_host)
            && crate::native::login_wall::related(&h, &page_host.to_ascii_lowercase())
            && !out.contains(&h)
        {
            out.push(h);
        }
    }
    out
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
    /// Why this item was used without `--item` (see [`choose`]).
    why: Option<String>,
    defer_otp: bool,
}

/// Choose the vault item for the tab's current page (masked), and which of
/// its values the login needs.
fn prepare(
    flags: &Flags,
    item_arg: Option<&str>,
    passkey_first: bool,
    no_submit: bool,
    totp_only: bool,
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
    // Sign-in frames matter only when the name has to settle it.
    let hosts = if item_arg.is_none() && candidates.len() > 1 {
        LoginHosts {
            page: host.clone(),
            frames: sign_in_frame_hosts(flags, &host),
        }
    } else {
        LoginHosts::page(&host)
    };
    let (chosen, why) = choose(
        &candidates,
        item_arg,
        &hosts,
        remembered_item(&host).as_deref(),
    )?;
    let id = chosen["id"].as_str().unwrap_or_default().to_string();
    let name = chosen["name"].as_str().unwrap_or_default().to_string();
    // Masked: which values the item has, and its `_autotype` steps.
    let item = bwu_json(&bwu, &["login", "--domain", &url, "--name", &id])?;
    let mut steps: Option<Vec<String>> = item["autotype"].as_array().map(|a| {
        a.iter()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect()
    });
    if totp_only {
        if !item["code"].is_string() {
            return Err(format!("the vault item '{name}' has no TOTP seed. Add the authenticator's setup key or otpauth URI to this login's authenticator field in Bitwarden; storing only the password is not enough. SMS, email and push codes need their own source."));
        }
        steps = Some(vec!["totp".into(), "enter".into()]);
    }
    let has = |k: &str| item[k].is_string();
    let wants = |step: &str| steps.as_ref().is_none_or(|s| s.iter().any(|x| x == step));

    let passkey_count = item["passkeys"].as_u64().unwrap_or(0);
    // --no-submit fills and presses nothing: no passkey (a ceremony signs and
    // submits) and no one-time code (sites submit on the last digit).
    if passkey_first && passkey_count == 0 {
        return Err(format!("the vault item '{name}' has no passkey"));
    }
    let mut envs: Vec<String> = Vec::new();
    if passkey_count > 0 && !no_submit && !totp_only {
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
    // Default multi-step login: do not spend a code's lifetime filling the
    // password. Explicit _autotype retains its caller-defined sequence.
    let defer_otp =
        has("code") && wants("totp") && !no_submit && steps.is_none() && !envs.is_empty();
    if has("code") && wants("totp") && !no_submit && !defer_otp {
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
        why,
        defer_otp,
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
        .env(ITEM_ID, &p.id)
        .env(FIELDS, serde_json::to_string(&p.custom).unwrap_or_default());
    if p.defer_otp {
        run.env(DEFER_OTP, "1");
    } else {
        run.env_remove(DEFER_OTP);
    }
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
        cmd["totpOnly"].as_bool().unwrap_or(false),
    )?;
    if !flags.json {
        eprintln!(
            "logging in to {} as vault item '{}' (bwu asks for Touch ID unless require_touch_id is off)",
            p.host, p.name
        );
    }
    // An item picked without --item is said even in --json mode, where the
    // line above is not: an agent must be able to see which account it was.
    if let Some(why) = &p.why {
        eprintln!(
            "auth login --bwu: used vault item '{}' ({why}); pass --item for another",
            p.name
        );
    }
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let response = run_login_phases(&p, &args)?;
    crate::output::print_response_with_opts(
        &response,
        Some("auth_login_bwu"),
        &crate::output::OutputOptions::from_flags(flags),
    );
    Ok(if response.success { 0 } else { 1 })
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

/// Capture the scrubbed JSON envelope, never raw secret values. Stderr keeps
/// bwu's authorization prompts and errors attached to the invoking terminal.
fn login_response(
    p: &Prepared,
    args: &[std::ffi::OsString],
) -> Result<crate::connection::Response, String> {
    let mut args = args.to_vec();
    args.insert(0, "--json".into());
    let output = child_command(p, &args)?
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| format!("couldn't run {}: {e}", p.bwu.display()))?;
    let response = String::from_utf8_lossy(&output.stdout)
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<crate::connection::Response>(line).ok());
    let mut response = response.ok_or_else(|| {
        format!(
            "auth login --bwu exited with {} without a result",
            output.status
        )
    })?;
    if !output.status.success() && response.success {
        response.success = false;
        response.error = Some(format!("auth login --bwu exited with {}", output.status));
    }
    Ok(response)
}

/// Continue only an observed code challenge, once, with the same vault item
/// and pinned origin. A failed or rejected code is never automatically retried.
fn run_login_phases(
    p: &Prepared,
    args: &[std::ffi::OsString],
) -> Result<crate::connection::Response, String> {
    let first = login_response(p, args)?;
    if !p.defer_otp
        || !first.success
        || !first
            .data
            .as_ref()
            .is_some_and(|d| d["otp"] == "deferred" && d["needs"] == "a verification code")
    {
        return Ok(first);
    }
    let otp = Prepared {
        bwu: p.bwu.clone(),
        envs: vec![format!("{OTP}=bw:{}#totp", p.id)],
        origin: p.origin.clone(),
        host: p.host.clone(),
        id: p.id.clone(),
        name: p.name.clone(),
        custom: Vec::new(),
        steps: Some(vec!["totp".into(), "enter".into()]),
        why: None,
        defer_otp: false,
    };
    let mut args = args.to_vec();
    args.push("--totp".into());
    let mut next = login_response(&otp, &args).map_err(|e| {
        format!("TOTP verification stopped; inspect the current page before trying again: {e}")
    })?;
    if !next.success {
        next.error = Some(format!(
            "TOTP verification stopped; inspect the current page before trying again: {}",
            next.error.as_deref().unwrap_or("no result")
        ));
    }
    if let (Some(before), Some(after)) = (first.data.as_ref(), next.data.as_mut()) {
        if let (Some(a), Some(b)) = (before["filled"].as_array(), after["filled"].as_array()) {
            let mut filled = a.clone();
            filled.extend(b.iter().cloned());
            after["filled"] = json!(filled);
        }
        if let Some(warning) = before.get("warning").and_then(Value::as_str) {
            let combined = match after["warning"].as_str() {
                Some(w) => format!("{warning}. {w}"),
                None => warning.to_string(),
            };
            after["warning"] = json!(combined);
        }
    }
    Ok(next)
}

/// Auto-login must not navigate away from a pending human verification step.
fn run_login_child(p: &Prepared, args: &[std::ffi::OsString]) -> Result<(), String> {
    let response = run_login_phases(p, args)?;
    if !response.success {
        return Err(response
            .error
            .unwrap_or_else(|| "auth login --bwu failed".into()));
    }
    if let Some(data) = response.data.as_ref() {
        if let Some(needs) = data["needs"].as_str() {
            return Err(format!(
                "login needs {needs}. {}",
                data["next"]
                    .as_str()
                    .unwrap_or("Complete verification on the current page.")
            ));
        }
        if data["signedIn"] == true || data["alreadySignedIn"] == true {
            return Ok(());
        }
    }
    Err("auth login --bwu did not confirm a sign-in".into())
}

/// Whether an `auth login --bwu` failure is worth one more try from the
/// sign-in page (#482): a step the tab detached under (its outcome unknown),
/// or a submit the site was not seen to take while the page showed no message
/// of its own. A page that says why it refused (a wrong password) is not
/// retried: the same login would be refused again and count as another failed
/// attempt.
fn retry_after_unknown_outcome(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    !e.contains("totp verification stopped")
        && !e.contains("one-time code")
        && (e.contains("action_outcome_unknown")
            || (e.contains("the sign-in was not confirmed") && !e.contains("the page says")))
}

/// Sign in to the login wall the tab is on with the only vault login for it
/// (several, or none, is reported, never guessed), then go back to
/// `returnTo`. Returns `{ok, item, error}` plus `returnedTo` / `returnError`.
pub fn auto_login(flags: &Flags, wall: &Value) -> Value {
    let mut out = json!({ "ok": false, "item": null, "error": null });
    let p = match prepare(flags, None, false, false, false) {
        Ok(p) => p,
        Err(e) => {
            out["error"] = json!(e);
            return out;
        }
    };
    out["item"] = json!(p.name);
    eprintln!(
        "login wall: signing in to {} as vault item '{}' (auto-login{})",
        p.host,
        p.name,
        p.why
            .as_deref()
            .map(|w| format!("; {w}"))
            .unwrap_or_default()
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

    fn pick(cands: &[Value], item: Option<&str>, host: &str) -> Result<Value, String> {
        choose(cands, item, &LoginHosts::page(host), None).map(|(c, _)| c)
    }

    #[test]
    fn choose_needs_one_account() {
        let one = [c("1", "github")];
        assert_eq!(pick(&one, None, "github.com").unwrap()["id"], "1");
        let two = [c("1", "work"), c("2", "personal")];
        let e = pick(&two, None, "github.com").unwrap_err();
        assert!(e.contains("--item 1") && e.contains("--item 2"), "{e}");
        assert_eq!(pick(&two, Some("2"), "github.com").unwrap()["id"], "2");
        assert_eq!(pick(&two, Some("work"), "github.com").unwrap()["id"], "1");
        assert_eq!(pick(&two, Some("WORK"), "github.com").unwrap()["id"], "1");
        assert!(pick(&two, Some("x"), "github.com").is_err());
        assert!(pick(&[], None, "github.com")
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
        assert_eq!(pick(&many, None, "zentao.example.com").unwrap()["id"], "2");
        assert_eq!(pick(&many, None, "ZENTAO.example.com").unwrap()["id"], "2");
        // No item named for the host, or two of them: still ask.
        assert!(pick(&many, None, "jira.example.com").is_err());
        let twins = [c("1", "zentao.example.com"), c("2", "zentao.example.com")];
        assert!(pick(&twins, None, "zentao.example.com").is_err());
        // --item still wins.
        assert_eq!(
            pick(&many, Some("1"), "zentao.example.com").unwrap()["id"],
            "1"
        );
    }

    /// App Store Connect: the page is appstoreconnect.apple.com, the form is
    /// in an idmsa.apple.com frame, and bwu lists every apple.com login.
    #[test]
    fn choose_considers_the_sign_in_frame_and_lists_named_items_first() {
        let apple = [
            c("p1", "Apple ID personal"),
            c("p2", "apple.com"),
            c("p3", "iCloud"),
            c("d1", "idmsa.apple.com"),
        ];
        let hosts = LoginHosts {
            page: "appstoreconnect.apple.com".into(),
            frames: vec!["idmsa.apple.com".into()],
        };
        let (item, why) = choose(&apple, None, &hosts, None).unwrap();
        assert_eq!(item["id"], "d1");
        assert!(why.unwrap().contains("idmsa.apple.com"));
        // An item named for the page itself beats one named for its frame.
        let mut both = apple.to_vec();
        both.push(c("a1", "appstoreconnect.apple.com"));
        assert_eq!(choose(&both, None, &hosts, None).unwrap().0["id"], "a1");

        // Nothing named for the page or the frame: ask, the site's item first.
        let plain = [
            c("p1", "Apple ID personal"),
            c("p3", "iCloud"),
            c("p2", "apple.com"),
        ];
        let e = choose(&plain, None, &hosts, None).unwrap_err();
        assert!(e.contains("its sign-in frame: idmsa.apple.com"), "{e}");
        let first = e.lines().nth(1).unwrap();
        assert!(
            first.contains("--item p2") && first.contains("named for apple.com"),
            "{e}"
        );
        // Two items named for frames: never guessed.
        let twins = [c("x", "idmsa.apple.com"), c("y", "idmsa.apple.com")];
        assert!(choose(&twins, None, &hosts, None).is_err());
    }

    #[test]
    fn choose_uses_the_item_that_signed_in_here_last_time() {
        let apple = [c("p1", "Apple ID personal"), c("d1", "Developer")];
        let hosts = LoginHosts::page("appstoreconnect.apple.com");
        let (item, why) = choose(&apple, None, &hosts, Some("d1")).unwrap();
        assert_eq!(item["id"], "d1");
        assert_eq!(why.as_deref(), Some(PICKED_REMEMBERED));
        // --item overrides it; a remembered item no longer listed is ignored.
        assert_eq!(
            choose(&apple, Some("p1"), &hosts, Some("d1")).unwrap().0["id"],
            "p1"
        );
        assert!(choose(&apple, None, &hosts, Some("gone")).is_err());
        // The only match needs no reason.
        assert_eq!(
            choose(&apple[..1], None, &hosts, Some("d1")).unwrap().1,
            None
        );
    }

    #[test]
    fn frame_hosts_are_same_site_children_only() {
        let frames = [
            json!({ "kind": "top", "url": "https://appstoreconnect.apple.com/login" }),
            json!({ "kind": "iframe", "url": "https://idmsa.apple.com/appleauth/auth/signin" }),
            json!({ "kind": "iframe", "url": "https://idmsa.apple.com/other" }),
            json!({ "kind": "iframe", "url": "https://ads.example.net/x" }),
            json!({ "kind": "iframe", "url": "about:blank" }),
        ];
        assert_eq!(
            frame_hosts(&frames, "appstoreconnect.apple.com"),
            ["idmsa.apple.com"]
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

    /// A mock bwu checks the actual subprocess boundary: passwords only in
    /// phase one, a freshly requested code only in phase two, and no retry.
    #[cfg(unix)]
    #[test]
    fn deferred_totp_reads_only_after_observed_challenge() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bwu = dir.path().join("bwu");
        let calls = dir.path().join("calls");
        std::fs::write(&bwu, r#"#!/usr/bin/env python3
import json, os, pathlib, sys
calls = pathlib.Path(__file__).with_name('calls')
args = sys.argv[1:]
n = int(calls.read_text()) if calls.exists() else 0
scenario = pathlib.Path(__file__).with_name('scenario').read_text()
calls.write_text(str(n + 1))
specs = [args[i+1] for i, a in enumerate(args) if a == '--env']
if n == 0:
    assert specs == ['CU_BWU_PW=bw:uuid#password'], specs
    assert os.environ.get('CU_BWU_DEFER_OTP') == '1'
    if scenario == 'already-done':
        print(json.dumps({'success': True, 'data': {'signedIn': True, 'filled': ['password']}, 'error': None}))
    elif scenario == 'password-failed':
        print(json.dumps({'success': False, 'data': None, 'error': 'wrong password'}))
        sys.exit(1)
    elif scenario == 'unsupported':
        print(json.dumps({'success': True, 'data': {'otp': 'unsupported', 'needs': 'a verification code', 'filled': ['password'], 'submitted': True}, 'error': None}))
    else:
        print(json.dumps({'success': True, 'data': {'otp': 'deferred', 'needs': 'a verification code', 'filled': ['password'], 'submitted': True}, 'error': None}))
else:
    assert n == 1, 'a code was retried'
    assert specs == ['CU_BWU_OTP=bw:uuid#totp'], specs
    assert 'CU_BWU_DEFER_OTP' not in os.environ
    assert json.loads(os.environ['CU_BWU_STEPS']) == ['totp', 'enter']
    assert '--totp' in args
    if scenario == 'accepted':
        print(json.dumps({'success': True, 'data': {'otp': 'filled', 'filled': ['totp'], 'signedIn': True}, 'error': None}))
    else:
        print(json.dumps({'success': False, 'data': None, 'error': 'the site rejected the code'}))
        sys.exit(1)
"#).unwrap();
        std::fs::set_permissions(&bwu, std::fs::Permissions::from_mode(0o700)).unwrap();
        let p = Prepared {
            bwu,
            envs: vec!["CU_BWU_PW=bw:uuid#password".into()],
            origin: "https://example.com".into(),
            host: "example.com".into(),
            id: "uuid".into(),
            name: "example".into(),
            custom: Vec::new(),
            steps: None,
            why: None,
            defer_otp: true,
        };
        for (scenario, count, success) in [
            ("rejected", "2", false),
            ("accepted", "2", true),
            ("already-done", "1", true),
            ("password-failed", "1", false),
            ("unsupported", "1", true),
        ] {
            let _ = std::fs::remove_file(&calls);
            std::fs::write(dir.path().join("scenario"), scenario).unwrap();
            let r = run_login_phases(&p, &["auth".into(), "login".into(), "--bwu".into()]).unwrap();
            assert_eq!(r.success, success, "{scenario}");
            if scenario == "rejected" {
                assert!(r
                    .error
                    .as_deref()
                    .unwrap()
                    .contains("the site rejected the code"));
            }
            if scenario == "accepted" {
                assert_eq!(r.data.unwrap()["filled"], json!(["password", "totp"]));
            }
            assert_eq!(
                std::fs::read_to_string(&calls).unwrap(),
                count,
                "{scenario}"
            );
        }
        assert!(!retry_after_unknown_outcome(
            "TOTP verification stopped: action_outcome_unknown"
        ));
        assert!(!retry_after_unknown_outcome(
            "TOTP verification stopped: the sign-in was not confirmed"
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
        assert!(parse(&["--bwu", "--totp", "--no-submit"], "1").is_err());
        assert!(parse(&["--bwu", "--totp", "--passkey"], "1").is_err());
        assert_eq!(parse(&["--bwu", "--totp"], "1").unwrap()["totpOnly"], true);

        for (k, v) in [
            (CHILD, "1"),
            (USER, "alice"),
            (PASS, "hunter2"),
            (DEFER_OTP, "1"),
            (FIELDS, r#"["PIN"]"#),
            ("CU_BWU_F0", "1234"),
            (
                STEPS,
                r#"["username","enter","password","custom:PIN","enter"]"#,
            ),
            (ORIGIN, "https://example.com"),
            (ITEM, "example"),
            (ITEM_ID, "uuid-1"),
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
        assert_eq!(cmd["deferOtp"], true);
        assert_eq!(cmd["fields"]["PIN"], "1234");
        assert_eq!(cmd["steps"][3], "custom:PIN");
        assert_eq!(cmd["origin"], "https://example.com");
        assert_eq!(cmd["itemId"], "uuid-1");
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
            DEFER_OTP,
            FIELDS,
            "CU_BWU_F0",
            STEPS,
            ORIGIN,
            ITEM,
            ITEM_ID,
            PASSKEYS,
        ] {
            assert!(std::env::var_os(k).is_none(), "{k} still set");
        }
    }
}
