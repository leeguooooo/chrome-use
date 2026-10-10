//! OpenCLI compatibility: run jackwener/OpenCLI adapters as `site` commands.
//!
//! OpenCLI's adapters run in Node and call a `page` object, so they cannot be
//! evaluated in a tab like our adapters. Instead `site update` installs a pinned
//! `@jackwener/opencli` package (public npm tarball, `--ignore-scripts`) into
//! `~/.chrome-use/opencli`, and `opencli_runner.mjs` runs an adapter with
//! OpenCLI's own registry, argument coercion, pipeline executor and `BasePage`
//! helpers, over a page whose transport is chrome-use. Nothing is converted or
//! copied into our packs; the one exception is `PATCHES`, whole-file fixes to
//! a few adapter files of the pinned release, written over the installed copy.
//!
//! Precedence: a chrome-use adapter (official, configured, then community) with
//! the same `name/cmd` always wins; OpenCLI only answers names we don't have,
//! and it is listed last in the domain hint.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

/// The OpenCLI release we install. Pinned rather than `latest` because its
/// adapters run as local Node code with the user's privileges; a version bump
/// is a deliberate change here. `AGENT_BROWSER_OPENCLI_VERSION` overrides.
pub const PINNED_VERSION: &str = "1.8.8";
pub const PACKAGE: &str = "@jackwener/opencli";
pub const SOURCE_LABEL: &str = "opencli";

const RUNNER_JS: &str = include_str!("opencli_runner.mjs");

/// chrome-use's fixes to files of the pinned OpenCLI release, as
/// `(path inside the package, full file)`. Each replaces that release's file
/// whole (the originals were first committed verbatim, so `git log -p` shows
/// the patch), so they are written only over PINNED_VERSION; an overridden
/// version runs unpatched. A version bump must re-derive or drop them, and
/// `cli/src/opencli_patches/douyin.test.mjs` runs them against the package.
/// - douyin: Douyin sends 64-bit ids (item_id) as bare JSON numbers, which
///   JSON.parse rounds; `douyin/delete` then reported a rounded item_id and
///   failed with card_not_found (#508).
const PATCHES: &[(&str, &str)] = &[
    (
        "clis/douyin/_shared/bigint-json.js",
        include_str!("opencli_patches/clis/douyin/_shared/bigint-json.js"),
    ),
    (
        "clis/douyin/_shared/browser-fetch.js",
        include_str!("opencli_patches/clis/douyin/_shared/browser-fetch.js"),
    ),
    (
        "clis/douyin/delete.js",
        include_str!("opencli_patches/clis/douyin/delete.js"),
    ),
];

pub fn disabled() -> bool {
    std::env::var_os("AGENT_BROWSER_SITES_NO_OPENCLI").is_some()
}

fn version() -> String {
    std::env::var("AGENT_BROWSER_OPENCLI_VERSION")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| PINNED_VERSION.to_string())
}

/// `~/.chrome-use/opencli` — npm prefix holding the package and the runner.
pub fn root() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".chrome-use").join("opencli"))
}

fn pkg_dir(root: &Path) -> PathBuf {
    root.join("node_modules").join("@jackwener").join("opencli")
}

fn installed_version(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(pkg_dir(root).join("package.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("version").and_then(|x| x.as_str()).map(String::from)
}

/// npm is `npm.cmd` on Windows; `Command` doesn't resolve PATHEXT for us.
fn npm() -> &'static str {
    if cfg!(windows) {
        "npm.cmd"
    } else {
        "npm"
    }
}

/// Default ceiling for one OpenCLI command, in seconds. A command that declares
/// its own `timeout` arg (login flows wait for the user) gets that plus a
/// margin. `AGENT_BROWSER_OPENCLI_TIMEOUT` (seconds) overrides the default.
const DEFAULT_TIMEOUT_SECS: u64 = 300;

fn run_timeout(kwargs: &serde_json::Map<String, Value>) -> std::time::Duration {
    let base = std::env::var("AGENT_BROWSER_OPENCLI_TIMEOUT")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    let declared = kwargs
        .get("timeout")
        .and_then(|v| v.as_str())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|t| t + 60)
        .unwrap_or(0);
    std::time::Duration::from_secs(base.max(declared))
}

fn on_path(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Install or update the pinned package. Best-effort: Ok(None) when skipped
/// (disabled, or no node/npm on PATH), Ok(Some(n)) with the command count.
pub fn sync() -> Result<Option<usize>, String> {
    if disabled() {
        return Ok(None);
    }
    if !on_path("node") || !on_path(npm()) {
        return Ok(None);
    }
    let root = root().ok_or("opencli: cannot resolve home dir")?;
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let want = version();
    if installed_version(&root).as_deref() != Some(want.as_str()) {
        let out = Command::new(npm())
            .arg("install")
            .arg("--prefix")
            .arg(&root)
            .arg(format!("{PACKAGE}@{want}"))
            .args([
                "--omit=dev",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                "--no-save",
                "--loglevel=error",
            ])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("opencli: npm: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "opencli: npm install {PACKAGE}@{want} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    }
    apply_patches(&pkg_dir(&root), installed_version(&root).as_deref())?;
    write_runner(&root)?;
    Ok(Some(manifest().len()))
}

/// Write `PATCHES` over the package in `pkg` when it is the pinned release.
/// Idempotent; returns how many files were (re)written.
fn apply_patches(pkg: &Path, installed: Option<&str>) -> Result<usize, String> {
    if installed != Some(PINNED_VERSION) {
        return Ok(0);
    }
    let mut written = 0;
    for (rel, body) in PATCHES {
        let path = pkg.join(rel);
        if std::fs::read_to_string(&path).ok().as_deref() == Some(*body) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("opencli: patch {rel}: {e}"))?;
        }
        std::fs::write(&path, body).map_err(|e| format!("opencli: patch {rel}: {e}"))?;
        written += 1;
    }
    Ok(written)
}

fn write_runner(root: &Path) -> Result<PathBuf, String> {
    let path = root.join("runner.mjs");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(RUNNER_JS) {
        std::fs::write(&path, RUNNER_JS).map_err(|e| format!("opencli: write runner: {e}"))?;
    }
    Ok(path)
}

/// The package's `cli-manifest.json` entries (one per command), or empty.
pub fn manifest() -> Vec<Value> {
    if disabled() {
        return Vec::new();
    }
    let Some(root) = root() else {
        return Vec::new();
    };
    std::fs::read_to_string(pkg_dir(&root).join("cli-manifest.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok())
        .unwrap_or_default()
}

pub fn spec_of(entry: &Value) -> Option<String> {
    let site = entry.get("site")?.as_str()?;
    let name = entry.get("name")?.as_str()?;
    Some(format!("{site}/{name}"))
}

/// The manifest entry for `site/name`, if OpenCLI has it.
pub fn lookup(spec: &str) -> Option<Value> {
    manifest()
        .into_iter()
        .find(|e| spec_of(e).as_deref() == Some(spec))
}

/// Whether `spec` should run through OpenCLI: we have no adapter by that name
/// and OpenCLI does.
pub fn handles(spec: &str) -> bool {
    spec.contains('/') && crate::site::load_adapter(spec).is_err() && lookup(spec).is_some()
}

/// `site info` for an OpenCLI command, shaped like an adapter's @meta.
pub fn info(entry: &Value) -> Value {
    let mut args = serde_json::Map::new();
    for a in entry
        .get("args")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        if let Some(name) = a.get("name").and_then(|v| v.as_str()) {
            args.insert(name.to_string(), a.clone());
        }
    }
    json!({
        "name": spec_of(entry),
        "description": entry.get("description"),
        "domain": entry.get("domain"),
        "readOnly": entry.get("access").and_then(|v| v.as_str()) == Some("read"),
        "strategy": entry.get("strategy"),
        "args": args,
        "columns": entry.get("columns"),
        "source": format!("{SOURCE_LABEL} ({PACKAGE}@{})", version()),
    })
}

/// Map CLI args onto the command's declared args: positional args fill the
/// ones marked `positional` in order, `--key value` sets by name, and a bare
/// `--flag` sets a boolean arg. Values stay strings; OpenCLI coerces them.
pub fn map_args(entry: &Value, rest: &[String]) -> Result<serde_json::Map<String, Value>, String> {
    let declared: Vec<&Value> = entry
        .get("args")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let positional: Vec<&str> = declared
        .iter()
        .filter(|a| a.get("positional").and_then(|v| v.as_bool()) == Some(true))
        .filter_map(|a| a.get("name").and_then(|v| v.as_str()))
        .collect();
    let is_bool = |name: &str| {
        declared.iter().any(|a| {
            a.get("name").and_then(|v| v.as_str()) == Some(name)
                && matches!(
                    a.get("type").and_then(|v| v.as_str()),
                    Some("bool") | Some("boolean")
                )
        })
    };
    let mut out = serde_json::Map::new();
    let mut pos = positional.iter();
    let mut it = rest.iter().peekable();
    while let Some(a) = it.next() {
        if let Some(key) = a.strip_prefix("--") {
            let (key, inline) = match key.split_once('=') {
                Some((k, v)) => (k, Some(v.to_string())),
                None => (key, None),
            };
            let value = match inline {
                Some(v) => v,
                None if is_bool(key) && it.peek().is_none_or(|n| n.starts_with("--")) => {
                    "true".to_string()
                }
                None => it
                    .next()
                    .cloned()
                    .ok_or_else(|| format!("site: --{key} needs a value"))?,
            };
            out.insert(key.to_string(), Value::String(value));
        } else {
            let name = pos.next().ok_or_else(|| {
                format!(
                    "site: unexpected argument `{a}` (positional args: {})",
                    if positional.is_empty() {
                        "none".to_string()
                    } else {
                        positional.join(", ")
                    }
                )
            })?;
            out.insert(name.to_string(), Value::String(a.clone()));
        }
    }
    Ok(out)
}

/// Same-meaning arg names across packs, for a fallback from one of our
/// adapters to OpenCLI's command of the same name.
const ARG_ALIASES: &[&[&str]] = &[
    &["limit", "count", "n", "num", "max", "size"],
    &["query", "q", "keyword", "keywords", "term"],
    &["id", "item", "itemid"],
    &["user", "username", "uid", "userid"],
];

/// Turn our adapter's resolved args (`{name: value}`) into `--name value`
/// pairs OpenCLI's command declares, renaming through ARG_ALIASES and dropping
/// what it doesn't take.
pub fn fallback_args(entry: &Value, ours: &Value) -> Vec<String> {
    let declared: Vec<String> = entry
        .get("args")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|a| {
            a.get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_ascii_lowercase())
        })
        .collect();
    let mut out = Vec::new();
    for (k, v) in ours.as_object().into_iter().flatten() {
        let value = match v {
            Value::String(s) => s.clone(),
            Value::Null => continue,
            other => other.to_string(),
        };
        let key = k.to_ascii_lowercase();
        let target = if declared.contains(&key) {
            Some(key)
        } else {
            ARG_ALIASES
                .iter()
                .find(|group| group.contains(&key.as_str()))
                .and_then(|group| group.iter().find(|n| declared.iter().any(|d| d == *n)))
                .map(|n| n.to_string())
        };
        if let Some(t) = target {
            out.push(format!("--{t}"));
            out.push(value);
        }
    }
    out
}

/// Run `site/name` through OpenCLI's runtime. Returns the runner's envelope:
/// `{success, data}` or `{success: false, error, hint?}`.
pub fn run(spec: &str, entry: &Value, rest: &[String], session: &str) -> Value {
    let fail = |e: String| json!({ "success": false, "error": e });
    if !on_path("node") {
        return fail(format!(
            "site {spec} comes from OpenCLI, which needs Node.js 20+ on PATH (https://nodejs.org)"
        ));
    }
    let Some(root) = root() else {
        return fail("opencli: cannot resolve home dir".into());
    };
    let runner = match write_runner(&root) {
        Ok(p) => p,
        Err(e) => return fail(e),
    };
    // A package installed by an older chrome-use has no patches yet.
    if let Err(e) = apply_patches(&pkg_dir(&root), installed_version(&root).as_deref()) {
        return fail(e);
    }
    let kwargs = match map_args(entry, rest) {
        Ok(k) => k,
        Err(e) => return fail(e),
    };
    let (site, name) = spec.split_once('/').unwrap_or((spec, ""));
    let chrome_use = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "chrome-use".to_string());
    let req = json!({
        "pkgDir": pkg_dir(&root),
        "chromeUse": chrome_use,
        "session": session,
        "site": site,
        "name": name,
        "modulePath": entry.get("modulePath"),
        "kwargs": kwargs,
    });
    let req_path =
        std::env::temp_dir().join(format!("cu-opencli-{}.json", uuid::Uuid::new_v4().simple()));
    if let Err(e) = std::fs::write(&req_path, req.to_string()) {
        return fail(format!("opencli: write request: {e}"));
    }
    let limit = run_timeout(&kwargs);
    let child = Command::new("node")
        .arg(&runner)
        .arg(&req_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&req_path);
            return fail(format!("opencli: node: {e}"));
        }
    };
    // Read stdout on a thread so a chatty command can't fill the pipe while
    // we wait on the deadline.
    let mut pipe = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(p) = pipe.as_mut() {
            let _ = std::io::Read::read_to_string(p, &mut buf);
        }
        buf
    });
    let started = std::time::Instant::now();
    let timed_out = loop {
        match child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) if started.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                break true;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(_) => break false,
        }
    };
    let _ = std::fs::remove_file(&req_path);
    let stdout = reader.join().unwrap_or_default();
    if timed_out {
        return fail(format!(
            "site {spec} (OpenCLI) did not finish within {}s; the page may still be working. \
             Set AGENT_BROWSER_OPENCLI_TIMEOUT=<seconds> for a longer limit",
            limit.as_secs()
        ));
    }
    stdout
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<Value>(l).ok())
        .unwrap_or_else(|| fail(format!("opencli: {spec} produced no result")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> Value {
        json!({
            "site": "bilibili", "name": "feed", "domain": "www.bilibili.com", "access": "read",
            "args": [
                {"name": "uid", "positional": true},
                {"name": "limit", "type": "int"},
                {"name": "verbose", "type": "bool"}
            ]
        })
    }

    #[test]
    fn maps_positional_named_and_bare_bool_args() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let m = map_args(&entry(), &s(&["123", "--limit", "5", "--verbose"])).unwrap();
        assert_eq!(m["uid"], "123");
        assert_eq!(m["limit"], "5");
        assert_eq!(m["verbose"], "true");
        let m = map_args(&entry(), &s(&["--limit=7"])).unwrap();
        assert_eq!(m["limit"], "7");
        assert!(map_args(&entry(), &s(&["a", "b"])).is_err());
        assert!(map_args(&entry(), &s(&["--limit"])).is_err());
    }

    #[test]
    fn timeout_defaults_and_follows_a_declared_timeout_arg() {
        let mut k = serde_json::Map::new();
        assert_eq!(run_timeout(&k).as_secs(), DEFAULT_TIMEOUT_SECS);
        k.insert("timeout".into(), json!("600"));
        assert_eq!(run_timeout(&k).as_secs(), 660);
        k.insert("timeout".into(), json!("10"));
        assert_eq!(run_timeout(&k).as_secs(), DEFAULT_TIMEOUT_SECS);
    }

    #[test]
    fn fallback_args_rename_through_aliases_and_drop_unknown() {
        let theirs = json!({"args": [{"name": "limit"}, {"name": "query"}]});
        let ours = json!({"count": 3, "q": "rust", "sort": "new", "x": null});
        let a = fallback_args(&theirs, &ours);
        let pairs: Vec<_> = a
            .chunks(2)
            .map(|c| (c[0].as_str(), c[1].as_str()))
            .collect();
        assert!(pairs.contains(&("--limit", "3")), "{a:?}");
        assert!(pairs.contains(&("--query", "rust")), "{a:?}");
        assert_eq!(pairs.len(), 2, "{a:?}");
    }

    #[test]
    fn patches_apply_only_to_the_pinned_release_and_are_idempotent() {
        let dir =
            std::env::temp_dir().join(format!("cu-oc-patch-{}", uuid::Uuid::new_v4().simple()));
        let pkg = dir.join("pkg");
        std::fs::create_dir_all(pkg.join("clis/douyin")).unwrap();
        std::fs::write(pkg.join("clis/douyin/delete.js"), "upstream").unwrap();

        assert_eq!(apply_patches(&pkg, Some("0.0.1")).unwrap(), 0);
        assert_eq!(apply_patches(&pkg, None).unwrap(), 0);
        assert_eq!(
            std::fs::read_to_string(pkg.join("clis/douyin/delete.js")).unwrap(),
            "upstream"
        );

        assert_eq!(
            apply_patches(&pkg, Some(PINNED_VERSION)).unwrap(),
            PATCHES.len()
        );
        for (rel, body) in PATCHES {
            assert_eq!(
                std::fs::read_to_string(pkg.join(rel)).unwrap(),
                *body,
                "{rel}"
            );
        }
        assert_eq!(apply_patches(&pkg, Some(PINNED_VERSION)).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn douyin_patches_keep_64_bit_ids_as_strings() {
        let body = |rel: &str| PATCHES.iter().find(|(r, _)| *r == rel).unwrap().1;
        assert!(
            body("clis/douyin/_shared/browser-fetch.js").contains("parseJsonKeepingBigInts(text)")
        );
        let delete = body("clis/douyin/delete.js");
        assert!(delete.contains("parseJsonKeepingBigInts(await res.text())"));
        assert!(!delete.contains("res.json()"));
    }

    #[test]
    fn info_reads_like_adapter_meta() {
        let i = info(&entry());
        assert_eq!(i["name"], "bilibili/feed");
        assert_eq!(i["readOnly"], true);
        assert!(i["args"].get("limit").is_some());
        assert!(i["source"].as_str().unwrap().starts_with("opencli"));
    }
}
