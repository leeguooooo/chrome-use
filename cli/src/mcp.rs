//! Stdio MCP server exposing chrome-use tools in one of two profiles.
//!
//! Every tool call is delegated to the current binary in `--json` mode: we
//! build the same argument vector a human would type on the command line,
//! run `chrome-use <args> --json` as a child process, and hand its JSON
//! output back as the tool result. No browser logic lives here — this file
//! is purely a JSON-RPC-over-stdio front end onto the existing CLI surface.
//!
//! Two profiles, selected with `chrome-use mcp [--tools <core|all>]`:
//!   - `core` (default): ~12 tools (open/read/snapshot/click/fill/type/press/
//!     eval/wait/back/forward/reload).
//!   - `all`: `core` plus an extended set (hover/select/screenshot/scroll/
//!     tabs/extract/expect/find/network_route/site/upload/download) — closer
//!     to full CLI parity, still not exhaustive.
//!
//! Tool argument names/flags are adapted from `commands.rs`'s parse arms for
//! each command, which have diverged from the upstream agent-browser CLI.

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

// Extended profile (`--tools all`) — see module docs above.
const TOOL_HOVER: &str = "chrome_use_hover";
const TOOL_SELECT: &str = "chrome_use_select";
const TOOL_SCREENSHOT: &str = "chrome_use_screenshot";
const TOOL_SCROLL: &str = "chrome_use_scroll";
const TOOL_TABS: &str = "chrome_use_tabs";
const TOOL_EXTRACT: &str = "chrome_use_extract";
const TOOL_EXPECT: &str = "chrome_use_expect";
const TOOL_FIND: &str = "chrome_use_find";
const TOOL_NETWORK_ROUTE: &str = "chrome_use_network_route";
const TOOL_SITE: &str = "chrome_use_site";
const TOOL_UPLOAD: &str = "chrome_use_upload";
const TOOL_DOWNLOAD: &str = "chrome_use_download";
const TOOL_DOWNLOAD_URL: &str = "chrome_use_download_url";
const TOOL_DOWNLOADS: &str = "chrome_use_downloads";

/// Which set of `chrome_use_*` tools this server instance exposes, chosen at
/// startup via `chrome-use mcp --tools <core|all>` (default `core`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Profile {
    Core,
    All,
}

impl Profile {
    /// Parse `--tools <core|all>` from the args after `mcp`. Anything else is
    /// an unknown option (mirrors the old "no args accepted" behavior for the
    /// common case of a typo/extra flag).
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut profile = Profile::Core;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--tools" => {
                    let value = args
                        .get(i + 1)
                        .ok_or_else(|| "--tools requires a value: core or all".to_string())?;
                    profile = match value.as_str() {
                        "core" => Profile::Core,
                        "all" => Profile::All,
                        other => {
                            return Err(format!(
                                "Unknown --tools value: {} (expected \"core\" or \"all\")",
                                other
                            ))
                        }
                    };
                    i += 2;
                }
                other => {
                    return Err(format!(
                        "Unknown mcp option(s): {}\nUsage: chrome-use mcp [--tools <core|all>]",
                        other
                    ));
                }
            }
        }
        Ok(profile)
    }

    fn is_all(&self) -> bool {
        matches!(self, Profile::All)
    }
}

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
    let profile = Profile::parse(args)?;

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let mut exit_after_response = false;
        let response = match line {
            Ok(line) if line.trim().is_empty() => None,
            Ok(line) => handle_line(&line, &mut exit_after_response, profile),
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

fn handle_line(line: &str, exit_after_response: &mut bool, profile: Profile) -> Option<Value> {
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

    match handle_request(method, message.get("params"), exit_after_response, profile) {
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
    profile: Profile,
) -> Result<Value, ProtocolError> {
    match method {
        "initialize" => Ok(initialize_result(params, profile)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools_for(profile) })),
        "tools/call" => call_tool(params, profile),
        "shutdown" => {
            *exit_after_response = true;
            Ok(json!({}))
        }
        _ => Err(ProtocolError::method_not_found(method)),
    }
}

fn initialize_result(params: Option<&Value>, profile: Profile) -> Value {
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
        "instructions": if profile.is_all() {
            "Use the chrome_use_* tools to drive a real browser. Call chrome_use_open to \
            navigate, then chrome_use_snapshot to get an accessibility tree with @ref \
            handles before clicking/typing. This is the ALL tool profile (core + extended \
            set: hover/select/screenshot/scroll/tabs/extract/expect/find/network_route/\
            site/upload/download) — still not exhaustive CLI parity."
        } else {
            "Use the chrome_use_* tools to drive a real browser. Call chrome_use_open to \
            navigate, then chrome_use_snapshot to get an accessibility tree with @ref \
            handles before clicking/typing. This is the CORE tool profile (~12 tools) — \
            not full chrome-use CLI parity. Start the server with --tools all for the \
            extended profile."
        },
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

/// Tools for the active profile: `core` always; `all` adds the extended set.
fn tools_for(profile: Profile) -> Vec<Value> {
    let mut tools = core_tools();
    if profile.is_all() {
        tools.extend(extended_tools());
    }
    tools
}

/// Build a tool's `inputSchema`, merging in the fields every tool accepts
/// (`session`, `timeoutMs`).
fn build_schema(props: serde_json::Map<String, Value>, required: &[&str]) -> Value {
    let mut props = props;
    props.insert(
        "session".to_string(),
        json!({
            "type": "string",
            "description": "Isolated session name (chrome-use --session <name>); omit to use the default session.",
        }),
    );
    props.insert(
        "timeoutMs".to_string(),
        json!({
            "type": "integer",
            "description": "Kill the underlying chrome-use process if it hasn't responded after this many ms (default 30000).",
            "minimum": 1,
        }),
    );
    let mut schema = json!({ "type": "object", "properties": props });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema
}

fn core_tools() -> Vec<Value> {
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

/// Extended profile (`--tools all`) — see module docs above. Each maps onto
/// the CLI command/flags named in its own doc comment below.
fn extended_tools() -> Vec<Value> {
    vec![
        json!({
            "name": TOOL_HOVER,
            "description": "Hover the pointer over an element.",
            "inputSchema": build_schema(obj(&[
                ("selector", json!({ "type": "string", "description": "CSS selector or @ref to hover." })),
            ]), &["selector"]),
        }),
        json!({
            "name": TOOL_SELECT,
            "description": "Select one or more options in a <select> element.",
            "inputSchema": build_schema(obj(&[
                ("selector", json!({ "type": "string", "description": "CSS selector or @ref of the <select>." })),
                ("values", json!({ "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "Option value(s) to select (multiple for a multi-select)." })),
            ]), &["selector", "values"]),
        }),
        json!({
            "name": TOOL_SCREENSHOT,
            "description": "Capture a screenshot of the page or one element.",
            "inputSchema": build_schema(obj(&[
                ("selector", json!({ "type": "string", "description": "CSS selector or @ref to screenshot; omit to capture the viewport/page." })),
                ("path", json!({ "type": "string", "description": "Save to this file path (should contain / or end in .png/.jpg/.jpeg/.webp so it isn't mistaken for a selector) instead of returning the image inline." })),
                ("fullPage", json!({ "type": "boolean", "description": "Capture the full scrollable page instead of just the viewport (--full)." })),
                ("clip", json!({
                    "type": "object",
                    "description": "Capture only this pixel region (--clip x,y,w,h).",
                    "properties": {
                        "x": { "type": "number" },
                        "y": { "type": "number" },
                        "width": { "type": "number" },
                        "height": { "type": "number" },
                    },
                    "required": ["x", "y", "width", "height"],
                })),
                ("maxWidth", json!({ "type": "integer", "description": "Downscale the saved image to this max width in px (--max-width)." })),
                ("maxHeight", json!({ "type": "integer", "description": "Downscale the saved image to this max height in px (--max-height)." })),
                ("scale", json!({ "type": "number", "description": "Downscale the saved image by this factor, (0, 1] (--scale)." })),
            ]), &[]),
        }),
        json!({
            "name": TOOL_SCROLL,
            "description": "Scroll the page, an element, or a specific viewport point/frame.",
            "inputSchema": build_schema(obj(&[
                ("direction", json!({ "type": "string", "enum": ["up", "down", "left", "right"], "description": "Scroll direction (default: down)." })),
                ("amount", json!({ "type": "integer", "description": "Pixels to scroll (default: 300)." })),
                ("selector", json!({ "type": "string", "description": "Scroll this element into/within view instead of the page (--selector)." })),
                ("at", json!({
                    "type": "object",
                    "description": "Dispatch the wheel event at this viewport point, reaching whatever is under it (incl. cross-origin iframes) (--at x,y).",
                    "properties": { "x": { "type": "number" }, "y": { "type": "number" } },
                    "required": ["x", "y"],
                })),
                ("frame", json!({ "type": "integer", "description": "Scroll the n-th frame from chrome_use snapshot/frames by index (--frame)." })),
            ]), &[]),
        }),
        json!({
            "name": TOOL_TABS,
            "description": "List, open, duplicate, close, or switch browser tabs.",
            "inputSchema": build_schema(obj(&[
                ("action", json!({ "type": "string", "enum": ["list", "new", "duplicate", "close", "switch"], "description": "Tab operation to perform." })),
                ("url", json!({ "type": "string", "description": "With action=new: URL to open in the new tab." })),
                ("label", json!({ "type": "string", "description": "With action=new or duplicate: assign this label to the new tab (--label)." })),
                ("tabId", json!({ "type": "string", "description": "Tab id, label, or stable target ID. Required for action=switch; optional for action=close or duplicate (defaults to current tab)." })),
                ("activate", json!({ "type": "boolean", "description": "With action=switch: also raise the tab to the foreground (--activate)." })),
                ("full", json!({ "type": "boolean", "description": "With action=list: emit untruncated tab URLs (--full)." })),
            ]), &["action"]),
        }),
        json!({
            "name": TOOL_EXTRACT,
            "description": "Scrape structured JSON from the page in one call via a declarative schema (rows + fields), instead of N find/get round-trips.",
            "inputSchema": build_schema(obj(&[
                ("schema", json!({
                    "type": "object",
                    "description": "{ rows?: <css selector for repeating containers>, fields: { name: <css> | { sel, get: 'text'|'@attr'|'html'|'value', all? } } }. Omitting rows returns one object for the whole document.",
                    "properties": { "fields": { "type": "object" } },
                    "required": ["fields"],
                })),
                ("frame", json!({ "type": "string", "description": "Scope extraction to this child frame (index, URL substring, @ref, or CSS selector) (--frame)." })),
            ]), &["schema"]),
        }),
        json!({
            "name": TOOL_EXPECT,
            "description": "Assert a condition and get a pass/fail result (polls up to a timeout). The \"did it actually work?\" gate.",
            "inputSchema": build_schema(obj(&[
                ("kind", json!({ "type": "string", "enum": ["element", "count", "text", "value", "attr", "url", "request", "no-errors"], "description": "Which condition grammar to use." })),
                ("selector", json!({ "type": "string", "description": "CSS selector or @ref. Required for kind=element/count/text/value/attr." })),
                ("state", json!({ "type": "string", "enum": ["visible", "hidden", "gone", "present"], "description": "With kind=element: the expected element state." })),
                ("op", json!({ "type": "string", "enum": ["==", "!=", ">", "<", ">=", "<="], "description": "With kind=count: comparison operator." })),
                ("count", json!({ "type": "integer", "description": "With kind=count: the number to compare against." })),
                ("predicate", json!({ "type": "string", "enum": ["equals", "contains", "matches"], "description": "With kind=text/value/attr/url: how to compare against `expected` (matches = glob, unless regex=true)." })),
                ("expected", json!({ "type": "string", "description": "With kind=text/value/attr/url: the expected string/pattern." })),
                ("attr", json!({ "type": "string", "description": "With kind=attr: the attribute name to read." })),
                ("filter", json!({ "type": "string", "description": "With kind=request: URL substring to match a captured request." })),
                ("method", json!({ "type": "string", "description": "With kind=request: filter by HTTP method (--method)." })),
                ("status", json!({ "type": "string", "description": "With kind=request: filter by status, e.g. \"2xx\" (--status)." })),
                ("type", json!({ "type": "string", "description": "With kind=request: filter by resource type, e.g. xhr (--type)." })),
                ("requestCount", json!({ "type": "integer", "description": "With kind=request: expected number of matching requests (--count)." })),
                ("regex", json!({ "type": "boolean", "description": "Treat `expected` as a raw regex instead of equals/contains/glob (--regex)." })),
                ("not", json!({ "type": "boolean", "description": "Invert the result: pass when the condition is false (--not)." })),
                ("noWait", json!({ "type": "boolean", "description": "Evaluate once instead of polling until timeout (--no-wait)." })),
                ("waitTimeoutMs", json!({ "type": "integer", "description": "How long to poll before giving up (distinct from the top-level timeoutMs) (--timeout)." })),
            ]), &["kind"]),
        }),
        json!({
            "name": TOOL_FIND,
            "description": "Find an element by a semantic locator (role/text/label/etc.) and optionally act on it, without needing a snapshot first.",
            "inputSchema": build_schema(obj(&[
                ("locator", json!({ "type": "string", "enum": ["role", "text", "label", "placeholder", "alt", "title", "testid", "first", "last", "nth"], "description": "Locator kind." })),
                ("value", json!({ "type": "string", "description": "The locator's value (role name, text, label, etc.), or the CSS selector for first/last/nth." })),
                ("index", json!({ "type": "integer", "description": "With locator=nth: 0-based match index." })),
                ("action", json!({ "type": "string", "enum": ["click", "fill", "type", "hover", "focus", "check", "uncheck"], "description": "Action to perform on the match (default: click)." })),
                ("text", json!({ "type": "string", "description": "With action=fill/type: the text to enter." })),
                ("name", json!({ "type": "string", "description": "With locator=role: filter by accessible name (--name)." })),
                ("exact", json!({ "type": "boolean", "description": "Require an exact (not substring) match (--exact); supported for text/label/placeholder/alt/title." })),
            ]), &["locator", "value"]),
        }),
        json!({
            "name": TOOL_NETWORK_ROUTE,
            "description": "Intercept network requests: abort, mock a response, rewrite an outgoing request, or edit a real response.",
            "inputSchema": build_schema(obj(&[
                ("action", json!({ "type": "string", "enum": ["route", "unroute"], "description": "route = install/replace an interceptor; unroute = remove one (or all, if url omitted)." })),
                ("url", json!({ "type": "string", "description": "URL glob pattern to match. Required for action=route; optional for action=unroute (omit to remove all routes)." })),
                ("abort", json!({ "type": "boolean", "description": "Abort matching requests (--abort)." })),
                ("body", json!({ "type": "string", "description": "Mock response body (--body)." })),
                ("status", json!({ "type": "integer", "description": "Mock response status code (--status)." })),
                ("contentType", json!({ "type": "string", "description": "Mock response Content-Type (--content-type)." })),
                ("headers", json!({ "type": "object", "additionalProperties": { "type": "string" }, "description": "Mock response headers (--header K=V, repeated)." })),
                ("setBody", json!({ "type": "string", "description": "Rewrite the outgoing request body (--set-body)." })),
                ("method", json!({ "type": "string", "description": "Rewrite the outgoing request method (--method)." })),
                ("rewriteUrl", json!({ "type": "string", "description": "Rewrite the outgoing request URL (--rewrite-url)." })),
                ("setHeaders", json!({ "type": "object", "additionalProperties": { "type": "string" }, "description": "Rewrite/add outgoing request headers (--set-header K=V, repeated)." })),
                ("editStatus", json!({ "type": "integer", "description": "Overwrite the REAL response's status code (--edit-status)." })),
                ("editHeaders", json!({ "type": "object", "additionalProperties": { "type": "string" }, "description": "Overwrite/add headers on the REAL response (--edit-header K=V, repeated)." })),
                ("replacements", json!({
                    "type": "array",
                    "description": "Text replacements applied to the REAL response body (--replace from=>to, repeated).",
                    "items": {
                        "type": "array", "items": { "type": "string" }, "minItems": 2, "maxItems": 2,
                    },
                })),
                ("resourceType", json!({ "type": "string", "description": "Only match this resource type (comma-separated, e.g. xhr,fetch) (--resource-type)." })),
            ]), &["action"]),
        }),
        json!({
            "name": TOOL_SITE,
            "description": "Run a community site adapter: navigates to the adapter's domain (reusing the tab if already there) and returns structured JSON. See `chrome-use site list` for available adapters.",
            "inputSchema": build_schema(obj(&[
                ("spec", json!({ "type": "string", "description": "Adapter spec \"<name>/<command>\", e.g. \"github/repo\"." })),
                ("args", json!({ "type": "array", "items": { "type": "string" }, "description": "Positional arguments for the adapter command." })),
                ("namedArgs", json!({ "type": "object", "additionalProperties": { "type": "string" }, "description": "Named arguments for the adapter command, passed as --key value pairs." })),
            ]), &["spec"]),
        }),
        json!({
            "name": TOOL_UPLOAD,
            "description": "Attach one or more local files to a file input.",
            "inputSchema": build_schema(obj(&[
                ("selector", json!({ "type": "string", "description": "CSS selector or @ref of the file input." })),
                ("files", json!({ "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "Local file path(s) to attach." })),
            ]), &["selector", "files"]),
        }),
        json!({
            "name": TOOL_DOWNLOAD,
            "description": "Download from an element and save the result to a path. HTTP(S) links use Chrome's downloads API when ab-connect supports it.",
            "inputSchema": build_schema(obj(&[
                ("selector", json!({ "type": "string", "description": "CSS selector or @ref that triggers the download." })),
                ("path", json!({ "type": "string", "description": "Local file path to save the download to." })),
            ]), &["selector", "path"]),
        }),
        json!({
            "name": TOOL_DOWNLOAD_URL,
            "description": "Start an HTTP(S) download through the connected Chrome profile.",
            "inputSchema": build_schema(obj(&[
                ("url", json!({ "type": "string", "description": "HTTP(S) URL to download." })),
                ("path", json!({ "type": "string", "description": "Optional local destination. Omit to keep Chrome's normal download path." })),
            ]), &["url"]),
        }),
        json!({
            "name": TOOL_DOWNLOADS,
            "description": "List recent Chrome downloads or clear download history without deleting files.",
            "inputSchema": build_schema(obj(&[
                ("limit", json!({ "type": "integer", "minimum": 1, "maximum": 100, "description": "Maximum entries to return." })),
                ("clear", json!({ "type": "boolean", "description": "Erase Chrome download history instead of listing it." })),
            ]), &[]),
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

fn is_known_tool(name: &str, profile: Profile) -> bool {
    let core = matches!(
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
    );
    if core {
        return true;
    }
    profile.is_all()
        && matches!(
            name,
            TOOL_HOVER
                | TOOL_SELECT
                | TOOL_SCREENSHOT
                | TOOL_SCROLL
                | TOOL_TABS
                | TOOL_EXTRACT
                | TOOL_EXPECT
                | TOOL_FIND
                | TOOL_NETWORK_ROUTE
                | TOOL_SITE
                | TOOL_UPLOAD
                | TOOL_DOWNLOAD
                | TOOL_DOWNLOAD_URL
                | TOOL_DOWNLOADS
        )
}

fn call_tool(params: Option<&Value>, profile: Profile) -> Result<Value, ProtocolError> {
    let params =
        params.ok_or_else(|| ProtocolError::invalid_params("tools/call requires params"))?;
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProtocolError::invalid_params("tools/call params.name must be a string"))?;
    let arguments = params.get("arguments").unwrap_or(&Value::Null);
    validate_arguments_object(arguments)?;

    if !is_known_tool(name, profile) {
        return Err(ProtocolError::invalid_params(format!(
            "Unknown tool: {} (this MCP server exposes the {} chrome_use_* profile)",
            name,
            if profile.is_all() { "all" } else { "core" }
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
        TOOL_HOVER => call_hover(arguments),
        TOOL_SELECT => call_select(arguments),
        TOOL_SCREENSHOT => call_screenshot(arguments),
        TOOL_SCROLL => call_scroll(arguments),
        TOOL_TABS => call_tabs(arguments),
        TOOL_EXTRACT => call_extract(arguments),
        TOOL_EXPECT => call_expect(arguments),
        TOOL_FIND => call_find(arguments),
        TOOL_NETWORK_ROUTE => call_network_route(arguments),
        TOOL_SITE => call_site(arguments),
        TOOL_UPLOAD => call_upload(arguments),
        TOOL_DOWNLOAD => call_download(arguments),
        TOOL_DOWNLOAD_URL => call_download_url(arguments),
        TOOL_DOWNLOADS => call_downloads(arguments),
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

// === Extended profile (`--tools all`) ===

/// `hover <selector>`.
fn call_hover(arguments: &Value) -> Result<Value, ProtocolError> {
    let selector = required_string(arguments, "selector")?;
    run_tool(arguments, vec!["hover".to_string(), selector])
}

/// `select <selector> <value...>`.
fn call_select(arguments: &Value) -> Result<Value, ProtocolError> {
    let selector = required_string(arguments, "selector")?;
    let values = required_string_array(arguments, "values")?;
    let mut args = vec!["select".to_string(), selector];
    args.extend(values);
    run_tool(arguments, args)
}

/// `screenshot [selector] [path] [--full] [--clip x,y,w,h] [--max-width n]
/// [--max-height n] [--scale n]`.
fn call_screenshot(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["screenshot".to_string()];
    if optional_bool(arguments, "fullPage")?.unwrap_or(false) {
        args.push("--full".to_string());
    }
    if let Some(clip) = optional_value(arguments, "clip").filter(|v| !v.is_null()) {
        let x = required_f64(clip, "x")?;
        let y = required_f64(clip, "y")?;
        let width = required_f64(clip, "width")?;
        let height = required_f64(clip, "height")?;
        args.push("--clip".to_string());
        args.push(format!("{},{},{},{}", x, y, width, height));
    }
    if let Some(w) = optional_u64(arguments, "maxWidth")? {
        args.push("--max-width".to_string());
        args.push(w.to_string());
    }
    if let Some(h) = optional_u64(arguments, "maxHeight")? {
        args.push("--max-height".to_string());
        args.push(h.to_string());
    }
    if let Some(s) = optional_f64(arguments, "scale")? {
        args.push("--scale".to_string());
        args.push(s.to_string());
    }
    // Positional order matters: with two args the CLI treats the first as the
    // selector and the second as the path (a single arg is disambiguated by
    // shape — see the `screenshot` help text).
    if let Some(selector) = optional_string(arguments, "selector")? {
        args.push(selector);
    }
    if let Some(path) = optional_string(arguments, "path")? {
        args.push(path);
    }
    run_tool(arguments, args)
}

/// `scroll [direction] [amount] [--selector <sel>] [--at <x,y>] [--frame <n>]`.
fn call_scroll(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["scroll".to_string()];
    if let Some(selector) = optional_string(arguments, "selector")? {
        args.push("--selector".to_string());
        args.push(selector);
    }
    if let Some(at) = optional_value(arguments, "at").filter(|v| !v.is_null()) {
        let x = required_f64(at, "x")?;
        let y = required_f64(at, "y")?;
        args.push("--at".to_string());
        args.push(format!("{},{}", x, y));
    }
    if let Some(frame) = optional_u64(arguments, "frame")? {
        args.push("--frame".to_string());
        args.push(frame.to_string());
    }
    // Positional direction must precede amount (index-based parsing on the
    // CLI side).
    if let Some(direction) = optional_string(arguments, "direction")? {
        args.push(direction);
    }
    if let Some(amount) = optional_u64(arguments, "amount")? {
        args.push(amount.to_string());
    }
    run_tool(arguments, args)
}

/// `tab list [--full]` | `tab new [url] [--label <name>]` | `tab duplicate
/// [tabId] [--label <name>]` | `tab close [tabId]` | `tab <tabId> [--activate]`.
fn call_tabs(arguments: &Value) -> Result<Value, ProtocolError> {
    run_tool(arguments, tabs_args(arguments)?)
}

fn tabs_args(arguments: &Value) -> Result<Vec<String>, ProtocolError> {
    let action = required_string(arguments, "action")?;
    let mut args = vec!["tab".to_string()];
    match action.as_str() {
        "list" => {
            args.push("list".to_string());
            if optional_bool(arguments, "full")?.unwrap_or(false) {
                args.push("--full".to_string());
            }
        }
        "new" => {
            args.push("new".to_string());
            if let Some(label) = optional_string(arguments, "label")? {
                args.push("--label".to_string());
                args.push(label);
            }
            if let Some(url) = optional_string(arguments, "url")? {
                args.push(url);
            }
        }
        "duplicate" => {
            args.push("duplicate".to_string());
            if let Some(tab_id) = optional_string(arguments, "tabId")? {
                args.push(tab_id);
            }
            if let Some(label) = optional_string(arguments, "label")? {
                args.push("--label".to_string());
                args.push(label);
            }
        }
        "close" => {
            args.push("close".to_string());
            if let Some(tab_id) = optional_string(arguments, "tabId")? {
                args.push(tab_id);
            }
        }
        "switch" => {
            let tab_id = required_string(arguments, "tabId")?;
            args.push(tab_id);
            if optional_bool(arguments, "activate")?.unwrap_or(false) {
                args.push("--activate".to_string());
            }
        }
        other => {
            return Err(ProtocolError::invalid_params(format!(
                "chrome_use_tabs: unknown action '{}' (use list, new, duplicate, close, or switch)",
                other
            )))
        }
    }
    Ok(args)
}

/// `extract [--frame <f>] --schema <json>`.
fn call_extract(arguments: &Value) -> Result<Value, ProtocolError> {
    let schema = optional_value(arguments, "schema")
        .filter(|v| !v.is_null())
        .ok_or_else(|| ProtocolError::invalid_params("schema must be an object"))?;
    if !schema.get("fields").map(|f| f.is_object()).unwrap_or(false) {
        return Err(ProtocolError::invalid_params(
            "schema needs a \"fields\" object",
        ));
    }

    let mut args = vec!["extract".to_string()];
    // `--frame` must lead (extract's parser only recognizes it as the first
    // token, so it never eats a schema token).
    if let Some(frame) = optional_string(arguments, "frame")? {
        args.push("--frame".to_string());
        args.push(frame);
    }
    args.push("--schema".to_string());
    args.push(serde_json::to_string(schema).map_err(|e| {
        ProtocolError::invalid_params(format!("schema must be JSON-serializable: {}", e))
    })?);
    run_tool(arguments, args)
}

/// `expect <condition> [--timeout <ms>] [--no-wait] [--not] [--regex]`. See
/// output.rs's `expect` help text for the full grammar; `kind` here selects
/// which of those condition shapes to build.
fn call_expect(arguments: &Value) -> Result<Value, ProtocolError> {
    let kind = required_string(arguments, "kind")?;
    let mut args = vec!["expect".to_string()];

    // Flags that `expect` recognizes anywhere in its args.
    if optional_bool(arguments, "noWait")?.unwrap_or(false) {
        args.push("--no-wait".to_string());
    }
    if optional_bool(arguments, "not")?.unwrap_or(false) {
        args.push("--not".to_string());
    }
    if optional_bool(arguments, "regex")?.unwrap_or(false) {
        args.push("--regex".to_string());
    }
    if let Some(timeout) = optional_u64(arguments, "waitTimeoutMs")? {
        args.push("--timeout".to_string());
        args.push(timeout.to_string());
    }
    if kind == "request" {
        if let Some(method) = optional_string(arguments, "method")? {
            args.push("--method".to_string());
            args.push(method);
        }
        if let Some(status) = optional_string(arguments, "status")? {
            args.push("--status".to_string());
            args.push(status);
        }
        if let Some(rtype) = optional_string(arguments, "type")? {
            args.push("--type".to_string());
            args.push(rtype);
        }
        if let Some(count) = optional_u64(arguments, "requestCount")? {
            args.push("--count".to_string());
            args.push(count.to_string());
        }
    }

    match kind.as_str() {
        "count" => {
            let selector = required_string(arguments, "selector")?;
            let op = required_string(arguments, "op")?;
            let count = required_u64(arguments, "count")?;
            args.push("count".to_string());
            args.push(selector);
            args.push(op);
            args.push(count.to_string());
        }
        "text" | "value" => {
            let selector = required_string(arguments, "selector")?;
            let predicate = required_string(arguments, "predicate")?;
            let expected = required_string(arguments, "expected")?;
            args.push(kind.clone());
            args.push(selector);
            args.push(predicate);
            args.push(expected);
        }
        "attr" => {
            let selector = required_string(arguments, "selector")?;
            let attr = required_string(arguments, "attr")?;
            let predicate = required_string(arguments, "predicate")?;
            let expected = required_string(arguments, "expected")?;
            args.push("attr".to_string());
            args.push(selector);
            args.push(attr);
            args.push(predicate);
            args.push(expected);
        }
        "url" => {
            let predicate = required_string(arguments, "predicate")?;
            let expected = required_string(arguments, "expected")?;
            args.push("url".to_string());
            args.push(predicate);
            args.push(expected);
        }
        "request" => {
            let filter = required_string(arguments, "filter")?;
            args.push("request".to_string());
            args.push(filter);
        }
        "no-errors" => {
            args.push("no-errors".to_string());
        }
        "element" => {
            let selector = required_string(arguments, "selector")?;
            let state = required_string(arguments, "state")?;
            args.push(selector);
            args.push(state);
        }
        other => {
            return Err(ProtocolError::invalid_params(format!(
                "chrome_use_expect: unknown kind '{}' (use element, count, text, value, attr, url, request, or no-errors)",
                other
            )))
        }
    }
    run_tool(arguments, args)
}

/// `find <locator> <value> [action] [text] [--name <n>] [--exact]` (locator
/// `nth` instead takes `find nth <index> <selector> [action] [text]`).
fn call_find(arguments: &Value) -> Result<Value, ProtocolError> {
    let locator = required_string(arguments, "locator")?;
    let action = optional_string(arguments, "action")?.unwrap_or_else(|| "click".to_string());
    let mut args = vec!["find".to_string()];

    if locator == "nth" {
        let index = required_u64(arguments, "index")?;
        let selector = required_string(arguments, "value")?;
        args.push("nth".to_string());
        args.push(index.to_string());
        args.push(selector);
        args.push(action);
        if let Some(text) = optional_string(arguments, "text")? {
            args.push(text);
        }
        // `find nth` has no flag parsing at all — --name/--exact would be
        // swallowed into the fill text, so they're not supported here.
        return run_tool(arguments, args);
    }

    let value = required_string(arguments, "value")?;
    args.push(locator);
    args.push(value);
    args.push(action);
    if let Some(name) = optional_string(arguments, "name")? {
        args.push("--name".to_string());
        args.push(name);
    }
    if optional_bool(arguments, "exact")?.unwrap_or(false) {
        args.push("--exact".to_string());
    }
    if let Some(text) = optional_string(arguments, "text")? {
        args.push(text);
    }
    run_tool(arguments, args)
}

/// `network route <url> [flags]` | `network unroute [url]`.
fn call_network_route(arguments: &Value) -> Result<Value, ProtocolError> {
    let action = optional_string(arguments, "action")?.unwrap_or_else(|| "route".to_string());
    let mut args = vec!["network".to_string()];

    match action.as_str() {
        "route" => {
            let url = required_string(arguments, "url")?;
            args.push("route".to_string());
            args.push(url);
            if optional_bool(arguments, "abort")?.unwrap_or(false) {
                args.push("--abort".to_string());
            }
            if let Some(body) = optional_string(arguments, "body")? {
                args.push("--body".to_string());
                args.push(body);
            }
            if let Some(status) = optional_u64(arguments, "status")? {
                args.push("--status".to_string());
                args.push(status.to_string());
            }
            if let Some(ct) = optional_string(arguments, "contentType")? {
                args.push("--content-type".to_string());
                args.push(ct);
            }
            for (k, v) in optional_string_map(arguments, "headers")? {
                args.push("--header".to_string());
                args.push(format!("{}={}", k, v));
            }
            if let Some(body) = optional_string(arguments, "setBody")? {
                args.push("--set-body".to_string());
                args.push(body);
            }
            if let Some(method) = optional_string(arguments, "method")? {
                args.push("--method".to_string());
                args.push(method);
            }
            if let Some(url) = optional_string(arguments, "rewriteUrl")? {
                args.push("--rewrite-url".to_string());
                args.push(url);
            }
            for (k, v) in optional_string_map(arguments, "setHeaders")? {
                args.push("--set-header".to_string());
                args.push(format!("{}={}", k, v));
            }
            if let Some(status) = optional_u64(arguments, "editStatus")? {
                args.push("--edit-status".to_string());
                args.push(status.to_string());
            }
            for (k, v) in optional_string_map(arguments, "editHeaders")? {
                args.push("--edit-header".to_string());
                args.push(format!("{}={}", k, v));
            }
            for (from, to) in optional_replacements(arguments, "replacements")? {
                args.push("--replace".to_string());
                args.push(format!("{}=>{}", from, to));
            }
            if let Some(rt) = optional_string(arguments, "resourceType")? {
                args.push("--resource-type".to_string());
                args.push(rt);
            }
        }
        "unroute" => {
            args.push("unroute".to_string());
            if let Some(url) = optional_string(arguments, "url")? {
                args.push(url);
            }
        }
        other => {
            return Err(ProtocolError::invalid_params(format!(
                "chrome_use_network_route: unknown action '{}' (use route or unroute)",
                other
            )))
        }
    }
    run_tool(arguments, args)
}

/// `site <name>/<command> [positional...] [--key value]`.
fn call_site(arguments: &Value) -> Result<Value, ProtocolError> {
    let spec = required_string(arguments, "spec")?;
    let mut args = vec!["site".to_string(), spec];
    args.extend(optional_string_array(arguments, "args")?);
    for (k, v) in optional_string_map(arguments, "namedArgs")? {
        args.push(format!("--{}", k));
        args.push(v);
    }
    run_tool(arguments, args)
}

/// `upload <selector> <files...>`.
fn call_upload(arguments: &Value) -> Result<Value, ProtocolError> {
    let selector = required_string(arguments, "selector")?;
    let files = required_string_array(arguments, "files")?;
    let mut args = vec!["upload".to_string(), selector];
    args.extend(files);
    run_tool(arguments, args)
}

/// `download <selector> <path>`.
fn call_download(arguments: &Value) -> Result<Value, ProtocolError> {
    let selector = required_string(arguments, "selector")?;
    let path = required_string(arguments, "path")?;
    run_tool(arguments, vec!["download".to_string(), selector, path])
}

fn call_download_url(arguments: &Value) -> Result<Value, ProtocolError> {
    let url = required_string(arguments, "url")?;
    let mut args = vec!["download-url".to_string(), url];
    if let Some(path) = optional_string(arguments, "path")? {
        args.push(path);
    }
    run_tool(arguments, args)
}

fn call_downloads(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["downloads".to_string()];
    if optional_bool(arguments, "clear")?.unwrap_or(false) {
        args.push("--clear".to_string());
    } else if let Some(limit) = optional_u64(arguments, "limit")? {
        args.push("--limit".to_string());
        args.push(limit.to_string());
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

fn required_u64(arguments: &Value, key: &str) -> Result<u64, ProtocolError> {
    optional_u64(arguments, key)?.ok_or_else(|| {
        ProtocolError::invalid_params(format!("{} must be a non-negative integer", key))
    })
}

fn optional_f64(arguments: &Value, key: &str) -> Result<Option<f64>, ProtocolError> {
    match optional_value(arguments, key) {
        Some(Value::Null) | None => Ok(None),
        Some(v) => v
            .as_f64()
            .map(Some)
            .ok_or_else(|| ProtocolError::invalid_params(format!("{} must be a number", key))),
    }
}

fn required_f64(arguments: &Value, key: &str) -> Result<f64, ProtocolError> {
    optional_f64(arguments, key)?
        .ok_or_else(|| ProtocolError::invalid_params(format!("{} must be a number", key)))
}

/// A JSON array of strings, defaulting to empty when the key is absent.
fn optional_string_array(arguments: &Value, key: &str) -> Result<Vec<String>, ProtocolError> {
    match optional_value(arguments, key) {
        Some(Value::Null) | None => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str().map(ToString::to_string).ok_or_else(|| {
                    ProtocolError::invalid_params(format!("{} must be an array of strings", key))
                })
            })
            .collect(),
        Some(_) => Err(ProtocolError::invalid_params(format!(
            "{} must be an array of strings",
            key
        ))),
    }
}

fn required_string_array(arguments: &Value, key: &str) -> Result<Vec<String>, ProtocolError> {
    let values = optional_string_array(arguments, key)?;
    if values.is_empty() {
        return Err(ProtocolError::invalid_params(format!(
            "{} must be a non-empty array of strings",
            key
        )));
    }
    Ok(values)
}

/// A JSON object of string→string, defaulting to empty when the key is
/// absent — used for repeatable `--flag K=V` CLI options (headers, etc.).
fn optional_string_map(
    arguments: &Value,
    key: &str,
) -> Result<Vec<(String, String)>, ProtocolError> {
    match optional_value(arguments, key) {
        Some(Value::Null) | None => Ok(Vec::new()),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(k, v)| {
                v.as_str()
                    .map(|v| (k.clone(), v.to_string()))
                    .ok_or_else(|| {
                        ProtocolError::invalid_params(format!("{}.{} must be a string", key, k))
                    })
            })
            .collect(),
        Some(_) => Err(ProtocolError::invalid_params(format!(
            "{} must be an object of string values",
            key
        ))),
    }
}

/// `replacements`: an array of `[from, to]` string pairs, for the
/// `network_route` tool's `--replace from=>to` (repeatable).
fn optional_replacements(
    arguments: &Value,
    key: &str,
) -> Result<Vec<(String, String)>, ProtocolError> {
    match optional_value(arguments, key) {
        Some(Value::Null) | None => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                let pair = item.as_array().filter(|p| p.len() == 2).ok_or_else(|| {
                    ProtocolError::invalid_params(format!(
                        "{} entries must be [from, to] string pairs",
                        key
                    ))
                })?;
                let from = pair[0].as_str().ok_or_else(|| {
                    ProtocolError::invalid_params(format!(
                        "{} entries must be [from, to] string pairs",
                        key
                    ))
                })?;
                let to = pair[1].as_str().ok_or_else(|| {
                    ProtocolError::invalid_params(format!(
                        "{} entries must be [from, to] string pairs",
                        key
                    ))
                })?;
                Ok((from.to_string(), to.to_string()))
            })
            .collect(),
        Some(_) => Err(ProtocolError::invalid_params(format!(
            "{} must be an array of [from, to] pairs",
            key
        ))),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_duplicate_maps_to_cli_contract() {
        let current = tabs_args(&json!({ "action": "duplicate" }))
            .ok()
            .expect("duplicate action should map to the CLI");
        assert_eq!(current, vec!["tab", "duplicate"]);

        let selected = tabs_args(&json!({
            "action": "duplicate",
            "tabId": "docs",
            "label": "docs-copy"
        }))
        .ok()
        .expect("duplicate action with ref and label should map to the CLI");
        assert_eq!(
            selected,
            vec!["tab", "duplicate", "docs", "--label", "docs-copy"]
        );
    }

    const CORE_TOOL_NAMES: &[&str] = &[
        TOOL_OPEN,
        TOOL_READ,
        TOOL_SNAPSHOT,
        TOOL_CLICK,
        TOOL_FILL,
        TOOL_TYPE,
        TOOL_PRESS,
        TOOL_EVAL,
        TOOL_WAIT,
        TOOL_BACK,
        TOOL_FORWARD,
        TOOL_RELOAD,
    ];

    const EXTENDED_ONLY_TOOL_NAMES: &[&str] = &[
        TOOL_HOVER,
        TOOL_SELECT,
        TOOL_SCREENSHOT,
        TOOL_SCROLL,
        TOOL_TABS,
        TOOL_EXTRACT,
        TOOL_EXPECT,
        TOOL_FIND,
        TOOL_NETWORK_ROUTE,
        TOOL_SITE,
        TOOL_UPLOAD,
        TOOL_DOWNLOAD,
        TOOL_DOWNLOAD_URL,
        TOOL_DOWNLOADS,
    ];

    fn tool_names(tools: &[Value]) -> Vec<&str> {
        tools
            .iter()
            .map(|t| t.get("name").and_then(|n| n.as_str()).unwrap())
            .collect()
    }

    /// Parity guard: the `core` profile must expose exactly the original 12
    /// tools (unchanged), and `all` must be a strict superset that adds every
    /// extended tool on top of `core` — so growing the extended set can never
    /// silently shrink or rename what `core` promises callers.
    #[test]
    fn core_profile_is_exactly_the_known_12_tools() {
        let core = tools_for(Profile::Core);
        let names = tool_names(&core);
        assert_eq!(names.len(), CORE_TOOL_NAMES.len());
        for expected in CORE_TOOL_NAMES {
            assert!(
                names.contains(expected),
                "core profile missing tool: {}",
                expected
            );
        }
        for name in &names {
            assert!(
                is_known_tool(name, Profile::Core),
                "core-listed tool not recognized in core profile: {}",
                name
            );
        }
    }

    #[test]
    fn all_profile_is_a_superset_including_extended_tools() {
        let core = tools_for(Profile::Core);
        let all = tools_for(Profile::All);
        let core_names = tool_names(&core);
        let all_names = tool_names(&all);

        for name in &core_names {
            assert!(
                all_names.contains(name),
                "all profile dropped a core tool: {}",
                name
            );
        }
        for expected in EXTENDED_ONLY_TOOL_NAMES {
            assert!(
                all_names.contains(expected),
                "all profile missing extended tool: {}",
                expected
            );
            assert!(
                !core_names.contains(expected),
                "extended tool leaked into core profile: {}",
                expected
            );
            assert!(
                !is_known_tool(expected, Profile::Core),
                "extended tool callable under core profile: {}",
                expected
            );
            assert!(
                is_known_tool(expected, Profile::All),
                "extended tool not callable under all profile: {}",
                expected
            );
        }
        assert_eq!(
            all_names.len(),
            core_names.len() + EXTENDED_ONLY_TOOL_NAMES.len()
        );
    }
}
