use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::io::{self, BufRead};

use crate::color;
use crate::flags::Flags;
use crate::validation::{is_valid_session_name, session_name_error};

/// Error type for command parsing with contextual information
#[derive(Debug)]
pub enum ParseError {
    /// Command does not exist
    UnknownCommand { command: String },
    /// Command exists but subcommand is invalid
    UnknownSubcommand {
        subcommand: String,
        valid_options: &'static [&'static str],
    },
    /// Command/subcommand exists but required arguments are missing
    MissingArguments {
        context: String,
        usage: &'static str,
    },
    /// Argument exists but has an invalid value
    InvalidValue {
        message: String,
        usage: &'static str,
    },
    /// Invalid session name (path traversal or invalid characters)
    InvalidSessionName { name: String },
}

/// Top-level commands an agent is likely to mistype, used for "did you mean"
/// suggestions on an unknown command (issue #29). Not exhaustive — just the
/// common verbs plus a few known wrong-guesses mapped to the real command.
const KNOWN_COMMANDS: &[&str] = &[
    "open",
    "navigate",
    "read",
    "expect",
    "extract",
    "form",
    "click",
    "fill",
    "type",
    "press",
    "snapshot",
    "screenshot",
    "eval",
    "get",
    "text",
    "html",
    "frames",
    "find",
    "wait",
    "scroll",
    "hover",
    "select",
    "check",
    "uncheck",
    "tab",
    "tabs",
    "close",
    "back",
    "forward",
    "reload",
    "sessions",
    "status",
    "daemon",
    "doctor",
    "upgrade",
    "connect",
    "cookies",
    "mouse",
    "keyboard",
    "stream",
    "frame",
    "profiles",
    "title",
    "url",
    "is",
    "drag",
    "solve-slider",
    "dialog",
    "upload",
    "site",
    "box",
    "adopt",
    "canvas",
    "viewport",
    "resize",
    "keep",
];

/// Parse a `drag` offset argument: `60`, `+60`, `-12`, or `60,-3` (dx[,dy]).
/// Returns `None` for anything that isn't a pure pixel offset — those fall
/// through to the normal ref/selector/text target path.
fn parse_drag_offset(s: &str) -> Option<(f64, f64)> {
    let s = s.trim();
    let (dx_str, dy_str) = match s.split_once(',') {
        Some((x, y)) => (x.trim(), Some(y.trim())),
        None => (s, None),
    };
    // Reject empties and bare signs; require the whole token to be numeric so a
    // selector like `.foo` or `@e5` never gets mistaken for an offset.
    let dx: f64 = dx_str.parse().ok()?;
    let dy: f64 = match dy_str {
        Some(y) => y.parse().ok()?,
        None => 0.0,
    };
    if !dx.is_finite() || !dy.is_finite() {
        return None;
    }
    Some((dx, dy))
}

/// Levenshtein distance, capped — small inputs only (command names).
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Closest known command within a small edit distance, or a prefix/substring
/// match — `None` if nothing is close enough to suggest confidently.
fn nearest_command(input: &str) -> Option<String> {
    let lower = input.to_lowercase();
    // Exact prefix/substring hits first (e.g. "session" -> "sessions").
    if let Some(c) = KNOWN_COMMANDS
        .iter()
        .find(|c| c.starts_with(&lower) || lower.starts_with(**c))
    {
        return Some(c.to_string());
    }
    // Tolerance scales with length: short words get distance 1, longer get 2.
    let max_dist = if lower.len() <= 4 { 1 } else { 2 };
    KNOWN_COMMANDS
        .iter()
        .map(|c| (*c, edit_distance(&lower, c)))
        .filter(|(_, d)| *d <= max_dist)
        .min_by_key(|(_, d)| *d)
        .map(|(c, _)| c.to_string())
}

impl ParseError {
    pub fn format(&self) -> String {
        match self {
            ParseError::UnknownCommand { command } => match nearest_command(command) {
                Some(suggestion) => format!(
                    "Unknown command: {}\nDid you mean: chrome-use {}?",
                    command, suggestion
                ),
                None => format!("Unknown command: {}", command),
            },
            ParseError::UnknownSubcommand {
                subcommand,
                valid_options,
            } => {
                format!(
                    "Unknown subcommand: {}\nValid options: {}",
                    subcommand,
                    valid_options.join(", ")
                )
            }
            ParseError::MissingArguments { context, usage } => {
                format!(
                    "Missing arguments for: {}\nUsage: chrome-use {}",
                    context, usage
                )
            }
            ParseError::InvalidValue { message, usage } => {
                format!("{}\nUsage: chrome-use {}", message, usage)
            }
            ParseError::InvalidSessionName { name } => session_name_error(name),
        }
    }
}

pub fn gen_id() -> String {
    format!(
        "r{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros()
            % 1000000
    )
}

/// Parse a cookies file in one of three auto-detected formats:
///
/// 1. JSON array — `[{"name":"x","value":"y"}, ...]`
/// 2. cURL dump  — the output of DevTools → Network → Copy → Copy as cURL
///    (the Cookie header is extracted from `-H 'cookie: ...'` or
///    `-b '...'`/`--cookie '...'`)
/// 3. Bare cookie header — `name=value; name2=value2`
///
/// Returns a JSON array of cookie objects (each `{ name, value }`) suitable
/// for the `cookies_set` daemon action. Error text never echoes the secret
/// value.
pub fn parse_curl_cookies(raw: &str) -> Result<Vec<Value>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("cookies file is empty".to_string());
    }

    if trimmed.starts_with('[') {
        let arr: Vec<Value> = serde_json::from_str(trimmed)
            .map_err(|e| format!("cookies JSON parse error: {}", e))?;
        let mut out = Vec::with_capacity(arr.len());
        for (i, c) in arr.into_iter().enumerate() {
            let name = c
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("cookies[{}] missing string name", i))?;
            let value = c
                .get("value")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("cookies[{}] missing string value", i))?;
            let mut cookie = json!({ "name": name, "value": value });
            let obj = cookie.as_object_mut().unwrap();
            // Preserve any CDP Network.setCookie attributes present on the
            // source object so a full auth state round-trips: httpOnly session
            // tokens, per-domain cookies (a single export spans .chatgpt.com,
            // .openai.com, ...), and secure/sameSite/expiry. A bare
            // {name,value} export is unchanged. Common aliases from DevTools /
            // EditThisCookie / extension exports are accepted.
            if let Some(v) = c.get("url").and_then(|v| v.as_str()) {
                obj.insert("url".into(), json!(v));
            }
            if let Some(v) = c.get("domain").and_then(|v| v.as_str()) {
                obj.insert("domain".into(), json!(v));
            }
            if let Some(v) = c.get("path").and_then(|v| v.as_str()) {
                obj.insert("path".into(), json!(v));
            }
            if let Some(v) = c.get("secure").and_then(|v| v.as_bool()) {
                obj.insert("secure".into(), json!(v));
            }
            if let Some(v) = c
                .get("httpOnly")
                .or_else(|| c.get("httponly"))
                .or_else(|| c.get("http_only"))
                .and_then(|v| v.as_bool())
            {
                obj.insert("httpOnly".into(), json!(v));
            }
            if let Some(v) = c
                .get("sameSite")
                .or_else(|| c.get("samesite"))
                .or_else(|| c.get("same_site"))
                .and_then(|v| v.as_str())
            {
                let norm = match v.to_lowercase().as_str() {
                    "strict" => "Strict",
                    "lax" => "Lax",
                    "none" | "no_restriction" => "None",
                    _ => "",
                };
                if !norm.is_empty() {
                    obj.insert("sameSite".into(), json!(norm));
                }
            }
            // CDP `expires` is seconds since the Unix epoch (f64). Accept
            // `expires` or EditThisCookie's `expirationDate`.
            if let Some(v) = c
                .get("expires")
                .or_else(|| c.get("expirationDate"))
                .and_then(|v| v.as_f64())
            {
                if v > 0.0 {
                    obj.insert("expires".into(), json!(v));
                }
            }
            out.push(cookie);
        }
        return Ok(out);
    }

    // Heuristic: cURL commands start with `curl` followed by space/quote.
    let looks_like_curl = {
        let head: String = trimmed.chars().take(5).collect::<String>().to_lowercase();
        head.starts_with("curl") && head.len() > 4 && {
            let c = head.chars().nth(4).unwrap();
            c.is_whitespace() || c == '\'' || c == '"'
        }
    };

    let header = if looks_like_curl {
        extract_cookie_header_from_curl(trimmed).ok_or_else(|| {
            "no Cookie header found in this cURL - right-click an authenticated request in DevTools → Network → Copy → Copy as cURL".to_string()
        })?
    } else {
        trimmed.to_string()
    };

    parse_cookie_header(&header)
}

fn extract_cookie_header_from_curl(curl: &str) -> Option<String> {
    // Strip bash (`\`) and cmd (`^`) line continuations so -H is on one line.
    let joined = curl
        .replace("\\\r\n", " ")
        .replace("\\\n", " ")
        .replace("^\r\n", " ")
        .replace("^\n", " ");
    if let Some(v) = match_quoted_arg(&joined, "-H", Some("cookie")) {
        return Some(v);
    }
    if let Some(v) = match_quoted_arg(&joined, "-b", None) {
        return Some(v);
    }
    if let Some(v) = match_quoted_arg(&joined, "--cookie", None) {
        return Some(v);
    }
    None
}

/// Find `flag <quote>[header:]value<quote>` in haystack and return the value.
/// When `expect_header` is set, the quoted value must start with that header
/// name followed by a colon (case-insensitive) and the prefix is stripped.
fn match_quoted_arg(haystack: &str, flag: &str, expect_header: Option<&str>) -> Option<String> {
    let bytes = haystack.as_bytes();
    let flag_b = flag.as_bytes();
    let mut i = 0;
    while i + flag_b.len() < bytes.len() {
        if &bytes[i..i + flag_b.len()] != flag_b {
            i += 1;
            continue;
        }
        // Must be at a word boundary on the left (start of string or whitespace).
        if i > 0 && !bytes[i - 1].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let mut j = i + flag_b.len();
        // Require a whitespace separator after the flag.
        if j >= bytes.len() || !bytes[j].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() {
            return None;
        }
        let quote = bytes[j];
        if quote != b'\'' && quote != b'"' {
            i = j;
            continue;
        }
        let start = j + 1;
        let mut k = start;
        while k < bytes.len() && bytes[k] != quote {
            k += 1;
        }
        if k >= bytes.len() {
            return None;
        }
        let value = String::from_utf8_lossy(&bytes[start..k]).into_owned();
        if let Some(header) = expect_header {
            let lower = value.to_lowercase();
            let prefix = format!("{}:", header.to_lowercase());
            if let Some(stripped) = lower.strip_prefix(&prefix) {
                let _ = stripped;
                return Some(value[prefix.len()..].trim().to_string());
            }
            i = k + 1;
            continue;
        }
        return Some(value);
    }
    None
}

fn parse_cookie_header(header: &str) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    for piece in header.split(';') {
        let piece = piece.trim();
        let Some(eq) = piece.find('=') else { continue };
        let name = piece[..eq].trim();
        let value = piece[eq + 1..].trim();
        if !name.is_empty() {
            out.push(json!({ "name": name, "value": value }));
        }
    }
    if out.is_empty() {
        return Err("no cookies found in input".to_string());
    }
    Ok(out)
}

pub fn parse_command(args: &[String], flags: &Flags) -> Result<Value, ParseError> {
    let mut result = parse_command_inner(args, flags)?;

    // Inject AGENT_BROWSER_DEFAULT_TIMEOUT into any wait-family command that
    // doesn't already carry an explicit timeout. Centralised here so that new
    // wait variants automatically inherit the default without per-variant wiring.
    if let Some(action) = result.get("action").and_then(|a| a.as_str()) {
        if action.starts_with("wait") && result.get("timeout").is_none() {
            if let Some(t) = flags.default_timeout {
                result["timeout"] = json!(t);
            }
        }
    }

    // Action guards (#65 followup): stamp the `--if-present`/`--optional` flag onto
    // the action so the daemon turns an "element absent" error into a skip.
    if flags.if_present {
        if let Some(obj) = result.as_object_mut() {
            obj.insert("ifPresent".to_string(), json!(true));
        }
    }
    // `--observe`: stamp so the daemon returns the post-action a11y delta.
    if flags.observe {
        if let Some(obj) = result.as_object_mut() {
            obj.insert("observe".to_string(), json!(true));
        }
    }

    Ok(result)
}

fn parse_command_inner(args: &[String], flags: &Flags) -> Result<Value, ParseError> {
    if args.is_empty() {
        return Err(ParseError::MissingArguments {
            context: "".to_string(),
            usage: "<command> [args...]",
        });
    }

    let cmd = args[0].as_str();
    let rest: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();
    let id = gen_id();

    if flags.cli_annotate && cmd != "screenshot" {
        eprintln!(
            "{} --annotate only applies to the screenshot command",
            color::warning_indicator()
        );
    }

    match cmd {
        // === Navigation ===
        // Maps to "navigate" action in protocol; reflected in ACTION_CATEGORIES in action-policy.ts
        "open" | "goto" | "navigate" => {
            // `open` without a URL launches the browser but stays on
            // about:blank. Lets agents set up routes, cookies, or init
            // scripts before the first real navigation (see `batch`).
            // `goto` and `navigate` still require a URL since those verbs
            // imply the navigation itself.
            // The URL is the first positional arg, skipping flags AND any value
            // consumed by `--wait-until` (so it isn't mistaken for the URL).
            let first_url = {
                let mut url = None;
                let mut skip_next = false;
                for a in &rest {
                    if skip_next {
                        skip_next = false;
                        continue;
                    }
                    if *a == "--wait-until" {
                        skip_next = true;
                        continue;
                    }
                    if !a.starts_with("--") {
                        url = Some(a);
                        break;
                    }
                }
                url
            };
            let url = match first_url {
                Some(u) => *u,
                None if cmd == "open" => {
                    return Ok(json!({ "id": id, "action": "launch", "headless": !flags.headed }));
                }
                None => {
                    return Err(ParseError::MissingArguments {
                        context: cmd.to_string(),
                        usage: "goto <url>",
                    });
                }
            };
            let url_lower = url.to_lowercase();
            let url = if url_lower.starts_with("http://")
                || url_lower.starts_with("https://")
                || url_lower.starts_with("about:")
                || url_lower.starts_with("data:")
                || url_lower.starts_with("file:")
                || url_lower.starts_with("chrome-extension://")
                || url_lower.starts_with("chrome://")
            {
                url.to_string()
            } else {
                format!("https://{}", url)
            };
            let mut nav_cmd = json!({ "id": id, "action": "navigate", "url": url });
            if flags.provider.is_some() {
                nav_cmd["waitUntil"] = json!("none");
            }
            // `--reuse-tab`: adopt an existing tab already on this URL instead of
            // navigating/spawning a new one (issue #21 — avoids duplicate tabs on
            // rebind, preserves in-page state).
            if rest.iter().any(|a| *a == "--reuse-tab" || *a == "--reuse") {
                nav_cmd["reuseTab"] = json!(true);
            }
            // Explicit readiness override (issue #10): SPAs whose `load` event
            // never fires (a long-lived XHR/websocket holds it open) hang out the
            // load-event wait. `--wait-until domcontentloaded` returns as soon as
            // the DOM is parsed.
            if let Some(i) = rest.iter().position(|a| *a == "--wait-until") {
                let val = rest.get(i + 1).ok_or(ParseError::MissingArguments {
                    context: "open --wait-until".to_string(),
                    usage: "open <url> --wait-until <load|domcontentloaded|networkidle|none>",
                })?;
                if !["load", "domcontentloaded", "networkidle", "none"].contains(val) {
                    return Err(ParseError::InvalidValue {
                        message: format!("Unknown --wait-until value: {}", val),
                        usage: "open <url> --wait-until <load|domcontentloaded|networkidle|none>",
                    });
                }
                nav_cmd["waitUntil"] = json!(val);
            }
            if let Some(ref headers_json) = flags.headers {
                let headers =
                    serde_json::from_str::<serde_json::Value>(headers_json).map_err(|_| {
                        ParseError::InvalidValue {
                            message: format!("Invalid JSON for --headers: {}", headers_json),
                            usage: "open <url> --headers '{\"Key\": \"Value\"}'",
                        }
                    })?;
                nav_cmd["headers"] = headers;
            }
            // Include iOS device info if specified (needed for auto-launch with existing daemon)
            if flags.provider.as_deref() == Some("ios") {
                if let Some(ref device) = flags.device {
                    nav_cmd["iosDevice"] = json!(device);
                }
            }
            Ok(nav_cmd)
        }
        "back" => Ok(json!({ "id": id, "action": "back" })),
        "forward" => Ok(json!({ "id": id, "action": "forward" })),
        "reload" => Ok(json!({ "id": id, "action": "reload" })),
        // Explicit opt-in to raise the active tab to the foreground (the core
        // skill references it; the daemon handler existed but the CLI didn't map
        // it — issue #19). Accept the documented camelCase + kebab/lowercase.
        "bringToFront" | "bring-to-front" | "bringtofront" => {
            Ok(json!({ "id": id, "action": "bringtofront" }))
        }

        // === Core Actions ===
        "click" => {
            let new_tab = rest.contains(&"--new-tab");
            // `--follow`: if the click opens a new tab, switch the active tab to
            // it (default reports the opened tab but stays put) (issue #24-A).
            let follow = rest.contains(&"--follow");
            // Coordinate click as a first-class form (issue #8.4): when the only
            // handle is a pixel position, no element/selector is needed.
            //   click <x> <y>          e.g. click 449 320
            //   click <x>,<y>          e.g. click 449,320
            //   click --coords <x>,<y> | --coords <x> <y>
            let coord_args: Vec<&str> = rest
                .iter()
                .copied()
                .filter(|a| !a.starts_with("--"))
                .collect();
            if let Some((x, y)) = parse_coords(&coord_args) {
                return Ok(json!({ "id": id, "action": "click", "x": x, "y": y }));
            }
            let sel = rest
                .iter()
                .find(|arg| !arg.starts_with("--"))
                .ok_or_else(|| ParseError::MissingArguments {
                    context: "click".to_string(),
                    usage:
                        "click <selector> | click <x> <y> | click --coords <x>,<y> [--new-tab] [--follow]",
                })?;
            let mut cmd = json!({ "id": id, "action": "click", "selector": sel });
            if new_tab {
                cmd["newTab"] = json!(true);
            }
            if follow {
                cmd["follow"] = json!(true);
            }
            Ok(cmd)
        }
        "dblclick" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "dblclick".to_string(),
                usage: "dblclick <selector>",
            })?;
            Ok(json!({ "id": id, "action": "dblclick", "selector": sel }))
        }
        "fill" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "fill".to_string(),
                usage: "fill <selector> <text> | fill <selector> --file <path> | fill <selector> --stdin",
            })?;
            // Large/multiline content without shell-escaping hell (issue #41):
            // `fill <sel> --file <path>` reads the value from a UTF-8 file, and
            // `fill <sel> --stdin` reads it from stdin — sent verbatim, so backticks,
            // quotes, newlines and non-ASCII pass through untouched.
            let value = match rest.get(1).copied() {
                Some("--file") => {
                    let path = rest.get(2).ok_or(ParseError::InvalidValue {
                        message: "fill --file requires a path".to_string(),
                        usage: "fill <selector> --file <path>",
                    })?;
                    std::fs::read_to_string(path).map_err(|e| ParseError::InvalidValue {
                        message: format!("fill --file: cannot read {path}: {e}"),
                        usage: "fill <selector> --file <path>",
                    })?
                }
                Some("--stdin") => {
                    use std::io::Read;
                    let mut buf = String::new();
                    io::stdin()
                        .read_to_string(&mut buf)
                        .map_err(|e| ParseError::InvalidValue {
                            message: format!("fill --stdin: {e}"),
                            usage: "fill <selector> --stdin",
                        })?;
                    buf
                }
                _ => rest[1..].join(" "),
            };
            Ok(json!({ "id": id, "action": "fill", "selector": sel, "value": value }))
        }
        "type" => {
            // `--key-events` (alias `--keys`): send real per-character keystrokes
            // instead of Input.insertText, so autocomplete/combobox widgets that
            // only react to key events fire (e.g. Google address postal lookup).
            let mut key_events = rest.iter().any(|a| *a == "--key-events" || *a == "--keys");
            // `--enter` (alias `--commit-enter`): press Enter after typing to commit
            // the highlighted candidate in an async-autocomplete widget (e.g.
            // juejin's tag input — issue #50). Implies `--key-events`: a bulk
            // Input.insertText never fires the keydown/input the dropdown queries
            // off, so the candidate Enter would commit never appears.
            let commit_enter = rest
                .iter()
                .any(|a| *a == "--enter" || *a == "--commit-enter");
            if commit_enter {
                key_events = true;
            }
            // `--clear` clears the field before typing; `--delay <ms>` types with
            // a per-key delay. The daemon has always honored these, but the CLI
            // used to join every remaining arg into the text, so they leaked in
            // as literal characters. Parse them out here. (Ported from
            // vercel-labs/agent-browser #1432.)
            let clear = rest.iter().any(|a| *a == "--clear");
            let mut delay: Option<u64> = None;
            let type_usage =
                "type <selector> <text>   (or: type --focused <text>) [--clear] [--delay <ms>] [--key-events] [--enter]";
            if let Some(i) = rest.iter().position(|a| *a == "--delay") {
                let raw = rest.get(i + 1).ok_or_else(|| ParseError::MissingArguments {
                    context: "type --delay".to_string(),
                    usage: type_usage,
                })?;
                delay = Some(raw.parse::<u64>().map_err(|_| ParseError::InvalidValue {
                    message: format!("--delay expects a number in ms, got '{}'", raw),
                    usage: type_usage,
                })?);
            }
            // Strip flags (and --delay's value) from the positional args.
            let mut rest: Vec<&str> = {
                let mut out = Vec::new();
                let mut skip_next = false;
                for a in rest.iter().copied() {
                    if skip_next {
                        skip_next = false;
                        continue;
                    }
                    match a {
                        "--key-events" | "--keys" | "--enter" | "--commit-enter" | "--clear" => {}
                        "--delay" => skip_next = true,
                        other => out.push(other),
                    }
                }
                out
            };
            // `type --focused <text>` types into whatever element currently has
            // focus (no selector) — for custom widgets that move focus to a hidden
            // input after you open them.
            if rest.first() == Some(&"--focused") {
                rest.remove(0);
                return Ok(json!({
                    "id": id, "action": "type", "focused": true,
                    "text": rest.join(" "), "keyEvents": key_events,
                    "commitEnter": commit_enter, "clear": clear, "delay": delay,
                }));
            }
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "type".to_string(),
                usage: type_usage,
            })?;
            Ok(
                json!({ "id": id, "action": "type", "selector": sel, "text": rest[1..].join(" "), "keyEvents": key_events, "commitEnter": commit_enter, "clear": clear, "delay": delay }),
            )
        }
        "pick" => {
            // pick <selector|@ref> --option "<text>"  — atomic combobox select:
            // open the control, wait for options (incl. portal menus), match by
            // text, fire the right event sequence, verify. Covers native <select>,
            // ARIA combobox/listbox, and react-select.
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "pick".to_string(),
                usage: "pick <selector> --option \"<text>\"",
            })?;
            let opt_pos = rest.iter().position(|a| *a == "--option" || *a == "-o");
            let option = match opt_pos {
                Some(p) => rest[p + 1..].join(" "),
                None => {
                    return Err(ParseError::MissingArguments {
                        context: "pick".to_string(),
                        usage: "pick <selector> --option \"<text>\"",
                    })
                }
            };
            if option.is_empty() {
                return Err(ParseError::MissingArguments {
                    context: "pick".to_string(),
                    usage: "pick <selector> --option \"<text>\"",
                });
            }
            Ok(json!({ "id": id, "action": "pick", "selector": sel, "option": option }))
        }
        "hover" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "hover".to_string(),
                usage: "hover <selector>",
            })?;
            Ok(json!({ "id": id, "action": "hover", "selector": sel }))
        }
        "focus" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "focus".to_string(),
                usage: "focus <selector>",
            })?;
            Ok(json!({ "id": id, "action": "focus", "selector": sel }))
        }
        "check" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "check".to_string(),
                usage: "check <selector>",
            })?;
            Ok(json!({ "id": id, "action": "check", "selector": sel }))
        }
        "uncheck" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "uncheck".to_string(),
                usage: "uncheck <selector>",
            })?;
            Ok(json!({ "id": id, "action": "uncheck", "selector": sel }))
        }
        "select" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "select".to_string(),
                usage: "select <selector> <value...>",
            })?;
            let _val = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "select".to_string(),
                usage: "select <selector> <value...>",
            })?;
            let values = &rest[1..];
            if values.len() == 1 {
                Ok(json!({ "id": id, "action": "select", "selector": sel, "values": values[0] }))
            } else {
                Ok(json!({ "id": id, "action": "select", "selector": sel, "values": values }))
            }
        }
        "drag" => {
            let src = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "drag".to_string(),
                usage: "drag <source> <target|dx[,dy]>",
            })?;
            let tgt = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "drag".to_string(),
                usage: "drag <source> <target|dx[,dy]>",
            })?;
            // Offset mode (sliders/canvas): `drag <handle> 60` or `drag <handle> +60,-3`
            // presses at the source center and moves by (dx,dy) px along the humanize
            // trajectory, instead of dragging to another element. A pure numeric /
            // `dx,dy` second arg is unambiguous vs a @ref/selector/text target.
            if let Some((dx, dy)) = parse_drag_offset(tgt) {
                Ok(
                    json!({ "id": id, "action": "drag", "source": src, "offset_dx": dx, "offset_dy": dy }),
                )
            } else {
                Ok(json!({ "id": id, "action": "drag", "source": src, "target": tgt }))
            }
        }
        "upload" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "upload".to_string(),
                usage: "upload <selector> <files...>",
            })?;
            Ok(json!({ "id": id, "action": "upload", "selector": sel, "files": &rest[1..] }))
        }
        "solve-slider" | "solve_slider" => {
            // Optional retry count: `solve-slider [retries]` (default 3).
            let retries = rest
                .first()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(3);
            Ok(json!({ "id": id, "action": "solve_slider", "retries": retries }))
        }
        "download" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "download".to_string(),
                usage: "download <selector> <path>",
            })?;
            let path = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "download".to_string(),
                usage: "download <selector> <path>",
            })?;
            Ok(json!({ "id": id, "action": "download", "selector": sel, "path": path }))
        }

        // === Keyboard ===
        "press" | "key" => {
            let key = rest.iter().find(|a| !a.starts_with("--")).ok_or_else(|| {
                ParseError::MissingArguments {
                    context: "press".to_string(),
                    usage: "press <key> [--hold <ms>]",
                }
            })?;
            let mut c = json!({ "id": id, "action": "press", "key": key });
            // `--hold <ms>`: hold the key down for <ms> then release, timed inside
            // the daemon (one round-trip, no shell-sleep jitter) — for games and
            // hold-to-charge where keydown+sleep+keyup over 3 round-trips is too
            // imprecise.
            if let Some(i) = rest.iter().position(|a| *a == "--hold") {
                let ms = rest.get(i + 1).and_then(|s| s.parse::<u64>().ok()).ok_or(
                    ParseError::MissingArguments {
                        context: "press --hold".to_string(),
                        usage: "press <key> --hold <ms>",
                    },
                )?;
                c["hold"] = json!(ms);
            }
            Ok(c)
        }
        "keydown" => {
            let key = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "keydown".to_string(),
                usage: "keydown <key>",
            })?;
            Ok(json!({ "id": id, "action": "keydown", "key": key }))
        }
        "keyup" => {
            let key = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "keyup".to_string(),
                usage: "keyup <key>",
            })?;
            Ok(json!({ "id": id, "action": "keyup", "key": key }))
        }
        "keyboard" => {
            let sub = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "keyboard".to_string(),
                usage: "keyboard <type|inserttext> <text>",
            })?;
            match *sub {
                "type" => {
                    let text: String = rest[1..].join(" ");
                    if text.is_empty() {
                        return Err(ParseError::MissingArguments {
                            context: "keyboard type".to_string(),
                            usage: "keyboard type <text>",
                        });
                    }
                    Ok(json!({ "id": id, "action": "keyboard", "subaction": "type", "text": text }))
                }
                "inserttext" | "insertText" => {
                    let text: String = rest[1..].join(" ");
                    if text.is_empty() {
                        return Err(ParseError::MissingArguments {
                            context: "keyboard inserttext".to_string(),
                            usage: "keyboard inserttext <text>",
                        });
                    }
                    Ok(
                        json!({ "id": id, "action": "keyboard", "subaction": "insertText", "text": text }),
                    )
                }
                _ => Err(ParseError::UnknownSubcommand {
                    subcommand: sub.to_string(),
                    valid_options: &["type", "inserttext"],
                }),
            }
        }

        // === Scroll ===
        "scroll" => {
            let mut cmd = json!({ "id": id, "action": "scroll" });
            let obj = cmd.as_object_mut().unwrap();
            let mut positional_index = 0;
            let mut i = 0;
            while i < rest.len() {
                match rest[i] {
                    "-s" | "--selector" => {
                        if let Some(s) = rest.get(i + 1) {
                            obj.insert("selector".to_string(), json!(s));
                            i += 1;
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "scroll --selector".to_string(),
                                usage: "scroll [direction] [amount] [--selector <sel>] [--at <x,y>] [--frame <n>]",
                            });
                        }
                    }
                    "--at" => {
                        // `--at x,y`: dispatch the wheel at this viewport pixel, so it
                        // scrolls whatever element/iframe is under the pointer — including
                        // cross-origin iframes that `window.scrollBy` can't reach (#36).
                        let val = rest.get(i + 1).ok_or(ParseError::MissingArguments {
                            context: "scroll --at".to_string(),
                            usage: "scroll [direction] [amount] --at <x,y>",
                        })?;
                        let mut parts = val.split(',');
                        match (
                            parts.next().and_then(|s| s.trim().parse::<f64>().ok()),
                            parts.next().and_then(|s| s.trim().parse::<f64>().ok()),
                        ) {
                            (Some(x), Some(y)) => {
                                obj.insert("at".to_string(), json!([x, y]));
                            }
                            _ => {
                                return Err(ParseError::InvalidValue {
                                    message: format!("scroll --at: invalid coordinate `{}`", val),
                                    usage:
                                        "scroll [direction] [amount] --at <x,y> (e.g. --at 640,400)",
                                })
                            }
                        }
                        i += 1;
                    }
                    "--frame" => {
                        // `--frame n`: scroll the n-th frame from `chrome-use frames` by
                        // dispatching the wheel at that frame's center — reaches content in
                        // a cross-origin iframe without needing a selector into it (#36).
                        let val = rest.get(i + 1).ok_or(ParseError::MissingArguments {
                            context: "scroll --frame".to_string(),
                            usage: "scroll [direction] [amount] --frame <n>",
                        })?;
                        match val.trim().parse::<usize>() {
                            Ok(n) => {
                                obj.insert("frame".to_string(), json!(n));
                            }
                            Err(_) => {
                                return Err(ParseError::InvalidValue {
                                    message: format!("scroll --frame: invalid index `{}`", val),
                                    usage: "scroll [direction] [amount] --frame <n> (index from `chrome-use frames`)",
                                })
                            }
                        }
                        i += 1;
                    }
                    arg if arg.starts_with('-') => {}
                    _ => {
                        match positional_index {
                            0 => {
                                obj.insert("direction".to_string(), json!(rest[i]));
                            }
                            1 => {
                                if let Ok(n) = rest[i].parse::<i32>() {
                                    obj.insert("amount".to_string(), json!(n));
                                }
                            }
                            _ => {}
                        }
                        positional_index += 1;
                    }
                }
                i += 1;
            }
            if !obj.contains_key("direction") {
                obj.insert("direction".to_string(), json!("down"));
            }
            if !obj.contains_key("amount") {
                obj.insert("amount".to_string(), json!(300));
            }
            Ok(cmd)
        }
        "scrollintoview" | "scrollinto" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "scrollintoview".to_string(),
                usage: "scrollintoview <selector>",
            })?;
            Ok(json!({ "id": id, "action": "scrollintoview", "selector": sel }))
        }

        // === Wait ===
        "wait" => {
            // Check for --url flag: wait --url "**/dashboard" [--timeout ms]
            if let Some(idx) = rest.iter().position(|&s| s == "--url" || s == "-u") {
                let url = rest
                    .get(idx + 1)
                    .ok_or_else(|| ParseError::MissingArguments {
                        context: "wait --url".to_string(),
                        usage: "wait --url <pattern>",
                    })?;
                if url.is_empty() {
                    return Err(ParseError::InvalidValue {
                        message: "wait --url needs a non-empty pattern (an empty pattern would \
                                  match any URL)."
                            .to_string(),
                        usage: "wait --url <pattern>",
                    });
                }
                let mut cmd = json!({ "id": id, "action": "waitforurl", "url": url });
                // Parse --timeout (without it the default applies — and a
                // non-matching pattern would otherwise wait the full default).
                if let Some(t_idx) = rest.iter().position(|&s| s == "--timeout") {
                    if let Some(ms) = rest.get(t_idx + 1).and_then(|s| s.parse::<u64>().ok()) {
                        cmd["timeout"] = json!(ms);
                    }
                }
                return Ok(cmd);
            }

            // Check for --load flag: wait --load networkidle
            if let Some(idx) = rest.iter().position(|&s| s == "--load" || s == "-l") {
                let state = rest
                    .get(idx + 1)
                    .ok_or_else(|| ParseError::MissingArguments {
                        context: "wait --load".to_string(),
                        usage: "wait --load <state>",
                    })?;
                return Ok(json!({ "id": id, "action": "waitforloadstate", "state": state }));
            }

            // Check for --fn flag: wait --fn "window.ready === true"
            if let Some(idx) = rest.iter().position(|&s| s == "--fn" || s == "-f") {
                let expr = rest
                    .get(idx + 1)
                    .ok_or_else(|| ParseError::MissingArguments {
                        context: "wait --fn".to_string(),
                        usage: "wait --fn <expression>",
                    })?;
                return Ok(json!({ "id": id, "action": "waitforfunction", "expression": expr }));
            }

            // Check for --text flag: wait --text "Welcome" [--timeout ms]
            if let Some(idx) = rest.iter().position(|&s| s == "--text" || s == "-t") {
                let text = rest
                    .get(idx + 1)
                    .ok_or_else(|| ParseError::MissingArguments {
                        context: "wait --text".to_string(),
                        usage: "wait --text <text>",
                    })?;
                let mut cmd = json!({ "id": id, "action": "wait", "text": text });
                if let Some(t_idx) = rest.iter().position(|&s| s == "--timeout") {
                    if let Some(Ok(ms)) = rest.get(t_idx + 1).map(|s| s.parse::<u64>()) {
                        cmd["timeout"] = json!(ms);
                    }
                }
                return Ok(cmd);
            }

            // Check for --download flag: wait --download [path] [--timeout ms]
            if rest.iter().any(|&s| s == "--download" || s == "-d") {
                let mut cmd = json!({ "id": id, "action": "waitfordownload" });
                // Check for optional path (first non-flag argument after --download)
                let download_idx = rest
                    .iter()
                    .position(|&s| s == "--download" || s == "-d")
                    .unwrap();
                if let Some(path) = rest.get(download_idx + 1) {
                    if !path.starts_with("--") {
                        cmd["path"] = json!(path);
                    }
                }
                // Check for optional timeout
                if let Some(idx) = rest.iter().position(|&s| s == "--timeout") {
                    if let Some(timeout_str) = rest.get(idx + 1) {
                        if let Ok(timeout) = timeout_str.parse::<u64>() {
                            cmd["timeout"] = json!(timeout);
                        }
                    }
                }
                return Ok(cmd);
            }

            // --gone / --hidden: wait for an element to leave the DOM or
            // become invisible. Useful after a click that's supposed to
            // close a dialog, so the next command fails fast instead of
            // racing into a half-rendered UI.
            let state_override = if rest.iter().any(|&s| s == "--gone" || s == "--detached") {
                Some("detached")
            } else if rest.contains(&"--hidden") {
                Some("hidden")
            } else {
                None
            };

            // Default: selector or timeout
            // First non-flag positional is selector or numeric timeout
            let positional = rest.iter().find(|&&s| !s.starts_with("--"));
            let timeout_ms = rest
                .iter()
                .position(|&s| s == "--timeout")
                .and_then(|idx| rest.get(idx + 1))
                .and_then(|s| s.parse::<u64>().ok());

            if let Some(arg) = positional {
                if let Ok(timeout) = arg.parse::<u64>() {
                    Ok(json!({ "id": id, "action": "wait", "timeout": timeout }))
                } else {
                    let mut cmd = json!({ "id": id, "action": "wait", "selector": arg });
                    if let Some(state) = state_override {
                        cmd["state"] = json!(state);
                    }
                    if let Some(t) = timeout_ms {
                        cmd["timeout"] = json!(t);
                    }
                    Ok(cmd)
                }
            } else {
                Err(ParseError::MissingArguments {
                    context: "wait".to_string(),
                    usage: "wait <selector|ms> [--gone|--hidden] [--timeout ms]",
                })
            }
        }

        // === Screenshot/PDF ===
        "screenshot" => {
            // screenshot [selector] [path] [--full/-f]
            // selector: @ref or CSS selector
            // path: file path (contains / or . or ends with known extension)
            let mut full_page = false;
            let mut clip: Option<Value> = None;
            let mut max_width: Option<u32> = None;
            let mut max_height: Option<u32> = None;
            let mut scale: Option<f64> = None;
            let mut positional: Vec<&str> = Vec::new();
            let mut i = 0;
            // Parse a numeric value for a downscale flag (issue #42).
            let parse_num = |i: &mut usize, flag: &str| -> Result<String, ParseError> {
                let v = rest
                    .get(*i + 1)
                    .ok_or_else(|| ParseError::MissingArguments {
                        context: format!("screenshot {flag}"),
                        usage: "screenshot [--max-width <px>] [--max-height <px>] [--scale <0..1>]",
                    })?;
                *i += 1;
                Ok(v.to_string())
            };
            while i < rest.len() {
                match rest[i] {
                    "--full" | "-f" => full_page = true,
                    // Downscale the saved image so retina/full-page shots fit an
                    // agent's image reader and screenshot px line up with click px (#42).
                    "--max-width" => {
                        let v = parse_num(&mut i, "--max-width")?;
                        max_width = Some(v.parse().map_err(|_| ParseError::InvalidValue {
                            message: format!("--max-width expects a number, got '{v}'"),
                            usage: "screenshot --max-width <px>",
                        })?);
                    }
                    "--max-height" => {
                        let v = parse_num(&mut i, "--max-height")?;
                        max_height = Some(v.parse().map_err(|_| ParseError::InvalidValue {
                            message: format!("--max-height expects a number, got '{v}'"),
                            usage: "screenshot --max-height <px>",
                        })?);
                    }
                    "--scale" => {
                        let v = parse_num(&mut i, "--scale")?;
                        let s: f64 = v.parse().map_err(|_| ParseError::InvalidValue {
                            message: format!("--scale expects a number like 0.5, got '{v}'"),
                            usage: "screenshot --scale <0..1>",
                        })?;
                        if s <= 0.0 || s > 1.0 {
                            return Err(ParseError::InvalidValue {
                                message: format!("--scale must be in (0, 1], got '{v}'"),
                                usage: "screenshot --scale <0..1>",
                            });
                        }
                        scale = Some(s);
                    }
                    // `--clip x,y,w,h` captures a pixel region (issue #34).
                    "--clip" => {
                        let raw = rest
                            .get(i + 1)
                            .ok_or_else(|| ParseError::MissingArguments {
                                context: "screenshot --clip".to_string(),
                                usage: "screenshot --clip <x,y,w,h> [path]",
                            })?;
                        let nums: Vec<f64> = raw
                            .split(',')
                            .filter_map(|n| n.trim().parse::<f64>().ok())
                            .collect();
                        if nums.len() != 4 {
                            return Err(ParseError::InvalidValue {
                                message: format!(
                                    "--clip expects 'x,y,w,h' (4 numbers), got '{raw}'"
                                ),
                                usage: "screenshot --clip <x,y,w,h> [path]",
                            });
                        }
                        clip = Some(json!({
                            "x": nums[0], "y": nums[1], "width": nums[2], "height": nums[3]
                        }));
                        i += 1;
                    }
                    other => positional.push(other),
                }
                i += 1;
            }
            let (selector, path) = match (positional.first(), positional.get(1)) {
                (Some(first), Some(second)) => {
                    // Two args: first is selector, second is path
                    (Some(*first), Some(*second))
                }
                (Some(first), None) => {
                    // One arg: determine if it's a selector or a path
                    let is_relative_path = first.starts_with("./") || first.starts_with("../");
                    let is_selector = !is_relative_path
                        && (first.starts_with('.')
                            || first.starts_with('#')
                            || first.starts_with('@'));
                    let has_path_extension = first.ends_with(".png")
                        || first.ends_with(".jpg")
                        || first.ends_with(".jpeg")
                        || first.ends_with(".webp");
                    let is_path = is_relative_path || first.contains('/') || has_path_extension;
                    if is_selector || !is_path {
                        (Some(*first), None)
                    } else {
                        (None, Some(*first))
                    }
                }
                _ => (None, None),
            };
            let mut cmd = json!({
                "id": id, "action": "screenshot",
                "path": path, "selector": selector,
                "fullPage": full_page, "annotate": flags.annotate
            });
            if let Some(c) = clip {
                cmd["clip"] = c;
            }
            if let Some(w) = max_width {
                cmd["maxWidth"] = json!(w);
            }
            if let Some(h) = max_height {
                cmd["maxHeight"] = json!(h);
            }
            if let Some(s) = scale {
                cmd["scale"] = json!(s);
            }
            if let Some(ref fmt) = flags.screenshot_format {
                cmd["format"] = json!(fmt);
            }
            if let Some(q) = flags.screenshot_quality {
                cmd["quality"] = json!(q);
                if flags.screenshot_format.as_deref() != Some("jpeg") {
                    eprintln!(
                        "{} --screenshot-quality is ignored for PNG; use --screenshot-format jpeg",
                        color::warning_indicator()
                    );
                }
            }
            if let Some(ref dir) = flags.screenshot_dir {
                cmd["screenshotDir"] = json!(dir);
            }
            Ok(cmd)
        }
        // Canvas/WebGL apps (Figma, games, maps, charts, drawing tools) render to a
        // <canvas> with no DOM/refs to read. `canvas` extracts what's actually
        // rendered: `list` enumerates canvases; `capture` saves a canvas to PNG via
        // toDataURL (full backing-store resolution) with a CDP-screenshot fallback
        // for WebGL contexts (no preserveDrawingBuffer) or cross-origin-tainted ones.
        "canvas" => {
            match rest.first().copied() {
                Some("list") => Ok(json!({ "id": id, "action": "canvas_list" })),
                Some("capture") => {
                    let force_screenshot = rest.contains(&"--screenshot");
                    let pos: Vec<&str> = rest[1..]
                        .iter()
                        .copied()
                        .filter(|a| !a.starts_with("--"))
                        .collect();
                    // `capture [selector] [path]`: a selector starts with . # @ or is
                    // a tag; a path contains / or ends in an image extension.
                    let is_path = |s: &str| {
                        s.contains('/')
                            || s.ends_with(".png")
                            || s.ends_with(".jpg")
                            || s.ends_with(".jpeg")
                            || s.ends_with(".webp")
                    };
                    let (selector, path) = match (pos.first(), pos.get(1)) {
                        (Some(a), Some(b)) => (Some(*a), Some(*b)),
                        (Some(a), None) if is_path(a) => (None, Some(*a)),
                        (Some(a), None) => (Some(*a), None),
                        _ => (None, None),
                    };
                    let mut cmd = json!({ "id": id, "action": "canvas_capture" });
                    if let Some(s) = selector {
                        cmd["selector"] = json!(s);
                    }
                    if let Some(p) = path {
                        cmd["path"] = json!(p);
                    }
                    if force_screenshot {
                        cmd["forceScreenshot"] = json!(true);
                    }
                    Ok(cmd)
                }
                _ => Err(ParseError::InvalidValue {
                    message: "canvas needs a subcommand".to_string(),
                    usage: "canvas list | canvas capture [selector] [path] [--screenshot]",
                }),
            }
        }
        "pdf" => {
            let path = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "pdf".to_string(),
                usage: "pdf <path>",
            })?;
            Ok(json!({ "id": id, "action": "pdf", "path": path }))
        }

        // === Snapshot ===
        "snapshot" => {
            let mut cmd = json!({ "id": id, "action": "snapshot" });
            let obj = cmd.as_object_mut().unwrap();
            let mut i = 0;
            while i < rest.len() {
                match rest[i] {
                    "-i" | "--interactive" => {
                        obj.insert("interactive".to_string(), json!(true));
                    }
                    "-c" | "--compact" => {
                        obj.insert("compact".to_string(), json!(true));
                    }
                    "-C" | "--cursor" => {
                        obj.insert("cursor".to_string(), json!(true));
                    }
                    "-u" | "--urls" => {
                        obj.insert("urls".to_string(), json!(true));
                    }
                    "-d" | "--depth" => {
                        if let Some(d) = rest.get(i + 1) {
                            if let Ok(n) = d.parse::<i32>() {
                                obj.insert("maxDepth".to_string(), json!(n));
                                i += 1;
                            }
                        }
                    }
                    "-s" | "--selector" => {
                        if let Some(s) = rest.get(i + 1) {
                            obj.insert("selector".to_string(), json!(s));
                            i += 1;
                        }
                    }
                    "-f" | "--filter" => {
                        if let Some(s) = rest.get(i + 1) {
                            obj.insert("filter".to_string(), json!(s));
                            i += 1;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            Ok(cmd)
        }

        // === Eval ===
        "eval" => {
            // Optional leading `--frame <selector|url|index>` (issue #58): run the
            // script in that frame's context instead of the main frame. Default
            // (no flag) stays the main frame. Leading-only so it never eats a token
            // from the script body.
            let mut frame: Option<String> = None;
            let rest: Vec<&str> = if rest.first() == Some(&"--frame") {
                let val = rest.get(1).copied().ok_or(ParseError::InvalidValue {
                    message: "eval --frame requires a value (CSS selector, @ref, url substring, \
                              or frame index)"
                        .to_string(),
                    usage: "eval --frame <selector|url|index> <js>",
                })?;
                frame = Some(val.to_string());
                rest[2..].to_vec()
            } else {
                rest.clone()
            };

            // Check for flags: -b/--base64, --stdin, or --file <path>
            let (is_base64, is_stdin, is_file, script_parts): (bool, bool, bool, &[&str]) =
                if rest.first() == Some(&"-b") || rest.first() == Some(&"--base64") {
                    (true, false, false, &rest[1..])
                } else if rest.first() == Some(&"--stdin") {
                    (false, true, false, &rest[1..])
                } else if rest.first() == Some(&"--file") {
                    (false, false, true, &rest[1..])
                } else {
                    (false, false, false, rest.as_slice())
                };

            let script = if is_file {
                // Read the script from a file. Avoids shell-mangling of inline JS
                // (non-ASCII identifiers/strings, quotes, large scripts) — the file
                // is read as UTF-8 and sent verbatim.
                let path = script_parts.first().ok_or(ParseError::InvalidValue {
                    message: "eval --file requires a path".to_string(),
                    usage: "eval --file <path>",
                })?;
                std::fs::read_to_string(path).map_err(|e| ParseError::InvalidValue {
                    message: format!("eval --file: cannot read {path}: {e}"),
                    usage: "eval --file <path>",
                })?
            } else if is_stdin {
                // Read script from stdin
                let stdin = io::stdin();
                let lines: Vec<String> = stdin
                    .lock()
                    .lines()
                    .map(|l| l.unwrap_or_default())
                    .collect();
                lines.join("\n")
            } else {
                let raw_script = script_parts.join(" ");
                if is_base64 {
                    let decoded =
                        STANDARD
                            .decode(&raw_script)
                            .map_err(|_| ParseError::InvalidValue {
                                message: "Invalid base64 encoding".to_string(),
                                usage: "eval -b <base64-encoded-script>",
                            })?;
                    String::from_utf8(decoded).map_err(|_| ParseError::InvalidValue {
                        message: "Base64 decoded to invalid UTF-8".to_string(),
                        usage: "eval -b <base64-encoded-script>",
                    })?
                } else {
                    raw_script
                }
            };
            let mut action = json!({ "id": id, "action": "evaluate", "script": script });
            if let Some(f) = frame {
                action["frame"] = Value::String(f);
            }
            Ok(action)
        }

        "site" => {
            // `site <name>/<command> [positional...] [--key value]`. The
            // `update`/`list`/`info` subcommands are handled CLI-side (main.rs)
            // and never reach here — by this point `rest[0]` is a `name/cmd`
            // adapter spec. Load it, map the args onto the adapter's declared
            // `args`, and emit a `site` action: the daemon navigates to the
            // adapter's @meta.domain (reusing the tab if already there) and evals
            // the adapter function in the site's own logged-in page.
            let spec = rest.first().ok_or(ParseError::InvalidValue {
                message: "site requires <name>/<command> (run `chrome-use site list`)".to_string(),
                usage: "site <name>/<command> [args]",
            })?;
            let adapter =
                crate::site::load_adapter(spec).map_err(|e| ParseError::InvalidValue {
                    message: e,
                    usage: "site <name>/<command> [args]",
                })?;
            let domain = adapter
                .domain()
                .ok_or(ParseError::InvalidValue {
                    message: format!("site: adapter `{spec}` @meta is missing a \"domain\""),
                    usage: "site <name>/<command>",
                })?
                .to_string();
            // Split remaining args: `--key value` → named, everything else → positional.
            let mut positional: Vec<String> = Vec::new();
            let mut named: Vec<(String, String)> = Vec::new();
            let mut it = rest[1..].iter();
            while let Some(a) = it.next() {
                if let Some(key) = a.strip_prefix("--") {
                    let val = it.next().map(|s| s.to_string()).unwrap_or_default();
                    named.push((key.to_string(), val));
                } else {
                    positional.push(a.to_string());
                }
            }
            let mapped = crate::site::map_args(&adapter, &positional, &named);
            let script = crate::site::build_eval(&adapter, &mapped);
            Ok(json!({ "id": id, "action": "site", "domain": domain, "script": script }))
        }

        // `adopt <url|targetId>`: the adoption happens at daemon connect (driven by
        // the AGENT_BROWSER_ADOPT env main.rs set + a forced-fresh daemon), so by
        // the time this command runs the tab is already attached. Resolve to a
        // `url` read so the response confirms which tab got adopted.
        "adopt" => Ok(json!({ "id": id, "action": "url" })),

        // `keep`: leave the active tab for the user — exempt it from the daemon's
        // auto-close/idle cleanup and remove it from the session's tab group.
        "keep" => Ok(json!({ "id": id, "action": "keep" })),

        // === Stealth self-check ===
        "stealth" => {
            // `stealth [status]` — local stealth self-check: mode, live probes
            // (navigator.webdriver, window.chrome, plugins, UA), and the list of
            // active overrides. --json for a stable machine-readable shape.
            // (Distinct from `doctor`, which checks install/env/Chrome health.)
            Ok(json!({ "id": id, "action": "stealth_status" }))
        }

        // `cf-status` — Cloudflare challenge/clearance preflight: is the page
        // currently a CF challenge, and is there a still-valid cf_clearance (the
        // HttpOnly persistence cookie)? Lets an agent SKIP re-solving when already
        // cleared, and know when it must solve. Persistence optimization.
        "cf-status" | "cf" | "cloudflare-status" | "clearance" => {
            Ok(json!({ "id": id, "action": "cf_status" }))
        }

        // === Close ===
        "close" | "quit" | "exit" => {
            // `close <tab>` closes only that tab (and the output says "Tab
            // closed"); bare `close` closes the browser/session. `close --all` is
            // intercepted earlier in the dispatcher. Previously `close t12` still
            // ran a browser close and alarmingly printed "Browser closed" (#26).
            if let Some(tab_ref) = rest.iter().find(|a| !a.starts_with("--")) {
                Ok(json!({ "id": id, "action": "tab_close", "tabId": tab_ref }))
            } else {
                Ok(json!({ "id": id, "action": "close" }))
            }
        }

        // The active tab's stable handle — `targetId` survives cross-process
        // navigation and is reusable across sessions, so an agent can hold it
        // instead of re-deriving "which tab is live" from `tabs` each step (#26).
        "current" => Ok(json!({ "id": id, "action": "current" })),

        // === Inspect ===
        "inspect" => Ok(json!({ "id": id, "action": "inspect" })),

        // === Authentication Vault ===
        "auth" => {
            let sub = rest.first().map(|s| s.as_ref());
            match sub {
                Some("save") => {
                    let name = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                        context: "auth save".to_string(),
                        usage: "chrome-use auth save <name> --url <url> --username <user> --password <pass>",
                    })?;

                    let mut url = None;
                    let mut username = None;
                    let mut password = None;
                    let mut password_stdin = false;
                    let mut username_selector = None;
                    let mut password_selector = None;
                    let mut submit_selector = None;

                    let mut j = 2;
                    while j < rest.len() {
                        match rest[j] {
                            "--url" => {
                                url = rest.get(j + 1).cloned();
                                j += 1;
                            }
                            "--username" => {
                                username = rest.get(j + 1).cloned();
                                j += 1;
                            }
                            "--password" => {
                                password = rest.get(j + 1).cloned();
                                j += 1;
                            }
                            "--password-stdin" => {
                                password_stdin = true;
                            }
                            "--username-selector" => {
                                username_selector = rest.get(j + 1).cloned();
                                j += 1;
                            }
                            "--password-selector" => {
                                password_selector = rest.get(j + 1).cloned();
                                j += 1;
                            }
                            "--submit-selector" => {
                                submit_selector = rest.get(j + 1).cloned();
                                j += 1;
                            }
                            other => {
                                if other.starts_with("--") {
                                    return Err(ParseError::InvalidValue {
                                        message: format!("unknown flag '{}' for auth save", other),
                                        usage: "chrome-use auth save <name> --url <url> --username <user> --password <pass>",
                                    });
                                }
                            }
                        }
                        j += 1;
                    }

                    let url_val = url.ok_or_else(|| ParseError::MissingArguments {
                        context: "auth save".to_string(),
                        usage: "chrome-use auth save <name> --url <url> --username <user> --password <pass> [--password-stdin]",
                    })?;
                    let user_val = username.ok_or_else(|| ParseError::MissingArguments {
                        context: "auth save".to_string(),
                        usage: "chrome-use auth save <name> --url <url> --username <user> --password <pass> [--password-stdin]",
                    })?;

                    if !password_stdin && password.is_none() {
                        return Err(ParseError::MissingArguments {
                            context: "auth save".to_string(),
                            usage: "chrome-use auth save <name> --url <url> --username <user> --password <pass> [--password-stdin]",
                        });
                    }

                    let mut cmd = json!({
                        "id": id,
                        "action": "auth_save",
                        "name": name,
                        "url": url_val,
                        "username": user_val,
                    });
                    if password_stdin {
                        cmd["passwordStdin"] = json!(true);
                    }
                    if let Some(pass_val) = password {
                        cmd["password"] = json!(pass_val);
                    }
                    if let Some(us) = username_selector {
                        cmd["usernameSelector"] = json!(us);
                    }
                    if let Some(ps) = password_selector {
                        cmd["passwordSelector"] = json!(ps);
                    }
                    if let Some(ss) = submit_selector {
                        cmd["submitSelector"] = json!(ss);
                    }
                    Ok(cmd)
                }
                Some("login") => {
                    let name = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                        context: "auth login".to_string(),
                        usage: "chrome-use auth login <name>",
                    })?;
                    Ok(json!({ "id": id, "action": "auth_login", "name": name }))
                }
                Some("list") => Ok(json!({ "id": id, "action": "auth_list" })),
                Some("delete") | Some("remove") => {
                    let name = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                        context: "auth delete".to_string(),
                        usage: "chrome-use auth delete <name>",
                    })?;
                    Ok(json!({ "id": id, "action": "auth_delete", "name": name }))
                }
                Some("show") => {
                    let name = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                        context: "auth show".to_string(),
                        usage: "chrome-use auth show <name>",
                    })?;
                    Ok(json!({ "id": id, "action": "auth_show", "name": name }))
                }
                _ => Err(ParseError::UnknownSubcommand {
                    subcommand: sub.unwrap_or("(none)").to_string(),
                    valid_options: &["save", "login", "list", "delete", "show"],
                }),
            }
        }

        // === Action Confirmation ===
        "confirm" => {
            let cid = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "confirm".to_string(),
                usage: "chrome-use confirm <confirmation-id>",
            })?;
            Ok(json!({ "id": id, "action": "confirm", "confirmationId": cid }))
        }
        "deny" => {
            let cid = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "deny".to_string(),
                usage: "chrome-use deny <confirmation-id>",
            })?;
            Ok(json!({ "id": id, "action": "deny", "confirmationId": cid }))
        }

        // === Connect (CDP) ===
        "connect" => {
            let endpoint = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "connect".to_string(),
                usage: "connect <port|url>",
            })?;
            // Check if it's a URL (ws://, wss://, http://, https://)
            if endpoint.starts_with("ws://")
                || endpoint.starts_with("wss://")
                || endpoint.starts_with("http://")
                || endpoint.starts_with("https://")
            {
                Ok(json!({ "id": id, "action": "launch", "cdpUrl": endpoint }))
            } else {
                // It's a port number - validate and use cdpPort field
                let port: u16 = match endpoint.parse::<u32>() {
                    Ok(0) => {
                        return Err(ParseError::InvalidValue {
                            message: "Invalid port: port must be greater than 0".to_string(),
                            usage: "connect <port|url>",
                        });
                    }
                    Ok(p) if p > 65535 => {
                        return Err(ParseError::InvalidValue {
                            message: format!(
                                "Invalid port: {} is out of range (valid range: 1-65535)",
                                p
                            ),
                            usage: "connect <port|url>",
                        });
                    }
                    Ok(p) => p as u16,
                    Err(_) => {
                        return Err(ParseError::InvalidValue {
                            message: format!(
                                "Invalid value: '{}' is not a valid port number or URL",
                                endpoint
                            ),
                            usage: "connect <port|url>",
                        });
                    }
                };
                Ok(json!({ "id": id, "action": "launch", "cdpPort": port }))
            }
        }

        // === Runtime stream control ===
        "stream" => match rest.first().copied() {
            Some("enable") => {
                let mut cmd = json!({ "id": id, "action": "stream_enable" });
                let mut i = 1;
                while i < rest.len() {
                    match rest[i] {
                        "--port" => {
                            let value =
                                rest.get(i + 1)
                                    .ok_or_else(|| ParseError::MissingArguments {
                                        context: "stream enable --port".to_string(),
                                        usage: "stream enable [--port <port>]",
                                    })?;
                            let port =
                                value.parse::<u32>().map_err(|_| ParseError::InvalidValue {
                                    message: format!(
                                        "Invalid port: '{}' is not a valid integer",
                                        value
                                    ),
                                    usage: "stream enable [--port <port>]",
                                })?;
                            if port > u16::MAX as u32 {
                                return Err(ParseError::InvalidValue {
                                    message: format!(
                                        "Invalid port: {} is out of range (valid range: 0-65535)",
                                        port
                                    ),
                                    usage: "stream enable [--port <port>]",
                                });
                            }
                            cmd["port"] = json!(port);
                            i += 2;
                        }
                        flag => {
                            return Err(ParseError::InvalidValue {
                                message: format!("Unknown flag for stream enable: {}", flag),
                                usage: "stream enable [--port <port>]",
                            });
                        }
                    }
                }
                Ok(cmd)
            }
            Some("disable") => Ok(json!({ "id": id, "action": "stream_disable" })),
            Some("status") => Ok(json!({ "id": id, "action": "stream_status" })),
            Some(sub) => Err(ParseError::UnknownSubcommand {
                subcommand: sub.to_string(),
                valid_options: &["enable", "disable", "status"],
            }),
            None => Err(ParseError::MissingArguments {
                context: "stream".to_string(),
                usage: "stream <enable|disable|status>",
            }),
        },

        // === Get ===
        "get" => parse_get(&rest, &id),

        // List every frame the session can reach (top + same-process child
        // frames + out-of-process iframes), with a text-length per frame so you
        // can see where a listing's description actually lives (issue #27).
        "frames" => Ok(json!({ "id": id, "action": "frames" })),

        // Top-level shortcuts for `get <x>` status reads — users naturally type
        // `chrome-use url` / `cdp-url` / `title` without the `get` prefix
        // (and expect `cdp-url`/`cdp_url` to work interchangeably).
        "url" | "cdp-url" | "cdp_url" | "title" | "html" | "text" | "value" | "count" | "box"
        | "styles" | "attr" => {
            let sub = if cmd == "cdp_url" { "cdp-url" } else { cmd };
            let mut get_args: Vec<&str> = Vec::with_capacity(rest.len() + 1);
            get_args.push(sub);
            get_args.extend_from_slice(&rest);
            parse_get(&get_args, &id)
        }

        // Hyphen/underscore aliases for `get text <selector>` — agents naturally
        // guess `get-text` / `get_text` (issue #8.4).
        "get-text" | "get_text" => {
            let mut get_args: Vec<&str> = Vec::with_capacity(rest.len() + 1);
            get_args.push("text");
            get_args.extend_from_slice(&rest);
            parse_get(&get_args, &id)
        }

        // === Is (state checks) ===
        "is" => parse_is(&rest, &id),
        "expect" => parse_expect(&rest, &id),
        "extract" => parse_extract(&rest, &id),
        "form" => parse_form(&rest, &id),

        // === Find (locators) ===
        "find" => parse_find(&rest, &id),

        // === Read (agent-readable text extraction) ===
        "read" => parse_read(&rest, &id, flags),

        // === Mouse ===
        "mouse" => parse_mouse(&rest, &id),

        // === Viewport / window size ===
        // Top-level shortcut for `set viewport` — agents (and the author, in
        // issue #47) reach for `viewport`/`resize` first. Uses a CDP virtual
        // viewport, so it also works on the extension relay without yanking the
        // user's real window around.
        "viewport" | "resize" => parse_viewport(&rest, &id),

        // === Set (browser settings) ===
        "set" => parse_set(&rest, &id),

        // === Network ===
        "network" => parse_network(&rest, &id),

        // === Storage ===
        "storage" => parse_storage(&rest, &id),

        // === Cookies ===
        "cookies" => {
            let op = rest.first().unwrap_or(&"get");
            match *op {
                "transfer" => {
                    // Copy a logged-in session between Chrome profiles: decrypt
                    // the SOURCE profile's on-disk cookie store and inject the
                    // cookies into the active (connected) session — no CDP access
                    // to the source, no Chrome restart.
                    //   cookies transfer --from <profile> [--domain <d>[,<d>]]
                    // `--from` wins; otherwise the global `--profile` is used.
                    let from = rest
                        .iter()
                        .position(|a| *a == "--from")
                        .and_then(|i| rest.get(i + 1).copied())
                        .or(flags.profile.as_deref());
                    let from = from.ok_or_else(|| ParseError::MissingArguments {
                        context: "cookies transfer".to_string(),
                        usage: "cookies transfer --from <profile> [--domain <domain>[,<domain>]]",
                    })?;
                    let domain = rest
                        .iter()
                        .position(|a| *a == "--domain")
                        .and_then(|i| rest.get(i + 1).copied());
                    let cookies =
                        crate::cookie_export::export_cookies(from, domain).map_err(|e| {
                            ParseError::InvalidValue {
                                message: format!("cookies transfer: {}", e),
                                usage: "cookies transfer --from <profile> [--domain <domain>]",
                            }
                        })?;
                    if cookies.is_empty() {
                        return Err(ParseError::InvalidValue {
                            message: format!(
                                "cookies transfer: no cookies found in profile \"{}\"{}",
                                from,
                                domain
                                    .map(|d| format!(" for domain {}", d))
                                    .unwrap_or_default()
                            ),
                            usage: "cookies transfer --from <profile> [--domain <domain>]",
                        });
                    }
                    Ok(json!({
                        "id": id,
                        "action": "cookies_set",
                        "cookies": cookies,
                    }))
                }
                "set" => {
                    // --curl <file> mode: import cookies from a JSON array,
                    // raw cURL dump, or bare Cookie header. Scoped to the
                    // host of --domain if provided; otherwise the cookies
                    // have no scope (daemon falls back to current origin).
                    if let Some(curl_idx) = rest.iter().position(|a| *a == "--curl") {
                        let path =
                            rest.get(curl_idx + 1)
                                .ok_or_else(|| {
                                    ParseError::MissingArguments {
                            context: "cookies set --curl".to_string(),
                            usage: "cookies set --curl <file> [--domain <domain>] [--url <url>]",
                        }
                                })?;
                        let raw = std::fs::read_to_string(path).map_err(|e| {
                            ParseError::InvalidValue {
                                message: format!("cookies --curl: cannot read '{}': {}", path, e),
                                usage: "cookies set --curl <file>",
                            }
                        })?;
                        let mut cookies =
                            parse_curl_cookies(&raw).map_err(|e| ParseError::InvalidValue {
                                message: format!("cookies --curl: {}", e),
                                usage: "cookies set --curl <file>",
                            })?;

                        let domain_idx = rest.iter().position(|a| *a == "--domain");
                        let domain = domain_idx.and_then(|i| rest.get(i + 1).copied());
                        let url_idx = rest.iter().position(|a| *a == "--url");
                        let url = url_idx.and_then(|i| rest.get(i + 1).copied());

                        for cookie in cookies.iter_mut() {
                            if let Some(d) = domain {
                                cookie["domain"] = json!(d);
                                cookie["path"] = cookie.get("path").cloned().unwrap_or(json!("/"));
                            }
                            if let Some(u) = url {
                                cookie["url"] = json!(u);
                            }
                        }

                        return Ok(json!({
                            "id": id,
                            "action": "cookies_set",
                            "cookies": cookies,
                        }));
                    }

                    let name = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                        context: "cookies set".to_string(),
                        usage: "cookies set <name> <value> [--url <url>] [--domain <domain>] [--path <path>] [--httpOnly] [--secure] [--sameSite <Strict|Lax|None>] [--expires <timestamp>]\n  or:  cookies set --curl <file> [--domain <domain>] [--url <url>]",
                    })?;
                    let value = rest.get(2).ok_or_else(|| ParseError::MissingArguments {
                        context: "cookies set".to_string(),
                        usage: "cookies set <name> <value> [--url <url>] [--domain <domain>] [--path <path>] [--httpOnly] [--secure] [--sameSite <Strict|Lax|None>] [--expires <timestamp>]\n  or:  cookies set --curl <file> [--domain <domain>] [--url <url>]",
                    })?;

                    let mut cookie = json!({ "name": name, "value": value });

                    // Parse optional flags
                    let mut i = 3;
                    while i < rest.len() {
                        match rest[i] {
                            "--url" => {
                                if let Some(url) = rest.get(i + 1) {
                                    cookie["url"] = json!(url);
                                    i += 2;
                                } else {
                                    return Err(ParseError::MissingArguments {
                                        context: "cookies set --url".to_string(),
                                        usage: "--url <url>",
                                    });
                                }
                            }
                            "--domain" => {
                                if let Some(domain) = rest.get(i + 1) {
                                    cookie["domain"] = json!(domain);
                                    i += 2;
                                } else {
                                    return Err(ParseError::MissingArguments {
                                        context: "cookies set --domain".to_string(),
                                        usage: "--domain <domain>",
                                    });
                                }
                            }
                            "--path" => {
                                if let Some(path) = rest.get(i + 1) {
                                    cookie["path"] = json!(path);
                                    i += 2;
                                } else {
                                    return Err(ParseError::MissingArguments {
                                        context: "cookies set --path".to_string(),
                                        usage: "--path <path>",
                                    });
                                }
                            }
                            "--httpOnly" => {
                                cookie["httpOnly"] = json!(true);
                                i += 1;
                            }
                            "--secure" => {
                                cookie["secure"] = json!(true);
                                i += 1;
                            }
                            "--sameSite" => {
                                if let Some(same_site) = rest.get(i + 1) {
                                    // Validate sameSite value
                                    if *same_site == "Strict"
                                        || *same_site == "Lax"
                                        || *same_site == "None"
                                    {
                                        cookie["sameSite"] = json!(same_site);
                                        i += 2;
                                    } else {
                                        return Err(ParseError::MissingArguments {
                                            context: "cookies set --sameSite".to_string(),
                                            usage: "--sameSite <Strict|Lax|None>",
                                        });
                                    }
                                } else {
                                    return Err(ParseError::MissingArguments {
                                        context: "cookies set --sameSite".to_string(),
                                        usage: "--sameSite <Strict|Lax|None>",
                                    });
                                }
                            }
                            "--expires" => {
                                if let Some(expires_str) = rest.get(i + 1) {
                                    if let Ok(expires) = expires_str.parse::<i64>() {
                                        cookie["expires"] = json!(expires);
                                        i += 2;
                                    } else {
                                        return Err(ParseError::MissingArguments {
                                            context: "cookies set --expires".to_string(),
                                            usage: "--expires <timestamp>",
                                        });
                                    }
                                } else {
                                    return Err(ParseError::MissingArguments {
                                        context: "cookies set --expires".to_string(),
                                        usage: "--expires <timestamp>",
                                    });
                                }
                            }
                            _ => {
                                // Unknown flag, skip it (or could error)
                                i += 1;
                            }
                        }
                    }

                    Ok(json!({ "id": id, "action": "cookies_set", "cookies": [cookie] }))
                }
                "clear" => Ok(json!({ "id": id, "action": "cookies_clear" })),
                _ => Ok(json!({ "id": id, "action": "cookies_get" })),
            }
        }

        // === Tabs ===
        // `tabs` (plural) is a natural guess for the `tab` subcommand tree —
        // alias it so `tabs` / `tabs list` / `tabs new` all work (issue #8.4).
        "tab" | "tabs" => {
            // `--full` makes `tab list` emit untruncated URLs (needed to re-open
            // a long SSO/redirect URL after a stale session — issue #19). Pick
            // the subcommand as the first non-flag arg so the flag can appear
            // anywhere (`tab --full`, `tab list --full`).
            let full = rest.contains(&"--full");
            match rest.iter().find(|a| !a.starts_with("--")).copied() {
                Some("new") => {
                    // Accepted forms:
                    //   tab new [url]
                    //   tab new --label <name> [url]
                    //   tab new [url] --label <name>
                    let mut cmd = json!({ "id": id, "action": "tab_new" });
                    let mut i = 1;
                    while i < rest.len() {
                        match rest[i] {
                            "--label" => {
                                let name = rest.get(i + 1).ok_or(ParseError::MissingArguments {
                                    context: "tab new --label".to_string(),
                                    usage: "tab new --label <name> [url]",
                                })?;
                                cmd["label"] = json!(name);
                                i += 2;
                            }
                            other if !other.starts_with("--") && cmd.get("url").is_none() => {
                                cmd["url"] = json!(other);
                                i += 1;
                            }
                            other => {
                                return Err(ParseError::UnknownSubcommand {
                                    subcommand: other.to_string(),
                                    valid_options: &["--label", "<url>"],
                                });
                            }
                        }
                    }
                    Ok(cmd)
                }
                Some("list") => {
                    let mut cmd = json!({ "id": id, "action": "tab_list" });
                    if full {
                        cmd["full"] = json!(true);
                    }
                    Ok(cmd)
                }
                Some("close") => {
                    let mut cmd = json!({ "id": id, "action": "tab_close" });
                    if let Some(tab_ref) = rest.get(1) {
                        cmd["tabId"] = json!(tab_ref);
                    }
                    Ok(cmd)
                }
                Some(tab_ref) => {
                    // `tab <ref> --activate` (alias `--front`) switches to the tab
                    // AND raises it to the foreground — for handing a specific tab
                    // to the human (SMS code, captcha) (issue #24-C).
                    let mut cmd = json!({ "id": id, "action": "tab_switch", "tabId": tab_ref });
                    if rest.iter().any(|a| *a == "--activate" || *a == "--front") {
                        cmd["activate"] = json!(true);
                    }
                    Ok(cmd)
                }
                None => {
                    let mut cmd = json!({ "id": id, "action": "tab_list" });
                    if full {
                        cmd["full"] = json!(true);
                    }
                    Ok(cmd)
                }
            }
        }

        // === Window ===
        "window" => {
            const VALID: &[&str] = &["new"];
            match rest.first().copied() {
                Some("new") => Ok(json!({ "id": id, "action": "window_new" })),
                Some(sub) => Err(ParseError::UnknownSubcommand {
                    subcommand: sub.to_string(),
                    valid_options: VALID,
                }),
                None => Err(ParseError::MissingArguments {
                    context: "window".to_string(),
                    usage: "window <new>",
                }),
            }
        }

        // === Frame ===
        "frame" => {
            if rest.first().copied() == Some("main") {
                Ok(json!({ "id": id, "action": "mainframe" }))
            } else {
                let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                    context: "frame".to_string(),
                    usage: "frame <selector|main>",
                })?;
                Ok(json!({ "id": id, "action": "frame", "selector": sel }))
            }
        }

        // === Dialog ===
        "dialog" => {
            const VALID: &[&str] = &["accept", "dismiss", "status"];
            match rest.first().copied() {
                Some("accept") => {
                    let mut cmd = json!({ "id": id, "action": "dialog", "response": "accept" });
                    if let Some(prompt_text) = rest.get(1) {
                        cmd["promptText"] = json!(prompt_text);
                    }
                    Ok(cmd)
                }
                Some("dismiss") => {
                    let mut cmd = json!({ "id": id, "action": "dialog", "response": "dismiss" });
                    if let Some(prompt_text) = rest.get(1) {
                        cmd["promptText"] = json!(prompt_text);
                    }
                    Ok(cmd)
                }
                Some("status") => Ok(json!({ "id": id, "action": "dialog", "response": "status" })),
                Some(sub) => Err(ParseError::UnknownSubcommand {
                    subcommand: sub.to_string(),
                    valid_options: VALID,
                }),
                None => Err(ParseError::MissingArguments {
                    context: "dialog".to_string(),
                    usage: "dialog <accept|dismiss|status> [text]",
                }),
            }
        }

        // === Debug ===
        "trace" => {
            const VALID: &[&str] = &["start", "stop"];
            match rest.first().copied() {
                Some("start") => Ok(json!({ "id": id, "action": "trace_start" })),
                Some("stop") => {
                    let mut cmd = json!({ "id": id, "action": "trace_stop" });
                    if let Some(path) = rest.get(1) {
                        cmd["path"] = json!(path);
                    }
                    Ok(cmd)
                }
                Some(sub) => Err(ParseError::UnknownSubcommand {
                    subcommand: sub.to_string(),
                    valid_options: VALID,
                }),
                None => Err(ParseError::MissingArguments {
                    context: "trace".to_string(),
                    usage: "trace <start|stop> [path]",
                }),
            }
        }

        // === Profiler (CDP Tracing / Chromium profiling) ===
        "profiler" => {
            const VALID: &[&str] = &["start", "stop"];
            match rest.first().copied() {
                Some("start") => {
                    let mut cmd = json!({ "id": id, "action": "profiler_start" });
                    if let Some(idx) = rest.iter().position(|s| *s == "--categories") {
                        if let Some(cats) = rest.get(idx + 1) {
                            let categories: Vec<&str> = cats.split(',').collect();
                            cmd["categories"] = json!(categories);
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "profiler start --categories".to_string(),
                                usage: "--categories <list>",
                            });
                        }
                    }
                    Ok(cmd)
                }
                Some("stop") => {
                    let mut cmd = json!({ "id": id, "action": "profiler_stop" });
                    if let Some(path) = rest.get(1) {
                        cmd["path"] = json!(path);
                    }
                    Ok(cmd)
                }
                Some(sub) => Err(ParseError::UnknownSubcommand {
                    subcommand: sub.to_string(),
                    valid_options: VALID,
                }),
                None => Err(ParseError::MissingArguments {
                    context: "profiler".to_string(),
                    usage: "profiler <start|stop> [options]",
                }),
            }
        }

        // === Recording (browser video recording) ===
        "record" => {
            const VALID: &[&str] = &["start", "stop", "restart"];
            match rest.first().copied() {
                Some("start") => {
                    let path = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                        context: "record start".to_string(),
                        usage: "record start <output.webm> [url]",
                    })?;
                    // Optional URL parameter
                    let url = rest.get(2);
                    let mut cmd = json!({ "id": id, "action": "recording_start", "path": path });
                    if let Some(u) = url {
                        // Add https:// prefix if needed (preserve special schemes)
                        let url_str = if u.starts_with("http") || u.contains("://") {
                            u.to_string()
                        } else {
                            format!("https://{}", u)
                        };
                        cmd["url"] = json!(url_str);
                    }
                    Ok(cmd)
                }
                Some("stop") => Ok(json!({ "id": id, "action": "recording_stop" })),
                Some("restart") => {
                    let path = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                        context: "record restart".to_string(),
                        usage: "record restart <output.webm> [url]",
                    })?;
                    // Optional URL parameter
                    let url = rest.get(2);
                    let mut cmd = json!({ "id": id, "action": "recording_restart", "path": path });
                    if let Some(u) = url {
                        // Add https:// prefix if needed (preserve special schemes)
                        let url_str = if u.starts_with("http") || u.contains("://") {
                            u.to_string()
                        } else {
                            format!("https://{}", u)
                        };
                        cmd["url"] = json!(url_str);
                    }
                    Ok(cmd)
                }
                Some(sub) => Err(ParseError::UnknownSubcommand {
                    subcommand: sub.to_string(),
                    valid_options: VALID,
                }),
                None => Err(ParseError::MissingArguments {
                    context: "record".to_string(),
                    usage: "record <start|stop|restart> [path] [url]",
                }),
            }
        }
        "console" => {
            let clear = rest.contains(&"--clear");
            Ok(json!({ "id": id, "action": "console", "clear": clear }))
        }
        "errors" => {
            let clear = rest.contains(&"--clear");
            Ok(json!({ "id": id, "action": "errors", "clear": clear }))
        }
        "highlight" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "highlight".to_string(),
                usage: "highlight <selector>",
            })?;
            Ok(json!({ "id": id, "action": "highlight", "selector": sel }))
        }

        // === Clipboard ===
        "clipboard" => match rest.first().copied() {
            Some("read") | None => {
                Ok(json!({ "id": id, "action": "clipboard", "operation": "read" }))
            }
            Some("write") => {
                rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                    context: "clipboard write".to_string(),
                    usage: "clipboard write <text>",
                })?;
                let text = rest[1..].join(" ");
                Ok(json!({ "id": id, "action": "clipboard", "operation": "write", "text": text }))
            }
            Some("copy") => Ok(json!({ "id": id, "action": "clipboard", "operation": "copy" })),
            Some("paste") => Ok(json!({ "id": id, "action": "clipboard", "operation": "paste" })),
            Some(sub) => Err(ParseError::UnknownSubcommand {
                subcommand: sub.to_string(),
                valid_options: &["read", "write", "copy", "paste"],
            }),
        },

        // === State ===
        "state" => {
            const VALID: &[&str] = &["save", "load", "list", "clear", "show", "clean", "rename"];
            match rest.first().copied() {
                Some("save") => {
                    let path = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                        context: "state save".to_string(),
                        usage: "state save <path>",
                    })?;
                    Ok(json!({ "id": id, "action": "state_save", "path": path }))
                }
                Some("load") => {
                    let path = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                        context: "state load".to_string(),
                        usage: "state load <path>",
                    })?;
                    Ok(json!({ "id": id, "action": "state_load", "path": path }))
                }
                Some("list") => Ok(json!({ "id": id, "action": "state_list" })),
                Some("clear") => {
                    let mut session_name: Option<&str> = None;
                    let mut all = false;

                    let mut i = 1;
                    while i < rest.len() {
                        match rest[i] {
                            "--all" | "-a" => {
                                all = true;
                            }
                            arg if !arg.starts_with('-') => {
                                session_name = Some(arg);
                            }
                            _ => {}
                        }
                        i += 1;
                    }

                    if let Some(name) = session_name {
                        if !is_valid_session_name(name) {
                            return Err(ParseError::InvalidSessionName {
                                name: name.to_string(),
                            });
                        }
                    }

                    let mut cmd = json!({ "id": id, "action": "state_clear" });
                    if all {
                        cmd["all"] = json!(true);
                    }
                    if let Some(name) = session_name {
                        cmd["sessionName"] = json!(name);
                    }
                    Ok(cmd)
                }
                Some("show") => {
                    let filename = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                        context: "state show".to_string(),
                        usage: "state show <filename>",
                    })?;
                    Ok(json!({ "id": id, "action": "state_show", "path": filename }))
                }
                Some("clean") => {
                    let mut days: Option<i64> = None;

                    let mut i = 1;
                    while i < rest.len() {
                        if rest[i] == "--older-than" {
                            if let Some(d) = rest.get(i + 1) {
                                days = d.parse().ok();
                                i += 1;
                            }
                        }
                        i += 1;
                    }

                    let days = days.ok_or_else(|| ParseError::MissingArguments {
                        context: "state clean".to_string(),
                        usage: "state clean --older-than <days>",
                    })?;

                    Ok(json!({ "id": id, "action": "state_clean", "days": days }))
                }
                Some("rename") => {
                    let old_name = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                        context: "state rename".to_string(),
                        usage: "state rename <old-name> <new-name>",
                    })?;
                    let new_name = rest.get(2).ok_or_else(|| ParseError::MissingArguments {
                        context: "state rename".to_string(),
                        usage: "state rename <old-name> <new-name>",
                    })?;
                    let old_name = old_name.trim_end_matches(".json");
                    let new_name = new_name.trim_end_matches(".json");

                    if !is_valid_session_name(old_name) {
                        return Err(ParseError::InvalidSessionName {
                            name: old_name.to_string(),
                        });
                    }
                    if !is_valid_session_name(new_name) {
                        return Err(ParseError::InvalidSessionName {
                            name: new_name.to_string(),
                        });
                    }

                    Ok(
                        json!({ "id": id, "action": "state_rename", "oldName": old_name, "newName": new_name }),
                    )
                }
                Some(sub) => Err(ParseError::UnknownSubcommand {
                    subcommand: sub.to_string(),
                    valid_options: VALID,
                }),
                None => Err(ParseError::MissingArguments {
                    context: "state".to_string(),
                    usage: "state <save|load|list|clear|show|clean|rename> ...",
                }),
            }
        }

        // === iOS-specific commands ===
        "tap" => {
            // Alias for click (semantic clarity for touch interfaces)
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "tap".to_string(),
                usage: "tap <selector>",
            })?;
            Ok(json!({ "id": id, "action": "tap", "selector": sel }))
        }
        "swipe" => {
            let direction = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "swipe".to_string(),
                usage: "swipe <up|down|left|right> [distance]",
            })?;
            let valid_directions = ["up", "down", "left", "right"];
            if !valid_directions.contains(direction) {
                return Err(ParseError::InvalidValue {
                    message: format!("Invalid swipe direction: {}", direction),
                    usage: "swipe <up|down|left|right> [distance]",
                });
            }
            let mut cmd = json!({ "id": id, "action": "swipe", "direction": direction });
            if let Some(distance) = rest.get(1) {
                if let Ok(d) = distance.parse::<u32>() {
                    cmd.as_object_mut()
                        .unwrap()
                        .insert("distance".to_string(), json!(d));
                }
            }
            Ok(cmd)
        }
        "device" => {
            match rest.first().copied() {
                Some("list") | None => {
                    // List available iOS simulators
                    Ok(json!({ "id": id, "action": "device_list" }))
                }
                Some(sub) => Err(ParseError::UnknownSubcommand {
                    subcommand: sub.to_string(),
                    valid_options: &["list"],
                }),
            }
        }

        "diff" => parse_diff(&rest, &id),

        // === Batch ===
        "batch" => {
            let bail = rest.contains(&"--bail");
            let commands: Vec<&str> = rest.iter().filter(|a| **a != "--bail").copied().collect();
            let mut cmd = json!({ "id": id, "action": "batch", "bail": bail });
            if !commands.is_empty() {
                cmd["commands"] = json!(commands);
            }
            Ok(cmd)
        }

        // === React (requires `open --enable react-devtools`) ===
        "react" => parse_react(&rest, &id),

        // === Core Web Vitals + hydration ===
        "vitals" | "web-vitals" => {
            let mut cmd = json!({ "id": id, "action": "vitals" });
            let json_out = rest.contains(&"--json");
            if json_out {
                cmd["json"] = json!(true);
            }
            if let Some(url) = rest.iter().find(|a| !a.starts_with("--")) {
                cmd["url"] = json!(url);
            }
            Ok(cmd)
        }

        // === SPA client-side navigation ===
        "pushstate" => {
            let url = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "pushstate".to_string(),
                usage: "pushstate <url>",
            })?;
            Ok(json!({ "id": id, "action": "pushstate", "url": url }))
        }

        // === Remove init script ===
        "removeinitscript" => {
            let identifier = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "removeinitscript".to_string(),
                usage: "removeinitscript <identifier>",
            })?;
            Ok(json!({ "id": id, "action": "removeinitscript", "identifier": identifier }))
        }

        _ => Err(ParseError::UnknownCommand {
            command: cmd.to_string(),
        }),
    }
}

fn parse_react(rest: &[&str], id: &str) -> Result<Value, ParseError> {
    const VALID: &[&str] = &["tree", "inspect", "renders", "suspense"];
    let sub = rest.first().copied().ok_or(ParseError::MissingArguments {
        context: "react".to_string(),
        usage: "react <tree|inspect|renders|suspense>",
    })?;
    let json_out = rest.contains(&"--json");
    let flag = |key: &str| -> Value {
        if json_out {
            json!({ "id": id, "action": key, "json": true })
        } else {
            json!({ "id": id, "action": key })
        }
    };
    match sub {
        "tree" => Ok(flag("react_tree")),
        "inspect" => {
            let id_arg = rest
                .iter()
                .skip(1)
                .find(|a| !a.starts_with("--"))
                .copied()
                .ok_or(ParseError::MissingArguments {
                    context: "react inspect".to_string(),
                    usage: "react inspect <id>",
                })?;
            let numeric: i64 = id_arg.parse().map_err(|_| ParseError::InvalidValue {
                message: format!("react inspect id must be a number, got '{}'", id_arg),
                usage: "react inspect <id>",
            })?;
            let mut cmd = json!({ "id": id, "action": "react_inspect", "fiberId": numeric });
            if json_out {
                cmd["json"] = json!(true);
            }
            Ok(cmd)
        }
        "renders" => {
            let op = rest.get(1).copied().unwrap_or("start");
            match op {
                "start" => Ok(flag("react_renders_start")),
                "stop" => Ok(flag("react_renders_stop")),
                other => Err(ParseError::UnknownSubcommand {
                    subcommand: other.to_string(),
                    valid_options: &["start", "stop"],
                }),
            }
        }
        "suspense" => {
            let only_dynamic = rest.contains(&"--only-dynamic");
            let mut cmd = json!({ "id": id, "action": "react_suspense" });
            if json_out {
                cmd["json"] = json!(true);
            }
            if only_dynamic {
                cmd["onlyDynamic"] = json!(true);
            }
            Ok(cmd)
        }
        other => Err(ParseError::UnknownSubcommand {
            subcommand: other.to_string(),
            valid_options: VALID,
        }),
    }
}

fn parse_diff(rest: &[&str], id: &str) -> Result<Value, ParseError> {
    const VALID: &[&str] = &["snapshot", "screenshot", "url"];

    match rest.first().copied() {
        Some("snapshot") => {
            let mut cmd = json!({ "id": id, "action": "diff_snapshot" });
            let obj = cmd.as_object_mut().unwrap();
            let mut i = 1;
            while i < rest.len() {
                match rest[i] {
                    "-b" | "--baseline" => {
                        if let Some(path) = rest.get(i + 1) {
                            obj.insert("baseline".to_string(), json!(path));
                            i += 1;
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "diff snapshot --baseline".to_string(),
                                usage: "diff snapshot --baseline <file>",
                            });
                        }
                    }
                    "-s" | "--selector" => {
                        if let Some(s) = rest.get(i + 1) {
                            obj.insert("selector".to_string(), json!(s));
                            i += 1;
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "diff snapshot --selector".to_string(),
                                usage: "diff snapshot --selector <sel>",
                            });
                        }
                    }
                    "-c" | "--compact" => {
                        obj.insert("compact".to_string(), json!(true));
                    }
                    "-d" | "--depth" => {
                        if let Some(d) = rest.get(i + 1) {
                            match d.parse::<u32>() {
                                Ok(n) => {
                                    obj.insert("maxDepth".to_string(), json!(n));
                                    i += 1;
                                }
                                Err(_) => {
                                    return Err(ParseError::InvalidValue {
                                        message: format!(
                                            "Depth must be a non-negative integer, got: {}",
                                            d
                                        ),
                                        usage: "diff snapshot --depth <n>",
                                    });
                                }
                            }
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "diff snapshot --depth".to_string(),
                                usage: "diff snapshot --depth <n>",
                            });
                        }
                    }
                    other if other.starts_with('-') => {
                        return Err(ParseError::InvalidValue {
                            message: format!("Unknown flag: {}", other),
                            usage: "diff snapshot [--baseline <file>] [--selector <sel>] [--compact] [--depth <n>]",
                        });
                    }
                    other => {
                        return Err(ParseError::InvalidValue {
                            message: format!("Unexpected argument: {}", other),
                            usage: "diff snapshot [--baseline <file>] [--selector <sel>] [--compact] [--depth <n>]",
                        });
                    }
                }
                i += 1;
            }
            Ok(cmd)
        }
        Some("screenshot") => {
            let mut cmd = json!({ "id": id, "action": "diff_screenshot" });
            let obj = cmd.as_object_mut().unwrap();
            let mut i = 1;
            while i < rest.len() {
                match rest[i] {
                    "-b" | "--baseline" => {
                        if let Some(path) = rest.get(i + 1) {
                            obj.insert("baseline".to_string(), json!(path));
                            i += 1;
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "diff screenshot --baseline".to_string(),
                                usage: "diff screenshot --baseline <file>",
                            });
                        }
                    }
                    "-o" | "--output" => {
                        if let Some(path) = rest.get(i + 1) {
                            obj.insert("output".to_string(), json!(path));
                            i += 1;
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "diff screenshot --output".to_string(),
                                usage: "diff screenshot --output <file>",
                            });
                        }
                    }
                    "-t" | "--threshold" => {
                        if let Some(t) = rest.get(i + 1) {
                            match t.parse::<f64>() {
                                Ok(n) if (0.0..=1.0).contains(&n) => {
                                    obj.insert("threshold".to_string(), json!(n));
                                    i += 1;
                                }
                                Ok(n) => {
                                    return Err(ParseError::InvalidValue {
                                        message: format!(
                                            "Threshold must be between 0 and 1, got {}",
                                            n
                                        ),
                                        usage: "diff screenshot --threshold <0-1>",
                                    });
                                }
                                Err(_) => {
                                    return Err(ParseError::InvalidValue {
                                        message: format!("Invalid threshold value: {}", t),
                                        usage: "diff screenshot --threshold <0-1>",
                                    });
                                }
                            }
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "diff screenshot --threshold".to_string(),
                                usage: "diff screenshot --threshold <0-1>",
                            });
                        }
                    }
                    "-s" | "--selector" => {
                        if let Some(s) = rest.get(i + 1) {
                            obj.insert("selector".to_string(), json!(s));
                            i += 1;
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "diff screenshot --selector".to_string(),
                                usage: "diff screenshot --selector <sel>",
                            });
                        }
                    }
                    "--full" | "-f" => {
                        obj.insert("fullPage".to_string(), json!(true));
                    }
                    other if other.starts_with('-') => {
                        return Err(ParseError::InvalidValue {
                            message: format!("Unknown flag: {}", other),
                            usage: "diff screenshot --baseline <file> [--output <file>] [--threshold <0-1>] [--selector <sel>] [--full/-f]",
                        });
                    }
                    other => {
                        return Err(ParseError::InvalidValue {
                            message: format!("Unexpected argument: {}", other),
                            usage: "diff screenshot --baseline <file> [--output <file>] [--threshold <0-1>] [--selector <sel>] [--full/-f]",
                        });
                    }
                }
                i += 1;
            }
            if !obj.contains_key("baseline") {
                return Err(ParseError::MissingArguments {
                    context: "diff screenshot".to_string(),
                    usage: "diff screenshot --baseline <file>",
                });
            }
            Ok(cmd)
        }
        Some("url") => {
            let url1 = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "diff url".to_string(),
                usage: "diff url <url1> <url2>",
            })?;
            let url2 = rest.get(2).ok_or_else(|| ParseError::MissingArguments {
                context: "diff url".to_string(),
                usage: "diff url <url1> <url2>",
            })?;
            let mut cmd = json!({
                "id": id,
                "action": "diff_url",
                "url1": url1,
                "url2": url2,
            });
            let obj = cmd.as_object_mut().unwrap();
            let mut i = 3;
            while i < rest.len() {
                match rest[i] {
                    "--screenshot" => {
                        obj.insert("screenshot".to_string(), json!(true));
                    }
                    "--full" | "-f" => {
                        obj.insert("fullPage".to_string(), json!(true));
                    }
                    "--wait-until" => {
                        if let Some(val) = rest.get(i + 1) {
                            obj.insert("waitUntil".to_string(), json!(val));
                            i += 1;
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "diff url --wait-until".to_string(),
                                usage: "diff url <url1> <url2> --wait-until <load|domcontentloaded|networkidle>",
                            });
                        }
                    }
                    "-s" | "--selector" => {
                        if let Some(s) = rest.get(i + 1) {
                            obj.insert("selector".to_string(), json!(s));
                            i += 1;
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "diff url --selector".to_string(),
                                usage: "diff url <url1> <url2> --selector <sel>",
                            });
                        }
                    }
                    "-c" | "--compact" => {
                        obj.insert("compact".to_string(), json!(true));
                    }
                    "-d" | "--depth" => {
                        if let Some(d) = rest.get(i + 1) {
                            match d.parse::<u32>() {
                                Ok(n) => {
                                    obj.insert("maxDepth".to_string(), json!(n));
                                    i += 1;
                                }
                                Err(_) => {
                                    return Err(ParseError::InvalidValue {
                                        message: format!(
                                            "Depth must be a non-negative integer, got: {}",
                                            d
                                        ),
                                        usage: "diff url <url1> <url2> --depth <n>",
                                    });
                                }
                            }
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "diff url --depth".to_string(),
                                usage: "diff url <url1> <url2> --depth <n>",
                            });
                        }
                    }
                    other if other.starts_with('-') => {
                        return Err(ParseError::InvalidValue {
                            message: format!("Unknown flag: {}", other),
                            usage: "diff url <url1> <url2> [--screenshot] [--full/-f] [--wait-until <strategy>] [--selector <sel>] [--compact] [--depth <n>]",
                        });
                    }
                    other => {
                        return Err(ParseError::InvalidValue {
                            message: format!("Unexpected argument: {}", other),
                            usage: "diff url <url1> <url2> [--screenshot] [--full/-f] [--wait-until <strategy>] [--selector <sel>] [--compact] [--depth <n>]",
                        });
                    }
                }
                i += 1;
            }
            Ok(cmd)
        }
        Some(sub) => Err(ParseError::UnknownSubcommand {
            subcommand: sub.to_string(),
            valid_options: VALID,
        }),
        None => Err(ParseError::MissingArguments {
            context: "diff".to_string(),
            usage: "diff <snapshot|screenshot|url>",
        }),
    }
}

fn parse_get(rest: &[&str], id: &str) -> Result<Value, ParseError> {
    const VALID: &[&str] = &[
        "text", "html", "value", "attr", "url", "title", "count", "box", "styles", "cdp-url",
    ];

    match rest.first().copied() {
        Some("text") => {
            // `get text --all-frames` aggregates visible text across every
            // frame, including out-of-process iframes invisible to the top
            // document (issue #27). The selector is ignored in this mode.
            let all_frames = rest[1..]
                .iter()
                .any(|a| matches!(*a, "--all-frames" | "--frames" | "-a"));
            if all_frames {
                return Ok(json!({ "id": id, "action": "gettext", "allFrames": true }));
            }
            // `get text --pierce` reads through CLOSED shadow DOM / child docs
            // via the CDP DOM tree — content eval/innerText can't reach, e.g. an
            // extension's injected panel in a closed shadow root (issue #30).
            let pierce = rest[1..]
                .iter()
                .any(|a| matches!(*a, "--pierce" | "--shadow" | "--deep"));
            if pierce {
                return Ok(json!({ "id": id, "action": "gettext", "pierce": true }));
            }
            // `get text --main` returns the main-content region (readability),
            // skipping header/nav/footer/sidebar boilerplate (issue #27).
            let main = rest[1..]
                .iter()
                .any(|a| matches!(*a, "--main" | "--readable" | "-m"));
            if main {
                return Ok(json!({ "id": id, "action": "gettext", "main": true }));
            }
            // `get text` with no selector reads the WHOLE PAGE and now defaults
            // to cross-frame aggregation, so an agent gets a page's iframed
            // content (listing descriptions etc.) without having to know about
            // `--all-frames` (#27). On a single-frame page this is identical to
            // the old body read; multi-frame pages get the child frames too —
            // a strict superset. An explicit selector stays element-scoped.
            match rest.iter().skip(1).find(|a| !a.starts_with("--")).copied() {
                Some(sel) => Ok(json!({ "id": id, "action": "gettext", "selector": sel })),
                None => Ok(json!({ "id": id, "action": "gettext", "allFrames": true })),
            }
        }
        Some("html") => {
            let sel = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "get html".to_string(),
                usage: "get html <selector>",
            })?;
            Ok(json!({ "id": id, "action": "innerhtml", "selector": sel }))
        }
        Some("value") => {
            let sel = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "get value".to_string(),
                usage: "get value <selector>",
            })?;
            Ok(json!({ "id": id, "action": "inputvalue", "selector": sel }))
        }
        Some("attr") => {
            let sel = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "get attr".to_string(),
                usage: "get attr <selector> <attribute>",
            })?;
            let attr = rest.get(2).ok_or_else(|| ParseError::MissingArguments {
                context: "get attr".to_string(),
                usage: "get attr <selector> <attribute>",
            })?;
            Ok(json!({ "id": id, "action": "getattribute", "selector": sel, "attribute": attr }))
        }
        Some("url") => Ok(json!({ "id": id, "action": "url" })),
        Some("cdp-url") => Ok(json!({ "id": id, "action": "cdp_url" })),
        Some("title") => Ok(json!({ "id": id, "action": "title" })),
        Some("count") => {
            let sel = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "get count".to_string(),
                usage: "get count <selector>",
            })?;
            Ok(json!({ "id": id, "action": "count", "selector": sel }))
        }
        Some("box") => {
            let sel = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "get box".to_string(),
                usage: "get box <selector>",
            })?;
            Ok(json!({ "id": id, "action": "boundingbox", "selector": sel }))
        }
        Some("styles") => {
            let sel = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "get styles".to_string(),
                usage: "get styles <selector>",
            })?;
            Ok(json!({ "id": id, "action": "styles", "selector": sel }))
        }
        Some(sub) => Err(ParseError::UnknownSubcommand {
            subcommand: sub.to_string(),
            valid_options: VALID,
        }),
        None => Err(ParseError::MissingArguments {
            context: "get".to_string(),
            usage: "get <text|html|value|attr|url|title|count|box|styles|cdp-url> [args...]",
        }),
    }
}

fn parse_is(rest: &[&str], id: &str) -> Result<Value, ParseError> {
    const VALID: &[&str] = &["visible", "enabled", "checked"];

    match rest.first().copied() {
        Some("visible") => {
            let sel = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "is visible".to_string(),
                usage: "is visible <selector>",
            })?;
            Ok(json!({ "id": id, "action": "isvisible", "selector": sel }))
        }
        Some("enabled") => {
            let sel = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "is enabled".to_string(),
                usage: "is enabled <selector>",
            })?;
            Ok(json!({ "id": id, "action": "isenabled", "selector": sel }))
        }
        Some("checked") => {
            let sel = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "is checked".to_string(),
                usage: "is checked <selector>",
            })?;
            Ok(json!({ "id": id, "action": "ischecked", "selector": sel }))
        }
        Some(sub) => Err(ParseError::UnknownSubcommand {
            subcommand: sub.to_string(),
            valid_options: VALID,
        }),
        None => Err(ParseError::MissingArguments {
            context: "is".to_string(),
            usage: "is <visible|enabled|checked> <selector>",
        }),
    }
}

/// `form fill --map <json>` — fill a whole form from a {label|selector: value}
/// map in one call, then optionally submit, returning per-field status + any
/// inline validation errors. Values: string → text/select/radio, bool → checkbox.
fn parse_form(rest: &[&str], id: &str) -> Result<Value, ParseError> {
    let usage =
        "form fill --map <json> [--submit <selector|text>]  (also --map-file <path> / --stdin)";
    if rest.first().copied() != Some("fill") {
        return Err(ParseError::UnknownSubcommand {
            subcommand: rest.first().copied().unwrap_or("").to_string(),
            valid_options: &["fill"],
        });
    }
    let rest = &rest[1..];
    let mut submit: Option<String> = None;
    let mut map_str: Option<String> = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i] {
            "--map" => {
                map_str = rest.get(i + 1).map(|s| s.to_string());
                i += 1;
            }
            "--map-file" => {
                let p = rest.get(i + 1).ok_or(ParseError::MissingArguments {
                    context: "form fill --map-file".to_string(),
                    usage,
                })?;
                map_str =
                    Some(
                        std::fs::read_to_string(p).map_err(|e| ParseError::InvalidValue {
                            message: format!("form fill --map-file: cannot read {p}: {e}"),
                            usage,
                        })?,
                    );
                i += 1;
            }
            "--stdin" => {
                use std::io::BufRead;
                let s = std::io::stdin();
                map_str = Some(
                    s.lock()
                        .lines()
                        .map(|l| l.unwrap_or_default())
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
            "--submit" => {
                submit = rest.get(i + 1).map(|s| s.to_string());
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    let map_str = map_str.ok_or(ParseError::MissingArguments {
        context: "form fill".to_string(),
        usage,
    })?;
    let map: Value =
        serde_json::from_str(map_str.trim()).map_err(|e| ParseError::InvalidValue {
            message: format!("form fill --map is not valid JSON: {e}"),
            usage,
        })?;
    if !map.is_object() || map.as_object().map(|m| m.is_empty()).unwrap_or(true) {
        return Err(ParseError::InvalidValue {
            message: "form fill --map must be a non-empty JSON object".to_string(),
            usage,
        });
    }
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), json!(id));
    obj.insert("action".into(), json!("form_fill"));
    obj.insert("map".into(), map);
    if let Some(s) = submit {
        obj.insert("submit".into(), json!(s));
    }
    Ok(Value::Object(obj))
}

/// `extract --schema <json>` — declarative structured scrape → JSON. Schema:
/// `{ "rows"?: "<css>", "fields": { "k": "<css>" | {"sel","get","all"} } }`.
/// `get` = text (default) | @attr | html | value. Compiled to one eval (see
/// handle_extract). `--frame` is leading-only, mirroring `eval`.
fn parse_extract(rest: &[&str], id: &str) -> Result<Value, ParseError> {
    let usage = "extract --schema <json> | --schema-file <path> | --stdin  [--frame <f>]";
    // Leading-only --frame (copy eval's pattern so it never eats a schema token).
    let mut frame: Option<String> = None;
    let rest: Vec<&str> = if rest.first() == Some(&"--frame") {
        let v = rest.get(1).copied().ok_or(ParseError::InvalidValue {
            message: "extract --frame requires a value".to_string(),
            usage,
        })?;
        frame = Some(v.to_string());
        rest[2..].to_vec()
    } else {
        rest.to_vec()
    };

    let schema_str = match rest.first().copied() {
        Some("--schema") => {
            rest.get(1)
                .map(|s| s.to_string())
                .ok_or(ParseError::MissingArguments {
                    context: "extract --schema".to_string(),
                    usage,
                })?
        }
        Some("--schema-file") => {
            let path = rest.get(1).ok_or(ParseError::MissingArguments {
                context: "extract --schema-file".to_string(),
                usage,
            })?;
            std::fs::read_to_string(path).map_err(|e| ParseError::InvalidValue {
                message: format!("extract --schema-file: cannot read {path}: {e}"),
                usage,
            })?
        }
        Some("--stdin") => {
            use std::io::BufRead;
            let stdin = std::io::stdin();
            stdin
                .lock()
                .lines()
                .map(|l| l.unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => {
            return Err(ParseError::MissingArguments {
                context: "extract".to_string(),
                usage,
            })
        }
    };

    let schema: Value =
        serde_json::from_str(schema_str.trim()).map_err(|e| ParseError::InvalidValue {
            message: format!("extract schema is not valid JSON: {e}"),
            usage,
        })?;
    if !schema.get("fields").map(|f| f.is_object()).unwrap_or(false) {
        return Err(ParseError::InvalidValue {
            message: "extract schema needs a \"fields\" object".to_string(),
            usage,
        });
    }

    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), json!(id));
    obj.insert("action".into(), json!("extract"));
    obj.insert("schema".into(), schema);
    if let Some(f) = frame {
        obj.insert("frame".into(), json!(f));
    }
    Ok(Value::Object(obj))
}

/// `expect <condition>` — pass/fail assertion verb. Subject-first for element
/// state (`expect <sel> visible`), keyword-first for the rest (count/text/value/
/// attr/url/request/no-errors). Waits up to --timeout by default. See the help
/// text in output.rs for the full grammar.
fn parse_expect(rest: &[&str], id: &str) -> Result<Value, ParseError> {
    // Split out boolean/value flags first; the positional grammar is parsed from
    // what's left, so flags may appear anywhere after the subject.
    let mut pos: Vec<&str> = Vec::new();
    let mut timeout: Option<u64> = None;
    let mut no_wait = false;
    let mut regex = false;
    let mut not = false;
    let mut method: Option<String> = None;
    let mut status: Option<String> = None;
    let mut rtype: Option<String> = None;
    let mut req_count: Option<u64> = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i] {
            "--no-wait" => no_wait = true,
            "--regex" => regex = true,
            "--not" => not = true,
            "--timeout" => {
                timeout = rest.get(i + 1).and_then(|v| v.parse().ok());
                i += 1;
            }
            "--method" => {
                method = rest.get(i + 1).map(|s| s.to_string());
                i += 1;
            }
            "--status" => {
                status = rest.get(i + 1).map(|s| s.to_string());
                i += 1;
            }
            "--type" => {
                rtype = rest.get(i + 1).map(|s| s.to_string());
                i += 1;
            }
            "--count" => {
                req_count = rest.get(i + 1).and_then(|v| v.parse().ok());
                i += 1;
            }
            other => pos.push(other),
        }
        i += 1;
    }

    let usage = "expect <sel> <visible|hidden|gone|present>  |  expect count <sel> <op> <n>  |  expect text|value <sel> <equals|contains|matches> <str>  |  expect attr <sel> <name> <pred> <str>  |  expect url <pred> <str>  |  expect request <url-substr> [--method M --status S]  |  expect no-errors";
    let miss = |ctx: &str| ParseError::MissingArguments {
        context: ctx.to_string(),
        usage,
    };
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), json!(id));
    obj.insert("action".into(), json!("expect"));
    if let Some(t) = timeout {
        obj.insert("timeout".into(), json!(t));
    }
    obj.insert("noWait".into(), json!(no_wait));
    obj.insert("not".into(), json!(not));

    let first = *pos.first().ok_or_else(|| miss("expect"))?;
    match first {
        "count" => {
            let sel = pos.get(1).ok_or_else(|| miss("expect count"))?;
            if sel.starts_with('@') {
                return Err(ParseError::InvalidValue {
                    message: "expect count takes a CSS selector, not an @ref".to_string(),
                    usage,
                });
            }
            let op_raw = *pos
                .get(2)
                .ok_or_else(|| miss("expect count <sel> <op> <n>"))?;
            let op = canon_count_op(op_raw).ok_or_else(|| ParseError::InvalidValue {
                message: format!("bad count op '{op_raw}' (use ==,!=,>,<,>=,<= or eq/ne/gt/lt/ge/le; quote > and <)"),
                usage,
            })?;
            let n: u64 = pos.get(3).and_then(|v| v.parse().ok()).ok_or_else(|| {
                ParseError::InvalidValue {
                    message: "expect count needs a numeric <n>".to_string(),
                    usage,
                }
            })?;
            obj.insert("kind".into(), json!("count"));
            obj.insert("selector".into(), json!(sel));
            obj.insert("op".into(), json!(op));
            obj.insert("value".into(), json!(n));
        }
        "text" | "value" => {
            let sel = pos.get(1).ok_or_else(|| miss("expect text/value"))?;
            let pred = *pos
                .get(2)
                .ok_or_else(|| miss("expect text <sel> <equals|contains|matches> <str>"))?;
            canon_predicate(pred).ok_or_else(|| ParseError::InvalidValue {
                message: format!("bad predicate '{pred}' (use equals|contains|matches)"),
                usage,
            })?;
            let expected = pos.get(3..).map(|s| s.join(" ")).unwrap_or_default();
            if pos.len() < 4 {
                return Err(miss("expect text <sel> <pred> <str>"));
            }
            obj.insert("kind".into(), json!(first));
            obj.insert("selector".into(), json!(sel));
            obj.insert("predicate".into(), json!(pred));
            obj.insert("expected".into(), json!(expected));
            obj.insert("regex".into(), json!(regex || pred == "matches"));
        }
        "attr" => {
            let sel = pos.get(1).ok_or_else(|| miss("expect attr"))?;
            let name = pos
                .get(2)
                .ok_or_else(|| miss("expect attr <sel> <name> <pred> <str>"))?;
            let pred = *pos
                .get(3)
                .ok_or_else(|| miss("expect attr <sel> <name> <pred> <str>"))?;
            canon_predicate(pred).ok_or_else(|| ParseError::InvalidValue {
                message: format!("bad predicate '{pred}' (use equals|contains|matches)"),
                usage,
            })?;
            if pos.len() < 5 {
                return Err(miss("expect attr <sel> <name> <pred> <str>"));
            }
            let expected = pos[4..].join(" ");
            obj.insert("kind".into(), json!("attr"));
            obj.insert("selector".into(), json!(sel));
            obj.insert("attr".into(), json!(name));
            obj.insert("predicate".into(), json!(pred));
            obj.insert("expected".into(), json!(expected));
            obj.insert("regex".into(), json!(regex || pred == "matches"));
        }
        "url" => {
            let pred = *pos
                .get(1)
                .ok_or_else(|| miss("expect url <equals|contains|matches> <str>"))?;
            canon_predicate(pred).ok_or_else(|| ParseError::InvalidValue {
                message: format!("bad predicate '{pred}' (use equals|contains|matches)"),
                usage,
            })?;
            let expected = pos.get(2..).map(|s| s.join(" ")).unwrap_or_default();
            if pos.len() < 3 {
                return Err(miss("expect url <pred> <str>"));
            }
            obj.insert("kind".into(), json!("url"));
            obj.insert("predicate".into(), json!(pred));
            obj.insert("expected".into(), json!(expected));
            obj.insert("regex".into(), json!(regex));
        }
        "request" => {
            let filter = pos
                .get(1)
                .ok_or_else(|| miss("expect request <url-substr>"))?;
            obj.insert("kind".into(), json!("request"));
            obj.insert("filter".into(), json!(filter));
            if let Some(m) = method {
                obj.insert("method".into(), json!(m));
            }
            if let Some(s) = status {
                obj.insert("status".into(), json!(s));
            }
            if let Some(t) = rtype {
                obj.insert("type".into(), json!(t));
            }
            if let Some(c) = req_count {
                obj.insert("count".into(), json!(c));
            }
        }
        "no-errors" => {
            obj.insert("kind".into(), json!("errors"));
            obj.insert("max".into(), json!(0));
        }
        // Subject-first element-state assertion: `expect <sel> <state>`.
        sel => {
            let state_raw = *pos
                .get(1)
                .ok_or_else(|| miss("expect <sel> <visible|hidden|gone|present>"))?;
            let state = canon_element_state(state_raw).ok_or_else(|| ParseError::InvalidValue {
                message: format!("unknown state '{state_raw}' (use visible|hidden|gone|present|attached|detached)"),
                usage,
            })?;
            obj.insert("kind".into(), json!("element"));
            obj.insert("selector".into(), json!(sel));
            obj.insert("state".into(), json!(state));
        }
    }
    Ok(Value::Object(obj))
}

/// Map a count operator token (symbolic or word) to canonical eq/ne/gt/lt/ge/le.
fn canon_count_op(op: &str) -> Option<&'static str> {
    match op {
        "==" | "eq" => Some("eq"),
        "!=" | "ne" => Some("ne"),
        ">" | "gt" => Some("gt"),
        "<" | "lt" => Some("lt"),
        ">=" | "ge" => Some("ge"),
        "<=" | "le" => Some("le"),
        _ => None,
    }
}

fn canon_predicate(p: &str) -> Option<&'static str> {
    match p {
        "equals" => Some("equals"),
        "contains" => Some("contains"),
        "matches" => Some("matches"),
        _ => None,
    }
}

/// Canonicalize an `expect` element state to what the wait helpers understand:
/// `gone`→`detached`, `present`→`attached`; visible/hidden/attached/detached
/// pass through. (Review fix: the helpers don't understand gone/present.)
fn canon_element_state(s: &str) -> Option<&'static str> {
    match s {
        "visible" => Some("visible"),
        "hidden" => Some("hidden"),
        "gone" | "detached" => Some("detached"),
        "present" | "attached" => Some("attached"),
        _ => None,
    }
}

fn parse_read(rest: &[&str], id: &str, flags: &Flags) -> Result<Value, ParseError> {
    const READ_USAGE: &str =
        "read [url] [--raw] [--require-md] [--llms <index|full>] [--outline] [--filter <text>] [--timeout <ms>]";
    let mut cmd = json!({
        "id": id,
        "action": "read",
        "timeout": crate::read::default_timeout_ms(),
    });
    let mut url: Option<&str> = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i] {
            "--raw" => {
                cmd["raw"] = json!(true);
            }
            "--require-md" => {
                cmd["requireMd"] = json!(true);
            }
            "--llms" => {
                let value = rest
                    .get(i + 1)
                    .ok_or_else(|| ParseError::MissingArguments {
                        context: "read --llms".to_string(),
                        usage: READ_USAGE,
                    })?;
                crate::read::parse_llms_mode(value).map_err(|message| {
                    ParseError::InvalidValue {
                        message,
                        usage: READ_USAGE,
                    }
                })?;
                cmd["llms"] = json!(value);
                i += 1;
            }
            "--outline" => {
                cmd["outline"] = json!(true);
            }
            "--filter" => {
                let value = rest
                    .get(i + 1)
                    .ok_or_else(|| ParseError::MissingArguments {
                        context: "read --filter".to_string(),
                        usage: READ_USAGE,
                    })?;
                cmd["filter"] = json!(value);
                i += 1;
            }
            "--timeout" => {
                let value = rest
                    .get(i + 1)
                    .ok_or_else(|| ParseError::MissingArguments {
                        context: "read --timeout".to_string(),
                        usage: READ_USAGE,
                    })?;
                let timeout = crate::read::parse_timeout_ms(value).map_err(|message| {
                    ParseError::InvalidValue {
                        message,
                        usage: READ_USAGE,
                    }
                })?;
                cmd["timeout"] = json!(timeout);
                i += 1;
            }
            "--json" => {
                cmd["json"] = json!(true);
            }
            arg if arg.starts_with("--") => {
                return Err(ParseError::UnknownSubcommand {
                    subcommand: arg.to_string(),
                    valid_options: &[
                        "--raw",
                        "--require-md",
                        "--llms",
                        "--outline",
                        "--filter",
                        "--timeout",
                        "--json",
                    ],
                });
            }
            arg => {
                if url.is_some() {
                    return Err(ParseError::InvalidValue {
                        message: format!("Unexpected read argument: {}", arg),
                        usage: READ_USAGE,
                    });
                }
                url = Some(arg);
            }
        }
        i += 1;
    }
    if cmd.get("llms").is_some()
        && cmd
            .get("outline")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    {
        return Err(ParseError::InvalidValue {
            message: "read --llms and --outline cannot be used together".to_string(),
            usage: READ_USAGE,
        });
    }
    if let Some(url) = url {
        cmd["url"] = json!(url);
    }
    if let Some(ref headers_json) = flags.headers {
        let headers = serde_json::from_str::<serde_json::Value>(headers_json).map_err(|_| {
            ParseError::InvalidValue {
                message: format!("Invalid JSON for --headers: {}", headers_json),
                usage: READ_USAGE,
            }
        })?;
        if !headers.is_object() {
            return Err(ParseError::InvalidValue {
                message: format!("Invalid JSON object for --headers: {}", headers_json),
                usage: READ_USAGE,
            });
        }
        cmd["headers"] = headers;
    }
    if let Some(ref allowed_domains) = flags.allowed_domains {
        cmd["allowedDomains"] = json!(allowed_domains);
    }
    Ok(cmd)
}

fn parse_find(rest: &[&str], id: &str) -> Result<Value, ParseError> {
    let locator = rest.first().ok_or_else(|| ParseError::MissingArguments {
        context: "find".to_string(),
        usage: "find <locator> <value> [action] [text]",
    })?;

    match *locator {
        "role" | "text" | "label" | "placeholder" | "alt" | "title" | "testid" | "first"
        | "last" => {
            let value = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: format!("find {}", locator),
                usage: match *locator {
                    "role" => "find role <role> [action] [--name <name>] [--exact]",
                    "text" => "find text <text> [action] [--exact]",
                    "label" => "find label <label> [action] [text] [--exact]",
                    "placeholder" => "find placeholder <text> [action] [text] [--exact]",
                    "alt" => "find alt <text> [action] [--exact]",
                    "title" => "find title <text> [action] [--exact]",
                    "testid" => "find testid <id> [action] [text]",
                    "first" => "find first <selector> [action] [text]",
                    "last" => "find last <selector> [action] [text]",
                    _ => "find <locator> <value> [action] [text]",
                },
            })?;
            let raw_subaction = rest.get(2).copied();
            if let Some(s) = raw_subaction {
                if s.starts_with("--") {
                    return Err(ParseError::InvalidValue {
                        message: format!(
                            "Missing action verb for `find {locator}` (got `{flag}` where action was expected).\n\
                             Valid actions: click, fill, check, hover, text\n\
                             Did you mean: chrome-use find {locator} <value> click {flag} ...?",
                            locator = locator,
                            flag = s,
                        ),
                        usage: match *locator {
                            "role" => "find role <role> <action> [--name <name>] [--exact]",
                            "text" => "find text <text> <action> [--exact]",
                            "label" => "find label <label> <action> [text] [--exact]",
                            "placeholder" => "find placeholder <text> <action> [text] [--exact]",
                            "alt" => "find alt <text> <action> [--exact]",
                            "title" => "find title <text> <action> [--exact]",
                            "testid" => "find testid <id> <action> [text]",
                            "first" => "find first <selector> <action> [text]",
                            "last" => "find last <selector> <action> [text]",
                            _ => "find <locator> <value> <action> [text]",
                        },
                    });
                }
            }
            let subaction = raw_subaction.unwrap_or("click");
            let mut name: Option<&str> = None;
            let mut exact = false;
            let mut fill_parts: Vec<&str> = Vec::new();

            if rest.len() > 3 {
                let mut i = 3;
                while i < rest.len() {
                    match rest[i] {
                        "--exact" => {
                            exact = true;
                            i += 1;
                        }
                        "--name" => {
                            let n =
                                rest.get(i + 1)
                                    .ok_or_else(|| ParseError::MissingArguments {
                                        context: format!("find {}", locator),
                                        usage:
                                            "find role <role> [action] [--name <name>] [--exact]",
                                    })?;
                            name = Some(*n);
                            i += 2;
                        }
                        token => {
                            fill_parts.push(token);
                            i += 1;
                        }
                    }
                }
            }

            let fill_value = if fill_parts.is_empty() {
                None
            } else {
                Some(fill_parts.join(" "))
            };

            match *locator {
                "role" => {
                    let mut cmd = json!({ "id": id, "action": "getbyrole", "role": value, "subaction": subaction, "name": name, "exact": exact });
                    if let Some(v) = fill_value {
                        cmd["value"] = json!(v);
                    }
                    Ok(cmd)
                }
                "text" => Ok(
                    json!({ "id": id, "action": "getbytext", "text": value, "subaction": subaction, "exact": exact }),
                ),
                "label" => {
                    let mut cmd = json!({ "id": id, "action": "getbylabel", "label": value, "subaction": subaction, "exact": exact });
                    if let Some(v) = fill_value {
                        cmd["value"] = json!(v);
                    }
                    Ok(cmd)
                }
                "placeholder" => {
                    let mut cmd = json!({ "id": id, "action": "getbyplaceholder", "placeholder": value, "subaction": subaction, "exact": exact });
                    if let Some(v) = fill_value {
                        cmd["value"] = json!(v);
                    }
                    Ok(cmd)
                }
                "alt" => Ok(
                    json!({ "id": id, "action": "getbyalttext", "text": value, "subaction": subaction, "exact": exact }),
                ),
                "title" => Ok(
                    json!({ "id": id, "action": "getbytitle", "text": value, "subaction": subaction, "exact": exact }),
                ),
                "testid" => {
                    let mut cmd = json!({ "id": id, "action": "getbytestid", "testId": value, "subaction": subaction });
                    if let Some(v) = fill_value {
                        cmd["value"] = json!(v);
                    }
                    Ok(cmd)
                }
                "first" => {
                    let mut cmd = json!({ "id": id, "action": "nth", "selector": value, "index": 0, "subaction": subaction });
                    if let Some(v) = fill_value {
                        cmd["value"] = json!(v);
                    }
                    Ok(cmd)
                }
                "last" => {
                    let mut cmd = json!({ "id": id, "action": "nth", "selector": value, "index": -1, "subaction": subaction });
                    if let Some(v) = fill_value {
                        cmd["value"] = json!(v);
                    }
                    Ok(cmd)
                }
                _ => unreachable!(),
            }
        }
        "nth" => {
            let idx_str = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "find nth".to_string(),
                usage: "find nth <index> <selector> [action] [text]",
            })?;
            let idx = idx_str
                .parse::<i32>()
                .map_err(|_| ParseError::MissingArguments {
                    context: "find nth".to_string(),
                    usage: "find nth <index> <selector> [action] [text]",
                })?;
            let sel = rest.get(2).ok_or_else(|| ParseError::MissingArguments {
                context: "find nth".to_string(),
                usage: "find nth <index> <selector> [action] [text]",
            })?;
            let sub = rest.get(3).unwrap_or(&"click");
            let fv = if rest.len() > 4 {
                Some(rest[4..].join(" "))
            } else {
                None
            };
            let mut cmd = json!({ "id": id, "action": "nth", "selector": sel, "index": idx, "subaction": sub });
            if let Some(v) = fv {
                cmd["value"] = json!(v);
            }
            Ok(cmd)
        }
        _ => Err(ParseError::InvalidValue {
            // The user passed a value where a locator keyword was expected — the
            // classic `find "I'm not a robot" click` mistake (issue #8.4). Lead
            // with the corrected command using their own value, then the menu.
            message: format!(
                "`{loc}` is not a find locator. To match by visible text, name the locator:\n  \
                 chrome-use find text \"{loc}\" click\n\n\
                 Locators: role, text, label, placeholder, alt, title, testid, first, last, nth\n\
                 Examples:\n  \
                 chrome-use find text \"Sign in\" click\n  \
                 chrome-use find role button --name \"Submit\" click\n  \
                 chrome-use find label \"Email\" fill you@example.com",
                loc = locator,
            ),
            usage: "find <locator> <value> [action] [text]",
        }),
    }
}

/// Parse a coordinate pair from `["449","320"]`, `["449,320"]`, or `["449, 320"]`.
/// Returns None if the args aren't a clean numeric pair (so callers fall back to
/// treating the argument as a selector). Used by first-class coordinate `click`.
fn parse_coords(args: &[&str]) -> Option<(f64, f64)> {
    let (a, b) = match args {
        [one] => one.split_once(',')?,
        [a, b] => (*a, *b),
        _ => return None,
    };
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

fn parse_mouse(rest: &[&str], id: &str) -> Result<Value, ParseError> {
    const VALID: &[&str] = &["move", "down", "up", "wheel"];

    match rest.first().copied() {
        Some("move") => {
            let x_str = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "mouse move".to_string(),
                usage: "mouse move <x> <y>",
            })?;
            let y_str = rest.get(2).ok_or_else(|| ParseError::MissingArguments {
                context: "mouse move".to_string(),
                usage: "mouse move <x> <y>",
            })?;
            let x = x_str
                .parse::<i32>()
                .map_err(|_| ParseError::MissingArguments {
                    context: "mouse move".to_string(),
                    usage: "mouse move <x> <y>",
                })?;
            let y = y_str
                .parse::<i32>()
                .map_err(|_| ParseError::MissingArguments {
                    context: "mouse move".to_string(),
                    usage: "mouse move <x> <y>",
                })?;
            Ok(json!({ "id": id, "action": "mousemove", "x": x, "y": y }))
        }
        Some("down") => {
            Ok(json!({ "id": id, "action": "mousedown", "button": rest.get(1).unwrap_or(&"left") }))
        }
        Some("up") => {
            Ok(json!({ "id": id, "action": "mouseup", "button": rest.get(1).unwrap_or(&"left") }))
        }
        Some("wheel") => {
            let dy = rest
                .get(1)
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(100);
            let dx = rest.get(2).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
            Ok(json!({ "id": id, "action": "wheel", "deltaX": dx, "deltaY": dy }))
        }
        Some(sub) => Err(ParseError::UnknownSubcommand {
            subcommand: sub.to_string(),
            valid_options: VALID,
        }),
        None => Err(ParseError::MissingArguments {
            context: "mouse".to_string(),
            usage: "mouse <move|down|up|wheel> [args...]",
        }),
    }
}

/// Parse a viewport / resize spec, shared by the top-level `viewport` / `resize`
/// commands and `set viewport`. Sets a CDP device-metrics override
/// (`Emulation.setDeviceMetricsOverride`) — a *virtual* viewport for the tab, so
/// it works headless AND on the extension relay without physically resizing the
/// user's real Chrome window (issue #47). Forms:
///   viewport <width> <height> [scale] [--dpr N] [--mobile]
///   viewport <width>x<height>        (e.g. 1280x800)
///   viewport reset | clear           (drop the override, restore real size)
fn parse_viewport(rest: &[&str], id: &str) -> Result<Value, ParseError> {
    const USAGE: &str = "viewport <width> <height> [scale] [--dpr N] [--mobile] | viewport reset";

    if matches!(
        rest.first().copied(),
        Some("reset") | Some("clear") | Some("off")
    ) {
        return Ok(json!({ "id": id, "action": "viewport", "reset": true }));
    }

    // Positional (non-flag) tokens. A `WxH` token counts as one positional.
    let positionals: Vec<&str> = rest
        .iter()
        .copied()
        .filter(|a| !a.starts_with("--"))
        .collect();

    let (w, h, scale_tok): (i32, i32, Option<&str>) = match positionals.first() {
        Some(first) if first.contains('x') || first.contains('X') => {
            let mut parts = first.split(['x', 'X']);
            let w = parts.next().and_then(|s| s.parse::<i32>().ok());
            let h = parts.next().and_then(|s| s.parse::<i32>().ok());
            match (w, h) {
                (Some(w), Some(h)) => (w, h, positionals.get(1).copied()),
                _ => {
                    return Err(ParseError::InvalidValue {
                        message: format!("Invalid viewport size: {}", first),
                        usage: USAGE,
                    })
                }
            }
        }
        Some(w_str) => {
            let h_str = positionals.get(1).ok_or(ParseError::MissingArguments {
                context: "viewport".to_string(),
                usage: USAGE,
            })?;
            let w = w_str.parse::<i32>().map_err(|_| ParseError::InvalidValue {
                message: format!("Invalid width: {}", w_str),
                usage: USAGE,
            })?;
            let h = h_str.parse::<i32>().map_err(|_| ParseError::InvalidValue {
                message: format!("Invalid height: {}", h_str),
                usage: USAGE,
            })?;
            (w, h, positionals.get(2).copied())
        }
        None => {
            return Err(ParseError::MissingArguments {
                context: "viewport".to_string(),
                usage: USAGE,
            })
        }
    };

    let mut cmd = json!({ "id": id, "action": "viewport", "width": w, "height": h });

    // Device-scale-factor: positional, overridden by --dpr / --scale.
    let mut scale: Option<f64> = match scale_tok {
        Some(s) => Some(s.parse::<f64>().map_err(|_| ParseError::InvalidValue {
            message: format!("Invalid scale: {}", s),
            usage: USAGE,
        })?),
        None => None,
    };
    if let Some(i) = rest.iter().position(|a| *a == "--dpr" || *a == "--scale") {
        let v = rest.get(i + 1).and_then(|s| s.parse::<f64>().ok()).ok_or(
            ParseError::InvalidValue {
                message: "--dpr/--scale needs a number".to_string(),
                usage: USAGE,
            },
        )?;
        scale = Some(v);
    }
    if let Some(s) = scale {
        cmd["deviceScaleFactor"] = json!(s);
    }
    if rest.contains(&"--mobile") {
        cmd["mobile"] = json!(true);
    }
    Ok(cmd)
}

fn parse_set(rest: &[&str], id: &str) -> Result<Value, ParseError> {
    const VALID: &[&str] = &[
        "viewport",
        "device",
        "geo",
        "geolocation",
        "offline",
        "headers",
        "credentials",
        "auth",
        "media",
    ];

    match rest.first().copied() {
        // `set viewport ...` is an alias for the top-level `viewport` command.
        Some("viewport") => parse_viewport(&rest[1..], id),
        Some("device") => {
            let dev = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "set device".to_string(),
                usage: "set device <name>",
            })?;
            Ok(json!({ "id": id, "action": "device", "device": dev }))
        }
        Some("geo") | Some("geolocation") => {
            let lat_str = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "set geo".to_string(),
                usage: "set geo <latitude> <longitude>",
            })?;
            let lng_str = rest.get(2).ok_or_else(|| ParseError::MissingArguments {
                context: "set geo".to_string(),
                usage: "set geo <latitude> <longitude>",
            })?;
            let lat = lat_str
                .parse::<f64>()
                .map_err(|_| ParseError::MissingArguments {
                    context: "set geo".to_string(),
                    usage: "set geo <latitude> <longitude>",
                })?;
            let lng = lng_str
                .parse::<f64>()
                .map_err(|_| ParseError::MissingArguments {
                    context: "set geo".to_string(),
                    usage: "set geo <latitude> <longitude>",
                })?;
            Ok(json!({ "id": id, "action": "geolocation", "latitude": lat, "longitude": lng }))
        }
        Some("offline") => {
            let off = rest
                .get(1)
                .map(|s| *s != "off" && *s != "false")
                .unwrap_or(true);
            Ok(json!({ "id": id, "action": "offline", "offline": off }))
        }
        Some("headers") => {
            let headers_json = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "set headers".to_string(),
                usage: "set headers <json>",
            })?;
            // Parse the JSON string into an object
            let headers: serde_json::Value =
                serde_json::from_str(headers_json).map_err(|_| ParseError::MissingArguments {
                    context: "set headers".to_string(),
                    usage: "set headers <json> (must be valid JSON object)",
                })?;
            Ok(json!({ "id": id, "action": "headers", "headers": headers }))
        }
        Some("credentials") | Some("auth") => {
            let user = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "set credentials".to_string(),
                usage: "set credentials <username> <password>",
            })?;
            let pass = rest.get(2).ok_or_else(|| ParseError::MissingArguments {
                context: "set credentials".to_string(),
                usage: "set credentials <username> <password>",
            })?;
            Ok(json!({ "id": id, "action": "credentials", "username": user, "password": pass }))
        }
        Some("media") => {
            let color = if rest.contains(&"dark") {
                "dark"
            } else if rest.contains(&"light") {
                "light"
            } else {
                "no-preference"
            };
            let reduced = if rest.contains(&"reduced-motion") {
                "reduce"
            } else {
                "no-preference"
            };
            Ok(
                json!({ "id": id, "action": "emulatemedia", "colorScheme": color, "reducedMotion": reduced }),
            )
        }
        Some(sub) => Err(ParseError::UnknownSubcommand {
            subcommand: sub.to_string(),
            valid_options: VALID,
        }),
        None => Err(ParseError::MissingArguments {
            context: "set".to_string(),
            usage: "set <viewport|device|geo|offline|headers|credentials|media> [args...]",
        }),
    }
}

/// Parse network interception, request inspection, and HAR recording commands.
fn parse_network(rest: &[&str], id: &str) -> Result<Value, ParseError> {
    const VALID: &[&str] = &["route", "unroute", "requests", "request", "har"];

    match rest.first().copied() {
        Some("route") => {
            let url = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "network route".to_string(),
                usage: "network route <url> [--abort] [--body <s> --status <n> --header K=V --content-type <ct>] [--method <M> --set-body <s> --set-header K=V --rewrite-url <u>] [--resource-type <csv>]",
            })?;
            // Single-value flag → its following token.
            let val = |name: &str| -> Option<&str> {
                rest.iter()
                    .position(|&s| s == name)
                    .and_then(|i| rest.get(i + 1).copied())
            };
            // Repeatable `--flag K=V` → object (split on the first '=').
            let kv_map = |name: &str| -> serde_json::Map<String, Value> {
                let mut m = serde_json::Map::new();
                for (i, &s) in rest.iter().enumerate() {
                    if s == name {
                        if let Some(&pair) = rest.get(i + 1) {
                            if let Some((k, v)) = pair.split_once('=') {
                                m.insert(k.to_string(), json!(v));
                            }
                        }
                    }
                }
                m
            };

            let abort = rest.contains(&"--abort");

            // Response side (Fetch.fulfillRequest — mock/replace the response).
            let body = val("--body");
            let status = val("--status").and_then(|s| s.parse::<u64>().ok());
            let content_type = val("--content-type");
            let resp_headers = kv_map("--header");
            let has_response =
                body.is_some() || status.is_some() || content_type.is_some() || !resp_headers.is_empty();

            // Request side (Fetch.continueRequest — rewrite the outgoing request).
            let set_body = val("--set-body");
            let method = val("--method");
            let rewrite_url = val("--rewrite-url");
            let req_headers = kv_map("--set-header");
            let has_request = set_body.is_some()
                || method.is_some()
                || rewrite_url.is_some()
                || !req_headers.is_empty();

            // Response-edit side (Fetch response stage — tweak the REAL response).
            let edit_status = val("--edit-status").and_then(|s| s.parse::<u64>().ok());
            let edit_headers = kv_map("--edit-header");
            // `--replace from=>to`, repeatable.
            let mut replacements: Vec<Value> = Vec::new();
            for (i, &s) in rest.iter().enumerate() {
                if s == "--replace" {
                    if let Some(&pair) = rest.get(i + 1) {
                        if let Some((from, to)) = pair.split_once("=>") {
                            replacements.push(json!([from, to]));
                        }
                    }
                }
            }
            let has_edit =
                edit_status.is_some() || !edit_headers.is_empty() || !replacements.is_empty();

            let rt_idx = rest
                .iter()
                .position(|&s| s == "--resource-type" || s == "--resource-types");
            let resource_type = rt_idx.and_then(|i| rest.get(i + 1).copied());

            let mut cmd = json!({ "id": id, "action": "route", "url": url, "abort": abort });
            if has_response {
                let mut resp = serde_json::Map::new();
                if let Some(b) = body {
                    resp.insert("body".into(), json!(b));
                }
                if let Some(st) = status {
                    resp.insert("status".into(), json!(st));
                }
                if let Some(ct) = content_type {
                    resp.insert("contentType".into(), json!(ct));
                }
                if !resp_headers.is_empty() {
                    resp.insert("headers".into(), Value::Object(resp_headers));
                }
                cmd["response"] = Value::Object(resp);
            }
            if has_request {
                let mut req = serde_json::Map::new();
                if let Some(b) = set_body {
                    req.insert("postData".into(), json!(b));
                }
                if let Some(m) = method {
                    req.insert("method".into(), json!(m));
                }
                if let Some(u) = rewrite_url {
                    req.insert("url".into(), json!(u));
                }
                if !req_headers.is_empty() {
                    req.insert("headers".into(), Value::Object(req_headers));
                }
                cmd["requestOverride"] = Value::Object(req);
            }
            if has_edit {
                let mut ed = serde_json::Map::new();
                if let Some(st) = edit_status {
                    ed.insert("status".into(), json!(st));
                }
                if !edit_headers.is_empty() {
                    ed.insert("headers".into(), Value::Object(edit_headers));
                }
                if !replacements.is_empty() {
                    ed.insert("replacements".into(), Value::Array(replacements));
                }
                cmd["responseEdit"] = Value::Object(ed);
            }
            if let Some(rt) = resource_type {
                cmd["resourceType"] = json!(rt);
            }
            Ok(cmd)
        }
        Some("unroute") => {
            let mut cmd = json!({ "id": id, "action": "unroute" });
            if let Some(url) = rest.get(1) {
                cmd["url"] = json!(url);
            }
            Ok(cmd)
        }
        Some("requests") => {
            let clear = rest.contains(&"--clear");
            let filter_idx = rest.iter().position(|&s| s == "--filter");
            let filter = filter_idx.and_then(|i| rest.get(i + 1).copied());
            let type_idx = rest.iter().position(|&s| s == "--type");
            let rtype = type_idx.and_then(|i| rest.get(i + 1).copied());
            let method_idx = rest.iter().position(|&s| s == "--method");
            let method = method_idx.and_then(|i| rest.get(i + 1).copied());
            let status_idx = rest.iter().position(|&s| s == "--status");
            let status = status_idx.and_then(|i| rest.get(i + 1).copied());
            let mut cmd = json!({ "id": id, "action": "requests", "clear": clear });
            if let Some(f) = filter {
                cmd["filter"] = json!(f);
            }
            if let Some(t) = rtype {
                cmd["type"] = json!(t);
            }
            if let Some(m) = method {
                cmd["method"] = json!(m);
            }
            if let Some(s) = status {
                cmd["status"] = json!(s);
            }
            Ok(cmd)
        }
        Some("request") => {
            let request_id = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "network request".to_string(),
                usage: "network request <requestId>",
            })?;
            Ok(json!({ "id": id, "action": "request_detail", "requestId": request_id }))
        }
        Some("har") => {
            const HAR_VALID: &[&str] = &["start", "stop"];
            match rest.get(1).copied() {
                Some("start") => Ok(json!({ "id": id, "action": "har_start" })),
                Some("stop") => {
                    let mut cmd = json!({ "id": id, "action": "har_stop" });
                    if let Some(path) = rest.get(2) {
                        cmd["path"] = json!(path);
                    }
                    Ok(cmd)
                }
                Some(sub) => Err(ParseError::UnknownSubcommand {
                    subcommand: sub.to_string(),
                    valid_options: HAR_VALID,
                }),
                None => Err(ParseError::MissingArguments {
                    context: "network har".to_string(),
                    usage: "network har <start|stop> [path]",
                }),
            }
        }
        Some(sub) => Err(ParseError::UnknownSubcommand {
            subcommand: sub.to_string(),
            valid_options: VALID,
        }),
        None => Err(ParseError::MissingArguments {
            context: "network".to_string(),
            usage: "network <route|unroute|requests|request|har> [args...]",
        }),
    }
}

fn parse_storage(rest: &[&str], id: &str) -> Result<Value, ParseError> {
    const VALID: &[&str] = &["local", "session"];

    match rest.first().copied() {
        Some("local") | Some("session") => {
            let storage_type = rest.first().unwrap();
            let (op, key, value) = match rest.get(1) {
                Some(&"get") => ("get", rest.get(2), rest.get(3)),
                Some(&"set") => ("set", rest.get(2), rest.get(3)),
                Some(&"clear") => ("clear", rest.get(2), rest.get(3)),
                Some(_) => ("get", rest.get(1), rest.get(2)),
                None => ("get", None, None),
            };
            match op {
                "set" => {
                    let k = key.ok_or_else(|| ParseError::MissingArguments {
                        context: format!("storage {} set", storage_type),
                        usage: "storage <local|session> set <key> <value>",
                    })?;
                    let v = value.ok_or_else(|| ParseError::MissingArguments {
                        context: format!("storage {} set", storage_type),
                        usage: "storage <local|session> set <key> <value>",
                    })?;
                    Ok(
                        json!({ "id": id, "action": "storage_set", "type": storage_type, "key": k, "value": v }),
                    )
                }
                "clear" => Ok(json!({ "id": id, "action": "storage_clear", "type": storage_type })),
                _ => {
                    let mut cmd =
                        json!({ "id": id, "action": "storage_get", "type": storage_type });
                    if let Some(k) = key {
                        cmd.as_object_mut()
                            .unwrap()
                            .insert("key".to_string(), json!(k));
                    }
                    Ok(cmd)
                }
            }
        }
        Some(sub) => Err(ParseError::UnknownSubcommand {
            subcommand: sub.to_string(),
            valid_options: VALID,
        }),
        None => Err(ParseError::MissingArguments {
            context: "storage".to_string(),
            usage: "storage <local|session> [get|set|clear] [key] [value]",
        }),
    }
}

/// Split a string into arguments respecting shell quoting (double/single quotes, backslash escapes).
pub fn shell_words_split(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_double = false;
    let mut in_single = false;
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\\' if !in_single => {
                if let Some(&next) = chars.peek() {
                    chars.next();
                    current.push(next);
                }
            }
            '"' if !in_single => in_double = !in_double,
            '\'' if !in_double => in_single = !in_single,
            ' ' if !in_double && !in_single => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_flags() -> Flags {
        Flags {
            session: "test".to_string(),
            json: false,
            headed: false,
            debug: false,
            headers: None,
            executable_path: None,
            extensions: Vec::new(),
            init_scripts: Vec::new(),
            enable: Vec::new(),
            cdp: None,
            browser: None,
            if_present: false,
            observe: false,
            profile: None,
            state: None,
            proxy: None,
            proxy_bypass: None,
            args: None,
            user_agent: None,
            provider: None,
            ignore_https_errors: false,
            allow_file_access: false,
            hide_scrollbars: true,
            device: None,
            auto_connect: false,
            force_launch: false,
            session_name: None,
            cli_executable_path: false,
            cli_extensions: false,
            cli_init_scripts: false,
            cli_enable: false,
            cli_profile: false,
            cli_state: false,
            cli_args: false,
            cli_user_agent: false,
            cli_proxy: false,
            cli_proxy_bypass: false,
            cli_allow_file_access: false,
            cli_hide_scrollbars: false,
            cli_annotate: false,
            cli_download_path: false,
            cli_headed: false,
            annotate: false,
            color_scheme: None,
            download_path: None,
            content_boundaries: false,
            max_output: None,
            allowed_domains: None,
            action_policy: None,
            confirm_actions: None,
            confirm_interactive: false,
            engine: None,
            screenshot_dir: None,
            screenshot_quality: None,
            screenshot_format: None,
            idle_timeout: None,
            default_timeout: None,
            no_auto_dialog: false,
            model: None,
            verbose: false,
            quiet: false,
        }
    }

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    // === Cookies Tests ===

    #[test]
    fn test_drag_offset_mode() {
        // Numeric / dx,dy second arg → offset drag (slider).
        let cmd = parse_command(&args("drag @e5 60"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "drag");
        assert_eq!(cmd["offset_dx"], 60.0);
        assert_eq!(cmd["offset_dy"], 0.0);
        assert!(cmd.get("target").is_none());

        let cmd = parse_command(&args("drag @e5 +60,-3"), &default_flags()).unwrap();
        assert_eq!(cmd["offset_dx"], 60.0);
        assert_eq!(cmd["offset_dy"], -3.0);

        // A real selector/ref target stays a ref-to-ref drag.
        let cmd = parse_command(&args("drag @e5 @e9"), &default_flags()).unwrap();
        assert_eq!(cmd["target"], "@e9");
        assert!(cmd.get("offset_dx").is_none());
    }

    #[test]
    fn test_parse_drag_offset_unit() {
        assert_eq!(parse_drag_offset("60"), Some((60.0, 0.0)));
        assert_eq!(parse_drag_offset("+60"), Some((60.0, 0.0)));
        assert_eq!(parse_drag_offset("-12"), Some((-12.0, 0.0)));
        assert_eq!(parse_drag_offset("60,-3"), Some((60.0, -3.0)));
        assert_eq!(parse_drag_offset(" 60 , 4 "), Some((60.0, 4.0)));
        // Not offsets — fall through to target path.
        assert_eq!(parse_drag_offset("@e5"), None);
        assert_eq!(parse_drag_offset(".foo"), None);
        assert_eq!(parse_drag_offset("+"), None);
        assert_eq!(parse_drag_offset(""), None);
        assert_eq!(parse_drag_offset("60px"), None);
    }

    #[test]
    fn test_cookies_get() {
        let cmd = parse_command(&args("cookies"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "cookies_get");
    }

    #[test]
    fn test_cookies_get_explicit() {
        let cmd = parse_command(&args("cookies get"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "cookies_get");
    }

    #[test]
    fn test_cookies_set() {
        let cmd = parse_command(&args("cookies set mycookie myvalue"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "cookies_set");
        assert_eq!(cmd["cookies"][0]["name"], "mycookie");
        assert_eq!(cmd["cookies"][0]["value"], "myvalue");
    }

    #[test]
    fn test_cookies_set_missing_value() {
        let result = parse_command(&args("cookies set mycookie"), &default_flags());
        assert!(result.is_err());
    }

    #[test]
    fn test_cookies_clear() {
        let cmd = parse_command(&args("cookies clear"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "cookies_clear");
    }

    #[test]
    fn test_parse_curl_cookies_json_preserves_attributes() {
        // A full cookie export (httpOnly session token, per-domain, secure,
        // sameSite, expiry) must round-trip — not get flattened to name/value.
        let input = r#"[
            {"name":"__Secure-next-auth.session-token","value":"eyJ.tok","domain":".chatgpt.com","path":"/","secure":true,"httpOnly":true,"sameSite":"Lax","expires":1893456000},
            {"name":"cf_clearance","value":"abc","domain":".openai.com","path":"/","secure":true,"http_only":true,"same_site":"no_restriction"}
        ]"#;
        let out = parse_curl_cookies(input).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["domain"], ".chatgpt.com");
        assert_eq!(out[0]["secure"], true);
        assert_eq!(out[0]["httpOnly"], true);
        assert_eq!(out[0]["sameSite"], "Lax");
        assert_eq!(out[0]["expires"], 1893456000.0);
        // alias keys (http_only, same_site=no_restriction) normalize to CDP shape
        assert_eq!(out[1]["domain"], ".openai.com");
        assert_eq!(out[1]["httpOnly"], true);
        assert_eq!(out[1]["sameSite"], "None");
    }

    #[test]
    fn test_parse_curl_cookies_json_array() {
        let input = r#"[{"name":"a","value":"1"},{"name":"b","value":"2"}]"#;
        let out = parse_curl_cookies(input).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["name"], "a");
        assert_eq!(out[0]["value"], "1");
        // bare {name,value} stays minimal — no spurious attribute keys
        assert!(out[0].get("domain").is_none());
        assert!(out[0].get("secure").is_none());
        assert_eq!(out[1]["name"], "b");
        assert_eq!(out[1]["value"], "2");
    }

    #[test]
    fn test_parse_curl_cookies_bare_header() {
        let input = "sid=abc; token=xyz; other=";
        let out = parse_curl_cookies(input).unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0]["name"], "sid");
        assert_eq!(out[0]["value"], "abc");
        assert_eq!(out[2]["name"], "other");
        assert_eq!(out[2]["value"], "");
    }

    #[test]
    fn test_parse_curl_cookies_from_curl_bash() {
        let input = "curl 'https://example.com/api' \\\n  -H 'accept: application/json' \\\n  -H 'cookie: sid=abc; token=xyz'";
        let out = parse_curl_cookies(input).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["name"], "sid");
        assert_eq!(out[1]["name"], "token");
    }

    #[test]
    fn test_parse_curl_cookies_from_curl_b_flag() {
        let input = "curl 'https://example.com/api' -b 'sid=abc; token=xyz'";
        let out = parse_curl_cookies(input).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["name"], "sid");
    }

    #[test]
    fn test_parse_curl_cookies_empty_error() {
        assert!(parse_curl_cookies("").is_err());
        assert!(parse_curl_cookies("   ").is_err());
    }

    #[test]
    fn test_parse_curl_cookies_never_echoes_values_in_errors() {
        // Error messages must never echo cookie values or any bytes from the
        // input that could be a secret. Use a distinct recognizable token per
        // case so a regression surfaces the leaking code path clearly.

        // JSON array with a missing `name` field.
        let name_secret = "LEAK_VIA_MISSING_NAME_xY9zQ";
        let input = format!(r#"[{{"value":"{}"}}]"#, name_secret);
        let err = parse_curl_cookies(&input).unwrap_err();
        assert!(
            !err.contains(name_secret),
            "missing-name error leaked secret value: {}",
            err
        );

        // Truncated / malformed JSON containing a secret. Depends on
        // serde_json's Display impl to report position only, not payload.
        let parse_secret = "LEAK_VIA_JSON_PARSE_aA1bB2";
        let input = format!(r#"[{{"name":"sid","value":"{}"#, parse_secret);
        let err = parse_curl_cookies(&input).unwrap_err();
        assert!(
            !err.contains(parse_secret),
            "malformed-JSON error leaked secret value: {}",
            err
        );

        // Valid cURL dump with no Cookie header but a secret in another
        // header. Should error on "no Cookie header found" without echoing.
        let curl_secret = "LEAK_VIA_CURL_NO_COOKIE_pP7qQ";
        let input = format!("curl 'https://example.com/' -H 'x-token: {}'", curl_secret);
        let err = parse_curl_cookies(&input).unwrap_err();
        assert!(
            !err.contains(curl_secret),
            "cURL-without-cookie error leaked secret value: {}",
            err
        );
    }

    #[test]
    fn test_react_tree_command() {
        let cmd = parse_command(&args("react tree"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "react_tree");
    }

    #[test]
    fn test_react_tree_json() {
        let cmd = parse_command(&args("react tree --json"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "react_tree");
        assert_eq!(cmd["json"], true);
    }

    #[test]
    fn test_react_inspect_command() {
        let cmd = parse_command(&args("react inspect 12345"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "react_inspect");
        assert_eq!(cmd["fiberId"], 12345);
    }

    #[test]
    fn test_react_inspect_requires_numeric_id() {
        let err = parse_command(&args("react inspect not-a-number"), &default_flags());
        assert!(err.is_err());
    }

    #[test]
    fn test_react_renders_start_stop() {
        let start = parse_command(&args("react renders start"), &default_flags()).unwrap();
        assert_eq!(start["action"], "react_renders_start");
        let stop = parse_command(&args("react renders stop"), &default_flags()).unwrap();
        assert_eq!(stop["action"], "react_renders_stop");
        let stop_json =
            parse_command(&args("react renders stop --json"), &default_flags()).unwrap();
        assert_eq!(stop_json["json"], true);
    }

    #[test]
    fn test_react_suspense() {
        let cmd = parse_command(&args("react suspense"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "react_suspense");
        // onlyDynamic defaults to unset; absence means the full report
        assert!(cmd.get("onlyDynamic").is_none());
    }

    #[test]
    fn test_react_suspense_only_dynamic() {
        let cmd = parse_command(&args("react suspense --only-dynamic"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "react_suspense");
        assert_eq!(cmd["onlyDynamic"], true);
    }

    #[test]
    fn test_react_suspense_only_dynamic_with_json() {
        let cmd = parse_command(
            &args("react suspense --only-dynamic --json"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "react_suspense");
        assert_eq!(cmd["onlyDynamic"], true);
        assert_eq!(cmd["json"], true);
    }

    #[test]
    fn test_vitals_command() {
        let cmd = parse_command(&args("vitals"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "vitals");
        let cmd = parse_command(
            &args("vitals http://localhost:3000/dashboard"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["url"], "http://localhost:3000/dashboard");
    }

    #[test]
    fn test_pushstate_command() {
        let cmd = parse_command(&args("pushstate /foo"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "pushstate");
        assert_eq!(cmd["url"], "/foo");
    }

    #[test]
    fn test_removeinitscript_command() {
        let cmd = parse_command(&args("removeinitscript abc123"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "removeinitscript");
        assert_eq!(cmd["identifier"], "abc123");
    }

    #[test]
    fn test_network_route_resource_type() {
        let cmd = parse_command(
            &args("network route * --abort --resource-type script"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "route");
        assert_eq!(cmd["resourceType"], "script");
        assert_eq!(cmd["abort"], true);
    }

    #[test]
    fn test_network_route_fulfill_response() {
        let cmd = parse_command(
            &args("network route **/api/* --body {\"ok\":1} --status 201 --header X-Test=yes"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "route");
        assert_eq!(cmd["response"]["body"], "{\"ok\":1}");
        assert_eq!(cmd["response"]["status"], 201);
        assert_eq!(cmd["response"]["headers"]["X-Test"], "yes");
        // No request-side override was given.
        assert!(cmd.get("requestOverride").is_none());
    }

    #[test]
    fn test_network_route_request_override() {
        let cmd = parse_command(
            &args("network route **/api/* --method POST --set-body hello --set-header A=B --rewrite-url https://x.test/y"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "route");
        assert_eq!(cmd["requestOverride"]["method"], "POST");
        assert_eq!(cmd["requestOverride"]["postData"], "hello");
        assert_eq!(cmd["requestOverride"]["headers"]["A"], "B");
        assert_eq!(cmd["requestOverride"]["url"], "https://x.test/y");
        // No response-side mock was given.
        assert!(cmd.get("response").is_none());
    }

    #[test]
    fn test_network_route_response_edit() {
        let cmd = parse_command(
            &args("network route **/api/* --edit-status 503 --edit-header X-Edited=1 --replace foo=>bar"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "route");
        assert_eq!(cmd["responseEdit"]["status"], 503);
        assert_eq!(cmd["responseEdit"]["headers"]["X-Edited"], "1");
        assert_eq!(cmd["responseEdit"]["replacements"][0][0], "foo");
        assert_eq!(cmd["responseEdit"]["replacements"][0][1], "bar");
        // Response-edit is distinct from full mock and request override.
        assert!(cmd.get("response").is_none());
        assert!(cmd.get("requestOverride").is_none());
    }

    #[test]
    fn test_cookies_set_with_url() {
        let cmd = parse_command(
            &args("cookies set mycookie myvalue --url https://example.com"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "cookies_set");
        assert_eq!(cmd["cookies"][0]["name"], "mycookie");
        assert_eq!(cmd["cookies"][0]["value"], "myvalue");
        assert_eq!(cmd["cookies"][0]["url"], "https://example.com");
    }

    #[test]
    fn test_cookies_set_with_domain() {
        let cmd = parse_command(
            &args("cookies set mycookie myvalue --domain example.com"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "cookies_set");
        assert_eq!(cmd["cookies"][0]["name"], "mycookie");
        assert_eq!(cmd["cookies"][0]["value"], "myvalue");
        assert_eq!(cmd["cookies"][0]["domain"], "example.com");
    }

    #[test]
    fn test_cookies_set_with_path() {
        let cmd = parse_command(
            &args("cookies set mycookie myvalue --path /api"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "cookies_set");
        assert_eq!(cmd["cookies"][0]["name"], "mycookie");
        assert_eq!(cmd["cookies"][0]["value"], "myvalue");
        assert_eq!(cmd["cookies"][0]["path"], "/api");
    }

    #[test]
    fn test_cookies_set_with_httponly() {
        let cmd = parse_command(
            &args("cookies set mycookie myvalue --httpOnly"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "cookies_set");
        assert_eq!(cmd["cookies"][0]["name"], "mycookie");
        assert_eq!(cmd["cookies"][0]["value"], "myvalue");
        assert_eq!(cmd["cookies"][0]["httpOnly"], true);
    }

    #[test]
    fn test_cookies_set_with_secure() {
        let cmd = parse_command(
            &args("cookies set mycookie myvalue --secure"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "cookies_set");
        assert_eq!(cmd["cookies"][0]["name"], "mycookie");
        assert_eq!(cmd["cookies"][0]["value"], "myvalue");
        assert_eq!(cmd["cookies"][0]["secure"], true);
    }

    #[test]
    fn test_cookies_set_with_samesite() {
        let cmd = parse_command(
            &args("cookies set mycookie myvalue --sameSite Strict"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "cookies_set");
        assert_eq!(cmd["cookies"][0]["name"], "mycookie");
        assert_eq!(cmd["cookies"][0]["value"], "myvalue");
        assert_eq!(cmd["cookies"][0]["sameSite"], "Strict");
    }

    #[test]
    fn test_cookies_set_with_expires() {
        let cmd = parse_command(
            &args("cookies set mycookie myvalue --expires 1234567890"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "cookies_set");
        assert_eq!(cmd["cookies"][0]["name"], "mycookie");
        assert_eq!(cmd["cookies"][0]["value"], "myvalue");
        assert_eq!(cmd["cookies"][0]["expires"], 1234567890);
    }

    #[test]
    fn test_cookies_set_with_multiple_flags() {
        let cmd = parse_command(&args("cookies set mycookie myvalue --url https://example.com --httpOnly --secure --sameSite Lax"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "cookies_set");
        assert_eq!(cmd["cookies"][0]["name"], "mycookie");
        assert_eq!(cmd["cookies"][0]["value"], "myvalue");
        assert_eq!(cmd["cookies"][0]["url"], "https://example.com");
        assert_eq!(cmd["cookies"][0]["httpOnly"], true);
        assert_eq!(cmd["cookies"][0]["secure"], true);
        assert_eq!(cmd["cookies"][0]["sameSite"], "Lax");
    }

    #[test]
    fn test_cookies_set_with_all_flags() {
        let cmd = parse_command(&args("cookies set mycookie myvalue --url https://example.com --domain example.com --path /api --httpOnly --secure --sameSite None --expires 9999999999"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "cookies_set");
        assert_eq!(cmd["cookies"][0]["name"], "mycookie");
        assert_eq!(cmd["cookies"][0]["value"], "myvalue");
        assert_eq!(cmd["cookies"][0]["url"], "https://example.com");
        assert_eq!(cmd["cookies"][0]["domain"], "example.com");
        assert_eq!(cmd["cookies"][0]["path"], "/api");
        assert_eq!(cmd["cookies"][0]["httpOnly"], true);
        assert_eq!(cmd["cookies"][0]["secure"], true);
        assert_eq!(cmd["cookies"][0]["sameSite"], "None");
        assert_eq!(cmd["cookies"][0]["expires"], 9999999999i64);
    }

    #[test]
    fn test_cookies_set_invalid_samesite() {
        let result = parse_command(
            &args("cookies set mycookie myvalue --sameSite Invalid"),
            &default_flags(),
        );
        assert!(result.is_err());
    }

    // === Storage Tests ===

    #[test]
    fn test_storage_local_get() {
        let cmd = parse_command(&args("storage local"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "storage_get");
        assert_eq!(cmd["type"], "local");
        assert!(cmd.get("key").is_none());
    }

    #[test]
    fn test_storage_local_get_key() {
        let cmd = parse_command(&args("storage local get mykey"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "storage_get");
        assert_eq!(cmd["type"], "local");
        assert_eq!(cmd["key"], "mykey");
    }

    #[test]
    fn test_storage_local_get_implicit_key() {
        let cmd = parse_command(&args("storage local mykey"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "storage_get");
        assert_eq!(cmd["type"], "local");
        assert_eq!(cmd["key"], "mykey");
    }

    #[test]
    fn test_storage_session_get() {
        let cmd = parse_command(&args("storage session"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "storage_get");
        assert_eq!(cmd["type"], "session");
    }

    #[test]
    fn test_storage_session_get_implicit_key() {
        let cmd = parse_command(&args("storage session mykey"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "storage_get");
        assert_eq!(cmd["type"], "session");
        assert_eq!(cmd["key"], "mykey");
    }

    #[test]
    fn test_storage_local_set() {
        let cmd =
            parse_command(&args("storage local set mykey myvalue"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "storage_set");
        assert_eq!(cmd["type"], "local");
        assert_eq!(cmd["key"], "mykey");
        assert_eq!(cmd["value"], "myvalue");
    }

    #[test]
    fn test_storage_session_set() {
        let cmd =
            parse_command(&args("storage session set skey svalue"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "storage_set");
        assert_eq!(cmd["type"], "session");
        assert_eq!(cmd["key"], "skey");
        assert_eq!(cmd["value"], "svalue");
    }

    #[test]
    fn test_storage_set_missing_value() {
        let result = parse_command(&args("storage local set mykey"), &default_flags());
        assert!(result.is_err());
    }

    #[test]
    fn test_storage_local_clear() {
        let cmd = parse_command(&args("storage local clear"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "storage_clear");
        assert_eq!(cmd["type"], "local");
    }

    #[test]
    fn test_storage_session_clear() {
        let cmd = parse_command(&args("storage session clear"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "storage_clear");
        assert_eq!(cmd["type"], "session");
    }

    #[test]
    fn test_storage_invalid_type() {
        let result = parse_command(&args("storage invalid"), &default_flags());
        assert!(result.is_err());
    }

    // === Navigation Tests ===

    #[test]
    fn test_open_without_url_launches() {
        let cmd = parse_command(&args("open"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "launch");
        assert_eq!(cmd["headless"], true);
    }

    #[test]
    fn test_open_without_url_headed() {
        let mut flags = default_flags();
        flags.headed = true;
        let cmd = parse_command(&args("open"), &flags).unwrap();
        assert_eq!(cmd["action"], "launch");
        assert_eq!(cmd["headless"], false);
    }

    #[test]
    fn test_goto_still_requires_url() {
        assert!(parse_command(&args("goto"), &default_flags()).is_err());
        assert!(parse_command(&args("navigate"), &default_flags()).is_err());
    }

    #[test]
    fn test_navigate_with_https() {
        let cmd = parse_command(&args("open https://example.com"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "navigate");
        assert_eq!(cmd["url"], "https://example.com");
    }

    #[test]
    fn test_navigate_without_protocol() {
        let cmd = parse_command(&args("open example.com"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "navigate");
        assert_eq!(cmd["url"], "https://example.com");
    }

    #[test]
    fn test_press_plain_and_hold() {
        let cmd = parse_command(&args("press d"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "press");
        assert_eq!(cmd["key"], "d");
        assert!(cmd.get("hold").is_none());

        let held = parse_command(&args("press d --hold 800"), &default_flags()).unwrap();
        assert_eq!(held["key"], "d");
        assert_eq!(held["hold"], 800);

        // Missing/invalid duration is an error, not a silent no-hold.
        assert!(parse_command(&args("press d --hold"), &default_flags()).is_err());
        assert!(parse_command(&args("press d --hold abc"), &default_flags()).is_err());
    }

    #[test]
    fn test_navigate_reuse_tab_flag() {
        let cmd = parse_command(
            &args("open https://example.com --reuse-tab"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "navigate");
        assert_eq!(cmd["reuseTab"], true);
        // Alias.
        let cmd2 =
            parse_command(&args("open https://example.com --reuse"), &default_flags()).unwrap();
        assert_eq!(cmd2["reuseTab"], true);
        // Absent by default.
        let cmd3 = parse_command(&args("open https://example.com"), &default_flags()).unwrap();
        assert!(cmd3.get("reuseTab").is_none());
    }

    #[test]
    fn test_navigate_with_headers() {
        let mut flags = default_flags();
        flags.headers = Some(r#"{"Authorization": "Bearer token"}"#.to_string());
        let cmd = parse_command(&args("open api.example.com"), &flags).unwrap();
        assert_eq!(cmd["action"], "navigate");
        assert_eq!(cmd["url"], "https://api.example.com");
        assert_eq!(cmd["headers"]["Authorization"], "Bearer token");
    }

    #[test]
    fn test_navigate_with_multiple_headers() {
        let mut flags = default_flags();
        flags.headers =
            Some(r#"{"Authorization": "Bearer token", "X-Custom": "value"}"#.to_string());
        let cmd = parse_command(&args("open api.example.com"), &flags).unwrap();
        assert_eq!(cmd["headers"]["Authorization"], "Bearer token");
        assert_eq!(cmd["headers"]["X-Custom"], "value");
    }

    #[test]
    fn test_navigate_without_headers_flag() {
        let cmd = parse_command(&args("open example.com"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "navigate");
        // headers should not be present when flag is not set
        assert!(cmd.get("headers").is_none());
    }

    #[test]
    fn test_navigate_with_invalid_headers_json() {
        let mut flags = default_flags();
        flags.headers = Some("not valid json".to_string());
        let result = parse_command(&args("open api.example.com"), &flags);
        // Invalid JSON should return a ParseError, not silently drop headers
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = err.format();
        assert!(msg.contains("Invalid JSON for --headers"));
    }

    #[test]
    fn test_navigate_chrome_extension_url() {
        let cmd = parse_command(
            &args("open chrome-extension://abcdefghijklmnop/popup.html"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "navigate");
        assert_eq!(cmd["url"], "chrome-extension://abcdefghijklmnop/popup.html");
    }

    #[test]
    fn test_navigate_chrome_url() {
        let cmd = parse_command(&args("open chrome://extensions"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "navigate");
        assert_eq!(cmd["url"], "chrome://extensions");
    }

    // === Set Headers Tests ===

    #[test]
    fn test_set_headers_parses_json() {
        let input: Vec<String> = vec![
            "set".to_string(),
            "headers".to_string(),
            r#"{"Authorization":"Bearer token"}"#.to_string(),
        ];
        let cmd = parse_command(&input, &default_flags()).unwrap();
        assert_eq!(cmd["action"], "headers");
        // Headers should be an object, not a string
        assert!(cmd["headers"].is_object());
        assert_eq!(cmd["headers"]["Authorization"], "Bearer token");
    }

    #[test]
    fn test_set_headers_with_multiple_values() {
        let input: Vec<String> = vec![
            "set".to_string(),
            "headers".to_string(),
            r#"{"Authorization": "Bearer token", "X-Custom": "value"}"#.to_string(),
        ];
        let cmd = parse_command(&input, &default_flags()).unwrap();
        assert_eq!(cmd["headers"]["Authorization"], "Bearer token");
        assert_eq!(cmd["headers"]["X-Custom"], "value");
    }

    #[test]
    fn test_set_headers_invalid_json_error() {
        let input: Vec<String> = vec![
            "set".to_string(),
            "headers".to_string(),
            "not-valid-json".to_string(),
        ];
        let result = parse_command(&input, &default_flags());
        assert!(result.is_err());
    }

    #[test]
    fn test_back() {
        let cmd = parse_command(&args("back"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "back");
    }

    #[test]
    fn test_forward() {
        let cmd = parse_command(&args("forward"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "forward");
    }

    #[test]
    fn test_reload() {
        let cmd = parse_command(&args("reload"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "reload");
    }

    // === issue #8.4: CLI ergonomics ===

    #[test]
    fn test_click_coords_two_args() {
        let cmd = parse_command(&args("click 449 320"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "click");
        assert_eq!(cmd["x"], 449.0);
        assert_eq!(cmd["y"], 320.0);
        assert!(cmd.get("selector").is_none());
    }

    #[test]
    fn test_click_coords_comma() {
        let cmd = parse_command(&args("click 449,320"), &default_flags()).unwrap();
        assert_eq!(cmd["x"], 449.0);
        assert_eq!(cmd["y"], 320.0);
    }

    #[test]
    fn test_click_coords_flag() {
        let cmd = parse_command(&args("click --coords 449,320"), &default_flags()).unwrap();
        assert_eq!(cmd["x"], 449.0);
        assert_eq!(cmd["y"], 320.0);
    }

    #[test]
    fn test_click_selector_not_coords() {
        let cmd = parse_command(&args("click button.submit"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "click");
        assert_eq!(cmd["selector"], "button.submit");
        assert!(cmd.get("x").is_none());
    }

    #[test]
    fn test_click_follow_flag() {
        // `--follow` sets the flag; the selector is still found even with the flag
        // before it (issue #24-A).
        let cmd = parse_command(&args("click @e5 --follow"), &default_flags()).unwrap();
        assert_eq!(cmd["selector"], "@e5");
        assert_eq!(cmd["follow"], true);
        let cmd2 = parse_command(&args("click --follow @e5"), &default_flags()).unwrap();
        assert_eq!(cmd2["selector"], "@e5");
        assert_eq!(cmd2["follow"], true);
        // Absent by default.
        let plain = parse_command(&args("click @e5"), &default_flags()).unwrap();
        assert!(plain.get("follow").is_none());
    }

    #[test]
    fn test_tabs_alias_lists() {
        assert_eq!(
            parse_command(&args("tabs"), &default_flags()).unwrap()["action"],
            "tab_list"
        );
        assert_eq!(
            parse_command(&args("tabs list"), &default_flags()).unwrap()["action"],
            "tab_list"
        );
        assert_eq!(
            parse_command(&args("tabs new"), &default_flags()).unwrap()["action"],
            "tab_new"
        );
    }

    #[test]
    fn test_tab_list_full_flag() {
        // issue #19: `--full` → untruncated URLs; works as `tab list --full`,
        // `tab --full`, and `tabs --full`. Plain list has no `full`.
        for inv in ["tab list --full", "tab --full", "tabs --full"] {
            let cmd = parse_command(&args(inv), &default_flags()).unwrap();
            assert_eq!(cmd["action"], "tab_list", "{inv}");
            assert_eq!(cmd["full"], true, "{inv}");
        }
        let plain = parse_command(&args("tab list"), &default_flags()).unwrap();
        assert_eq!(plain["action"], "tab_list");
        assert!(plain.get("full").is_none());
    }

    #[test]
    fn test_bring_to_front_aliases() {
        // issue #19: the documented `bringToFront` (+ kebab/lowercase) maps to
        // the existing daemon action.
        for inv in ["bringToFront", "bring-to-front", "bringtofront"] {
            let cmd = parse_command(&args(inv), &default_flags()).unwrap();
            assert_eq!(cmd["action"], "bringtofront", "{inv}");
        }
    }

    #[test]
    fn test_get_text_hyphen_and_underscore_aliases() {
        for verb in ["get-text", "get_text"] {
            let cmd = parse_command(&args(&format!("{verb} .price")), &default_flags()).unwrap();
            assert_eq!(cmd["action"], "gettext", "{verb}");
            assert_eq!(cmd["selector"], ".price", "{verb}");
        }
    }

    #[test]
    fn test_open_wait_until_after_url() {
        let cmd = parse_command(
            &args("open https://x.com --wait-until domcontentloaded"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "navigate");
        assert_eq!(cmd["url"], "https://x.com");
        assert_eq!(cmd["waitUntil"], "domcontentloaded");
    }

    #[test]
    fn test_open_wait_until_before_url_not_mistaken_for_url() {
        // The --wait-until value must not be picked up as the URL.
        let cmd = parse_command(
            &args("open --wait-until domcontentloaded https://x.com"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["url"], "https://x.com");
        assert_eq!(cmd["waitUntil"], "domcontentloaded");
    }

    #[test]
    fn test_open_wait_until_rejects_bogus_value() {
        let err = parse_command(
            &args("open https://x.com --wait-until wat"),
            &default_flags(),
        )
        .unwrap_err();
        assert!(
            err.format().contains("--wait-until"),
            "got: {}",
            err.format()
        );
    }

    #[test]
    fn test_find_bare_value_suggests_text_locator() {
        // `find "I'm not a robot" click` — value where a locator keyword was
        // expected. Error must steer to the corrected `find text ...` form.
        let input: Vec<String> = vec![
            "find".to_string(),
            "I'm not a robot".to_string(),
            "click".to_string(),
        ];
        let err = parse_command(&input, &default_flags()).unwrap_err();
        let msg = err.format();
        assert!(msg.contains("find text"), "got: {msg}");
        assert!(msg.contains("I'm not a robot"), "got: {msg}");
    }

    // === Core Actions ===

    #[test]
    fn test_click() {
        let cmd = parse_command(&args("click #button"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "click");
        assert_eq!(cmd["selector"], "#button");
    }

    #[test]
    fn test_fill() {
        let cmd = parse_command(&args("fill #input hello world"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "fill");
        assert_eq!(cmd["selector"], "#input");
        assert_eq!(cmd["value"], "hello world");
    }

    #[test]
    fn test_read_command() {
        let cmd = parse_command(&args("read example.com/docs"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "read");
        assert_eq!(cmd["url"], "example.com/docs");
        assert_eq!(cmd["timeout"], crate::read::default_timeout_ms());
    }

    #[test]
    fn test_read_without_url_uses_active_tab() {
        let cmd = parse_command(&args("read"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "read");
        assert!(cmd.get("url").is_none());
    }

    #[test]
    fn test_read_flags() {
        let cmd = parse_command(
            &args("read https://example.com --raw --require-md --timeout 2500"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "read");
        assert_eq!(cmd["url"], "https://example.com");
        assert_eq!(cmd["raw"], true);
        assert_eq!(cmd["requireMd"], true);
        assert_eq!(cmd["timeout"], 2500);
    }

    #[test]
    fn test_read_includes_global_headers_and_allowed_domains() {
        let mut flags = default_flags();
        flags.headers = Some(r#"{"Authorization":"Bearer token","X-Trace":"abc"}"#.to_string());
        flags.allowed_domains = Some(vec!["example.com".to_string(), "*.example.org".to_string()]);

        let cmd = parse_command(&args("read https://example.com/docs"), &flags).unwrap();

        assert_eq!(cmd["action"], "read");
        assert_eq!(cmd["headers"]["Authorization"], "Bearer token");
        assert_eq!(cmd["headers"]["X-Trace"], "abc");
        assert_eq!(cmd["allowedDomains"], json!(["example.com", "*.example.org"]));
    }

    #[test]
    fn test_read_rejects_invalid_headers_json() {
        let mut flags = default_flags();
        flags.headers = Some("not json".to_string());
        let result = parse_command(&args("read https://example.com/docs"), &flags);
        assert!(matches!(result, Err(ParseError::InvalidValue { .. })));
    }

    #[test]
    fn test_read_llms_index_filter_flags() {
        let cmd = parse_command(
            &args("read https://example.com/docs --llms index --filter auth"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "read");
        assert_eq!(cmd["url"], "https://example.com/docs");
        assert_eq!(cmd["llms"], "index");
        assert_eq!(cmd["filter"], "auth");
    }

    #[test]
    fn test_read_llms_and_outline_conflict() {
        let result = parse_command(
            &args("read https://example.com --llms full --outline"),
            &default_flags(),
        );
        assert!(matches!(result, Err(ParseError::InvalidValue { .. })));
    }

    #[test]
    fn test_type_command() {
        let cmd = parse_command(&args("type #input some text"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "type");
        assert_eq!(cmd["selector"], "#input");
        assert_eq!(cmd["text"], "some text");
        assert_eq!(cmd["keyEvents"], false);
        assert_eq!(cmd["commitEnter"], false);
    }

    #[test]
    fn test_type_clear_and_delay() {
        // --clear and --delay must be parsed as flags, not typed as literal text.
        let cmd = parse_command(
            &args("type #input hello world --clear --delay 40"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "type");
        assert_eq!(cmd["selector"], "#input");
        assert_eq!(cmd["text"], "hello world");
        assert_eq!(cmd["clear"], true);
        assert_eq!(cmd["delay"], 40);
    }

    #[test]
    fn test_type_delay_requires_number() {
        let err = parse_command(&args("type #input hi --delay abc"), &default_flags());
        assert!(err.is_err(), "non-numeric --delay must be rejected");
    }

    #[test]
    fn test_type_key_events() {
        // --key-events sends real keystrokes (for autocomplete/combobox) and must
        // not be swallowed into the typed text.
        let cmd = parse_command(
            &args("type #postal 201-0001 --key-events"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "type");
        assert_eq!(cmd["selector"], "#postal");
        assert_eq!(cmd["text"], "201-0001");
        assert_eq!(cmd["keyEvents"], true);

        let focused =
            parse_command(&args("type --focused 201-0001 --keys"), &default_flags()).unwrap();
        assert_eq!(focused["focused"], true);
        assert_eq!(focused["text"], "201-0001");
        assert_eq!(focused["keyEvents"], true);
    }

    #[test]
    fn test_type_commit_enter() {
        // --enter commits the typed value with a trailing Enter (async-autocomplete
        // tag widgets, issue #50) and must imply --key-events so the dropdown the
        // Enter commits has actually been triggered.
        let cmd = parse_command(&args("type #tag ChatGPT --enter"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "type");
        assert_eq!(cmd["selector"], "#tag");
        assert_eq!(cmd["text"], "ChatGPT");
        assert_eq!(cmd["commitEnter"], true);
        assert_eq!(cmd["keyEvents"], true);

        // Alias --commit-enter behaves identically, on the --focused path too.
        let focused = parse_command(
            &args("type --focused ChatGPT --commit-enter"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(focused["focused"], true);
        assert_eq!(focused["text"], "ChatGPT");
        assert_eq!(focused["commitEnter"], true);
        assert_eq!(focused["keyEvents"], true);
    }

    #[test]
    fn test_select() {
        let cmd = parse_command(&args("select #menu option1"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "select");
        assert_eq!(cmd["selector"], "#menu");
        assert_eq!(cmd["values"], "option1");
    }

    #[test]
    fn test_select_multiple_values() {
        let cmd = parse_command(&args("select #menu opt1 opt2 opt3"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "select");
        assert_eq!(cmd["selector"], "#menu");
        assert_eq!(cmd["values"], json!(["opt1", "opt2", "opt3"]));
    }

    #[test]
    fn test_frame_main() {
        let cmd = parse_command(&args("frame main"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "mainframe");
    }

    // === Tabs ===

    #[test]
    fn test_tab_new() {
        let cmd = parse_command(&args("tab new"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "tab_new");
        assert!(
            cmd.get("url").is_none(),
            "url should not be present when not provided"
        );
    }

    #[test]
    fn test_tab_new_with_url() {
        let cmd = parse_command(&args("tab new https://example.com"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "tab_new");
        assert_eq!(cmd["url"], "https://example.com");
    }

    #[test]
    fn test_tab_list() {
        let cmd = parse_command(&args("tab list"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "tab_list");
    }

    #[test]
    fn test_tab_switch_by_id() {
        let cmd = parse_command(&args("tab t2"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "tab_switch");
        assert_eq!(cmd["tabId"], "t2");
    }

    #[test]
    fn test_tab_switch_by_label() {
        let cmd = parse_command(&args("tab docs"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "tab_switch");
        assert_eq!(cmd["tabId"], "docs");
    }

    #[test]
    fn test_tab_close() {
        let cmd = parse_command(&args("tab close"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "tab_close");
    }

    #[test]
    fn test_tab_close_with_id() {
        let cmd = parse_command(&args("tab close t2"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "tab_close");
        assert_eq!(cmd["tabId"], "t2");
    }

    #[test]
    fn test_tab_close_with_label() {
        let cmd = parse_command(&args("tab close docs"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "tab_close");
        assert_eq!(cmd["tabId"], "docs");
    }

    #[test]
    fn test_close_tab_vs_browser() {
        // `close <tab>` closes that tab (says "Tab closed"); bare `close` closes
        // the browser (#26).
        let tab = parse_command(&args("close t12"), &default_flags()).unwrap();
        assert_eq!(tab["action"], "tab_close");
        assert_eq!(tab["tabId"], "t12");
        let browser = parse_command(&args("close"), &default_flags()).unwrap();
        assert_eq!(browser["action"], "close");
        // `quit`/`exit` aliases still browser-close.
        assert_eq!(
            parse_command(&args("quit"), &default_flags()).unwrap()["action"],
            "close"
        );
    }

    #[test]
    fn test_current_command() {
        let cmd = parse_command(&args("current"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "current");
    }

    #[test]
    fn test_tab_sends_string_tab_id() {
        let cmd = parse_command(&args("tab t2"), &default_flags()).unwrap();
        assert!(
            cmd["tabId"].is_string(),
            "tabId must be a string, got: {:?}",
            cmd["tabId"]
        );
        assert!(cmd.get("index").is_none());
    }

    #[test]
    fn test_tab_new_with_label() {
        let cmd = parse_command(&args("tab new --label docs"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "tab_new");
        assert_eq!(cmd["label"], "docs");
    }

    #[test]
    fn test_tab_new_with_label_and_url() {
        let cmd = parse_command(
            &args("tab new --label docs https://docs.example.com"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "tab_new");
        assert_eq!(cmd["label"], "docs");
        assert_eq!(cmd["url"], "https://docs.example.com");
    }

    #[test]
    fn test_tab_new_with_url_then_label() {
        let cmd = parse_command(
            &args("tab new https://docs.example.com --label docs"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["url"], "https://docs.example.com");
        assert_eq!(cmd["label"], "docs");
    }

    #[test]
    fn test_tab_no_args_defaults_to_list() {
        let cmd = parse_command(&args("tab"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "tab_list");
    }

    #[test]
    fn test_tab_unknown_flag_errors() {
        // Unknown flags on `tab new` must error instead of being silently
        // dropped. This protects against typos like `--labl` or `--new-tab`.
        let result = parse_command(
            &args("tab new --unknown-flag https://example.com"),
            &default_flags(),
        );
        assert!(
            result.is_err(),
            "tab new with an unknown flag must error, got: {:?}",
            result
        );
    }

    #[test]
    fn test_tab_non_keyword_treated_as_ref() {
        // After the shift to `t<N>`/label ids, non-keyword tokens (`select`,
        // `docs`, etc.) are valid label refs; `tab <something>` routes to
        // tab_switch and the runtime decides whether the label exists.
        let cmd = parse_command(&args("tab select"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "tab_switch");
        assert_eq!(cmd["tabId"], "select");
    }

    // === Network ===

    #[test]
    fn test_network_har_start() {
        let cmd = parse_command(&args("network har start"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "har_start");
    }

    #[test]
    fn test_network_har_stop_with_path() {
        let cmd = parse_command(&args("network har stop ./capture.har"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "har_stop");
        assert_eq!(cmd["path"], "./capture.har");
    }

    #[test]
    fn test_network_har_stop_without_path() {
        let cmd = parse_command(&args("network har stop"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "har_stop");
        assert!(cmd.get("path").is_none());
    }

    #[test]
    fn test_network_har_requires_subcommand() {
        let result = parse_command(&args("network har"), &default_flags());
        assert!(matches!(result, Err(ParseError::MissingArguments { .. })));
    }

    #[test]
    fn test_network_requests_type_filter() {
        let cmd =
            parse_command(&args("network requests --type xhr,fetch"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "requests");
        assert_eq!(cmd["type"], "xhr,fetch");
    }

    #[test]
    fn test_network_requests_method_filter() {
        let cmd = parse_command(&args("network requests --method POST"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "requests");
        assert_eq!(cmd["method"], "POST");
    }

    #[test]
    fn test_network_requests_status_filter() {
        let cmd = parse_command(&args("network requests --status 2xx"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "requests");
        assert_eq!(cmd["status"], "2xx");
    }

    #[test]
    fn test_network_requests_combined_filters() {
        let cmd = parse_command(
            &args("network requests --filter api --type xhr --method GET --status 200"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["filter"], "api");
        assert_eq!(cmd["type"], "xhr");
        assert_eq!(cmd["method"], "GET");
        assert_eq!(cmd["status"], "200");
    }

    #[test]
    fn test_network_request_detail() {
        let cmd = parse_command(&args("network request 1234.5"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "request_detail");
        assert_eq!(cmd["requestId"], "1234.5");
    }

    #[test]
    fn test_network_request_detail_requires_id() {
        let result = parse_command(&args("network request"), &default_flags());
        assert!(matches!(result, Err(ParseError::MissingArguments { .. })));
    }

    // === Screenshot ===

    #[test]
    fn test_screenshot() {
        let cmd = parse_command(&args("screenshot"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "screenshot");
        assert_eq!(cmd["path"], serde_json::Value::Null);
        assert_eq!(cmd["selector"], serde_json::Value::Null);
    }

    #[test]
    fn test_screenshot_path() {
        let cmd = parse_command(&args("screenshot out.png"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "screenshot");
        assert_eq!(cmd["path"], "out.png");
    }

    #[test]
    fn test_screenshot_full_page() {
        let cmd = parse_command(&args("screenshot --full"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "screenshot");
        assert_eq!(cmd["fullPage"], true);
    }

    #[test]
    fn test_screenshot_full_page_shorthand() {
        let cmd = parse_command(&args("screenshot -f"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "screenshot");
        assert_eq!(cmd["fullPage"], true);
    }

    #[test]
    fn test_screenshot_clip() {
        // `--clip x,y,w,h` captures a pixel region (issue #34); the path still parses.
        let cmd = parse_command(
            &args("screenshot --clip 10,20,200,40 out.png"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "screenshot");
        assert_eq!(cmd["clip"]["x"], 10.0);
        assert_eq!(cmd["clip"]["y"], 20.0);
        assert_eq!(cmd["clip"]["width"], 200.0);
        assert_eq!(cmd["clip"]["height"], 40.0);
        assert_eq!(cmd["path"], "out.png");
        // Bad clip is a clear error, not silent.
        assert!(parse_command(&args("screenshot --clip 1,2,3"), &default_flags()).is_err());
    }

    #[test]
    fn test_screenshot_with_ref() {
        let cmd = parse_command(&args("screenshot @e1"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "screenshot");
        assert_eq!(cmd["selector"], "@e1");
        assert_eq!(cmd["path"], serde_json::Value::Null);
    }

    #[test]
    fn test_screenshot_with_css_class() {
        let cmd = parse_command(&args("screenshot .my-button"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "screenshot");
        assert_eq!(cmd["selector"], ".my-button");
        assert_eq!(cmd["path"], serde_json::Value::Null);
    }

    #[test]
    fn test_screenshot_with_css_id() {
        let cmd = parse_command(&args("screenshot #header"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "screenshot");
        assert_eq!(cmd["selector"], "#header");
        assert_eq!(cmd["path"], serde_json::Value::Null);
    }

    #[test]
    fn test_screenshot_with_path() {
        let cmd = parse_command(&args("screenshot ./output.png"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "screenshot");
        assert_eq!(cmd["selector"], serde_json::Value::Null);
        assert_eq!(cmd["path"], "./output.png");
    }

    #[test]
    fn test_screenshot_with_selector_and_path() {
        let cmd = parse_command(&args("screenshot .btn ./button.png"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "screenshot");
        assert_eq!(cmd["selector"], ".btn");
        assert_eq!(cmd["path"], "./button.png");
    }

    // === Snapshot ===

    #[test]
    fn test_snapshot() {
        let cmd = parse_command(&args("snapshot"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "snapshot");
    }

    #[test]
    fn test_snapshot_interactive() {
        let cmd = parse_command(&args("snapshot -i"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "snapshot");
        assert_eq!(cmd["interactive"], true);
    }

    #[test]
    fn test_snapshot_cursor() {
        let cmd = parse_command(&args("snapshot -C"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "snapshot");
        assert_eq!(cmd["cursor"], true);
    }

    #[test]
    fn test_snapshot_interactive_cursor() {
        let cmd = parse_command(&args("snapshot -i -C"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "snapshot");
        assert_eq!(cmd["interactive"], true);
        assert_eq!(cmd["cursor"], true);
    }

    #[test]
    fn test_snapshot_compact() {
        let cmd = parse_command(&args("snapshot --compact"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "snapshot");
        assert_eq!(cmd["compact"], true);
    }

    #[test]
    fn test_snapshot_depth() {
        let cmd = parse_command(&args("snapshot -d 3"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "snapshot");
        assert_eq!(cmd["maxDepth"], 3);
    }

    #[test]
    fn test_snapshot_urls() {
        let cmd = parse_command(&args("snapshot -i --urls"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "snapshot");
        assert_eq!(cmd["interactive"], true);
        assert_eq!(cmd["urls"], true);
    }

    #[test]
    fn test_snapshot_urls_short() {
        let cmd = parse_command(&args("snapshot -i -u"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "snapshot");
        assert_eq!(cmd["urls"], true);
    }

    // === Wait ===

    #[test]
    fn test_wait_selector() {
        let cmd = parse_command(&args("wait #element"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "wait");
        assert_eq!(cmd["selector"], "#element");
    }

    #[test]
    fn test_wait_timeout() {
        let cmd = parse_command(&args("wait 5000"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "wait");
        assert_eq!(cmd["timeout"], 5000);
    }

    #[test]
    fn test_wait_url() {
        let cmd = parse_command(&args("wait --url **/dashboard"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "waitforurl");
        assert_eq!(cmd["url"], "**/dashboard");
    }

    #[test]
    fn test_wait_url_empty_pattern_rejected() {
        // An empty pattern would match any URL — reject it rather than silently
        // always-match. (Build argv directly: split_whitespace can't yield "".)
        let argv = vec!["wait".to_string(), "--url".to_string(), String::new()];
        let err = parse_command(&argv, &default_flags());
        assert!(err.is_err(), "empty --url pattern should be rejected");
    }

    #[test]
    fn test_wait_url_with_timeout() {
        // --timeout must be parsed for the --url path; without it a non-matching
        // pattern waits the full default (and could wedge the daemon).
        let cmd = parse_command(
            &args("wait --url **/dashboard --timeout 3000"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "waitforurl");
        assert_eq!(cmd["url"], "**/dashboard");
        assert_eq!(cmd["timeout"], 3000);
    }

    #[test]
    fn test_wait_load() {
        let cmd = parse_command(&args("wait --load networkidle"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "waitforloadstate");
        assert_eq!(cmd["state"], "networkidle");
    }

    #[test]
    fn test_wait_load_missing_state() {
        let result = parse_command(&args("wait --load"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_wait_fn() {
        let cmd = parse_command(&args("wait --fn window.ready"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "waitforfunction");
        assert_eq!(cmd["expression"], "window.ready");
    }

    #[test]
    fn test_wait_text() {
        let cmd = parse_command(&args("wait --text Welcome"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "wait");
        assert_eq!(cmd["text"], "Welcome");
        assert!(cmd.get("timeout").is_none());
    }

    #[test]
    fn test_wait_text_with_timeout() {
        let cmd = parse_command(
            &args("wait --text Welcome --timeout 5000"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "wait");
        assert_eq!(cmd["text"], "Welcome");
        assert_eq!(cmd["timeout"], 5000);
    }

    // === Clipboard Tests ===

    #[test]
    fn test_clipboard_read_default() {
        let cmd = parse_command(&args("clipboard"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "clipboard");
        assert_eq!(cmd["operation"], "read");
    }

    #[test]
    fn test_clipboard_read_explicit() {
        let cmd = parse_command(&args("clipboard read"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "clipboard");
        assert_eq!(cmd["operation"], "read");
    }

    #[test]
    fn test_clipboard_write() {
        let cmd = parse_command(&args("clipboard write hello"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "clipboard");
        assert_eq!(cmd["operation"], "write");
        assert_eq!(cmd["text"], "hello");
    }

    #[test]
    fn test_clipboard_write_multi_word() {
        let cmd = parse_command(&args("clipboard write hello world"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "clipboard");
        assert_eq!(cmd["operation"], "write");
        assert_eq!(cmd["text"], "hello world");
    }

    #[test]
    fn test_clipboard_copy() {
        let cmd = parse_command(&args("clipboard copy"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "clipboard");
        assert_eq!(cmd["operation"], "copy");
    }

    #[test]
    fn test_clipboard_paste() {
        let cmd = parse_command(&args("clipboard paste"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "clipboard");
        assert_eq!(cmd["operation"], "paste");
    }

    #[test]
    fn test_clipboard_write_missing_text() {
        let result = parse_command(&args("clipboard write"), &default_flags());
        assert!(result.is_err());
    }

    #[test]
    fn test_clipboard_unknown_subcommand() {
        let result = parse_command(&args("clipboard clear"), &default_flags());
        assert!(result.is_err());
    }

    // === Unknown command ===

    // === Record Tests ===

    #[test]
    fn test_record_start() {
        let cmd = parse_command(&args("record start output.webm"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "recording_start");
        assert_eq!(cmd["path"], "output.webm");
        assert!(cmd.get("url").is_none());
    }

    #[test]
    fn test_record_start_with_url() {
        let cmd = parse_command(
            &args("record start demo.webm https://example.com"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "recording_start");
        assert_eq!(cmd["path"], "demo.webm");
        assert_eq!(cmd["url"], "https://example.com");
    }

    #[test]
    fn test_record_start_with_url_no_protocol() {
        let cmd = parse_command(
            &args("record start demo.webm example.com"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "recording_start");
        assert_eq!(cmd["path"], "demo.webm");
        assert_eq!(cmd["url"], "https://example.com");
    }

    #[test]
    fn test_record_start_with_chrome_extension_url() {
        let cmd = parse_command(
            &args("record start demo.webm chrome-extension://abcdef/popup.html"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "recording_start");
        assert_eq!(cmd["path"], "demo.webm");
        assert_eq!(cmd["url"], "chrome-extension://abcdef/popup.html");
    }

    #[test]
    fn test_record_start_missing_path() {
        let result = parse_command(&args("record start"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_record_stop() {
        let cmd = parse_command(&args("record stop"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "recording_stop");
    }

    #[test]
    fn test_record_restart() {
        let cmd = parse_command(&args("record restart output.webm"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "recording_restart");
        assert_eq!(cmd["path"], "output.webm");
        assert!(cmd.get("url").is_none());
    }

    #[test]
    fn test_record_restart_with_url() {
        let cmd = parse_command(
            &args("record restart demo.webm https://example.com"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "recording_restart");
        assert_eq!(cmd["path"], "demo.webm");
        assert_eq!(cmd["url"], "https://example.com");
    }

    #[test]
    fn test_record_restart_missing_path() {
        let result = parse_command(&args("record restart"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_record_invalid_subcommand() {
        let result = parse_command(&args("record foo"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::UnknownSubcommand { .. }
        ));
    }

    #[test]
    fn test_record_missing_subcommand() {
        let result = parse_command(&args("record"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    // === Profile (CDP Tracing) Tests ===

    #[test]
    fn test_profiler_start() {
        let cmd = parse_command(&args("profiler start"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "profiler_start");
        assert!(cmd.get("categories").is_none());
    }

    #[test]
    fn test_profiler_start_with_categories() {
        let cmd = parse_command(
            &args("profiler start --categories devtools.timeline,v8.execute"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "profiler_start");
        let categories = cmd["categories"].as_array().unwrap();
        assert_eq!(categories.len(), 2);
        assert_eq!(categories[0], "devtools.timeline");
        assert_eq!(categories[1], "v8.execute");
    }

    #[test]
    fn test_profiler_start_categories_missing_value() {
        let result = parse_command(&args("profiler start --categories"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_profiler_stop_with_path() {
        let cmd = parse_command(&args("profiler stop trace.json"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "profiler_stop");
        assert_eq!(cmd["path"], "trace.json");
    }

    #[test]
    fn test_profiler_stop_no_path() {
        let cmd = parse_command(&args("profiler stop"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "profiler_stop");
        assert!(cmd.get("path").is_none());
    }

    #[test]
    fn test_profiler_invalid_subcommand() {
        let result = parse_command(&args("profiler foo"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::UnknownSubcommand { .. }
        ));
    }

    #[test]
    fn test_profiler_missing_subcommand() {
        let result = parse_command(&args("profiler"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    // === Eval Tests ===

    #[test]
    fn test_eval_basic() {
        let cmd = parse_command(&args("eval document.title"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "evaluate");
        assert_eq!(cmd["script"], "document.title");
    }

    #[test]
    fn test_eval_no_frame_field_by_default() {
        let cmd = parse_command(&args("eval document.title"), &default_flags()).unwrap();
        assert!(cmd.get("frame").is_none());
    }

    #[test]
    fn test_eval_frame_flag() {
        let cmd = parse_command(
            &args("eval --frame accounts.google.com location.href"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "evaluate");
        assert_eq!(cmd["frame"], "accounts.google.com");
        assert_eq!(cmd["script"], "location.href");
    }

    // === expect (assertion verb) parse tests ===
    #[test]
    fn test_expect_element_state_canonicalized() {
        let c = parse_command(&args("expect #x gone"), &default_flags()).unwrap();
        assert_eq!(c["action"], "expect");
        assert_eq!(c["kind"], "element");
        assert_eq!(c["selector"], "#x");
        assert_eq!(c["state"], "detached"); // gone → detached (review fix)
        let c = parse_command(&args("expect @e5 visible"), &default_flags()).unwrap();
        assert_eq!(c["kind"], "element");
        assert_eq!(c["state"], "visible");
        let c = parse_command(&args("expect #m present"), &default_flags()).unwrap();
        assert_eq!(c["state"], "attached"); // present → attached
    }

    #[test]
    fn test_expect_unknown_state_errors() {
        assert!(parse_command(&args("expect #x wobbly"), &default_flags()).is_err());
    }

    #[test]
    fn test_expect_count_ops() {
        let c = parse_command(&args("expect count .row >= 3"), &default_flags()).unwrap();
        assert_eq!(c["kind"], "count");
        assert_eq!(c["op"], "ge");
        assert_eq!(c["value"], 3);
        let c = parse_command(&args("expect count .row == 5"), &default_flags()).unwrap();
        assert_eq!(c["op"], "eq");
        // word form + bad value / bad op
        assert_eq!(
            parse_command(&args("expect count .r ne 2"), &default_flags()).unwrap()["op"],
            "ne"
        );
        assert!(parse_command(&args("expect count .r >= x"), &default_flags()).is_err());
        assert!(parse_command(&args("expect count .r ?? 2"), &default_flags()).is_err());
        // count rejects @ref
        assert!(parse_command(&args("expect count @e1 == 1"), &default_flags()).is_err());
    }

    #[test]
    fn test_expect_text_value_attr() {
        let c = parse_command(&args("expect text @e1 equals Saved"), &default_flags()).unwrap();
        assert_eq!(c["kind"], "text");
        assert_eq!(c["predicate"], "equals");
        assert_eq!(c["expected"], "Saved");
        let c = parse_command(
            &args("expect text #t contains hello world"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(c["expected"], "hello world"); // remaining tokens joined
        let c = parse_command(&args("expect text #t matches ^Saved$"), &default_flags()).unwrap();
        assert_eq!(c["regex"], true); // matches ⇒ regex
        let c = parse_command(
            &args("expect attr @e1 aria-expanded equals true"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(c["kind"], "attr");
        assert_eq!(c["attr"], "aria-expanded");
        assert!(parse_command(&args("expect text #t bogus x"), &default_flags()).is_err());
    }

    #[test]
    fn test_expect_url_request_errors() {
        let c = parse_command(&args("expect url contains /dash"), &default_flags()).unwrap();
        assert_eq!(c["kind"], "url");
        assert_eq!(c["predicate"], "contains");
        let c = parse_command(
            &args("expect request /api/save --method POST --status 2xx --count 1"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(c["kind"], "request");
        assert_eq!(c["filter"], "/api/save");
        assert_eq!(c["method"], "POST");
        assert_eq!(c["status"], "2xx");
        assert_eq!(c["count"], 1);
        let c = parse_command(&args("expect no-errors"), &default_flags()).unwrap();
        assert_eq!(c["kind"], "errors");
        assert_eq!(c["max"], 0);
    }

    #[test]
    fn test_if_present_stamps_action() {
        let mut f = default_flags();
        f.if_present = true;
        let c = parse_command(&args("click #x"), &f).unwrap();
        assert_eq!(c["ifPresent"], true);
        // off by default
        let c = parse_command(&args("click #x"), &default_flags()).unwrap();
        assert!(c.get("ifPresent").is_none());
    }

    #[test]
    fn test_observe_stamps_action() {
        let mut f = default_flags();
        f.observe = true;
        let c = parse_command(&args("click @e1"), &f).unwrap();
        assert_eq!(c["observe"], true);
        let c = parse_command(&args("click @e1"), &default_flags()).unwrap();
        assert!(c.get("observe").is_none());
    }

    // === form fill parse tests ===
    #[test]
    fn test_form_fill_ok() {
        let c = parse_command(
            &args(r#"form fill --map {"Email":"a@b.com","Sub":true} --submit Save"#),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(c["action"], "form_fill");
        assert_eq!(c["map"]["Email"], "a@b.com");
        assert_eq!(c["map"]["Sub"], true);
        assert_eq!(c["submit"], "Save");
    }

    #[test]
    fn test_form_fill_validation() {
        assert!(parse_command(&args("form fill --map {bad"), &default_flags()).is_err());
        assert!(parse_command(&args("form fill --map {}"), &default_flags()).is_err());
        assert!(parse_command(&args("form fill"), &default_flags()).is_err());
        assert!(parse_command(&args("form wiggle"), &default_flags()).is_err());
    }

    // === extract parse tests ===
    #[test]
    fn test_extract_schema_ok() {
        let c = parse_command(
            &args(r#"extract --schema {"rows":".card","fields":{"name":".t"}}"#),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(c["action"], "extract");
        assert_eq!(c["schema"]["rows"], ".card");
        assert_eq!(c["schema"]["fields"]["name"], ".t");
    }

    #[test]
    fn test_extract_requires_fields_and_valid_json() {
        // missing fields
        assert!(
            parse_command(&args(r#"extract --schema {"rows":".x"}"#), &default_flags()).is_err()
        );
        // invalid json
        assert!(parse_command(&args("extract --schema {bad"), &default_flags()).is_err());
        // no source
        assert!(parse_command(&args("extract"), &default_flags()).is_err());
    }

    #[test]
    fn test_extract_leading_frame() {
        let c = parse_command(
            &args(r#"extract --frame 1 --schema {"fields":{"a":".x"}}"#),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(c["frame"], "1");
        assert_eq!(c["schema"]["fields"]["a"], ".x");
    }

    #[test]
    fn test_expect_flags_stamped() {
        let c = parse_command(
            &args("expect #x visible --timeout 9000 --not --no-wait"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(c["timeout"], 9000);
        assert_eq!(c["not"], true);
        assert_eq!(c["noWait"], true);
    }

    #[test]
    fn test_eval_frame_flag_with_index_and_base64() {
        // --frame composes with the source flags; index value preserved verbatim.
        let cmd = parse_command(
            &args("eval --frame 2 -b ZG9jdW1lbnQudGl0bGU="),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["frame"], "2");
        assert_eq!(cmd["script"], "document.title");
    }

    #[test]
    fn test_eval_frame_flag_requires_value() {
        assert!(parse_command(&args("eval --frame"), &default_flags()).is_err());
    }

    #[test]
    fn test_eval_base64_short_flag() {
        // "document.title" in base64
        let cmd = parse_command(&args("eval -b ZG9jdW1lbnQudGl0bGU="), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "evaluate");
        assert_eq!(cmd["script"], "document.title");
    }

    #[test]
    fn test_eval_base64_long_flag() {
        // "document.title" in base64
        let cmd = parse_command(
            &args("eval --base64 ZG9jdW1lbnQudGl0bGU="),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "evaluate");
        assert_eq!(cmd["script"], "document.title");
    }

    #[test]
    fn test_eval_base64_with_special_chars() {
        // "document.querySelector('[src*=\"_next\"]')" in base64
        let cmd = parse_command(
            &args("eval -b ZG9jdW1lbnQucXVlcnlTZWxlY3RvcignW3NyYyo9Il9uZXh0Il0nKQ=="),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "evaluate");
        assert_eq!(cmd["script"], "document.querySelector('[src*=\"_next\"]')");
    }

    #[test]
    fn test_eval_base64_invalid() {
        let result = parse_command(&args("eval -b !!!invalid!!!"), &default_flags());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ParseError::InvalidValue { .. }));
        assert!(err.format().contains("Invalid base64"));
    }

    #[test]
    fn test_unknown_command() {
        let result = parse_command(&args("unknowncommand"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::UnknownCommand { .. }
        ));
    }

    #[test]
    fn test_empty_args() {
        let result = parse_command(&[], &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    // === Error message tests ===

    #[test]
    fn test_get_missing_subcommand() {
        let result = parse_command(&args("get"), &default_flags());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ParseError::MissingArguments { .. }));
        assert!(err.format().contains("get"));
    }

    #[test]
    fn test_get_unknown_subcommand() {
        let result = parse_command(&args("get foo"), &default_flags());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ParseError::UnknownSubcommand { .. }));
        assert!(err.format().contains("foo"));
        assert!(err.format().contains("text"));
    }

    #[test]
    fn test_get_text_defaults_to_all_frames() {
        // `get text` with no selector now reads the whole page across ALL frames
        // by default (#27), so iframed content isn't silently missed. (Was: a
        // top-frame `body` read, #24-D.)
        let cmd = parse_command(&args("get text"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "gettext");
        assert_eq!(cmd["allFrames"], true);
        assert!(cmd.get("selector").is_none());
        // An explicit selector still wins and stays element-scoped.
        let cmd2 = parse_command(&args("get text h1"), &default_flags()).unwrap();
        assert_eq!(cmd2["selector"], "h1");
        assert!(cmd2.get("allFrames").is_none());
        // `text` top-level shortcut behaves the same.
        let cmd3 = parse_command(&args("text"), &default_flags()).unwrap();
        assert_eq!(cmd3["allFrames"], true);
    }

    #[test]
    fn test_get_text_all_frames() {
        // `--all-frames` switches to whole-page, cross-frame aggregation and
        // drops the selector (issue #27).
        for variant in ["get text --all-frames", "get text --frames", "text -a"] {
            let cmd = parse_command(&args(variant), &default_flags()).unwrap();
            assert_eq!(cmd["action"], "gettext", "{variant}");
            assert_eq!(cmd["allFrames"], true, "{variant}");
            assert!(cmd.get("selector").is_none(), "{variant}");
        }
        // A flag mixed with a selector still triggers all-frames.
        let cmd = parse_command(&args("get text body --all-frames"), &default_flags()).unwrap();
        assert_eq!(cmd["allFrames"], true);
        // Without the flag, a leading flag-like token is skipped for the selector.
        let cmd = parse_command(&args("get text main"), &default_flags()).unwrap();
        assert_eq!(cmd["selector"], "main");
        assert!(cmd.get("allFrames").is_none());
    }

    #[test]
    fn test_frames_command() {
        let cmd = parse_command(&args("frames"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "frames");
    }

    #[test]
    fn test_nearest_command_suggestions() {
        assert_eq!(nearest_command("sesions").as_deref(), Some("sessions"));
        assert_eq!(nearest_command("session").as_deref(), Some("sessions"));
        assert_eq!(nearest_command("clik").as_deref(), Some("click"));
        assert_eq!(
            nearest_command("screenshits").as_deref(),
            Some("screenshot")
        );
        // Nonsense with no close match stays silent.
        assert_eq!(nearest_command("xyzzy"), None);
        // The unknown-command error embeds the suggestion.
        let err = ParseError::UnknownCommand {
            command: "sesions".to_string(),
        };
        assert!(err.format().contains("Did you mean: chrome-use sessions?"));
    }

    #[test]
    fn test_get_text_main() {
        for variant in ["get text --main", "get text --readable", "text -m"] {
            let cmd = parse_command(&args(variant), &default_flags()).unwrap();
            assert_eq!(cmd["action"], "gettext", "{variant}");
            assert_eq!(cmd["main"], true, "{variant}");
            assert!(cmd.get("selector").is_none(), "{variant}");
        }
    }

    #[test]
    fn test_get_text_pierce() {
        for variant in ["get text --pierce", "get text --shadow", "text --deep"] {
            let cmd = parse_command(&args(variant), &default_flags()).unwrap();
            assert_eq!(cmd["action"], "gettext", "{variant}");
            assert_eq!(cmd["pierce"], true, "{variant}");
            assert!(cmd.get("selector").is_none(), "{variant}");
        }
    }

    #[test]
    fn test_tab_activate_flag() {
        let plain = parse_command(&args("tab t3"), &default_flags()).unwrap();
        assert_eq!(plain["action"], "tab_switch");
        assert!(plain.get("activate").is_none());

        let act = parse_command(&args("tab t3 --activate"), &default_flags()).unwrap();
        assert_eq!(act["action"], "tab_switch");
        assert_eq!(act["tabId"], "t3");
        assert_eq!(act["activate"], true);
        // `--front` alias.
        let front = parse_command(&args("tab t3 --front"), &default_flags()).unwrap();
        assert_eq!(front["activate"], true);
    }

    // === Protocol alignment tests ===

    #[test]
    fn test_mouse_wheel() {
        let cmd = parse_command(&args("mouse wheel 100 50"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "wheel");
        assert_eq!(cmd["deltaY"], 100);
        assert_eq!(cmd["deltaX"], 50);
    }

    #[test]
    fn test_set_media() {
        let cmd = parse_command(&args("set media dark"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "emulatemedia");
        assert_eq!(cmd["colorScheme"], "dark");
        assert_eq!(cmd["reducedMotion"], "no-preference");
    }

    #[test]
    fn test_set_media_reduced_motion() {
        let cmd = parse_command(&args("set media light reduced-motion"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "emulatemedia");
        assert_eq!(cmd["colorScheme"], "light");
        assert_eq!(cmd["reducedMotion"], "reduce");
    }

    #[test]
    fn test_set_viewport() {
        let cmd = parse_command(&args("set viewport 1920 1080"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "viewport");
        assert_eq!(cmd["width"], 1920);
        assert_eq!(cmd["height"], 1080);
        assert!(cmd.get("deviceScaleFactor").is_none());
    }

    #[test]
    fn test_set_viewport_with_scale() {
        let cmd = parse_command(&args("set viewport 1920 1080 2"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "viewport");
        assert_eq!(cmd["width"], 1920);
        assert_eq!(cmd["height"], 1080);
        assert_eq!(cmd["deviceScaleFactor"], 2.0);
    }

    #[test]
    fn test_set_viewport_with_fractional_scale() {
        let cmd = parse_command(&args("set viewport 375 812 3"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "viewport");
        assert_eq!(cmd["width"], 375);
        assert_eq!(cmd["height"], 812);
        assert_eq!(cmd["deviceScaleFactor"], 3.0);
    }

    #[test]
    fn test_set_viewport_missing_height() {
        let result = parse_command(&args("set viewport 1920"), &default_flags());
        assert!(result.is_err());
    }

    #[test]
    fn test_set_viewport_invalid_scale() {
        let result = parse_command(&args("set viewport 1920 1080 abc"), &default_flags());
        assert!(result.is_err());
    }

    #[test]
    fn test_viewport_toplevel() {
        let cmd = parse_command(&args("viewport 1280 800"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "viewport");
        assert_eq!(cmd["width"], 1280);
        assert_eq!(cmd["height"], 800);
        assert!(cmd.get("deviceScaleFactor").is_none());
        assert!(cmd.get("mobile").is_none());
    }

    #[test]
    fn test_resize_alias() {
        let cmd = parse_command(&args("resize 700 800"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "viewport");
        assert_eq!(cmd["width"], 700);
        assert_eq!(cmd["height"], 800);
    }

    #[test]
    fn test_viewport_wxh_form() {
        let cmd = parse_command(&args("viewport 375x812"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "viewport");
        assert_eq!(cmd["width"], 375);
        assert_eq!(cmd["height"], 812);
    }

    #[test]
    fn test_viewport_dpr_and_mobile_flags() {
        let cmd =
            parse_command(&args("viewport 375 812 --dpr 3 --mobile"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "viewport");
        assert_eq!(cmd["width"], 375);
        assert_eq!(cmd["height"], 812);
        assert_eq!(cmd["deviceScaleFactor"], 3.0);
        assert_eq!(cmd["mobile"], true);
    }

    #[test]
    fn test_viewport_reset() {
        for spec in ["viewport reset", "viewport clear", "resize reset"] {
            let cmd = parse_command(&args(spec), &default_flags()).unwrap();
            assert_eq!(cmd["action"], "viewport", "{spec}");
            assert_eq!(cmd["reset"], true, "{spec}");
        }
    }

    #[test]
    fn test_viewport_missing_height() {
        assert!(parse_command(&args("viewport 1280"), &default_flags()).is_err());
    }

    #[test]
    fn test_viewport_invalid_width() {
        assert!(parse_command(&args("viewport abc 800"), &default_flags()).is_err());
    }

    #[test]
    fn test_find_first_no_value() {
        let cmd = parse_command(&args("find first a click"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "nth");
        assert_eq!(cmd["index"], 0);
        assert!(cmd.get("value").is_none());
    }

    #[test]
    fn test_find_first_with_value() {
        let cmd = parse_command(&args("find first input fill hello"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "nth");
        assert_eq!(cmd["index"], 0);
        assert_eq!(cmd["value"], "hello");
    }

    #[test]
    fn test_find_nth_no_value() {
        let cmd = parse_command(&args("find nth 2 a click"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "nth");
        assert_eq!(cmd["index"], 2);
        assert!(cmd.get("value").is_none());
    }

    #[test]
    fn test_find_role_fill_does_not_include_flags_in_value() {
        let cmd = parse_command(
            &args("find role textbox fill hello --name username --exact"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "getbyrole");
        assert_eq!(cmd["role"], "textbox");
        assert_eq!(cmd["subaction"], "fill");
        assert_eq!(cmd["name"], "username");
        assert_eq!(cmd["exact"], true);
        assert_eq!(cmd["value"], "hello");
    }

    // === Download Tests ===

    #[test]
    fn test_download() {
        let cmd = parse_command(&args("download #btn ./file.pdf"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "download");
        assert_eq!(cmd["selector"], "#btn");
        assert_eq!(cmd["path"], "./file.pdf");
    }

    #[test]
    fn test_download_with_ref() {
        let cmd = parse_command(&args("download @e5 ./report.xlsx"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "download");
        assert_eq!(cmd["selector"], "@e5");
        assert_eq!(cmd["path"], "./report.xlsx");
    }

    #[test]
    fn test_download_missing_path() {
        let result = parse_command(&args("download #btn"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_download_missing_selector() {
        let result = parse_command(&args("download"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    // === Wait for Download Tests ===

    #[test]
    fn test_wait_download() {
        let cmd = parse_command(&args("wait --download"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "waitfordownload");
        assert!(cmd.get("path").is_none());
    }

    #[test]
    fn test_wait_download_with_path() {
        let cmd = parse_command(&args("wait --download ./file.pdf"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "waitfordownload");
        assert_eq!(cmd["path"], "./file.pdf");
    }

    #[test]
    fn test_wait_download_with_timeout() {
        let cmd =
            parse_command(&args("wait --download --timeout 30000"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "waitfordownload");
        assert_eq!(cmd["timeout"], 30000);
    }

    #[test]
    fn test_wait_download_with_path_and_timeout() {
        let cmd = parse_command(
            &args("wait --download ./file.pdf --timeout 30000"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "waitfordownload");
        assert_eq!(cmd["path"], "./file.pdf");
        assert_eq!(cmd["timeout"], 30000);
    }

    #[test]
    fn test_wait_download_short_flag() {
        let cmd = parse_command(&args("wait -d ./file.pdf"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "waitfordownload");
        assert_eq!(cmd["path"], "./file.pdf");
    }

    // === Default timeout (AGENT_BROWSER_DEFAULT_TIMEOUT) tests ===

    fn flags_with_default_timeout(ms: u64) -> Flags {
        let mut f = default_flags();
        f.default_timeout = Some(ms);
        f
    }

    #[test]
    fn test_wait_selector_inherits_default_timeout() {
        let flags = flags_with_default_timeout(3000);
        let cmd = parse_command(&args("wait #element"), &flags).unwrap();
        assert_eq!(cmd["action"], "wait");
        assert_eq!(cmd["selector"], "#element");
        assert_eq!(cmd["timeout"], 3000);
    }

    #[test]
    fn test_wait_url_inherits_default_timeout() {
        let flags = flags_with_default_timeout(4000);
        let cmd = parse_command(&args("wait --url **/dashboard"), &flags).unwrap();
        assert_eq!(cmd["action"], "waitforurl");
        assert_eq!(cmd["timeout"], 4000);
    }

    #[test]
    fn test_wait_load_inherits_default_timeout() {
        let flags = flags_with_default_timeout(4000);
        let cmd = parse_command(&args("wait --load networkidle"), &flags).unwrap();
        assert_eq!(cmd["action"], "waitforloadstate");
        assert_eq!(cmd["timeout"], 4000);
    }

    #[test]
    fn test_wait_fn_inherits_default_timeout() {
        let flags = flags_with_default_timeout(4000);
        let cmd = parse_command(&args("wait --fn window.ready"), &flags).unwrap();
        assert_eq!(cmd["action"], "waitforfunction");
        assert_eq!(cmd["timeout"], 4000);
    }

    #[test]
    fn test_wait_text_inherits_default_timeout() {
        let flags = flags_with_default_timeout(2000);
        let cmd = parse_command(&args("wait --text Welcome"), &flags).unwrap();
        assert_eq!(cmd["action"], "wait");
        assert_eq!(cmd["text"], "Welcome");
        assert_eq!(cmd["timeout"], 2000);
    }

    #[test]
    fn test_wait_download_inherits_default_timeout() {
        let flags = flags_with_default_timeout(5000);
        let cmd = parse_command(&args("wait --download"), &flags).unwrap();
        assert_eq!(cmd["action"], "waitfordownload");
        assert_eq!(cmd["timeout"], 5000);
    }

    #[test]
    fn test_wait_explicit_timeout_overrides_default() {
        let flags = flags_with_default_timeout(5000);
        let cmd = parse_command(&args("wait --text Welcome --timeout 1000"), &flags).unwrap();
        assert_eq!(cmd["timeout"], 1000);
    }

    #[test]
    fn test_wait_no_default_timeout_omits_field() {
        let cmd = parse_command(&args("wait #element"), &default_flags()).unwrap();
        assert!(cmd.get("timeout").is_none());
    }

    // === Connect (CDP) tests ===

    #[test]
    fn test_connect_with_port() {
        let cmd = parse_command(&args("connect 9222"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "launch");
        assert_eq!(cmd["cdpPort"], 9222);
        assert!(cmd.get("cdpUrl").is_none());
    }

    #[test]
    fn test_connect_with_ws_url() {
        let input: Vec<String> = vec![
            "connect".to_string(),
            "ws://localhost:9222/devtools/browser/abc123".to_string(),
        ];
        let cmd = parse_command(&input, &default_flags()).unwrap();
        assert_eq!(cmd["action"], "launch");
        assert_eq!(cmd["cdpUrl"], "ws://localhost:9222/devtools/browser/abc123");
        assert!(cmd.get("cdpPort").is_none());
    }

    #[test]
    fn test_connect_with_wss_url() {
        let input: Vec<String> = vec![
            "connect".to_string(),
            "wss://remote-browser.example.com/cdp?token=xyz".to_string(),
        ];
        let cmd = parse_command(&input, &default_flags()).unwrap();
        assert_eq!(cmd["action"], "launch");
        assert_eq!(
            cmd["cdpUrl"],
            "wss://remote-browser.example.com/cdp?token=xyz"
        );
        assert!(cmd.get("cdpPort").is_none());
    }

    #[test]
    fn test_connect_with_http_url() {
        let input: Vec<String> = vec!["connect".to_string(), "http://localhost:9222".to_string()];
        let cmd = parse_command(&input, &default_flags()).unwrap();
        assert_eq!(cmd["action"], "launch");
        assert_eq!(cmd["cdpUrl"], "http://localhost:9222");
        assert!(cmd.get("cdpPort").is_none());
    }

    #[test]
    fn test_connect_missing_argument() {
        let result = parse_command(&args("connect"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_connect_invalid_port() {
        let result = parse_command(&args("connect notanumber"), &default_flags());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ParseError::InvalidValue { .. }));
        assert!(err.format().contains("not a valid port number or URL"));
    }

    #[test]
    fn test_connect_port_zero() {
        let result = parse_command(&args("connect 0"), &default_flags());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ParseError::InvalidValue { .. }));
        assert!(err.format().contains("port must be greater than 0"));
    }

    #[test]
    fn test_connect_port_out_of_range() {
        let result = parse_command(&args("connect 65536"), &default_flags());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ParseError::InvalidValue { .. }));
        assert!(err.format().contains("out of range"));
        assert!(err.format().contains("1-65535"));
    }

    #[test]
    fn test_connect_port_max_valid() {
        let cmd = parse_command(&args("connect 65535"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "launch");
        assert_eq!(cmd["cdpPort"], 65535);
    }

    #[test]
    fn test_connect_port_min_valid() {
        let cmd = parse_command(&args("connect 1"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "launch");
        assert_eq!(cmd["cdpPort"], 1);
    }

    // === Runtime stream control tests ===

    #[test]
    fn test_stream_enable_auto_port() {
        let cmd = parse_command(&args("stream enable"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "stream_enable");
        assert!(cmd.get("port").is_none());
    }

    #[test]
    fn test_stream_enable_with_port() {
        let cmd = parse_command(&args("stream enable --port 9223"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "stream_enable");
        assert_eq!(cmd["port"], 9223);
    }

    #[test]
    fn test_stream_status() {
        let cmd = parse_command(&args("stream status"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "stream_status");
    }

    #[test]
    fn test_stream_disable() {
        let cmd = parse_command(&args("stream disable"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "stream_disable");
    }

    #[test]
    fn test_stream_enable_invalid_port() {
        let result = parse_command(&args("stream enable --port abc"), &default_flags());
        assert!(matches!(result, Err(ParseError::InvalidValue { .. })));
    }

    #[test]
    fn test_stream_missing_subcommand() {
        let result = parse_command(&args("stream"), &default_flags());
        assert!(matches!(result, Err(ParseError::MissingArguments { .. })));
    }

    // === Trace Tests ===

    #[test]
    fn test_trace_start() {
        let cmd = parse_command(&args("trace start"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "trace_start");
    }

    #[test]
    fn test_trace_stop_with_path() {
        let cmd = parse_command(&args("trace stop ./trace.zip"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "trace_stop");
        assert_eq!(cmd["path"], "./trace.zip");
    }

    #[test]
    fn test_trace_stop_without_path() {
        let cmd = parse_command(&args("trace stop"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "trace_stop");
        assert!(cmd.get("path").is_none() || cmd["path"].is_null());
    }

    // === Diff Tests ===

    #[test]
    fn test_diff_snapshot_basic() {
        let cmd = parse_command(&args("diff snapshot"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "diff_snapshot");
    }

    #[test]
    fn test_diff_snapshot_baseline() {
        let cmd = parse_command(
            &args("diff snapshot --baseline before.txt"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_snapshot");
        assert_eq!(cmd["baseline"], "before.txt");
    }

    #[test]
    fn test_diff_snapshot_selector_compact_depth() {
        let cmd = parse_command(
            &args("diff snapshot --selector #main --compact --depth 3"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_snapshot");
        assert_eq!(cmd["selector"], "#main");
        assert_eq!(cmd["compact"], true);
        assert_eq!(cmd["maxDepth"], 3);
    }

    #[test]
    fn test_diff_snapshot_short_flags() {
        let cmd = parse_command(
            &args("diff snapshot -b snap.txt -s .content -c -d 2"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_snapshot");
        assert_eq!(cmd["baseline"], "snap.txt");
        assert_eq!(cmd["selector"], ".content");
        assert_eq!(cmd["compact"], true);
        assert_eq!(cmd["maxDepth"], 2);
    }

    #[test]
    fn test_diff_screenshot_baseline() {
        let cmd = parse_command(
            &args("diff screenshot --baseline before.png"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_screenshot");
        assert_eq!(cmd["baseline"], "before.png");
    }

    #[test]
    fn test_diff_screenshot_all_options() {
        let cmd = parse_command(
            &args("diff screenshot --baseline b.png --output d.png --threshold 0.2 --selector #hero --full"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_screenshot");
        assert_eq!(cmd["baseline"], "b.png");
        assert_eq!(cmd["output"], "d.png");
        assert_eq!(cmd["threshold"], 0.2);
        assert_eq!(cmd["selector"], "#hero");
        assert_eq!(cmd["fullPage"], true);
    }

    #[test]
    fn test_diff_screenshot_missing_baseline() {
        let result = parse_command(&args("diff screenshot"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_diff_screenshot_command_full_flag() {
        let cmd = parse_command(
            &args("diff screenshot --baseline b.png --full"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_screenshot");
        assert_eq!(cmd["fullPage"], true);
    }

    #[test]
    fn test_diff_screenshot_command_full_flag_shorthand() {
        let cmd = parse_command(
            &args("diff screenshot --baseline b.png -f"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_screenshot");
        assert_eq!(cmd["fullPage"], true);
    }

    #[test]
    fn test_diff_url_basic() {
        let cmd = parse_command(
            &args("diff url https://a.com https://b.com"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_url");
        assert_eq!(cmd["url1"], "https://a.com");
        assert_eq!(cmd["url2"], "https://b.com");
    }

    #[test]
    fn test_diff_url_with_screenshot_full() {
        let cmd = parse_command(
            &args("diff url https://a.com https://b.com --screenshot --full"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_url");
        assert_eq!(cmd["screenshot"], true);
        assert_eq!(cmd["fullPage"], true);
    }

    #[test]
    fn test_diff_url_with_wait_until() {
        let cmd = parse_command(
            &args("diff url https://a.com https://b.com --wait-until networkidle"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_url");
        assert_eq!(cmd["waitUntil"], "networkidle");
    }

    #[test]
    fn test_diff_url_command_full_flag() {
        let cmd = parse_command(
            &args("diff url https://a.com https://b.com --full"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["fullPage"], true);
    }

    #[test]
    fn test_diff_missing_subcommand() {
        let result = parse_command(&args("diff"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_diff_unknown_subcommand() {
        let result = parse_command(&args("diff invalid"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::UnknownSubcommand { .. }
        ));
    }

    #[test]
    fn test_diff_snapshot_baseline_missing_value() {
        let result = parse_command(&args("diff snapshot --baseline"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_diff_snapshot_selector_missing_value() {
        let result = parse_command(&args("diff snapshot --selector"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_diff_snapshot_depth_missing_value() {
        let result = parse_command(&args("diff snapshot --depth"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_diff_screenshot_threshold_missing_value() {
        let result = parse_command(
            &args("diff screenshot --baseline b.png --threshold"),
            &default_flags(),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_diff_screenshot_output_missing_value() {
        let result = parse_command(
            &args("diff screenshot --baseline b.png --output"),
            &default_flags(),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_diff_url_wait_until_missing_value() {
        let result = parse_command(
            &args("diff url https://a.com https://b.com --wait-until"),
            &default_flags(),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_diff_snapshot_unexpected_arg() {
        let result = parse_command(&args("diff snapshot foo"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::InvalidValue { .. }
        ));
    }

    #[test]
    fn test_diff_screenshot_unexpected_arg() {
        let result = parse_command(
            &args("diff screenshot --baseline b.png unexpected"),
            &default_flags(),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::InvalidValue { .. }
        ));
    }

    #[test]
    fn test_diff_url_unexpected_arg() {
        let result = parse_command(
            &args("diff url https://a.com https://b.com extra"),
            &default_flags(),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::InvalidValue { .. }
        ));
    }

    #[test]
    fn test_diff_snapshot_unknown_flag() {
        let result = parse_command(&args("diff snapshot --invalid"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::InvalidValue { .. }
        ));
    }

    #[test]
    fn test_diff_url_missing_urls() {
        let result = parse_command(&args("diff url"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_diff_url_missing_second_url() {
        let result = parse_command(&args("diff url https://a.com"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_diff_snapshot_depth_invalid_value() {
        let result = parse_command(&args("diff snapshot --depth abc"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::InvalidValue { .. }
        ));
    }

    #[test]
    fn test_diff_screenshot_threshold_invalid_value() {
        let result = parse_command(
            &args("diff screenshot --baseline b.png --threshold abc"),
            &default_flags(),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::InvalidValue { .. }
        ));
    }

    #[test]
    fn test_diff_screenshot_threshold_out_of_range() {
        let result = parse_command(
            &args("diff screenshot --baseline b.png --threshold 1.5"),
            &default_flags(),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::InvalidValue { .. }
        ));
    }

    #[test]
    fn test_diff_screenshot_threshold_negative() {
        let result = parse_command(
            &args("diff screenshot --baseline b.png --threshold -0.5"),
            &default_flags(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_diff_url_with_selector() {
        let cmd = parse_command(
            &args("diff url https://a.com https://b.com --selector #main"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_url");
        assert_eq!(cmd["selector"], "#main");
    }

    #[test]
    fn test_diff_url_with_compact_depth() {
        let cmd = parse_command(
            &args("diff url https://a.com https://b.com --compact --depth 3"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_url");
        assert_eq!(cmd["compact"], true);
        assert_eq!(cmd["maxDepth"], 3);
    }

    #[test]
    fn test_diff_url_with_short_snapshot_flags() {
        let cmd = parse_command(
            &args("diff url https://a.com https://b.com -s .content -c -d 2"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_url");
        assert_eq!(cmd["selector"], ".content");
        assert_eq!(cmd["compact"], true);
        assert_eq!(cmd["maxDepth"], 2);
    }

    #[test]
    fn test_diff_url_depth_invalid_value() {
        let result = parse_command(
            &args("diff url https://a.com https://b.com --depth abc"),
            &default_flags(),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::InvalidValue { .. }
        ));
    }

    #[test]
    fn test_diff_snapshot_depth_negative_value() {
        let result = parse_command(&args("diff snapshot --depth -1"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::InvalidValue { .. }
        ));
    }

    #[test]
    fn test_diff_url_depth_negative_value() {
        let result = parse_command(
            &args("diff url https://a.com https://b.com --depth -1"),
            &default_flags(),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::InvalidValue { .. }
        ));
    }

    #[test]
    fn test_diff_url_selector_missing_value() {
        let result = parse_command(
            &args("diff url https://a.com https://b.com --selector"),
            &default_flags(),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    // === Scroll Tests ===

    #[test]
    fn test_scroll_defaults() {
        let cmd = parse_command(&args("scroll"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "scroll");
        assert_eq!(cmd["direction"], "down");
        assert_eq!(cmd["amount"], 300);
        assert!(cmd.get("selector").is_none());
    }

    #[test]
    fn test_scroll_direction_and_amount() {
        let cmd = parse_command(&args("scroll up 200"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "scroll");
        assert_eq!(cmd["direction"], "up");
        assert_eq!(cmd["amount"], 200);
    }

    #[test]
    fn test_scroll_with_selector() {
        let cmd = parse_command(
            &args("scroll down 500 --selector div.scroll-container"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "scroll");
        assert_eq!(cmd["direction"], "down");
        assert_eq!(cmd["amount"], 500);
        assert_eq!(cmd["selector"], "div.scroll-container");
    }

    #[test]
    fn test_scroll_with_selector_short_flag() {
        let cmd = parse_command(&args("scroll left 100 -s .sidebar"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "scroll");
        assert_eq!(cmd["direction"], "left");
        assert_eq!(cmd["amount"], 100);
        assert_eq!(cmd["selector"], ".sidebar");
    }

    #[test]
    fn test_scroll_at_coordinate() {
        // `--at x,y` carries a [x, y] array for a wheel dispatched at that pixel
        // (issue #36: cross-origin iframe scroll).
        let cmd = parse_command(&args("scroll down 700 --at 640,400"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "scroll");
        assert_eq!(cmd["direction"], "down");
        assert_eq!(cmd["amount"], 700);
        assert_eq!(cmd["at"], json!([640.0, 400.0]));
    }

    #[test]
    fn test_scroll_at_rejects_garbage() {
        assert!(parse_command(&args("scroll --at nope"), &default_flags()).is_err());
        assert!(parse_command(&args("scroll --at 1"), &default_flags()).is_err());
    }

    #[test]
    fn test_scroll_frame_index() {
        let cmd = parse_command(&args("scroll down 700 --frame 2"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "scroll");
        assert_eq!(cmd["frame"], 2);
    }

    #[test]
    fn test_scroll_frame_rejects_non_integer() {
        assert!(parse_command(&args("scroll --frame two"), &default_flags()).is_err());
    }

    #[test]
    fn test_scroll_selector_before_positional() {
        let cmd =
            parse_command(&args("scroll --selector .panel down 400"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "scroll");
        assert_eq!(cmd["direction"], "down");
        assert_eq!(cmd["amount"], 400);
        assert_eq!(cmd["selector"], ".panel");
    }

    #[test]
    fn test_scroll_selector_only() {
        let cmd = parse_command(&args("scroll --selector .content"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "scroll");
        assert_eq!(cmd["direction"], "down");
        assert_eq!(cmd["amount"], 300);
        assert_eq!(cmd["selector"], ".content");
    }

    #[test]
    fn test_scroll_selector_missing_value() {
        let result = parse_command(&args("scroll down 500 --selector"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    // === Inspect / CDP URL ===

    #[test]
    fn test_inspect() {
        let cmd = parse_command(&args("inspect"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "inspect");
    }

    #[test]
    fn test_get_cdp_url() {
        let cmd = parse_command(&args("get cdp-url"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "cdp_url");
    }

    // === Batch Tests ===

    #[test]
    fn test_batch_default() {
        let cmd = parse_command(&args("batch"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "batch");
        assert_eq!(cmd["bail"], false);
    }

    #[test]
    fn test_batch_with_bail() {
        let cmd = parse_command(&args("batch --bail"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "batch");
        assert_eq!(cmd["bail"], true);
    }

    #[test]
    fn test_batch_with_args() {
        let cmd_args = vec![
            "batch".to_string(),
            "open https://example.com".to_string(),
            "screenshot".to_string(),
        ];
        let cmd = parse_command(&cmd_args, &default_flags()).unwrap();
        assert_eq!(cmd["action"], "batch");
        assert_eq!(cmd["bail"], false);
        let commands = cmd["commands"].as_array().unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0], "open https://example.com");
        assert_eq!(commands[1], "screenshot");
    }

    #[test]
    fn test_batch_with_args_and_bail() {
        let cmd_args = vec![
            "batch".to_string(),
            "--bail".to_string(),
            "open https://example.com".to_string(),
            "screenshot".to_string(),
        ];
        let cmd = parse_command(&cmd_args, &default_flags()).unwrap();
        assert_eq!(cmd["action"], "batch");
        assert_eq!(cmd["bail"], true);
        let commands = cmd["commands"].as_array().unwrap();
        assert_eq!(commands.len(), 2);
    }

    #[test]
    fn test_batch_no_args_no_commands_field() {
        let cmd = parse_command(&args("batch"), &default_flags()).unwrap();
        assert!(cmd.get("commands").is_none());
    }

    // === parse_find: friendly error when action verb is missing ===

    #[test]
    fn test_find_role_missing_action_verb_with_name_flag() {
        let err =
            parse_command(&args("find role button --name Submit"), &default_flags()).unwrap_err();
        let msg = err.format();
        assert!(
            msg.contains("Missing action verb"),
            "expected 'Missing action verb' in error, got: {msg}"
        );
        assert!(
            msg.contains("Did you mean"),
            "expected 'Did you mean' suggestion, got: {msg}"
        );
        assert!(
            msg.contains("--name"),
            "error should echo the offending flag back, got: {msg}"
        );
    }

    #[test]
    fn test_find_testid_missing_action_verb_with_exact_flag() {
        let err = parse_command(&args("find testid foo --exact"), &default_flags()).unwrap_err();
        assert!(err.format().contains("Missing action verb"));
    }

    #[test]
    fn test_find_role_with_action_still_works() {
        let cmd = parse_command(
            &args("find role button click --name Submit"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "getbyrole");
        assert_eq!(cmd["subaction"], "click");
        assert_eq!(cmd["name"], "Submit");
    }

    #[test]
    fn test_find_role_default_subaction_click_when_no_action() {
        // Backwards compat: `find role button` (no flags, no action) keeps
        // defaulting to click — only `--xxx` in action position errors.
        let cmd = parse_command(&args("find role button"), &default_flags()).unwrap();
        assert_eq!(cmd["subaction"], "click");
    }

    // === wait --gone / --hidden ===

    #[test]
    fn test_wait_selector_default_visible() {
        let cmd = parse_command(&args("wait .toast"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "wait");
        assert_eq!(cmd["selector"], ".toast");
        assert!(cmd.get("state").is_none(), "default state stays implicit");
    }

    #[test]
    fn test_wait_selector_gone_sets_detached_state() {
        let cmd = parse_command(&args("wait .toast --gone"), &default_flags()).unwrap();
        assert_eq!(cmd["selector"], ".toast");
        assert_eq!(cmd["state"], "detached");
    }

    #[test]
    fn test_wait_selector_hidden_sets_hidden_state() {
        let cmd = parse_command(&args("wait .toast --hidden"), &default_flags()).unwrap();
        assert_eq!(cmd["state"], "hidden");
    }

    #[test]
    fn test_wait_gone_with_timeout() {
        let cmd =
            parse_command(&args("wait .modal --gone --timeout 2000"), &default_flags()).unwrap();
        assert_eq!(cmd["selector"], ".modal");
        assert_eq!(cmd["state"], "detached");
        assert_eq!(cmd["timeout"], 2000);
    }

    #[test]
    fn test_wait_numeric_timeout_still_works() {
        // `wait 500` keeps meaning "sleep 500ms", not "wait for selector 500"
        let cmd = parse_command(&args("wait 500"), &default_flags()).unwrap();
        assert_eq!(cmd["timeout"], 500);
        assert!(cmd.get("selector").is_none());
    }
}
