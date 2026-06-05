//! `find-url` — search the user's local Chrome/Edge **bookmarks** for pages they
//! saved, by keyword. Borrowed from web-access's `find-url.mjs`; lets an agent
//! locate an internal system or a previously-saved page that public search
//! can't reach, without opening a browser.
//!
//! v1 covers bookmarks only (a zero-dependency JSON read). Visited-history lives
//! in a locked SQLite DB and would need a SQLite dependency — not included yet.

use std::path::PathBuf;

use serde_json::Value;

use crate::color;

struct Hit {
    name: String,
    url: String,
    folder: String,
    date_added: i64,
}

/// Entry point for the `find-url` subcommand. `args` is the full cleaned argv
/// (including the leading "find-url").
pub fn run_find_url(args: &[String], json: bool) {
    // Parse flags out of args[1..]; everything else is a keyword.
    let mut browser = "chrome".to_string();
    let mut profile = "Default".to_string();
    let mut limit: usize = 20;
    let mut keywords: Vec<String> = Vec::new();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--browser" => {
                if let Some(v) = args.get(i + 1) {
                    browser = v.to_lowercase();
                    i += 1;
                }
            }
            "--profile" => {
                if let Some(v) = args.get(i + 1) {
                    profile = v.clone();
                    i += 1;
                }
            }
            "--limit" => {
                if let Some(v) = args.get(i + 1).and_then(|s| s.parse::<usize>().ok()) {
                    limit = v;
                    i += 1;
                }
            }
            "--json" => {}
            other if other.starts_with("--") => {}
            other => keywords.push(other.to_lowercase()),
        }
        i += 1;
    }

    let path = match bookmarks_path(&browser, &profile) {
        Some(p) => p,
        None => {
            emit_error(
                json,
                &format!("Could not locate {browser} bookmarks for profile '{profile}'"),
            );
            return;
        }
    };

    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(e) => {
            emit_error(json, &format!("Failed to read {}: {e}", path.display()));
            return;
        }
    };
    let root: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            emit_error(json, &format!("Failed to parse bookmarks JSON: {e}"));
            return;
        }
    };

    let mut hits: Vec<Hit> = Vec::new();
    if let Some(roots) = root.get("roots").and_then(|r| r.as_object()) {
        for node in roots.values() {
            walk(node, "", &keywords, &mut hits);
        }
    }

    // Most-recently-added first (date_added is microseconds since 1601).
    hits.sort_by(|a, b| b.date_added.cmp(&a.date_added));
    hits.truncate(limit);

    if json {
        let arr: Vec<Value> = hits
            .iter()
            .map(|h| {
                serde_json::json!({
                    "name": h.name,
                    "url": h.url,
                    "folder": h.folder,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "success": true,
                "data": { "results": arr, "count": hits.len() },
            }))
            .unwrap_or_default()
        );
        return;
    }

    if hits.is_empty() {
        let kw = if keywords.is_empty() {
            String::new()
        } else {
            format!(" matching {:?}", keywords.join(" "))
        };
        println!("No {browser} bookmarks found{kw}.");
        return;
    }
    for h in &hits {
        if h.folder.is_empty() {
            println!("{}\n  {}", h.name, h.url);
        } else {
            println!("{}  ({})\n  {}", h.name, h.folder, h.url);
        }
    }
}

/// Recursively walk a bookmark node, collecting URL entries that match every
/// keyword (in name or url). Empty keyword list matches everything.
fn walk(node: &Value, folder: &str, keywords: &[String], out: &mut Vec<Hit>) {
    match node.get("type").and_then(|t| t.as_str()) {
        Some("url") => {
            let name = node.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let url = node.get("url").and_then(|v| v.as_str()).unwrap_or("");
            // Skip non-navigable bookmarks: javascript: bookmarklets and data:
            // URIs aren't pages you can visit, and their bodies can be huge.
            if url.is_empty()
                || url.starts_with("javascript:")
                || url.starts_with("data:")
            {
                return;
            }
            let hay = format!("{} {}", name.to_lowercase(), url.to_lowercase());
            if keywords.iter().all(|k| hay.contains(k.as_str())) {
                let date_added = node
                    .get("date_added")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(0);
                out.push(Hit {
                    name: name.to_string(),
                    url: url.to_string(),
                    folder: folder.to_string(),
                    date_added,
                });
            }
        }
        Some("folder") => {
            let fname = node.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let child_folder = if folder.is_empty() {
                fname.to_string()
            } else {
                format!("{folder}/{fname}")
            };
            if let Some(children) = node.get("children").and_then(|c| c.as_array()) {
                for child in children {
                    walk(child, &child_folder, keywords, out);
                }
            }
        }
        _ => {}
    }
}

/// Resolve the Bookmarks file path for a browser + profile across platforms.
fn bookmarks_path(browser: &str, profile: &str) -> Option<PathBuf> {
    let base = browser_user_data_dir(browser)?;
    let path = base.join(profile).join("Bookmarks");
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

/// The "User Data" directory that holds per-profile folders, per OS/browser.
fn browser_user_data_dir(browser: &str) -> Option<PathBuf> {
    let is_edge = browser == "edge" || browser == "msedge";

    #[cfg(target_os = "macos")]
    {
        let app_support = dirs::config_dir()?; // ~/Library/Application Support
        let sub = if is_edge {
            "Microsoft Edge"
        } else {
            "Google/Chrome"
        };
        Some(app_support.join(sub))
    }
    #[cfg(target_os = "windows")]
    {
        let local = dirs::data_local_dir()?; // %LOCALAPPDATA%
        let sub = if is_edge {
            "Microsoft/Edge/User Data"
        } else {
            "Google/Chrome/User Data"
        };
        Some(local.join(sub))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let config = dirs::config_dir()?; // ~/.config
        let sub = if is_edge {
            "microsoft-edge"
        } else {
            "google-chrome"
        };
        Some(config.join(sub))
    }
}

fn emit_error(json: bool, msg: &str) {
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "success": false,
                "error": msg,
            }))
            .unwrap_or_default()
        );
    } else {
        eprintln!("{} {msg}", color::error_indicator());
    }
    std::process::exit(1);
}
