//! OpenCLI compatibility: run jackwener/OpenCLI adapters as `site` commands.
//!
//! OpenCLI's adapters run in Node and call a `page` object, so they cannot be
//! evaluated in a tab like our adapters. Instead `site update` installs a pinned
//! `@jackwener/opencli` package (public npm tarball, `--ignore-scripts`) into
//! `~/.chrome-use/opencli`, and `opencli_runner.mjs` runs an adapter with
//! OpenCLI's own registry, argument coercion, pipeline executor and `BasePage`
//! helpers, over a page whose transport is chrome-use. Nothing is converted or
//! copied into our packs.
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
    if !on_path("node") || !on_path("npm") {
        return Ok(None);
    }
    let root = root().ok_or("opencli: cannot resolve home dir")?;
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let want = version();
    if installed_version(&root).as_deref() != Some(want.as_str()) {
        let out = Command::new("npm")
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
    write_runner(&root)?;
    Ok(Some(manifest().len()))
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
    let out = Command::new("node")
        .arg(&runner)
        .arg(&req_path)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output();
    let _ = std::fs::remove_file(&req_path);
    let out = match out {
        Ok(o) => o,
        Err(e) => return fail(format!("opencli: node: {e}")),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
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
    fn info_reads_like_adapter_meta() {
        let i = info(&entry());
        assert_eq!(i["name"], "bilibili/feed");
        assert_eq!(i["readOnly"], true);
        assert!(i["args"].get("limit").is_some());
        assert!(i["source"].as_str().unwrap().starts_with("opencli"));
    }
}
