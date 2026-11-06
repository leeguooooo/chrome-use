//! `auth login --bwu` / `auth login --1password`: log in to the current page
//! with an account from the user's password manager: Bitwarden through
//! bitwarden-use (`bwu`, 0.7.0+), or 1Password through the official `op` CLI
//! on PATH. 1Password cannot hand out passkeys, so `--passkey` stays with
//! Bitwarden.
//!
//! Outside `bwu run` this CLI picks the account (masked:
//! `bwu login --domain <url> --list`, then the item's `_autotype` steps; for
//! 1Password `op item list --categories Login`, matched against the page's
//! host here, with no values and no probe of the item), and runs the same
//! command line again under `bwu run` (or `op run`, with `op://` references
//! in its environment), which authorizes access
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
    "chrome-use auth login --bwu|--1password [--item <id|name>] [--passkey | --totp] [--no-submit]";

/// The password manager `auth login` takes the account from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Manager {
    /// `--bwu`: bitwarden-use.
    Bitwarden,
    /// `--1password`: the official 1Password CLI, `op`.
    OnePassword,
}

impl Manager {
    /// `bwu` / `bitwarden`, or `1password` / `op`, as in
    /// `AGENT_BROWSER_PASSWORD_MANAGER` or `auth.passwordManager`.
    pub fn from_name(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "bwu" | "bitwarden" | "bitwarden-use" => Some(Self::Bitwarden),
            "1password" | "op" | "onepassword" => Some(Self::OnePassword),
            _ => None,
        }
    }

    /// The one sign-ins and login-wall hints use when no flag names one:
    /// `AGENT_BROWSER_PASSWORD_MANAGER`, else an `AGENT_BROWSER_AUTO_LOGIN`
    /// that names one (`1password` turns auto-login on and picks it), else
    /// `"auth": {"passwordManager"}` in `~/.chrome-use/config.json`, else an
    /// `auth.autoLogin` that names one, else Bitwarden.
    pub fn configured() -> Self {
        Self::resolve(
            Self::from_env().map(Self::name),
            crate::report::user_config().as_ref(),
        )
    }

    /// The manager this process's environment names:
    /// `AGENT_BROWSER_PASSWORD_MANAGER`, else an `AGENT_BROWSER_AUTO_LOGIN`
    /// that names one.
    pub fn from_env() -> Option<Self> {
        let env = |k: &str| std::env::var(k).ok().and_then(|v| Self::from_name(&v));
        env("AGENT_BROWSER_PASSWORD_MANAGER").or_else(|| env("AGENT_BROWSER_AUTO_LOGIN"))
    }

    /// [`Self::configured`] for a command the daemon runs: the environment
    /// part comes from the CLI that sent it (`_passwordManager`, empty when
    /// its environment names none), since the daemon's own is frozen at
    /// spawn. A command from an older CLI without the field uses the
    /// daemon's.
    pub fn for_command(cmd: &Value) -> Self {
        match cmd.get("_passwordManager").and_then(Value::as_str) {
            Some(env) => Self::resolve(Some(env), crate::report::user_config().as_ref()),
            None => Self::configured(),
        }
    }

    fn resolve(env: Option<&str>, config: Option<&Value>) -> Self {
        if let Some(m) = env.and_then(Self::from_name) {
            return m;
        }
        let auth = |key: &str| {
            config.and_then(|c| {
                c.get("auth")
                    .and_then(|a| a.get(key))
                    .or_else(|| c.get(format!("auth.{key}")))
                    .and_then(Value::as_str)
                    .and_then(Self::from_name)
            })
        };
        auth("passwordManager")
            .or_else(|| auth("autoLogin"))
            .unwrap_or(Self::Bitwarden)
    }

    pub fn flag(self) -> &'static str {
        match self {
            Self::Bitwarden => "--bwu",
            Self::OnePassword => "--1password",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Bitwarden => "Bitwarden",
            Self::OnePassword => "1Password",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Bitwarden => "bwu",
            Self::OnePassword => "1password",
        }
    }

    /// One value of an item for the child: `bwu run --env VAR=bw:<id>#<field>`,
    /// or `VAR=op://<vault>/<id>/<field>` in `op run`'s environment.
    fn env_ref(self, var: &str, vault: Option<&str>, id: &str, field: &str) -> String {
        match self {
            Self::Bitwarden => format!("{var}=bw:{id}#{field}"),
            Self::OnePassword => {
                let field = match field {
                    "totp" => "one-time password?attribute=otp",
                    f => f.strip_prefix("custom:").unwrap_or(f),
                };
                format!("{var}=op://{}/{id}/{field}", vault.unwrap_or_default())
            }
        }
    }

    fn missing(self) -> &'static str {
        match self {
            Self::Bitwarden => {
                "auth login --bwu needs bitwarden-use (bwu) on PATH. Install it with: curl -fsSL \
                 https://raw.githubusercontent.com/leeguooooo/bitwarden-use/main/install.sh | sh"
            }
            Self::OnePassword => {
                "auth login --1password needs the 1Password CLI (`op`) on PATH, signed in \
                 through the 1Password app or OP_SERVICE_ACCOUNT_TOKEN: \
                 https://developer.1password.com/docs/cli/get-started/"
            }
        }
    }

    /// The manager's CLI on PATH.
    fn find(self) -> Option<std::path::PathBuf> {
        find_on_path(match self {
            Self::Bitwarden => &["bwu", "bitwarden-use"],
            Self::OnePassword => &["op"],
        })
    }

    /// `~/.chrome-use/<file>`: the item last used to sign in to each host.
    fn remembered_file(self) -> &'static str {
        match self {
            Self::Bitwarden => "bwu-items.json",
            Self::OnePassword => "1password-items.json",
        }
    }
}

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
    manager: Manager,
    item: Option<String>,
    no_submit: bool,
    passkey: bool,
    totp: bool,
}

fn opts(args: &[&str]) -> Result<Opts, ParseError> {
    let mut o = Opts {
        manager: Manager::Bitwarden,
        item: None,
        no_submit: false,
        passkey: false,
        totp: false,
    };
    if args.contains(&"--bwu") && args.contains(&"--1password") {
        return Err(ParseError::InvalidValue {
            message: "pick one of --bwu and --1password".to_string(),
            usage: USAGE,
        });
    }
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "--bwu" => {}
            "--1password" => o.manager = Manager::OnePassword,
            "--no-submit" => o.no_submit = true,
            "--passkey" => o.passkey = true,
            "--totp" => o.totp = true,
            "--item" => {
                o.item = Some(
                    args.get(i + 1)
                        .ok_or_else(|| ParseError::MissingArguments {
                            context: format!("auth login {} --item", o.manager.flag()),
                            usage: USAGE,
                        })?
                        .to_string(),
                );
                i += 1;
            }
            other => {
                return Err(ParseError::InvalidValue {
                    message: format!(
                        "auth login {} takes no credential name ('{other}'): the account \
                         comes from your {} vault for the current page",
                        o.manager.flag(),
                        o.manager.label()
                    ),
                    usage: USAGE,
                })
            }
        }
        i += 1;
    }
    if o.passkey && o.manager == Manager::OnePassword {
        return Err(ParseError::InvalidValue {
            message: "--passkey needs --bwu: 1Password cannot hand a passkey to chrome-use"
                .to_string(),
            usage: USAGE,
        });
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

/// Parse `auth login --bwu …` or `auth login --1password …` (the args after
/// `login`).
pub fn parse(args: &[&str], id: &str) -> Result<Value, ParseError> {
    let o = opts(args)?;
    if std::env::var_os(CHILD).is_none() {
        return Ok(json!({
            "id": id, "action": "auth_login_bwu_probe", "manager": o.manager.name(),
            "item": o.item, "noSubmit": o.no_submit, "passkey": o.passkey, "totpOnly": o.totp,
        }));
    }
    // Under `bwu run` / `op run`: take the values and drop them from the
    // environment before a daemon this command starts can inherit them, along
    // with the service account token `op run` passes through.
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
    if o.manager == Manager::OnePassword {
        std::env::remove_var("OP_SERVICE_ACCOUNT_TOKEN");
    }
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
        "id": id, "action": "auth_login_bwu", "manager": o.manager.name(),
        "secret": true, "secrets": secrets,
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

/// The first of `names` on PATH.
fn find_on_path(names: &[&str]) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for name in names {
        for dir in std::env::split_paths(&path) {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Run `bwu` / `op` and parse its stdout as JSON. Its stderr (unlock
/// prompts, update notices) goes to ours.
fn helper_json(bin: &std::path::Path, args: &[&str]) -> Result<Value, String> {
    let tool = bin
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "bwu".into());
    let out = Command::new(bin)
        .args(args)
        // A tool that asks on stdin would wait on ours forever (and `op`
        // reads piped stdin as a list of items).
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("couldn't run {}: {e}", bin.display()))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        if stderr.contains("unexpected argument '--list'") {
            return Err(
                "auth login --bwu needs bitwarden-use 0.7.0 or newer: run `bwu upgrade`"
                    .to_string(),
            );
        }
        return Err(format!(
            "`{tool} {}` failed: {}",
            args.join(" "),
            stderr.trim()
        ));
    }
    eprint!("{stderr}");
    serde_json::from_slice(&out.stdout).map_err(|e| {
        format!(
            "`{tool} {}` printed something that is not JSON: {e}",
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
    manager: Manager,
) -> Result<(Value, Option<String>), String> {
    let host = hosts.page.as_str();
    let line = |c: &Value| {
        format!(
            "  --item {}  {}{}{}{}",
            c["id"].as_str().unwrap_or(""),
            c["name"].as_str().unwrap_or(""),
            c["folder"]
                .as_str()
                .map(|f| format!(" [{f}]"))
                .unwrap_or_default(),
            // `op item list` keeps no use count: just when it last changed.
            match (&c["uses"], c["last_used"].as_str(), c["updated"].as_str()) {
                (Value::Null, _, Some(t)) => format!("  (updated {t})"),
                (Value::Null, _, None) => String::new(),
                (uses, last, _) => format!(
                    "  (used {uses}×{})",
                    last.map(|t| format!(", last {t}")).unwrap_or_default()
                ),
            },
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
            "no login in your {} vault matches {host}. Save one with this site's URL, or \
             fill the form by hand.",
            manager.label()
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

/// The Login items of `op item list --categories Login --format json` with a
/// website on the page's site (its registrable domain by the Public Suffix
/// List, private suffixes included: `alice.github.io` is not
/// `mallory.github.io`; the exact host when the list does not know it), most
/// recently changed first, in the shape [`choose`] reads. `op item list`
/// carries no field values.
fn op_candidates(items: &Value, host: &str) -> Vec<Value> {
    let url_host = |href: &str| {
        let href = href.trim();
        let full = if href.contains("://") {
            href.to_string()
        } else {
            format!("https://{href}")
        };
        url::Url::parse(&full)
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
    };
    let host = host.to_ascii_lowercase();
    let mut out: Vec<Value> = items
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i["category"] == "LOGIN")
        .filter(|i| {
            i["urls"].as_array().into_iter().flatten().any(|u| {
                u["href"]
                    .as_str()
                    .and_then(url_host)
                    .is_some_and(|h| same_site(&h, &host))
            })
        })
        .map(|i| {
            json!({
                "id": i["id"], "name": i["title"], "folder": i["vault"]["name"],
                "vault": i["vault"]["id"], "updated": i["updated_at"],
            })
        })
        .collect();
    out.sort_by(|a, b| b["updated"].as_str().cmp(&a["updated"].as_str()));
    out
}

/// Whether two hosts share a registrable domain the Public Suffix List knows
/// (or are the same host).
fn same_site(a: &str, b: &str) -> bool {
    use crate::native::login_wall::registrable_domain as site;
    a.trim_end_matches('.') == b.trim_end_matches('.')
        || matches!((site(a), site(b)), (Some(x), Some(y)) if x == y)
}

/// `~/.chrome-use/bwu-items.json` (`1password-items.json`): the vault item
/// last used to sign in to each host, by id. Only ids and names, never a value.
fn remembered_path(manager: Manager) -> Option<std::path::PathBuf> {
    Some(
        dirs::home_dir()?
            .join(".chrome-use")
            .join(manager.remembered_file()),
    )
}

/// The item id last used to sign in to `host`.
fn remembered_item(manager: Manager, host: &str) -> Option<String> {
    let store: Value = std::fs::read_to_string(remembered_path(manager)?)
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
    let manager = cmd["manager"]
        .as_str()
        .and_then(Manager::from_name)
        .unwrap_or(Manager::Bitwarden);
    if let (Some(host), Some(id)) = (host, cmd["itemId"].as_str()) {
        remember_item(manager, &host, id);
    }
}

/// Remember that `id` signed in to `host`. Best effort: a failure to write
/// only means the next login asks again.
fn remember_item(manager: Manager, host: &str, id: &str) {
    let Some(path) = remembered_path(manager) else {
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
            && crate::native::login_wall::same_login_site(&h, &page_host.to_ascii_lowercase())
            && !out.contains(&h)
        {
            out.push(h);
        }
    }
    out
}

/// The parent half: choose the account, then run this command line again
/// under the helper's `run`. Returns the exit code.
pub fn run(flags: &Flags, cmd: &Value) -> i32 {
    match run_inner(flags, cmd) {
        Ok(code) => code,
        Err(e) => fail(flags, &e),
    }
}

/// The account chosen for the tab's page and how `bwu run` / `op run` hands
/// its values to the child.
struct Prepared {
    manager: Manager,
    /// `bwu` or `op`.
    tool: std::path::PathBuf,
    /// The 1Password vault (id) the item is in, for its `op://` references.
    vault: Option<String>,
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
    manager: Manager,
    item_arg: Option<&str>,
    passkey_first: bool,
    no_submit: bool,
    totp_only: bool,
) -> Result<Prepared, String> {
    let tool = manager.find().ok_or(manager.missing())?;
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

    let candidates = match manager {
        Manager::Bitwarden => helper_json(&tool, &["login", "--domain", &url, "--list"])?
            ["candidates"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
        Manager::OnePassword => op_candidates(
            &helper_json(
                &tool,
                &["item", "list", "--categories", "Login", "--format", "json"],
            )?,
            &host,
        ),
    };
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
        remembered_item(manager, &host).as_deref(),
        manager,
    )?;
    let id = chosen["id"].as_str().unwrap_or_default().to_string();
    let name = chosen["name"].as_str().unwrap_or_default().to_string();
    let vault = chosen["vault"].as_str().map(str::to_string);
    let item = match manager {
        // Masked: which values the item has, and its `_autotype` steps.
        Manager::Bitwarden => helper_json(&tool, &["login", "--domain", &url, "--name", &id])?,
        // No probe: `op item get` would hand this process the item's values.
        // A Login item always has a username and a password field; a code
        // the item lacks makes `op run` fail, which is reported.
        Manager::OnePassword => json!({ "username": "", "password": "", "code": "" }),
    };
    let mut steps: Option<Vec<String>> = item["autotype"].as_array().map(|a| {
        a.iter()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect()
    });
    if totp_only {
        if !item["code"].is_string() {
            return Err(format!("the vault item '{name}' has no TOTP seed. Add the authenticator's setup key or otpauth URI to this login's one-time password field in {}; storing only the password is not enough. SMS, email and push codes need their own source.", manager.label()));
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
        envs.push(manager.env_ref(PASSKEYS, vault.as_deref(), &id, "passkeys"));
    }
    // Signing in with the passkey needs nothing else from the vault.
    let wants = |step: &str| !passkey_first && wants(step);
    if has("username") && wants("username") {
        envs.push(manager.env_ref(USER, vault.as_deref(), &id, "username"));
    }
    if has("password") && wants("password") {
        envs.push(manager.env_ref(PASS, vault.as_deref(), &id, "password"));
    }
    // Default multi-step login: do not spend a code's lifetime filling the
    // password. Explicit _autotype retains its caller-defined sequence.
    let defer_otp =
        has("code") && wants("totp") && !no_submit && steps.is_none() && !envs.is_empty();
    if has("code") && wants("totp") && !no_submit && !defer_otp {
        envs.push(manager.env_ref(OTP, vault.as_deref(), &id, "totp"));
    }
    let custom: Vec<String> = steps
        .iter()
        .flatten()
        .filter_map(|s| s.strip_prefix("custom:").map(str::to_string))
        .filter(|_| !passkey_first)
        .collect();
    for (i, field) in custom.iter().enumerate() {
        envs.push(manager.env_ref(
            &format!("CU_BWU_F{i}"),
            vault.as_deref(),
            &id,
            &format!("custom:{field}"),
        ));
    }
    if envs.is_empty() {
        return Err(format!(
            "the vault item '{name}' has no username or password to log in with"
        ));
    }
    Ok(Prepared {
        manager,
        tool,
        vault,
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

/// `bwu run --env … -- chrome-use <child_args>` (or `op run --
/// chrome-use …` with the `op://` references in its environment): the child
/// fills the page.
fn child_command(p: &Prepared, child_args: &[std::ffi::OsString]) -> Result<Command, String> {
    let me =
        std::env::current_exe().map_err(|e| format!("couldn't find chrome-use itself: {e}"))?;
    let mut run = Command::new(&p.tool);
    run.arg("run");
    for e in &p.envs {
        match p.manager {
            Manager::Bitwarden => {
                run.arg("--env").arg(e);
            }
            Manager::OnePassword => {
                let (var, reference) = e.split_once('=').unwrap_or((e, ""));
                run.env(var, reference);
            }
        }
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
    let manager = cmd["manager"]
        .as_str()
        .and_then(Manager::from_name)
        .unwrap_or(Manager::Bitwarden);
    let p = prepare(
        flags,
        manager,
        cmd["item"].as_str(),
        cmd["passkey"].as_bool().unwrap_or(false),
        cmd["noSubmit"].as_bool().unwrap_or(false),
        cmd["totpOnly"].as_bool().unwrap_or(false),
    )?;
    if !flags.json {
        let approval = match p.manager {
            Manager::Bitwarden => "bwu asks for Touch ID unless require_touch_id is off",
            Manager::OnePassword => {
                "the 1Password app asks for approval unless op signs in with a service account"
            }
        };
        eprintln!(
            "logging in to {} as vault item '{}' ({approval})",
            p.host, p.name
        );
    }
    // An item picked without --item is said even in --json mode, where the
    // line above is not: an agent must be able to see which account it was.
    if let Some(why) = &p.why {
        eprintln!(
            "auth login {}: used vault item '{}' ({why}); pass --item for another",
            p.manager.flag(),
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
    Ok(login_exit_code(&response))
}

/// Keep the CLI's per-site approval exit code through the secret child.
fn login_exit_code(response: &crate::connection::Response) -> i32 {
    if !response.success {
        return 1;
    }
    if response
        .data
        .as_ref()
        .is_some_and(|data| data["confirmation_required"] == true && data.get("site").is_some())
    {
        return 3;
    }
    0
}

/// Whether login walls are signed in automatically (#434):
/// `AGENT_BROWSER_AUTO_LOGIN=bwu` (or `1password`), or `"auth": {"autoLogin":
/// "bwu"}` (or `"auth.autoLogin"`, either manager) in
/// `~/.chrome-use/config.json`. A set env var wins either way.
pub fn auto_login_enabled(env: Option<&str>, config: Option<&Value>) -> bool {
    if let Some(v) = env.map(str::trim).filter(|v| !v.is_empty()) {
        return Manager::from_name(v).is_some();
    }
    config.is_some_and(|c| {
        c.get("auth")
            .and_then(|a| a.get("autoLogin"))
            .or_else(|| c.get("auth.autoLogin"))
            .and_then(Value::as_str)
            .is_some_and(|v| Manager::from_name(v).is_some())
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
        .map_err(|e| format!("couldn't run {}: {e}", p.tool.display()))?;
    let response = String::from_utf8_lossy(&output.stdout)
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<crate::connection::Response>(line).ok());
    let flag = p.manager.flag();
    let mut response = response.ok_or_else(|| {
        let hint = match p.manager {
            Manager::Bitwarden => "",
            // `op run` stops before the child when a reference does not resolve.
            Manager::OnePassword if p.envs.iter().any(|e| e.starts_with(OTP)) => {
                " (op could not read the item's values: see its error above; for a code \
                 the item needs a one-time password field)"
            }
            Manager::OnePassword => " (op could not read the item's values: see its error above)",
        };
        format!(
            "auth login {flag} exited with {} without a result{hint}",
            output.status
        )
    })?;
    let pending_site_gate = output.status.code() == Some(3) && login_exit_code(&response) == 3;
    if !output.status.success() && response.success && !pending_site_gate {
        response.success = false;
        response.error = Some(format!("auth login {flag} exited with {}", output.status));
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
        manager: p.manager,
        tool: p.tool.clone(),
        vault: p.vault.clone(),
        envs: vec![p.manager.env_ref(OTP, p.vault.as_deref(), &p.id, "totp")],
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
            .unwrap_or_else(|| format!("auth login {} failed", p.manager.flag())));
    }
    if let Some(data) = response.data.as_ref() {
        if data["confirmation_required"] == true {
            return Err(format!("login awaits confirmation {} for {}. Ask the user to approve in the chrome-use side panel, then run confirm in the same session; do not report a completed login.", data["confirmation_id"].as_str().unwrap_or(""), data["site"].as_str().unwrap_or(&p.host)));
        }
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
    Err(format!(
        "auth login {} did not confirm a sign-in",
        p.manager.flag()
    ))
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
/// (several, or none, is reported, never guessed) from the configured
/// password manager ([`Manager::configured`]), then go back to `returnTo`.
/// Returns `{ok, item, error}` plus `returnedTo` / `returnError`.
pub fn auto_login(flags: &Flags, wall: &Value) -> Value {
    let mut out = json!({ "ok": false, "item": null, "error": null });
    let manager = Manager::configured();
    let p = match prepare(flags, manager, None, false, false, false) {
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
        manager.flag(),
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
        choose(
            cands,
            item,
            &LoginHosts::page(host),
            None,
            Manager::Bitwarden,
        )
        .map(|(c, _)| c)
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
        let (item, why) = choose(&apple, None, &hosts, None, Manager::Bitwarden).unwrap();
        assert_eq!(item["id"], "d1");
        assert!(why.unwrap().contains("idmsa.apple.com"));
        // An item named for the page itself beats one named for its frame.
        let mut both = apple.to_vec();
        both.push(c("a1", "appstoreconnect.apple.com"));
        assert_eq!(
            choose(&both, None, &hosts, None, Manager::Bitwarden)
                .unwrap()
                .0["id"],
            "a1"
        );

        // Nothing named for the page or the frame: ask, the site's item first.
        let plain = [
            c("p1", "Apple ID personal"),
            c("p3", "iCloud"),
            c("p2", "apple.com"),
        ];
        let e = choose(&plain, None, &hosts, None, Manager::Bitwarden).unwrap_err();
        assert!(e.contains("its sign-in frame: idmsa.apple.com"), "{e}");
        let first = e.lines().nth(1).unwrap();
        assert!(
            first.contains("--item p2") && first.contains("named for apple.com"),
            "{e}"
        );
        // Two items named for frames: never guessed.
        let twins = [c("x", "idmsa.apple.com"), c("y", "idmsa.apple.com")];
        assert!(choose(&twins, None, &hosts, None, Manager::Bitwarden).is_err());
    }

    #[test]
    fn choose_uses_the_item_that_signed_in_here_last_time() {
        let apple = [c("p1", "Apple ID personal"), c("d1", "Developer")];
        let hosts = LoginHosts::page("appstoreconnect.apple.com");
        let (item, why) = choose(&apple, None, &hosts, Some("d1"), Manager::Bitwarden).unwrap();
        assert_eq!(item["id"], "d1");
        assert_eq!(why.as_deref(), Some(PICKED_REMEMBERED));
        // --item overrides it; a remembered item no longer listed is ignored.
        assert_eq!(
            choose(&apple, Some("p1"), &hosts, Some("d1"), Manager::Bitwarden)
                .unwrap()
                .0["id"],
            "p1"
        );
        assert!(choose(&apple, None, &hosts, Some("gone"), Manager::Bitwarden).is_err());
        // The only match needs no reason.
        assert_eq!(
            choose(&apple[..1], None, &hosts, Some("d1"), Manager::Bitwarden)
                .unwrap()
                .1,
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
    elif scenario == 'gate':
        print(json.dumps({'success': True, 'data': {'confirmation_required': True, 'confirmation_id': 'gate-fixture', 'site': 'example.test', 'approval': 'user'}, 'error': None}))
        sys.exit(3)
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
            manager: Manager::Bitwarden,
            tool: bwu,
            vault: None,
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
            ("gate", "1", true),
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
            if scenario == "gate" {
                assert_eq!(login_exit_code(&r), 3);
                assert_eq!(r.data.as_ref().unwrap()["confirmation_id"], "gate-fixture");
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

    #[test]
    fn manager_from_env_then_config() {
        let op = json!({"auth": {"passwordManager": "1password"}});
        assert_eq!(Manager::resolve(None, None), Manager::Bitwarden);
        assert_eq!(Manager::resolve(None, Some(&op)), Manager::OnePassword);
        assert_eq!(Manager::resolve(Some("bwu"), Some(&op)), Manager::Bitwarden);
        assert_eq!(Manager::resolve(Some("OP"), None), Manager::OnePassword);
        // A daemon command carries the CLI's environment choice.
        assert_eq!(
            Manager::for_command(&json!({"_passwordManager": "1password"})),
            Manager::OnePassword
        );
        assert_eq!(
            Manager::for_command(&json!({"_passwordManager": "bwu"})),
            Manager::Bitwarden
        );
        // An unknown env value falls through to the config.
        assert_eq!(Manager::resolve(Some("x"), Some(&op)), Manager::OnePassword);
        assert_eq!(
            Manager::resolve(None, Some(&json!({"auth.autoLogin": "1password"}))),
            Manager::OnePassword
        );
        assert_eq!(
            Manager::resolve(None, Some(&json!({"auth": {"autoLogin": "bwu"}}))),
            Manager::Bitwarden
        );
        assert!(auto_login_enabled(Some("1password"), None));
        assert_eq!(
            Manager::OnePassword.env_ref(PASS, Some("v1"), "abc", "password"),
            "CU_BWU_PW=op://v1/abc/password"
        );
        assert_eq!(
            Manager::OnePassword.env_ref(OTP, Some("v1"), "abc", "totp"),
            "CU_BWU_OTP=op://v1/abc/one-time password?attribute=otp"
        );
        assert_eq!(
            Manager::Bitwarden.env_ref(PASS, None, "abc", "password"),
            "CU_BWU_PW=bw:abc#password"
        );
        assert_eq!(Manager::from_name("1pass-use"), None);
    }

    /// `op item list` rows: only Login items with a website on the page's
    /// site, newest first, with their vault id for the `op://` references.
    #[test]
    fn op_candidates_match_the_pages_site() {
        let row = |id: &str, href: Option<&str>, cat: &str, updated: &str| {
            let mut r = json!({
                "id": id, "title": format!("t-{id}"), "category": cat,
                "vault": {"id": "vid", "name": "Agent"}, "updated_at": updated,
            });
            if let Some(h) = href {
                r["urls"] = json!([{ "href": h, "primary": true }]);
            }
            r
        };
        let items = json!([
            row(
                "a",
                Some("https://accounts.example.com/pass"),
                "LOGIN",
                "2026-11-01T00:00:00Z"
            ),
            row("b", Some("example.com"), "LOGIN", "2026-11-03T00:00:00Z"),
            row(
                "c",
                Some("https://example.com.evil.net"),
                "LOGIN",
                "2026-11-04T00:00:00Z"
            ),
            row("d", None, "LOGIN", "2026-11-04T00:00:00Z"),
            row(
                "e",
                Some("https://accounts.example.com"),
                "PASSWORD",
                "2026-11-04T00:00:00Z"
            ),
        ]);
        // A shared host's tenants are different sites.
        let pages = json!([
            row(
                "f",
                Some("https://alice.github.io"),
                "LOGIN",
                "2026-11-04T00:00:00Z"
            ),
            row(
                "g",
                Some("mallory.github.io/login"),
                "LOGIN",
                "2026-11-04T00:00:00Z"
            ),
            row(
                "h",
                Some("http://localhost:3000"),
                "LOGIN",
                "2026-11-04T00:00:00Z"
            ),
        ]);
        let ids = |got: Vec<Value>| -> Vec<String> {
            got.iter()
                .filter_map(|c| c["id"].as_str().map(String::from))
                .collect()
        };
        assert_eq!(ids(op_candidates(&pages, "mallory.github.io")), ["g"]);
        assert_eq!(ids(op_candidates(&pages, "localhost")), ["h"]);
        let got = op_candidates(&items, "Accounts.Example.com");
        let ids: Vec<&str> = got.iter().filter_map(|c| c["id"].as_str()).collect();
        assert_eq!(ids, ["b", "a"]);
        assert_eq!(got[0]["vault"], "vid");
        assert_eq!(got[0]["folder"], "Agent");
        assert_eq!(got[0]["name"], "t-b");
        // No use count from op: the listing says when the item last changed.
        let e = choose(
            &got,
            None,
            &LoginHosts::page("accounts.example.com"),
            None,
            Manager::OnePassword,
        )
        .unwrap_err();
        assert!(
            e.contains("--item b  t-b [Agent]  (updated 2026-11-03T00:00:00Z)"),
            "{e}"
        );
    }

    /// A mock `op`: `op run -- chrome-use …` with the item's `op://`
    /// references (its vault in them) in the environment, never in the
    /// arguments; `op run` puts the values only in the child's environment.
    #[cfg(unix)]
    #[test]
    fn op_run_gets_references_in_its_environment() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let op = dir.path().join("op");
        std::fs::write(&op, r#"#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
assert args[:2] == ['run', '--'], args
assert not any('op://' in a or '--env' == a or '--vault' == a for a in args), args
assert os.environ['CU_BWU_USER'] == 'op://vid/item1/username'
assert os.environ['CU_BWU_PW'] == 'op://vid/item1/password'
assert os.environ.get('CU_BWU_CHILD') == '1'
print(json.dumps({'success': True, 'data': {'signedIn': True, 'filled': ['username', 'password']}, 'error': None}))
"#).unwrap();
        std::fs::set_permissions(&op, std::fs::Permissions::from_mode(0o700)).unwrap();
        let p = Prepared {
            manager: Manager::OnePassword,
            tool: op,
            vault: Some("vid".into()),
            envs: vec![
                Manager::OnePassword.env_ref(USER, Some("vid"), "item1", "username"),
                Manager::OnePassword.env_ref(PASS, Some("vid"), "item1", "password"),
            ],
            origin: "https://example.com".into(),
            host: "example.com".into(),
            id: "item1".into(),
            name: "example".into(),
            custom: Vec::new(),
            steps: None,
            why: None,
            defer_otp: false,
        };
        let r =
            run_login_phases(&p, &["auth".into(), "login".into(), "--1password".into()]).unwrap();
        assert!(r.success, "{:?}", r.error);
        assert_eq!(r.data.unwrap()["signedIn"], true);
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
        assert_eq!(parse(&["--bwu"], "1").unwrap()["manager"], "bwu");
        assert_eq!(
            parse(&["--1password"], "1").unwrap()["manager"],
            "1password"
        );
        assert!(parse(&["--bwu", "--1password"], "1").is_err());
        assert!(parse(&["--1password", "--passkey"], "1").is_err());

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
