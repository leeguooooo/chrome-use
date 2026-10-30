//! Single-pass scripting — the `script` daemon action (Phase 1).
//!
//! Runs a JSON op-list in ONE round-trip over the resident daemon, with a value
//! bus + control flow, by re-entering `execute_command` per op. Each op therefore
//! goes through the identical policy / stealth / humanize / @ref-heal / stale-
//! recovery path as a standalone command — nothing is reimplemented. This is the
//! "code base, not CLI base" surpass work: a later op consumes an earlier op's
//! result via `{{name.path}}`, and `waitUntil` / `forEach` / `assert` / `set` /
//! `push` / `return` give real control flow without the LLM back in the loop.
//!
//! A statement is one of:
//!   { "do": "<action>", ...params, "bind"?: "name", "optional"?: bool,
//!     "or"?: [ { "do": ... }, ... ] }   // alternatives if it found nothing to act on
//!   { "waitUntil": <cond>, "timeoutMs"?: n, "pollMs"?: n }
//!   { "forEach": "{{list}}", "as"?: "item", "max": n, "body": [ ...stmts ] }
//!   { "assert": <cond>, "msg"?: "..." }
//!   { "set": "name", "value": <val|"{{expr}}"> }
//!   { "push": "name", "value": <val|"{{expr}}"> }
//!   { "return": <val|"{{expr}}"> }
//!   { "log": <val|"...{{expr}}..."> }
//!
//! <cond> is one of: {"eq":[a,b]} {"ne":[a,b]} {"contains":[a,b]} {"matches":[a,rx]}
//!                   {"exists":"<selector>"} {"gone":"<selector>"}
//!
//! `{{expr}}` value bus: a string that is EXACTLY `{{path}}` becomes the looked-up
//! JSON value (type-preserving); an embedded `..{{path}}..` string-interpolates.
//! `path` is dotted with array indices: `rows.0.ref`, `rows[0].url`.

use super::actions::{execute_command, DaemonState};
use serde_json::{json, Map, Value};
use std::time::{Duration, Instant};

const MAX_OPS: usize = 100_000;
const DEFAULT_WAIT_MS: u64 = 10_000;

enum Flow {
    Normal,
    Return(Value),
}

enum PolicyStop {
    Pending(Value),
    Refused,
}

struct Ctx {
    vars: Map<String, Value>,
    steps: Vec<Value>,
    advisories: Vec<Value>,
    start: Instant,
    budget: Option<Duration>,
    auto_confirm: bool,
    policy_stop: Option<PolicyStop>,
    op_count: usize,
}

impl Ctx {
    fn timed_out(&self) -> bool {
        self.budget.is_some_and(|b| self.start.elapsed() > b)
    }
}

/// Entry point wired into `execute_command`'s match as the `"script"` action.
pub async fn handle_script(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    // JS engine path (Phase 2): a `source` string is a real JavaScript program
    // driven through the `cu.*` helpers. The JSON `program` path is below.
    // Context management, before anything tries to run (#289).
    if let Some(name) = cmd.get("dropContext").and_then(|v| v.as_str()) {
        let dropped = state.script_contexts.drop_context(name);
        return Ok(json!({
            "ok": true,
            "dropped": dropped,
            "context": name,
            "contexts": state.script_contexts.names(),
        }));
    }
    if cmd
        .get("listContexts")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Ok(json!({ "ok": true, "contexts": state.script_contexts.names() }));
    }

    if let Some(source) = cmd.get("source").and_then(|v| v.as_str()) {
        let timeout_ms = cmd.get("timeout_ms").and_then(|v| v.as_u64());

        // A named context keeps its declarations between calls, so the program
        // is evaluated at top level instead of inside an IIFE. `--keep` creates
        // the context on demand; `--in` refuses an unknown name, because
        // silently starting an empty one would lose exactly the state the
        // caller was reaching for and only show up as a confusing
        // `ReferenceError` further in.
        let named = cmd
            .get("keepContext")
            .and_then(|v| v.as_str())
            .map(|n| (n, true))
            .or_else(|| {
                cmd.get("inContext")
                    .and_then(|v| v.as_str())
                    .map(|n| (n, false))
            });
        if let Some((name, create)) = named {
            return match super::script_js::run_js_in(name, source, create, state).await {
                Ok(data) => {
                    let mut obj = data;
                    if let Some(m) = obj.as_object_mut() {
                        m.entry("ok").or_insert(json!(true));
                    }
                    Ok(obj)
                }
                Err(e) => Ok(json!({
                    "ok": false,
                    "error": e,
                    "return": Value::Null,
                    "context": name,
                })),
            };
        }

        return match super::script_js::run_js(source, timeout_ms, state).await {
            Ok(data) => {
                let mut obj = data;
                if let Some(m) = obj.as_object_mut() {
                    m.entry("ok").or_insert(json!(true));
                }
                Ok(obj)
            }
            Err(e) => Ok(json!({ "ok": false, "error": e, "return": Value::Null })),
        };
    }

    let program = cmd
        .get("program")
        .and_then(|v| v.as_array())
        .ok_or("`script` requires a `program` array or a `source` string")?
        .clone();

    // Structural validation up front so a malformed program is exit-code 2
    // (invalid) rather than a mid-run runtime failure (exit 1).
    validate_block(&program)?;

    if cmd.get("dryRun").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Ok(json!({
            "dryRun": true,
            "opCount": count_ops(&program),
            "program": program,
        }));
    }

    let mut ctx = Ctx {
        vars: Map::new(),
        steps: Vec::new(),
        advisories: Vec::new(),
        start: Instant::now(),
        budget: cmd
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .map(Duration::from_millis),
        auto_confirm: cmd
            .get("autoConfirm")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        op_count: 0,
        policy_stop: None,
    };
    if let Some(args) = cmd.get("args").and_then(|v| v.as_object()) {
        for (k, v) in args {
            ctx.vars.insert(k.clone(), v.clone());
        }
    }

    let flow = run_block(&program, &mut ctx, state).await;
    let (ret, err) = match flow {
        Ok(Flow::Return(v)) => (v, None),
        Ok(Flow::Normal) => (Value::Null, None),
        Err(e) => (Value::Null, Some(e)),
    };
    let timed_out = ctx.timed_out();
    let result = json!({
        "ok": err.is_none() && !timed_out,
        "return": ret,
        "vars": Value::Object(ctx.vars),
        "steps": ctx.steps,
        "advisories": ctx.advisories,
        "timedOut": timed_out,
        "error": err,
    });
    Ok(match ctx.policy_stop {
        Some(PolicyStop::Pending(gate)) => pending_script_result(&gate, result),
        Some(PolicyStop::Refused) => {
            let mut result = result;
            result["approval_refused"] = json!(true);
            result
        }
        None => result,
    })
}

/// Only permission metadata crosses the pause boundary, never the held command.
pub(crate) fn approval_payload(gate: &Value) -> Value {
    let mut data = Map::new();
    for key in [
        "confirmation_required",
        "confirmation_id",
        "approval",
        "site",
        "action",
        "category",
        "description",
        "requestedActions",
    ] {
        if let Some(value) = gate.get(key) {
            data.insert(key.to_string(), value.clone());
        }
    }
    Value::Object(data)
}

pub(super) fn pending_gate(response: &Value) -> Option<Value> {
    (response["success"] == true
        && response.pointer("/data/confirmation_required") == Some(&json!(true)))
    .then(|| approval_payload(&response["data"]))
}

pub(super) fn execution_barrier(response: &Value) -> Option<Value> {
    pending_gate(response).or_else(|| {
        (response["success"] == true && response.pointer("/data/approval_refused") == Some(&json!(true)))
            .then(|| json!({"approval_refused":true,"error":response.pointer("/data/error").cloned().unwrap_or(Value::Null)}))
    })
}

pub(super) fn policy_stopped_result(stop: &Value, mut result: Value) -> Value {
    if stop["approval_refused"] != true {
        return pending_script_result(stop, result);
    }
    result["ok"] = json!(false);
    result["return"] = Value::Null;
    result["approval_refused"] = json!(true);
    result["error"] = stop
        .get("error")
        .cloned()
        .filter(|e| e.is_string())
        .unwrap_or_else(|| json!("Script stopped after an approved operation was refused"));
    result
}

pub(super) fn pending_script_result(gate: &Value, mut result: Value) -> Value {
    if let Some(fields) = approval_payload(gate).as_object() {
        for (key, value) in fields {
            result[key] = value.clone();
        }
    }
    result["ok"] = json!(false);
    result["return"] = Value::Null;
    result["error"] = json!(format!("Script paused for confirmation {}; approve outside this execution, then continue only the remaining operations", gate["confirmation_id"].as_str().unwrap_or("")));
    result
}

fn approval_refusal(response: &Value) -> Option<String> {
    if response.pointer("/data/denied") == Some(&json!(true)) {
        return Some("approval was declined".into());
    }
    if response["success"] == true {
        return None;
    }
    let message = response["error"].as_str()?;
    let lower = message.to_ascii_lowercase();
    let policy = lower.starts_with("action '")
        && (lower.contains(" denied by policy:")
            || lower.contains(" denied by your organization's chrome-use policy:")
            || lower.contains(" refused:"));
    (policy
        || lower.starts_with("confirmation target")
        || lower.starts_with("could not verify confirmation target")
        || lower.starts_with("cannot classify the intended field for site policy")
        || lower.starts_with("approved navigation "))
    .then(|| message.to_string())
}

/// Resolve one confirmation attempt before optional/fallback error handling.
fn confirmation_reply(response: Value, original_gate: &Value, state: &DaemonState) -> Value {
    let response = response
        .pointer("/data/result")
        .cloned()
        .unwrap_or(response);
    if execution_barrier(&response).is_some() {
        return response;
    }
    let original_id = original_gate["confirmation_id"]
        .as_str()
        .filter(|id| !id.is_empty());
    let held_id = state.pending_confirmation.as_ref().and_then(|pending| {
        pending
            .site
            .as_ref()
            .map(|site| site.id.as_str())
            .or_else(|| pending.cmd["id"].as_str())
    });
    if original_id.is_some() && original_id == held_id {
        return json!({"success":true,"data":original_gate,"error":null});
    }
    if let Some(reason) = approval_refusal(&response) {
        let mut data = json!({"approval_refused":true,"error":reason});
        if let Some(steps) = response.pointer("/data/steps").filter(|s| s.is_array()) {
            data["steps"] = steps.clone();
        }
        return json!({"success":true,"data":data,"error":null});
    }
    response
}

fn hold_confirmation(
    ctx: &mut Ctx,
    verb: &str,
    gate: Value,
    nested_steps: Option<&Value>,
) -> String {
    let message = format!(
        "op `{verb}` paused for confirmation {}",
        gate["confirmation_id"].as_str().unwrap_or("")
    );
    let mut step = json!({"op":verb,"ok":false,"confirmation_required":true});
    if let Some(steps) = nested_steps.filter(|steps| steps.is_array()) {
        step["steps"] = steps.clone();
    }
    ctx.steps.push(step);
    ctx.policy_stop = Some(PolicyStop::Pending(gate));
    message
}

fn hold_refusal(ctx: &mut Ctx, verb: &str, reason: &str, nested_steps: Option<&Value>) -> String {
    let mut step = json!({"op":verb,"ok":false,"approval_refused":true});
    if let Some(steps) = nested_steps.filter(|steps| steps.is_array()) {
        step["steps"] = steps.clone();
    }
    ctx.steps.push(step);
    ctx.policy_stop = Some(PolicyStop::Refused);
    format!("op `{verb}` stopped: {reason}")
}

fn run_block<'a>(
    block: &'a [Value],
    ctx: &'a mut Ctx,
    state: &'a mut DaemonState,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Flow, String>> + Send + 'a>> {
    Box::pin(async move {
        for stmt in block {
            if ctx.timed_out() {
                return Err("script exceeded timeout_ms".to_string());
            }
            ctx.op_count += 1;
            if ctx.op_count > MAX_OPS {
                return Err("script exceeded op limit".to_string());
            }
            let obj = stmt
                .as_object()
                .ok_or("each statement must be a JSON object")?;

            if obj.contains_key("do") {
                if let Err(e) = run_action(obj, ctx, state).await {
                    if ctx.policy_stop.is_some() {
                        return Err(e);
                    }
                    run_fallbacks(obj, e, ctx, state).await?;
                }
            } else if let Some(ret) = obj.get("return") {
                return Ok(Flow::Return(resolve(ret, &ctx.vars)));
            } else if let Some(name) = obj.get("set").and_then(|v| v.as_str()) {
                let val = resolve(obj.get("value").unwrap_or(&Value::Null), &ctx.vars);
                ctx.vars.insert(name.to_string(), val);
            } else if let Some(name) = obj.get("push").and_then(|v| v.as_str()) {
                let val = resolve(obj.get("value").unwrap_or(&Value::Null), &ctx.vars);
                let entry = ctx
                    .vars
                    .entry(name.to_string())
                    .or_insert_with(|| json!([]));
                if let Some(arr) = entry.as_array_mut() {
                    arr.push(val);
                } else {
                    return Err(format!("`push` target `{}` is not an array", name));
                }
            } else if let Some(cond) = obj.get("assert") {
                let ok = eval_cond(cond, ctx, state).await?;
                ctx.steps.push(json!({ "assert": cond, "ok": ok }));
                if !ok {
                    let msg = obj
                        .get("msg")
                        .and_then(|v| v.as_str())
                        .unwrap_or("assertion failed");
                    return Err(format!("assert failed: {}", msg));
                }
            } else if obj.contains_key("waitUntil") {
                run_wait_until(obj, ctx, state).await?;
            } else if obj.contains_key("forEach") {
                if let Flow::Return(v) = run_for_each(obj, ctx, state).await? {
                    return Ok(Flow::Return(v));
                }
            } else if let Some(msg) = obj.get("log") {
                ctx.steps.push(json!({ "log": resolve(msg, &ctx.vars) }));
            } else {
                return Err(format!("unrecognized statement: {}", stmt));
            }
        }
        Ok(Flow::Normal)
    })
}

async fn run_action(
    obj: &Map<String, Value>,
    ctx: &mut Ctx,
    state: &mut DaemonState,
) -> Result<(), String> {
    let verb = obj
        .get("do")
        .and_then(|v| v.as_str())
        .ok_or("`do` must be an action name")?;
    let bind = obj.get("bind").and_then(|v| v.as_str()).map(String::from);
    let optional = obj
        .get("optional")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut sub = Map::new();
    for (k, v) in obj {
        if matches!(k.as_str(), "do" | "bind" | "optional" | "or") {
            continue;
        }
        sub.insert(normalize_key(k), resolve(v, &ctx.vars));
    }
    sub.insert("action".to_string(), json!(alias_action(verb)));
    sub.insert("id".to_string(), json!(format!("s{}", ctx.op_count)));

    let mut resp = Box::pin(execute_command(&Value::Object(sub), state)).await;

    if let Some(original_gate) = pending_gate(&resp) {
        let nested_pause =
            alias_action(verb) == "script" && resp.pointer("/data/ok") == Some(&json!(false));
        if ctx.auto_confirm && !nested_pause {
            let confirm = json!({"action":"confirm", "id":format!("s{}c", ctx.op_count)});
            let reply = Box::pin(execute_command(&confirm, state)).await;
            resp = confirmation_reply(reply, &original_gate, state);
        }
    }
    if let Some(stop) = execution_barrier(&resp) {
        return Err(if stop["approval_refused"] == true {
            hold_refusal(
                ctx,
                verb,
                stop["error"]
                    .as_str()
                    .unwrap_or("an approved operation was refused"),
                resp.pointer("/data/steps"),
            )
        } else {
            hold_confirmation(ctx, verb, stop, resp.pointer("/data/steps"))
        });
    }

    super::progress::collect_advisories(&resp, &mut ctx.advisories);
    let ok = resp
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        && (alias_action(verb) != "script" || resp.pointer("/data/ok") == Some(&json!(true)));
    let data = resp.get("data").cloned().unwrap_or(Value::Null);
    let errmsg = resp
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| {
            if ok {
                None
            } else {
                data.get("error").and_then(Value::as_str)
            }
        })
        .map(String::from);

    let mut step = json!({ "op": verb, "ok": ok, "error": errmsg });
    if let Some(advisory) = data.pointer("/observed/noProgress") {
        step["noProgress"] = advisory.clone();
    }
    ctx.steps.push(step);

    if let Some(name) = bind {
        ctx.vars.insert(
            name,
            if ok {
                data
            } else {
                json!({ "error": errmsg.clone() })
            },
        );
    }
    if !ok && !optional {
        return Err(format!(
            "op `{}` failed: {}",
            verb,
            errmsg.unwrap_or_else(|| "unknown error".to_string())
        ));
    }
    Ok(())
}

/// `"or": [<do statement>, ...]` on a `do`: alternatives tried in order when
/// the primary op found nothing to act on (`chrome-use teach` writes a
/// semantic locator first and a CSS selector after it). Only a failure that
/// dispatched nothing falls back, so an action is never done twice.
async fn run_fallbacks(
    obj: &Map<String, Value>,
    err: String,
    ctx: &mut Ctx,
    state: &mut DaemonState,
) -> Result<(), String> {
    let Some(alts) = obj.get("or").and_then(Value::as_array) else {
        return Err(err);
    };
    if !nothing_dispatched(&err) {
        return Err(err);
    }
    let mut last = err;
    for alt in alts {
        let Some(alt) = alt.as_object() else { continue };
        let mut alt = alt.clone();
        alt.remove("or");
        if let Some(bind) = obj.get("bind") {
            alt.insert("bind".into(), bind.clone());
        }
        match run_action(&alt, ctx, state).await {
            Ok(()) => {
                ctx.steps
                    .push(json!({ "fallback": alt.get("do"), "after": last }));
                return Ok(());
            }
            Err(e) if ctx.policy_stop.is_some() => return Err(e),
            Err(e) if nothing_dispatched(&e) => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

/// The op failed before acting: its target was missing or ambiguous.
fn nothing_dispatched(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("nothing was dispatched")
        || e.contains("no element")
        || e.contains("not found")
        || e.contains("selector_ambiguous")
        || e.contains("matched ")
}

async fn run_wait_until(
    obj: &Map<String, Value>,
    ctx: &mut Ctx,
    state: &mut DaemonState,
) -> Result<(), String> {
    let cond = obj.get("waitUntil").unwrap().clone();
    let timeout = obj
        .get("timeoutMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_WAIT_MS);
    let poll = obj
        .get("pollMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(300)
        .max(50);
    let start = Instant::now();
    loop {
        if eval_cond(&cond, ctx, state).await? {
            ctx.steps.push(json!({ "waitUntil": cond, "ok": true }));
            return Ok(());
        }
        if start.elapsed() > Duration::from_millis(timeout) || ctx.timed_out() {
            return Err(format!("waitUntil timed out after {}ms: {}", timeout, cond));
        }
        tokio::time::sleep(Duration::from_millis(poll)).await;
    }
}

async fn run_for_each(
    obj: &Map<String, Value>,
    ctx: &mut Ctx,
    state: &mut DaemonState,
) -> Result<Flow, String> {
    let list = resolve(obj.get("forEach").unwrap(), &ctx.vars);
    let items = list
        .as_array()
        .ok_or("forEach target did not resolve to an array")?
        .clone();
    let as_name = obj
        .get("as")
        .and_then(|v| v.as_str())
        .unwrap_or("item")
        .to_string();
    let max = obj
        .get("max")
        .and_then(|v| v.as_u64())
        .ok_or("forEach requires a `max` (hard iteration cap)")? as usize;
    let body = obj.get("body").and_then(|v| v.as_array()).unwrap().clone();

    for (i, item) in items.into_iter().take(max).enumerate() {
        ctx.vars.insert(as_name.clone(), item);
        ctx.vars.insert(format!("{}Index", as_name), json!(i));
        if let Flow::Return(v) = run_block(&body, ctx, state).await? {
            return Ok(Flow::Return(v));
        }
    }
    Ok(Flow::Normal)
}

async fn eval_cond(cond: &Value, ctx: &mut Ctx, state: &mut DaemonState) -> Result<bool, String> {
    let obj = cond.as_object().ok_or("condition must be a JSON object")?;
    if let Some(p) = obj.get("eq") {
        let (a, b) = two(p, &ctx.vars)?;
        return Ok(a == b);
    }
    if let Some(p) = obj.get("ne") {
        let (a, b) = two(p, &ctx.vars)?;
        return Ok(a != b);
    }
    if let Some(p) = obj.get("contains") {
        let (a, b) = two(p, &ctx.vars)?;
        return Ok(json_contains(&a, &b));
    }
    if let Some(p) = obj.get("matches") {
        let (a, b) = two(p, &ctx.vars)?;
        let hay = as_text(&a);
        let rx = as_text(&b);
        return Ok(regex_lite::Regex::new(&rx)
            .map(|r| r.is_match(&hay))
            .unwrap_or(false));
    }
    if let Some(sel) = obj.get("exists") {
        return probe_visible(&resolve(sel, &ctx.vars), ctx, state).await;
    }
    if let Some(sel) = obj.get("gone") {
        return Ok(!probe_visible(&resolve(sel, &ctx.vars), ctx, state).await?);
    }
    Err(format!("unsupported condition: {}", cond))
}

async fn probe_visible(
    sel: &Value,
    ctx: &mut Ctx,
    state: &mut DaemonState,
) -> Result<bool, String> {
    let cmd = json!({ "action": "isvisible", "selector": sel, "id": "cond" });
    let r = Box::pin(execute_command(&cmd, state)).await;
    if let Some(gate) = pending_gate(&r) {
        return Err(hold_confirmation(ctx, "isvisible", gate, None));
    }
    if r.get("success").and_then(|v| v.as_bool()) != Some(true) {
        return Ok(false);
    }
    let d = r.get("data");
    Ok(d.and_then(|d| d.get("visible"))
        .and_then(|v| v.as_bool())
        .or_else(|| d.and_then(|v| v.as_bool()))
        .unwrap_or(false))
}

// --- value bus -------------------------------------------------------------

fn resolve(v: &Value, vars: &Map<String, Value>) -> Value {
    match v {
        Value::String(s) => resolve_str(s, vars),
        Value::Array(a) => Value::Array(a.iter().map(|x| resolve(x, vars)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), resolve(x, vars)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn resolve_str(s: &str, vars: &Map<String, Value>) -> Value {
    let trimmed = s.trim();
    // Exact single {{expr}} → type-preserving value.
    if let Some(inner) = trimmed
        .strip_prefix("{{")
        .and_then(|r| r.strip_suffix("}}"))
    {
        if !inner.contains("{{") && !inner.contains("}}") {
            return lookup(inner.trim(), vars).unwrap_or(Value::Null);
        }
    }
    if !s.contains("{{") {
        return Value::String(s.to_string());
    }
    // Embedded templates → string interpolation.
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        if let Some(end) = after.find("}}") {
            let expr = after[..end].trim();
            if let Some(val) = lookup(expr, vars) {
                out.push_str(&as_text(&val));
            }
            rest = &after[end + 2..];
        } else {
            out.push_str("{{");
            rest = after;
        }
    }
    out.push_str(rest);
    Value::String(out)
}

fn lookup(path: &str, vars: &Map<String, Value>) -> Option<Value> {
    let norm = path.replace('[', ".").replace(']', "");
    let mut segs = norm.split('.').filter(|s| !s.is_empty());
    let first = segs.next()?;
    let mut cur = vars.get(first)?.clone();
    for seg in segs {
        cur = if let Ok(idx) = seg.parse::<usize>() {
            cur.get(idx).cloned()?
        } else {
            cur.get(seg).cloned()?
        };
    }
    Some(cur)
}

fn two(p: &Value, vars: &Map<String, Value>) -> Result<(Value, Value), String> {
    let a = p.as_array().ok_or("comparison expects a 2-element array")?;
    if a.len() != 2 {
        return Err("comparison expects exactly 2 elements".to_string());
    }
    Ok((resolve(&a[0], vars), resolve(&a[1], vars)))
}

fn as_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn json_contains(hay: &Value, needle: &Value) -> bool {
    match hay {
        Value::String(s) => s.contains(&as_text(needle)),
        Value::Array(a) => a.contains(needle),
        _ => false,
    }
}

// --- verb / param aliases --------------------------------------------------

fn alias_action(verb: &str) -> String {
    match verb {
        "open" | "goto" => "navigate",
        "eval" | "js" => "evaluate",
        other => other,
    }
    .to_string()
}

fn normalize_key(k: &str) -> String {
    match k {
        "js" | "code" | "expression" => "script",
        other => other,
    }
    .to_string()
}

// --- validation / counting -------------------------------------------------

fn count_ops(block: &[Value]) -> usize {
    let mut n = 0;
    for s in block {
        if let Some(o) = s.as_object() {
            if o.contains_key("do") {
                n += 1;
            } else if let Some(body) = o.get("body").and_then(|v| v.as_array()) {
                n += count_ops(body);
            }
        }
    }
    n
}

fn validate_block(block: &[Value]) -> Result<(), String> {
    for stmt in block {
        let obj = stmt
            .as_object()
            .ok_or_else(|| format!("statement must be an object: {}", stmt))?;
        let recognized = [
            "do",
            "return",
            "set",
            "push",
            "assert",
            "waitUntil",
            "forEach",
            "log",
        ];
        if !recognized.iter().any(|k| obj.contains_key(*k)) {
            return Err(format!("unrecognized statement (no known key): {}", stmt));
        }
        if let Some(alts) = obj.get("or") {
            let ok = obj.contains_key("do")
                && alts.as_array().is_some_and(|a| {
                    a.iter()
                        .all(|x| x.as_object().is_some_and(|o| o.contains_key("do")))
                });
            if !ok {
                return Err(
                    "`or` belongs on a `do` statement and lists `do` statements".to_string()
                );
            }
        }
        if obj.contains_key("forEach") {
            if obj.get("max").and_then(|v| v.as_u64()).is_none() {
                return Err("forEach requires a numeric `max`".to_string());
            }
            let body = obj
                .get("body")
                .and_then(|v| v.as_array())
                .ok_or("forEach requires a `body` array")?;
            validate_block(body)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("h1".into(), json!({ "result": "Example Domain" }));
        m.insert("n".into(), json!(3));
        m.insert("rows".into(), json!([{ "ref": "e1" }, { "ref": "e2" }]));
        m
    }

    #[test]
    fn or_alternatives_must_be_do_statements_on_a_do() {
        assert!(validate_block(&[
            json!({"do": "click", "selector": "#a", "or": [{"do": "click", "selector": "#b"}]})
        ])
        .is_ok());
        assert!(validate_block(&[json!({"do": "click", "or": [{"set": "x"}]})]).is_err());
        assert!(validate_block(&[json!({"log": "x", "or": []})]).is_err());
        assert!(nothing_dispatched(
            "semantic locator has no visible match in its scope; nothing was dispatched"
        ));
        assert!(!nothing_dispatched(
            "click landed but the page did not change"
        ));
    }

    #[test]
    fn exact_placeholder_preserves_type() {
        // `{{n}}` alone → the number 3, not the string "3".
        assert_eq!(resolve_str("{{n}}", &vars()), json!(3));
        assert_eq!(
            resolve_str("{{rows}}", &vars()),
            json!([{"ref":"e1"},{"ref":"e2"}])
        );
    }

    #[test]
    fn embedded_template_stringifies() {
        assert_eq!(
            resolve_str("{{h1.result}} has {{n}} links", &vars()),
            json!("Example Domain has 3 links")
        );
    }

    #[test]
    fn lookup_dotted_and_indexed() {
        let v = vars();
        assert_eq!(lookup("h1.result", &v), Some(json!("Example Domain")));
        assert_eq!(lookup("rows.0.ref", &v), Some(json!("e1")));
        assert_eq!(lookup("rows[1].ref", &v), Some(json!("e2")));
        assert_eq!(lookup("rows.5.ref", &v), None);
        assert_eq!(lookup("missing", &v), None);
    }

    #[test]
    fn resolve_recurses_into_params() {
        let v = vars();
        let out = resolve(&json!({ "selector": "{{rows.0.ref}}", "n": "{{n}}" }), &v);
        assert_eq!(out, json!({ "selector": "e1", "n": 3 }));
    }

    #[test]
    fn validate_rejects_foreach_without_max() {
        let bad = json!([{ "forEach": "{{rows}}", "body": [] }]);
        assert!(validate_block(bad.as_array().unwrap()).is_err());
        let good = json!([{ "forEach": "{{rows}}", "max": 5, "body": [] }]);
        assert!(validate_block(good.as_array().unwrap()).is_ok());
    }

    #[test]
    fn validate_rejects_unknown_statement() {
        let bad = json!([{ "nonsense": 1 }]);
        assert!(validate_block(bad.as_array().unwrap()).is_err());
    }

    #[test]
    fn count_ops_includes_foreach_body() {
        let prog = json!([
            { "do": "navigate", "url": "x" },
            { "forEach": "{{rows}}", "max": 2, "body": [ { "do": "click", "selector": "{{item.ref}}" } ] },
            { "return": "done" }
        ]);
        assert_eq!(count_ops(prog.as_array().unwrap()), 2);
    }
}

#[cfg(test)]
#[path = "script_confirmation_tests.rs"]
mod confirmation_tests;
