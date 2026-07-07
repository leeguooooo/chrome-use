//! Site adapters: turn any website into a structured-data CLI by running a small
//! per-command JS adapter inside your real, logged-in browser tab (it reuses the
//! site's cookies / same-origin fetch / its own webpack modules — the site thinks
//! it's you, because it is).
//!
//! The adapter format is the community **bb-sites** convention
//! (<https://github.com/epiral/bb-sites>): one `.js` file per command, a
//! `/* @meta {...} */` JSON header (name, description, domain, args), then an
//! `async function(args){ ... return {...} }`. chrome-use ships none of those
//! adapters — `chrome-use site update` fetches the upstream repo at runtime into
//! `~/.chrome-use/sites` (like a package manager pulling a dependency), so the
//! adapters stay the property of their authors. Running an adapter navigates to
//! its `@meta.domain` and `eval`s the function in the site's own logged-in page.

use std::path::PathBuf;

use serde_json::Value;

const SITES_ZIP_URL: &str = "https://github.com/epiral/bb-sites/archive/refs/heads/main.zip";

/// `~/.chrome-use/sites` — where synced adapters live.
pub fn sites_dir() -> Option<PathBuf> {
    dirs_home().map(|h| h.join(".chrome-use").join("sites"))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
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

/// Download the bb-sites repo zip and extract its adapters into `~/.chrome-use/sites`.
pub async fn update() -> Result<usize, String> {
    let dir = sites_dir().ok_or("site: cannot resolve home dir")?;
    let client = reqwest::Client::builder()
        .user_agent("chrome-use")
        .build()
        .map_err(|e| e.to_string())?;
    let bytes = client
        .get(SITES_ZIP_URL)
        .send()
        .await
        .map_err(|e| format!("site update: download failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("site update: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("site update: read body: {e}"))?;

    let cursor = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(cursor).map_err(|e| format!("site update: bad zip: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut count = 0usize;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
        let Some(enclosed) = f.enclosed_name() else {
            continue;
        };
        // Strip the top-level `bb-sites-main/` component from the archive path.
        let rel: PathBuf = enclosed.components().skip(1).collect();
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out = dir.join(&rel);
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
    // Build the domain→adapters index and stamp the sync time so navigation can
    // suggest adapters (auto-trigger) and `needs_refresh` can pace re-syncs.
    write_domain_index(&dir);
    if let Some(p) = last_update_path() {
        let _ = std::fs::write(p, now_secs().to_string());
    }
    Ok(count)
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

/// Whether the adapter pack should be (re)synced: true on first use (nothing
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
/// Reads the prebuilt `.index.json`; empty if the pack isn't synced yet.
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
