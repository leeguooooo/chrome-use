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

use serde_json::Value;

pub const COMMUNITY_SITES_SOURCE: &str = "epiral/bb-sites";
pub const OFFICIAL_SITES_SOURCE: &str = "leeguooooo/chrome-use-sites";
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

/// Build the JS to eval: `(<adapter function>)(<args JSON>)`. The adapter's
/// `async function(args)` returns a promise; chrome-use's eval awaits it. When
/// the family ships a `_helper` module, define it in an enclosing scope the
/// adapter expression closes over, so helper calls resolve instead of throwing
/// `ReferenceError: findGraphQLQueryId is not defined` (#99).
pub fn build_eval(adapter: &Adapter, args: &Value, helper_src: Option<&str>) -> String {
    let args_json = serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string());
    match helper_src {
        Some(h) if !h.trim().is_empty() => format!(
            "(() => {{\n{h}\n;\nreturn ({func})({args});\n}})()",
            h = h,
            func = adapter.func_src,
            args = args_json,
        ),
        _ => format!("({})({})", adapter.func_src, args_json),
    }
}

/// List installed adapters as `name/cmd` strings (sorted).
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
                    out.push(format!("{name}/{stem}"));
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
    for source in default_sources() {
        sync_source(&client, source, None, &dir)
            .await
            .map_err(|e| format!("site update: default source `{source}` failed: {e}"))?;
    }

    // 2) Extra sources (private/org packs). Best-effort, overlaid on top. Built-in
    // names are filtered by read_sources() for compatibility with old config files.
    let token = sources_token();
    for source in read_sources() {
        if let Err(e) = sync_source(&client, &source, token.as_deref(), &dir).await {
            eprintln!("site update: source `{source}` skipped: {e}");
        }
    }

    // Total adapter count after all overlays.
    let count = list_adapters().map(|l| l.len()).unwrap_or(0);
    // Build the domain→adapters index and stamp the sync time so navigation can
    // suggest adapters (auto-trigger) and `needs_refresh` can pace re-syncs.
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
) -> Result<usize, String> {
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
) -> Result<usize, String> {
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
    let mut count = 0usize;
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
        if out.extension().and_then(|e| e.to_str()) == Some("js") {
            count += 1;
        }
    }
    Ok(count)
}

/// Copy a local adapter tree (a directory of `<name>/<cmd>.js` packs) into `dir`.
fn copy_local_tree(src: &std::path::Path, dir: &std::path::Path) -> Result<usize, String> {
    let mut count = 0usize;
    for entry in walkdir_js(src) {
        let rel = entry.strip_prefix(src).map_err(|e| e.to_string())?;
        let out = dir.join(rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::copy(&entry, &out).map_err(|e| e.to_string())?;
        if out.extension().and_then(|e| e.to_str()) == Some("js") {
            count += 1;
        }
    }
    Ok(count)
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
    let ordered: std::collections::BTreeMap<String, Vec<String>> = by_domain
        .into_iter()
        .map(|(domain, mut v)| {
            // read-only (true) first, then by spec name
            v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            (domain, v.into_iter().map(|(_, s)| s).collect())
        })
        .collect();
    if let Ok(json) = serde_json::to_string(&ordered) {
        let _ = std::fs::write(dir.join(".index.json"), json);
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
    let Some(raw) = index_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return Vec::new();
    };
    let Ok(idx) = serde_json::from_str::<std::collections::BTreeMap<String, Vec<String>>>(&raw)
    else {
        return Vec::new();
    };
    // Preserve the index's per-domain ordering (read-only adapters first); just
    // dedup if a host somehow matches multiple domain keys.
    let mut out: Vec<String> = Vec::new();
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
    out
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
}
