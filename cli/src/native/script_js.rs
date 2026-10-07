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
    let mut advisories: Vec<Value> = Vec::new();
    while let Some(msg) = req_rx.recv().await {
        let action = msg.cmd.get("action").and_then(|v| v.as_str()).unwrap_or("");
        if action == "__log" {
            let m = msg.cmd.get("msg").cloned().unwrap_or(Value::Null);
            logs.push(m);
            let _ = msg.reply.send(json!({ "success": true, "data": null }));
            continue;
        }
        let result = Box::pin(execute_command(&msg.cmd, state)).await;
        collect_advisory(&result, &mut advisories);
        let _ = msg.reply.send(result);
    }

    match engine.await {
        Ok(Ok(ret)) => Ok(json!({ "return": ret, "logs": logs, "advisories": advisories })),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(format!("script engine thread failed: {}", e)),
    }
}

/// Keep bounded advisory evidence even when a script ignores a cu.* return.
fn collect_advisory(response: &Value, advisories: &mut Vec<Value>) {
    if advisories.len() < 20 {
        if let Some(advisory) = response.pointer("/data/observed/noProgress") {
            advisories.push(advisory.clone());
        }
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
        if s.is_empty() {
            "{}".to_string()
        } else {
            s
        }
    };
    let mut cmd: Value = serde_json::from_str(&params_str).unwrap_or_else(|_| json!({}));
    if let Some(obj) = cmd.as_object_mut() {
        obj.insert("action".to_string(), json!(alias_action(&action)));
        obj.insert("id".to_string(), json!("js"));
    }

    let (reply_tx, reply_rx) = stdmpsc::channel::<Value>();
    let send_result = CU_TX.with(|c| {
        c.borrow().as_ref().map(|tx| {
            tx.blocking_send(CuRequest {
                cmd,
                reply: reply_tx,
            })
        })
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
  // `fill` takes `value`, not `text` — sending `text` made every `cu.fill`
  // fail with "Missing 'value' parameter". Found by driving a real login form.
  fill(selector, value) { return this._call('fill', { selector: selector, value: value }); },
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
  // Documented in the script guide but never defined here, so `cu.find(...)`
  // threw "not a function". Mirrors the CLI: `find "<label>"` is the
  // natural-language search (action `findfuzzy`), while a CSS selector lists
  // matching elements (action `find`). Pass `{ selector }` for the latter.
  find(spec) {
    if (spec && typeof spec === 'object' && spec.selector) {
      return this._call('find', { selector: spec.selector });
    }
    return this._call('findfuzzy', { query: spec });
  },
  extract(schema) { return this._call('extract', { schema: schema }); },
  screenshot(path) { return this._call('screenshot', path ? { path: path } : {}); },
  text(selector) { return this._call('gettext', { selector: selector }); },
  log(msg) { __cu('__log', JSON.stringify({ msg: (typeof msg === 'string' ? msg : JSON.stringify(msg)) })); },
};
"#;

// ---------------------------------------------------------------------------
// Persistent named contexts (#289)
// ---------------------------------------------------------------------------

/// One job handed to a resident context thread: the program to run, the channel
/// its `cu.*` calls should use for this run, and where to put the result.
struct Job {
    source: String,
    cu_tx: tokio::sync::mpsc::Sender<CuRequest>,
    done: stdmpsc::Sender<Result<Value, String>>,
}

/// A live named context: a dedicated thread owning a boa `Context` (which is
/// not `Send`, so it can never leave that thread) plus the channel that feeds it
/// jobs. Dropping the handle closes the channel, which ends the thread.
struct CtxHandle {
    jobs: stdmpsc::Sender<Job>,
    /// True while a job is running on this context's thread.
    ///
    /// A context runs ONE program at a time, so a `cu.*` call that re-enters
    /// `script --in <the same name>` would queue a job behind the very program
    /// that is waiting for it: the thread cannot pick the new job up, nothing
    /// ever closes the new run's bridge channel, and the daemon waits forever —
    /// which surfaces as the session going unresponsive (#309's shape).
    /// Refusing the re-entrant call outright is the whole guard.
    busy: bool,
}

/// The session's named script contexts. Lives in `DaemonState`, so every context
/// is released when the session's daemon goes away — no extra teardown path.
#[derive(Default)]
pub struct JsContexts {
    map: std::collections::HashMap<String, CtxHandle>,
}

impl JsContexts {
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.map.keys().cloned().collect();
        names.sort();
        names
    }

    /// Release one context. `true` if it existed. The thread ends when the
    /// dropped handle closes its job channel.
    pub fn drop_context(&mut self, name: &str) -> bool {
        self.map.remove(name).is_some()
    }
}

/// Run `source` in the named persistent context, creating it if `create` is set.
///
/// Unlike [`run_js`], the program is evaluated at TOP LEVEL rather than inside an
/// arrow IIFE: that is the whole point of a persistent context — `const tab = …`
/// has to still be there on the next call. Two consequences the caller must know,
/// and which `script.rs` documents: top-level `return` is a syntax error here (end
/// with an expression instead), and re-declaring the same `const` in a later call
/// throws, exactly as it does in a Node REPL.
pub async fn run_js_in(
    name: &str,
    source: &str,
    create: bool,
    state: &mut DaemonState,
) -> Result<Value, String> {
    if !state.script_contexts.map.contains_key(name) {
        if !create {
            let known = state.script_contexts.names();
            return Err(if known.is_empty() {
                format!(
                    "no script context named `{name}` (this session has none; \
                     create one with `script --keep {name}`)"
                )
            } else {
                format!(
                    "no script context named `{name}` (this session has: {}); \
                     create one with `script --keep {name}`",
                    known.join(", ")
                )
            });
        }
        state
            .script_contexts
            .map
            .insert(name.to_string(), spawn_context_thread()?);
    }

    if state.script_contexts.map[name].busy {
        return Err(format!(
            "script context `{name}` is already running a program; a context runs one at a time, so a script cannot re-enter its own context (use a different context name)"
        ));
    }

    let (req_tx, mut req_rx) = tokio::sync::mpsc::channel::<CuRequest>(1);
    let (done_tx, done_rx) = stdmpsc::channel::<Result<Value, String>>();
    let job = Job {
        source: source.to_string(),
        cu_tx: req_tx,
        done: done_tx,
    };

    // A context whose thread died (a panic in boa) must not wedge the name
    // forever: drop it and say so, so the next call can create a fresh one.
    if state.script_contexts.map[name].jobs.send(job).is_err() {
        state.script_contexts.map.remove(name);
        return Err(format!(
            "script context `{name}` is gone (its engine thread ended); it has been \
             released — rerun to start a fresh one"
        ));
    }
    if let Some(handle) = state.script_contexts.map.get_mut(name) {
        handle.busy = true;
    }

    // Same actor loop as a one-shot run: the thread drops its `cu_tx` when the
    // job finishes, which closes this channel and ends the loop.
    let mut logs: Vec<Value> = Vec::new();
    let mut advisories: Vec<Value> = Vec::new();
    while let Some(msg) = req_rx.recv().await {
        let action = msg.cmd.get("action").and_then(|v| v.as_str()).unwrap_or("");
        if action == "__log" {
            let m = msg.cmd.get("msg").cloned().unwrap_or(Value::Null);
            logs.push(m);
            let _ = msg.reply.send(json!({ "success": true, "data": null }));
            continue;
        }
        let result = Box::pin(execute_command(&msg.cmd, state)).await;
        collect_advisory(&result, &mut advisories);
        let _ = msg.reply.send(result);
    }

    let outcome = done_rx.recv();
    if let Some(handle) = state.script_contexts.map.get_mut(name) {
        handle.busy = false;
    }
    match outcome {
        Ok(Ok(ret)) => {
            Ok(json!({ "return": ret, "logs": logs, "advisories": advisories, "context": name }))
        }
        Ok(Err(e)) => Err(e),
        Err(_) => {
            state.script_contexts.map.remove(name);
            Err(format!(
                "script context `{name}` ended without a result; it has been released"
            ))
        }
    }
}

/// Start a resident boa context on its own thread and return the handle that
/// feeds it jobs. The engine is built ONCE here — that persistence is the point.
fn spawn_context_thread() -> Result<CtxHandle, String> {
    let (jobs_tx, jobs_rx) = stdmpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("chrome-use-script-ctx".to_string())
        .spawn(move || context_thread_main(jobs_rx))
        .map_err(|e| format!("failed to start script context thread: {e}"))?;
    Ok(CtxHandle {
        jobs: jobs_tx,
        busy: false,
    })
}

fn context_thread_main(jobs: stdmpsc::Receiver<Job>) {
    use boa_engine::{js_string, Context, NativeFunction, Source};

    let mut ctx = Context::default();
    let setup = (|| -> Result<(), String> {
        ctx.register_global_callable(
            js_string!("__cu"),
            2,
            NativeFunction::from_fn_ptr(cu_native),
        )
        .map_err(|e| format!("failed to register cu bridge: {e}"))?;
        ctx.eval(Source::from_bytes(CU_PRELUDE.as_bytes()))
            .map_err(|e| format!("cu prelude error: {e}"))?;
        Ok(())
    })();

    while let Ok(job) = jobs.recv() {
        let Job {
            source,
            cu_tx,
            done,
        } = job;
        if let Err(e) = &setup {
            // Report the same failure to every caller rather than running a
            // context with no `cu.*` in it.
            let _ = done.send(Err(e.clone()));
            continue;
        }
        CU_TX.with(|c| *c.borrow_mut() = Some(cu_tx));
        let result = eval_top_level(&mut ctx, &source);
        // Drop this run's sender BEFORE reporting, so the caller's actor loop
        // sees the channel close and stops awaiting bridged ops.
        CU_TX.with(|c| *c.borrow_mut() = None);
        let _ = done.send(result);
    }
}

/// Evaluate `source` as a top-level program in `ctx` and return its completion
/// value as JSON. The value is handed back through a global rather than
/// converted in Rust, so this needs none of boa's json feature — the same
/// string-only boundary the one-shot path uses.
fn eval_top_level(ctx: &mut boa_engine::Context, source: &str) -> Result<Value, String> {
    use boa_engine::{js_string, Source};

    let completion = ctx
        .eval(Source::from_bytes(source.as_bytes()))
        .map_err(|e| top_level_error(&js_err(&e)))?;

    let global = ctx.global_object();
    global
        .set(js_string!("__cu_last"), completion, false, ctx)
        .map_err(|e| format!("script error: {}", js_err(&e)))?;

    let json_str = ctx
        .eval(Source::from_bytes(
            b"JSON.stringify(globalThis.__cu_last ?? null)".as_slice(),
        ))
        .map_err(|e| format!("script error: {}", js_err(&e)))?
        .as_string()
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_else(|| "null".to_string());
    Ok(serde_json::from_str(&json_str).unwrap_or(Value::Null))
}

/// Turn a top-level evaluation failure into something the caller can act on.
///
/// Two of these are near-certain the first time someone moves a working script
/// into `--keep`, and both read as nonsense without the context: a persistent
/// context evaluates at top level, so the `return` that the one-shot path's IIFE
/// made legal is a SyntaxError, and a `const` the previous call already declared
/// cannot be declared again.
fn top_level_error(message: &str) -> String {
    let hint = if message.contains("unexpected token 'return'") {
        Some(
            "a persistent context evaluates at TOP LEVEL so that declarations survive, and top-level `return` is a syntax error there. End with the expression itself (`result` instead of `return result`), or drop `--keep`/`--in` to get the one-shot path's IIFE back.",
        )
    } else if message.contains("redeclaration")
        || message.contains("already been declared")
        // boa reports a re-declared lexical binding as "invalid scope analysis",
        // which names nothing the caller can act on. Observed live against a
        // context that already held the `const`.
        || message.contains("invalid scope analysis")
    {
        Some(
            "this context still holds the declaration from an earlier call — that is what `--keep` is for. Reassign instead of re-declaring, or start over with `script --drop <name>`.",
        )
    } else {
        None
    };
    match hint {
        Some(hint) => format!("script error: {message}\nHint: {hint}"),
        None => format!("script error: {message}"),
    }
}

#[cfg(test)]
mod persistent_context_tests {
    use super::*;

    #[test]
    fn top_level_return_explains_the_iife_difference() {
        let out = top_level_error("SyntaxError: unexpected token 'return', statement at line 1");
        assert!(out.contains("TOP LEVEL"), "{out}");
        assert!(out.contains("one-shot"), "{out}");
    }

    #[test]
    fn redeclaration_points_at_the_context_that_still_holds_it() {
        let out = top_level_error("SyntaxError: redeclaration of lexical binding `tab`");
        assert!(out.contains("--drop"), "{out}");

        // What boa actually says for this case, which on its own names nothing.
        let boa = top_level_error("SyntaxError: invalid scope analysis at line 1, col 1");
        assert!(boa.contains("--drop"), "{boa}");
    }

    /// An ordinary runtime failure must keep its own message and gain nothing.
    #[test]
    fn unrelated_errors_are_passed_through_unchanged() {
        let out = top_level_error("TypeError: cu.click is not a function");
        assert_eq!(out, "script error: TypeError: cu.click is not a function");
    }

    /// A context runs one program at a time. Before the busy flag, a `cu.*`
    /// call that re-entered `script --in <the same name>` queued a job behind
    /// the program waiting for it and the daemon hung forever — the session
    /// would just look unresponsive. Verified live: the inner call is now
    /// refused and the context stays usable afterwards.
    #[test]
    fn a_context_is_marked_busy_only_while_a_job_is_in_flight() {
        let mut contexts = JsContexts::default();
        contexts
            .map
            .insert("re".to_string(), spawn_context_thread().unwrap());
        assert!(!contexts.map["re"].busy, "a fresh context is idle");

        contexts.map.get_mut("re").unwrap().busy = true;
        assert!(contexts.map["re"].busy);
        // Dropping it must work even mid-job, so a wedged context is always
        // recoverable with `script --drop`.
        assert!(contexts.drop_context("re"));
        assert!(contexts.names().is_empty());
    }

    #[test]
    fn dropping_an_unknown_context_reports_false_without_creating_one() {
        let mut contexts = JsContexts::default();
        assert!(!contexts.drop_context("nope"));
        assert!(contexts.names().is_empty());
    }
}

#[cfg(test)]
mod advisory_tests {
    use super::*;

    #[test]
    fn discarded_script_results_still_collect_bounded_advisories() {
        let mut advisories = Vec::new();
        collect_advisory(
            &json!({"success":true,"data":{"result":"ordinary"}}),
            &mut advisories,
        );
        assert!(advisories.is_empty());
        for _ in 0..25 {
            collect_advisory(
                &json!({"success":true,"data":{"observed":{"noProgress":
                {"hint":"Inspect state", "retryAction":false}}}}),
                &mut advisories,
            );
        }
        assert_eq!(advisories.len(), 20);
        assert_eq!(advisories[0]["retryAction"], false);
    }
}
