//! Stdio MCP server exposing a CORE profile of chrome-use tools.
//!
//! Every tool call is delegated to the current binary in `--json` mode: we
//! build the same argument vector a human would type on the command line,
//! run `chrome-use <args> --json` as a child process, and hand its JSON
//! output back as the tool result. No browser logic lives here — this file
//! is purely a JSON-RPC-over-stdio front end onto the existing CLI surface.
//!
//! Only ~12 tools are exposed (open/read/snapshot/click/fill/type/press/eval/
//! wait/back/forward/reload) — a CORE profile, not full CLI parity. Tool
//! argument names/flags are adapted from `commands.rs`'s parse arms for each
//! command, which have diverged from the upstream agent-browser CLI.

use serde_json::{json, Value};
use std::env;
use std::io::{self, BufRead, Read, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const PROTOCOL_VERSION: &str = "2025-06-18";
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

const TOOL_OPEN: &str = "chrome_use_open";
const TOOL_READ: &str = "chrome_use_read";
const TOOL_SNAPSHOT: &str = "chrome_use_snapshot";
const TOOL_CLICK: &str = "chrome_use_click";
const TOOL_FILL: &str = "chrome_use_fill";
const TOOL_TYPE: &str = "chrome_use_type";
const TOOL_PRESS: &str = "chrome_use_press";
const TOOL_EVAL: &str = "chrome_use_eval";
const TOOL_WAIT: &str = "chrome_use_wait";
const TOOL_BACK: &str = "chrome_use_back";
const TOOL_FORWARD: &str = "chrome_use_forward";
const TOOL_RELOAD: &str = "chrome_use_reload";

struct ProtocolError {
    code: i64,
    message: String,
}

impl ProtocolError {
    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
        }
    }

    fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("Method not found: {}", method),
        }
    }
}

struct CliRun {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Run the MCP stdio server until stdin closes or a `shutdown` request lands.
pub fn run_mcp(args: &[String]) -> Result<(), String> {
    if !args.is_empty() {
        return Err(format!(
            "Unknown mcp option(s): {}\nUsage: chrome-use mcp",
            args.join(" ")
        ));
    }

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let mut exit_after_response = false;
        let response = match line {
            Ok(line) if line.trim().is_empty() => None,
            Ok(line) => handle_line(&line, &mut exit_after_response),
            Err(e) => Some(error_response(
                Value::Null,
                -32603,
                format!("Failed to read stdin: {}", e),
            )),
        };

        if let Some(response) = response {
            if write_json_line(&mut stdout, &response).is_err() {
                break;
            }
        }

        if exit_after_response {
            break;
        }
    }

    Ok(())
}

fn handle_line(line: &str, exit_after_response: &mut bool) -> Option<Value> {
    let message: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(e) => {
            return Some(error_response(
                Value::Null,
                -32700,
                format!("Parse error: {}", e),
            ));
        }
    };

    let id = message.get("id").cloned();
    let method = match message.get("method").and_then(|v| v.as_str()) {
        Some(method) => method,
        None => {
            return id.map(|id| error_response(id, -32600, "Invalid request: missing method"));
        }
    };

    // Notifications (no `id`) never get a response.
    let id = id?;

    match handle_request(method, message.get("params"), exit_after_response) {
        Ok(result) => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        })),
        Err(err) => Some(error_response(id, err.code, err.message)),
    }
}

fn handle_request(
    method: &str,
    params: Option<&Value>,
    exit_after_response: &mut bool,
) -> Result<Value, ProtocolError> {
    match method {
        "initialize" => Ok(initialize_result(params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => call_tool(params),
        "shutdown" => {
            *exit_after_response = true;
            Ok(json!({}))
        }
        _ => Err(ProtocolError::method_not_found(method)),
    }
}

fn initialize_result(params: Option<&Value>) -> Value {
    let requested = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(|v| v.as_str())
        .unwrap_or(PROTOCOL_VERSION);
    let protocol_version = if SUPPORTED_PROTOCOL_VERSIONS.contains(&requested) {
        requested
    } else {
        PROTOCOL_VERSION
    };

    json!({
        "protocolVersion": protocol_version,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": "chrome-use",
            "title": "chrome-use",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": "Use the chrome_use_* tools to drive a real browser. Call \
            chrome_use_open to navigate, then chrome_use_snapshot to get an \
            accessibility tree with @ref handles before clicking/typing. This is a \
            CORE tool profile (~12 tools) — not full chrome-use CLI parity.",
    })
}

/// Common optional fields accepted by every tool: `session` (isolated
/// session name, maps to `--session <name>`) and `timeoutMs` (how long to
/// wait for the child `chrome-use` process before killing it).
fn append_common_args(args: &mut Vec<String>, arguments: &Value) -> Result<(), ProtocolError> {
    if let Some(session) = optional_string(arguments, "session")? {
        args.push("--session".to_string());
        args.push(session);
    }
    Ok(())
}

fn tools() -> Vec<Value> {
    let mut common = json!({
        "session": {
            "type": "string",
            "description": "Isolated session name (chrome-use --session <name>); omit to use the default session.",
        },
        "timeoutMs": {
            "type": "integer",
            "description": "Kill the underlying chrome-use process if it hasn't responded after this many ms (default 30000).",
            "minimum": 1,
        },
    });
    let common_props = common.as_object_mut().unwrap().clone();

    let build_schema = |props: serde_json::Map<String, Value>, required: &[&str]| -> Value {
        let mut props = props;
        for (k, v) in common_props.clone() {
            props.insert(k, v);
        }
        let mut schema = json!({ "type": "object", "properties": props });
        if !required.is_empty() {
            schema["required"] = json!(required);
        }
        schema
    };

    vec![
        json!({
            "name": TOOL_OPEN,
            "description": "Navigate to a URL. Omitting `url` just launches the browser and stays on about:blank (useful before setting up cookies/routes).",
            "inputSchema": build_schema(obj(&[
                ("url", json!({ "type": "string", "description": "URL to open. https:// is assumed if no scheme is given." })),
                ("waitUntil", json!({ "type": "string", "enum": ["load", "domcontentloaded", "networkidle", "none"], "description": "Navigation readiness to wait for (default: load)." })),
                ("reuseTab", json!({ "type": "boolean", "description": "Adopt an existing tab already on this URL instead of opening a new one." })),
            ]), &[]),
        }),
        json!({
            "name": TOOL_READ,
            "description": "Fetch a URL (or the active tab, if url is omitted) as agent-readable text/markdown.",
            "inputSchema": build_schema(obj(&[
                ("url", json!({ "type": "string", "description": "URL to read; omit to read the active tab." })),
                ("raw", json!({ "type": "boolean", "description": "Return raw HTML instead of cleaned markdown." })),
                ("requireMd", json!({ "type": "boolean", "description": "Fail instead of falling back to raw text if markdown conversion is unavailable." })),
                ("llms", json!({ "type": "string", "enum": ["index", "full"], "description": "Read from the site's llms.txt instead of the page itself." })),
                ("outline", json!({ "type": "boolean", "description": "Return only the heading outline instead of full content." })),
                ("filter", json!({ "type": "string", "description": "Keep only lines matching this text/regex filter." })),
                ("timeoutMs", json!({ "type": "integer", "description": "Read-specific timeout in ms (distinct from the top-level timeoutMs, which bounds the whole child process)." })),
            ]), &[]),
        }),
        json!({
            "name": TOOL_SNAPSHOT,
            "description": "Accessibility tree of the current page with @ref handles, for locating elements to click/fill/type.",
            "inputSchema": build_schema(obj(&[
                ("interactive", json!({ "type": "boolean", "description": "Only interactive elements (-i)." })),
                ("compact", json!({ "type": "boolean", "description": "Remove empty structural elements (-c)." })),
                ("cursor", json!({ "type": "boolean", "description": "Include the current cursor position marker (-C)." })),
                ("urls", json!({ "type": "boolean", "description": "Include link/image URLs (-u)." })),
                ("depth", json!({ "type": "integer", "description": "Limit tree depth (-d)." })),
                ("selector", json!({ "type": "string", "description": "Scope the snapshot to a CSS selector (-s)." })),
                ("filter", json!({ "type": "string", "description": "Regex: keep only matching lines + ancestor context (-f)." })),
            ]), &[]),
        }),
        json!({
            "name": TOOL_CLICK,
            "description": "Click an element (by CSS selector or @ref) or a raw viewport coordinate. Provide either `selector` or both `x` and `y`.",
            "inputSchema": build_schema(obj(&[
                ("selector", json!({ "type": "string", "description": "CSS selector or @ref to click." })),
                ("x", json!({ "type": "number", "description": "Viewport x coordinate (use with y instead of selector)." })),
                ("y", json!({ "type": "number", "description": "Viewport y coordinate (use with x instead of selector)." })),
                ("newTab", json!({ "type": "boolean", "description": "Report a tab opened by this click without switching to it." })),
                ("follow", json!({ "type": "boolean", "description": "If the click opens a new tab, switch the active tab to it." })),
            ]), &[]),
        }),
        json!({
            "name": TOOL_FILL,
            "description": "Clear and fill a form field (handles CodeMirror/Monaco/ProseMirror/contenteditable in addition to plain inputs).",
            "inputSchema": build_schema(obj(&[
                ("selector", json!({ "type": "string", "description": "CSS selector or @ref of the field." })),
                ("value", json!({ "type": "string", "description": "Text to fill in." })),
            ]), &["selector", "value"]),
        }),
        json!({
            "name": TOOL_TYPE,
            "description": "Type text into an element (or, with no selector, whatever currently has focus).",
            "inputSchema": build_schema(obj(&[
                ("selector", json!({ "type": "string", "description": "CSS selector or @ref to type into; omit to type into the focused element." })),
                ("text", json!({ "type": "string", "description": "Text to type." })),
                ("clear", json!({ "type": "boolean", "description": "Clear the field before typing." })),
                ("delayMs", json!({ "type": "integer", "description": "Per-key delay in ms." })),
                ("keyEvents", json!({ "type": "boolean", "description": "Send real per-character keystrokes instead of a bulk insertText (needed for autocomplete widgets)." })),
                ("enter", json!({ "type": "boolean", "description": "Press Enter after typing (implies keyEvents) to commit an autocomplete candidate." })),
            ]), &["text"]),
        }),
        json!({
            "name": TOOL_PRESS,
            "description": "Press a keyboard key (e.g. Enter, Tab, Control+a).",
            "inputSchema": build_schema(obj(&[
                ("key", json!({ "type": "string", "description": "Key or chord to press, e.g. \"Enter\" or \"Control+a\"." })),
                ("holdMs", json!({ "type": "integer", "description": "Hold the key down this long (ms) before releasing." })),
            ]), &["key"]),
        }),
        json!({
            "name": TOOL_EVAL,
            "description": "Run JavaScript in the page's main world (state persists, page handlers fire) and return its result.",
            "inputSchema": build_schema(obj(&[
                ("script", json!({ "type": "string", "description": "JavaScript to evaluate." })),
                ("frame", json!({ "type": "string", "description": "Run in this frame's context instead of the main frame (CSS selector, @ref, URL substring, or frame index)." })),
            ]), &["script"]),
        }),
        json!({
            "name": TOOL_WAIT,
            "description": "Wait for an element, a fixed duration, a URL, a load state, a JS condition, text, or a download. Provide exactly one of: selector, ms, url, loadState, expression, text, download.",
            "inputSchema": build_schema(obj(&[
                ("selector", json!({ "type": "string", "description": "Wait for this CSS selector/@ref to appear (or disappear, with gone/hidden)." })),
                ("gone", json!({ "type": "boolean", "description": "With selector: wait for it to leave the DOM instead of appearing." })),
                ("hidden", json!({ "type": "boolean", "description": "With selector: wait for it to become invisible instead of appearing." })),
                ("ms", json!({ "type": "integer", "description": "Wait this many milliseconds." })),
                ("url", json!({ "type": "string", "description": "Wait for the page URL to match this glob pattern." })),
                ("loadState", json!({ "type": "string", "enum": ["load", "domcontentloaded", "networkidle"], "description": "Wait for this load state." })),
                ("expression", json!({ "type": "string", "description": "Wait for this JS expression to become truthy." })),
                ("text", json!({ "type": "string", "description": "Wait for this text to appear on the page." })),
                ("download", json!({ "type": "boolean", "description": "Wait for a download to complete." })),
                ("downloadPath", json!({ "type": "string", "description": "With download: save it to this path." })),
                ("waitTimeoutMs", json!({ "type": "integer", "description": "How long to wait before giving up (distinct from the top-level timeoutMs)." })),
            ]), &[]),
        }),
        json!({
            "name": TOOL_BACK,
            "description": "Go back in browser history.",
            "inputSchema": build_schema(obj(&[]), &[]),
        }),
        json!({
            "name": TOOL_FORWARD,
            "description": "Go forward in browser history.",
            "inputSchema": build_schema(obj(&[]), &[]),
        }),
        json!({
            "name": TOOL_RELOAD,
            "description": "Reload the current page.",
            "inputSchema": build_schema(obj(&[]), &[]),
        }),
    ]
}

fn obj(pairs: &[(&str, Value)]) -> serde_json::Map<String, Value> {
    let mut map = serde_json::Map::new();
    for (k, v) in pairs {
        map.insert(k.to_string(), v.clone());
    }
    map
}

fn is_known_tool(name: &str) -> bool {
    matches!(
        name,
        TOOL_OPEN
            | TOOL_READ
            | TOOL_SNAPSHOT
            | TOOL_CLICK
            | TOOL_FILL
            | TOOL_TYPE
            | TOOL_PRESS
            | TOOL_EVAL
            | TOOL_WAIT
            | TOOL_BACK
            | TOOL_FORWARD
            | TOOL_RELOAD
    )
}

fn call_tool(params: Option<&Value>) -> Result<Value, ProtocolError> {
    let params =
        params.ok_or_else(|| ProtocolError::invalid_params("tools/call requires params"))?;
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProtocolError::invalid_params("tools/call params.name must be a string"))?;
    let arguments = params.get("arguments").unwrap_or(&Value::Null);
    validate_arguments_object(arguments)?;

    if !is_known_tool(name) {
        return Err(ProtocolError::invalid_params(format!(
            "Unknown tool: {} (this MCP server only exposes the core chrome_use_* profile)",
            name
        )));
    }

    match name {
        TOOL_OPEN => call_open(arguments),
        TOOL_READ => call_read(arguments),
        TOOL_SNAPSHOT => call_snapshot(arguments),
        TOOL_CLICK => call_click(arguments),
        TOOL_FILL => call_fill(arguments),
        TOOL_TYPE => call_type(arguments),
        TOOL_PRESS => call_press(arguments),
        TOOL_EVAL => call_eval(arguments),
        TOOL_WAIT => call_wait(arguments),
        TOOL_BACK => call_literal(arguments, &["back"]),
        TOOL_FORWARD => call_literal(arguments, &["forward"]),
        TOOL_RELOAD => call_literal(arguments, &["reload"]),
        _ => unreachable!("known MCP tool missing call handler: {}", name),
    }
}

fn call_literal(arguments: &Value, parts: &[&str]) -> Result<Value, ProtocolError> {
    run_tool(arguments, parts.iter().map(|s| s.to_string()).collect())
}

fn call_open(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["open".to_string()];
    if let Some(url) = optional_string(arguments, "url")? {
        if !url.is_empty() {
            args.push(url);
        }
    }
    if let Some(wait_until) = optional_string(arguments, "waitUntil")? {
        args.push("--wait-until".to_string());
        args.push(wait_until);
    }
    if optional_bool(arguments, "reuseTab")?.unwrap_or(false) {
        args.push("--reuse-tab".to_string());
    }
    run_tool(arguments, args)
}

fn call_read(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["read".to_string()];
    if optional_bool(arguments, "raw")?.unwrap_or(false) {
        args.push("--raw".to_string());
    }
    if optional_bool(arguments, "requireMd")?.unwrap_or(false) {
        args.push("--require-md".to_string());
    }
    if let Some(llms) = optional_string(arguments, "llms")? {
        args.push("--llms".to_string());
        args.push(llms);
    }
    if optional_bool(arguments, "outline")?.unwrap_or(false) {
        args.push("--outline".to_string());
    }
    if let Some(filter) = optional_string(arguments, "filter")? {
        args.push("--filter".to_string());
        args.push(filter);
    }
    if let Some(timeout) = optional_u64(arguments, "timeoutMs")? {
        args.push("--timeout".to_string());
        args.push(timeout.to_string());
    }
    if let Some(url) = optional_string(arguments, "url")? {
        if !url.is_empty() {
            args.push(url);
        }
    }
    run_tool(arguments, args)
}

fn call_snapshot(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["snapshot".to_string()];
    if optional_bool(arguments, "interactive")?.unwrap_or(false) {
        args.push("-i".to_string());
    }
    if optional_bool(arguments, "compact")?.unwrap_or(false) {
        args.push("-c".to_string());
    }
    if optional_bool(arguments, "cursor")?.unwrap_or(false) {
        args.push("-C".to_string());
    }
    if optional_bool(arguments, "urls")?.unwrap_or(false) {
        args.push("-u".to_string());
    }
    if let Some(depth) = optional_u64(arguments, "depth")? {
        args.push("-d".to_string());
        args.push(depth.to_string());
    }
    if let Some(selector) = optional_string(arguments, "selector")? {
        args.push("-s".to_string());
        args.push(selector);
    }
    if let Some(filter) = optional_string(arguments, "filter")? {
        args.push("-f".to_string());
        args.push(filter);
    }
    run_tool(arguments, args)
}

fn call_click(arguments: &Value) -> Result<Value, ProtocolError> {
    let selector = optional_string(arguments, "selector")?;
    let x = optional_number_string(arguments, "x")?;
    let y = optional_number_string(arguments, "y")?;

    let mut args = vec!["click".to_string()];
    match (selector, x, y) {
        (Some(sel), _, _) => args.push(sel),
        (None, Some(x), Some(y)) => {
            args.push(x);
            args.push(y);
        }
        _ => {
            return Err(ProtocolError::invalid_params(
                "chrome_use_click requires either `selector` or both `x` and `y`",
            ))
        }
    }
    if optional_bool(arguments, "newTab")?.unwrap_or(false) {
        args.push("--new-tab".to_string());
    }
    if optional_bool(arguments, "follow")?.unwrap_or(false) {
        args.push("--follow".to_string());
    }
    run_tool(arguments, args)
}

fn call_fill(arguments: &Value) -> Result<Value, ProtocolError> {
    let selector = required_string(arguments, "selector")?;
    let value = required_string(arguments, "value")?;
    run_tool(arguments, vec!["fill".to_string(), selector, value])
}

fn call_type(arguments: &Value) -> Result<Value, ProtocolError> {
    let text = required_string(arguments, "text")?;
    let mut args = vec!["type".to_string()];
    match optional_string(arguments, "selector")? {
        Some(selector) => args.push(selector),
        None => args.push("--focused".to_string()),
    }
    args.push(text);
    if optional_bool(arguments, "clear")?.unwrap_or(false) {
        args.push("--clear".to_string());
    }
    if let Some(delay) = optional_u64(arguments, "delayMs")? {
        args.push("--delay".to_string());
        args.push(delay.to_string());
    }
    if optional_bool(arguments, "keyEvents")?.unwrap_or(false) {
        args.push("--key-events".to_string());
    }
    if optional_bool(arguments, "enter")?.unwrap_or(false) {
        args.push("--enter".to_string());
    }
    run_tool(arguments, args)
}

fn call_press(arguments: &Value) -> Result<Value, ProtocolError> {
    let key = required_string(arguments, "key")?;
    let mut args = vec!["press".to_string(), key];
    if let Some(hold) = optional_u64(arguments, "holdMs")? {
        args.push("--hold".to_string());
        args.push(hold.to_string());
    }
    run_tool(arguments, args)
}

fn call_eval(arguments: &Value) -> Result<Value, ProtocolError> {
    let script = required_string(arguments, "script")?;
    let mut args = vec!["eval".to_string()];
    if let Some(frame) = optional_string(arguments, "frame")? {
        args.push("--frame".to_string());
        args.push(frame);
    }
    // `script` is passed as a single argv element (no shell in between), so
    // it needs no escaping/base64/--stdin trick even if it contains spaces,
    // quotes, or newlines.
    args.push(script);
    run_tool(arguments, args)
}

fn call_wait(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["wait".to_string()];

    if let Some(url) = optional_string(arguments, "url")? {
        args.push("--url".to_string());
        args.push(url);
    } else if let Some(state) = optional_string(arguments, "loadState")? {
        args.push("--load".to_string());
        args.push(state);
    } else if let Some(expr) = optional_string(arguments, "expression")? {
        args.push("--fn".to_string());
        args.push(expr);
    } else if let Some(text) = optional_string(arguments, "text")? {
        args.push("--text".to_string());
        args.push(text);
    } else if optional_bool(arguments, "download")?.unwrap_or(false) {
        args.push("--download".to_string());
        if let Some(path) = optional_string(arguments, "downloadPath")? {
            args.push(path);
        }
    } else if let Some(selector) = optional_string(arguments, "selector")? {
        args.push(selector);
        if optional_bool(arguments, "gone")?.unwrap_or(false) {
            args.push("--gone".to_string());
        } else if optional_bool(arguments, "hidden")?.unwrap_or(false) {
            args.push("--hidden".to_string());
        }
    } else if let Some(ms) = optional_u64(arguments, "ms")? {
        args.push(ms.to_string());
    } else {
        return Err(ProtocolError::invalid_params(
            "chrome_use_wait requires one of: selector, ms, url, loadState, expression, text, download",
        ));
    }

    if let Some(timeout) = optional_u64(arguments, "waitTimeoutMs")? {
        args.push("--timeout".to_string());
        args.push(timeout.to_string());
    }
    run_tool(arguments, args)
}

/// Build the final argv (command args + `--session`/global flags + `--json`)
/// and run it as a child `chrome-use` process.
fn run_tool(arguments: &Value, mut args: Vec<String>) -> Result<Value, ProtocolError> {
    append_common_args(&mut args, arguments)?;
    args.push("--json".to_string());
    let timeout_ms = optional_u64(arguments, "timeoutMs")?.unwrap_or(DEFAULT_TIMEOUT_MS);

    let run = run_cli(&args, timeout_ms)
        .map_err(|e| ProtocolError::invalid_params(format!("Failed to run chrome-use: {}", e)))?;
    Ok(tool_result_from_run(run))
}

fn validate_arguments_object(arguments: &Value) -> Result<(), ProtocolError> {
    if arguments.is_null() || arguments.is_object() {
        Ok(())
    } else {
        Err(ProtocolError::invalid_params(
            "tool arguments must be an object",
        ))
    }
}

fn optional_value<'a>(arguments: &'a Value, key: &str) -> Option<&'a Value> {
    arguments.get(key)
}

fn required_string(arguments: &Value, key: &str) -> Result<String, ProtocolError> {
    optional_value(arguments, key)
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
        .ok_or_else(|| ProtocolError::invalid_params(format!("{} must be a string", key)))
}

fn optional_string(arguments: &Value, key: &str) -> Result<Option<String>, ProtocolError> {
    match optional_value(arguments, key) {
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(ProtocolError::invalid_params(format!(
            "{} must be a string",
            key
        ))),
    }
}

fn optional_bool(arguments: &Value, key: &str) -> Result<Option<bool>, ProtocolError> {
    match optional_value(arguments, key) {
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(ProtocolError::invalid_params(format!(
            "{} must be a boolean",
            key
        ))),
    }
}

fn optional_u64(arguments: &Value, key: &str) -> Result<Option<u64>, ProtocolError> {
    match optional_value(arguments, key) {
        Some(Value::Null) | None => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| {
            ProtocolError::invalid_params(format!("{} must be a non-negative integer", key))
        }),
    }
}

fn optional_number_string(arguments: &Value, key: &str) -> Result<Option<String>, ProtocolError> {
    match optional_value(arguments, key) {
        Some(Value::Null) | None => Ok(None),
        Some(value) => {
            if let Some(n) = value.as_i64() {
                Ok(Some(n.to_string()))
            } else if let Some(n) = value.as_f64() {
                Ok(Some(n.to_string()))
            } else {
                Err(ProtocolError::invalid_params(format!(
                    "{} must be a number",
                    key
                )))
            }
        }
    }
}

fn run_cli(args: &[String], timeout_ms: u64) -> Result<CliRun, String> {
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;

    let mut child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to open child stdout".to_string())?;
    let mut child_stderr = child
        .stderr
        .take()
        .ok_or_else(|| "failed to open child stderr".to_string())?;

    let stdout_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        child_stdout.read_to_end(&mut buf).map(|_| buf)
    });
    let stderr_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        child_stderr.read_to_end(&mut buf).map(|_| buf)
    });

    let started = Instant::now();
    let timeout = Duration::from_millis(timeout_ms);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let stdout = join_output(stdout_thread)?;
                    let stderr = join_output(stderr_thread)?;
                    return Ok(CliRun {
                        exit_code: None,
                        stdout,
                        stderr: append_timeout_message(stderr, timeout_ms),
                    });
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e.to_string()),
        }
    };

    Ok(CliRun {
        exit_code: status.code(),
        stdout: join_output(stdout_thread)?,
        stderr: join_output(stderr_thread)?,
    })
}

fn join_output(handle: thread::JoinHandle<io::Result<Vec<u8>>>) -> Result<String, String> {
    let bytes = handle
        .join()
        .map_err(|_| "failed to join output reader".to_string())?
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn append_timeout_message(stderr: String, timeout_ms: u64) -> String {
    let msg = format!("chrome-use command timed out after {}ms", timeout_ms);
    if stderr.trim().is_empty() {
        msg
    } else {
        format!("{}\n{}", stderr.trim_end(), msg)
    }
}

fn tool_result_from_run(run: CliRun) -> Value {
    let parsed = serde_json::from_str::<Value>(run.stdout.trim()).ok();
    let cli_success = parsed
        .as_ref()
        .and_then(|v| v.get("success"))
        .and_then(|v| v.as_bool())
        .unwrap_or_else(|| run.exit_code == Some(0));
    let success = run.exit_code == Some(0) && cli_success;

    json!({
        "content": [{
            "type": "text",
            "text": tool_text(parsed.as_ref(), &run.stdout, &run.stderr),
        }],
        "structuredContent": {
            "exitCode": run.exit_code,
            "stdout": run.stdout,
            "stderr": run.stderr,
            "response": parsed,
        },
        "isError": !success,
    })
}

fn tool_text(parsed: Option<&Value>, stdout: &str, stderr: &str) -> String {
    let mut text = match parsed {
        Some(value) => {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| stdout.trim().to_string())
        }
        None => stdout.trim().to_string(),
    };

    let stderr = stderr.trim();
    if !stderr.is_empty() {
        if !text.is_empty() {
            text.push_str("\n\nstderr:\n");
        }
        text.push_str(stderr);
    }

    if text.is_empty() {
        "(no output)".to_string()
    } else {
        text
    }
}

fn error_response(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message.into(),
        },
    })
}

fn write_json_line(stdout: &mut io::Stdout, value: &Value) -> io::Result<()> {
    let line = serde_json::to_string(value)?;
    stdout.write_all(line.as_bytes())?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}
