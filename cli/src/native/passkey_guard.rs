//! The `create()` guard of `auth login --bwu`: while chrome-use's temporary
//! virtual authenticator is installed, the page cannot register a passkey
//! with it. Every `navigator.credentials.create()` is refused before it
//! reaches Chrome, with the error Chrome itself gives for a refused ceremony.
//! A passkey sign-in has no business creating any credential, so nothing is
//! passed through and the original is never called from the page's realm.
//! Chrome's `credentialAdded` event stays the backstop.
//!
//! The guard is a `Proxy` around the original on `CredentialsContainer`'s
//! prototype (so `CredentialsContainer.prototype.create.call(...)` is refused
//! too), and around an own `create` of the `navigator.credentials` instance
//! when the page put one there. Its handler has no prototype, so no trap
//! lookup ever reaches page code (`Object.prototype.get` and friends would
//! otherwise be called with the handler and the original). Page script has no
//! way to read a proxy's target or handler; only CDP can, through the proxy's
//! `[[Target]]`. The page can still tell a guard is there:
//! `Function.prototype.toString` gives `function () { [native code] }`,
//! without the native's name.
//!
//! Where it is installed:
//! - every new document of the tab and of each attached out-of-process frame,
//!   by `Page.addScriptToEvaluateOnNewDocument`, which runs before any page
//!   script; the builtins it uses are taken before anything else runs;
//! - right away, in the current document and in every same-process frame
//!   (reached in its main world through its document node, without
//!   `Runtime.enable`) and in each attached out-of-process frame.
//!
//! Known limits, by design: a document that ran before the guard (the page
//! open when the login starts, and its frames) can have kept the original
//! `create` or replaced `Proxy` / `Object.defineProperty` first; page script
//! cannot win against page script that ran first. An out-of-process frame
//! that attaches after the install is not guarded. In both cases a
//! registration still shows up as `credentialAdded` and stops the login.
//!
//! Restoring is by identity, never by source text: a guard installed through
//! CDP is the very object whose handle we kept, and every guard carries this
//! install's random key on its handler, which the page can neither read nor
//! guess. A proxy that only looks like ours is left alone. The documents are
//! searched through CDP alone, never by calling a function the page could
//! have replaced since; where the page replaced what the search goes through
//! (`navigator`, its `credentials`, `CredentialsContainer`), or hid `create`
//! under too many proxies, the restore reports a guard may be left rather
//! than calling the document clean.
//!
//! The guard is looked for on every object of the `[[Prototype]]` chains of
//! `navigator.credentials` and of `CredentialsContainer.prototype`, not only
//! where it was put: the page can move it deeper (onto a new object it puts
//! between the prototype and `Object.prototype`). A chain the page reshaped
//! is not itself a guard left: what fails the restore is a chain that cannot
//! be walked through CDP (a proxy in it, more than 64 objects, an accessor
//! `create`) or a `create` that, looked up from either root afterwards, still
//! resolves to a guard of ours. A `create` that resolves to the page's own
//! function is left as the page made it; a function of the page's that kept
//! a guard of ours to call is beyond what CDP can see.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};

use super::cdp::client::CdpClient;
use super::element::GroupRelease;

/// Object group of every handle the guard holds or uses.
pub(super) const GROUP: &str = "chrome-use-passkey-guard";

/// The guard installer, as a function of the install's key, which every
/// guard's handler carries. Returns `[prototype, instance, prototype guard,
/// instance guard or null]`, or null, with nothing left replaced, when
/// `create` could not be guarded.
const INSTALLER: &str = r#"function (key) {
    'use strict';
    const P = Proxy, O = Object, proto = O.getPrototypeOf, own = O.getOwnPropertyDescriptor, define = O.defineProperty;
    const reject = Promise.reject.bind(Promise), E = DOMException;
    const msg = 'The operation either timed out or was not allowed. See: https://www.w3.org/TR/webauthn-2/#sctn-privacy-considerations-client.';
    const c = navigator.credentials;
    const p = c && proto(c);
    const d = p && own(p, 'create');
    if (!d || typeof d.value !== 'function') return null;
    const o = own(c, 'create');
    if (o && typeof o.value !== 'function') return null;
    // All or nothing: a value that cannot be replaced is found before
    // anything is, and a replacement that fails takes the others back.
    const settable = (x) => x.writable || x.configurable;
    if (!settable(d) || (o && !settable(o))) return null;
    const wrap = (f) => {
        const h = { __proto__: null, apply() { return reject(new E(msg, 'NotAllowedError')); } };
        h[key] = 0;
        return new P(f, h);
    };
    const gp = wrap(d.value), gc = o ? wrap(o.value) : null;
    try {
        define(p, 'create', { __proto__: null, ...d, value: gp });
        if (o) define(c, 'create', { __proto__: null, ...o, value: gc });
        // Only when the guards really replaced create().
        if (own(p, 'create').value === gp && (!o || own(c, 'create').value === gc)) return [p, c, gp, gc];
    } catch (e) {}
    try { define(p, 'create', { __proto__: null, ...d }); } catch (e) {}
    if (o) try { define(c, 'create', { __proto__: null, ...o }); } catch (e) {}
    return null;
}"#;

/// The new-document script: the installer with the install's key.
pub(super) fn no_create_js(key: &str) -> String {
    format!("({INSTALLER})({});", Value::String(key.to_string()))
}

/// A fresh key for one install.
pub(super) fn new_key() -> String {
    format!("k{}", uuid::Uuid::new_v4().simple())
}

/// Guards installed through CDP in one document.
struct Installed {
    session: String,
    frame: String,
    /// `CredentialsContainer.prototype` and `navigator.credentials`.
    holders: [String; 2],
    /// The proxies on them (the instance one only when it had an own create).
    guards: [Option<String>; 2],
}

/// Everything one install did, for [`restore`].
pub(super) struct CreateGuard {
    key: String,
    /// (session, new-document script identifier).
    scripts: Vec<(String, String)>,
    installed: Vec<Installed>,
    /// Releases [`GROUP`] in each session holding handles, also when the
    /// login is cancelled.
    holds: Vec<GroupRelease>,
}

impl CreateGuard {
    pub(super) fn new() -> Self {
        Self {
            key: new_key(),
            scripts: Vec::new(),
            installed: Vec::new(),
            holds: Vec::new(),
        }
    }

    fn hold(&mut self, client: &Arc<CdpClient>, session: &str) {
        if !self.holds.iter().any(|h| h.session() == session) {
            self.holds
                .push(GroupRelease::new(client.clone(), session, GROUP));
        }
    }
}

/// An error that only says the frame, document or session is gone.
pub(super) fn gone(e: &str) -> bool {
    [
        "Cannot find context with specified id",
        "Execution context was destroyed",
        "Inspected target navigated or closed",
        "Could not find object with given id",
        "No session with given id",
        "No frame with given id",
        "Frame with the given id was not found",
        "No node with given id",
        "Node with given id does not belong to the document",
        "Session with given id not found",
    ]
    .iter()
    .any(|m| e.contains(m))
}

fn threw(v: &Value) -> Option<String> {
    v.get("exceptionDetails")
        .map(|e| format!("page script threw: {}", e["text"].as_str().unwrap_or("?")))
}

fn object_id(v: &Value) -> Option<String> {
    v["result"]["objectId"].as_str().map(str::to_string)
}

/// The page session first, then each attached out-of-process frame's.
fn sessions(page: &str, iframes: &HashMap<String, String>, extra: &[&str]) -> Vec<String> {
    let mut out = vec![page.to_string()];
    let mut rest: Vec<&String> = iframes.values().collect();
    rest.sort();
    for s in rest
        .into_iter()
        .map(String::as_str)
        .chain(extra.iter().copied())
    {
        if !out.iter().any(|o| o == s) {
            out.push(s.to_string());
        }
    }
    out
}

/// The frames whose documents live in `session`: its root, then the
/// same-process children (out-of-process ones are their own sessions), each
/// with whether Chrome counts it a secure context (the only kind that has
/// `navigator.credentials`).
async fn frames(client: &CdpClient, session: &str) -> Result<Vec<(String, bool)>, String> {
    fn walk(node: &Value, out: &mut Vec<(String, bool)>) {
        if let Some(id) = node["frame"]["id"].as_str() {
            let secure = node["frame"]["secureContextType"]
                .as_str()
                .is_none_or(|t| t.starts_with("Secure"));
            out.push((id.to_string(), secure));
        }
        for child in node["childFrames"].as_array().into_iter().flatten() {
            walk(child, out);
        }
    }
    let tree = client
        .send_command_no_params("Page.getFrameTree", Some(session))
        .await?;
    let mut all = Vec::new();
    walk(&tree["frameTree"], &mut all);
    Ok(all)
}

/// Run `function` in the main world of `frame` (`root`: the session's own
/// document; otherwise through the frame's document node). Ok(None) when the
/// frame has no document in this session.
async fn call_in_frame(
    client: &CdpClient,
    session: &str,
    frame: &str,
    root: bool,
    function: &str,
    args: Value,
) -> Result<Option<Value>, String> {
    let r = if root {
        client
            .send_command(
                "Runtime.evaluate",
                Some(json!({
                    "expression": format!(
                        "({function})({})",
                        args.as_array()
                            .map(|a| a.iter().map(Value::to_string).collect::<Vec<_>>().join(", "))
                            .unwrap_or_default()
                    ),
                    "objectGroup": GROUP, "__cuMain": true,
                })),
                Some(session),
            )
            .await?
    } else {
        let owner = client
            .send_command(
                "DOM.getFrameOwner",
                Some(json!({ "frameId": frame })),
                Some(session),
            )
            .await?;
        let Some(owner) = owner["backendNodeId"].as_i64() else {
            return Ok(None);
        };
        let node = client
            .send_command(
                "DOM.describeNode",
                Some(json!({ "backendNodeId": owner, "depth": 1 })),
                Some(session),
            )
            .await?;
        let Some(doc) = node
            .pointer("/node/contentDocument/backendNodeId")
            .and_then(Value::as_i64)
        else {
            return Ok(None);
        };
        // No execution context named: the document's main world.
        let doc = client
            .send_command(
                "DOM.resolveNode",
                Some(json!({ "backendNodeId": doc, "objectGroup": GROUP, "__cuMain": true })),
                Some(session),
            )
            .await?;
        let Some(doc) = doc.pointer("/object/objectId").and_then(Value::as_str) else {
            return Ok(None);
        };
        let args: Vec<Value> = args
            .as_array()
            .map(|a| a.iter().map(|v| json!({ "value": v })).collect())
            .unwrap_or_default();
        client
            .send_command(
                "Runtime.callFunctionOn",
                Some(json!({
                    "objectId": doc,
                    "functionDeclaration": function,
                    "arguments": args,
                    "objectGroup": GROUP, "__cuMain": true,
                })),
                Some(session),
            )
            .await?
    };
    if let Some(e) = threw(&r) {
        return Err(e);
    }
    Ok(Some(r))
}

/// The elements of a remote array, by index (`None` for null ones).
async fn elements(
    client: &CdpClient,
    session: &str,
    array: &str,
    n: usize,
) -> Result<Vec<Option<String>>, String> {
    let props = client
        .send_command(
            "Runtime.getProperties",
            Some(json!({ "objectId": array, "ownProperties": true })),
            Some(session),
        )
        .await?;
    Ok((0..n)
        .map(|i| {
            props["result"].as_array().and_then(|a| {
                a.iter()
                    .find(|p| p["name"].as_str().and_then(|n| n.parse::<usize>().ok()) == Some(i))
                    .and_then(|p| p["value"]["objectId"].as_str().map(str::to_string))
            })
        })
        .collect())
}

/// Guard every document of the tab now and every new one from here on.
/// On Err the caller still [`restore`]s, which undoes what was done.
pub(super) async fn install(
    client: &Arc<CdpClient>,
    page_session: &str,
    iframe_sessions: &HashMap<String, String>,
    guard: &mut CreateGuard,
) -> Result<(), String> {
    let source = no_create_js(&guard.key);
    for session in sessions(page_session, iframe_sessions, &[]) {
        let page = session == page_session;
        if !page {
            // Without the Page domain on, a frame target never runs its
            // new-document scripts (the page session has it on already), so
            // a frame where it fails is not guarded.
            match client
                .send_command_no_params("Page.enable", Some(&session))
                .await
            {
                Ok(_) => {}
                Err(e) if gone(&e) => continue,
                Err(e) => return Err(format!("frame session {session}: Page.enable: {e}")),
            }
        }
        match client
            .send_command(
                "Page.addScriptToEvaluateOnNewDocument",
                Some(json!({ "source": source })),
                Some(&session),
            )
            .await
        {
            Ok(v) => match v["identifier"].as_str() {
                Some(id) => guard.scripts.push((session.clone(), id.to_string())),
                None => return Err("no script identifier".to_string()),
            },
            // An out-of-process frame that went away needs no guard.
            Err(e) if !page && gone(&e) => continue,
            Err(e) => return Err(e),
        }
        guard.hold(client, &session);
        let list = match frames(client, &session).await {
            Ok(l) => l,
            Err(e) if !page && gone(&e) => continue,
            Err(e) => return Err(e),
        };
        for (i, (frame, _)) in list.iter().enumerate() {
            let root = i == 0;
            let r =
                match call_in_frame(client, &session, frame, root, INSTALLER, json!([guard.key]))
                    .await
                {
                    Ok(Some(r)) => r,
                    // Out of process (its own session), or gone.
                    Ok(None) => continue,
                    Err(e) if gone(&e) && !(page && root) => continue,
                    Err(e) => return Err(format!("frame {frame}: {e}")),
                };
            let Some(array) = object_id(&r) else {
                return Err(format!("frame {frame}: create() could not be guarded"));
            };
            let e = elements(client, &session, &array, 4).await?;
            let (Some(p), Some(c), Some(gp)) = (e[0].clone(), e[1].clone(), e[2].clone()) else {
                return Err(format!("frame {frame}: create() could not be guarded"));
            };
            guard.installed.push(Installed {
                session: session.clone(),
                frame: frame.clone(),
                holders: [p, c],
                guards: [Some(gp), e[3].clone()],
            });
        }
    }
    Ok(())
}

/// What restoring one document found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Outcome {
    /// A guard of ours was taken off.
    Restored,
    /// Nothing of ours was there.
    Clean,
    /// The document went away, and its guard with it.
    Gone,
    /// A guard of ours is still in effect.
    Failed(String),
}

/// One line per document restore looked at: (frame id, outcome).
pub(super) type RestoreReport = Vec<(String, Outcome)>;

/// Ok when no guard of ours is left anywhere; Err names each frame where one
/// is.
pub(super) fn report_result(report: &RestoreReport) -> Result<(), String> {
    let failed: Vec<String> = report
        .iter()
        .filter_map(|(f, r)| match r {
            Outcome::Failed(e) => Some(format!("frame {f}: {e}")),
            _ => None,
        })
        .collect();
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed.join("; "))
    }
}

/// Undo [`install`] through CDP only: remove the new-document scripts, then
/// take off every guard of ours in the documents we guarded and in every
/// document there is now (the script guarded the ones created since), putting
/// back each proxy's `[[Target]]`. [`GROUP`] is released in every session
/// touched, also if this is cancelled.
pub(super) async fn restore(
    client: &Arc<CdpClient>,
    page_session: &str,
    iframe_sessions: &HashMap<String, String>,
    guard: &mut CreateGuard,
) -> RestoreReport {
    let mut report = RestoreReport::new();
    let script_sessions: Vec<String> = guard.scripts.iter().map(|(s, _)| s.clone()).collect();
    let all = sessions(
        page_session,
        iframe_sessions,
        &script_sessions
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    for s in &all {
        guard.hold(client, s);
    }
    for (session, id) in std::mem::take(&mut guard.scripts) {
        if let Err(e) = client
            .send_command(
                "Page.removeScriptToEvaluateOnNewDocument",
                Some(json!({ "identifier": id })),
                Some(&session),
            )
            .await
        {
            if !gone(&e) {
                report.push((
                    session.clone(),
                    Outcome::Failed(format!("removing the new-document guard script: {e}")),
                ));
            }
        }
    }
    // The documents guarded through CDP, by the handles kept.
    for i in &guard.installed {
        let mut out = Outcome::Clean;
        for (holder, mine) in i.holders.iter().zip(&i.guards) {
            let Some(mine) = mine else { continue };
            match restore_slot(client, &i.session, holder, &[mine.as_str()], &guard.key).await {
                Ok(true) => out = Outcome::Restored,
                Ok(false) => {}
                Err(e) if gone(&e) => {
                    out = Outcome::Gone;
                    break;
                }
                Err(e) => {
                    out = Outcome::Failed(e);
                    break;
                }
            }
        }
        report.push((i.frame.clone(), out));
    }
    // Every document there is now, for guards the script made.
    for session in &all {
        let list = match frames(client, session).await {
            Ok(l) => l,
            Err(e) if gone(&e) => continue,
            Err(e) => {
                report.push((session.clone(), Outcome::Failed(e)));
                continue;
            }
        };
        for (n, (frame, secure)) in list.iter().enumerate() {
            let out = sweep_frame(client, session, frame, n == 0, *secure, &guard.key).await;
            match report.iter_mut().find(|(f, _)| f == frame) {
                Some(seen) => seen.1 = merge(std::mem::replace(&mut seen.1, Outcome::Clean), out),
                None => report.push((frame.clone(), out)),
            }
        }
    }
    for h in std::mem::take(&mut guard.holds) {
        h.release().await;
    }
    guard.installed.clear();
    report
}

/// Two looks at one frame (its guarded document by handle, then whatever
/// document it has now): a guard left anywhere wins, then one taken off.
fn merge(a: Outcome, b: Outcome) -> Outcome {
    match (a, b) {
        (f @ Outcome::Failed(_), _) | (_, f @ Outcome::Failed(_)) => f,
        (Outcome::Restored, _) | (_, Outcome::Restored) => Outcome::Restored,
        (a, _) => a,
    }
}

async fn sweep_frame(
    client: &CdpClient,
    session: &str,
    frame: &str,
    root: bool,
    secure: bool,
    key: &str,
) -> Outcome {
    // Nothing to guard where there is no `navigator.credentials`.
    if !secure {
        return Outcome::Clean;
    }
    let roots = match holders(client, session, frame, root).await {
        Ok(Some(r)) => r,
        Ok(None) => return Outcome::Clean,
        Err(e) if gone(&e) => return Outcome::Gone,
        Err(e) => return Outcome::Failed(e),
    };
    match sweep_chains(client, session, &roots, key).await {
        Ok(true) => Outcome::Restored,
        Ok(false) => Outcome::Clean,
        Err(e) if gone(&e) => Outcome::Gone,
        Err(e) => Outcome::Failed(e),
    }
}

/// Take our guards off every object on the `[[Prototype]]` chains of
/// `roots` (`navigator.credentials` and `CredentialsContainer.prototype`),
/// then check that `create`, looked up from each root the way the page
/// would, is no guard of ours. Ok(true) when one was taken off.
///
/// Round 9: the page can move the guard deeper, where none of the objects
/// we know has an own `create` (a new object between the prototype and
/// `Object.prototype` holding it, `delete` on the prototype). The whole chain
/// is walked instead. A chain the page reshaped is not a guard left: it
/// fails only when it cannot be walked (a proxy in it, more than [`CHAIN`]
/// objects, an accessor `create`) or `create` still resolves to ours.
async fn sweep_chains(
    client: &CdpClient,
    session: &str,
    roots: &[String],
    key: &str,
) -> Result<bool, String> {
    let mut restored = false;
    for r in roots {
        for holder in chain(client, session, r).await? {
            restored |= restore_slot(client, session, &holder, &[], key).await?;
        }
    }
    for r in roots {
        if let Some(top) = resolve_create(client, session, r).await? {
            if find_ours(client, session, &top, &[], key).await?.is_some() {
                return Err(
                    "create() still resolves to a guard of ours; reload the page to \
                            get the original back"
                        .to_string(),
                );
            }
        }
    }
    Ok(restored)
}

/// How many objects of a `[[Prototype]]` chain are walked. A longer chain
/// may hide a guard: reported, never taken for clean.
const CHAIN: usize = 64;

/// `start` and every object on its `[[Prototype]]` chain, through CDP (no
/// `getPrototypeOf` trap or builtin of the page's runs). Err when the chain
/// has a proxy, whose traps decide what `create` is, or is longer than
/// [`CHAIN`].
async fn chain(client: &CdpClient, session: &str, start: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut next = Some(start.to_string());
    while let Some(object) = next {
        if out.len() == CHAIN {
            return Err(format!(
                "the credentials prototype chain is longer than {CHAIN} objects, so a \
                 create() guard there cannot be looked for; reload the page to get the \
                 original back"
            ));
        }
        let props = own_props(client, session, &object, false).await?;
        if internal(&props, "[[Target]]").is_some() {
            return Err(
                "the page put a proxy in the credentials prototype chain, so a create() \
                 guard there cannot be looked for; reload the page to get the original back"
                    .to_string(),
            );
        }
        next = internal(&props, "[[Prototype]]");
        out.push(object);
    }
    Ok(out)
}

/// What `create` on `start` resolves to, found the way a property lookup
/// goes but through CDP: the first own `create` along the chain. Ok(None)
/// when there is none or it is no object. Err as for [`chain`] and
/// [`own_create`].
async fn resolve_create(
    client: &CdpClient,
    session: &str,
    start: &str,
) -> Result<Option<String>, String> {
    for holder in chain(client, session, start).await? {
        let props = own_props(client, session, &holder, false).await?;
        if prop(&props, "create").is_some() {
            return own_create(client, session, &holder).await;
        }
    }
    Ok(None)
}

/// Own properties of `object` through CDP (no getter of the page's runs).
async fn own_props(
    client: &CdpClient,
    session: &str,
    object: &str,
    accessors_only: bool,
) -> Result<Value, String> {
    client
        .send_command(
            "Runtime.getProperties",
            Some(json!({
                "objectId": object,
                "ownProperties": true,
                "accessorPropertiesOnly": accessors_only,
            })),
            Some(session),
        )
        .await
}

fn prop<'a>(props: &'a Value, name: &str) -> Option<&'a Value> {
    props["result"]
        .as_array()
        .and_then(|a| a.iter().find(|p| p["name"] == name))
}

fn internal(props: &Value, name: &str) -> Option<String> {
    props["internalProperties"]
        .as_array()
        .and_then(|a| a.iter().find(|p| p["name"] == name))
        .and_then(|p| p["value"]["objectId"].as_str().map(str::to_string))
}

/// The objects of a secure document a guard can be on, found without
/// calling anything the page can replace (round 8: a page that swapped
/// `Object.getPrototypeOf` after the install hid them): the global object
/// from a function of our own, `navigator` and `CredentialsContainer` as
/// Chrome's own values on it, `navigator.credentials` through Chrome's own
/// getter. Returns the instance and `CredentialsContainer.prototype` (by
/// the interface, in case the page took it off the instance's chain), the
/// roots of the chains [`sweep_chains`] walks; Ok(None) when the frame has
/// no document here. Err when the page replaced one of
/// them, since then a guard may be where we cannot look.
async fn holders(
    client: &CdpClient,
    session: &str,
    frame: &str,
    root: bool,
) -> Result<Option<[String; 2]>, String> {
    const GLOBAL: &str = "function () { return (function () { return this; })(); }";
    let replaced = |what: &str| {
        format!(
            "the page replaced {what}, so a create() guard there cannot be looked for; \
             reload the page to get the original back"
        )
    };
    let Some(r) = call_in_frame(client, session, frame, root, GLOBAL, json!([])).await? else {
        return Ok(None);
    };
    let Some(global) = object_id(&r) else {
        return Ok(None);
    };
    let globals = own_props(client, session, &global, false).await?;
    // A value means Chrome's own (a getter of the page's has no value, and
    // a data property of the page's is no platform object or a proxy).
    let native = |p: Option<&Value>, class: &str, description: Option<&str>| {
        p.filter(|p| {
            p["value"]["subtype"] != "proxy"
                && p["value"]["className"] == class
                && description.is_none_or(|d| p["value"]["description"] == d)
        })
        .and_then(|p| p["value"]["objectId"].as_str().map(str::to_string))
    };
    let navigator = match native(prop(&globals, "navigator"), "Navigator", None) {
        Some(n) => n,
        // A Chrome that lists `navigator` by its getter instead: Chrome's own
        // getter is called, never one of the page's.
        None if prop(&globals, "navigator").is_some_and(|p| {
            p["get"]["subtype"] != "proxy"
                && p["get"]["description"] == "function get navigator() { [native code] }"
        }) =>
        {
            let r = client
                .send_command(
                    "Runtime.callFunctionOn",
                    Some(json!({
                        "objectId": global,
                        "functionDeclaration": "function () { return this.navigator; }",
                        "objectGroup": GROUP, "__cuMain": true,
                    })),
                    Some(session),
                )
                .await?;
            if let Some(e) = threw(&r) {
                return Err(format!("{}: {e}", replaced("navigator")));
            }
            match object_id(&r) {
                Some(n)
                    if r["result"]["subtype"] != "proxy"
                        && r["result"]["className"] == "Navigator" =>
                {
                    n
                }
                _ => return Err(replaced("navigator")),
            }
        }
        None => return Err(replaced("navigator")),
    };
    let Some(interface) = native(
        prop(&globals, "CredentialsContainer"),
        "Function",
        Some("function CredentialsContainer() { [native code] }"),
    ) else {
        return Err(replaced("CredentialsContainer"));
    };
    // `prototype` of an interface can be neither written nor redefined.
    let Some(prototype) = prop(
        &own_props(client, session, &interface, false).await?,
        "prototype",
    )
    .and_then(|p| p["value"]["objectId"].as_str().map(str::to_string)) else {
        return Err(replaced("CredentialsContainer"));
    };
    let nav = own_props(client, session, &navigator, false).await?;
    if prop(&nav, "credentials").is_some() {
        return Err(replaced("navigator.credentials"));
    }
    let Some(nav_proto) = internal(&nav, "[[Prototype]]") else {
        return Err(replaced("navigator"));
    };
    let getter = own_props(client, session, &nav_proto, true).await?;
    let native_getter = prop(&getter, "credentials").is_some_and(|p| {
        p["get"]["subtype"] != "proxy"
            && p["get"]["description"] == "function get credentials() { [native code] }"
    });
    if !native_getter {
        return Err(replaced("navigator.credentials"));
    }
    // Only Chrome's getter runs here: there is no own `credentials`.
    let r = client
        .send_command(
            "Runtime.callFunctionOn",
            Some(json!({
                "objectId": navigator,
                "functionDeclaration": "function () { return this.credentials; }",
                "objectGroup": GROUP, "__cuMain": true,
            })),
            Some(session),
        )
        .await?;
    if let Some(e) = threw(&r) {
        return Err(format!("{}: {e}", replaced("navigator")));
    }
    let Some(instance) = object_id(&r) else {
        return Err(replaced("navigator.credentials"));
    };
    Ok(Some([instance, prototype]))
}

/// The current own data value of `create` on `holder`, read through CDP
/// (no getter, no page code). Err when the page made it an accessor, which
/// could be handing out a guard of ours where we cannot look.
async fn own_create(
    client: &CdpClient,
    session: &str,
    holder: &str,
) -> Result<Option<String>, String> {
    let props = own_props(client, session, holder, false).await?;
    let Some(create) = prop(&props, "create") else {
        return Ok(None);
    };
    if create.get("get").is_some() || create.get("set").is_some() {
        return Err(
            "the page replaced create with an accessor, so a create() guard there cannot be \
             looked for; reload the page to get the original back"
                .to_string(),
        );
    }
    Ok(create["value"]["objectId"].as_str().map(str::to_string))
}

/// `[[Target]]` and `[[Handler]]` of `object` when it is a proxy.
async fn proxy_parts(
    client: &CdpClient,
    session: &str,
    object: &str,
) -> Result<Option<(String, String)>, String> {
    let props = own_props(client, session, object, false).await?;
    Ok(internal(&props, "[[Target]]").zip(internal(&props, "[[Handler]]")))
}

/// Whether two handles name the same object (`===`, no page code).
async fn same(client: &CdpClient, session: &str, a: &str, b: &str) -> Result<bool, String> {
    let r = client
        .send_command(
            "Runtime.callFunctionOn",
            Some(json!({
                "objectId": a,
                "functionDeclaration": "function (v) { return this === v; }",
                "arguments": [{ "objectId": b }],
                "returnByValue": true,
            })),
            Some(session),
        )
        .await?;
    Ok(r["result"]["value"] == json!(true))
}

/// Whether the proxy with `handler` is ours: one of `mine` (same document),
/// or its handler carries this install's key.
async fn ours(
    client: &CdpClient,
    session: &str,
    object: &str,
    handler: &str,
    mine: &[&str],
    key: &str,
) -> Result<bool, String> {
    for m in mine {
        if same(client, session, m, object).await? {
            return Ok(true);
        }
    }
    let props = client
        .send_command(
            "Runtime.getProperties",
            Some(json!({ "objectId": handler, "ownProperties": true })),
            Some(session),
        )
        .await?;
    Ok(props["result"]
        .as_array()
        .is_some_and(|a| a.iter().any(|p| p["name"] == key)))
}

/// How many proxies deep `create` is looked through. Deeper than this, the
/// guard may be under them: reported, never taken for clean.
const DEPTH: usize = 32;

/// Take our guards off `holder.create`. Ok(true) when one was taken off,
/// Ok(false) when none of ours is there; Err when one still is (under a
/// wrapper the page put on top, or the page kept it), or may be (under more
/// proxies than [`DEPTH`]).
async fn restore_slot(
    client: &CdpClient,
    session: &str,
    holder: &str,
    mine: &[&str],
    key: &str,
) -> Result<bool, String> {
    let Some(top) = own_create(client, session, holder).await? else {
        return Ok(false);
    };
    let Some(original) = find_ours(client, session, &top, mine, key).await? else {
        return Ok(false);
    };
    let put = client
        .send_command(
            "Runtime.callFunctionOn",
            Some(json!({
                "objectId": holder,
                // A plain assignment first: it needs no builtin the page may
                // have replaced since.
                "functionDeclaration": "function (original) { try { this.create = original; } catch (e) {} if (this.create === original) return; try { const d = Object.getOwnPropertyDescriptor(this, 'create'); Object.defineProperty(this, 'create', { __proto__: null, ...d, value: original }); } catch (e) {} }",
                "arguments": [{ "objectId": original }],
            })),
            Some(session),
        )
        .await?;
    if let Some(e) = threw(&put) {
        return Err(e);
    }
    match own_create(client, session, holder).await? {
        Some(now) if same(client, session, &original, &now).await? => Ok(true),
        _ => Err("the page kept the create() guard".to_string()),
    }
}

/// The `[[Target]]` of the guard of ours on top of `top`, looked through
/// every proxy (a navigation between registering the new-document script
/// and the install on the current page wraps twice). Ok(None) when none of
/// ours is there; Err when one is under a proxy of the page's, so still in
/// effect and not ours to unwrap, or may be (under more than [`DEPTH`]).
async fn find_ours(
    client: &CdpClient,
    session: &str,
    top: &str,
    mine: &[&str],
    key: &str,
) -> Result<Option<String>, String> {
    let mut current = top.to_string();
    let mut original = None;
    let mut wrapped = false;
    let mut depth = 0;
    while let Some((target, handler)) = proxy_parts(client, session, &current).await? {
        if depth == DEPTH {
            return Err(format!(
                "create() is under more than {DEPTH} proxies, so a guard there cannot be \
                 looked for; reload the page to get the original back"
            ));
        }
        depth += 1;
        if !ours(client, session, &current, &handler, mine, key).await? {
            wrapped = true;
        } else if wrapped {
            return Err(
                "the page wrapped the create() guard in its own function; reload the page \
                 to get the original back"
                    .to_string(),
            );
        } else {
            original = Some(target.clone());
        }
        current = target;
    }
    Ok(original)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the real guard against a stub `navigator.credentials` in node (V8,
    /// like Chrome); skipped where node is missing. `setup` runs before the
    /// guard (the page's own state), `probe` after it and prints JSON.
    fn run_node(setup: &str, key: &str, probe: &str) -> Option<Value> {
        let dir = tempfile::tempdir().ok()?;
        let script = dir.path().join("guard.js");
        std::fs::write(
            &script,
            format!(
                r#"'use strict';
let calls = 0;
// A method, like the native: no [[Construct]], so `new` on the guard throws.
const original = {{ create() {{ calls++; return 'ORIGINAL'; }} }}.create;
class CredentialsContainer {{}}
Object.defineProperty(CredentialsContainer.prototype, 'create', {{
    value: original, writable: true, enumerable: true, configurable: true,
}});
Object.defineProperty(globalThis, 'navigator', {{ value: {{ credentials: new CredentialsContainer() }}, configurable: true }});
{setup}
const installed = {guard};
{probe}
"#,
                guard = no_create_js(key),
            ),
        )
        .ok()?;
        let Ok(out) = std::process::Command::new("node").arg(&script).output() else {
            eprintln!("node not found; skipping");
            return None;
        };
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Some(serde_json::from_slice(&out.stdout).unwrap())
    }

    /// Round 6: the page has no way back to `create`: not through a token
    /// in the source, odd arguments, patched call/apply/Reflect.apply, nor
    /// (round 7) a trap it puts on `Object.prototype`, which a handler with a
    /// prototype would look up and call with the handler and the original.
    #[test]
    fn the_page_has_no_way_back_to_create() {
        let probe = r#"
const c = navigator.credentials, p = CredentialsContainer.prototype;
const src = Function.prototype.toString.call(c.create);
const leaks = [];
const refusals = [];
const traps = ['get', 'set', 'has', 'ownKeys', 'getOwnPropertyDescriptor', 'defineProperty',
    'deleteProperty', 'getPrototypeOf', 'setPrototypeOf', 'isExtensible', 'preventExtensions', 'construct'];
for (const t of traps) {
    Object.defineProperty(Object.prototype, t, { __proto__: null, configurable: true, value: function (...a) {
        if (a.includes(original) || this === undefined || Object.getPrototypeOf(this) === null) leaks.push('trap ' + t);
        return Reflect[t](...a);
    } });
}
const g = c.create;
try { g.foo; g.bar = 1; 'x' in g; Object.keys(g); Object.getOwnPropertyDescriptor(g, 'name');
      Object.defineProperty(g, 'q', { __proto__: null, value: 1, configurable: true }); delete g.q;
      Object.getPrototypeOf(g); Object.isExtensible(g); } catch (e) {}
try { new g(); } catch (e) {}
for (const t of traps) delete Object.prototype[t];
const tokens = src.match(/[0-9a-f]{16,}/g) || [];
const saved = [Function.prototype.call, Function.prototype.apply, Reflect.apply];
Function.prototype.call = function () { if (this === original) leaks.push('call'); };
Function.prototype.apply = function () { if (this === original) leaks.push('apply'); };
Reflect.apply = function (f) { if (f === original) leaks.push('Reflect.apply'); };
const args = [undefined, null, {}, src, original, { publicKey: {} }, ...tokens];
const pending = [];
for (const a of args) {
    for (const call of [() => c.create(a), () => p.create(a), () => new Function('c', 'a', 'return c.create(a)')(c, a)]) {
        let r;
        try { r = call(); } catch (e) { refusals.push('threw ' + e.name); continue; }
        if (r === original || typeof r === 'function') leaks.push('returned a function');
        pending.push(Promise.resolve(r).then(() => leaks.push('resolved'), (e) => refusals.push(e.name)));
    }
}
Promise.all(pending).then(() => {
    [Function.prototype.call, Function.prototype.apply, Reflect.apply] = saved;
    process.stdout.write(JSON.stringify({
        installed: Array.isArray(installed) && installed[2] === p.create && installed[3] === null,
        src, tokens, leaks, calls,
        refusals: [...new Set(refusals)],
        name: c.create.name, length: c.create.length,
        own: Object.prototype.hasOwnProperty.call(c, 'create'),
        enumerable: Object.getOwnPropertyDescriptor(p, 'create').enumerable,
    }));
});
"#;
        let Some(r) = run_node("", &new_key(), probe) else {
            return;
        };
        assert_eq!(r["installed"], true, "{r}");
        assert_eq!(r["src"], "function () { [native code] }", "{r}");
        assert_eq!(r["tokens"], json!([]), "{r}");
        assert_eq!(r["leaks"], json!([]), "{r}");
        assert_eq!(r["calls"], 0, "{r}");
        assert_eq!(r["refusals"], json!(["NotAllowedError"]), "{r}");
        assert_eq!(r["name"], "create", "{r}");
        assert_eq!(r["length"], 0, "{r}");
        // No own property where the page had none: nothing to fingerprint.
        assert_eq!(r["own"], false, "{r}");
        assert_eq!(r["enumerable"], true, "{r}");
    }

    /// Round 7: an own `create` on the instance shadows the prototype; it is
    /// guarded too, and keeps its own descriptor.
    #[test]
    fn an_own_create_on_the_instance_is_guarded_too() {
        let setup = r#"
let ownCalls = 0;
const ownCreate = function create() { ownCalls++; return 'OWN'; };
Object.defineProperty(navigator.credentials, 'create', { value: ownCreate, writable: true, enumerable: false, configurable: true });
"#;
        let probe = r#"
const c = navigator.credentials;
Promise.allSettled([c.create({}), CredentialsContainer.prototype.create.call(c, {})]).then((rs) => {
    const d = Object.getOwnPropertyDescriptor(c, 'create');
    process.stdout.write(JSON.stringify({
        installed: Array.isArray(installed) && installed[3] === d.value,
        outcomes: rs.map((r) => r.status + ':' + (r.reason ? r.reason.name : r.value)),
        calls, ownCalls, enumerable: d.enumerable,
        src: Function.prototype.toString.call(c.create),
    }));
});
"#;
        let Some(r) = run_node(setup, &new_key(), probe) else {
            return;
        };
        assert_eq!(r["installed"], true, "{r}");
        assert_eq!(
            r["outcomes"],
            json!(["rejected:NotAllowedError", "rejected:NotAllowedError"]),
            "{r}"
        );
        assert_eq!(r["calls"], 0, "{r}");
        assert_eq!(r["ownCalls"], 0, "{r}");
        assert_eq!(r["enumerable"], false, "{r}");
        assert_eq!(r["src"], "function () { [native code] }", "{r}");
    }

    /// Round 7: the builtins are taken before anything else runs, and an own
    /// `create` that is not a function, or a getter, fails closed. Round 8:
    /// with nothing left replaced, also when the instance's `create` cannot
    /// be replaced after the prototype's could, or replacing it throws.
    #[test]
    fn an_unguardable_create_fails_closed() {
        let probe = r#"
const c = navigator.credentials, p = CredentialsContainer.prototype;
const before = calls;
p.create.call(c);
process.stdout.write(JSON.stringify({
    installed,
    prototype: Object.getOwnPropertyDescriptor(p, 'create').value === original,
    instance: ((d) => d.value === was.value && d.get === was.get)(Object.getOwnPropertyDescriptor(c, 'create')),
    reached: calls - before,
}));
"#;
        for setup in [
            "Object.defineProperty(navigator.credentials, 'create', { get() { return original; }, configurable: true });",
            "Object.defineProperty(navigator.credentials, 'create', { value: 1, configurable: true });",
            "Object.defineProperty(navigator.credentials, 'create', { value: ownCreate, writable: false, configurable: false });",
            "Object.defineProperty(navigator.credentials, 'create', { value: ownCreate, writable: true, configurable: true }); \
             Object.defineProperty(globalThis, 'navigator', { value: { credentials: new Proxy(navigator.credentials, { \
                 defineProperty() { throw new Error('no'); } }) }, configurable: true });",
        ] {
            let setup = format!(
                "const ownCreate = function create() {{}};\n{setup}\n\
                 const was = Object.getOwnPropertyDescriptor(navigator.credentials, 'create');"
            );
            let Some(r) = run_node(&setup, &new_key(), probe) else {
                return;
            };
            assert_eq!(
                r,
                json!({ "installed": null, "prototype": true, "instance": true, "reached": 1 }),
                "{setup}"
            );
        }
    }

    /// Round 7: what tells a guard of ours from the page's is the key on its
    /// handler (or our handle), never its source. The key is on our handler
    /// only; a proxy rebuilt from the same trap text has none, and the key
    /// appears nowhere the page can read.
    #[test]
    fn only_our_handler_carries_the_key() {
        let key = new_key();
        let probe = format!(
            r#"
const util = require('util');
const handlerOf = (f) => util.inspect(f, {{ showProxy: true, depth: 4 }});
const ours = handlerOf(navigator.credentials.create);
const forged = new Proxy(function () {{}}, {{ __proto__: null, apply() {{ return reject(new E(msg, 'NotAllowedError')); }} }});
const visible = [Function.prototype.toString.call(navigator.credentials.create),
    Object.getOwnPropertyNames(navigator.credentials.create).join(','),
    JSON.stringify(Object.getOwnPropertyDescriptors(CredentialsContainer.prototype))].join('|');
process.stdout.write(JSON.stringify({{
    ours: ours.includes({key:?}),
    forged: handlerOf(forged).includes({key:?}),
    visible: visible.includes({key:?}),
}}));
"#
        );
        let Some(r) = run_node("", &key, &probe) else {
            return;
        };
        assert_eq!(
            r,
            json!({ "ours": true, "forged": false, "visible": false })
        );
        assert_ne!(new_key(), key);
    }

    type Respond = Arc<dyn Fn(&Value) -> Result<Value, String> + Send + Sync>;

    /// A CDP endpoint answering each command with `respond(request)` (Err:
    /// a CDP error with that message). Returns its URL and the
    /// `session:objectGroup` of every `Runtime.releaseObjectGroup`.
    async fn fake_cdp(respond: Respond) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        use futures_util::{SinkExt, StreamExt};
        let released = Arc::new(std::sync::Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen = released.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let seen = seen.clone();
                let respond = respond.clone();
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    while let Some(Ok(msg)) = ws.next().await {
                        let tokio_tungstenite::tungstenite::Message::Text(text) = msg else {
                            continue;
                        };
                        let req: Value = serde_json::from_str(&text).unwrap();
                        if req["method"] == "Runtime.releaseObjectGroup" {
                            seen.lock().unwrap().push(format!(
                                "{}:{}",
                                req["sessionId"].as_str().unwrap_or(""),
                                req["params"]["objectGroup"].as_str().unwrap_or("")
                            ));
                        }
                        let mut out = match respond(&req) {
                            Ok(result) => json!({ "id": req["id"], "result": result }),
                            Err(message) => json!({
                                "id": req["id"],
                                "error": { "code": -32000, "message": message },
                            }),
                        };
                        if let Some(sid) = req.get("sessionId") {
                            out["sessionId"] = sid.clone();
                        }
                        let reply = tokio_tungstenite::tungstenite::Message::Text(out.to_string());
                        if ws.send(reply).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        (url, released)
    }

    async fn recorder() -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        fake_cdp(Arc::new(|_| Ok(json!({})))).await
    }

    /// A heap of objects for [`fake_cdp`]: `H.create` is `w{layers}`, each
    /// `w{i}` a proxy of the page's around `w{i-1}` (`w1` around `G`), and
    /// `G` our guard (its handler has the key) around the native `N`.
    fn wrapped_guard(layers: usize, key: &str) -> Respond {
        let key = key.to_string();
        Arc::new(move |req| {
            let id = req["params"]["objectId"].as_str().unwrap_or("");
            let proxy = |target: &str, handler: &str| {
                json!({ "result": [], "internalProperties": [
                    { "name": "[[Target]]", "value": { "objectId": target } },
                    { "name": "[[Handler]]", "value": { "objectId": handler } },
                ] })
            };
            Ok(match req["method"].as_str().unwrap_or("") {
                "Runtime.getProperties" => match id {
                    "H" => json!({ "result": [{ "name": "create", "value": {
                        "objectId": if layers == 0 { "G".to_string() } else { format!("w{layers}") },
                    } }] }),
                    "G" => proxy("N", "ours"),
                    "ours" => {
                        json!({ "result": [{ "name": key.as_str(), "value": { "value": 0 } }] })
                    }
                    "w1" => proxy("G", "page"),
                    w if w.starts_with('w') => {
                        let i: usize = w[1..].parse().unwrap();
                        proxy(&format!("w{}", i - 1), "page")
                    }
                    _ => json!({ "result": [] }),
                },
                "Runtime.callFunctionOn" => json!({ "result": { "value":
                    req["params"]["arguments"][0]["objectId"] == req["params"]["objectId"] } }),
                _ => json!({}),
            })
        })
    }

    /// Round 8: our guard under proxies of the page's is reported however
    /// deep it is, never taken for clean (eight layers used to be).
    #[tokio::test]
    async fn a_guard_under_page_proxies_is_never_clean() {
        let key = new_key();
        for layers in [1, 8, 31, 40] {
            let (url, _) = fake_cdp(wrapped_guard(layers, &key)).await;
            let client = CdpClient::connect(&url).await.unwrap();
            let r = restore_slot(&client, "S", "H", &[], &key).await;
            let err = r.expect_err(&format!("{layers} layers"));
            assert!(
                err.contains(if layers < DEPTH {
                    "wrapped the create() guard"
                } else {
                    "more than 32 proxies"
                }),
                "{layers}: {err}"
            );
        }
        // Not ours (another key): nothing of ours there.
        let (url, _) = fake_cdp(wrapped_guard(8, &key)).await;
        let client = CdpClient::connect(&url).await.unwrap();
        assert_eq!(
            restore_slot(&client, "S", "H", &[], &new_key()).await,
            Ok(false)
        );
    }

    /// A secure document for [`fake_cdp`] whose global lists `navigator`
    /// by `getter` (a Chrome that does not show its value), with Chrome's
    /// own `credentials` getter and `CredentialsContainer`.
    fn navigator_by_getter(getter: Value) -> Respond {
        Arc::new(move |req| {
            let id = req["params"]["objectId"].as_str().unwrap_or("");
            let entry = |name: &str, v: Value| {
                let mut e = json!({ "name": name });
                e.as_object_mut()
                    .unwrap()
                    .extend(v.as_object().unwrap().clone());
                e
            };
            let proto = |p: &str| {
                json!({ "result": [], "internalProperties": [
                { "name": "[[Prototype]]", "value": { "objectId": p } },
            ] })
            };
            Ok(match req["method"].as_str().unwrap_or("") {
                "Runtime.evaluate" => json!({ "result": { "objectId": "W" } }),
                "Runtime.getProperties" => match id {
                    "W" => json!({ "result": [
                        entry("navigator", json!({ "get": getter.clone() })),
                        entry("CredentialsContainer", json!({ "value": {
                            "objectId": "IF", "className": "Function",
                            "description": "function CredentialsContainer() { [native code] }",
                        } })),
                    ] }),
                    "IF" => {
                        json!({ "result": [entry("prototype", json!({ "value": { "objectId": "PROTO" } }))] })
                    }
                    "NAV" => proto("NP"),
                    "NP" => json!({ "result": [entry("credentials", json!({ "get": {
                        "description": "function get credentials() { [native code] }",
                    } }))] }),
                    "C" => proto("PROTO"),
                    _ => json!({ "result": [] }),
                },
                "Runtime.callFunctionOn" => match id {
                    "W" => json!({ "result": { "objectId": "NAV", "className": "Navigator" } }),
                    "NAV" => {
                        json!({ "result": { "objectId": "C", "className": "CredentialsContainer" } })
                    }
                    _ => json!({}),
                },
                _ => json!({}),
            })
        })
    }

    /// Round 8: a Chrome that lists `navigator` by its own getter is still
    /// searched (through that getter); a getter of the page's is reported.
    #[tokio::test]
    async fn navigator_listed_by_chromes_getter_is_still_searched() {
        let native = json!({ "type": "function", "description": "function get navigator() { [native code] }" });
        let (url, _) = fake_cdp(navigator_by_getter(native)).await;
        let client = CdpClient::connect(&url).await.unwrap();
        assert_eq!(
            holders(&client, "S", "F", true).await,
            Ok(Some(["C".to_string(), "PROTO".to_string()]))
        );
        for page in [
            json!({ "type": "function", "description": "get() { return {}; }" }),
            json!({ "type": "object", "subtype": "proxy", "description": "function get navigator() { [native code] }" }),
        ] {
            let (url, _) = fake_cdp(navigator_by_getter(page.clone())).await;
            let client = CdpClient::connect(&url).await.unwrap();
            let err = holders(&client, "S", "F", true).await.unwrap_err();
            assert!(err.contains("the page replaced navigator"), "{page}: {err}");
        }
    }

    /// Round 8: a `create` the page made an accessor may hand out a guard of
    /// ours: reported, not clean.
    #[tokio::test]
    async fn an_accessor_create_is_never_clean() {
        let (url, _) = fake_cdp(Arc::new(|req| {
            Ok(match req["params"]["objectId"].as_str() {
                Some("H") => json!({ "result": [{ "name": "create",
                    "get": { "type": "function", "objectId": "g" }, "set": { "type": "undefined" } }] }),
                _ => json!({ "result": [] }),
            })
        }))
        .await;
        let client = CdpClient::connect(&url).await.unwrap();
        let err = restore_slot(&client, "S", "H", &[], &new_key())
            .await
            .unwrap_err();
        assert!(err.contains("accessor"), "{err}");
    }

    /// Round 8: when Chrome will not enable the Page domain on an
    /// out-of-process frame's session, its new-document guard would never
    /// run there: the install fails instead of passing that frame by.
    #[tokio::test]
    async fn an_oopif_without_the_page_domain_fails_the_install() {
        let (url, _) = fake_cdp(Arc::new(|req| {
            match (
                req["method"].as_str().unwrap_or(""),
                req["sessionId"].as_str(),
            ) {
                ("Page.enable", Some("S2")) => Err("Page domain unavailable".to_string()),
                ("Page.addScriptToEvaluateOnNewDocument", _) => Ok(json!({ "identifier": "1" })),
                _ => Ok(json!({})),
            }
        }))
        .await;
        let client = Arc::new(CdpClient::connect(&url).await.unwrap());
        let mut iframes = HashMap::new();
        iframes.insert("F1".to_string(), "S2".to_string());
        let mut guard = CreateGuard::new();
        let err = install(&client, "S1", &iframes, &mut guard)
            .await
            .unwrap_err();
        assert!(err.contains("S2") && err.contains("Page.enable"), "{err}");
        assert_eq!(guard.scripts, vec![("S1".to_string(), "1".to_string())]);
        // A session that is already gone is no failure.
        let (url, _) = fake_cdp(Arc::new(|req| {
            match (
                req["method"].as_str().unwrap_or(""),
                req["sessionId"].as_str(),
            ) {
                ("Page.enable", Some("S2")) => Err("Session with given id not found".to_string()),
                ("Page.addScriptToEvaluateOnNewDocument", _) => Ok(json!({ "identifier": "1" })),
                _ => Ok(json!({})),
            }
        }))
        .await;
        let client = Arc::new(CdpClient::connect(&url).await.unwrap());
        let mut guard = CreateGuard::new();
        assert_eq!(install(&client, "S1", &iframes, &mut guard).await, Ok(()));
    }

    /// Round 7: a restore cancelled part-way (here before its first command
    /// is on the wire) still releases the guard's object group in every
    /// session it touched.
    #[tokio::test]
    async fn restore_releases_the_group_when_cancelled() {
        let (url, released) = recorder().await;
        let client = Arc::new(CdpClient::connect(&url).await.unwrap());
        let mut iframes = HashMap::new();
        iframes.insert("F1".to_string(), "S2".to_string());
        let mut guard = CreateGuard::new();
        guard.scripts.push(("S1".to_string(), "1".to_string()));
        let writer = client.hold_writer().await;
        let restore = restore(&client, "S1", &iframes, &mut guard);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), restore)
                .await
                .is_err()
        );
        drop(guard);
        assert!(released.lock().unwrap().is_empty());
        drop(writer);
        for _ in 0..50 {
            if released.lock().unwrap().len() == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let mut seen = released.lock().unwrap().clone();
        seen.sort();
        assert_eq!(seen, vec![format!("S1:{GROUP}"), format!("S2:{GROUP}")]);
    }

    /// How the page arranged the chain for [`moved_guard`].
    #[derive(Clone, Copy, PartialEq)]
    enum Moved {
        /// `C → PROTO → HIDDEN → OP`, our guard `G` (around `N`) on HIDDEN.
        Deeper,
        /// The same, with `create` an accessor on HIDDEN.
        Accessor,
        /// PROTO's `[[Prototype]]` a proxy around HIDDEN.
        ProxyInChain,
        /// 70 empty objects under C.
        Long,
        /// Deeper, and once HIDDEN is put back the page has put another
        /// guard of ours, `G2`, on C.
        Again,
    }

    /// A heap of objects for [`fake_cdp`] (round 9): the page moved our
    /// guard off `CredentialsContainer.prototype` (PROTO) onto a new object
    /// under it. Putting `N` back on HIDDEN takes.
    fn moved_guard(how: Moved, key: &str) -> (Respond, Arc<std::sync::atomic::AtomicBool>) {
        use std::sync::atomic::{AtomicBool, Ordering};
        let key = key.to_string();
        let put = Arc::new(AtomicBool::new(false));
        let done = put.clone();
        let respond: Respond = Arc::new(move |req| {
            let id = req["params"]["objectId"].as_str().unwrap_or("");
            let internal = |pairs: &[(&str, &str)]| -> Vec<Value> {
                pairs
                    .iter()
                    .map(|(n, o)| json!({ "name": n, "value": { "objectId": o } }))
                    .collect()
            };
            let object = |create: Option<Value>, proto: Option<&str>| {
                json!({
                    "result": create.map(|c| vec![c]).unwrap_or_default(),
                    "internalProperties": internal(&proto.map(|p| vec![("[[Prototype]]", p)]).unwrap_or_default()),
                })
            };
            let data = |o: &str| json!({ "name": "create", "value": { "objectId": o } });
            let proxy = |target: &str, handler: &str| {
                json!({ "result": [], "internalProperties":
                    internal(&[("[[Target]]", target), ("[[Handler]]", handler)]) })
            };
            let put_now = put.load(Ordering::SeqCst);
            Ok(match req["method"].as_str().unwrap_or("") {
                "Runtime.getProperties" => match id {
                    "C" if how == Moved::Long => object(None, Some("L1")),
                    "C" if how == Moved::Again && put_now => {
                        object(Some(data("G2")), Some("PROTO"))
                    }
                    "C" => object(None, Some("PROTO")),
                    o if o.starts_with('L') => {
                        let i: usize = o[1..].parse().unwrap();
                        object(None, (i < 70).then(|| format!("L{}", i + 1)).as_deref())
                    }
                    "PROTO" if how == Moved::ProxyInChain => object(None, Some("PX")),
                    "PROTO" => object(None, Some("HIDDEN")),
                    "PX" => proxy("HIDDEN", "page"),
                    "HIDDEN" if how == Moved::Accessor => object(
                        Some(json!({ "name": "create",
                            "get": { "type": "function", "objectId": "g" }, "set": { "type": "undefined" } })),
                        Some("OP"),
                    ),
                    "HIDDEN" => object(Some(data(if put_now { "N" } else { "G" })), Some("OP")),
                    "OP" => object(None, None),
                    "G" | "G2" => proxy("N", "ours"),
                    "ours" => {
                        json!({ "result": [{ "name": key.as_str(), "value": { "value": 0 } }] })
                    }
                    _ => json!({ "result": [] }),
                },
                "Runtime.callFunctionOn" => {
                    let f = req["params"]["functionDeclaration"].as_str().unwrap_or("");
                    let arg = &req["params"]["arguments"][0]["objectId"];
                    if f.contains("this.create = original") {
                        if id == "HIDDEN" && *arg == "N" {
                            put.store(true, Ordering::SeqCst);
                        }
                        json!({ "result": { "type": "undefined" } })
                    } else {
                        json!({ "result": { "value": *arg == req["params"]["objectId"] } })
                    }
                }
                _ => json!({}),
            })
        });
        (respond, done)
    }

    /// Round 9: a guard the page moved deeper in the prototype chain, where
    /// none of the objects we know has an own `create`, is still found and
    /// taken off; a chain that cannot be walked, or that still resolves to
    /// a guard of ours afterwards, is reported, never clean.
    #[tokio::test]
    async fn a_guard_moved_down_the_prototype_chain_is_found() {
        let key = new_key();
        let roots = ["C".to_string(), "PROTO".to_string()];
        let (respond, put) = moved_guard(Moved::Deeper, &key);
        let (url, _) = fake_cdp(respond).await;
        let client = CdpClient::connect(&url).await.unwrap();
        assert_eq!(sweep_chains(&client, "S", &roots, &key).await, Ok(true));
        assert!(put.load(std::sync::atomic::Ordering::SeqCst));
        // Another install's key: nothing of ours, nothing written.
        let (respond, put) = moved_guard(Moved::Deeper, &key);
        let (url, _) = fake_cdp(respond).await;
        let client = CdpClient::connect(&url).await.unwrap();
        assert_eq!(
            sweep_chains(&client, "S", &roots, &new_key()).await,
            Ok(false)
        );
        assert!(!put.load(std::sync::atomic::Ordering::SeqCst));
        for (how, want) in [
            (Moved::Accessor, "accessor"),
            (
                Moved::ProxyInChain,
                "proxy in the credentials prototype chain",
            ),
            (Moved::Long, "longer than 64 objects"),
            (Moved::Again, "still resolves to a guard of ours"),
        ] {
            let (respond, _) = moved_guard(how, &key);
            let (url, _) = fake_cdp(respond).await;
            let client = CdpClient::connect(&url).await.unwrap();
            let err = sweep_chains(&client, "S", &roots, &key).await.unwrap_err();
            assert!(err.contains(want), "{want}: {err}");
        }
    }

    #[test]
    fn a_failed_frame_fails_the_restore() {
        let ok = vec![
            ("A".to_string(), Outcome::Restored),
            ("B".to_string(), Outcome::Gone),
            ("C".to_string(), Outcome::Clean),
        ];
        assert_eq!(report_result(&ok), Ok(()));
        let mut bad = ok.clone();
        bad.push(("D".to_string(), Outcome::Failed("kept".to_string())));
        assert_eq!(report_result(&bad), Err("frame D: kept".to_string()));
    }
}
