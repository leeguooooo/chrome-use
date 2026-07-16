use std::sync::OnceLock;

use crate::color;
use crate::connection::Response;

static BOUNDARY_NONCE: OnceLock<String> = OnceLock::new();

/// Per-process nonce for content boundary markers. Uses a CSPRNG (getrandom) so
/// that untrusted page content cannot predict or spoof the boundary delimiter.
/// Process ID or timestamps would be insufficient since pages can read those.
fn get_boundary_nonce() -> &'static str {
    BOUNDARY_NONCE.get_or_init(|| {
        let mut buf = [0u8; 16];
        getrandom::getrandom(&mut buf).expect("failed to generate random nonce");
        buf.iter().map(|b| format!("{:02x}", b)).collect()
    })
}

#[derive(Default)]
pub struct OutputOptions {
    pub json: bool,
    pub content_boundaries: bool,
    pub max_output: Option<usize>,
}

impl OutputOptions {
    pub fn from_flags(flags: &crate::flags::Flags) -> Self {
        Self {
            json: flags.json,
            content_boundaries: flags.content_boundaries,
            max_output: flags.max_output,
        }
    }
}

fn truncate_if_needed(content: &str, max: Option<usize>) -> String {
    let Some(limit) = max else {
        return content.to_string();
    };
    // Fast path: byte length is a lower bound on char count, so if the
    // byte length is within the limit the char count must be too.
    if content.len() <= limit {
        return content.to_string();
    }
    // Find the byte offset of the limit-th character.
    match content.char_indices().nth(limit).map(|(i, _)| i) {
        Some(byte_offset) => {
            let total_chars = content.chars().count();
            format!(
                "{}\n[truncated: showing {} of {} chars. Use --max-output to adjust]",
                &content[..byte_offset],
                limit,
                total_chars
            )
        }
        // Content has fewer than `limit` chars despite more bytes
        None => content.to_string(),
    }
}

fn print_with_boundaries(content: &str, origin: Option<&str>, opts: &OutputOptions) {
    let content = truncate_if_needed(content, opts.max_output);
    if opts.content_boundaries {
        let origin_str = origin.unwrap_or("unknown");
        let nonce = get_boundary_nonce();
        println!(
            "--- AGENT_BROWSER_PAGE_CONTENT nonce={} origin={} ---",
            nonce, origin_str
        );
        println!("{}", content);
        println!("--- END_AGENT_BROWSER_PAGE_CONTENT nonce={} ---", nonce);
    } else {
        println!("{}", content);
    }
}

fn format_storage_value(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(ToString::to_string)
        .unwrap_or_else(|| serde_json::to_string(value).unwrap_or_default())
}

fn format_storage_text(data: &serde_json::Value) -> Option<String> {
    if let Some(entries) = data.get("data").and_then(|v| v.as_object()) {
        if entries.is_empty() {
            return Some("No storage entries".to_string());
        }

        let lines = entries
            .iter()
            .map(|(key, value)| format!("{}: {}", key, format_storage_value(value)))
            .collect::<Vec<_>>();
        return Some(lines.join("\n"));
    }

    let key = data.get("key").and_then(|v| v.as_str())?;
    let value = data.get("value")?;
    Some(format!("{}: {}", key, format_storage_value(value)))
}

fn format_stream_status_text(action: Option<&str>, data: &serde_json::Value) -> Option<String> {
    match action {
        Some("stream_disable") => data
            .get("disabled")
            .and_then(|v| v.as_bool())
            .filter(|disabled| *disabled)
            .map(|_| "Streaming disabled".to_string()),
        Some("stream_enable") | Some("stream_status") => {
            let enabled = data.get("enabled").and_then(|v| v.as_bool())?;
            if !enabled {
                return Some("Streaming disabled".to_string());
            }

            let port = data.get("port").and_then(|v| v.as_u64())?;
            let connected = data
                .get("connected")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let screencasting = data
                .get("screencasting")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            Some(format!(
                "Streaming enabled on ws://127.0.0.1:{port}\nConnected: {connected}\nScreencasting: {screencasting}"
            ))
        }
        _ => None,
    }
}

/// Shorten an over-long string by keeping its head and tail and eliding the
/// middle, with a char count. Used so multi-KB URLs (JWT/OTP login links) don't
/// flood `tab list`.
fn truncate_middle(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1) / 2;
    let head: String = s.chars().take(keep).collect();
    let tail: String = s.chars().skip(n - keep).collect();
    format!("{head}…{tail} [{n} chars]")
}

pub fn print_response_with_opts(resp: &Response, action: Option<&str>, opts: &OutputOptions) {
    if opts.json {
        if opts.content_boundaries {
            let mut json_val = serde_json::to_value(resp).unwrap_or_default();
            if let Some(obj) = json_val.as_object_mut() {
                let nonce = get_boundary_nonce();
                let origin = obj
                    .get("data")
                    .and_then(|d| d.get("origin"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                obj.insert(
                    "_boundary".to_string(),
                    serde_json::json!({
                        "nonce": nonce,
                        "origin": origin,
                    }),
                );
            }
            println!("{}", serde_json::to_string(&json_val).unwrap_or_default());
        } else {
            println!("{}", serde_json::to_string(resp).unwrap_or_default());
        }
        // JSON mode includes the warning field in the JSON payload already
        return;
    }

    if !resp.success {
        eprintln!(
            "{} {}",
            color::error_indicator(),
            resp.error.as_deref().unwrap_or("Unknown error")
        );
        // Still print dialog warning after errors, since a pending dialog
        // is the most common cause of commands timing out
        if let Some(ref warning) = resp.warning {
            eprintln!("{} {}", color::warning_indicator(), warning);
        }
        return;
    }

    if let Some(data) = &resp.data {
        // Auto-trigger: when you land on / read a page whose domain has site
        // adapters, surface them so the agent pulls structured data via
        // `chrome-use site <name>/<cmd>` instead of scraping the DOM. (In --json
        // mode this same info rides along in the `siteAdapters` field above.)
        if let Some(hint) = data.get("siteAdapters") {
            let domain = hint.get("domain").and_then(|v| v.as_str()).unwrap_or("");
            let cmds: Vec<&str> = hint
                .get("commands")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            if !cmds.is_empty() {
                eprintln!("💡 site adapters for {domain} — prefer these for structured data:");
                eprintln!("   {}", color::dim(&cmds.join(", ")));
                eprintln!(
                    "   {}",
                    color::dim(&format!("e.g. chrome-use site {} --json", cmds[0]))
                );
            }
        }
        // A click that opened a new tab: surface it so the agent doesn't read the
        // unchanged old page as a failed click (issue #24-A).
        if let Some(opened) = data.get("openedTab") {
            let tid = opened.get("tabId").and_then(|v| v.as_str()).unwrap_or("?");
            let url = opened.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let followed = data
                .get("followed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let verb = if followed {
                "switched to new tab"
            } else {
                "opened new tab"
            };
            eprintln!(
                "{} {} [{}] {}",
                color::cyan("→"),
                verb,
                tid,
                color::dim(url)
            );
        }

        // `current`: the active tab's stable handle (#26).
        if data
            .get("current")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let tid = data.get("tabId").and_then(|v| v.as_str()).unwrap_or("?");
            let title = data.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let url = data.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let target = data.get("targetId").and_then(|v| v.as_str()).unwrap_or("");
            println!("{} [{}] {} - {}", color::cyan("→"), tid, title, url);
            println!("      {}", color::dim(&format!("target: {}", target)));
            return;
        }

        // `expect` assertion result → a PASS/FAIL line. Action-gated and placed
        // early so the generic key-based renderers don't swallow it. The exit code
        // is set in main.rs.
        if action == Some("expect") {
            let pass = data.get("pass").and_then(|v| v.as_bool()).unwrap_or(false);
            let cond = data.get("condition").and_then(|v| v.as_str()).unwrap_or("");
            if pass {
                println!("{} expect: {}", color::green("PASS"), cond);
            } else {
                let actual = data
                    .get("actual")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let timed = data
                    .get("timedOut")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let suffix = if timed { " (timed out)" } else { "" };
                println!(
                    "{} expect: {} — actual: {}{}",
                    color::red("FAIL"),
                    cond,
                    if actual.is_null() {
                        "∅".to_string()
                    } else {
                        actual.to_string()
                    },
                    suffix
                );
            }
            return;
        }

        // Action guard skip (`--if-present`/`--optional`): the target was absent so
        // the action no-op'd. Surface it so a skip isn't mistaken for a real action.
        if data
            .get("skipped")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let reason = data
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("skipped");
            println!("{} {}", color::dim("↷"), color::dim(reason));
            return;
        }

        // `extract` → print the structured result as pretty JSON (its whole point
        // is machine-readable data; that's also the best human view). Action-gated
        // + early so generic renderers don't swallow it.
        if action == Some("extract") {
            if let Some(ex) = data.get("extracted") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(ex).unwrap_or_else(|_| ex.to_string())
                );
            }
            return;
        }

        // `read` prints only the extracted document content (markdown / text /
        // outline), optionally wrapped in content boundaries. Action-gated + early
        // so the generic `✓ <url>` renderer doesn't swallow the body.
        if action == Some("read") {
            if let Some(content) = data.get("content").and_then(|v| v.as_str()) {
                let origin = data
                    .get("finalUrl")
                    .and_then(|v| v.as_str())
                    .or_else(|| data.get("url").and_then(|v| v.as_str()));
                print_with_boundaries(content, origin, opts);
            }
            return;
        }

        // Cloudflare challenge/clearance preflight (`cf-status`). Checked early
        // because its response carries `url`/`title`, which later generic
        // renderers would otherwise swallow.
        if action == Some("cf_status") {
            let challenged = data
                .get("challenged")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let rec = data
                .get("recommendation")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let cl = data.get("clearance");
            let present = cl
                .and_then(|c| c.get("present"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let expired = cl
                .and_then(|c| c.get("expired"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let expires_in = cl.and_then(|c| c.get("expiresIn")).and_then(|v| v.as_i64());
            let device = data
                .get("deviceVerified")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let (icon, headline) = match rec {
                "proceed" => (color::success_indicator().to_string(), "cleared — no challenge, proceed"),
                "solve" => (color::warning_indicator().to_string(), "Cloudflare challenge active, no valid clearance — solve it"),
                "reissue" => (color::warning_indicator().to_string(), "challenge active but a clearance cookie exists — stale (IP/UA changed?), re-solve"),
                _ => (color::cyan("•").to_string(), "unknown"),
            };
            println!("{} {}", icon, headline);
            println!(
                "  challenged:     {}",
                if challenged { "yes" } else { "no" }
            );
            let cl_desc = if !present {
                "absent".to_string()
            } else if expired {
                "present but EXPIRED".to_string()
            } else if let Some(s) = expires_in {
                format!("valid, expires in {}m {}s", s / 60, s % 60)
            } else {
                "present (session)".to_string()
            };
            println!("  cf_clearance:   {}", cl_desc);
            println!(
                "  device trusted: {}",
                if device {
                    "yes (CF_VERIFIED_DEVICE)"
                } else {
                    "no"
                }
            );
            return;
        }

        // Dialog status response
        if action == Some("dialog") {
            if let Some(has_dialog) = data.get("hasDialog").and_then(|v| v.as_bool()) {
                if has_dialog {
                    let dtype = data
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let message = data.get("message").and_then(|v| v.as_str()).unwrap_or("");
                    println!(
                        "{} JavaScript {} dialog is open: \"{}\"",
                        color::warning_indicator(),
                        dtype,
                        message
                    );
                    if let Some(default_prompt) = data.get("defaultPrompt").and_then(|v| v.as_str())
                    {
                        println!("  Default prompt text: \"{}\"", default_prompt);
                    }
                    println!("  Use `dialog accept [text]` or `dialog dismiss` to resolve it");
                } else {
                    println!("{} No dialog is currently open", color::success_indicator());
                }
                print_warning(resp);
                return;
            }
        }
        if let Some(output) = format_stream_status_text(action, data) {
            println!("{}", output);
            return;
        }
        if action == Some("storage_get") {
            if let Some(output) = format_storage_text(data) {
                println!("{}", output);
                return;
            }
        }
        // Inspect response (check before generic URL handler since it also has a "url" field)
        if action == Some("inspect") {
            let opened = data
                .get("opened")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if opened {
                if let Some(url) = data.get("url").and_then(|v| v.as_str()) {
                    println!("{} Opened DevTools: {}", color::success_indicator(), url);
                } else {
                    println!("{} Opened DevTools", color::success_indicator());
                }
            } else if let Some(err) = data.get("error").and_then(|v| v.as_str()) {
                eprintln!("Could not open DevTools: {}", err);
            }
            return;
        }
        // Navigation response
        if let Some(url) = data.get("url").and_then(|v| v.as_str()) {
            let title = data
                .get("title")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|t| !t.is_empty());
            match title {
                Some(t) => {
                    println!("{} {}", color::success_indicator(), color::bold(t));
                    println!("  {}", color::dim(url));
                }
                // Title-less page: show the URL with the checkmark instead of an
                // empty title line.
                None => println!("{} {}", color::success_indicator(), color::dim(url)),
            }
            // Soft warning carried in the response (e.g. the load event timed out
            // but the DOM was ready — issue #10). Goes to stderr so it doesn't
            // pollute the stdout url/title that scripts parse.
            if let Some(w) = data.get("warning").and_then(|v| v.as_str()) {
                eprintln!("⚠ navigation: {w}");
            }
            return;
        }
        if let Some(cdp_url) = data.get("cdpUrl").and_then(|v| v.as_str()) {
            println!("{}", cdp_url);
            return;
        }
        // Diff responses -- route by action to avoid fragile shape probing
        if let Some(obj) = data.as_object() {
            match action {
                Some("diff_snapshot") => {
                    print_snapshot_diff(obj);
                    return;
                }
                Some("diff_screenshot") => {
                    print_screenshot_diff(obj);
                    return;
                }
                Some("diff_url") => {
                    if let Some(snap_data) = obj.get("snapshot").and_then(|v| v.as_object()) {
                        println!("{}", color::bold("Snapshot diff:"));
                        print_snapshot_diff(snap_data);
                    }
                    if let Some(ss_data) = obj.get("screenshot").and_then(|v| v.as_object()) {
                        println!("\n{}", color::bold("Screenshot diff:"));
                        print_screenshot_diff(ss_data);
                    }
                    return;
                }
                _ => {}
            }
        }
        let origin = data.get("origin").and_then(|v| v.as_str());
        // Snapshot
        if let Some(snapshot) = data.get("snapshot").and_then(|v| v.as_str()) {
            print_with_boundaries(snapshot, origin, opts);
            // Canvas-app hint: the tree was near-empty but the page paints to a
            // <canvas>, so refs are a dead end — point at the screenshot path.
            if let Some(note) = data.get("note").and_then(|v| v.as_str()) {
                eprintln!("{}", color::dim(note));
            }
            return;
        }
        // Frame list (`chrome-use frames`)
        if action == Some("frames") {
            if let Some(list) = data.get("frames").and_then(|v| v.as_array()) {
                let count = list.len();
                println!(
                    "{}",
                    color::bold(&format!(
                        "{} frame{}",
                        count,
                        if count == 1 { "" } else { "s" }
                    ))
                );
                for f in list {
                    let idx = f.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
                    let kind = f.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                    let url = f.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let len = f.get("textLen").and_then(|v| v.as_i64()).unwrap_or(0);
                    println!(
                        "  [{}] {:<6} {} chars  {}",
                        idx,
                        kind,
                        len,
                        color::dim(if url.is_empty() { "(about:blank)" } else { url })
                    );
                }
                eprintln!(
                    "{}",
                    color::dim("read everything with: chrome-use get text --all-frames")
                );
            }
            return;
        }
        // Title
        if let Some(title) = data.get("title").and_then(|v| v.as_str()) {
            println!("{}", title);
            return;
        }
        // Text
        if let Some(text) = data.get("text").and_then(|v| v.as_str()) {
            print_with_boundaries(text, origin, opts);
            return;
        }
        // HTML
        if let Some(html) = data.get("html").and_then(|v| v.as_str()) {
            print_with_boundaries(html, origin, opts);
            return;
        }
        // Value
        if let Some(value) = data.get("value").and_then(|v| v.as_str()) {
            println!("{}", value);
            return;
        }
        // Count
        if let Some(count) = data.get("count").and_then(|v| v.as_i64()) {
            println!("{}", count);
            return;
        }
        // Bounding box (get box)
        if action == Some("boundingbox") {
            if let Some(obj) = data.as_object() {
                let x = obj.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let y = obj.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let w = obj.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let h = obj.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0);
                println!("x:      {}", x);
                println!("y:      {}", y);
                println!("width:  {}", w);
                println!("height: {}", h);
                if let (Some(cx), Some(cy)) = (
                    obj.get("centerX").and_then(|v| v.as_i64()),
                    obj.get("centerY").and_then(|v| v.as_i64()),
                ) {
                    // Echoed in click-ready CSS px so the agent can paste straight
                    // into `click <centerX> <centerY>` (issue #43).
                    println!("center: {} {}", cx, cy);
                }
                if let Some(iv) = obj.get("inViewport").and_then(|v| v.as_bool()) {
                    println!("inViewport: {}", iv);
                }
            }
            return;
        }
        // Computed styles (get styles)
        if let Some(styles) = data.get("styles").and_then(|v| v.as_object()) {
            for (key, val) in styles {
                let display = match val.as_str() {
                    Some(s) => s.to_string(),
                    None => val.to_string(),
                };
                println!("{}: {}", key, display);
            }
            return;
        }
        // Boolean results
        if let Some(visible) = data.get("visible").and_then(|v| v.as_bool()) {
            println!("{}", visible);
            return;
        }
        if let Some(enabled) = data.get("enabled").and_then(|v| v.as_bool()) {
            println!("{}", enabled);
            return;
        }
        // Stealth self-check (`stealth status` / `doctor`)
        if let Some(s) = data.get("stealthStatus") {
            let ok = s.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            let mode = s.get("mode").and_then(|v| v.as_str()).unwrap_or("?");
            println!(
                "{} stealth: {}  ·  mode: {}",
                if ok {
                    color::success_indicator().to_string()
                } else {
                    color::cyan("•")
                },
                if ok {
                    "all checks pass"
                } else {
                    "some checks need attention"
                },
                mode
            );
            if let Some(checks) = s.get("checks").and_then(|v| v.as_array()) {
                for c in checks {
                    let pass = c.get("pass").and_then(|v| v.as_bool()).unwrap_or(false);
                    let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    println!("  {} {}", if pass { "✓" } else { "✗" }, name);
                }
            }
            if let Some(ovs) = s.get("overrides").and_then(|v| v.as_array()) {
                println!("  applied overrides:");
                for o in ovs.iter().filter_map(|v| v.as_str()) {
                    println!("    {}", color::dim(&format!("· {o}")));
                }
            }
            return;
        }
        if let Some(checked) = data.get("checked").and_then(|v| v.as_bool()) {
            println!("{}", checked);
            return;
        }
        // Eval result
        if let Some(result) = data.get("result") {
            // Surface which page the eval actually ran on — to stderr, so it
            // never corrupts the parsed value on stdout. Lets an agent catch tab
            // drift (commands landing on the wrong tab) before trusting a result,
            // e.g. a logged-in `fetch` that hit the wrong origin. (In
            // content-boundaries mode the origin is already in the banner.)
            if !opts.content_boundaries {
                if let Some(o) = origin.filter(|o| !o.is_empty()) {
                    eprintln!("eval @ {o}");
                }
            }
            let formatted = serde_json::to_string_pretty(result).unwrap_or_default();
            print_with_boundaries(&formatted, origin, opts);
            return;
        }
        // iOS Devices
        if let Some(devices) = data.get("devices").and_then(|v| v.as_array()) {
            if devices.is_empty() {
                println!("No iOS devices available. Open Xcode to download simulator runtimes.");
                return;
            }

            // Separate real devices from simulators
            let real_devices: Vec<_> = devices
                .iter()
                .filter(|d| {
                    d.get("isRealDevice")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                })
                .collect();
            let simulators: Vec<_> = devices
                .iter()
                .filter(|d| {
                    !d.get("isRealDevice")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                })
                .collect();

            if !real_devices.is_empty() {
                println!("Connected Devices:\n");
                for device in real_devices.iter() {
                    let name = device
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown");
                    let runtime = device.get("runtime").and_then(|v| v.as_str()).unwrap_or("");
                    let udid = device.get("udid").and_then(|v| v.as_str()).unwrap_or("");
                    println!("  {} {} ({})", color::green("●"), name, runtime);
                    println!("    {}", color::dim(udid));
                }
                println!();
            }

            if !simulators.is_empty() {
                println!("Simulators:\n");
                for device in simulators.iter() {
                    let name = device
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown");
                    let runtime = device.get("runtime").and_then(|v| v.as_str()).unwrap_or("");
                    let state = device
                        .get("state")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown");
                    let udid = device.get("udid").and_then(|v| v.as_str()).unwrap_or("");
                    let state_indicator = if state == "Booted" {
                        color::green("●")
                    } else {
                        color::dim("○")
                    };
                    println!("  {} {} ({})", state_indicator, name, runtime);
                    println!("    {}", color::dim(udid));
                }
            }
            return;
        }
        // Tabs
        if let Some(tabs) = data.get("tabs").and_then(|v| v.as_array()) {
            // `tab list --full` prints untruncated URLs so a long SSO/redirect
            // URL can actually be re-opened after a stale session (issue #19).
            let full = data.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
            for tab in tabs {
                let tab_id = tab.get("tabId").and_then(|v| v.as_str()).unwrap_or("?");
                let tab_label = tab.get("label").and_then(|v| v.as_str());
                let title = tab
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Untitled");
                // A page can set its title to a multi-KB string (e.g. equal to a
                // giant JWT/OTP URL); truncate it like the URL so the row stays
                // readable.
                let title = truncate_middle(title, 120);
                let title = title.as_str();
                let url = tab.get("url").and_then(|v| v.as_str()).unwrap_or("");
                // Truncate very long URLs (e.g. multi-KB JWT/OTP login links) so
                // the list stays readable instead of flooding the terminal —
                // unless `--full` was asked for (to re-open the exact URL).
                let url = if full {
                    url.to_string()
                } else {
                    truncate_middle(url, 120)
                };
                let active = tab.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
                let marker = if active {
                    color::cyan("→")
                } else {
                    " ".to_string()
                };
                if let Some(label) = tab_label {
                    println!("{} [{}] {} {} - {}", marker, tab_id, label, title, url);
                } else {
                    println!("{} [{}] {} - {}", marker, tab_id, title, url);
                }
                // `--full` also surfaces the stable cross-session CDP targetId so
                // a stranded tab can be adopted from another session via
                // `tab <targetId>` (issue #21).
                if full {
                    if let Some(target_id) = tab.get("targetId").and_then(|v| v.as_str()) {
                        println!("      {}", color::dim(&format!("target: {}", target_id)));
                    }
                }
            }
            return;
        }
        // Tab switch
        if action == Some("tab_switch") {
            if let Some(tab_id) = data.get("tabId").and_then(|v| v.as_str()) {
                let warning = data.get("warning").and_then(|v| v.as_str());
                // A non-responding session isn't a real success — show a warning
                // indicator instead of the green ✓ (issue #29.3).
                let indicator = if warning.is_some() {
                    color::warning_indicator()
                } else {
                    color::success_indicator()
                };
                if let Some(url) = data.get("url").and_then(|v| v.as_str()) {
                    println!("{} Switched to tab [{}] ({})", indicator, tab_id, url);
                } else {
                    println!("{} Switched to tab [{}]", indicator, tab_id);
                }
                if let Some(w) = warning {
                    eprintln!("{}", color::dim(w));
                }
                return;
            }
        }
        // New tab/window
        if let Some(tab_id) = data.get("tabId").and_then(|v| v.as_str()) {
            if let Some(total) = data.get("total").and_then(|v| v.as_i64()) {
                let label_noun = match action {
                    Some("window_new") => "Window opened",
                    Some("tab_duplicate") => "Tab duplicated",
                    _ => "Tab opened",
                };
                let tab_label = data.get("label").and_then(|v| v.as_str());
                if let Some(lbl) = tab_label {
                    println!(
                        "{} {} [{}] {} ({} total)",
                        color::success_indicator(),
                        label_noun,
                        tab_id,
                        lbl,
                        total
                    );
                } else {
                    println!(
                        "{} {} [{}] ({} total)",
                        color::success_indicator(),
                        label_noun,
                        tab_id,
                        total
                    );
                }
                return;
            }
        }
        // Console logs
        if let Some(logs) = data.get("messages").and_then(|v| v.as_array()) {
            if opts.content_boundaries {
                let mut console_output = String::new();
                for log in logs {
                    let level = log.get("type").and_then(|v| v.as_str()).unwrap_or("log");
                    let text = log.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    console_output.push_str(&format!(
                        "{} {}\n",
                        color::console_level_prefix(level),
                        text
                    ));
                }
                if console_output.ends_with('\n') {
                    console_output.pop();
                }
                print_with_boundaries(&console_output, origin, opts);
            } else {
                for log in logs {
                    let level = log.get("type").and_then(|v| v.as_str()).unwrap_or("log");
                    let text = log.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    println!("{} {}", color::console_level_prefix(level), text);
                }
            }
            return;
        }
        // Errors
        if let Some(errors) = data.get("errors").and_then(|v| v.as_array()) {
            for err in errors {
                let msg = err.get("message").and_then(|v| v.as_str()).unwrap_or("");
                println!("{} {}", color::error_indicator(), msg);
            }
            return;
        }
        // Cookies
        if let Some(cookies) = data.get("cookies").and_then(|v| v.as_array()) {
            for cookie in cookies {
                let name = cookie.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let value = cookie.get("value").and_then(|v| v.as_str()).unwrap_or("");
                println!("{}={}", name, value);
            }
            return;
        }
        // Network requests
        if let Some(requests) = data.get("requests").and_then(|v| v.as_array()) {
            // Stamp the page these requests were read from, mirroring `eval @ url`,
            // so a read against a drifted/wrong tab is obvious (issue #8.1).
            if let Some(o) = data
                .get("origin")
                .and_then(|v| v.as_str())
                .filter(|o| !o.is_empty())
            {
                eprintln!("network @ {o}");
            }
            if requests.is_empty() {
                println!("No requests captured");
            } else {
                for req in requests {
                    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("GET");
                    let url = req.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let resource_type = req
                        .get("resourceType")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let request_id = req.get("requestId").and_then(|v| v.as_str()).unwrap_or("");
                    let status = req.get("status").and_then(|v| v.as_i64());
                    match status {
                        Some(s) => println!(
                            "[{}] {} {} ({}) {}",
                            request_id, method, url, resource_type, s
                        ),
                        None => println!("[{}] {} {} ({})", request_id, method, url, resource_type),
                    }
                }
            }
            return;
        }
        // Cleared (cookies, console, or request log)
        if let Some(cleared) = data.get("cleared").and_then(|v| v.as_bool()) {
            if cleared {
                let label = match action {
                    Some("cookies_clear") => "Cookies cleared",
                    Some("console") => "Console log cleared",
                    _ => "Request log cleared",
                };
                println!("{} {}", color::success_indicator(), label);
                return;
            }
        }
        // Bounding box
        if let Some(box_data) = data.get("box") {
            println!(
                "{}",
                serde_json::to_string_pretty(box_data).unwrap_or_default()
            );
            return;
        }
        // Element styles
        if let Some(elements) = data.get("elements").and_then(|v| v.as_array()) {
            for (i, el) in elements.iter().enumerate() {
                let tag = el.get("tag").and_then(|v| v.as_str()).unwrap_or("?");
                let text = el.get("text").and_then(|v| v.as_str()).unwrap_or("");
                println!("[{}] {} \"{}\"", i, tag, text);

                if let Some(box_data) = el.get("box") {
                    let w = box_data.get("width").and_then(|v| v.as_i64()).unwrap_or(0);
                    let h = box_data.get("height").and_then(|v| v.as_i64()).unwrap_or(0);
                    let x = box_data.get("x").and_then(|v| v.as_i64()).unwrap_or(0);
                    let y = box_data.get("y").and_then(|v| v.as_i64()).unwrap_or(0);
                    println!("    box: {}x{} at ({}, {})", w, h, x, y);
                }

                if let Some(styles) = el.get("styles") {
                    let font_size = styles
                        .get("fontSize")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let font_weight = styles
                        .get("fontWeight")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let font_family = styles
                        .get("fontFamily")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let color = styles.get("color").and_then(|v| v.as_str()).unwrap_or("");
                    let bg = styles
                        .get("backgroundColor")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let radius = styles
                        .get("borderRadius")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    println!("    font: {} {} {}", font_size, font_weight, font_family);
                    println!("    color: {}", color);
                    println!("    background: {}", bg);
                    if radius != "0px" {
                        println!("    border-radius: {}", radius);
                    }
                }
                println!();
            }
            return;
        }
        // Closed (browser or tab)
        if data.get("closed").is_some() {
            let label = match action {
                Some("tab_close") => {
                    if let Some(closed_id) = data.get("tabId").and_then(|v| v.as_str()) {
                        println!("{} Tab [{}] closed", color::success_indicator(), closed_id);
                        return;
                    }
                    "Tab closed"
                }
                _ => "Browser closed",
            };
            println!("{} {}", color::success_indicator(), label);
            return;
        }
        // Started actions (profiling, HAR, recording)
        if let Some(started) = data.get("started").and_then(|v| v.as_bool()) {
            if started {
                match action {
                    Some("profiler_start") => {
                        println!("{} Profiling started", color::success_indicator());
                    }
                    Some("har_start") => {
                        println!("{} HAR recording started", color::success_indicator());
                    }
                    _ => {
                        if let Some(path) = data.get("path").and_then(|v| v.as_str()) {
                            println!("{} Recording started: {}", color::success_indicator(), path);
                        } else {
                            println!("{} Recording started", color::success_indicator());
                        }
                    }
                }
                return;
            }
        }
        // Recording restart (has "stopped" field - from recording_restart action)
        if data.get("stopped").is_some() {
            let path = data
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            if let Some(prev_path) = data.get("previousPath").and_then(|v| v.as_str()) {
                println!(
                    "{} Recording restarted: {} (previous saved to {})",
                    color::success_indicator(),
                    path,
                    prev_path
                );
            } else {
                println!("{} Recording started: {}", color::success_indicator(), path);
            }
            return;
        }
        // Recording stop (has "frames" field - from recording_stop action)
        if data.get("frames").is_some() {
            if let Some(path) = data.get("path").and_then(|v| v.as_str()) {
                if let Some(error) = data.get("error").and_then(|v| v.as_str()) {
                    println!(
                        "{} Recording saved to {} - {}",
                        color::warning_indicator(),
                        path,
                        error
                    );
                } else {
                    println!("{} Recording saved to {}", color::success_indicator(), path);
                }
            } else {
                println!("{} Recording stopped", color::success_indicator());
            }
            return;
        }
        // Download response (has "suggestedFilename" or "filename" field)
        if data.get("suggestedFilename").is_some() || data.get("filename").is_some() {
            if let Some(path) = data.get("path").and_then(|v| v.as_str()) {
                let filename = data
                    .get("suggestedFilename")
                    .or_else(|| data.get("filename"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if filename.is_empty() {
                    println!(
                        "{} Downloaded to {}",
                        color::success_indicator(),
                        color::green(path)
                    );
                } else {
                    println!(
                        "{} Downloaded to {} ({})",
                        color::success_indicator(),
                        color::green(path),
                        filename
                    );
                }
                return;
            }
        }
        // Trace stop without path
        if data.get("traceStopped").is_some() {
            println!("{} Trace stopped", color::success_indicator());
            return;
        }
        // Path-based operations (screenshot/pdf/trace/har/download/state/video)
        if let Some(path) = data.get("path").and_then(|v| v.as_str()) {
            match action.unwrap_or("") {
                "screenshot" => {
                    println!(
                        "{} Screenshot saved to {}",
                        color::success_indicator(),
                        color::green(path)
                    );
                    // Stamp which page was captured (mirrors `eval @ url`) so a
                    // screenshot of the wrong/drifted tab is obvious (issue #8.1).
                    if let Some(o) = data
                        .get("origin")
                        .and_then(|v| v.as_str())
                        .filter(|o| !o.is_empty())
                    {
                        eprintln!("screenshot @ {o}");
                    }
                    if let Some(annotations) = data.get("annotations").and_then(|v| v.as_array()) {
                        // Cap the printed legend on dense pages (it can be
                        // hundreds of lines and flood the terminal). The image
                        // still shows every marker; --json returns the full list.
                        const LEGEND_CAP: usize = 40;
                        let total = annotations.len();
                        for ann in annotations.iter().take(LEGEND_CAP) {
                            let num = ann.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
                            let ref_id = ann.get("ref").and_then(|r| r.as_str()).unwrap_or("");
                            let role = ann.get("role").and_then(|r| r.as_str()).unwrap_or("");
                            let name = ann.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            if name.is_empty() {
                                println!(
                                    "   {} @{} {}",
                                    color::dim(&format!("[{}]", num)),
                                    ref_id,
                                    role,
                                );
                            } else {
                                println!(
                                    "   {} @{} {} {:?}",
                                    color::dim(&format!("[{}]", num)),
                                    ref_id,
                                    role,
                                    name,
                                );
                            }
                        }
                        if total > LEGEND_CAP {
                            println!(
                                "   {}",
                                color::dim(&format!(
                                    "… and {} more markers (shown in the image; --json for the full list)",
                                    total - LEGEND_CAP
                                ))
                            );
                        }
                    }
                }
                "pdf" => println!(
                    "{} PDF saved to {}",
                    color::success_indicator(),
                    color::green(path)
                ),
                "trace_stop" => println!(
                    "{} Trace saved to {}",
                    color::success_indicator(),
                    color::green(path)
                ),
                "profiler_stop" => println!(
                    "{} Profile saved to {} ({} events)",
                    color::success_indicator(),
                    color::green(path),
                    data.get("eventCount").and_then(|c| c.as_u64()).unwrap_or(0)
                ),
                "har_stop" => println!(
                    "{} HAR saved to {} ({} requests)",
                    color::success_indicator(),
                    color::green(path),
                    data.get("requestCount")
                        .and_then(|c| c.as_u64())
                        .unwrap_or(0)
                ),
                "download" | "waitfordownload" => println!(
                    "{} Download saved to {}",
                    color::success_indicator(),
                    color::green(path)
                ),
                "video_stop" => println!(
                    "{} Video saved to {}",
                    color::success_indicator(),
                    color::green(path)
                ),
                "state_save" => println!(
                    "{} State saved to {}",
                    color::success_indicator(),
                    color::green(path)
                ),
                "state_load" => {
                    if let Some(note) = data.get("note").and_then(|v| v.as_str()) {
                        println!("{}", note);
                    }
                    println!(
                        "{} State path set to {}",
                        color::success_indicator(),
                        color::green(path)
                    );
                }
                // video_start and other commands that provide a path with a note
                "video_start" => {
                    if let Some(note) = data.get("note").and_then(|v| v.as_str()) {
                        println!("{}", note);
                    }
                    println!("Path: {}", path);
                }
                _ => println!(
                    "{} Saved to {}",
                    color::success_indicator(),
                    color::green(path)
                ),
            }
            return;
        }

        // State list
        if let Some(files) = data.get("files").and_then(|v| v.as_array()) {
            if let Some(dir) = data.get("directory").and_then(|v| v.as_str()) {
                println!("{}", color::bold(&format!("Saved states in {}", dir)));
            }
            if files.is_empty() {
                println!("{}", color::dim("  No state files found"));
            } else {
                for file in files {
                    let filename = file.get("filename").and_then(|v| v.as_str()).unwrap_or("");
                    let size = file.get("size").and_then(|v| v.as_i64()).unwrap_or(0);
                    let modified = file.get("modified").and_then(|v| v.as_str()).unwrap_or("");
                    let encrypted = file
                        .get("encrypted")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let size_str = if size > 1024 {
                        format!("{:.1}KB", size as f64 / 1024.0)
                    } else {
                        format!("{}B", size)
                    };
                    let date_str = modified.split('T').next().unwrap_or(modified);
                    let enc_str = if encrypted { " [encrypted]" } else { "" };
                    println!(
                        "  {} {}",
                        filename,
                        color::dim(&format!("({}, {}){}", size_str, date_str, enc_str))
                    );
                }
            }
            return;
        }

        // State rename
        if let Some(true) = data.get("renamed").and_then(|v| v.as_bool()) {
            let old_name = data.get("oldName").and_then(|v| v.as_str()).unwrap_or("");
            let new_name = data.get("newName").and_then(|v| v.as_str()).unwrap_or("");
            println!(
                "{} Renamed {} -> {}",
                color::success_indicator(),
                old_name,
                new_name
            );
            return;
        }

        // State clear
        if let Some(cleared) = data.get("cleared").and_then(|v| v.as_i64()) {
            println!(
                "{} Cleared {} state file(s)",
                color::success_indicator(),
                cleared
            );
            return;
        }

        // State show summary
        if let Some(summary) = data.get("summary") {
            let cookies = summary.get("cookies").and_then(|v| v.as_i64()).unwrap_or(0);
            let origins = summary.get("origins").and_then(|v| v.as_i64()).unwrap_or(0);
            let encrypted = data
                .get("encrypted")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let enc_str = if encrypted { " (encrypted)" } else { "" };
            println!("State file summary{}:", enc_str);
            println!("  Cookies: {}", cookies);
            println!("  Origins with localStorage: {}", origins);
            return;
        }

        // State clean
        if let Some(cleaned) = data.get("cleaned").and_then(|v| v.as_i64()) {
            println!(
                "{} Cleaned {} old state file(s)",
                color::success_indicator(),
                cleaned
            );
            return;
        }

        // Informational note
        if let Some(note) = data.get("note").and_then(|v| v.as_str()) {
            println!("{}", note);
            return;
        }
        // Auth list
        if let Some(profiles) = data.get("profiles").and_then(|v| v.as_array()) {
            if profiles.is_empty() {
                println!("{}", color::dim("No auth profiles saved"));
            } else {
                println!("{}", color::bold("Auth profiles:"));
                for p in profiles {
                    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let url = p.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let user = p.get("username").and_then(|v| v.as_str()).unwrap_or("");
                    println!(
                        "  {} {} {}",
                        color::green(name),
                        color::dim(user),
                        color::dim(url)
                    );
                }
            }
            return;
        }

        // Auth show
        if let Some(profile) = data.get("profile").and_then(|v| v.as_object()) {
            let name = profile.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let url = profile.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let user = profile
                .get("username")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let created = profile
                .get("createdAt")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let last_login = profile.get("lastLoginAt").and_then(|v| v.as_str());
            println!("Name: {}", name);
            println!("URL: {}", url);
            println!("Username: {}", user);
            println!("Created: {}", created);
            if let Some(ll) = last_login {
                println!("Last login: {}", ll);
            }
            return;
        }

        // Auth save/update/login/delete
        if data.get("saved").and_then(|v| v.as_bool()).unwrap_or(false) {
            let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("");
            println!(
                "{} Auth profile '{}' saved",
                color::success_indicator(),
                name
            );
            return;
        }
        if data
            .get("updated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            && !data.get("saved").and_then(|v| v.as_bool()).unwrap_or(false)
        {
            let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("");
            println!(
                "{} Auth profile '{}' updated",
                color::success_indicator(),
                name
            );
            return;
        }
        if data
            .get("loggedIn")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if let Some(title) = data.get("title").and_then(|v| v.as_str()) {
                println!(
                    "{} Logged in as '{}' - {}",
                    color::success_indicator(),
                    name,
                    title
                );
            } else {
                println!("{} Logged in as '{}'", color::success_indicator(), name);
            }
            return;
        }
        if data
            .get("deleted")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            if let Some(name) = data.get("name").and_then(|v| v.as_str()) {
                println!(
                    "{} Auth profile '{}' deleted",
                    color::success_indicator(),
                    name
                );
                return;
            }
        }

        // Confirmation required (for orchestrator use)
        if data
            .get("confirmation_required")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let category = data.get("category").and_then(|v| v.as_str()).unwrap_or("");
            let description = data
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let cid = data
                .get("confirmation_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            println!("Confirmation required:");
            println!("  {}: {}", category, description);
            println!("  Run: chrome-use confirm {}", cid);
            println!("  Or:  chrome-use deny {}", cid);
            return;
        }
        if data
            .get("confirmed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            println!("{} Action confirmed", color::success_indicator());
            return;
        }
        if data
            .get("denied")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            println!("{} Action denied", color::success_indicator());
            return;
        }

        // Default success
        println!("{} Done", color::success_indicator());
    } else {
        // Success response with no data payload — still confirm the command ran
        // instead of printing nothing (a silent exit 0 looks like a no-op and
        // hides whether anything happened).
        println!("{} Done", color::success_indicator());
    }

    print_warning(resp);
}

fn print_warning(resp: &Response) {
    if let Some(ref warning) = resp.warning {
        eprintln!("{} {}", color::warning_indicator(), warning);
    }
}

/// Print command-specific help. Returns true if help was printed, false if command unknown.
pub fn print_command_help(command: &str) -> bool {
    let help = match command {
        // === Navigation ===
        "open" | "goto" | "navigate" => {
            r##"
chrome-use open - Launch the browser, optionally navigate

Usage: chrome-use open [url]

Without a URL, launches the browser but stays on about:blank. This lets
you stage state (network routes, cookies, init scripts) before the first
real navigation — useful for SSR debug, auth setup, and capturing fresh
`react suspense` / `vitals` state without noise from a prior page.

With a URL, launches and navigates. If no protocol is provided, https://
is automatically prepended.

The `goto` and `navigate` aliases still require a URL.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session
  --headers <json>     Set HTTP headers (scoped to this origin)
  --headed             Show browser window (default; headless is forbidden — it's a bot tell)
  --window <mode>      dedicated = open agent tabs in a separate window (same
                       profile) so they don't clutter your active window;
                       user (default) = interleave into your current window.
                       Extension-relay path only; applies when the session starts.
  --enable react-devtools   Inject the React DevTools hook before any page JS
  --init-script <path>      Register a page init script (repeatable)

Examples:
  chrome-use open                     # Launch, no nav
  chrome-use open example.com
  chrome-use open https://github.com
  chrome-use open localhost:3000
  chrome-use open api.example.com --headers '{"Authorization": "Bearer token"}'
    # ^ Headers only sent to api.example.com, not other domains

  # Pre-navigation setup in one turn:
  chrome-use batch \
    '["open"]' \
    '["network","route","*","--abort","--resource-type","script"]' \
    '["navigate","http://localhost:3000/target"]'
"##
        }
        "back" => {
            r##"
chrome-use back - Navigate back in history

Usage: chrome-use back

Goes back one page in the browser history, equivalent to clicking
the browser's back button.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use back
"##
        }
        "forward" => {
            r##"
chrome-use forward - Navigate forward in history

Usage: chrome-use forward

Goes forward one page in the browser history, equivalent to clicking
the browser's forward button.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use forward
"##
        }
        "reload" => {
            r##"
chrome-use reload - Reload the current page

Usage: chrome-use reload

Reloads the current page, equivalent to pressing F5 or clicking
the browser's reload button.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use reload
"##
        }

        // === Core Actions ===
        "click" => {
            r##"
chrome-use click - Click an element or a coordinate

Usage: chrome-use click <selector> [--new-tab]
       chrome-use click <x> <y> | <x>,<y> | --coords <x>,<y>

Clicks on the specified element. The selector can be a CSS selector,
XPath, or an element reference from snapshot (e.g., @e1).

A bare-number argument is treated as a viewport coordinate, not a
selector — `click 449 320` clicks the pixel point (no element needed).

Options:
  --coords <x>,<y>     Click a viewport coordinate (explicit form)
  --new-tab            Open link in a new tab instead of navigating current tab
                       (only works on elements with href attribute)

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use click "#submit-button"
  chrome-use click @e1
  chrome-use click "button.primary"
  chrome-use click "//button[@type='submit']"
  chrome-use click @e3 --new-tab
"##
        }
        "dblclick" => {
            r##"
chrome-use dblclick - Double-click an element

Usage: chrome-use dblclick <selector>

Double-clicks on the specified element. Useful for text selection
or triggering double-click handlers.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use dblclick "#editable-text"
  chrome-use dblclick @e5
"##
        }
        "fill" => {
            r##"
chrome-use fill - Clear and fill an input field

Usage: chrome-use fill <selector> <text>
       chrome-use fill <selector> --file <path>
       chrome-use fill <selector> --stdin

Clears the field and fills it with the text, replacing existing content.
Works on rich editors too (issue #41): CodeMirror 5, Monaco, ProseMirror and
plain contenteditable are detected and set via their own API / input events,
not a raw `.value` write — and the response echoes which `engine` was used.
For framework inputs (React/Vue/Angular) the value goes through the native
setter so the form registers it (no more "pristine" Save no-ops).

Options:
  --file <path>        Read the value from a UTF-8 file (large/multiline text,
                       backticks/quotes/newlines/non-ASCII — no shell escaping)
  --stdin              Read the value from stdin

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use fill "#email" "user@example.com"
  chrome-use fill @e3 "Hello World"
  chrome-use fill ".CodeMirror" --file ./article.md     # set a CodeMirror editor
  cat post.md | chrome-use fill @e7 --stdin
"##
        }
        "type" => {
            r##"
chrome-use type - Type text into an element

Usage: chrome-use type <selector> <text>

Types text into the specified element character by character.
Unlike fill, this does not clear existing content first.

Options:
  --key-events         Send real per-character keyDown/keyUp instead of
  (alias --keys)       Input.insertText. Use for autocomplete / combobox fields
                       that only react to key events — e.g. a postal-code box
                       that auto-fills city/prefecture, or Google Places.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use type "#search" "hello"
  chrome-use type @e2 "additional text"
  chrome-use type @e5 "201-0001" --key-events   # trigger the address autocomplete

See Also:
  For typing into contenteditable editors (Lexical, ProseMirror, etc.)
  without a selector, use 'keyboard type' instead:
    chrome-use keyboard type "# My Heading"
"##
        }
        "hover" => {
            r##"
chrome-use hover - Hover over an element

Usage: chrome-use hover <selector>

Moves the mouse to hover over the specified element. Useful for
triggering hover states or dropdown menus.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use hover "#dropdown-trigger"
  chrome-use hover @e4
"##
        }
        "focus" => {
            r##"
chrome-use focus - Focus an element

Usage: chrome-use focus <selector>

Sets keyboard focus to the specified element.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use focus "#input-field"
  chrome-use focus @e2
"##
        }
        "check" => {
            r##"
chrome-use check - Check a checkbox

Usage: chrome-use check <selector>

Checks a checkbox element. If already checked, no action is taken.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use check "#terms-checkbox"
  chrome-use check @e7
"##
        }
        "uncheck" => {
            r##"
chrome-use uncheck - Uncheck a checkbox

Usage: chrome-use uncheck <selector>

Unchecks a checkbox element. If already unchecked, no action is taken.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use uncheck "#newsletter-opt-in"
  chrome-use uncheck @e8
"##
        }
        "select" => {
            r##"
chrome-use select - Select a dropdown option

Usage: chrome-use select <selector> <value...>

Selects one or more options in a <select> dropdown by value.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use select "#country" "US"
  chrome-use select @e5 "option2"
  chrome-use select "#menu" "opt1" "opt2" "opt3"
"##
        }
        "drag" => {
            r##"
chrome-use drag - Drag and drop

Usage: chrome-use drag <source> <target>

Drags an element from source to target location.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use drag "#draggable" "#drop-zone"
  chrome-use drag @e1 @e2
"##
        }
        "upload" => {
            r##"
chrome-use upload - Upload files

Usage: chrome-use upload <selector> <files...>

Uploads one or more files to a file input element.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use upload "#file-input" ./document.pdf
  chrome-use upload @e3 ./image1.png ./image2.png
"##
        }
        "download" => {
            r##"
chrome-use download - Download a file by clicking an element

Usage: chrome-use download <selector> <path>

Clicks an element that triggers a download and saves the file to the specified path.

Arguments:
  selector             Element to click (CSS selector or @ref)
  path                 Path where the downloaded file will be saved

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use download "#download-btn" ./file.pdf
  chrome-use download @e5 ./report.xlsx
  chrome-use download "a[href$='.zip']" ./archive.zip
"##
        }

        // === Keyboard ===
        "press" | "key" => {
            r##"
chrome-use press - Press a key or key combination

Usage: chrome-use press <key>

Presses a key or key combination. Supports special keys and modifiers.

Aliases: key

Special Keys:
  Enter, Tab, Escape, Backspace, Delete, Space
  ArrowUp, ArrowDown, ArrowLeft, ArrowRight
  Home, End, PageUp, PageDown
  F1-F12

Modifiers (combine with +):
  Control, Alt, Shift, Meta

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use press Enter
  chrome-use press Tab
  chrome-use press Control+a
  chrome-use press Control+Shift+s
  chrome-use press Escape
"##
        }
        "keydown" => {
            r##"
chrome-use keydown - Press a key down (without release)

Usage: chrome-use keydown <key>

Presses a key down without releasing it. Use keyup to release.
Useful for holding modifier keys.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use keydown Shift
  chrome-use keydown Control
"##
        }
        "keyup" => {
            r##"
chrome-use keyup - Release a key

Usage: chrome-use keyup <key>

Releases a key that was pressed with keydown.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use keyup Shift
  chrome-use keyup Control
"##
        }
        "keyboard" => {
            r##"
chrome-use keyboard - Raw keyboard input (no selector needed)

Usage: chrome-use keyboard <subcommand> <text>

Sends keyboard input to whatever element currently has focus.
Unlike 'type' which requires a selector, 'keyboard' operates on
the current focus — essential for contenteditable editors like
Lexical, ProseMirror, CodeMirror, and Monaco.

Subcommands:
  type <text>          Type text character-by-character with real
                       key events (keydown, keypress, keyup per char)
  inserttext <text>    Insert text without key events (like paste)

Note: For key combos (Enter, Control+a), use the 'press' command
directly — it already operates on the current focus.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use keyboard type "Hello, World!"
  chrome-use keyboard type "# My Heading"
  chrome-use keyboard inserttext "pasted content"

Use Cases:
  # Type into a Lexical/ProseMirror contenteditable editor:
  chrome-use click "[contenteditable]"
  chrome-use keyboard type "# My Heading"
  chrome-use press Enter
  chrome-use keyboard type "Some paragraph text"
"##
        }

        // === Scroll ===
        "scroll" => {
            r##"
chrome-use scroll - Scroll the page

Usage: chrome-use scroll [direction] [amount] [options]

Scrolls the page or a specific element in the specified direction.

Without --selector, scroll dispatches a real (isTrusted) mouse wheel at a
viewport coordinate, so it scrolls whatever container is under the pointer —
including cross-origin iframes (Google Payments, Stripe, embedded checkout/KYC)
that plain page scroll can't reach.

Arguments:
  direction            up, down, left, right (default: down)
  amount               Pixels to scroll (default: 300)

Options:
  -s, --selector <sel> CSS selector for a scrollable container (same-origin)
  --at <x,y>           Dispatch the wheel at this viewport pixel (read it from a
                       screenshot) — precise way into a cross-origin iframe
  --frame <n>          Scroll the n-th frame from `chrome-use frames` (wheel at
                       that frame's center)

Without --selector/--at/--frame the wheel lands at the viewport center.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use scroll
  chrome-use scroll down 500
  chrome-use scroll up 200
  chrome-use scroll left 100
  chrome-use scroll down 500 --selector "div.scroll-container"
  chrome-use scroll down 700 --at 640,400      # wheel at a pixel over an iframe
  chrome-use scroll down 700 --frame 2         # scroll frame 2 from `frames`
"##
        }
        "scrollintoview" | "scrollinto" => {
            r##"
chrome-use scrollintoview - Scroll element into view

Usage: chrome-use scrollintoview <selector>

Scrolls the page until the specified element is visible in the viewport.

Aliases: scrollinto

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use scrollintoview "#footer"
  chrome-use scrollintoview @e15
"##
        }

        // === Wait ===
        "wait" => {
            r##"
chrome-use wait - Wait for condition

Usage: chrome-use wait <selector|ms|option>

Waits for an element to appear, a timeout, or other conditions.

Modes:
  <selector>           Wait for element to appear
  <ms>                 Wait for specified milliseconds
  --url <pattern>      Wait for URL to match pattern
  --load <state>       Wait for load state (load, domcontentloaded, networkidle)
  --fn <expression>    Wait for JavaScript expression to be truthy
  --text <text>        Wait for text to appear on page (substring match)
  --download [path]    Wait for a download to complete (optionally save to path)

Download Options (with --download):
  --timeout <ms>       Timeout in milliseconds for download to start

Wait for text to disappear:
  Use --fn or --state hidden to wait for text or elements to go away:
  wait --fn "!document.body.innerText.includes('Loading...')"
  wait "#spinner" --state hidden
  wait @e5 --state detached

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use wait "#loading-spinner"
  chrome-use wait 2000
  chrome-use wait --url "**/dashboard"
  chrome-use wait --load networkidle
  chrome-use wait --fn "window.appReady === true"
  chrome-use wait --text "Welcome back"
  chrome-use wait --download ./file.pdf
  chrome-use wait --download ./report.xlsx --timeout 30000
  chrome-use wait --fn "!document.body.innerText.includes('Loading...')"
"##
        }

        // === Expect (assertion) ===
        "expect" => {
            r##"
chrome-use expect - Assert a condition (pass/fail with exit code)

Usage: chrome-use expect <condition> [--timeout <ms>] [--no-wait] [--not]

The "did it actually work?" gate. Waits up to --timeout for the condition to
hold (poll), then exits: 0 = pass, 1 = condition false, 2 = un-evaluable
(no browser / bad grammar). --json prints {pass, actual, ...} on both outcomes.

Conditions:
  expect <sel|@ref> visible|hidden|gone|present   element state
  expect count <css> <op> <n>                     op: == != > < >= <= (quote > <) or eq/ne/gt/lt/ge/le
  expect text|value <sel|@ref> equals|contains|matches <str>
  expect attr <sel|@ref> <name> equals|contains|matches <str>
  expect url equals|contains|matches <pat>        matches = glob (use --regex for raw regex)
  expect request <url-substr> [--method M] [--status 2xx] [--type xhr] [--count N]
  expect no-errors                                no console errors captured

Flags:
  --timeout <ms>   how long to wait for it to hold (default AGENT_BROWSER_DEFAULT_TIMEOUT)
  --no-wait        evaluate once, don't poll
  --not            invert (pass when the condition is false)
  --regex          treat the match value as a raw regex (text/value/attr/url)

Examples:
  chrome-use click @e8 && chrome-use expect "#toast" visible
  chrome-use expect count ".result" ">=" 1
  chrome-use expect text @e3 contains "Saved"
  chrome-use expect url contains /dashboard
  chrome-use requests --clear && chrome-use click @save && chrome-use expect request /api/save --status 2xx
  chrome-use expect no-errors

Note: `expect request` only sees requests captured AFTER tracking is on — run
`requests --clear` (or any network command) before the action that fires it.
`expect no-errors` needs console capture active (run `console` once to enable).
"##
        }

        // === Extract (structured scrape) ===
        "extract" => {
            r##"
chrome-use extract - Scrape structured JSON from the page

Usage:
  chrome-use extract --schema <json>        inline schema
  chrome-use extract --schema-file <path>   schema from a file
  chrome-use extract --stdin                schema from stdin (heredoc)
  [--frame <index|url|@ref|css>]            scope to a child frame (leading flag)

One declarative call instead of N find/get round-trips or hand-written eval.
Compiled to a single page evaluation; returns a JSON array (with `rows`) or a
single object.

Schema:
  {
    "rows": "<css>",        // optional: repeating container → array; omit → one object on the document
    "fields": {
      "name": ".title",                          // shorthand: css → trimmed text
      "price": { "sel": ".price", "get": "text" },
      "href":  { "sel": "a", "get": "@href" },   // @attr
      "html":  { "sel": ".body", "get": "html" },
      "value": { "sel": "input", "get": "value" },
      "tags":  { "sel": ".tag", "get": "text", "all": true }   // array of every match
    }
  }
  get = text (default) | @<attr> | html | value.  sel "" or omitted = the row root itself.

Examples:
  chrome-use extract --schema '{"rows":".product","fields":{"name":".name","price":".price"}}'
  chrome-use extract --schema '{"fields":{"title":"h1","canonical":{"sel":"link[rel=canonical]","get":"@href"}}}'
  cat schema.json | chrome-use extract --stdin
"##
        }

        // === Friction log ===
        "friction" => {
            r##"
chrome-use friction - What's been painful to drive (local failure log)

Usage:
  chrome-use friction            aggregated summary (by command / category / host)
  chrome-use friction --json     machine-readable aggregation
  chrome-use friction --clear    reset the log

Every command that genuinely FAILS is auto-recorded to a local JSONL log
(~/.chrome-use/friction.jsonl) with its error category and the page host. This
turns "what should we improve" into real usage data instead of guesswork.

Privacy: LOCAL ONLY — never uploaded. Records the host (e.g. example.com), never
the full URL/query/params. Opt out entirely: AGENT_BROWSER_NO_FRICTION_LOG=1.
"##
        }

        // === Form fill ===
        "form" => {
            r##"
chrome-use form fill - Fill a whole form in one call

Usage:
  chrome-use form fill --map <json> [--submit <selector|text>]
  chrome-use form fill --map-file <path> | --stdin   [--submit ...]

Fills every field from a {label-or-selector: value} map in one shot, dispatching
the right control type, then optionally clicks a submit button and returns
per-field status + any inline validation errors (the alerts `snapshot -i` shows).
Keys match a <label>, aria-label, placeholder, name, or a CSS selector.
Values: string → text/select/radio; true/false → checkbox.

Standard controls (input/select/textarea/checkbox/radio/contenteditable) only —
for rich editors (DraftJS/Monaco/CodeMirror) fill those fields individually with
`fill`, which handles them.

Examples:
  chrome-use form fill --map '{"Email":"a@b.com","Country":"US","Subscribe":true}'
  chrome-use form fill --map '{"#email":"a@b.com"}' --submit "Sign up"

Returns { filled:[{key,ok,type}], submitted, errors:[...] } — use --json for it.
"##
        }

        // === Screenshot/PDF ===
        "screenshot" => {
            r##"
chrome-use screenshot - Take a screenshot

Usage: chrome-use screenshot [selector] [path]

Captures a screenshot of the current page. If no path is provided,
saves to a temporary directory with a generated filename.
Headless Chromium screenshots hide native scrollbars for consistent image output.
Pass --hide-scrollbars false when launching to keep native scrollbars visible.

Options:
  --full, -f           Capture full page (not just viewport)
  [selector]           Capture just an element (CSS or @ref), e.g. `screenshot ".header" h.png`
  --clip <x,y,w,h>     Capture a pixel region, e.g. `screenshot --clip 0,0,200,40 corner.png`
  --max-width <px>     Downscale so the image's width ≤ px (preserves aspect)
  --max-height <px>    Downscale so the image's height ≤ px
  --scale <0..1>       Downscale by a factor, e.g. 0.5 (DPR-1, so screenshot px
                       line up 1:1 with `click x y`)
                       Default: capped at 2000px longest edge unless overridden
                       (AGENT_BROWSER_SCREENSHOT_MAX_EDGE; 0 disables). Annotated
                       shots are never downscaled, so ref overlays stay aligned.
  --annotate           Overlay numbered labels on interactive elements.
                       Each label [N] corresponds to ref @eN from snapshot.
                       Prints a legend mapping labels to element roles/names.
                       With --json, annotations are included in the response.
                       Supported on Chromium and Lightpanda.
  --screenshot-dir <path>  Default output directory for screenshots
                       (or AGENT_BROWSER_SCREENSHOT_DIR env)
  --screenshot-quality <0-100>  JPEG quality (0-100, only applies to jpeg format)
                       (or AGENT_BROWSER_SCREENSHOT_QUALITY env)
  --screenshot-format <fmt>  Image format: png (default) or jpeg
                       (or AGENT_BROWSER_SCREENSHOT_FORMAT env)

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use screenshot
  chrome-use screenshot ./screenshot.png
  chrome-use screenshot --full ./full-page.png
  chrome-use screenshot ".header .indicator" corner.png  # just one element
  chrome-use screenshot --clip 1600,0,200,40 corner.png  # a pixel region
  chrome-use screenshot --scale 0.5 ./half.png   # DPR-1: screenshot px == click px
  chrome-use screenshot --max-width 1400 ./shot.png  # cap width for image readers
  chrome-use screenshot --annotate              # Labeled screenshot + legend
  chrome-use screenshot --annotate ./page.png   # Save annotated screenshot
  chrome-use screenshot --annotate --json       # JSON output with annotations
  chrome-use screenshot --screenshot-dir ./shots # Save to custom directory
  chrome-use screenshot --screenshot-format jpeg --screenshot-quality 80
"##
        }
        "pdf" => {
            r##"
chrome-use pdf - Save page as PDF

Usage: chrome-use pdf <path>

Saves the current page as a PDF file.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use pdf ./page.pdf
  chrome-use pdf ~/Documents/report.pdf
"##
        }

        // === Snapshot ===
        "snapshot" => {
            r##"
chrome-use snapshot - Get accessibility tree snapshot

Usage: chrome-use snapshot [options]

Returns an accessibility tree representation of the page with element
references (like @e1, @e2) that can be used in subsequent commands.
Designed for AI agents to understand page structure.

Options:
  -i, --interactive    Only include interactive elements
  -u, --urls           Include href URLs for link elements
  -c, --compact        Remove empty structural elements
  -d, --depth <n>      Limit tree depth
  -s, --selector <sel> Scope snapshot to CSS selector
  -f, --filter <regex> Keep only lines matching <regex> (case-insensitive) + their
                       ancestor context; refs preserved. For desktop-shell apps
                       (Synology DSM, NAS/router panels) where one snapshot holds
                       many windows. e.g. -f "SSH|端口|应用|确定"

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use snapshot
  chrome-use snapshot -i
  chrome-use snapshot -i --urls
  chrome-use snapshot --compact --depth 5
  chrome-use snapshot -s "#main-content"
"##
        }

        // === Eval ===
        "eval" => {
            r##"
chrome-use eval - Execute JavaScript

Usage: chrome-use eval [options] <script>

Executes JavaScript code in the browser context and returns the result.

Runs in the MAIN frame by default. Use --frame to target a child frame's
context (see `chrome-use frames` for the index/url list).

Options:
  -b, --base64         Decode script from base64 (avoids shell escaping issues)
  --stdin              Read script from stdin (useful for heredocs/multiline)
  --file <path>        Read script from a file (verbatim UTF-8; no shell mangling)
  --frame <f>          Run in a child frame instead of the main frame. <f> is a
                       frame index (from `chrome-use frames`), a URL substring, or
                       an iframe @ref/CSS selector. Cross-origin (out-of-process)
                       frames run in their own MAIN world; same-process in-page
                       frames run in an isolated world (DOM visible, page globals
                       not).

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use eval "document.title"
  chrome-use eval "window.location.href"
  chrome-use eval "document.querySelectorAll('a').length"
  chrome-use eval -b "ZG9jdW1lbnQudGl0bGU="
  chrome-use eval --frame accounts.google.com "location.href"  # into a child frame
  chrome-use eval --frame 1 "document.body.innerText"          # frame #1 from `frames`

  # Read from stdin with heredoc
  cat <<'EOF' | chrome-use eval --stdin
  const links = document.querySelectorAll('a');
  links.length;
  EOF
"##
        }

        // === Close ===
        "close" | "quit" | "exit" => {
            r##"
chrome-use close - Close the browser

Usage: chrome-use close [options]

Closes the browser instance for the current session.

Aliases: quit, exit

Options:
  --all                Close all active sessions

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use close
  chrome-use close --session mysession
  chrome-use close --all
"##
        }

        // === Inspect ===
        "inspect" => {
            r##"
chrome-use inspect - Open Chrome DevTools for the active page

Starts a local WebSocket proxy and opens Chrome's DevTools frontend in your
default browser. The proxy routes DevTools traffic through the daemon's
existing CDP connection, so both DevTools and chrome-use commands work
simultaneously.

Usage: chrome-use inspect

Examples:
  chrome-use open example.com
  chrome-use inspect          # opens DevTools in your browser
  chrome-use click "Submit"   # commands still work while DevTools is open
"##
        }

        // === Get ===
        "get" => {
            r##"
chrome-use get - Retrieve information from elements or page

Usage: chrome-use get <subcommand> [args]

Retrieves various types of information from elements or the page.

Subcommands:
  text [selector]            Element text; no selector = WHOLE PAGE, all frames
  text --main                Main-content text only (skip nav/header/sidebar)
  text --pierce              Read through CLOSED shadow DOM (injected panels)
  html <selector>            Get inner HTML of element
  value <selector>           Get value of input element
  attr <selector> <name>     Get attribute value
  title                      Get page title
  url                        Get current URL
  count <selector>           Count matching elements
  box <selector>             Get bounding box (x, y, width, height)
  styles <selector>          Get computed styles of elements
  cdp-url                    Get Chrome DevTools Protocol WebSocket URL

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use get text                  # whole page across ALL frames (default)
  chrome-use get text @e1              # one element
  chrome-use get text --main           # main content, no nav/sidebar boilerplate
  chrome-use frames                    # list frames + where the text lives
  chrome-use get html "#content"
  chrome-use get value "#email-input"
  chrome-use get attr "#link" href
  chrome-use get title
  chrome-use get url
  chrome-use get count "li.item"
  chrome-use get box "#header"
  chrome-use get styles "button"
  chrome-use get styles @e1
"##
        }

        // === Is ===
        "is" => {
            r##"
chrome-use is - Check element state

Usage: chrome-use is <subcommand> <selector>

Checks the state of an element and returns true/false.

Subcommands:
  visible <selector>   Check if element is visible
  enabled <selector>   Check if element is enabled (not disabled)
  checked <selector>   Check if checkbox/radio is checked

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use is visible "#modal"
  chrome-use is enabled "#submit-btn"
  chrome-use is checked "#agree-checkbox"
"##
        }

        // === Find ===
        "find" => {
            r##"
chrome-use find - Find and interact with elements by locator

Usage: chrome-use find <locator> <value> [action] [text]

Finds elements using semantic locators and optionally performs an action.

Locators:
  role <role>              Find by ARIA role (--name <n>, --exact)
  text <text>              Find by text content (--exact)
  label <label>            Find by associated label (--exact)
  placeholder <text>       Find by placeholder text (--exact)
  alt <text>               Find by alt text (--exact)
  title <text>             Find by title attribute (--exact)
  testid <id>              Find by data-testid attribute
  first <selector>         First matching element
  last <selector>          Last matching element
  nth <index> <selector>   Nth matching element (0-based)

Actions (default: click):
  click, fill, type, hover, focus, check, uncheck

Options:
  --name <name>        Filter role by accessible name
  --exact              Require exact text match

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use find role button click --name Submit
  chrome-use find text "Sign In" click
  chrome-use find label "Email" fill "user@example.com"
  chrome-use find placeholder "Search..." type "query"
  chrome-use find testid "login-form" click
  chrome-use find first "li.item" click
  chrome-use find nth 2 ".card" hover
"##
        }

        // === Mouse ===
        "mouse" => {
            r##"
chrome-use mouse - Low-level mouse operations

Usage: chrome-use mouse <subcommand> [args]

Performs low-level mouse operations for precise control.

Subcommands:
  move <x> <y>         Move mouse to coordinates
  down [button]        Press mouse button (left, right, middle)
  up [button]          Release mouse button
  wheel <dy> [dx]      Scroll mouse wheel

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use mouse move 100 200
  chrome-use mouse down
  chrome-use mouse up
  chrome-use mouse down right
  chrome-use mouse wheel 100
  chrome-use mouse wheel -50 0
"##
        }

        // === Set ===
        "set" => {
            r##"
chrome-use set - Configure browser settings

Usage: chrome-use set <setting> [args]

Configures various browser settings and emulation options.

Settings:
  viewport <w> <h> [scale]   Set viewport size (scale = deviceScaleFactor, e.g. 2 for retina)
  device <name>              Emulate device (e.g., "iPhone 12")
  geo <lat> <lng>            Set geolocation
  offline [on|off]           Toggle offline mode
  headers <json>             Set extra HTTP headers
  credentials <user> <pass>  Set HTTP authentication
  media [dark|light]         Set color scheme preference
        [reduced-motion]     Enable reduced motion

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use set viewport 1920 1080
  chrome-use set viewport 1920 1080 2    # 2x retina
  chrome-use set device "iPhone 12"
  chrome-use set geo 37.7749 -122.4194
  chrome-use set offline on
  chrome-use set headers '{"X-Custom": "value"}'
  chrome-use set credentials admin secret123
  chrome-use set media dark
  chrome-use set media light reduced-motion
"##
        }

        // === Network ===
        "network" => {
            r##"
chrome-use network - Network interception and monitoring

Usage: chrome-use network <subcommand> [args]

Intercept, mock, or monitor network requests.

Subcommands:
  route <url> [options]      Intercept requests matching URL pattern
    --abort                  Abort matching requests
    --body <json>            Respond with custom body
  unroute [url]              Remove route (all if no URL)
  requests [options]         List captured requests
    --clear                  Clear request log
    --filter <pattern>       Filter by URL pattern
    --type <types>           Filter by resource type (comma-separated: xhr,fetch,document)
    --method <method>        Filter by HTTP method (GET, POST, etc.)
    --status <code>          Filter by status (200, 2xx, 400-499)
  request <requestId>        View full request/response detail (including body)
  har <start|stop> [path]    Record and export a HAR file

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use network route "**/api/*" --abort
  chrome-use network route "**/data.json" --body '{"mock": true}' --status 200 --header X-From=mock
  chrome-use network route "**/api/save" --method POST --set-body '{"x":1}' --set-header Authorization="Bearer test"
  chrome-use network route "**/v1/*" --rewrite-url https://staging.example.com/v1/thing
  chrome-use network route "**/api/me" --edit-status 503 --edit-header X-Env=test --replace 'prod=>staging'  # edit the REAL response
  chrome-use network unroute
  chrome-use network requests
  chrome-use network requests --filter "api"
  chrome-use network requests --type xhr,fetch
  chrome-use network requests --method POST --status 2xx
  chrome-use network requests --clear
  chrome-use network request 1234.5
  chrome-use network har start
  chrome-use network har stop ./capture.har
"##
        }

        // === Storage ===
        "storage" => {
            r##"
chrome-use storage - Manage web storage

Usage: chrome-use storage <type> [operation] [key] [value]

Manage localStorage and sessionStorage.

Types:
  local                localStorage
  session              sessionStorage

Operations:
  get [key]            Get all storage or specific key
  set <key> <value>    Set a key-value pair
  clear                Clear all storage

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use storage local
  chrome-use storage local get authToken
  chrome-use storage local set theme "dark"
  chrome-use storage local clear
  chrome-use storage session get userId
"##
        }

        // === Cookies ===
        "cookies" => {
            r##"
chrome-use cookies - Manage browser cookies

Usage: chrome-use cookies [operation] [args]

Manage browser cookies for the current context.

Operations:
  get                                Get all cookies (default)
  set <name> <value> [options]       Set a cookie with optional properties
  clear                              Clear all cookies

Cookie Set Options:
  --url <url>                        URL for the cookie (allows setting before page load)
  --domain <domain>                  Cookie domain (e.g., ".example.com")
  --path <path>                      Cookie path (e.g., "/api")
  --httpOnly                         Set HttpOnly flag (prevents JavaScript access)
  --secure                           Set Secure flag (HTTPS only)
  --sameSite <Strict|Lax|None>       SameSite policy
  --expires <timestamp>              Expiration time (Unix timestamp in seconds)

Note: If --url, --domain, and --path are all omitted, the cookie will be set
for the current page URL.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  # Simple cookie for current page
  chrome-use cookies set session_id "abc123"

  # Set cookie for a URL before loading it (useful for authentication)
  chrome-use cookies set session_id "abc123" --url https://app.example.com

  # Set secure, httpOnly cookie with domain and path
  chrome-use cookies set auth_token "xyz789" --domain example.com --path /api --httpOnly --secure

  # Set cookie with SameSite policy
  chrome-use cookies set tracking_consent "yes" --sameSite Strict

  # Set cookie with expiration (Unix timestamp)
  chrome-use cookies set temp_token "temp123" --expires 1735689600

  # Get all cookies
  chrome-use cookies

  # Clear all cookies
  chrome-use cookies clear
"##
        }

        // === Tabs ===
        "tab" => {
            r##"
chrome-use tab - Manage browser tabs

Usage: chrome-use tab [operation] [args]

Manage browser tabs in the current window. Stable tab ids look like `t1`,
`t2`, `t3`. An id is never reused within a session, so scripts can keep
referring to the same tab across commands. Optional user-assigned labels
(e.g. `docs`, `app`) are interchangeable with ids everywhere a tab ref is
accepted.

Operations:
  list                       List open tabs with their ids and labels (default)
  new [url]                  Open a new tab
  new --label <name> [url]   Open a new tab with a label like `docs` or `app`
  duplicate [ref]            Natively duplicate a tab (current if no ref given)
  duplicate [ref] --label <name>
                             Natively duplicate and label a tab
  close [ref]                Close a tab (current if no ref given)
  <ref>                      Switch to a tab

Native duplication requires real Chrome connected through the chrome-use
extension. It restores the previously visible foreground tab when complete.
The duplicate becomes chrome-use's internal active tab. There is no URL-based
fallback for launched Chrome, raw CDP, Lightpanda, or cloud providers.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use tab
  chrome-use tab list
  chrome-use tab new
  chrome-use tab new https://example.com
  chrome-use tab new --label docs https://docs.example.com
  chrome-use tab duplicate
  chrome-use tab duplicate docs --label docs-copy
  chrome-use tab t2
  chrome-use tab docs
  chrome-use tab close
  chrome-use tab close t1
  chrome-use tab close docs
"##
        }

        // === Window ===
        "window" => {
            r##"
chrome-use window - Manage browser windows

Usage: chrome-use window <operation>

Manage browser windows.

Operations:
  new                  Open new browser window

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use window new
"##
        }

        // === Frame ===
        "frame" => {
            r##"
chrome-use frame - Switch frame context

Usage: chrome-use frame <selector|main>

Switch to an iframe or back to the main frame.

Arguments:
  <selector>           CSS selector for iframe
  main                 Switch back to main frame

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use frame "#embed-iframe"
  chrome-use frame "iframe[name='content']"
  chrome-use frame main
"##
        }

        // === Auth ===
        "auth" => {
            r##"
chrome-use auth - Manage authentication profiles

Usage: chrome-use auth <subcommand> [args]

Subcommands:
  save <name>              Save credentials for a login profile
  login <name>             Login using saved credentials (waits for form fields)
  list                     List saved profiles (names and URLs only)
  show <name>              Show profile metadata (no passwords)
  delete <name>            Delete a saved profile

Save Options:
  --url <url>              Login page URL (required)
  --username <user>        Username (required)
  --password <pass>        Password (required unless --password-stdin)
  --password-stdin          Read password from stdin (recommended)
  --username-selector <s>  Custom CSS selector for username field
  --password-selector <s>  Custom CSS selector for password field
  --submit-selector <s>    Custom CSS selector for submit button

Login behavior:
  auth login waits for form selectors to appear before filling/clicking.
  Selector wait timeout follows the default action timeout.

Global Options:
  --json                   Output as JSON
  --session <name>         Use specific session

Examples:
  echo "pass" | chrome-use auth save github --url https://github.com/login --username user --password-stdin
  chrome-use auth save github --url https://github.com/login --username user --password pass
  chrome-use auth login github
  chrome-use auth list
  chrome-use auth show github
  chrome-use auth delete github
"##
        }

        // === Confirm/Deny ===
        "confirm" | "deny" => {
            r##"
chrome-use confirm/deny - Approve or deny pending actions

Usage:
  chrome-use confirm <confirmation-id>
  chrome-use deny <confirmation-id>

When --confirm-actions is set, certain action categories return a
confirmation_required response with a confirmation ID. Use confirm/deny
to approve or reject the action.

Pending confirmations auto-deny after 60 seconds.

Examples:
  chrome-use confirm c_8f3a1234
  chrome-use deny c_8f3a1234
"##
        }

        // === Dialog ===
        "dialog" => {
            r##"
chrome-use dialog - Handle browser dialogs

Usage: chrome-use dialog <accept|dismiss|status> [text]

Respond to or check for browser dialogs (alert, confirm, prompt).

Operations:
  accept [text]        Accept dialog, optionally with prompt text
  dismiss              Dismiss/cancel dialog
  status               Check if a dialog is currently open

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use dialog accept
  chrome-use dialog accept "my input"
  chrome-use dialog dismiss
  chrome-use dialog status
"##
        }

        // === Trace ===
        "trace" => {
            r##"
chrome-use trace - Record execution trace

Usage: chrome-use trace <operation> [path]

Record a Chrome DevTools trace for debugging.

Operations:
  start [path]         Start recording trace
  stop [path]          Stop recording and save trace

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use trace start
  chrome-use trace start ./my-trace
  chrome-use trace stop
  chrome-use trace stop ./debug-trace.zip
"##
        }

        // === Profile (CDP Tracing) ===
        "profiler" => {
            r##"
chrome-use profiler - Record Chrome DevTools performance profile

Usage: chrome-use profiler <operation> [options]

Record a performance profile using Chrome DevTools Protocol (CDP) Tracing.
The output JSON file can be loaded into Chrome DevTools Performance panel,
Perfetto UI (https://ui.perfetto.dev/), or other trace analysis tools.

Operations:
  start                Start profiling
  stop [path]          Stop profiling and save to file

Start Options:
  --categories <list>  Comma-separated trace categories (default includes
                       devtools.timeline, v8.execute, blink, and others)

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  # Basic profiling
  chrome-use profiler start
  chrome-use navigate https://example.com
  chrome-use click "#button"
  chrome-use profiler stop ./trace.json

  # With custom categories
  chrome-use profiler start --categories "devtools.timeline,v8.execute,blink.user_timing"
  chrome-use profiler stop ./custom-trace.json

The output file can be viewed in:
  - Chrome DevTools: Performance panel > Load profile
  - Perfetto: https://ui.perfetto.dev/
"##
        }

        // === Record (video) ===
        "record" => {
            r##"
chrome-use record - Record browser session to video

Usage: chrome-use record start <path.webm> [url]
       chrome-use record stop
       chrome-use record restart <path.webm> [url]

Record the browser to a WebM video file.
Creates a fresh browser context but preserves cookies and localStorage.
If no URL is provided, automatically navigates to your current page.

Operations:
  start <path> [url]     Start recording (defaults to current URL if omitted)
  stop                   Stop recording and save video
  restart <path> [url]   Stop current recording (if any) and start a new one

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  # Record from current page (preserves login state)
  chrome-use open https://app.example.com/dashboard
  chrome-use snapshot -i            # Explore and plan
  chrome-use record start ./demo.webm
  chrome-use click @e3              # Execute planned actions
  chrome-use record stop

  # Or specify a different URL
  chrome-use record start ./demo.webm https://example.com

  # Restart recording with a new file (stops previous, starts new)
  chrome-use record restart ./take2.webm
"##
        }

        // === Console/Errors ===
        "console" => {
            r##"
chrome-use console - View console logs

Usage: chrome-use console [--clear] [--limit N]

View browser console output (log, warn, error, info).

Options:
  --clear              Clear console log buffer
  --limit N            Show only the last N entries (newest kept)

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use console
  chrome-use console --limit 20
  chrome-use console --clear
"##
        }
        "errors" => {
            r##"
chrome-use errors - View page errors

Usage: chrome-use errors [--clear]

View JavaScript errors and uncaught exceptions.

Options:
  --clear              Clear error buffer

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use errors
  chrome-use errors --clear
"##
        }

        // === Highlight ===
        "highlight" => {
            r##"
chrome-use highlight - Highlight an element

Usage: chrome-use highlight <selector>

Visually highlights an element on the page for debugging.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use highlight "#target-element"
  chrome-use highlight @e5
"##
        }

        // === Clipboard ===
        "clipboard" => {
            r##"
chrome-use clipboard - Read and write clipboard

Usage: chrome-use clipboard <operation> [text]

Read from or write to the browser clipboard.

Operations:
  read                 Read text from clipboard
  write <text>         Write text to clipboard
  copy                 Copy current selection (simulates Ctrl+C)
  paste                Paste from clipboard (simulates Ctrl+V)

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use clipboard read
  chrome-use clipboard write "Hello, World!"
  chrome-use clipboard copy
  chrome-use clipboard paste
"##
        }

        // === State ===
        "state" => {
            r##"
chrome-use state - Manage browser state

Usage: chrome-use state <operation> [args]

Save, restore, list, and manage browser state (cookies, localStorage, sessionStorage).

Operations:
  save <path>                        Save current state to file
  load <path>                        Load state from file
  list                               List saved state files
  show <filename>                    Show state summary
  rename <old-name> <new-name>       Rename state file
  clear [session-name] [--all]       Clear saved states
  clean --older-than <days>          Delete expired state files

Automatic State Persistence:
  Use --session-name to auto-save/restore state across restarts:
  chrome-use --session-name myapp open https://example.com
  Or set AGENT_BROWSER_SESSION_NAME environment variable.

State Encryption:
  Set AGENT_BROWSER_ENCRYPTION_KEY (64-char hex) for AES-256-GCM encryption.
  Generate a key: openssl rand -hex 32

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use state save ./auth-state.json
  chrome-use state load ./auth-state.json
  chrome-use state list
  chrome-use state show myapp-default.json
  chrome-use state rename old-name new-name
  chrome-use state clear --all
  chrome-use state clean --older-than 7
"##
        }

        // === Session ===
        "session" => {
            r##"
chrome-use session - Manage sessions

Usage: chrome-use session [operation]

Manage isolated browser sessions. Each session has its own browser
instance with separate cookies, storage, and state.

Operations:
  (none)               Show current session name
  list                 List all active sessions

Environment:
  AGENT_BROWSER_SESSION    Default session name

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use session
  chrome-use session list
  chrome-use --session test open example.com
"##
        }

        // === Install ===
        "install" => {
            r##"
chrome-use install - Install browser binaries

Usage: chrome-use install [--with-deps]

Downloads and installs browser binaries required for automation.

Options:
  -d, --with-deps      Also install system dependencies (Linux only)

Examples:
  chrome-use install
  chrome-use install --with-deps
"##
        }

        // === Upgrade ===
        "upgrade" => {
            r##"
chrome-use upgrade - Upgrade to the latest version

Usage: chrome-use upgrade

Detects the current installation method (npm, Homebrew, or Cargo) and runs
the appropriate update command. Displays the version change on success, or
informs you if you are already on the latest version.

Examples:
  chrome-use upgrade
"##
        }

        // === MCP ===
        "mcp" => {
            r##"
chrome-use mcp - Run a stdio MCP server

Usage: chrome-use mcp [--tools <core|all>]

Runs a Model Context Protocol server over stdio (JSON-RPC, newline-delimited)
so MCP-only hosts (Claude Desktop, etc.) can drive chrome-use. Each tool call
delegates to this same binary in `--json` mode, so behavior matches the
normal CLI surface.

Profiles (--tools, default core):
  core   ~12 tools: chrome_use_open, chrome_use_read, chrome_use_snapshot,
         chrome_use_click, chrome_use_fill, chrome_use_type, chrome_use_press,
         chrome_use_eval, chrome_use_wait, chrome_use_back, chrome_use_forward,
         chrome_use_reload
  all    core + an extended set: chrome_use_hover, chrome_use_select,
         chrome_use_screenshot, chrome_use_scroll, chrome_use_tabs,
         chrome_use_extract, chrome_use_expect, chrome_use_find,
         chrome_use_network_route, chrome_use_site, chrome_use_upload,
         chrome_use_download — still not exhaustive CLI parity.

Claude Desktop config (claude_desktop_config.json):
  {
    "mcpServers": {
      "chrome-use": { "command": "chrome-use", "args": ["mcp"] }
    }
  }

  # extended tool profile
  { "mcpServers": { "chrome-use": { "command": "chrome-use", "args": ["mcp", "--tools", "all"] } } }

Examples:
  chrome-use mcp
  chrome-use mcp --tools all
"##
        }

        // === Doctor ===
        "doctor" => {
            r##"
chrome-use doctor - Diagnose and repair your install

Usage: chrome-use doctor [options]

Runs a battery of checks across environment, Chrome install, daemon state,
config files, encryption key, providers, network reachability, and a live
headless browser launch test.

Auto-cleans stale daemon socket/pid/version sidecar files. Destructive
repairs (reinstalling Chrome, purging old state files, generating a missing
encryption key) are gated behind --fix.

Options:
  --offline            Skip network probes
  --quick              Skip the live headless launch test
  --fix                Also run destructive repairs
  --json               JSON output

Exit codes:
  0  All checks pass (warnings OK)
  1  At least one check failed

Examples:
  chrome-use doctor
  chrome-use doctor --offline --quick
  chrome-use doctor --fix
  chrome-use doctor --json
"##
        }

        // === Dashboard ===
        "dashboard" => {
            r##"
chrome-use dashboard - Observability dashboard

Usage: chrome-use dashboard [start|stop] [options]

Manage the observability dashboard, a local web UI that shows live
browser viewports and command activity feeds for all sessions.
The dashboard is bundled into the binary and requires no separate install.

Subcommands:
  start [--port <n>]   Start the dashboard server (default port: 4848)
  stop                 Stop the dashboard server

Running 'chrome-use dashboard' with no subcommand is equivalent to 'dashboard start'.

The dashboard runs as a standalone background process, independent of
browser sessions. All sessions automatically stream to the dashboard.
It works from http://localhost:4848 or a proxied/forwarded URL that
reaches the dashboard server, such as https://dashboard.chrome-use.localhost
or a Coder workspace URL. The browser stays on the dashboard origin;
session tabs, status, and stream traffic are proxied internally, so
session ports do not need to be exposed.

Options:
  --port <n>           Port for the dashboard server (default: 4848)

Global Options:
  --json               Output as JSON

Examples:
  chrome-use dashboard start
  chrome-use dashboard start --port 8080
  chrome-use dashboard stop
"##
        }

        // === Connect ===
        "connect" => {
            r##"
chrome-use connect - Connect to browser via CDP

Usage: chrome-use connect <port|url>

Connects to a running browser instance via Chrome DevTools Protocol (CDP).
This allows controlling browsers, Electron apps, or remote browser services.

Arguments:
  <port>               Local port number (e.g., 9222)
  <url>                Full WebSocket URL (ws://, wss://, http://, https://)

Supported URL formats:
  - Port number: 9222 (connects to http://localhost:9222)
  - WebSocket URL: ws://localhost:9222/devtools/browser/...
  - Remote service: wss://remote-browser.example.com/cdp?token=...

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  # Connect to local Chrome with remote debugging
  # Start Chrome: google-chrome --remote-debugging-port=9222
  chrome-use connect 9222

  # Connect using WebSocket URL from /json/version endpoint
  chrome-use connect "ws://localhost:9222/devtools/browser/abc123"

  # Connect to remote browser service
  chrome-use connect "wss://browser-service.example.com/cdp?token=xyz"

  # After connecting, run commands normally
  chrome-use snapshot
  chrome-use click @e1
"##
        }

        // === Runtime streaming ===
        "stream" => {
            r##"
chrome-use stream - Manage live WebSocket browser streaming

Usage:
  chrome-use stream enable [--port <port>]
  chrome-use stream disable
  chrome-use stream status

Enables or disables the session-scoped WebSocket stream server without restarting
an already-running daemon. If --port is omitted, chrome-use binds an
available localhost port automatically and reports it back.

Notes:
  - 'stream enable' creates the WebSocket server.
  - WebSocket clients trigger frame streaming automatically.
  - 'screencast_start' and 'screencast_stop' still control explicit CDP screencasts.
  - Streaming is always enabled. Set AGENT_BROWSER_STREAM_PORT to bind to a
    specific port instead of the default OS-assigned port.

The WS is BIDIRECTIONAL — the high-throughput way to drive a live/real-time page
(games, canvas apps) instead of one screenshot + one CLI call per action:
  - Server -> client (JSON text frames):
      {"type":"frame","data":"<base64 jpeg>"}   live screencast (~60fps)
      plus status / tabs messages.
  - Client -> server (send JSON text):
      {"type":"input_keyboard","eventType":"keyDown|keyUp","key":" ","code":"Space",
       "windowsVirtualKeyCode":32}
      {"type":"input_mouse","eventType":"mousePressed|mouseReleased|mouseMoved",
       "x":640,"y":360,"button":"left","clickCount":1}
      {"type":"input_touch","eventType":"touchStart|touchEnd","touchPoints":[...]}
  Connect once and run a tight local loop: read frames, send timed input — no
  per-action process spawn, no round-trip. Works over the extension relay too.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use stream status
  chrome-use stream enable
  chrome-use stream enable --port 9223
  chrome-use stream disable
"##
        }

        // === iOS Commands ===
        "tap" => {
            r##"
chrome-use tap - Tap an element (touch gesture)

Usage: chrome-use tap <selector>

Taps an element. This is an alias for 'click' that provides semantic clarity
for touch-based interfaces like iOS Safari.

Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use tap "#submit-button"
  chrome-use tap @e1
  chrome-use -p ios tap "button:has-text('Sign In')"
"##
        }
        "swipe" => {
            r##"
chrome-use swipe - Swipe gesture (iOS)

Usage: chrome-use swipe <direction> [distance]

Performs a swipe gesture on iOS Safari. The direction determines
which way the content moves (swipe up scrolls down, etc.).

Arguments:
  direction    up, down, left, or right
  distance     Optional distance in pixels (default: 300)

Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use -p ios swipe up
  chrome-use -p ios swipe down 500
  chrome-use -p ios swipe left
"##
        }
        "device" => {
            r##"
chrome-use device - Manage iOS simulators

Usage: chrome-use device <subcommand>

Subcommands:
  list    List available iOS simulators

Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use device list
  chrome-use -p ios device list
"##
        }

        "diff" => {
            r##"
chrome-use diff - Compare page states

Subcommands:

  diff snapshot                   Compare current snapshot to last snapshot in session
  diff screenshot --baseline <f>  Visual pixel diff against a baseline image
  diff url <url1> <url2>          Compare two pages

Snapshot Diff:

  Usage: chrome-use diff snapshot [options]

  Options:
    -b, --baseline <file>    Compare against a saved snapshot file
    -s, --selector <sel>     Scope snapshot to a CSS selector or @ref
    -c, --compact            Use compact snapshot format
    -d, --depth <n>          Limit snapshot tree depth

  Without --baseline, compares against the last snapshot taken in this session.

Screenshot Diff:

  Usage: chrome-use diff screenshot --baseline <file> [options]

  Options:
    -b, --baseline <file>    Baseline image to compare against (required)
    -o, --output <file>      Path for the diff image (default: temp dir)
    -t, --threshold <0-1>    Color distance threshold (default: 0.1)
    -s, --selector <sel>     Scope screenshot to element
        --full               Full page screenshot

URL Diff:

  Usage: chrome-use diff url <url1> <url2> [options]

  Options:
    --screenshot             Also compare screenshots (default: snapshot only)
    --full                   Full page screenshots
    --wait-until <strategy>  Navigation wait strategy: load, domcontentloaded, networkidle (default: load)
    -s, --selector <sel>     Scope snapshots to a CSS selector or @ref
    -c, --compact            Use compact snapshot format
    -d, --depth <n>          Limit snapshot tree depth

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  chrome-use diff snapshot
  chrome-use diff snapshot --baseline before.txt
  chrome-use diff screenshot --baseline before.png
  chrome-use diff screenshot --baseline before.png --output diff.png --threshold 0.2
  chrome-use diff url https://staging.example.com https://prod.example.com
  chrome-use diff url https://v1.example.com https://v2.example.com --screenshot
"##
        }

        "batch" => {
            r##"
chrome-use batch - Execute multiple commands sequentially

Usage: chrome-use batch [options] "<cmd1>" "<cmd2>" ...
       echo '<json>' | chrome-use batch [options]

Runs multiple commands in sequence. Commands can be passed as quoted
arguments or piped as JSON via stdin. Results are printed in order,
separated by blank lines (or as a JSON array with --json).

Options:
  --bail               Stop on first error (default: continue all commands)
  --json               Output results as a JSON array

Argument Mode:
  Each quoted argument is a full command string:
  chrome-use batch "open https://example.com" "snapshot -i" "screenshot"

Stdin Mode (JSON):
  A JSON array of string arrays. Each inner array is one command:
  [
    ["open", "https://example.com"],
    ["snapshot", "-i"],
    ["click", "@e1"],
    ["fill", "@e2", "test@example.com"],
    ["screenshot", "result.png"]
  ]

Examples:
  chrome-use batch "open https://example.com" "screenshot"
  chrome-use batch --bail "open https://example.com" "click @e1" "screenshot"
  echo '[["open", "https://example.com"], ["snapshot"]]' | chrome-use batch
  chrome-use batch --bail < commands.json
"##
        }

        "profiles" => {
            r##"
chrome-use profiles - List available Chrome profiles

Usage: chrome-use profiles

Lists all Chrome profiles found in your Chrome user data directory, showing
the directory name and display name for each profile. Use the directory name
with --profile to launch Chrome with that profile's login state.

Global Options:
  --json               Output as JSON

Examples:
  chrome-use profiles
  chrome-use profiles --json
  chrome-use --profile Default open https://gmail.com
"##
        }

        "chat" => {
            r##"
chrome-use chat - Natural language browser control via AI

Usage:
  chrome-use chat <message>         Single-shot: execute instruction and exit
  chrome-use chat                   Interactive REPL (when stdin is a TTY)
  echo "instruction" | chrome-use chat   Piped input

Sends natural language instructions to an AI model that translates them
into chrome-use commands and executes them against the active session.
Requires AI_GATEWAY_API_KEY to be set.

In interactive mode, type "quit", "exit", or "q" to leave the REPL.

Chat Options:
  --model <name>         AI model (or AI_GATEWAY_MODEL env, default: anthropic/claude-sonnet-4.6)
  -v, --verbose          Show tool commands and their raw output
  -q, --quiet            Show only the AI text response (hide tool calls)

Global Options:
  --json                 Structured JSON output per turn
  --session <name>       Target session for commands

Examples:
  chrome-use chat "open google.com and search for cats"
  chrome-use chat "take a screenshot of the current page"
  chrome-use -q chat "summarize this page"
  chrome-use -v chat "fill in the login form with test@example.com"
  chrome-use --model openai/gpt-4o chat "navigate to hacker news"
  chrome-use chat
"##
        }

        "skills" => {
            r##"
chrome-use skills - List and retrieve bundled skill content

Usage: chrome-use skills [subcommand] [options]

Subcommands:
  list                       List all available skills (default)
  get <name> [name...]       Output a skill's full content
  get <name> --full          Include references and templates
  get --all                  Output every skill
  path [name]                Print filesystem path to skill directory

Options:
  --json                     Output as JSON

The skills command serves bundled skill content that always matches the
installed CLI version. Agents should use this to get current instructions
rather than relying on cached copies.

Examples:
  chrome-use skills
  chrome-use skills list
  chrome-use skills get core
  chrome-use skills get core --full
  chrome-use skills get electron --full
  chrome-use skills get --all
  chrome-use skills path core
  chrome-use skills list --json

Environment:
  AGENT_BROWSER_SKILLS_DIR   Override the skills directory path
"##
        }

        _ => return false,
    };
    println!("{}", help.trim());
    true
}

pub fn print_help() {
    println!(
        r#"
chrome-use - fast browser automation CLI for AI agents

Usage: chrome-use <command> [args] [options]

Start here (for AI agents):
  chrome-use skills get core --full

  Skills ship with the CLI (always version-matched) and include workflow
  patterns, ref/selector usage, and copy-paste examples. Prefer this over
  guessing commands from flag docs alone. Specialized skills cover Electron
  apps, Slack, exploratory testing, and cloud browser providers.

  skills [list]                List available skills
  skills get core              Core usage guide (overview + common patterns)
  skills get core --full       Include full command reference and templates
  skills get <name>            Load a specialized skill (electron, slack, ...)
  skills path [name]           Print skill directory path

Core Commands:
  open <url>                 Navigate to URL
  read [url]                 Fetch a URL (or the active tab) as agent-readable text
                             [--raw --require-md --llms <index|full> --outline --filter <t> --timeout <ms>]
  click <sel|x y>            Click element/@ref, or a viewport coordinate
  dblclick <sel>             Double-click element
  type <sel> <text>          Type into element
  fill <sel> <text>          Clear and fill (handles CodeMirror/Monaco/ProseMirror/
                             contenteditable; `--file <path>`/`--stdin` for large text)
  press <key> [--hold <ms>]  Press key (Enter, Tab, Control+a). --hold keeps it
                             down <ms> then releases — precise (in-daemon), for
                             games/charge: `press d --hold 800`
  keydown <key>              Hold a key down (no auto-release) — for games/shortcuts
  keyup <key>                Release a held key. Pair with keydown to hold-to-move:
                             `keydown d` … `keyup d`
  keyboard type <text>       Type text with real keystrokes (no selector)
  keyboard inserttext <text> Insert text without key events
  hover <sel>                Hover element
  focus <sel>                Focus element
  check <sel>                Check checkbox
  uncheck <sel>              Uncheck checkbox
  select <sel> <val...>      Select dropdown option
  drag <src> <dst>           Drag and drop
  upload <sel> <files...>    Upload files
  download <sel> <path>      Download file by clicking element
  scroll <dir> [px]          Scroll (up/down/left/right)
  scrollintoview <sel>       Scroll element into view
  wait <sel|ms>              Wait for element or time
  expect <condition>         Assert (pass/fail + exit code): element visible/gone,
                             count, text/value/attr, url, request fired, no-errors
  extract --schema <json>    Scrape structured JSON (rows+fields) → array/object
                             (one call vs N find/get; generic, any logged-in site)
  form fill --map <json>     Fill a whole form by label/selector, submit, return
                             per-field status + inline validation errors
  friction [--json|--clear]  Local log of failed commands (what's painful to
                             drive) — local only, never uploaded
  screenshot [path]          Take screenshot (auto-downscaled to ≤2000px long edge;
                             --max-width/--max-height/--scale to override)
  pdf <path>                 Save as PDF
  canvas list                List <canvas> elements (size, type) on the page
  canvas capture [sel] [path]  Save a canvas's rendered pixels to PNG — for
                             WebGL/canvas apps (Figma, games, maps, charts) that
                             expose no DOM. toDataURL, with a screenshot fallback.
  snapshot                   Accessibility tree with refs (for AI)
  eval <js>                  Run JavaScript
  connect <port|url>         Connect to browser via CDP
  reconnect                  Re-bind to the running Chrome's relay (alias for
                             `extension connect`) — recover a dropped relay, no reinstall
  browsers                   List connected Chrome profiles; pin a session to one
                             with --browser <id|email> (for multi-profile Chrome)
  keep                       Leave the active tab for the user — exempt it from
                             auto-close/idle cleanup + remove it from the session
                             tab group (so scratch tabs get cleaned, this one stays)
  close [--all]              Close browser (--all closes every session)

Navigation:
  back                       Go back
  forward                    Go forward
  reload                     Reload page

Get Info:  chrome-use get <what> [selector]
  text, html, value, attr <name>, title, url, count, box, styles, cdp-url
  box <sel>  →  x,y,width,height,centerX,centerY,inViewport in CSS px (feed
               centerX/centerY into `click x y`); value reads CodeMirror/Monaco too
  text (no selector = whole page, all frames), text --main, frames (list)

Check State:  chrome-use is <what> <selector>
  visible, enabled, checked

Anti-bot:  chrome-use stealth | cf-status
  stealth         stealth self-check (webdriver/UA/plugins + overrides)
  cf-status       Cloudflare challenge + cf_clearance preflight (skip re-solving)

Find Elements:  chrome-use find <locator> <value> <action> [text]
  role, text, label, placeholder, alt, title, testid, first, last, nth

Mouse:  chrome-use mouse <action> [args]
  move <x> <y>, down [btn], up [btn], wheel <dy> [dx]

Browser Settings:  chrome-use set <setting> [value]
  viewport <w> <h>, device <name>, geo <lat> <lng>
  offline [on|off], headers <json>, credentials <user> <pass>
  media [dark|light] [reduced-motion]

Network:  chrome-use network <action>
  route <url> [--abort]
        [--body <s> --status <n> --header K=V --content-type <ct>]   # mock response
        [--method <M> --set-body <s> --set-header K=V --rewrite-url <u>]  # rewrite request
        [--edit-status <n> --edit-header K=V --replace 'from=>to' --set-json path=value]  # edit the real response
        [--resource-type <csv>]
  unroute [url]
  requests [--clear] [--filter <pattern>]
  har <start|stop> [path]

Storage:
  cookies [get|set|clear]    Manage cookies (set supports --url, --domain, --path, --httpOnly, --secure, --sameSite, --expires)
                             Or:  cookies set --curl <file> [--domain <host>] (auto-detects JSON/cURL/Cookie-header files)
  cookies export --from <profile> [--domain <d>[,<d>]]
                             Decrypt another Chrome profile's cookies -> JSON for `cookies set --curl` (macOS)
  cookies transfer --from <profile> [--domain <d>[,<d>]]
                             Copy a logged-in session from another profile into the connected browser (macOS)
  storage <local|session>    Manage web storage

Tabs:
  tab [new|duplicate|list|close|<ref>]
                             Manage tabs (<ref> = t<N>, a label, or a CDP targetId)
  tab duplicate [ref] [--label <name>]
                             Natively duplicate on extension-connected real Chrome
  tab list --full            Full URLs + stable cross-session targetId per tab
  tab <targetId>             Adopt a specific tab (incl. another session's) by its
                             stable targetId, no reload — preserves in-page state
  open <url> --reuse-tab     Reuse an existing tab on that URL instead of spawning
                             a duplicate (matches origin+path; preserves state)
  adopt <url|targetId>       Read a PRE-EXISTING tab (the user's own, or another
                             session's) WITHOUT opening a new one — matches by URL
                             substring or stable targetId, then drives it. e.g.
                             `adopt "github.com/owner/repo"`

Diff:
  diff snapshot              Compare current vs last snapshot
  diff screenshot --baseline Compare current vs baseline image
  diff url <u1> <u2>         Compare two pages

Debug:
  trace start|stop [path]    Record Chrome DevTools trace
  profiler start|stop [path] Record Chrome DevTools profile
  record start <path> [url]  Start video recording (WebM)
  record stop                Stop and save video
  console [--clear] [--limit N]  View console logs (--limit tails last N)
  errors [--clear]           View page errors
  highlight <sel>            Highlight element
  inspect                    Open Chrome DevTools for the active page
  clipboard <op> [text]      Read/write clipboard (read, write, copy, paste)

Streaming:
  stream enable [--port <n>] Start runtime WebSocket streaming for this session
  stream disable             Stop runtime WebSocket streaming
  stream status              Show streaming status and active port

React (requires `open --enable react-devtools`):
  react tree                 Full React component tree (depth id parent name columns)
  react inspect <id>         Inspect one fiber (props, hooks, state, source)
  react renders start        Start recording re-renders via onCommitFiberRoot
  react renders stop [--json] Stop and print render profile
  react suspense [--only-dynamic] [--json]
                             Walk Suspense boundaries + classifier report
                             --only-dynamic hides the "static" list

Performance:
  vitals [url] [--json]      Core Web Vitals (LCP/CLS/TTFB/FCP/INP) +
                             React hydration timing when profiling build detected

SPA:
  pushstate <url>            SPA client-side nav. Auto-detects window.next.router.push
                             (triggers RSC fetch on Next.js); falls back to
                             history.pushState + popstate/navigate events for other frameworks

Init scripts:
  removeinitscript <id>      Remove a script registered via --init-script or addinitscript

Batch:
  batch [--bail] ["cmd" ...]  Execute multiple commands sequentially (args or stdin)
                              --bail stops on first error (default: continue all)

Site adapters:  turn a website into a structured-data CLI (runs as you, in your tab)
  site update                Fetch the community adapter pack into ~/.chrome-use/sites
  site list                  List installed adapters (name/cmd)
  site info <name>/<cmd>     Show an adapter's @meta (args, domain, capabilities)
  site <name>/<cmd> [args]   Run an adapter: navigate to its site + return JSON
                             e.g. site github/issues epiral/repo, site reddit/search rust
                             Positional args fill declared args in order; --key value overrides

Auth Vault:
  auth save <name> [opts]    Save auth profile (--url, --username, --password/--password-stdin)
  auth login <name>          Login using saved credentials (waits for form fields)
  auth list                  List saved auth profiles
  auth show <name>           Show auth profile metadata
  auth delete <name>         Delete auth profile

Confirmation:
  confirm <id>               Approve a pending action
  deny <id>                  Deny a pending action

Sessions:
  session                    Show current session name
  session list               List active sessions
  session stop [name]        Stop one session daemon (default: current) — graceful,
                             closes the tabs it created
  session prune              Stop ALL session daemons now (closes their tabs; they
                             respawn clean on next use). For clearing idle daemons.
  sessions                   List running session daemons (alias of daemon status)
  daemon status              List running session daemons (+ relay state)
  daemon restart             Kill all session daemons; keeps the extension relay
                             up. Clears stale/cross-leaked state after an upgrade.

  Lifecycle: each --session <name> spawns a background daemon that drives that
  session's tabs. A daemon auto-shuts-down after 10 min idle (no commands) —
  AGENT_BROWSER_IDLE_TIMEOUT_MS overrides, 0 disables — and on shutdown closes
  the scratch tabs IT created (its tab group). Use `keep` to leave a tab for the
  user (exempt from auto-close), `session stop/prune` to reclaim now.

Chat (AI):
  chat <message>             Send a natural language instruction (single-shot)
  chat                       Start interactive chat (REPL mode when stdin is a TTY)
  Options: --model <name>, -v/--verbose, -q/--quiet

Dashboard:
  dashboard [start]          Start the dashboard server (default port: 4848)
  dashboard start --port <n> Start on a specific port
  dashboard stop             Stop the dashboard server

MCP:
  mcp                         Run a stdio MCP server (JSON-RPC) exposing a
                              core ~12-tool profile for MCP-only hosts
                              (Claude Desktop, etc.)
  mcp --tools all             Extended profile: core + hover/select/screenshot/
                              scroll/tabs/extract/expect/find/network_route/
                              site/upload/download

Setup:
  install                    Install browser binaries
  install --with-deps        Also install system dependencies (Linux)
  upgrade                    Upgrade to the latest version
  doctor [--fix]             Diagnose install; auto-clean stale files
  dashboard start            Start the observability dashboard
  profiles                   List available Chrome profiles

Snapshot Options:
  -i, --interactive          Only interactive elements
  -c, --compact              Remove empty structural elements
  -d, --depth <n>            Limit tree depth
  -s, --selector <sel>       Scope to CSS selector

Authentication:
  --profile <name|path>      Chrome profile name (e.g., Default) to reuse login state,
                             or a directory path for a persistent custom profile
                             (or AGENT_BROWSER_PROFILE env)
  --session-name <name>      Auto-save/restore cookies and localStorage by name
                             (or AGENT_BROWSER_SESSION_NAME env)
  --state <path>             Load saved auth state (cookies + storage) from JSON file
                             (or AGENT_BROWSER_STATE env)
  --auto-connect             Connect to a running Chrome (DEFAULT - shares cookies/sessions)
                             Tip: enable CDP via chrome://inspect/#remote-debugging
  --launch, --new            Launch a fresh browser instead of connecting to existing
  --headers <json>           HTTP headers scoped to URL's origin (e.g., Authorization bearer token)

Options:
  --session <name>           Isolated session (or AGENT_BROWSER_SESSION env)
  --executable-path <path>   Custom browser executable (or AGENT_BROWSER_EXECUTABLE_PATH)
  --extension <path>         Load browser extensions (repeatable)
  --init-script <path>       Register a page init script before the first navigation (repeatable)
                             (or AGENT_BROWSER_INIT_SCRIPTS env, comma-separated)
  --enable <feature>         Built-in init scripts: react-devtools (repeatable or comma-separated)
                             (or AGENT_BROWSER_ENABLE env)
  --args <args>              Browser launch args, comma or newline separated (or AGENT_BROWSER_ARGS)
                             e.g., --args "--no-sandbox,--disable-blink-features=AutomationControlled"
  --user-agent <ua>          Custom User-Agent (or AGENT_BROWSER_USER_AGENT)
  --proxy <server>           Proxy server URL (or AGENT_BROWSER_PROXY, HTTP_PROXY, HTTPS_PROXY, ALL_PROXY)
                             Supports authenticated proxies: --proxy "http://user:pass@127.0.0.1:7890"
  --proxy-bypass <hosts>     Bypass proxy for these hosts (or AGENT_BROWSER_PROXY_BYPASS, NO_PROXY)
                             e.g., --proxy-bypass "localhost,*.internal.com"
  --ignore-https-errors      Ignore HTTPS certificate errors
  --allow-file-access        Allow file:// URLs to access local files (Chromium only)
  --hide-scrollbars <bool>   Hide native scrollbars in headless Chromium screenshots (default: true)
                             Use --hide-scrollbars false to keep scrollbars visible
  -p, --provider <name>      Browser provider: ios, browserbase, kernel, browseruse, browserless, agentcore
  --device <name>            iOS device name (e.g., "iPhone 15 Pro")
  --json                     JSON output
  --annotate                 Annotated screenshot with numbered labels and legend
  --screenshot-dir <path>    Default screenshot output directory (or AGENT_BROWSER_SCREENSHOT_DIR)
  --screenshot-quality <n>   JPEG quality 0-100; ignored for PNG (or AGENT_BROWSER_SCREENSHOT_QUALITY)
  --screenshot-format <fmt>  Screenshot format: png, jpeg (or AGENT_BROWSER_SCREENSHOT_FORMAT)
  --headed                   Always on (default). Headless is forbidden (bot-detection tell);
                             display-less servers can opt back in with AGENT_BROWSER_ALLOW_HEADLESS=1
  --cdp <port>               Connect via CDP (Chrome DevTools Protocol)
  --browser <id|email>       Pin this session to a specific connected Chrome profile
                             (run `chrome-use browsers` to list; for multi-profile
                             relay setups). Sticky per session.
  --color-scheme <scheme>    Color scheme: dark, light, no-preference (or AGENT_BROWSER_COLOR_SCHEME)
  --download-path <path>     Default download directory (or AGENT_BROWSER_DOWNLOAD_PATH)
  --content-boundaries       Wrap page output in boundary markers (or AGENT_BROWSER_CONTENT_BOUNDARIES)
  --max-output <chars>       Truncate page output to N chars (or AGENT_BROWSER_MAX_OUTPUT)
  --allowed-domains <list>   Restrict navigation domains (or AGENT_BROWSER_ALLOWED_DOMAINS)
  --action-policy <path>     Action policy JSON file (or AGENT_BROWSER_ACTION_POLICY)
  --confirm-actions <list>   Categories requiring confirmation (or AGENT_BROWSER_CONFIRM_ACTIONS)
  --confirm-interactive      Interactive confirmation prompts; auto-denies if stdin is not a TTY (or AGENT_BROWSER_CONFIRM_INTERACTIVE)
  --engine <name>            Browser engine: chrome (default), lightpanda (or AGENT_BROWSER_ENGINE)
  --no-auto-dialog           Disable automatic dismissal of alert/beforeunload dialogs (or AGENT_BROWSER_NO_AUTO_DIALOG)
  --if-present, --optional   Skip a selector action (success, no-op) instead of
                             erroring when the target element is absent
  --observe                  After a mutating action, return only what changed
                             (a11y delta + url + requests) — skip act→snapshot→diff
  --model <name>             AI model for chat (or AI_GATEWAY_MODEL env)
  -v, --verbose              Show tool commands and their raw output
  -q, --quiet                Show only AI text responses (hide tool calls)
  --config <path>            Use a custom config file (or AGENT_BROWSER_CONFIG env)
  --debug                    Debug output
  --version, -V              Show version

Configuration:
  chrome-use looks for chrome-use.json in these locations (lowest to highest priority):
    1. ~/.chrome-use/config.json      User-level defaults
    2. ./chrome-use.json              Project-level overrides
    3. Environment variables             Override config file values
    4. CLI flags                         Override everything

  Use --config <path> to load a specific config file instead of the defaults.
  If --config points to a missing or invalid file, chrome-use exits with an error.

  Boolean flags accept an optional true/false value to override config:
    --headed           (same as --headed true)
    --headed false     (disables "headed": true from config)
    --hide-scrollbars false (keeps native scrollbars visible in headless Chromium screenshots)

  Extensions from user and project configs are merged (not replaced).

  Example chrome-use.json:
    {{"headed": true, "hideScrollbars": false, "proxy": "http://localhost:8080"}}

Environment:
  AGENT_BROWSER_CONFIG           Path to config file (or use --config)
  AGENT_BROWSER_SESSION          Session name (default: "default")
  AGENT_BROWSER_SESSION_NAME     Auto-save/restore state persistence name
  AGENT_BROWSER_ENCRYPTION_KEY   64-char hex key for AES-256-GCM state encryption
  AGENT_BROWSER_STATE_EXPIRE_DAYS Auto-delete states older than N days (default: 30)
  AGENT_BROWSER_EXECUTABLE_PATH  Custom browser executable path
  AGENT_BROWSER_EXTENSIONS       Comma-separated browser extension paths
  AGENT_BROWSER_INIT_SCRIPTS     Comma-separated paths to page init scripts
  AGENT_BROWSER_ENABLE           Comma-separated built-in init script features (e.g. react-devtools)
  AGENT_BROWSER_HEADED           Show browser window (not headless)
  AGENT_BROWSER_JSON             JSON output
  AGENT_BROWSER_ANNOTATE         Annotated screenshot with numbered labels and legend
  AGENT_BROWSER_DEBUG            Debug output
  AGENT_BROWSER_IGNORE_HTTPS_ERRORS Ignore HTTPS certificate errors
  AGENT_BROWSER_PROVIDER         Browser provider (ios, browserbase, kernel, browseruse, browserless, agentcore)
  AGENT_BROWSER_AUTO_CONNECT     Auto-discover and connect to running Chrome
  AGENT_BROWSER_ALLOW_FILE_ACCESS Allow file:// URLs to access local files
  AGENT_BROWSER_HIDE_SCROLLBARS  Hide scrollbars in headless Chromium screenshots (default: true)
  AGENT_BROWSER_COLOR_SCHEME     Color scheme preference (dark, light, no-preference)
  AGENT_BROWSER_DOWNLOAD_PATH    Default download directory for browser downloads
  AGENT_BROWSER_DEFAULT_TIMEOUT  Default action timeout in ms (default: 25000)
  AGENT_BROWSER_SESSION_NAME     Auto-save/load state persistence name
  AGENT_BROWSER_STATE_EXPIRE_DAYS Auto-delete saved states older than N days (default: 30)
  AGENT_BROWSER_ENCRYPTION_KEY   64-char hex key for AES-256-GCM session encryption
  AGENT_BROWSER_STREAM_PORT      Override WebSocket streaming port (default: OS-assigned)
  AGENT_BROWSER_IDLE_TIMEOUT_MS  Auto-shutdown daemon after N ms of inactivity (disabled by default)
  AGENT_BROWSER_IOS_DEVICE       Default iOS device name
  AGENT_BROWSER_IOS_UDID         Default iOS device UDID
  AGENT_BROWSER_CONTENT_BOUNDARIES Wrap page output in boundary markers
  AGENT_BROWSER_MAX_OUTPUT       Max characters for page output
  AGENT_BROWSER_ALLOWED_DOMAINS  Comma-separated allowed domain patterns
  AGENT_BROWSER_ACTION_POLICY    Path to action policy JSON file
  AGENT_BROWSER_CONFIRM_ACTIONS  Action categories requiring confirmation
  AGENT_BROWSER_CONFIRM_INTERACTIVE Enable interactive confirmation prompts
  AGENT_BROWSER_NO_AUTO_DIALOG   Disable automatic dismissal of alert/beforeunload dialogs
  AGENT_BROWSER_ENGINE           Browser engine: chrome (default), lightpanda
  HTTP_PROXY / HTTPS_PROXY       Standard proxy env vars (fallback if AGENT_BROWSER_PROXY not set)
  ALL_PROXY                      SOCKS proxy (fallback for proxy)
  NO_PROXY                       Bypass proxy for hosts (fallback for proxy-bypass)
  AGENT_BROWSER_SCREENSHOT_DIR   Default screenshot output directory
  AGENT_BROWSER_SCREENSHOT_QUALITY JPEG quality 0-100
  AGENT_BROWSER_SCREENSHOT_FORMAT Screenshot format: png, jpeg
  AI_GATEWAY_URL                 Vercel AI Gateway base URL (default: https://ai-gateway.vercel.sh)
  AI_GATEWAY_API_KEY             API key for the AI Gateway (enables chat command and dashboard AI chat)
  AI_GATEWAY_MODEL               Default AI model (default: anthropic/claude-sonnet-4.6, or --model flag)

Install:
  npm install -g chrome-use           # npm
  brew install chrome-use             # Homebrew
  cargo install chrome-use            # Cargo
  chrome-use install                  # Download Chrome (first time)

Examples:
  chrome-use open example.com
  chrome-use snapshot -i              # Interactive elements only
  chrome-use click @e2                # Click by ref from snapshot
  chrome-use fill @e3 "test@example.com"
  chrome-use find role button click --name Submit
  chrome-use get text @e1
  chrome-use screenshot --full
  chrome-use screenshot --annotate    # Labeled screenshot for vision models
  chrome-use wait 2000               # Wait for slow pages to settle
  chrome-use --cdp 9222 snapshot      # Connect via CDP port
  chrome-use --auto-connect snapshot  # Auto-discover running Chrome
  chrome-use stream enable            # Start runtime streaming on an auto-selected port
  chrome-use stream status            # Inspect runtime streaming state
  chrome-use --color-scheme dark open example.com  # Dark mode
  chrome-use --profile Default open gmail.com        # Reuse Chrome login state
  chrome-use --profile ~/.myapp open example.com    # Persistent custom profile
  chrome-use profiles                               # List available Chrome profiles
  chrome-use --session-name myapp open example.com  # Auto-save/restore state
  chrome-use chat "open google.com and search for cats"  # AI chat (single-shot)
  chrome-use chat                                        # AI chat (interactive REPL)
  chrome-use -q chat "summarize this page"               # Quiet mode (text only)

Command Chaining:
  Chain commands with && in a single shell call (browser persists via daemon):

  chrome-use open example.com && chrome-use snapshot -i
  chrome-use fill @e1 "user@example.com" && chrome-use fill @e2 "pass" && chrome-use click @e3
  chrome-use open example.com && chrome-use screenshot

iOS Simulator (requires Xcode and Appium):
  chrome-use -p ios open example.com                    # Use default iPhone
  chrome-use -p ios --device "iPhone 15 Pro" open url   # Specific device
  chrome-use -p ios device list                         # List simulators
  chrome-use -p ios swipe up                            # Swipe gesture
  chrome-use -p ios tap @e1                             # Touch element

Hit a bug or rough edge? A 30-second issue genuinely sharpens this tool:
  https://github.com/leeguooooo/chrome-use/issues
"#
    );
}

fn print_snapshot_diff(data: &serde_json::Map<String, serde_json::Value>) {
    let changed = data
        .get("changed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !changed {
        println!("{} No changes detected", color::success_indicator());
        return;
    }
    if let Some(diff) = data.get("diff").and_then(|v| v.as_str()) {
        for line in diff.lines() {
            if line.starts_with("+ ") {
                println!("{}", color::green(line));
            } else if line.starts_with("- ") {
                println!("{}", color::red(line));
            } else {
                println!("{}", color::dim(line));
            }
        }
        let additions = data.get("additions").and_then(|v| v.as_i64()).unwrap_or(0);
        let removals = data.get("removals").and_then(|v| v.as_i64()).unwrap_or(0);
        let unchanged = data.get("unchanged").and_then(|v| v.as_i64()).unwrap_or(0);
        println!(
            "\n{} additions, {} removals, {} unchanged",
            color::green(&additions.to_string()),
            color::red(&removals.to_string()),
            unchanged
        );
    }
}

fn print_screenshot_diff(data: &serde_json::Map<String, serde_json::Value>) {
    let mismatch = data
        .get("mismatchPercentage")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let is_match = data.get("match").and_then(|v| v.as_bool()).unwrap_or(false);
    let dim_mismatch = data
        .get("dimensionMismatch")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if dim_mismatch {
        println!(
            "{} Images have different dimensions",
            color::error_indicator()
        );
    } else if is_match {
        println!(
            "{} Images match (0% difference)",
            color::success_indicator()
        );
    } else {
        println!(
            "{} {:.2}% pixels differ",
            color::error_indicator(),
            mismatch
        );
    }
    if let Some(diff_path) = data.get("diffPath").and_then(|v| v.as_str()) {
        println!("  Diff image: {}", color::green(diff_path));
    }
    let total = data
        .get("totalPixels")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let different = data
        .get("differentPixels")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    println!(
        "  {} different / {} total pixels",
        color::red(&different.to_string()),
        total
    );
}

pub fn print_version() {
    println!("chrome-use {}", env!("CARGO_PKG_VERSION"));
    println!("report bugs / rough edges: https://github.com/leeguooooo/chrome-use/issues");
}

#[cfg(test)]
mod tests {
    use super::format_storage_text;
    use serde_json::json;

    #[test]
    fn test_format_stream_status_text_for_enabled_stream() {
        let data = json!({
            "enabled": true,
            "port": 9223,
            "connected": true,
            "screencasting": false
        });

        let rendered = super::format_stream_status_text(Some("stream_status"), &data).unwrap();

        assert_eq!(
            rendered,
            "Streaming enabled on ws://127.0.0.1:9223\nConnected: true\nScreencasting: false"
        );
    }

    #[test]
    fn test_format_stream_status_text_for_disabled_stream() {
        let data =
            json!({ "enabled": false, "port": null, "connected": false, "screencasting": false });

        let rendered = super::format_stream_status_text(Some("stream_status"), &data).unwrap();

        assert_eq!(rendered, "Streaming disabled");
    }

    #[test]
    fn test_format_storage_text_for_all_entries() {
        let data = json!({
            "data": {
                "token": "abc123",
                "user": "alice"
            }
        });

        let rendered = format_storage_text(&data).unwrap();

        assert_eq!(rendered, "token: abc123\nuser: alice");
    }

    #[test]
    fn test_format_storage_text_for_key_lookup() {
        let data = json!({
            "key": "token",
            "value": "abc123"
        });

        let rendered = format_storage_text(&data).unwrap();

        assert_eq!(rendered, "token: abc123");
    }

    #[test]
    fn test_format_storage_text_for_empty_store() {
        let data = json!({
            "data": {}
        });

        let rendered = format_storage_text(&data).unwrap();

        assert_eq!(rendered, "No storage entries");
    }
}
