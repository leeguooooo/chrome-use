//! Reading the page without the page seeing it.
//!
//! JS that chrome-use evaluates in the page's main world calls the page's own
//! functions: a site that wraps `querySelector` or `getBoundingClientRect` and
//! looks at the caller's stack sees an `<anonymous>` caller that is not one of
//! its scripts (botcheck's main-world-reads check). The helpers here do the
//! same reads either with CDP's DOM methods (no JS runs) or in an isolated
//! world, which shares the DOM but has its own prototypes, so nothing the page
//! wrapped is ever called.
//!
//! One isolated world per tab session and main frame, created on first use
//! under a name chosen per process, and created again when its context is
//! gone (a navigation).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use super::cdp::client::CdpClient;

/// Worlds belong to a transport and document, not just a numeric context id.
/// Chrome can reuse an id after navigation without reporting a missing context.
#[derive(Default)]
pub(crate) struct WorldCache(Mutex<WorldCacheState>);

#[derive(Default)]
struct WorldCacheState {
    next_generation: u64,
    worlds: HashMap<String, WorldEntry>,
}

struct WorldEntry {
    generation: u64,
    context: Option<Option<i64>>,
}

impl WorldCache {
    fn cached(&self, session: &str) -> (u64, Option<Option<i64>>) {
        let mut state = self.0.lock().unwrap();
        if let Some(entry) = state.worlds.get(session) {
            return (entry.generation, entry.context);
        }
        let generation = state.next_generation;
        state.next_generation = state.next_generation.wrapping_add(1);
        state.worlds.insert(
            session.to_string(),
            WorldEntry {
                generation,
                context: None,
            },
        );
        (generation, None)
    }

    fn store(&self, session: &str, generation: u64, context: Option<i64>) -> bool {
        let mut state = self.0.lock().unwrap();
        let Some(entry) = state.worlds.get_mut(session) else {
            return false;
        };
        if entry.generation != generation {
            return false;
        }
        entry.context = Some(context);
        true
    }

    fn invalidate(&self, session: &str) {
        let mut state = self.0.lock().unwrap();
        state.worlds.remove(session);
    }

    /// Invalidate before broadcasting, so readers observing a commit cannot
    /// reuse its predecessor's world. Child navigation keeps the main world.
    pub(crate) fn on_event(&self, event: &super::cdp::types::CdpEvent) {
        let session = if event.method == "Target.detachedFromTarget" {
            event.params.get("sessionId").and_then(Value::as_str)
        } else {
            event.session_id.as_deref()
        };
        let Some(session) = session else { return };
        let invalidate = match event.method.as_str() {
            "Page.frameNavigated" => event
                .params
                .pointer("/frame/parentId")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty),
            "Page.frameDetached"
            | "Runtime.executionContextsCleared"
            | "Inspector.detached"
            | "Target.detachedFromTarget" => true,
            "Runtime.executionContextDestroyed" => {
                let state = self.0.lock().unwrap();
                state
                    .worlds
                    .get(session)
                    .and_then(|entry| entry.context.flatten())
                    .is_some_and(|context| {
                        event
                            .params
                            .get("executionContextId")
                            .and_then(Value::as_i64)
                            == Some(context)
                    })
            }
            _ => false,
        };
        if invalidate {
            self.invalidate(session);
        }
    }
}

pub(crate) const WORLD_CHANGED: &str =
    "Page changed while creating its isolated world; observe the page again";

fn world_name() -> &'static str {
    static N: OnceLock<String> = OnceLock::new();
    N.get_or_init(|| format!("w{}", &uuid::Uuid::new_v4().simple().to_string()[..12]))
}

async fn create_world(client: &CdpClient, session_id: &str) -> Result<i64, String> {
    // An event can arrive while createIsolatedWorld is in flight. Never
    // repopulate a cache invalidated in the meantime with its old response.
    for _ in 0..2 {
        let (generation, _) = client.quiet_worlds.cached(session_id);
        let made = try_create_world(client, session_id).await;
        if client
            .quiet_worlds
            .store(session_id, generation, made.as_ref().ok().copied())
        {
            return made;
        }
    }
    Err(WORLD_CHANGED.to_string())
}

async fn try_create_world(client: &CdpClient, session_id: &str) -> Result<i64, String> {
    let tree = client
        .send_command("Page.getFrameTree", None, Some(session_id))
        .await?;
    let frame_id = tree
        .pointer("/frameTree/frame/id")
        .and_then(|v| v.as_str())
        .ok_or("no main frame")?
        .to_string();
    let out = client
        .send_command(
            "Page.createIsolatedWorld",
            Some(json!({ "frameId": frame_id, "worldName": world_name() })),
            Some(session_id),
        )
        .await?;
    out.get("executionContextId")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| "no isolated world context".to_string())
}

/// The world's context is gone (a navigation or reload).
pub fn context_gone(e: &str) -> bool {
    e.contains("Cannot find context with specified id")
        || e.contains("Execution context was destroyed")
        || e.contains("Cannot find default execution context")
}

/// The node lives in a frame the main frame's isolated world cannot reach.
pub fn other_frame(e: &str) -> bool {
    e.contains("does not belong to") || e.contains("Node is not in the target") || context_gone(e)
}

/// Make the tab's isolated world again (after its context went away).
pub async fn recreate(client: &CdpClient, session_id: &str) -> Result<i64, String> {
    create_world(client, session_id).await
}

fn gone(e: &str) -> bool {
    e.contains("Cannot find context")
        || e.contains("Could not find object")
        || e.contains("Execution context was destroyed")
        || e.contains("No node with given id")
        || e.contains("Cannot find default execution context")
}

/// The isolated world's context for this tab, created when there is none.
pub async fn world(client: &CdpClient, session_id: &str) -> Result<i64, String> {
    let (_, cached) = client.quiet_worlds.cached(session_id);
    match cached {
        Some(Some(ctx)) => Ok(ctx),
        Some(None) => Err("no isolated world on this target".to_string()),
        None => create_world(client, session_id).await,
    }
}

/// Run `f` with the world's context, creating the world again once if its
/// context turned out to be gone.
async fn with_world<F, Fut, T>(client: &CdpClient, session_id: &str, f: F) -> Result<T, String>
where
    F: Fn(i64) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let ctx = world(client, session_id).await?;
    match f(ctx).await {
        Err(e) if gone(&e) => {
            let ctx = create_world(client, session_id).await?;
            f(ctx).await
        }
        other => other,
    }
}

fn exception(out: &Value) -> Option<String> {
    out.get("exceptionDetails").map(|ex| {
        ex.pointer("/exception/description")
            .and_then(|v| v.as_str())
            .or_else(|| ex.get("text").and_then(|v| v.as_str()))
            .unwrap_or("page error")
            .to_string()
    })
}

/// A node's objectId in the isolated world.
async fn resolve_in(
    client: &CdpClient,
    session_id: &str,
    backend_node_id: i64,
    ctx: i64,
) -> Result<String, String> {
    let out = client
        .send_command(
            "DOM.resolveNode",
            Some(json!({ "backendNodeId": backend_node_id, "executionContextId": ctx })),
            Some(session_id),
        )
        .await?;
    out.pointer("/object/objectId")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "the element is no longer in the page".to_string())
}

async fn backend_of(client: &CdpClient, session_id: &str, object_id: &str) -> Result<i64, String> {
    let out = client
        .send_command(
            "DOM.describeNode",
            Some(json!({ "objectId": object_id })),
            Some(session_id),
        )
        .await?;
    out.pointer("/node/backendNodeId")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| "not an element".to_string())
}

/// The node a CSS selector names in the top document, found in the
/// isolated world (the page's `querySelector` is never called). None when
/// nothing matches.
pub async fn query_selector(
    client: &CdpClient,
    session_id: &str,
    selector: &str,
) -> Result<Option<i64>, String> {
    let sel = serde_json::to_string(selector).unwrap_or_default();
    let found = with_world(client, session_id, |ctx| {
        let expr = format!("document.querySelector({sel})");
        async move {
            let out = client
                .send_command(
                    "Runtime.evaluate",
                    Some(json!({ "expression": expr, "contextId": ctx, "returnByValue": false })),
                    Some(session_id),
                )
                .await?;
            if let Some(e) = exception(&out) {
                return Err(format!("bad selector: {e}"));
            }
            Ok(out
                .pointer("/result/objectId")
                .and_then(|v| v.as_str())
                .map(String::from))
        }
    })
    .await?;
    match found {
        Some(obj) => Ok(Some(backend_of(client, session_id, &obj).await?)),
        None => Ok(None),
    }
}

/// The node a main-world object handle points at (e.g. an element an
/// adapter passed to `args.ui`), read by property, without calling anything.
pub async fn backend_of_property(
    client: &CdpClient,
    session_id: &str,
    holder_object_id: &str,
    path: &str,
) -> Result<Option<i64>, String> {
    let out = client
        .send_command(
            "Runtime.callFunctionOn",
            Some(json!({
                "objectId": holder_object_id,
                "functionDeclaration": format!("function () {{ return this.{path}; }}"),
                "returnByValue": false,
            })),
            Some(session_id),
        )
        .await?;
    if let Some(e) = exception(&out) {
        return Err(e);
    }
    match out.pointer("/result/objectId").and_then(|v| v.as_str()) {
        Some(obj) => Ok(Some(backend_of(client, session_id, obj).await?)),
        None => Ok(None),
    }
}

/// Call `func` on the node in the isolated world, `this` being the node.
pub async fn call_on_node(
    client: &CdpClient,
    session_id: &str,
    backend_node_id: i64,
    func: &str,
    args: Vec<Value>,
) -> Result<Value, String> {
    with_world(client, session_id, |ctx| {
        let args = args.clone();
        async move {
            let obj = resolve_in(client, session_id, backend_node_id, ctx).await?;
            let out = client
                .send_command(
                    "Runtime.callFunctionOn",
                    Some(json!({
                        "objectId": obj,
                        "functionDeclaration": func,
                        "arguments": args,
                        "returnByValue": true,
                    })),
                    Some(session_id),
                )
                .await?;
            if let Some(e) = exception(&out) {
                return Err(e);
            }
            Ok(out.pointer("/result/value").cloned().unwrap_or(Value::Null))
        }
    })
    .await
}

/// Box (top-level viewport CSS px, same-origin frames offset), viewport size
/// and nearest scrollable ancestor of a node: the shape `args.ui` uses.
pub const MEASURE_FN: &str = "function () {\n\
    const el = this;\n\
    if (!el.isConnected) return { error: 'element is no longer in the page' };\n\
    let ox = 0, oy = 0;\n\
    try { let w = el.ownerDocument.defaultView; while (w && w !== window && w.frameElement) { const f = w.frameElement; const b = f.getBoundingClientRect(); ox += b.left + f.clientLeft; oy += b.top + f.clientTop; w = w.parent; } } catch (_) {}\n\
    const b = el.getBoundingClientRect();\n\
    let sc = null;\n\
    for (let p = el.parentElement; p; p = p.parentElement) {\n\
      const cs = getComputedStyle(p);\n\
      if (/(auto|scroll)/.test(cs.overflowY) && p.scrollHeight > p.clientHeight + 1) { const pb = p.getBoundingClientRect(); sc = { x: pb.left + ox, y: pb.top + oy, w: pb.width, h: pb.height }; break; }\n\
    }\n\
    return { x: b.left + ox, y: b.top + oy, w: b.width, h: b.height, vw: window.innerWidth, vh: window.innerHeight, scroller: sc };\n\
}";

pub async fn measure(
    client: &CdpClient,
    session_id: &str,
    backend_node_id: i64,
) -> Result<Value, String> {
    call_on_node(client, session_id, backend_node_id, MEASURE_FN, vec![]).await
}

/// Whether the point (top-level viewport CSS px) lands on the node or inside
/// it: `(hit, what is there instead)`. `elementFromPoint` in the isolated
/// world, where the page's own copy is never called; it takes viewport
/// coordinates and passes through `pointer-events: none`, as a real click
/// does. (`DOM.getNodeForLocation` does not take viewport coordinates: on a
/// scrolled page it answered for a different spot.)
pub async fn hit_test(
    client: &CdpClient,
    session_id: &str,
    backend_node_id: i64,
    x: f64,
    y: f64,
) -> Result<(bool, Option<String>), String> {
    let v = call_on_node(
        client,
        session_id,
        backend_node_id,
        "function (x, y) {\n\
            const el = this;\n\
            let top = document.elementFromPoint(x, y);\n\
            while (top && top.shadowRoot && top !== el) { const inner = top.shadowRoot.elementFromPoint(x, y); if (!inner || inner === top) break; top = inner; }\n\
            let hit = false;\n\
            for (let p = top; p; p = p.parentNode || p.host || null) { if (p === el) { hit = true; break; } }\n\
            if (!hit && top && top.tagName === 'IFRAME' && el.ownerDocument !== document) hit = true;\n\
            const d = top ? (top.tagName.toLowerCase() + (top.id ? '#' + top.id : '') + (typeof top.className === 'string' && top.className ? '.' + top.className.trim().split(/\\s+/).slice(0, 2).join('.') : '')) : null;\n\
            return { hit, top: hit ? null : d };\n\
        }",
        vec![json!({ "value": x }), json!({ "value": y })],
    )
    .await?;
    Ok((
        v.get("hit").and_then(|b| b.as_bool()).unwrap_or(false),
        v.get("top").and_then(|t| t.as_str()).map(String::from),
    ))
}
