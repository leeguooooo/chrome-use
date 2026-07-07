//! Single-pass scripting — the JS engine path (Phase 2).
//!
//! `chrome-use script <<'JS' … JS` runs a real JavaScript program with `cu.*`
//! helpers (snapshot/click/fill/find/wait/eval/extract/…) that drive the user's
//! REAL logged-in Chrome over the stealth relay — ego lite's "code base" idiom,
//! but on your existing browser, cross-platform, from any shell.
//!
//! Architecture (deliberately avoids boa's async machinery):
//!   - boa runs SYNC on a `spawn_blocking` worker thread.
//!   - the single host fn `__cu(action, paramsJson) -> respJson` is pure string
//!     I/O (no JsValue<->serde conversion, no boa `json` feature). It blocks on an
//!     mpsc round-trip to the daemon actor loop, which owns `&mut DaemonState` and
//!     runs `execute_command` — the SAME per-op path as every other command
//!     (policy / stealth / humanize / @ref-heal / stale-recovery).
//!   - the ergonomic `cu.*` surface is defined in a JS prelude, so almost nothing
//!     depends on boa's exact API.
//!
//! Because `cu.*` calls block until the browser op returns, user scripts are plain
//! SYNCHRONOUS JS — no `await`. The engine lives in the daemon (outside the page),
//! so it survives hard navigations, which is exactly where a page-world runtime
//! (and ego's edge over a naive shim) would break.

use super::actions::{execute_command, DaemonState};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::sync::mpsc as stdmpsc;

/// One bridged `cu.*` call: a daemon command + a channel to return its response.
struct CuRequest {
    cmd: Value,
    reply: stdmpsc::Sender<Value>,
}

thread_local! {
    /// Set for the lifetime of one boa run on its worker thread; the native fn
    /// reads it to reach the daemon. Cleared at the end of the run so a pooled
    /// blocking thread never reuses a stale sender.
    static CU_TX: RefCell<Option<tokio::sync::mpsc::Sender<CuRequest>>> = const { RefCell::new(None) };
}

/// Run a JS `source` program to completion, servicing its `cu.*` calls against
/// `state`. Returns `{ return, logs }` (the IIFE's return value + collected
/// `cu.log(...)` lines).
pub async fn run_js(
    source: &str,
    _timeout_ms: Option<u64>,
    state: &mut DaemonState,
) -> Result<Value, String> {
    let (req_tx, mut req_rx) = tokio::sync::mpsc::channel::<CuRequest>(1);
    let src = source.to_string();
    let engine = tokio::task::spawn_blocking(move || run_boa_thread(&src, req_tx));

    // Actor loop: service one bridged op at a time until the engine finishes and
    // drops its sender (channel closes). `__log` is handled locally (no browser).
    let mut logs: Vec<Value> = Vec::new();
    while let Some(msg) = req_rx.recv().await {
        let action = msg.cmd.get("action").and_then(|v| v.as_str()).unwrap_or("");
        if action == "__log" {
            let m = msg.cmd.get("msg").cloned().unwrap_or(Value::Null);
            logs.push(m);
            let _ = msg.reply.send(json!({ "success": true, "data": null }));
            continue;
        }
        let result = Box::pin(execute_command(&msg.cmd, state)).await;
        let _ = msg.reply.send(result);
    }

    match engine.await {
        Ok(Ok(ret)) => Ok(json!({ "return": ret, "logs": logs })),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(format!("script engine thread failed: {}", e)),
    }
}

/// Runs on the blocking worker thread: sets up boa, registers `__cu`, evals the
/// prelude + user source, and returns the program's completion value (as JSON).
fn run_boa_thread(
    source: &str,
    req_tx: tokio::sync::mpsc::Sender<CuRequest>,
) -> Result<Value, String> {
    use boa_engine::{js_string, Context, NativeFunction, Source};

    CU_TX.with(|c| *c.borrow_mut() = Some(req_tx));
    // Ensure the sender is dropped when this run ends, even on a pooled thread,
    // so the actor loop's channel closes and `run_js` can return.
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            CU_TX.with(|c| *c.borrow_mut() = None);
        }
    }
    let _guard = Guard;

    let mut ctx = Context::default();
    ctx.register_global_callable(
        js_string!("__cu"),
        2,
        NativeFunction::from_fn_ptr(cu_native),
    )
    .map_err(|e| format!("failed to register cu bridge: {}", e))?;

    ctx.eval(Source::from_bytes(CU_PRELUDE.as_bytes()))
        .map_err(|e| format!("cu prelude error: {}", e))?;

    // Wrap the user program in an arrow IIFE so top-level `return` works and the
    // returned value is the completion value; JSON.stringify it at the JS boundary
    // so we cross back to Rust as a plain string (no boa json feature needed).
    let wrapped = format!("JSON.stringify((() => {{\n{}\n}})() ?? null)", source);
    let completion = ctx
        .eval(Source::from_bytes(wrapped.as_bytes()))
        .map_err(|e| format!("script error: {}", js_err(&e)))?;

    let json_str = completion
        .as_string()
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_else(|| "null".to_string());
    Ok(serde_json::from_str(&json_str).unwrap_or(Value::Null))
}

fn js_err(e: &boa_engine::JsError) -> String {
    e.to_string()
}

/// `__cu(action: string, paramsJson: string) -> respJson: string`
/// The only Rust<->JS boundary: strings in, string out. Blocks on the daemon
/// round-trip so user scripts can be plain synchronous JS.
fn cu_native(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    _ctx: &mut boa_engine::Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    use boa_engine::{js_string, JsError, JsValue};

    let arg_str = |i: usize| -> String {
        args.get(i)
            .and_then(|v| v.as_string())
            .map(|s| s.to_std_string_escaped())
            .unwrap_or_default()
    };
    let action = arg_str(0);
    let params_str = {
        let s = arg_str(1);
        if s.is_empty() { "{}".to_string() } else { s }
    };
    let mut cmd: Value = serde_json::from_str(&params_str).unwrap_or_else(|_| json!({}));
    if let Some(obj) = cmd.as_object_mut() {
        obj.insert("action".to_string(), json!(alias_action(&action)));
        obj.insert("id".to_string(), json!("js"));
    }

    let (reply_tx, reply_rx) = stdmpsc::channel::<Value>();
    let send_result = CU_TX.with(|c| {
        c.borrow()
            .as_ref()
            .map(|tx| tx.blocking_send(CuRequest { cmd, reply: reply_tx }))
    });
    match send_result {
        Some(Ok(())) => {}
        _ => {
            return Err(JsError::from_opaque(
                js_string!("cu bridge is closed").into(),
            ))
        }
    }
    let resp = reply_rx
        .recv()
        .map_err(|_| JsError::from_opaque(js_string!("cu reply channel dropped").into()))?;

    let out = serde_json::to_string(&resp).unwrap_or_else(|_| "null".to_string());
    Ok(JsValue::from(js_string!(out)))
}

/// Verb aliases so `cu.open`/`cu.eval` map to the daemon action names.
fn alias_action(verb: &str) -> String {
    match verb {
        "open" | "goto" => "navigate",
        "eval" | "js" => "evaluate",
        other => other,
    }
    .to_string()
}

/// The `cu.*` helper surface, defined in JS over the single `__cu` bridge. Each
/// helper throws on a failed op so scripts fail fast; `cu.eval` returns the raw
/// page value. Kept in JS so it needs almost none of boa's Rust API.
const CU_PRELUDE: &str = r#"
globalThis.cu = {
  _call(action, params) {
    const r = JSON.parse(__cu(action, JSON.stringify(params || {})));
    if (!r || r.success !== true) {
      throw new Error((r && r.error) || ('cu.' + action + ' failed'));
    }
    return r.data;
  },
  snapshot(opts) { return this._call('snapshot', opts || { interactive: true }); },
  eval(js) { return this._call('evaluate', { script: js }).result; },
  js(js) { return this.eval(js); },
  open(url) { return this._call('navigate', { url: url }); },
  navigate(url) { return this.open(url); },
  goto(url) { return this.open(url); },
  click(selector) { return this._call('click', { selector: selector }); },
  dblclick(selector) { return this._call('dblclick', { selector: selector }); },
  fill(selector, text) { return this._call('fill', { selector: selector, text: text }); },
  type(selector, text) { return this._call('type', { selector: selector, text: text }); },
  press(key) { return this._call('press', { key: key }); },
  hover(selector) { return this._call('hover', { selector: selector }); },
  select(selector, value) { return this._call('select', { selector: selector, value: value }); },
  check(selector) { return this._call('check', { selector: selector }); },
  uncheck(selector) { return this._call('uncheck', { selector: selector }); },
  scroll(dir, px) { return this._call('scroll', { direction: dir, amount: px }); },
  back() { return this._call('back', {}); },
  forward() { return this._call('forward', {}); },
  reload() { return this._call('reload', {}); },
  wait(ms) { return this._call('wait', { timeout: ms }); },
  visible(selector) {
    const r = JSON.parse(__cu('isvisible', JSON.stringify({ selector: selector })));
    return !!(r && r.success && r.data && (r.data.visible === true || r.data === true));
  },
  waitFor(selector, timeoutMs) {
    const deadline = (timeoutMs || 10000);
    let waited = 0;
    while (waited < deadline) {
      if (this.visible(selector)) return true;
      this.wait(250);
      waited += 250;
    }
    throw new Error('waitFor timed out: ' + selector);
  },
  extract(schema) { return this._call('extract', { schema: schema }); },
  screenshot(path) { return this._call('screenshot', path ? { path: path } : {}); },
  text(selector) { return this._call('gettext', { selector: selector }); },
  log(msg) { __cu('__log', JSON.stringify({ msg: (typeof msg === 'string' ? msg : JSON.stringify(msg)) })); },
};
"#;
