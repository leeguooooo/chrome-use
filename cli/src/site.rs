//! Site adapters: turn any website into a structured-data CLI by running a small
//! per-command JS adapter inside your real, logged-in browser tab (it reuses the
//! site's cookies / same-origin fetch / its own webpack modules — the site thinks
//! it's you, because it is).
//!
//! The adapter format follows the **bb-sites** convention: one `.js` file per command, a
//! `/* @meta {...} */` JSON header (name, description, domain, args), then an
//! `async function(args){ ... return {...} }`. chrome-use ships none of those adapters —
//! `chrome-use site update` fetches the community **epiral/bb-sites** pack and the
//! official **leeguooooo/chrome-use-sites** pack at runtime into `~/.chrome-use/sites`
//! (like a package manager pulling dependencies), so the adapters stay the property of
//! their authors. Running an adapter navigates to its `@meta.domain` and `eval`s the
//! function in the site's own logged-in page.

use std::path::PathBuf;

use serde_json::{json, Value};

pub const COMMUNITY_SITES_SOURCE: &str = "epiral/bb-sites";
pub const OFFICIAL_SITES_SOURCE: &str = "leeguooooo/chrome-use-sites";
/// Sync order is precedence: later sources overwrite a shared `name/cmd`, so the
/// official pack stays last to win over the community one.
const DEFAULT_SITES_SOURCES: [&str; 2] = [COMMUNITY_SITES_SOURCE, OFFICIAL_SITES_SOURCE];

/// Built-in adapter sources, synced on every update without user configuration.
pub fn default_sources() -> &'static [&'static str] {
    &DEFAULT_SITES_SOURCES
}

pub fn is_default_source(source: &str) -> bool {
    DEFAULT_SITES_SOURCES.contains(&source.trim())
}

/// `~/.chrome-use/sites` — where synced adapters live.
pub fn sites_dir() -> Option<PathBuf> {
    dirs_home().map(|h| h.join(".chrome-use").join("sites"))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// `~/.chrome-use/sites.sources` — extra adapter sources, one per line (issue
/// #127). Each line is a GitHub `owner/repo`, a `.zip` URL, or a local directory.
/// `#` starts a comment. Lets orgs auto-sync **private/internal** adapter packs
/// (self-hosted Gogs/GitLab, internal tools) that can't live in the public
/// built-in packs, with the same lifecycle as the community and official packs.
pub fn sources_config_path() -> Option<PathBuf> {
    dirs_home().map(|h| h.join(".chrome-use").join("sites.sources"))
}

/// The configured extra sources: env `CHROME_USE_SITES_SOURCES` (comma-separated)
/// first, then the `sites.sources` file — deduped, order preserved. The built-in
/// community and official packs are always synced and are NOT listed here. If an
/// older configuration explicitly contains either built-in source, it is ignored
/// so the repository is not downloaded twice.
pub fn read_sources() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        let s = s.trim();
        if !s.is_empty()
            && !s.starts_with('#')
            && !is_default_source(s)
            && !out.iter().any(|e| e == s)
        {
            out.push(s.to_string());
        }
    };
    if let Ok(env) = std::env::var("CHROME_USE_SITES_SOURCES") {
        for part in env.split(',') {
            push(part);
        }
    }
    if let Some(cfg) = sources_config_path() {
        if let Ok(text) = std::fs::read_to_string(cfg) {
            for line in text.lines() {
                push(line);
            }
        }
    }
    out
}

/// Add a source to `sites.sources` (idempotent). Returns whether it was newly
/// added (false = already present). Env-only sources aren't written here.
pub fn add_source(source: &str) -> Result<bool, String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("site add: empty source".into());
    }
    if is_default_source(source) {
        return Ok(false);
    }
    let path = sources_config_path().ok_or("site add: cannot resolve home dir")?;
    let existing: Vec<String> = std::fs::read_to_string(&path)
        .ok()
        .map(|t| t.lines().map(|l| l.trim().to_string()).collect())
        .unwrap_or_default();
    if existing.iter().any(|l| l == source) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(source);
    text.push('\n');
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(true)
}

/// Remove a source from `sites.sources`. Returns whether a line was removed.
pub fn remove_source(source: &str) -> Result<bool, String> {
    let source = source.trim();
    if is_default_source(source) {
        return Err(format!(
            "site remove: `{source}` is a built-in default source and cannot be removed"
        ));
    }
    let path = sources_config_path().ok_or("site remove: cannot resolve home dir")?;
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };
    let mut removed = false;
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| {
            if l.trim() == source {
                removed = true;
                false
            } else {
                true
            }
        })
        .collect();
    if removed {
        let mut out = kept.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        std::fs::write(&path, out).map_err(|e| e.to_string())?;
    }
    Ok(removed)
}

/// Parsed adapter: its `@meta` JSON and the raw `async function(args){...}` source.
pub struct Adapter {
    pub meta: Value,
    pub func_src: String,
    /// The adapter's declared `args` keys in DECLARATION order. Parsed from the
    /// raw @meta text because `serde_json` sorts object keys alphabetically, which
    /// would otherwise scramble positional-arg mapping for multi-arg adapters.
    pub arg_order: Vec<String>,
    /// `name/cmd` the adapter was loaded as, for usage hints.
    pub spec: String,
}

impl Adapter {
    pub fn domain(&self) -> Option<&str> {
        self.meta.get("domain").and_then(|v| v.as_str())
    }
}

/// Load `<sites>/<name>/<cmd>.js`, splitting the `/* @meta {...} */` header from
/// the function body. `spec` is `name/cmd`.
pub fn load_adapter(spec: &str) -> Result<Adapter, String> {
    let (name, cmd) = spec
        .split_once('/')
        .ok_or_else(|| format!("site: expected <name>/<command>, got `{spec}`"))?;
    if name.is_empty()
        || cmd.is_empty()
        || name.contains("..")
        || cmd.contains("..")
        || name.contains('/')
        || cmd.contains('/')
    {
        return Err(format!("site: invalid adapter spec `{spec}`"));
    }
    let dir = sites_dir().ok_or("site: cannot resolve home dir")?;
    let path = dir.join(name).join(format!("{cmd}.js"));
    if !path.exists() {
        return Err(format!(
            "site: adapter `{spec}` not found. Run `chrome-use site update` to sync adapters, \
             or `chrome-use site list` to see what's installed."
        ));
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("site: read {spec}: {e}"))?;
    parse_adapter(&raw, spec)
}

/// Split the `@meta` JSON block and the function source from an adapter file.
pub fn parse_adapter(raw: &str, spec: &str) -> Result<Adapter, String> {
    let start = raw
        .find("@meta")
        .and_then(|i| raw[i..].find('{').map(|j| i + j))
        .ok_or_else(|| format!("site: {spec} missing /* @meta {{...}} */ header"))?;
    // Find the matching close brace for the @meta object (brace-count, string-aware).
    let bytes = raw.as_bytes();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut end = None;
    for (k, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(k + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end.ok_or_else(|| format!("site: {spec} @meta header has no closing brace"))?;
    let meta: Value = serde_json::from_str(&raw[start..end])
        .map_err(|e| format!("site: {spec} @meta is not valid JSON: {e}"))?;
    // The function is everything after the meta comment's closing `*/`.
    let after = raw[end..].find("*/").map(|i| end + i + 2).unwrap_or(end);
    let func_src = raw[after..].trim().to_string();
    if func_src.is_empty() {
        return Err(format!("site: {spec} has no function body after @meta"));
    }
    let arg_order = arg_order_from_meta(&raw[start..end]);
    Ok(Adapter {
        meta,
        func_src,
        arg_order,
        spec: spec.to_string(),
    })
}

/// Extract the `args` object's keys in DECLARATION order from the raw @meta JSON
/// text (serde sorts them, losing order). Brace/string-aware: finds the `"args"`
/// value object and collects only its top-level keys.
fn arg_order_from_meta(meta_json: &str) -> Vec<String> {
    let bytes = meta_json.as_bytes();
    // Locate the `"args"` key, then the `{` that opens its value object.
    let Some(args_pos) = meta_json.find("\"args\"") else {
        return Vec::new();
    };
    let Some(brace_off) = meta_json[args_pos..].find('{') else {
        return Vec::new();
    };
    let open = args_pos + brace_off;
    let mut keys = Vec::new();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut cur = String::new();
    let mut last_str: Option<String> = None;
    for &b in bytes.iter().skip(open) {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
                last_str = Some(std::mem::take(&mut cur));
            } else {
                cur.push(b as char);
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    break; // end of the args object
                }
            }
            // A `:` at depth 1 means the preceding string was a key of `args`.
            b':' if depth == 1 => {
                if let Some(k) = last_str.take() {
                    keys.push(k);
                }
            }
            _ => {}
        }
    }
    keys
}

/// Load a family's shared `_helper.js` if present. It's plain function
/// declarations (no `@meta`) that adapters call as if in scope — e.g.
/// `twitter/_helper` defines `findGraphQLQueryId`, which every `twitter/*`
/// adapter uses. bb-browser auto-loads it before each adapter; so must we (#99).
pub fn load_family_helper(family: &str) -> Option<String> {
    if family.is_empty() || family.contains("..") || family.contains('/') {
        return None;
    }
    let dir = sites_dir()?;
    let src = std::fs::read_to_string(dir.join(family).join("_helper.js")).ok()?;
    if src.trim().is_empty() {
        None
    } else {
        Some(src)
    }
}

/// Build the JS to eval. The adapter's `async function(args)` returns a promise;
/// chrome-use's eval awaits it. When the family ships a `_helper` module, define
/// it in an enclosing scope the adapter expression closes over, so helper calls
/// resolve instead of throwing `ReferenceError: findGraphQLQueryId is not
/// defined` (#99).
///
/// Around the call (#359): a required arg the caller left out is filled from the
/// current page when its description documents a URL template for it
/// (`linkedin.com/in/<username>`) and the tab is on a matching page, so
/// `site linkedin/profile` run on someone's profile just works. An adapter's
/// bare `Missing argument: x` error also gets a `hint` saying how to pass args.
#[cfg(test)]
pub fn build_eval(adapter: &Adapter, args: &Value, helper_src: Option<&str>) -> String {
    let args_json = serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string());
    format!(
        "({})({args_json})",
        build_runner_fn(adapter, args, helper_src)
    )
}

/// The `async (__args) => {...}` expression [`build_eval`] calls: URL-template
/// inference, the adapter call, and the missing-arg hint.
fn build_runner_fn(adapter: &Adapter, args: &Value, helper_src: Option<&str>) -> String {
    let invoke = match helper_src {
        Some(h) if !h.trim().is_empty() => format!(
            "(() => {{\n{h}\n;\nreturn ({func})(__args);\n}})()",
            h = h,
            func = adapter.func_src,
        ),
        _ => format!("({})(__args)", adapter.func_src),
    };
    let infer: Vec<Value> = url_inferences(adapter, args)
        .into_iter()
        .map(|t| json!({ "arg": t.arg, "host": t.host, "re": t.path_regex }))
        .collect();
    let infer_json = serde_json::to_string(&infer).unwrap_or_else(|_| "[]".to_string());
    let hint_json = serde_json::to_string(&missing_arg_hint(adapter)).unwrap_or_default();
    let login_js = login_signal_js();
    format!(
        "(async (__args) => {{\n\
         for (const t of {infer_json}) {{\n\
         if (__args[t.arg]) continue;\n\
         const h = location.hostname;\n\
         if (t.host && h !== t.host && !h.endsWith('.' + t.host)) continue;\n\
         const m = location.pathname.match(new RegExp(t.re));\n\
         if (m) {{ try {{ __args[t.arg] = decodeURIComponent(m[1]); }} catch (_) {{ __args[t.arg] = m[1]; }} }}\n\
         }}\n\
         {login_js}\n\
         let __r;\n\
         try {{ __r = await {invoke}; }} catch (e) {{\n\
         const ev = __cuAuthEvidence();\n\
         if (!ev) throw e;\n\
         return {{ error: 'login_required', loginRequired: true, loginEvidence: ev, \
         adapterError: String((e && e.message) || e) }};\n\
         }}\n\
         if (__r && typeof __r === 'object' && typeof __r.error === 'string' \
         && /^missing arg/i.test(__r.error) && !__r.hint) __r.hint = {hint_json};\n\
         return __cuLoginNormalize(__r);\n\
         }})"
    )
}

/// Error codes an adapter may return to say "the site says you are not signed
/// in" (#479), matched case-insensitively at the start of `error`. The
/// documented spelling is `loginRequired: true`; these cover adapters written
/// before that existed (`not_logged_in`, `Not logged in`).
pub const LOGIN_ERROR_RE: &str = r"^\s*(login[ _-]?required|not[ _-]?logged[ _-]?in)\b";

/// The page-side half of the adapter login signal (#479), spliced into the
/// runner ahead of the adapter call:
///
/// - `fetch` is shadowed in the adapter's scope so the runtime sees, without
///   the adapter's help, a response with HTTP 401 or one that was redirected
///   to a sign-in URL (same rules as the daemon's login-wall check).
/// - `__cuLoginNormalize(result)` marks a result `loginRequired: true` when the
///   adapter said so (`loginRequired: true`, or an error code matching
///   [`LOGIN_ERROR_RE`]), or when the adapter failed and one of those
///   responses was seen. Evidence goes in `loginEvidence`: `{source:
///   "adapter"}`, `{source: "http401", url}` or `{source: "redirect", url}`.
///   A successful result is never touched.
fn login_signal_js() -> String {
    let segs = serde_json::to_string(crate::native::login_wall::LOGIN_SEGMENTS).unwrap_or_default();
    let hosts =
        serde_json::to_string(crate::native::login_wall::LOGIN_HOST_LABELS).unwrap_or_default();
    let re = serde_json::to_string(LOGIN_ERROR_RE).unwrap_or_default();
    format!(
        "const __cuAuth = {{ s401: '', login: '' }};\n\
         const __cuLoginish = (u) => {{ try {{\n\
         const x = new URL(u, location.href); const host = x.hostname.toLowerCase();\n\
         if (host.includes('.') && {hosts}.includes(host.split('.')[0])) return true;\n\
         return x.pathname.split('/').some((s) => {{ const t = s.toLowerCase().split('.')[0];\n\
         return {segs}.includes(t) || ['login', 'signin', 'logon'].some((w) => t.length > w.length + 3 && t.endsWith(w)); }});\n\
         }} catch (_) {{ return false; }} }};\n\
         const __cuFetch = (typeof window !== 'undefined' && window.fetch) ? window.fetch.bind(window) : undefined;\n\
         const fetch = async (...a) => {{\n\
         const res = await __cuFetch(...a);\n\
         try {{\n\
         const u = res.url || String((a[0] && a[0].url) || a[0]);\n\
         if (res.status === 401 && !__cuAuth.s401) __cuAuth.s401 = u;\n\
         else if (res.redirected && !__cuAuth.login && __cuLoginish(res.url)) __cuAuth.login = res.url;\n\
         }} catch (_) {{}}\n\
         return res;\n\
         }};\n\
         const __cuAuthEvidence = () => __cuAuth.login ? {{ source: 'redirect', url: __cuAuth.login }}\n\
         : __cuAuth.s401 ? {{ source: 'http401', url: __cuAuth.s401 }} : null;\n\
         const __cuLoginNormalize = (r) => {{\n\
         if (!r || typeof r !== 'object' || Array.isArray(r)) return r;\n\
         const err = typeof r.error === 'string' ? r.error : '';\n\
         if (r.loginRequired === true || (err && new RegExp({re}, 'i').test(err))) {{\n\
         r.loginRequired = true; if (!err) r.error = 'login_required';\n\
         if (!r.loginEvidence) r.loginEvidence = {{ source: 'adapter' }};\n\
         return r;\n\
         }}\n\
         const ev = (err || r.success === false) ? __cuAuthEvidence() : null;\n\
         if (ev) {{ r.loginRequired = true; r.loginEvidence = ev; }}\n\
         return r;\n\
         }};"
    )
}

/// How a site adapter run ended up behind a login (#479), from its result
/// and, when the daemon also saw the tab land on a sign-in page, that wall.
/// `None` when nothing says the run failed for want of a login.
///
/// The returned `loginWall` object carries the generic wall's fields (`url`,
/// `returnTo`, `host`, `hint`) plus `source: "site"`, `spec`, `loginUrl`,
/// `evidence` and `rerunnable` (whether the command may be run again by itself
/// after signing in: a read, or an adapter that said explicitly it was not
/// signed in, which means it wrote nothing).
pub fn site_login_wall(
    result: &Value,
    page_wall: Option<&Value>,
    origin: &str,
    domain: &str,
    spec: &str,
    read_only: bool,
) -> Option<Value> {
    let obj = result.as_object()?;
    let err = obj.get("error").and_then(|v| v.as_str()).unwrap_or("");
    let failed = !err.is_empty() || obj.get("success").and_then(|v| v.as_bool()) == Some(false);
    let said = obj.get("loginRequired").and_then(|v| v.as_bool()) == Some(true)
        || regex_lite::Regex::new(&format!("(?i){LOGIN_ERROR_RE}"))
            .map(|re| re.is_match(err))
            .unwrap_or(false);
    let page_url = page_wall
        .and_then(|w| w.get("url"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let evidence = if said {
        obj.get("loginEvidence")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or_else(|| json!({ "source": "adapter" }))
    } else if failed && page_url.is_some() {
        json!({ "source": "page", "url": page_url })
    } else {
        return None;
    };
    let base = url::Url::parse(origin)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .or_else(|| url::Url::parse(&format!("https://{domain}/")).ok());
    let absolute = |s: &str| -> Option<String> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        match &base {
            Some(b) => b.join(s).ok().map(|u| u.to_string()),
            None => url::Url::parse(s).ok().map(|u| u.to_string()),
        }
    };
    let source = evidence
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("adapter");
    let login_url = obj
        .get("loginUrl")
        .and_then(|v| v.as_str())
        .and_then(absolute)
        .or_else(|| {
            (source == "redirect" || source == "page")
                .then(|| evidence.get("url").and_then(|v| v.as_str()))
                .flatten()
                .and_then(absolute)
        })
        .or_else(|| page_url.clone());
    let host = base
        .as_ref()
        .and_then(|u| u.host_str().map(str::to_string))
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| domain.to_string());
    // Back to the page the adapter ran on, unless that is the sign-in page,
    // or a sign-out page that would end the new session at once.
    let return_to = base
        .as_ref()
        .filter(|u| !crate::native::login_wall::login_ish(u) && !logout_ish(u))
        .map(|u| u.to_string());
    let rerun = read_only || source == "adapter";
    let hint = site_login_hint(&host, spec, login_url.as_deref(), err, source);
    Some(json!({
        "source": "site",
        "spec": spec,
        "url": origin,
        "returnTo": return_to,
        "host": host,
        "loginUrl": login_url,
        "evidence": evidence,
        "rerunnable": rerun,
        "hint": hint,
    }))
}

/// Whether a URL looks like a sign-out page (`/logout`, `/user-logout.html`,
/// `/auth/sign_out`): never a page to go back to after signing in.
fn logout_ish(u: &url::Url) -> bool {
    u.path_segments().into_iter().flatten().any(|seg| {
        let stem = seg.split('.').next().unwrap_or("").to_ascii_lowercase();
        let squashed: String = stem.chars().filter(|c| !matches!(c, '-' | '_')).collect();
        ["logout", "signout", "logoff"]
            .iter()
            .any(|w| squashed == *w || squashed.ends_with(w))
    })
}

/// The stderr line and error message for a site adapter's login wall.
pub fn site_login_hint(
    host: &str,
    spec: &str,
    login_url: Option<&str>,
    adapter_error: &str,
    source: &str,
) -> String {
    let why = match source {
        "http401" => "got HTTP 401".to_string(),
        "redirect" => "was redirected to a sign-in page".to_string(),
        "page" => "left the tab on a sign-in page".to_string(),
        _ if !adapter_error.is_empty() => format!("reported {adapter_error}"),
        _ => "reported that it is not signed in".to_string(),
    };
    let how = match login_url {
        Some(u) => format!("sign in with `chrome-use open {u}` and `chrome-use auth login --bwu`"),
        None => "open its sign-in page and sign in with `chrome-use auth login --bwu`".to_string(),
    };
    format!(
        "login wall: {host} is not signed in (site {spec} {why}); {how} (add --item <name> if \
         the vault has several logins for it), then run the command again. Ask the user only if \
         no vault item matches, 2FA needs them, or login fails"
    )
}

/// Statuses that mean "run the same command again" under `--until-done`, the
/// convention the resumable publish adapters already return.
pub const DEFAULT_RETRY_STATUSES: &[&str] = &["incomplete", "uploading"];

/// Start the adapter as a background run in the page and return at once.
///
/// One relay CDP command is capped at ~8s by the extension, so a publish flow
/// that waits on an upload cannot be a single awaited `eval`. The run lives on
/// a non-enumerable `window[run_key]`; the daemon polls it with
/// [`poll_script`] (each poll is a quick read) until it settles or the
/// command's timeout passes (#366).
///
/// Adapters see three extras on `args`, all non-enumerable so an adapter that
/// serializes its args is unaffected:
/// - `args.budgetMs`: time this run may take, for adapters that pace themselves.
/// - `args.progress(msg)`: a progress line, reported with the result.
/// - for each `"type": "file"` arg, `args.<name>` is `{path, name, size,
///   setOn(selector?)}`. `await setOn(sel)` asks the daemon to put that local
///   file on the file input `sel` (default: the arg's `"input"`), the same way
///   `chrome-use upload` does (#364). The page never sees the file system: it
///   can only name a selector for a file the caller passed.
pub fn build_start(
    adapter: &Adapter,
    args: &Value,
    helper_src: Option<&str>,
    run_key: &str,
    files: &Value,
) -> String {
    let runner = build_runner_fn(adapter, args, helper_src);
    let args_json = serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string());
    let files_json = serde_json::to_string(files).unwrap_or_else(|_| "{}".to_string());
    let key_json = serde_json::to_string(run_key).unwrap_or_default();
    format!(
        "(() => {{\n\
         const K = {key_json};\n\
         const run = {{ done: false, result: undefined, error: undefined, progress: [], requests: [], waiters: {{}}, seq: 0 }};\n\
         Object.defineProperty(window, K, {{ value: run, configurable: true, enumerable: false, writable: true }});\n\
         const rpc = (kind, payload) => new Promise((resolve, reject) => {{\n\
         const id = ++run.seq; run.waiters[id] = {{ resolve, reject }};\n\
         run.requests.push(Object.assign({{ id, kind }}, payload));\n\
         }});\n\
         const a = {args_json};\n\
         const hide = (name, value) => Object.defineProperty(a, name, {{ value, configurable: true, enumerable: false, writable: true }});\n\
         for (const [k, f] of Object.entries({files_json})) {{\n\
         a[k] = {{ path: f.path, name: f.name, size: f.size,\n\
         setOn: (selector) => {{\n\
         const sel = selector || f.input;\n\
         if (!sel) return Promise.reject(new Error('setOn: no selector, and @meta.args.' + k + ' declares no \"input\"'));\n\
         return rpc('upload', {{ arg: k, selector: sel }});\n\
         }} }};\n\
         a[k].toString = () => f.path;\n\
         }}\n\
         hide('budgetMs', {BUDGET_PLACEHOLDER});\n\
         hide('progress', (m) => {{ run.progress.push(String(m)); }});\n\
         Promise.resolve().then(() => ({runner})(a)).then(\n\
         (r) => {{ run.result = r; run.done = true; }},\n\
         (e) => {{ run.error = String((e && e.stack) || e); run.done = true; }});\n\
         return 'started';\n\
         }})()"
    )
}

/// Stands in for `args.budgetMs` in [`build_start`]'s script; the daemon puts
/// the time left for each attempt there.
pub const BUDGET_PLACEHOLDER: &str = "__CU_SITE_BUDGET_MS__";

/// Read and drain a background run: `{state: running|done|lost, progress,
/// requests, result?, error?}`. `lost` means the page navigated (or reloaded)
/// and took the run with it. A finished run is removed from `window`.
/// `eval --background`: start a plain expression the way `build_start` starts
/// an adapter (same run object, so `poll_script` and the daemon's site runner
/// handle it unchanged), without adapter args, files or helpers.
pub fn build_expr_start(expr: &str, run_key: &str) -> String {
    let key_json = serde_json::to_string(run_key).unwrap_or_default();
    format!(
        "(() => {{\n\
         const K = {key_json};\n\
         const run = {{ done: false, result: undefined, error: undefined, progress: [], requests: [], waiters: {{}}, seq: 0 }};\n\
         Object.defineProperty(window, K, {{ value: run, configurable: true, enumerable: false, writable: true }});\n\
         Promise.resolve().then(() => ({expr}\n)).then(\n\
         (r) => {{ run.result = r; run.done = true; }},\n\
         (e) => {{ run.error = String((e && e.stack) || e); run.done = true; }});\n\
         return 'started';\n\
         }})()"
    )
}

pub fn poll_script(run_key: &str) -> String {
    let key_json = serde_json::to_string(run_key).unwrap_or_default();
    format!(
        "(() => {{\n\
         const r = window[{key_json}];\n\
         if (!r) return {{ state: 'lost' }};\n\
         const out = {{ state: r.done ? 'done' : 'running', progress: r.progress.splice(0), requests: r.requests.splice(0) }};\n\
         if (r.done) {{ out.result = r.result; out.error = r.error; delete window[{key_json}]; }}\n\
         return out;\n\
         }})()"
    )
}

/// Resolve (or reject) the page-side promise of request `id`.
pub fn settle_script(run_key: &str, id: u64, ok: bool, value: &Value) -> String {
    let key_json = serde_json::to_string(run_key).unwrap_or_default();
    let value_json = serde_json::to_string(value).unwrap_or_else(|_| "null".to_string());
    let how = if ok {
        format!("w.resolve({value_json})")
    } else {
        format!("w.reject(new Error({value_json}))")
    };
    format!(
        "(() => {{ const r = window[{key_json}]; const w = r && r.waiters[{id}]; \
         if (!w) return false; delete r.waiters[{id}]; {how}; return true; }})()"
    )
}

/// Runner options for one `site` invocation, taken out of the adapter args.
#[derive(Debug, Default, PartialEq)]
pub struct RunOptions {
    /// `--timeout <secs|Ns|Nm>`; falls back to `@meta.timeout` (seconds).
    pub timeout_ms: Option<u64>,
    /// `--until-done`: rerun on a retry status or a lost run until done.
    pub until_done: bool,
}

/// Default time for a run when neither `--timeout` nor `@meta.timeout` says.
pub const DEFAULT_RUN_TIMEOUT_MS: u64 = 120_000;
/// Default overall time for `--until-done`.
pub const DEFAULT_UNTIL_DONE_TIMEOUT_MS: u64 = 600_000;

impl RunOptions {
    /// The command's total time budget.
    pub fn effective_timeout_ms(&self, adapter: &Adapter) -> u64 {
        self.timeout_ms
            .or_else(|| {
                adapter
                    .meta
                    .get("timeout")
                    .and_then(|v| v.as_f64())
                    .filter(|t| *t > 0.0)
                    .map(|t| (t * 1000.0) as u64)
            })
            .unwrap_or(if self.until_done {
                DEFAULT_UNTIL_DONE_TIMEOUT_MS
            } else {
                DEFAULT_RUN_TIMEOUT_MS
            })
    }
}

/// `300`, `300s`, `5m`, `1h`, `1500ms` → milliseconds.
pub fn parse_duration_ms(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix("ms") {
        (n, 1.0)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1000.0)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60_000.0)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3_600_000.0)
    } else {
        (s, 1000.0)
    };
    let v: f64 = num.trim().parse().ok()?;
    (v > 0.0 && v.is_finite()).then_some((v * mult) as u64)
}

fn declared_arg<'a>(adapter: &'a Adapter, key: &str) -> Option<&'a Value> {
    adapter.meta.get("args").and_then(|a| a.get(key))
}

/// Whether `--name` is a runner flag here. An adapter that declares an arg of
/// the same name keeps it.
pub fn is_runner_flag(adapter: &Adapter, name: &str) -> bool {
    matches!(name, "until-done" | "timeout") && declared_arg(adapter, name).is_none()
}

/// Resolve one arg value (#365):
/// - `@-` reads stdin; `@path` reads the file when it exists
/// - `@x` that names no file stays literal (`--user @jack`), unless it looks
///   like a path (has a `/` or a file extension): a typo there must not post
///   the literal string `@post.md` as an article body
/// - `\@...` passes a literal leading `@`
pub fn resolve_arg_value(
    raw: &str,
    read_stdin: &mut dyn FnMut() -> Result<String, String>,
) -> Result<String, String> {
    if let Some(lit) = raw.strip_prefix("\\@") {
        return Ok(format!("@{lit}"));
    }
    let Some(path) = raw.strip_prefix('@') else {
        return Ok(raw.to_string());
    };
    if path == "-" {
        return read_stdin();
    }
    if path.is_empty() {
        return Ok(raw.to_string());
    }
    let p = std::path::Path::new(path);
    if p.is_file() {
        return std::fs::read_to_string(p).map_err(|e| format!("site: read {path}: {e}"));
    }
    let looks_like_path = path.contains('/')
        || path.contains('\\')
        || p.extension().is_some_and(|e| e.len() <= 5 && !e.is_empty());
    if looks_like_path {
        return Err(format!(
            "site: `{raw}` names no readable file. Fix the path, or write \\{raw} to pass the text literally."
        ));
    }
    Ok(raw.to_string())
}

/// Everything a `site` command needs beyond the adapter source.
#[derive(Debug)]
pub struct Invocation {
    pub args: Value,
    /// `{arg: {path, name, size, input}}` for each `"type": "file"` arg given.
    pub files: Value,
    pub options: RunOptions,
}

/// Map CLI args onto the adapter: positionals in declaration order, `--key
/// value`, `--key @file` / `@-` and `--key-file path` (#365), `"type": "file"`
/// args checked and made absolute (#364), and the runner flags (#366).
/// `named` excludes the value-less `--until-done`, which the caller has already
/// recorded in `until_done`.
pub fn prepare_invocation(
    adapter: &Adapter,
    positional: &[String],
    named: &[(String, String)],
    until_done: bool,
    read_stdin: &mut dyn FnMut() -> Result<String, String>,
) -> Result<Invocation, String> {
    let mut options = RunOptions {
        until_done,
        ..Default::default()
    };
    let mut resolved: Vec<(String, String)> = Vec::new();
    for (k, v) in named {
        if k == "timeout" && is_runner_flag(adapter, k) {
            options.timeout_ms = Some(parse_duration_ms(v).ok_or_else(|| {
                format!("site: --timeout expects a duration like 300, 90s or 5m, got `{v}`")
            })?);
            continue;
        }
        if let Some(base) = k.strip_suffix("-file") {
            if declared_arg(adapter, k).is_none() && declared_arg(adapter, base).is_some() {
                let text = if v == "-" {
                    read_stdin()?
                } else {
                    std::fs::read_to_string(v).map_err(|e| format!("site: --{k} {v}: {e}"))?
                };
                resolved.push((base.to_string(), text));
                continue;
            }
        }
        resolved.push((k.clone(), resolve_arg_value(v, read_stdin)?));
    }
    let mut pos: Vec<String> = Vec::new();
    for v in positional {
        pos.push(resolve_arg_value(v, read_stdin)?);
    }
    let mut args = map_args(adapter, &pos, &resolved);

    let mut files = serde_json::Map::new();
    if let (Some(decl), Some(obj)) = (
        adapter.meta.get("args").and_then(|a| a.as_object()),
        args.as_object_mut(),
    ) {
        for (k, spec) in decl {
            if spec.get("type").and_then(|t| t.as_str()) != Some("file") {
                continue;
            }
            let Some(given) = obj.get(k).and_then(|v| v.as_str()).map(str::to_string) else {
                continue;
            };
            let path =
                std::fs::canonicalize(&given).map_err(|e| format!("site: --{k} {given}: {e}"))?;
            let meta = std::fs::metadata(&path).map_err(|e| format!("site: --{k} {given}: {e}"))?;
            if !meta.is_file() {
                return Err(format!("site: --{k} {given} is not a file"));
            }
            let abs = path.to_string_lossy().to_string();
            files.insert(
                k.clone(),
                json!({
                    "path": abs,
                    "name": path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                    "size": meta.len(),
                    "input": spec.get("input").and_then(|v| v.as_str()),
                }),
            );
            obj.insert(k.clone(), Value::String(abs));
        }
    }
    Ok(Invocation {
        args,
        files: Value::Object(files),
        options,
    })
}

/// Retry statuses for `--until-done`: `@meta.retryStatuses`, else the default.
pub fn retry_statuses(adapter: &Adapter) -> Vec<String> {
    adapter
        .meta
        .get("retryStatuses")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_else(|| {
            DEFAULT_RETRY_STATUSES
                .iter()
                .map(|s| s.to_string())
                .collect()
        })
}

/// A URL template an adapter documents in an arg's description, e.g.
/// `"LinkedIn username (from URL linkedin.com/in/<username>)"`.
#[derive(Debug, PartialEq)]
pub struct UrlInference {
    pub arg: String,
    /// Host the template names (`linkedin.com`, `www.` stripped); `None` for a
    /// bare path (`/design/p/<id>`), which matches on whatever host the tab is on.
    pub host: Option<String>,
    /// JS regex source matched against `location.pathname`; group 1 is the value.
    pub path_regex: String,
}

/// Templates for the adapter's *required* args the caller did not supply.
pub fn url_inferences(adapter: &Adapter, args: &Value) -> Vec<UrlInference> {
    let Some(decl) = adapter.meta.get("args").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    let supplied = |k: &str| {
        args.get(k)
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty())
    };
    adapter
        .arg_order
        .iter()
        .filter_map(|key| {
            let spec = decl.get(key)?;
            if spec.get("required").and_then(|v| v.as_bool()) != Some(true) || supplied(key) {
                return None;
            }
            let desc = spec.get("description").and_then(|v| v.as_str())?;
            parse_url_template(key, desc)
        })
        .collect()
}

/// Find the URL-ish token containing `<arg>` in `desc` and turn its path into a
/// regex. Tokens end at whitespace, quotes, brackets and CJK punctuation.
fn parse_url_template(arg: &str, desc: &str) -> Option<UrlInference> {
    let placeholder = format!("<{arg}>");
    desc.match_indices(&placeholder)
        .find_map(|(at, _)| url_template_at(arg, &placeholder, desc, at))
}

fn url_template_at(arg: &str, placeholder: &str, desc: &str, at: usize) -> Option<UrlInference> {
    let is_break = |c: char| c.is_whitespace() || "()[]{}'\"`,;，；（）、".contains(c);
    let start = desc[..at]
        .char_indices()
        .rev()
        .find(|&(_, c)| is_break(c))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let end = desc[at..]
        .char_indices()
        .find(|&(_, c)| is_break(c))
        .map(|(i, _)| at + i)
        .unwrap_or(desc.len());
    let mut token = &desc[start..end];
    for scheme in ["https://", "http://"] {
        token = token.strip_prefix(scheme).unwrap_or(token);
    }
    let (host, path) = match token.find('/')? {
        0 => (None, token),
        i => {
            let host = &token[..i];
            // A host must look like one (`a.b`), or this is not a URL template.
            if !host.contains('.') {
                return None;
            }
            let host = host.strip_prefix("www.").unwrap_or(host);
            (Some(host.to_string()), &token[i..])
        }
    };
    // Only the path through the placeholder matters.
    let upto = path.find(placeholder)? + placeholder.len();
    let mut rest = &path[..upto];
    let mut re = String::from("^");
    while let Some(open) = rest.find('<') {
        re.push_str(&regex_escape(&rest[..open]));
        let close = open + rest[open..].find('>')?;
        let name = &rest[open + 1..close];
        re.push_str(if name == arg { "([^/?#]+)" } else { "[^/?#]+" });
        rest = &rest[close + 1..];
    }
    re.push_str(&regex_escape(rest));
    re.push_str("(?:[/?#]|$)");
    Some(UrlInference {
        arg: arg.to_string(),
        host,
        path_regex: re,
    })
}

fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.^$|?*+()[]{}/".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// How to call the adapter, attached to a `Missing argument` error: required
/// args as positionals in declaration order, and where the full list lives.
pub fn missing_arg_hint(adapter: &Adapter) -> String {
    let decl = adapter.meta.get("args").and_then(|v| v.as_object());
    let required: Vec<&str> = adapter
        .arg_order
        .iter()
        .filter(|k| {
            decl.and_then(|d| d.get(k.as_str()))
                .and_then(|s| s.get("required"))
                .and_then(|v| v.as_bool())
                == Some(true)
        })
        .map(|k| k.as_str())
        .collect();
    let spec = &adapter.spec;
    let mut usage = format!("chrome-use site {spec}");
    for k in &required {
        usage.push_str(&format!(" <{k}>"));
    }
    let named = required
        .first()
        .map(|k| format!(" (or --{k} <value>)"))
        .unwrap_or_default();
    format!("Usage: {usage}{named}. All args: chrome-use site info {spec}")
}

/// List installed adapters as `name/cmd` strings (sorted).
/// Whether a `.js` file stem in a pack directory is a runnable adapter.
/// `_`-prefixed files are loader internals (family helpers like `_helper`,
/// injected automatically), and `*.test.js` are the pack's own tests — neither
/// is invokable as `site name/<stem>`, so listing them sends an agent to a
/// command that does nothing (#302).
pub fn is_runnable_adapter_stem(stem: &str) -> bool {
    !stem.starts_with('_') && !stem.ends_with(".test")
}

pub fn list_adapters() -> Result<Vec<String>, String> {
    let dir = sites_dir().ok_or("site: cannot resolve home dir")?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for site in std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .flatten()
    {
        if !site.path().is_dir() {
            continue;
        }
        let name = site.file_name().to_string_lossy().to_string();
        for cmd in std::fs::read_dir(site.path())
            .map_err(|e| e.to_string())?
            .flatten()
        {
            let p = cmd.path();
            if p.extension().and_then(|e| e.to_str()) == Some("js") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    if is_runnable_adapter_stem(stem) {
                        out.push(format!("{name}/{stem}"));
                    }
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Sync both built-in packs (`epiral/bb-sites` community and
/// `leeguooooo/chrome-use-sites` official) **plus** any configured extra sources
/// into `~/.chrome-use/sites`. Sources are applied in order, so on a pack-dir name
/// collision the later source wins (with a warning). Both built-in sources are
/// required; extra sources are best-effort, so one failing (offline/private-auth)
/// doesn't abort the others or the built-in sync. Returns the total adapter count.
pub async fn update() -> Result<usize, String> {
    let dir = sites_dir().ok_or("site: cannot resolve home dir")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let client = reqwest::Client::builder()
        .user_agent("chrome-use")
        .build()
        .map_err(|e| e.to_string())?;

    // 1) Built-in packs — both are first-class defaults. A hard failure preserves
    // the original contract: an incomplete first sync must not look successful.
    // Which source each adapter came from. Sources overlay in order, so a later
    // one wins a shared `name/cmd` both on disk and here — the official pack is
    // synced after the community one so ours takes precedence (see
    // DEFAULT_SITES_SOURCES), and a configured extra source overrides both.
    let mut provenance: std::collections::BTreeMap<String, String> = Default::default();
    for source in default_sources() {
        let written = sync_source(&client, source, None, &dir)
            .await
            .map_err(|e| format!("site update: default source `{source}` failed: {e}"))?;
        for spec in written {
            provenance.insert(spec, source.to_string());
        }
    }

    // 2) Extra sources (private/org packs). Best-effort, overlaid on top. Built-in
    // names are filtered by read_sources() for compatibility with old config files.
    let token = sources_token();
    for source in read_sources() {
        match sync_source(&client, &source, token.as_deref(), &dir).await {
            Ok(written) => {
                for spec in written {
                    provenance.insert(spec, source.clone());
                }
            }
            Err(e) => eprintln!("site update: source `{source}` skipped: {e}"),
        }
    }

    // Total adapter count after all overlays.
    let count = list_adapters().map(|l| l.len()).unwrap_or(0);
    // Build the domain→adapters index and stamp the sync time so navigation can
    // suggest adapters (auto-trigger) and `needs_refresh` can pace re-syncs.
    if let Ok(json) = serde_json::to_string(&provenance) {
        let _ = std::fs::write(dir.join(".provenance.json"), json);
    }
    // OpenCLI's adapters run from its own package (see opencli.rs); a failure
    // here never fails the update.
    match tokio::task::spawn_blocking(crate::opencli::sync).await {
        Ok(Err(e)) => eprintln!("site update: {e}"),
        Err(e) => eprintln!("site update: opencli: {e}"),
        _ => {}
    }
    write_domain_index(&dir);
    if let Some(p) = last_update_path() {
        let _ = std::fs::write(p, now_secs().to_string());
    }
    Ok(count)
}

/// A GitHub token for private-repo sources: `CHROME_USE_SITES_TOKEN`, else the
/// usual `GITHUB_TOKEN` / `GH_TOKEN`. Public sources need none.
fn sources_token() -> Option<String> {
    for k in ["CHROME_USE_SITES_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(v) = std::env::var(k) {
            if !v.trim().is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// Resolve one built-in or configured source and merge it into `dir`:
/// - a local directory path → copy its `.js`/`_helper.js` tree in;
/// - a `.zip`/http(s) URL → download + extract;
/// - a GitHub `owner/repo` → download its default-branch archive (private repos
///   use the token via the API zipball endpoint).
async fn sync_source(
    client: &reqwest::Client,
    source: &str,
    token: Option<&str>,
    dir: &std::path::Path,
) -> Result<Vec<String>, String> {
    // Local directory: copy the adapter tree verbatim (no strip).
    let as_path = PathBuf::from(source);
    if as_path.is_dir() {
        return copy_local_tree(&as_path, dir);
    }
    // Explicit zip / http(s) URL.
    if source.starts_with("http://") || source.starts_with("https://") {
        let strip = source.contains("github.com") || source.contains("api.github.com");
        return fetch_zip_into(client, source, token, dir, strip).await;
    }
    // GitHub `owner/repo`.
    if let Some((owner, repo)) = parse_owner_repo(source) {
        if let Some(tok) = token {
            // API zipball works for private repos and honours the token.
            let url = format!("https://api.github.com/repos/{owner}/{repo}/zipball");
            return fetch_zip_into(client, &url, Some(tok), dir, true).await;
        }
        // Public: hit the archive host directly (no api.github.com rate limit).
        let main = format!("https://github.com/{owner}/{repo}/archive/refs/heads/main.zip");
        match fetch_zip_into(client, &main, None, dir, true).await {
            Ok(n) => return Ok(n),
            Err(_) => {
                let master =
                    format!("https://github.com/{owner}/{repo}/archive/refs/heads/master.zip");
                return fetch_zip_into(client, &master, None, dir, true).await;
            }
        }
    }
    Err("unrecognized source (want `owner/repo`, a .zip URL, or a local dir path)".to_string())
}

/// `owner/repo` shape check: exactly one `/`, non-empty halves, no traversal /
/// URL scheme / whitespace. Returns the split.
fn parse_owner_repo(s: &str) -> Option<(String, String)> {
    if s.contains("://") || s.contains(' ') || s.contains("..") {
        return None;
    }
    let (owner, repo) = s.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// Download a zip and extract it into `dir`. When `strip_top` is set, drop the
/// single top-level wrapper component every GitHub archive adds
/// (`<repo>-<ref>/…`). Files are overlaid (later sources overwrite earlier).
async fn fetch_zip_into(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
    dir: &std::path::Path,
    strip_top: bool,
) -> Result<Vec<String>, String> {
    let mut req = client.get(url);
    if let Some(tok) = token {
        req = req.header("Authorization", format!("Bearer {tok}"));
    }
    let bytes = req
        .send()
        .await
        .map_err(|e| format!("download failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("{e}"))?
        .bytes()
        .await
        .map_err(|e| format!("read body: {e}"))?;

    let cursor = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(cursor).map_err(|e| format!("bad zip: {e}"))?;
    let mut written = Vec::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
        let Some(enclosed) = f.enclosed_name() else {
            continue;
        };
        let rel: PathBuf = if strip_top {
            enclosed.components().skip(1).collect()
        } else {
            enclosed.to_path_buf()
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out = dir.join(&rel);
        // Defense-in-depth: never let a crafted zip escape the sites dir.
        if !out.starts_with(dir) {
            continue;
        }
        if f.is_dir() {
            let _ = std::fs::create_dir_all(&out);
            continue;
        }
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut buf = Vec::new();
        std::io::copy(&mut f, &mut buf).map_err(|e| e.to_string())?;
        std::fs::write(&out, &buf).map_err(|e| e.to_string())?;
        if let Some(spec) = spec_of(&rel) {
            written.push(spec);
        }
    }
    Ok(written)
}

/// `<name>/<cmd>.js` (relative to the sites dir) → `name/cmd`, for runnable
/// adapters only; helpers, tests and deeper paths → None.
fn spec_of(rel: &std::path::Path) -> Option<String> {
    let parts: Vec<&str> = rel.iter().filter_map(|c| c.to_str()).collect();
    let [name, file] = parts.as_slice() else {
        return None;
    };
    let stem = file.strip_suffix(".js")?;
    is_runnable_adapter_stem(stem).then(|| format!("{name}/{stem}"))
}

/// Copy a local adapter tree (a directory of `<name>/<cmd>.js` packs) into `dir`.
fn copy_local_tree(src: &std::path::Path, dir: &std::path::Path) -> Result<Vec<String>, String> {
    let mut written = Vec::new();
    for entry in walkdir_js(src) {
        let rel = entry.strip_prefix(src).map_err(|e| e.to_string())?;
        let out = dir.join(rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::copy(&entry, &out).map_err(|e| e.to_string())?;
        if let Some(spec) = spec_of(rel) {
            written.push(spec);
        }
    }
    Ok(written)
}

/// Recursively collect `.js` files under `root` (shallow, dependency-free walk).
fn walkdir_js(root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&cur) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()) == Some("js") {
                out.push(p);
            }
        }
    }
    out
}

/// `~/.chrome-use/sites/.last_update` — unix-seconds marker of the last sync.
fn last_update_path() -> Option<PathBuf> {
    sites_dir().map(|d| d.join(".last_update"))
}

/// `~/.chrome-use/sites/.index.json` — `{ "github.com": ["github/issues", …], … }`,
/// built on `update` so navigation can look up adapters by domain without parsing
/// all ~145 adapter files on every command.
fn index_path() -> Option<PathBuf> {
    sites_dir().map(|d| d.join(".index.json"))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Parse every installed adapter and write the domain→adapters index. Within a
/// domain, read-only adapters are listed first (then alphabetical) so the
/// auto-suggested example leads with a safe read, not a write action.
fn write_domain_index(dir: &std::path::Path) {
    let provenance = read_provenance(dir);
    let mut by_domain: std::collections::BTreeMap<String, Vec<(bool, String)>> = Default::default();
    for spec in list_adapters().unwrap_or_default() {
        if let Ok(a) = load_adapter(&spec) {
            if let Some(d) = a.domain() {
                let read_only = a
                    .meta
                    .get("readOnly")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                by_domain
                    .entry(d.to_string())
                    .or_default()
                    .push((read_only, spec));
            }
        }
    }
    // OpenCLI commands ride along after ours, for names we don't already have.
    let ours: std::collections::HashSet<String> = by_domain
        .values()
        .flat_map(|v| v.iter().map(|(_, s)| s.clone()))
        .collect();
    let mut opencli_by_domain: std::collections::BTreeMap<String, Vec<(bool, String)>> =
        Default::default();
    for entry in crate::opencli::manifest() {
        let (Some(spec), Some(domain)) = (
            crate::opencli::spec_of(&entry),
            entry.get("domain").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        // OpenCLI also drives local Electron apps over CDP (`localhost`,
        // `127.0.0.1`); those are not websites, and indexing them would offer
        // app commands on every local dev page.
        let host = domain.split(':').next().unwrap_or(domain);
        if domain.is_empty() || ours.contains(&spec) || is_local_host(host) {
            continue;
        }
        let read_only = entry.get("access").and_then(|v| v.as_str()) == Some("read");
        opencli_by_domain
            .entry(domain.to_string())
            .or_default()
            .push((read_only, spec));
    }
    let ordered: std::collections::BTreeMap<String, Vec<String>> = by_domain
        .into_iter()
        .map(|(domain, mut v)| {
            // ours (official / configured) before community, then read-only
            // (true) first, then by spec name
            v.sort_by(|a, b| {
                let ra = source_rank(provenance.get(&a.1));
                let rb = source_rank(provenance.get(&b.1));
                ra.cmp(&rb)
                    .then_with(|| b.0.cmp(&a.0))
                    .then_with(|| a.1.cmp(&b.1))
            });
            (domain, v.into_iter().map(|(_, s)| s).collect())
        })
        .collect();
    // OpenCLI goes in its own index so a lookup can always rank it after
    // ours, even when the two packs spell the domain differently
    // (`v2ex.com` vs `www.v2ex.com`).
    let theirs: std::collections::BTreeMap<String, Vec<String>> = opencli_by_domain
        .into_iter()
        .map(|(domain, mut v)| {
            v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            (domain, v.into_iter().map(|(_, s)| s).collect())
        })
        .collect();
    if let Ok(json) = serde_json::to_string(&theirs) {
        let _ = std::fs::write(dir.join(".index-opencli.json"), json);
    }
    if let Ok(json) = serde_json::to_string(&ordered) {
        let _ = std::fs::write(dir.join(".index.json"), json);
    }
}

/// `.provenance.json` — `name/cmd` → the source it was synced from. Empty for
/// packs synced before it existed (every adapter then ranks as community).
fn read_provenance(dir: &std::path::Path) -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(dir.join(".provenance.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Sort rank for a source: our own adapters (the official pack or a configured
/// extra source) ahead of the community pack.
fn source_rank(source: Option<&String>) -> u8 {
    match source.map(String::as_str) {
        Some(COMMUNITY_SITES_SOURCE) | None => 1,
        Some(_) => 0,
    }
}

const DEFAULT_TTL_DAYS: u64 = 7;

/// Whether the adapter packs should be (re)synced: true on first use (nothing
/// installed) or when the last sync is older than the TTL. Disabled by
/// `AGENT_BROWSER_SITES_NO_AUTO_UPDATE=1`; TTL overridable via
/// `AGENT_BROWSER_SITES_TTL_DAYS` (0 = always).
pub fn needs_refresh() -> bool {
    if std::env::var_os("AGENT_BROWSER_SITES_NO_AUTO_UPDATE").is_some() {
        return false;
    }
    let Some(dir) = sites_dir() else {
        return false;
    };
    // First use: no adapters installed yet.
    if list_adapters().map(|l| l.is_empty()).unwrap_or(true) {
        let _ = &dir;
        return true;
    }
    let ttl_days = std::env::var("AGENT_BROWSER_SITES_TTL_DAYS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TTL_DAYS);
    let ttl = ttl_days.saturating_mul(86_400);
    match last_update_path().and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(s) => match s.trim().parse::<u64>() {
            Ok(ts) => now_secs().saturating_sub(ts) >= ttl,
            Err(_) => true,
        },
        None => true, // no marker → treat as stale
    }
}

/// Adapters whose `@meta.domain` matches `host` (exact, or `host` is a subdomain
/// of it) — for auto-suggesting `site` commands when you land on a known site.
/// Reads the prebuilt `.index.json`; empty if the packs aren't synced yet.
pub fn adapters_for_domain(host: &str) -> Vec<String> {
    let host = host.trim_start_matches("www.");
    // Ours first, then OpenCLI's (its own index, see write_domain_index).
    let mut out: Vec<String> = Vec::new();
    let files = [
        index_path(),
        sites_dir().map(|d| d.join(".index-opencli.json")),
    ];
    for path in files.into_iter().flatten() {
        if path.ends_with(".index-opencli.json") && crate::opencli::disabled() {
            continue;
        }
        let Some(idx) = std::fs::read_to_string(&path).ok().and_then(|raw| {
            serde_json::from_str::<std::collections::BTreeMap<String, Vec<String>>>(&raw).ok()
        }) else {
            continue;
        };
        // Preserve each index's per-domain ordering (read-only first); dedup
        // when a host matches several domain keys.
        for (domain, specs) in idx {
            let d = domain.trim_start_matches("www.");
            if host == d || host.ends_with(&format!(".{d}")) {
                for s in specs {
                    if !out.contains(&s) {
                        out.push(s);
                    }
                }
            }
        }
    }
    out
}

/// `~/.chrome-use/site-usage.json` — per host: the days it was driven (last
/// 30 distinct) and when we last suggested writing an adapter for it. Hosts
/// only, never paths; local to this machine.
fn usage_path() -> Option<PathBuf> {
    dirs_home().map(|h| h.join(".chrome-use").join("site-usage.json"))
}

/// Actions in one session on an adapter-less host before suggesting one.
const SUGGEST_SESSION_ACTIONS: u32 = 30;
/// ...or this many distinct days on it, with at least a few actions today.
const SUGGEST_DAYS: usize = 3;
const SUGGEST_DAYS_MIN_ACTIONS: u32 = 5;
/// Don't ask again about the same host within this window.
const SUGGEST_COOLDOWN_SECS: u64 = 14 * 86_400;

fn is_local_host(host: &str) -> bool {
    host.is_empty()
        || host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.parse::<std::net::IpAddr>().is_ok()
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok()
}

/// Pure decision: should a host with this usage get an adapter suggestion now?
pub fn should_suggest_adapter(
    host: &str,
    session_actions: u32,
    days_used: usize,
    last_suggested: Option<u64>,
    now: u64,
) -> bool {
    if is_local_host(host) {
        return false;
    }
    if last_suggested.is_some_and(|t| now.saturating_sub(t) < SUGGEST_COOLDOWN_SECS) {
        return false;
    }
    session_actions >= SUGGEST_SESSION_ACTIONS
        || (days_used >= SUGGEST_DAYS && session_actions >= SUGGEST_DAYS_MIN_ACTIONS)
}

fn read_usage() -> serde_json::Map<String, Value> {
    usage_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn write_usage(map: &serde_json::Map<String, Value>) {
    if let Some(p) = usage_path() {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(p, Value::Object(map.clone()).to_string());
    }
}

/// Note that `host` was driven today. Call once per host per session; returns
/// the number of distinct days it has been used (today included).
pub fn record_usage_day(host: &str) -> usize {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut map = read_usage();
    let entry = map.entry(host.to_string()).or_insert_with(|| json!({}));
    let mut days: Vec<String> = entry
        .get("days")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|d| d.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if !days.contains(&today) {
        days.push(today);
        let excess = days.len().saturating_sub(30);
        days.drain(..excess);
        entry["days"] = json!(days);
        write_usage(&map);
    }
    days.len()
}

/// When this host was last suggested for an adapter, if ever.
pub fn last_suggested(host: &str) -> Option<u64> {
    read_usage()
        .get(host)
        .and_then(|e| e.get("suggested"))
        .and_then(|v| v.as_u64())
}

pub fn mark_suggested(host: &str) {
    let mut map = read_usage();
    let entry = map.entry(host.to_string()).or_insert_with(|| json!({}));
    entry["suggested"] = json!(now_secs());
    write_usage(&map);
}

/// The `siteAdapterSuggestion` payload: frequent use of a site with no adapter.
/// Phrased as a question for the user — writing one is their call.
pub fn adapter_suggestion(host: &str, session_actions: u32, days_used: usize) -> Value {
    json!({
        "domain": host,
        "actionsThisSession": session_actions,
        "daysUsed": days_used,
        "message": format!(
            "{host} is driven often and has no site adapter. Ask the user whether to \
             turn the repeated steps into one (a single `chrome-use site <name>/<cmd>` call); \
             only write it if they agree. Guide: `chrome-use skills get core/site-adapters`."
        ),
    })
}

/// `now_secs` for callers outside this module.
pub fn unix_now() -> u64 {
    now_secs()
}

/// Page-side half of `site analyze`: what an adapter author needs to pick a
/// data source. Requests come from the Resource Timing buffer, so they cover
/// what the page has loaded so far — interact first (search, scroll, open a
/// list), then analyze, to catch the request that action made.
pub const ANALYZE_JS: &str = r#"(() => {
  const out = { url: location.href, host: location.hostname, title: document.title };
  const host = location.hostname.replace(/^www\./, '');
  const base = host.split('.').slice(-2).join('.');
  const noise = /google-analytics|googletagmanager|doubleclick|googlesyndication|adtrafficquality|adservice|pagead|facebook\.net|hotjar|sentry|segment\.(io|com)|mixpanel|clarity\.ms|bat\.bing|newrelic|datadoghq|amplitude|\/collect\b|\/log(ging)?\b|\/track(ing)?\b|\/beacon\b|\/metrics?\b|\/report\b|\/telemetry\b|\/pixel\b/i;
  const seen = new Set();
  const api = [];
  for (const e of performance.getEntriesByType('resource')) {
    if (!['fetch', 'xmlhttprequest'].includes(e.initiatorType)) continue;
    let u; try { u = new URL(e.name); } catch (_) { continue; }
    const key = u.origin + u.pathname;
    if (seen.has(key)) continue;
    seen.add(key);
    const sameSite = u.hostname === location.hostname || u.hostname.endsWith('.' + base) || u.hostname === base;
    const reasons = [];
    let score = 0;
    if (noise.test(e.name)) { score -= 5; reasons.push('analytics/telemetry'); }
    if (sameSite) { score += 2; reasons.push('same site'); }
    if (/\/(api|ajax|graphql|gql|rest|v\d+|x\/|web-interface|rpc|data)\b/i.test(u.pathname)) { score += 3; reasons.push('api-like path'); }
    if (/\.json\b/i.test(u.pathname)) { score += 2; reasons.push('json'); }
    if (/graphql|gql/i.test(u.pathname)) reasons.push('graphql');
    if (/[?&](page|cursor|offset|limit|size|count|keyword|q|query|id|uid)=/i.test(u.search)) { score += 1; reasons.push('paging/query params'); }
    if (e.transferSize > 2000) { score += 1; reasons.push('sizeable body'); }
    api.push({ url: e.name.length > 300 ? e.name.slice(0, 300) + '…' : e.name, type: e.initiatorType, sameSite, bytes: e.transferSize || 0, score, reasons });
  }
  api.sort((a, b) => b.score - a.score);
  out.api = api.filter(a => a.score >= 2).slice(0, 12);
  out.requestsSeen = api.length;

  const known = ['__NEXT_DATA__', '__NUXT__', '__NUXT_DATA__', '__INITIAL_STATE__', '__INITIAL_DATA__', '__INITIAL_PROPS__', '__PRELOADED_STATE__', '__APOLLO_STATE__', '__REDUX_STATE__', '__SSR_DATA__', '__remixContext', '__UNIVERSAL_DATA_FOR_REHYDRATION__', 'ytInitialData', 'ytInitialPlayerResponse', '__pinia', '__INITIAL_SSR_STATE__', 'g_initialProps', '__STATE__'];
  const names = new Set(known.filter(k => { try { return window[k] != null; } catch (_) { return false; } }));
  for (const k of Object.getOwnPropertyNames(window)) {
    if (names.size > 20) break;
    if (/^__.*(STATE|DATA|PROPS|CONTEXT|STORE)__?$/i.test(k) || /^(initial|preloaded|ssr)(State|Data|Props)$/i.test(k)) {
      try { if (window[k] && typeof window[k] === 'object') names.add(k); } catch (_) {}
    }
  }
  const describe = v => {
    let size = 0; try { size = JSON.stringify(v).length; } catch (_) { size = -1; }
    const keys = v && typeof v === 'object' ? Object.keys(v).slice(0, 10) : [];
    return { size, keys };
  };
  out.state = [];
  for (const k of names) { try { out.state.push({ name: 'window.' + k, ...describe(window[k]) }); } catch (_) {} }
  for (const el of document.querySelectorAll('script[type="application/json"], script[type="application/ld+json"]')) {
    if (out.state.length > 25) break;
    let v; try { v = JSON.parse(el.textContent); } catch (_) { continue; }
    const sel = el.id ? 'script#' + el.id : 'script[type="' + el.type + '"]';
    out.state.push({ name: sel, ...describe(v) });
  }
  // Extension-injected globals (Vue/React devtools) and telemetry config are
  // not the page's data.
  const junk = /devtools|rum|analytics|tracking|gtm|sentry/i;
  const seenState = new Set();
  out.state = out.state.filter(s => {
    if (junk.test(s.name) || seenState.has(s.name)) return false;
    seenState.add(s.name);
    return s.size === -1 || s.size > 200;
  });

  out.webpack = Object.getOwnPropertyNames(window).filter(k => /^webpackChunk|^webpackJsonp/.test(k)).slice(0, 3);
  out.signals = {
    cookies: document.cookie.split(';').map(c => c.trim().split('=')[0]).filter(Boolean),
    scripts: Array.from(document.scripts, s => s.src || '').filter(Boolean),
    globals: Object.getOwnPropertyNames(window).filter(k => /_px|bmak|_abck|datadome|reese84|kpsdk|incap_ses|visid_incap|akam/i.test(k)),
  };
  out.loggedInHint = document.cookie.length > 0;
  return out;
})()"#;

/// Turn the page scan into the `site analyze` report: drop the raw signals,
/// attach anti-bot vendors and installed adapters, and recommend a data source
/// in the order that breaks least often — a site's JSON API called from the
/// page, then state the page already embeds, then the DOM.
pub fn analyze_report(raw: &Value, vendors: &[&str], adapters: &[String]) -> Value {
    let mut report = raw.clone();
    if let Some(o) = report.as_object_mut() {
        o.remove("signals");
        o.remove("loggedInHint");
    }
    let api = raw
        .get("api")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let state = raw
        .get("state")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let webpack = raw
        .get("webpack")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty());
    let host = raw.get("host").and_then(|v| v.as_str()).unwrap_or("");

    let mut steps: Vec<String> = Vec::new();
    let strategy;
    if !adapters.is_empty() {
        steps.push(format!(
            "Adapters already exist for {host}: {}. Run `chrome-use site info <name>/<cmd>` before writing a new one.",
            adapters.join(", ")
        ));
    }
    if let Some(top) = api
        .iter()
        .find(|a| a.get("sameSite").and_then(|v| v.as_bool()) == Some(true))
    {
        strategy = "page-fetch";
        let url = top.get("url").and_then(|v| v.as_str()).unwrap_or("");
        steps.push(format!(
            "Call the site's own API from the page: `fetch(url, {{credentials: 'include'}})`, starting from {url}. Check its JSON with `chrome-use eval` first, then map it to the fields the user needs."
        ));
    } else if let Some(s) = state.first() {
        strategy = "page-state";
        let name = s.get("name").and_then(|v| v.as_str()).unwrap_or("");
        steps.push(format!(
            "No API call seen yet, but the page embeds its data in {name}. Read it in the adapter (no extra request). If the data you need comes from a later action, do that action and analyze again."
        ));
    } else {
        strategy = "dom";
        steps.push(
            "No API call or embedded state found. Do the action that loads the data (search, scroll, open a list) and run `site analyze` again; fall back to reading the DOM only if nothing shows up — it breaks the most often.".to_string(),
        );
    }
    if webpack {
        steps.push("The page is a webpack bundle: its own modules (e.g. signed request helpers) can be reached via the webpackChunk global if the API needs a signature.".to_string());
    }
    if !vendors.is_empty() {
        steps.push(format!(
            "Anti-bot protection detected ({}). Keep requests inside the page (same-origin fetch with the page's cookies), keep the call rate low, and never replay them from outside the browser.",
            vendors.join(", ")
        ));
    }
    steps.push("Write the adapter to ~/.chrome-use/my-sites/<name>/<cmd>.js, register it once with `chrome-use site add ~/.chrome-use/my-sites`, run `site update`, then `chrome-use site verify <name>/<cmd> --write-fixture` to record what a good result looks like. Guide: `chrome-use skills get core/site-adapters`.".to_string());

    if let Some(o) = report.as_object_mut() {
        o.insert("antiBot".into(), json!(vendors));
        o.insert("adapters".into(), json!(adapters));
        o.insert("strategy".into(), json!(strategy));
        o.insert("next".into(), json!(steps));
    }
    report
}

/// `~/.chrome-use/site-fixtures/<name>/<cmd>.json` — the recorded shape of a
/// good result, for `site verify`.
pub fn fixture_path(spec: &str) -> Option<PathBuf> {
    let (name, cmd) = spec.split_once('/')?;
    dirs_home().map(|h| {
        h.join(".chrome-use")
            .join("site-fixtures")
            .join(name)
            .join(format!("{cmd}.json"))
    })
}

/// A structural summary of an adapter result: types per field, depth-limited,
/// with array items merged over the first few elements. Values are dropped, so
/// a fixture holds no user data.
pub fn result_shape(v: &Value) -> Value {
    shape_at(v, 0)
}

fn shape_at(v: &Value, depth: usize) -> Value {
    match v {
        Value::Null => json!("null"),
        Value::Bool(_) => json!("boolean"),
        Value::Number(_) => json!("number"),
        Value::String(_) => json!("string"),
        Value::Array(a) => {
            let mut items = Value::Null;
            if depth < 4 {
                for el in a.iter().take(5) {
                    items = merge_shape(items, shape_at(el, depth + 1));
                }
            }
            json!({ "array": items, "nonEmpty": !a.is_empty() })
        }
        Value::Object(o) => {
            if depth >= 4 {
                return json!("object");
            }
            let fields: serde_json::Map<String, Value> = o
                .iter()
                .map(|(k, v)| (k.clone(), shape_at(v, depth + 1)))
                .collect();
            json!({ "object": fields })
        }
    }
}

/// Merge two shapes of sibling array items: keep fields present in any item,
/// and let a concrete type win over "null".
fn merge_shape(a: Value, b: Value) -> Value {
    match (a, b) {
        (Value::Null, b) => b,
        (a, Value::String(t)) if t == "null" => a,
        (Value::String(t), b) if t == "null" => b,
        (Value::Object(mut x), Value::Object(y)) => {
            if let (Some(Value::Object(fx)), Some(Value::Object(fy))) =
                (x.get("object").cloned(), y.get("object"))
            {
                let mut merged = fx;
                for (k, v) in fy {
                    let cur = merged.remove(k).unwrap_or(Value::Null);
                    merged.insert(k.clone(), merge_shape(cur, v.clone()));
                }
                x.insert("object".into(), Value::Object(merged));
            }
            Value::Object(x)
        }
        (a, _) => a,
    }
}

/// Differences that mean the adapter broke: a field the fixture had is gone,
/// a field changed type, or a list that had rows came back empty. New fields
/// and null values are fine.
pub fn shape_diff(expected: &Value, actual: &Value) -> Vec<String> {
    let mut out = Vec::new();
    diff_at(expected, actual, "result", &mut out);
    out
}

fn kind(v: &Value) -> &str {
    match v {
        Value::String(t) => t.as_str(),
        Value::Object(o) if o.contains_key("array") => "array",
        Value::Object(o) if o.contains_key("object") => "object",
        _ => "unknown",
    }
}

fn diff_at(exp: &Value, act: &Value, path: &str, out: &mut Vec<String>) {
    let (ke, ka) = (kind(exp), kind(act));
    if ke == "null" || ka == "null" || ke == "unknown" {
        return;
    }
    if ke != ka {
        out.push(format!("{path}: was {ke}, now {ka}"));
        return;
    }
    match ke {
        "array" => {
            let had = exp
                .get("nonEmpty")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let has = act
                .get("nonEmpty")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if had && !has {
                out.push(format!("{path}: was a non-empty list, now empty"));
                return;
            }
            if let (Some(e), Some(a)) = (exp.get("array"), act.get("array")) {
                if !a.is_null() {
                    diff_at(e, a, &format!("{path}[]"), out);
                }
            }
        }
        "object" => {
            let (Some(fe), Some(fa)) = (
                exp.get("object").and_then(|v| v.as_object()),
                act.get("object").and_then(|v| v.as_object()),
            ) else {
                return;
            };
            for (k, ve) in fe {
                match fa.get(k) {
                    Some(va) => diff_at(ve, va, &format!("{path}.{k}"), out),
                    None => out.push(format!("{path}.{k}: missing")),
                }
            }
        }
        _ => {}
    }
}

/// `site verify`: check a run's result against the stored fixture, or record
/// one. Returns (ok, report).
pub fn verify_result(spec: &str, result: &Value, write_fixture: bool) -> (bool, Value) {
    let shape = result_shape(result);
    let path = fixture_path(spec);
    let empty = match result {
        Value::Null => true,
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        Value::String(s) => s.is_empty(),
        _ => false,
    };
    if write_fixture {
        if empty {
            return (
                false,
                json!({ "spec": spec, "ok": false, "issues": ["result is empty; not recording it as a fixture"] }),
            );
        }
        let fixture = json!({ "spec": spec, "recordedAt": now_secs(), "shape": shape });
        let written = path.as_ref().is_some_and(|p| {
            p.parent()
                .is_some_and(|d| std::fs::create_dir_all(d).is_ok())
                && std::fs::write(
                    p,
                    serde_json::to_string_pretty(&fixture).unwrap_or_default(),
                )
                .is_ok()
        });
        return (
            written,
            json!({
                "spec": spec,
                "ok": written,
                "fixture": path.map(|p| p.display().to_string()),
                "recorded": written,
            }),
        );
    }
    let stored = path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let Some(stored) = stored else {
        let mut issues = Vec::new();
        if empty {
            issues.push("result is empty".to_string());
        }
        return (
            !empty,
            json!({
                "spec": spec,
                "ok": !empty,
                "fixture": null,
                "issues": issues,
                "next": format!("no fixture yet: run `chrome-use site verify {spec} … --write-fixture` once the result looks right"),
            }),
        );
    };
    let issues = shape_diff(stored.get("shape").unwrap_or(&Value::Null), &shape);
    let ok = issues.is_empty();
    (
        ok,
        json!({
            "spec": spec,
            "ok": ok,
            "fixture": path.map(|p| p.display().to_string()),
            "issues": issues,
        }),
    )
}

/// Map CLI args to the adapter's `args` object. Positional args fill the adapter's
/// declared `args` keys in order; `--key value` overrides by name. The adapter
/// validates required args itself.
///
/// Caveat (issue #125): a `--key` whose name collides with a reserved global flag
/// (e.g. `--state`, `--profile`, `--session`) is consumed by the global flag
/// parser before it reaches here, so it never lands in `named`. Pass such args
/// positionally, or after the `--` end-of-options marker
/// (`site <name>/<cmd> -- --state closed`), which forwards everything verbatim.
/// The CLI warns when it detects this collision.
/// #122: a `site` adapter can return an application-level error (e.g.
/// `{error:"HTTP 429", hint:...}`) while the *eval* itself succeeds, so the
/// transport envelope stays `success:true` / exit 0 and automation can't tell
/// a rate-limited/failed call from a real empty result. Promote such an
/// adapter error into the top-level envelope so both `--json`
/// (`success:false`, `error`) and the exit code (1) reflect it.
pub fn promote_adapter_error(resp: &mut crate::connection::Response) {
    let Some(result) = resp.data.as_ref().and_then(|d| d.get("result")) else {
        return;
    };
    let adapter_err = result
        .get("error")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let explicit_fail = result.get("success").and_then(|v| v.as_bool()) == Some(false);
    if adapter_err.is_none() && !explicit_fail {
        return;
    }
    resp.success = false;
    if resp.error.is_none() {
        let err = adapter_err.unwrap_or_else(|| "site adapter reported failure".to_string());
        // Carry the adapter's `hint` (#359: how to pass a missing arg) into
        // the message the caller reads.
        let hint = result
            .get("hint")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        resp.error = Some(match hint {
            Some(h) => format!("{err} — {h}"),
            None => err,
        });
    }
}

/// What [`apply_site_login_wall`] needs from the outside world, so the flow
/// can be tested without a browser or a vault.
pub struct SiteLoginIo<'a> {
    /// Whether to sign in at this wall (#481): the stored decision, a
    /// prompt at a terminal, or an `ask` object for an agent.
    /// `crate::autologin::decide` in the CLI.
    pub decide: &'a mut dyn FnMut(&Value) -> crate::autologin::Outcome,
    /// `--json`: stdout is the envelope, so the wall also goes to stderr.
    /// Text mode prints the error (the same line) there already.
    pub json: bool,
    /// Open the site's sign-in page in this session.
    pub navigate: &'a mut dyn FnMut(&str) -> Result<(), String>,
    /// `bwu_login::auto_login`: sign in on the current page, return to
    /// `returnTo`; `{ok, item, error, returnedTo}`.
    pub sign_in: &'a mut dyn FnMut(&Value) -> Value,
    /// Send the same `site` command again.
    pub rerun: &'a mut dyn FnMut() -> Result<crate::connection::Response, String>,
}

fn data_wall(resp: &crate::connection::Response) -> Option<&Value> {
    resp.data.as_ref().and_then(|d| d.get("loginWall"))
}

fn wall_for(cmd: &Value, resp: &crate::connection::Response) -> Option<Value> {
    let data = resp.data.as_ref()?;
    let result = data.get("result")?;
    // The daemon's own check (#434) flags a tab that landed on a sign-in
    // page; that is evidence for the adapter's failure too.
    let page_wall =
        data_wall(resp).filter(|w| w.get("source").and_then(|v| v.as_str()) != Some("site"));
    site_login_wall(
        result,
        page_wall,
        data.get("origin").and_then(|v| v.as_str()).unwrap_or(""),
        data.get("domain")
            .and_then(|v| v.as_str())
            .or_else(|| cmd.get("domain").and_then(|v| v.as_str()))
            .unwrap_or(""),
        cmd.get("spec").and_then(|v| v.as_str()).unwrap_or(""),
        cmd.get("readOnly").and_then(|v| v.as_bool()) == Some(true),
    )
}

fn set_wall(resp: &mut crate::connection::Response, wall: Value) {
    let data = resp.data.get_or_insert_with(|| json!({}));
    if let Some(d) = data.as_object_mut() {
        d.insert("loginWall".into(), wall);
    }
}

/// A `site` adapter run that failed because the site is not signed in
/// (#479) is a login wall like a redirect to a sign-in page (#434): replace
/// the adapter's error with the wall's hint (which points at `auth login
/// --bwu`, not at the user), put `loginWall` in the data, and with auto-login
/// on, open the sign-in page, sign in from the vault and run the command once
/// more. A write whose failure was only inferred (HTTP 401, a redirect) is
/// not rerun: it may have half-run. Returns whether there was a wall.
pub fn apply_site_login_wall(
    cmd: &Value,
    resp: &mut crate::connection::Response,
    io: SiteLoginIo<'_>,
) -> bool {
    let Some(mut wall) = wall_for(cmd, resp) else {
        return false;
    };
    let hint = wall["hint"].as_str().unwrap_or("").to_string();
    let host = wall["host"].as_str().unwrap_or("").to_string();
    let spec = wall["spec"].as_str().unwrap_or("").to_string();
    resp.success = false;
    resp.error = Some(hint.clone());
    match (io.decide)(&wall) {
        crate::autologin::Outcome::SignIn { .. } => {
            eprintln!("{} {hint}", crate::color::warning_indicator());
        }
        crate::autologin::Outcome::Skip { source } => {
            if io.json {
                eprintln!("{} {hint}", crate::color::warning_indicator());
            }
            wall["autoLoginDecision"] = json!(source);
            set_wall(resp, wall);
            return true;
        }
        crate::autologin::Outcome::Ask(ask) => {
            let text = crate::autologin::ask_text(&host, &ask);
            if io.json {
                eprintln!("{} {text}", crate::color::warning_indicator());
            }
            wall["ask"] = ask;
            resp.error = Some(text);
            set_wall(resp, wall);
            return true;
        }
    }
    if let Some(u) = wall["loginUrl"].as_str().map(str::to_string) {
        if let Err(e) = (io.navigate)(&u) {
            wall["autoLogin"] =
                json!({ "ok": false, "item": null, "error": format!("couldn't open {u}: {e}") });
            resp.error = Some(format!(
                "login wall: auto-login failed: couldn't open {u}: {e} — {hint}"
            ));
            set_wall(resp, wall);
            return true;
        }
    }
    let auto = (io.sign_in)(&wall);
    wall["autoLogin"] = auto.clone();
    if let Some(e) = auto.get("error").and_then(|v| v.as_str()) {
        eprintln!(
            "{} login wall: auto-login failed: {e}",
            crate::color::warning_indicator()
        );
        resp.error = Some(format!("login wall: auto-login failed: {e} — {hint}"));
        set_wall(resp, wall);
        return true;
    }
    if wall["rerunnable"].as_bool() != Some(true) {
        let source = wall["evidence"]["source"]
            .as_str()
            .unwrap_or("")
            .to_string();
        eprintln!("login wall: signed in to {host}");
        resp.error = Some(format!(
            "login wall: signed in to {host}, but site {spec} writes and its failure was inferred \
             ({source}), not reported by the adapter, so it was not run again: check the page, \
             then run the command again"
        ));
        set_wall(resp, wall);
        return true;
    }
    eprintln!("login wall: signed in to {host}; running site {spec} again");
    match (io.rerun)() {
        Ok(mut again) => {
            promote_adapter_error(&mut again);
            if let Some(still) = wall_for(cmd, &again) {
                // Signed in, and the adapter still says it is not: report
                // that, and do not loop.
                let h = still["hint"].as_str().unwrap_or("").to_string();
                wall["rerun"] = json!({ "ok": false, "error": h });
                again.success = false;
                again.error = Some(format!(
                    "login wall: signed in to {host}, but site {spec} still reports it is not \
                     signed in — {h}"
                ));
            } else {
                wall["rerun"] = json!({ "ok": again.success, "error": again.error });
            }
            set_wall(&mut again, wall);
            *resp = again;
        }
        Err(e) => {
            wall["rerun"] = json!({ "ok": false, "error": e });
            resp.error = Some(format!(
                "login wall: signed in to {host}; running site {spec} again failed: {e}"
            ));
            set_wall(resp, wall);
        }
    }
    true
}

pub fn map_args(adapter: &Adapter, positional: &[String], named: &[(String, String)]) -> Value {
    let mut obj = serde_json::Map::new();
    // Positional args fill the adapter's declared args in DECLARATION order
    // (`arg_order`), not serde's alphabetized key order — otherwise a 2-arg
    // adapter like `{projectId, path}` would map positionals to `{path, projectId}`.
    for (i, val) in positional.iter().enumerate() {
        if let Some(k) = adapter.arg_order.get(i) {
            obj.insert(k.clone(), Value::String(val.clone()));
        }
    }
    for (k, v) in named {
        obj.insert(k.clone(), Value::String(v.clone()));
    }
    Value::Object(obj)
}

#[cfg(test)]
mod tests {
    #[test]
    fn spec_of_keeps_runnable_adapters_only() {
        use std::path::Path;
        assert_eq!(
            spec_of(Path::new("twitter/search.js")).as_deref(),
            Some("twitter/search")
        );
        assert_eq!(spec_of(Path::new("twitter/_helper.js")), None);
        assert_eq!(spec_of(Path::new("README.md")), None);
        assert_eq!(spec_of(Path::new("a/b/c.js")), None);
    }

    #[test]
    fn official_and_configured_sources_rank_before_community() {
        let official = OFFICIAL_SITES_SOURCE.to_string();
        let community = COMMUNITY_SITES_SOURCE.to_string();
        let extra = "acme/internal-sites".to_string();
        assert!(source_rank(Some(&official)) < source_rank(Some(&community)));
        assert!(source_rank(Some(&extra)) < source_rank(Some(&community)));
        assert_eq!(source_rank(None), source_rank(Some(&community)));
    }

    #[test]
    fn analyze_prefers_same_site_api_then_state_then_dom() {
        let api = json!({"host":"x.com","api":[{"url":"https://x.com/i/api/graphql/Q","sameSite":true}],"state":[],"webpack":[],"signals":{}});
        let r = analyze_report(&api, &[], &[]);
        assert_eq!(r["strategy"], "page-fetch");
        assert!(r.get("signals").is_none());
        let state =
            json!({"host":"a.com","api":[],"state":[{"name":"window.__NEXT_DATA__","size":900}]});
        assert_eq!(analyze_report(&state, &[], &[])["strategy"], "page-state");
        let dom = json!({"host":"a.com","api":[{"url":"https://cdn.other.net/x","sameSite":false}],"state":[]});
        let r = analyze_report(&dom, &["akamai"], &["a/b".to_string()]);
        assert_eq!(r["strategy"], "dom");
        assert_eq!(r["antiBot"][0], "akamai");
        let next = r["next"].to_string();
        assert!(next.contains("a/b") && next.contains("Anti-bot"));
    }

    #[test]
    fn verify_flags_missing_fields_type_changes_and_emptied_lists() {
        let good = json!({"items":[{"id":1,"title":"a","tag":null},{"id":2,"title":"b","tag":"x"}],"total":2});
        let exp = result_shape(&good);
        assert!(shape_diff(&exp, &result_shape(&good)).is_empty());
        // extra field and null value are fine
        let extra = json!({"items":[{"id":3,"title":null,"tag":"y","new":true}],"total":1});
        assert!(shape_diff(&exp, &result_shape(&extra)).is_empty());
        let broken = json!({"items":[{"id":"3"}],"total":1});
        let d = shape_diff(&exp, &result_shape(&broken));
        assert!(
            d.iter()
                .any(|x| x.contains("result.items[].id: was number, now string")),
            "{d:?}"
        );
        assert!(
            d.iter()
                .any(|x| x.contains("result.items[].title: missing")),
            "{d:?}"
        );
        let emptied = json!({"items":[],"total":0});
        let d = shape_diff(&exp, &result_shape(&emptied));
        assert!(
            d.iter().any(|x| x.contains("non-empty list, now empty")),
            "{d:?}"
        );
    }

    #[test]
    fn local_app_domains_with_ports_count_as_local() {
        for d in ["localhost:9222", "127.0.0.1", "127.0.0.1:5173"] {
            assert!(is_local_host(d.split(':').next().unwrap()), "{d}");
        }
        assert!(!is_local_host("www.v2ex.com"));
    }

    #[test]
    fn adapter_suggestion_thresholds() {
        let now = 10_000_000;
        assert!(!should_suggest_adapter("example.com", 29, 1, None, now));
        assert!(should_suggest_adapter("example.com", 30, 1, None, now));
        assert!(should_suggest_adapter("example.com", 5, 3, None, now));
        assert!(!should_suggest_adapter("example.com", 4, 3, None, now));
        // cooldown
        assert!(!should_suggest_adapter(
            "example.com",
            99,
            9,
            Some(now - 86_400),
            now
        ));
        assert!(should_suggest_adapter(
            "example.com",
            99,
            9,
            Some(now - 15 * 86_400),
            now
        ));
        // local hosts never
        for h in [
            "",
            "localhost",
            "app.localhost",
            "nas.local",
            "127.0.0.1",
            "[::1]",
            "192.168.0.5",
        ] {
            assert!(!should_suggest_adapter(h, 99, 9, None, now), "{h}");
        }
    }

    use super::*;

    const SAMPLE: &str = r#"/* @meta
{
  "name": "github/issues",
  "domain": "github.com",
  "args": { "repo": {"required": true}, "state": {"required": false} }
}
*/

async function(args) { return { repo: args.repo }; }"#;

    #[test]
    fn parses_meta_and_function() {
        let a = parse_adapter(SAMPLE, "github/issues").unwrap();
        assert_eq!(a.domain(), Some("github.com"));
        assert!(a.func_src.starts_with("async function(args)"));
    }

    const PUBLISH: &str = r#"/* @meta
{
  "name": "demo/publish",
  "domain": "example.com",
  "timeout": 300,
  "args": {
    "title": {"required": true},
    "markdown": {"required": false},
    "video": {"required": false, "type": "file", "input": "input[type=file]"}
  }
}
*/
async function(args) {
  args.progress('starting with ' + args.budgetMs + 'ms');
  const up = await args.video.setOn();
  args.progress('uploaded');
  return { status: 'submitted', title: args.title, file: args.video.name, size: args.video.size,
           attached: up.attached, keys: Object.keys(args).join(',') };
}"#;

    fn no_stdin() -> Result<String, String> {
        Err("stdin not expected".to_string())
    }

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration_ms("300"), Some(300_000));
        assert_eq!(parse_duration_ms("90s"), Some(90_000));
        assert_eq!(parse_duration_ms("5m"), Some(300_000));
        assert_eq!(parse_duration_ms("1500ms"), Some(1500));
        assert_eq!(parse_duration_ms("1h"), Some(3_600_000));
        assert_eq!(parse_duration_ms("0"), None);
        assert_eq!(parse_duration_ms("soon"), None);
    }

    #[test]
    fn arg_values_read_files_and_stdin() {
        let dir = std::env::temp_dir().join(format!("cu-site-args-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let post = dir.join("post.md");
        std::fs::write(&post, "# Title\n\n\"quoted\" `code`\n").unwrap();
        let at = format!("@{}", post.display());
        assert_eq!(
            resolve_arg_value(&at, &mut no_stdin).unwrap(),
            "# Title\n\n\"quoted\" `code`\n"
        );
        // stdin
        let mut stdin = || Ok("from stdin".to_string());
        assert_eq!(resolve_arg_value("@-", &mut stdin).unwrap(), "from stdin");
        // A handle is not a file: stays literal.
        assert_eq!(resolve_arg_value("@jack", &mut no_stdin).unwrap(), "@jack");
        // A typo'd path must not be posted as text.
        let missing = format!("@{}", dir.join("nope.md").display());
        assert!(resolve_arg_value(&missing, &mut no_stdin)
            .unwrap_err()
            .contains("names no readable file"));
        assert!(resolve_arg_value("@nope.md", &mut no_stdin).is_err());
        // Escape for a literal leading @.
        assert_eq!(
            resolve_arg_value("\\@nope.md", &mut no_stdin).unwrap(),
            "@nope.md"
        );
        assert_eq!(resolve_arg_value("plain", &mut no_stdin).unwrap(), "plain");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invocation_maps_files_key_file_and_runner_flags() {
        let dir = std::env::temp_dir().join(format!("cu-site-inv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("clip.mp4");
        std::fs::write(&video, b"0123456789").unwrap();
        let body = dir.join("body.md");
        std::fs::write(&body, "long body").unwrap();
        let a = parse_adapter(PUBLISH, "demo/publish").unwrap();

        let inv = prepare_invocation(
            &a,
            &["Hello".into()],
            &[
                ("markdown-file".into(), body.display().to_string()),
                ("video".into(), video.display().to_string()),
                ("timeout".into(), "10m".into()),
            ],
            true,
            &mut no_stdin,
        )
        .unwrap();
        assert_eq!(inv.args["title"], "Hello");
        assert_eq!(inv.args["markdown"], "long body");
        assert!(inv.args.get("markdown-file").is_none());
        assert!(inv.args.get("timeout").is_none());
        assert_eq!(inv.files["video"]["name"], "clip.mp4");
        assert_eq!(inv.files["video"]["size"], 10);
        assert_eq!(inv.files["video"]["input"], "input[type=file]");
        assert!(std::path::Path::new(inv.files["video"]["path"].as_str().unwrap()).is_absolute());
        assert_eq!(inv.options.timeout_ms, Some(600_000));
        assert!(inv.options.until_done);
        assert_eq!(inv.options.effective_timeout_ms(&a), 600_000);

        // @meta.timeout applies when --timeout is absent; then the defaults.
        let inv = prepare_invocation(&a, &["Hello".into()], &[], false, &mut no_stdin).unwrap();
        assert_eq!(inv.options.effective_timeout_ms(&a), 300_000);
        let plain = parse_adapter(SAMPLE, "github/issues").unwrap();
        assert_eq!(
            RunOptions::default().effective_timeout_ms(&plain),
            DEFAULT_RUN_TIMEOUT_MS
        );
        assert_eq!(
            RunOptions {
                until_done: true,
                ..Default::default()
            }
            .effective_timeout_ms(&plain),
            DEFAULT_UNTIL_DONE_TIMEOUT_MS
        );

        // A missing file arg is an error before anything reaches the page.
        let err = prepare_invocation(
            &a,
            &["Hello".into()],
            &[("video".into(), dir.join("gone.mp4").display().to_string())],
            false,
            &mut no_stdin,
        )
        .unwrap_err();
        assert!(err.contains("--video"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn adapter_declared_timeout_arg_is_not_a_runner_flag() {
        let raw = r#"/* @meta {"name":"x/y","domain":"x.com","args":{"timeout":{"required":false},"until-done":{"required":false}}} */
async function(args) { return args; }"#;
        let a = parse_adapter(raw, "x/y").unwrap();
        assert!(!is_runner_flag(&a, "timeout"));
        assert!(!is_runner_flag(&a, "until-done"));
        let inv = prepare_invocation(
            &a,
            &[],
            &[("timeout".into(), "5".into())],
            false,
            &mut no_stdin,
        )
        .unwrap();
        assert_eq!(inv.args["timeout"], "5");
        assert_eq!(inv.options.timeout_ms, None);
    }

    #[test]
    fn retry_statuses_default_and_override() {
        let a = parse_adapter(SAMPLE, "github/issues").unwrap();
        assert_eq!(retry_statuses(&a), vec!["incomplete", "uploading"]);
        let raw = r#"/* @meta {"name":"x/y","domain":"x.com","retryStatuses":["processing"]} */
async function(args) { return args; }"#;
        assert_eq!(
            retry_statuses(&parse_adapter(raw, "x/y").unwrap()),
            vec!["processing"]
        );
    }

    /// Drive the real start / poll / settle scripts under node, playing the
    /// daemon: the run starts in the background, asks for an upload through
    /// `setOn`, reports progress, and the result comes back through a poll.
    #[test]
    fn background_run_protocol_round_trips_under_node() {
        let Some(node) = std::env::var_os("PATH").and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("node"))
                .find(|n| n.is_file())
        }) else {
            return;
        };
        let a = parse_adapter(PUBLISH, "demo/publish").unwrap();
        let args = json!({ "title": "Hi", "video": "/abs/clip.mp4" });
        let files = json!({ "video": { "path": "/abs/clip.mp4", "name": "clip.mp4", "size": 10, "input": "input[type=file]" } });
        let start =
            build_start(&a, &args, None, "__cu_site_t", &files).replace(BUDGET_PLACEHOLDER, "4200");
        let poll = poll_script("__cu_site_t");
        let settle = settle_script("__cu_site_t", 1, true, &json!({ "attached": 1 }));
        let driver = format!(
            "globalThis.window = globalThis; globalThis.location = new URL('https://example.com/');\n\
             const tick = () => new Promise(r => setTimeout(r, 5));\n\
             (async () => {{\n\
             const started = {start};\n\
             await tick();\n\
             const first = {poll};\n\
             const settled = {settle};\n\
             await tick();\n\
             const last = {poll};\n\
             const after = {poll};\n\
             console.log(JSON.stringify({{ started, first, settled, last, after, enumerable: Object.keys(window).includes('__cu_site_t') }}));\n\
             }})();"
        );
        let out = std::process::Command::new(node)
            .arg("-e")
            .arg(driver)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["started"], "started");
        assert_eq!(v["enumerable"], false);
        assert_eq!(v["first"]["state"], "running");
        assert_eq!(v["first"]["progress"][0], "starting with 4200ms");
        assert_eq!(v["first"]["requests"][0]["kind"], "upload");
        assert_eq!(v["first"]["requests"][0]["arg"], "video");
        assert_eq!(v["first"]["requests"][0]["selector"], "input[type=file]");
        assert_eq!(v["settled"], true);
        assert_eq!(v["last"]["state"], "done");
        assert_eq!(v["last"]["progress"][0], "uploaded");
        assert_eq!(v["last"]["result"]["status"], "submitted");
        assert_eq!(v["last"]["result"]["file"], "clip.mp4");
        assert_eq!(v["last"]["result"]["attached"], 1);
        // budgetMs / progress are hidden from an adapter that enumerates args.
        assert_eq!(v["last"]["result"]["keys"], "title,video");
        // A settled run is removed; the next poll reports it gone.
        assert_eq!(v["after"]["state"], "lost");
    }

    #[test]
    fn build_eval_wraps_and_passes_args() {
        let a = parse_adapter(SAMPLE, "github/issues").unwrap();
        let args = map_args(
            &a,
            &["owner/repo".into()],
            &[("state".into(), "closed".into())],
        );
        let js = build_eval(&a, &args, None);
        assert!(js.contains("async function(args)"));
        assert!(js.contains("\"repo\":\"owner/repo\""));
        assert!(js.contains("\"state\":\"closed\""));
    }

    #[test]
    fn build_eval_injects_family_helper() {
        // With a `_helper` source, the adapter runs inside a scope where the helper
        // declarations are visible, so calls like findGraphQLQueryId resolve (#99).
        let a = parse_adapter(SAMPLE, "twitter/thread").unwrap();
        let args = map_args(&a, &["123".into()], &[]);
        let helper = "function findGraphQLQueryId(){ return 'x'; }";
        let js = build_eval(&a, &args, Some(helper));
        assert!(js.contains("function findGraphQLQueryId()"));
        assert!(js.contains("return ("));
        assert!(js.contains("async function(args)"));
        // Helper is defined before the adapter expression that closes over it.
        assert!(js.find("findGraphQLQueryId").unwrap() < js.find("return (").unwrap());
        // Empty/None helper keeps the plain wrapper.
        assert_eq!(
            build_eval(&a, &args, Some("   ")),
            build_eval(&a, &args, None)
        );
    }

    // #359: a required arg whose description documents a URL template is
    // inferred from the current page when omitted.
    const LINKEDIN: &str = r#"/* @meta
{
  "name": "linkedin/profile",
  "domain": "www.linkedin.com",
  "args": {
    "username": {"required": true, "description": "LinkedIn username (from URL linkedin.com/in/<username>)"}
  }
}
*/
async function(args) { if (!args.username) return {error: 'Missing argument: username'}; return args; }"#;

    #[test]
    fn url_template_inferred_for_missing_required_arg() {
        let a = parse_adapter(LINKEDIN, "linkedin/profile").unwrap();
        let inf = url_inferences(&a, &map_args(&a, &[], &[]));
        assert_eq!(
            inf,
            vec![UrlInference {
                arg: "username".into(),
                host: Some("linkedin.com".into()),
                path_regex: r"^\/in\/([^/?#]+)(?:[/?#]|$)".into(),
            }]
        );
        // Supplied → nothing to infer.
        assert!(url_inferences(&a, &map_args(&a, &["bill".into()], &[])).is_empty());
        let js = build_eval(&a, &map_args(&a, &[], &[]), None);
        assert!(js.contains("location.pathname.match"));
        assert!(js.contains(
            "Usage: chrome-use site linkedin/profile <username> (or --username <value>)"
        ));
    }

    #[test]
    fn url_template_parsing_variants() {
        let t = parse_url_template("id", "Opus ID (from URL: bilibili.com/opus/<id>)").unwrap();
        assert_eq!(t.host.as_deref(), Some("bilibili.com"));
        assert_eq!(t.path_regex, r"^\/opus\/([^/?#]+)(?:[/?#]|$)");
        let t = parse_url_template("mid", "用户 mid (从 space.bilibili.com/<mid> 获取)").unwrap();
        assert_eq!(t.host.as_deref(), Some("space.bilibili.com"));
        // The first `<id>` is bare text; the one inside the path is the template.
        let t = parse_url_template("id", "design project id (the <id> in /design/p/<id>)").unwrap();
        assert_eq!(t.host, None);
        assert_eq!(t.path_regex, r"^\/design\/p\/([^/?#]+)(?:[/?#]|$)");
        let t = parse_url_template(
            "id",
            "Conversation UUID, or a https://chatgpt.com/c/<id> URL",
        )
        .unwrap();
        assert_eq!(t.host.as_deref(), Some("chatgpt.com"));
        let t = parse_url_template("gizmo", "the /g/<gizmo>/c/<id> form").unwrap();
        assert_eq!(t.host, None);
        assert_eq!(t.path_regex, r"^\/g\/([^/?#]+)(?:[/?#]|$)");
        assert!(parse_url_template("q", "search query").is_none());
    }

    #[test]
    fn rejects_bad_spec() {
        assert!(load_adapter("noslash").is_err());
        assert!(load_adapter("../etc/passwd").is_err());
    }

    // #127: `owner/repo` source resolution — accept a clean single-slash pair,
    // reject URLs, traversal, whitespace, and nested paths.
    #[test]
    fn parse_owner_repo_accepts_and_rejects() {
        assert_eq!(
            parse_owner_repo("leeguooooo/chrome-use-sites"),
            Some(("leeguooooo".into(), "chrome-use-sites".into()))
        );
        assert_eq!(parse_owner_repo("noslash"), None);
        assert_eq!(parse_owner_repo("https://example.com/x.zip"), None);
        assert_eq!(parse_owner_repo("a/b/c"), None);
        assert_eq!(parse_owner_repo("../etc/passwd"), None);
        assert_eq!(parse_owner_repo("owner /repo"), None);
        assert_eq!(parse_owner_repo("/absolute/dir"), None);
    }

    #[test]
    fn built_in_sources_include_community_and_official_packs() {
        assert_eq!(
            default_sources(),
            &["epiral/bb-sites", "leeguooooo/chrome-use-sites"]
        );
        assert!(is_default_source("epiral/bb-sites"));
        assert!(is_default_source(" leeguooooo/chrome-use-sites "));
        assert!(!is_default_source("example/private-sites"));
        assert!(!add_source(OFFICIAL_SITES_SOURCE).unwrap());
        assert!(remove_source(COMMUNITY_SITES_SOURCE)
            .unwrap_err()
            .contains("built-in default source"));
    }

    // Regression: positional args must follow DECLARATION order, not serde's
    // alphabetical key order. With `{projectId, path}` (not alphabetical),
    // `<uuid> <file>` must map projectId←uuid, path←file — not swapped.
    #[test]
    fn positional_args_follow_declaration_order_not_alphabetical() {
        let raw = r#"/* @meta
{
  "name": "claude-design/get-file",
  "domain": "claude.ai",
  "args": { "projectId": {"required": true}, "path": {"required": true} }
}
*/
async function(args){ return args; }"#;
        let a = parse_adapter(raw, "claude-design/get-file").unwrap();
        assert_eq!(a.arg_order, vec!["projectId", "path"]);
        let args = map_args(&a, &["the-uuid".into(), "misonote.dc.html".into()], &[]);
        assert_eq!(args["projectId"], "the-uuid");
        assert_eq!(args["path"], "misonote.dc.html");
    }

    #[test]
    fn loader_internals_and_tests_are_not_listed_as_adapters() {
        // Real adapters list; the loader's `_helper` family file and a pack's
        // own `*.test.js` do not (#302).
        assert!(is_runnable_adapter_stem("issues"));
        assert!(is_runnable_adapter_stem("top"));
        assert!(!is_runnable_adapter_stem("_helper"));
        assert!(!is_runnable_adapter_stem("_"));
        // `foo.test.js` → stem is `foo.test`.
        assert!(!is_runnable_adapter_stem("adapters.test"));
        assert!(!is_runnable_adapter_stem("thread.test"));
        // A normal adapter that merely contains "test" in its name still lists.
        assert!(is_runnable_adapter_stem("latest"));
        assert!(is_runnable_adapter_stem("testimonials"));
    }

    // #479: the adapter login signal, page side. Runs the real runner under
    // node with a stub `window.fetch`.
    fn run_login_case(func: &str, fetch_stub: &str) -> Option<Value> {
        let node = std::env::var_os("PATH").and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("node"))
                .find(|n| n.is_file())
        })?;
        let raw = format!(
            "/* @meta\n{{\"name\": \"demo/login\", \"domain\": \"example.com\", \"args\": {{}}}}\n*/\n{func}"
        );
        let a = parse_adapter(&raw, "demo/login").unwrap();
        let start = build_start(&a, &json!({}), None, "__cu_site_l", &json!({}))
            .replace(BUDGET_PLACEHOLDER, "1000");
        let poll = poll_script("__cu_site_l");
        let driver = format!(
            "globalThis.window = globalThis; globalThis.location = new URL('https://example.com/app/');\n\
             window.fetch = {fetch_stub};\n\
             (async () => {{ {start}; for (let i = 0; i < 50; i++) {{ await new Promise(r => setTimeout(r, 5));\n\
             const p = {poll}; if (p.state === 'done') {{ console.log(JSON.stringify(p)); return; }} }} }})();"
        );
        let out = std::process::Command::new(node)
            .arg("-e")
            .arg(driver)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Some(serde_json::from_slice(&out.stdout).unwrap())
    }

    const OK_FETCH: &str = "async (u) => ({ status: 200, ok: true, redirected: false, url: new URL(u, location.href).href, json: async () => ({}) })";
    const FETCH_401: &str = "async (u) => ({ status: 401, ok: false, redirected: false, url: new URL(u, location.href).href, json: async () => ({ error: 'Unauthorized' }) })";
    const FETCH_REDIRECT: &str = "async (u) => ({ status: 200, ok: true, redirected: true, url: 'https://example.com/zentao/user-login.html?referer=x', json: async () => { throw new Error('Unexpected token <'); } })";

    #[test]
    fn runner_marks_adapter_reported_login() {
        let Some(p) = run_login_case(
            "async function(args) { return { error: 'not_logged_in', hint: 'log in first' }; }",
            OK_FETCH,
        ) else {
            return;
        };
        assert_eq!(p["result"]["loginRequired"], true);
        assert_eq!(p["result"]["error"], "not_logged_in");
        assert_eq!(p["result"]["loginEvidence"]["source"], "adapter");
        // The documented spelling, with no error code of its own.
        let p = run_login_case(
            "async function(args) { return { loginRequired: true, loginUrl: '/login' }; }",
            OK_FETCH,
        )
        .unwrap();
        assert_eq!(p["result"]["error"], "login_required");
        assert_eq!(p["result"]["loginUrl"], "/login");
    }

    #[test]
    fn runner_infers_login_from_401_redirect_and_throw() {
        // An old adapter that misreads a 401 as a write failure.
        let Some(p) = run_login_case(
            "async function(args) { await (await fetch('/api/bugs/1')).json(); return { error: 'not_recorded' }; }",
            FETCH_401,
        ) else {
            return;
        };
        assert_eq!(p["result"]["loginRequired"], true);
        assert_eq!(p["result"]["error"], "not_recorded");
        assert_eq!(p["result"]["loginEvidence"]["source"], "http401");
        assert_eq!(
            p["result"]["loginEvidence"]["url"],
            "https://example.com/api/bugs/1"
        );
        // Redirected to a sign-in page, and the adapter threw parsing it.
        let p = run_login_case(
            "async function(args) { return (await fetch('/api/bugs/1')).json(); }",
            FETCH_REDIRECT,
        )
        .unwrap();
        assert!(p["error"].is_null(), "{p}");
        assert_eq!(p["result"]["error"], "login_required");
        assert_eq!(p["result"]["loginEvidence"]["source"], "redirect");
        assert!(p["result"]["adapterError"]
            .as_str()
            .unwrap()
            .contains("Unexpected token"));
        // A throw with no login evidence stays a throw.
        let p = run_login_case(
            "async function(args) { throw new Error('boom'); }",
            OK_FETCH,
        )
        .unwrap();
        assert!(p["error"].as_str().unwrap().contains("boom"));
    }

    #[test]
    fn runner_leaves_successful_and_unrelated_results_alone() {
        // A 401 the adapter handled itself (an optional call) and succeeded.
        let Some(p) = run_login_case(
            "async function(args) { await fetch('/api/optional'); return { items: [1] }; }",
            FETCH_401,
        ) else {
            return;
        };
        assert!(p["result"].get("loginRequired").is_none(), "{p}");
        assert_eq!(p["result"]["items"][0], 1);
        // An error with no login evidence is just an error.
        let p = run_login_case(
            "async function(args) { await fetch('/api/x'); return { error: 'HTTP 429' }; }",
            OK_FETCH,
        )
        .unwrap();
        assert!(p["result"].get("loginRequired").is_none(), "{p}");
        // Arrays and scalars pass through.
        let p = run_login_case("async function(args) { return [1, 2]; }", OK_FETCH).unwrap();
        assert_eq!(p["result"], json!([1, 2]));
    }

    #[test]
    fn site_login_wall_from_results() {
        let origin = "https://zentao.example.com/zentao/my.html";
        let r = json!({ "error": "not_logged_in", "loginRequired": true,
            "loginEvidence": { "source": "adapter" }, "loginUrl": "/zentao/user-login.html",
            "hint": "先在 Chrome 打开站点登录" });
        let w =
            site_login_wall(&r, None, origin, "zentao.example.com", "zentao/bug", true).unwrap();
        assert_eq!(w["source"], "site");
        assert_eq!(w["host"], "zentao.example.com");
        assert_eq!(
            w["loginUrl"],
            "https://zentao.example.com/zentao/user-login.html"
        );
        assert_eq!(w["returnTo"], origin);
        assert_eq!(w["rerunnable"], true);
        let hint = w["hint"].as_str().unwrap();
        assert!(
            hint.starts_with("login wall: zentao.example.com is not signed in"),
            "{hint}"
        );
        assert!(hint.contains("auth login --bwu"), "{hint}");
        assert!(
            hint.contains("`chrome-use open https://zentao.example.com/zentao/user-login.html` and `chrome-use auth login --bwu`"),
            "{hint}"
        );
        assert!(!hint.contains("先在 Chrome"), "{hint}");
        // Older adapters: the error code alone, no runner normalization.
        let w = site_login_wall(
            &json!({ "error": "Not logged in" }),
            None,
            origin,
            "",
            "x/y",
            false,
        )
        .unwrap();
        assert_eq!(w["evidence"]["source"], "adapter");
        assert_eq!(w["rerunnable"], true);
        assert!(w["loginUrl"].is_null());
        // Inferred on a write: signed in, never rerun by itself.
        let r = json!({ "error": "not_recorded", "loginRequired": true,
            "loginEvidence": { "source": "http401", "url": "https://zentao.example.com/api" } });
        let w = site_login_wall(&r, None, origin, "", "zentao/bug-comment", false).unwrap();
        assert_eq!(w["rerunnable"], false);
        assert!(w["hint"].as_str().unwrap().contains("got HTTP 401"));
        // Redirect evidence names the sign-in page.
        let r = json!({ "error": "login_required", "loginRequired": true,
            "loginEvidence": { "source": "redirect", "url": "https://zentao.example.com/zentao/user-login-x.html" } });
        let w = site_login_wall(&r, None, origin, "", "zentao/bug", true).unwrap();
        assert_eq!(
            w["loginUrl"],
            "https://zentao.example.com/zentao/user-login-x.html"
        );
        // The daemon saw the tab on a sign-in page and the adapter failed.
        let page =
            json!({ "url": "https://app.example.com/login?next=%2F", "host": "app.example.com" });
        let w = site_login_wall(
            &json!({ "error": "HTTP 500" }),
            Some(&page),
            "https://app.example.com/login?next=%2F",
            "",
            "a/b",
            true,
        )
        .unwrap();
        assert_eq!(w["evidence"]["source"], "page");
        assert_eq!(w["loginUrl"], "https://app.example.com/login?next=%2F");
        // ...back to the sign-in page itself is not a destination.
        assert!(w["returnTo"].is_null());
        // Never back to a sign-out page: it would end the new session.
        let w = site_login_wall(
            &json!({ "error": "login_required" }),
            None,
            "https://zentao.example.com/zentao/user-logout.html",
            "",
            "zentao/bug",
            true,
        )
        .unwrap();
        assert!(w["returnTo"].is_null(), "{w}");
        for u in [
            "https://a.example.com/logout",
            "https://a.example.com/auth/sign_out",
            "https://a.example.com/Log-Off.aspx",
        ] {
            assert!(logout_ish(&url::Url::parse(u).unwrap()), "{u}");
        }
        assert!(!logout_ish(
            &url::Url::parse("https://a.example.com/blog/layout").unwrap()
        ));
        // Not walls: a success, a plain failure, a success on a sign-in page.
        assert!(site_login_wall(&json!({ "items": [] }), None, origin, "", "a/b", true).is_none());
        assert!(site_login_wall(
            &json!({ "error": "HTTP 429" }),
            None,
            origin,
            "",
            "a/b",
            true
        )
        .is_none());
        assert!(
            site_login_wall(&json!({ "ok": 1 }), Some(&page), origin, "", "a/b", true).is_none()
        );
        assert!(site_login_wall(&json!([1]), None, origin, "", "a/b", true).is_none());
        // `not_recorded` alone is not a login signal.
        assert!(site_login_wall(
            &json!({ "error": "not_recorded" }),
            None,
            origin,
            "",
            "a/b",
            false
        )
        .is_none());
    }

    fn site_resp(result: Value) -> crate::connection::Response {
        crate::connection::Response {
            success: true,
            data: Some(
                json!({ "result": result, "origin": "https://z.example.com/zentao/", "domain": "z.example.com" }),
            ),
            ..Default::default()
        }
    }

    fn site_cmd(read_only: bool) -> Value {
        json!({ "action": "site", "spec": "zentao/bug", "domain": "z.example.com", "readOnly": read_only })
    }

    const WALLED: &str = r#"{"error":"not_logged_in","loginRequired":true,"loginEvidence":{"source":"adapter"},"loginUrl":"/zentao/user-login.html"}"#;

    /// Runs the flow with recording fakes; returns (resp, navigations, sign-ins, reruns).
    fn run_flow(
        cmd: &Value,
        first: Value,
        auto: bool,
        sign_in_result: Value,
        reruns: Vec<Value>,
    ) -> (crate::connection::Response, Vec<String>, usize, usize, bool) {
        let outcome = if auto {
            crate::autologin::Outcome::SignIn {
                source: "test".into(),
            }
        } else {
            crate::autologin::Outcome::Skip {
                source: "test".into(),
            }
        };
        run_flow_outcome(cmd, first, outcome, sign_in_result, reruns)
    }

    fn run_flow_outcome(
        cmd: &Value,
        first: Value,
        outcome: crate::autologin::Outcome,
        sign_in_result: Value,
        reruns: Vec<Value>,
    ) -> (crate::connection::Response, Vec<String>, usize, usize, bool) {
        let mut decide = |_w: &Value| outcome.clone();
        let mut resp = site_resp(first);
        promote_adapter_error(&mut resp);
        let mut navs: Vec<String> = Vec::new();
        let mut signs = 0usize;
        let mut ran = 0usize;
        let mut queue = reruns.into_iter();
        let mut navigate = |u: &str| {
            navs.push(u.to_string());
            Ok(())
        };
        let mut sign_in = |_w: &Value| {
            signs += 1;
            sign_in_result.clone()
        };
        let mut rerun = || {
            ran += 1;
            Ok(site_resp(queue.next().expect("unexpected rerun")))
        };
        let walled = apply_site_login_wall(
            cmd,
            &mut resp,
            SiteLoginIo {
                decide: &mut decide,
                json: true,
                navigate: &mut navigate,
                sign_in: &mut sign_in,
                rerun: &mut rerun,
            },
        );
        (resp, navs, signs, ran, walled)
    }

    #[test]
    fn undecided_site_wall_asks_the_user_through_the_agent() {
        let ask = crate::autologin::ask_payload(
            "z.example.com",
            "zt",
            Some("https://z.example.com/zentao/user-login.html"),
            true,
        );
        let (resp, navs, signs, ran, walled) = run_flow_outcome(
            &site_cmd(true),
            serde_json::from_str(WALLED).unwrap(),
            crate::autologin::Outcome::Ask(ask),
            json!({}),
            vec![],
        );
        assert!(walled);
        assert!(
            navs.is_empty() && signs == 0 && ran == 0,
            "nothing happens before the user answers"
        );
        assert!(!resp.success);
        let err = resp.error.as_deref().unwrap();
        assert!(
            err.starts_with("login wall: z.example.com needs a sign-in. Ask the user"),
            "{err}"
        );
        assert!(
            err.contains("chrome-use auth autologin always z.example.com"),
            "{err}"
        );
        let wall = &resp.data.as_ref().unwrap()["loginWall"];
        assert_eq!(wall["ask"]["options"][1]["choice"], "always");
        assert!(wall.get("autoLogin").is_none());
    }

    #[test]
    fn site_wall_without_auto_login_points_at_bwu() {
        let (resp, navs, signs, ran, walled) = run_flow(
            &site_cmd(true),
            serde_json::from_str(WALLED).unwrap(),
            false,
            json!({}),
            vec![],
        );
        assert!(walled);
        assert!(!resp.success);
        let err = resp.error.as_deref().unwrap();
        assert!(
            err.starts_with("login wall: z.example.com is not signed in"),
            "{err}"
        );
        assert!(err.contains("auth login --bwu"), "{err}");
        let wall = &resp.data.as_ref().unwrap()["loginWall"];
        assert_eq!(wall["source"], "site");
        assert!(wall.get("autoLogin").is_none());
        assert!(navs.is_empty() && signs == 0 && ran == 0);
    }

    #[test]
    fn site_wall_with_auto_login_signs_in_and_reruns_once() {
        let (resp, navs, signs, ran, walled) = run_flow(
            &site_cmd(true),
            serde_json::from_str(WALLED).unwrap(),
            true,
            json!({ "ok": true, "item": "zentao", "error": null, "returnedTo": "https://z.example.com/zentao/" }),
            vec![json!({ "id": 8876, "title": "t" })],
        );
        assert!(walled);
        assert_eq!(navs, vec!["https://z.example.com/zentao/user-login.html"]);
        assert_eq!((signs, ran), (1, 1));
        assert!(resp.success, "{:?}", resp.error);
        assert!(resp.error.is_none());
        let data = resp.data.as_ref().unwrap();
        assert_eq!(data["result"]["id"], 8876);
        assert_eq!(data["loginWall"]["autoLogin"]["ok"], true);
        assert_eq!(data["loginWall"]["rerun"]["ok"], true);

        // Still walled after signing in: reported, not looped.
        let (resp, _, signs, ran, _) = run_flow(
            &site_cmd(true),
            serde_json::from_str(WALLED).unwrap(),
            true,
            json!({ "ok": true, "item": "zentao", "error": null }),
            vec![serde_json::from_str(WALLED).unwrap()],
        );
        assert_eq!((signs, ran), (1, 1));
        assert!(!resp.success);
        assert!(resp
            .error
            .unwrap()
            .contains("still reports it is not signed in"));

        // Sign-in failed: no rerun, the error names both.
        let (resp, _, _, ran, _) = run_flow(
            &site_cmd(true),
            serde_json::from_str(WALLED).unwrap(),
            true,
            json!({ "ok": false, "item": null, "error": "the vault has 2 logins for z.example.com" }),
            vec![],
        );
        assert_eq!(ran, 0);
        let err = resp.error.unwrap();
        assert!(
            err.starts_with("login wall: auto-login failed: the vault has 2"),
            "{err}"
        );
        assert!(err.contains("auth login --bwu"), "{err}");
    }

    #[test]
    fn inferred_wall_on_a_write_signs_in_but_does_not_rerun() {
        let first = json!({ "error": "not_recorded", "loginRequired": true,
            "loginEvidence": { "source": "http401", "url": "https://z.example.com/api" } });
        let (resp, _, signs, ran, walled) = run_flow(
            &site_cmd(false),
            first,
            true,
            json!({ "ok": true, "item": "zentao", "error": null }),
            vec![],
        );
        assert!(walled);
        assert_eq!((signs, ran), (1, 0));
        assert!(!resp.success);
        assert!(resp.error.unwrap().contains("was not run again"));
        // An adapter that says so itself wrote nothing: a write is rerun.
        let (resp, _, _, ran, _) = run_flow(
            &site_cmd(false),
            serde_json::from_str(WALLED).unwrap(),
            true,
            json!({ "ok": true, "item": "zentao", "error": null }),
            vec![json!({ "commented": true })],
        );
        assert_eq!(ran, 1);
        assert!(resp.success);
    }

    #[test]
    fn signed_in_results_are_untouched() {
        for first in [
            json!({ "id": 1, "title": "ok" }),
            json!({ "error": "HTTP 429", "hint": "slow down" }),
        ] {
            let (resp, navs, signs, ran, walled) =
                run_flow(&site_cmd(true), first.clone(), true, json!({}), vec![]);
            assert!(!walled);
            assert!(navs.is_empty() && signs == 0 && ran == 0);
            assert!(resp.data.as_ref().unwrap().get("loginWall").is_none());
            if first.get("error").is_some() {
                assert_eq!(resp.error.as_deref(), Some("HTTP 429 — slow down"));
            } else {
                assert!(resp.success && resp.error.is_none());
            }
        }
    }
}
