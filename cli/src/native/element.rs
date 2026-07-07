use std::collections::HashMap;

use serde_json::Value;

use super::adaptive::{self, ElementFingerprint};
use super::cdp::client::CdpClient;
use super::cdp::types::*;

#[derive(Debug, Clone)]
pub struct RefEntry {
    pub backend_node_id: Option<i64>,
    pub role: String,
    pub name: String,
    pub nth: Option<usize>,
    pub selector: Option<String>,
    pub frame_id: Option<String>,
    /// AX fingerprint captured at snapshot time, used by adaptive relocation when
    /// the node is gone and the role/name/nth re-query also fails.
    pub fingerprint: Option<ElementFingerprint>,
}

pub struct RefMap {
    map: HashMap<String, RefEntry>,
    next_ref: usize,
}

impl RefMap {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            next_ref: 1,
        }
    }

    pub fn add(
        &mut self,
        ref_id: String,
        backend_node_id: Option<i64>,
        role: &str,
        name: &str,
        nth: Option<usize>,
    ) {
        self.add_with_frame(ref_id, backend_node_id, role, name, nth, None);
    }

    pub fn add_with_frame(
        &mut self,
        ref_id: String,
        backend_node_id: Option<i64>,
        role: &str,
        name: &str,
        nth: Option<usize>,
        frame_id: Option<&str>,
    ) {
        self.map.insert(
            ref_id,
            RefEntry {
                backend_node_id,
                role: role.to_string(),
                name: name.to_string(),
                nth,
                selector: None,
                frame_id: frame_id.map(|s| s.to_string()),
                fingerprint: None,
            },
        );
    }

    /// Attach an AX fingerprint to an existing ref (set during snapshot, used by
    /// adaptive relocation). No-op if the ref is unknown.
    pub fn set_fingerprint(&mut self, ref_id: &str, fingerprint: ElementFingerprint) {
        if let Some(entry) = self.map.get_mut(ref_id) {
            entry.fingerprint = Some(fingerprint);
        }
    }

    pub fn add_selector(
        &mut self,
        ref_id: String,
        selector: String,
        role: &str,
        name: &str,
        nth: Option<usize>,
    ) {
        self.map.insert(
            ref_id,
            RefEntry {
                backend_node_id: None,
                role: role.to_string(),
                name: name.to_string(),
                nth,
                selector: Some(selector),
                frame_id: None,
                fingerprint: None,
            },
        );
    }

    pub fn get(&self, ref_id: &str) -> Option<&RefEntry> {
        self.map.get(ref_id)
    }

    /// Whether `selector_or_ref` is a `@ref` whose snapshot entry lives inside an
    /// iframe (has a `frame_id`). Pointer interactions use this to choose
    /// DOM-dispatch over coordinates for OOPIF elements (issue #36).
    pub fn ref_is_in_iframe(&self, selector_or_ref: &str) -> bool {
        parse_ref(selector_or_ref)
            .and_then(|r| self.map.get(&r).map(|e| e.frame_id.is_some()))
            .unwrap_or(false)
    }

    pub fn entries_sorted(&self) -> Vec<(String, RefEntry)> {
        let mut entries = self
            .map
            .iter()
            .map(|(ref_id, entry)| (ref_id.clone(), entry.clone()))
            .collect::<Vec<_>>();

        entries.sort_by_key(|(ref_id, _)| {
            ref_id
                .strip_prefix('e')
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(usize::MAX)
        });

        entries
    }

    pub fn remove(&mut self, ref_id: &str) {
        self.map.remove(ref_id);
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.next_ref = 1;
    }

    pub fn next_ref_num(&self) -> usize {
        self.next_ref
    }

    pub fn set_next_ref_num(&mut self, n: usize) {
        self.next_ref = n;
    }
}

pub fn parse_ref(input: &str) -> Option<String> {
    let trimmed = input.trim();

    if let Some(stripped) = trimmed.strip_prefix('@') {
        if stripped.starts_with('e') && stripped[1..].chars().all(|c| c.is_ascii_digit()) {
            return Some(stripped.to_string());
        }
    }

    if let Some(stripped) = trimmed.strip_prefix("ref=") {
        if stripped.starts_with('e') && stripped[1..].chars().all(|c| c.is_ascii_digit()) {
            return Some(stripped.to_string());
        }
    }

    if trimmed.starts_with('e')
        && trimmed.len() > 1
        && trimmed[1..].chars().all(|c| c.is_ascii_digit())
    {
        return Some(trimmed.to_string());
    }

    None
}

/// When a saved `@ref`'s node is gone and the role/name/nth re-query also failed,
/// try to relocate the element by AX fingerprint similarity. Returns the chosen
/// backend node id only when confident (high score + clear margin over the
/// runner-up). Opt out with `AGENT_BROWSER_ADAPTIVE_REF=0`.
async fn relocate_stale_ref(
    client: &CdpClient,
    ref_id: &str,
    entry: &RefEntry,
    session_id: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Option<i64> {
    if std::env::var("AGENT_BROWSER_ADAPTIVE_REF").as_deref() == Ok("0") {
        return None;
    }
    let baseline = entry.fingerprint.as_ref()?;
    let candidates = super::snapshot::collect_current_fingerprints(
        client,
        session_id,
        entry.frame_id.as_deref(),
        iframe_sessions,
    )
    .await
    .ok()?;
    match adaptive::pick_best(
        baseline,
        &candidates,
        adaptive::ADAPTIVE_THRESHOLD,
        adaptive::ADAPTIVE_MARGIN,
    ) {
        Ok(reloc) => {
            eprintln!(
                "[adaptive] relocated {ref_id} ({} \"{}\") score={:.2} second={:.2} -> backendNodeId {}",
                entry.role, entry.name, reloc.score, reloc.second_score, reloc.backend_node_id
            );
            Some(reloc.backend_node_id)
        }
        Err(_) => None,
    }
}

/// Resolve a `@ref` or CSS selector to a click point. Returns
/// `(centre_x, centre_y, width, height, session_id)`. Width/height come from the
/// element's box model and feed humanize's in-bounds landing jitter; the CSS
/// selector path returns zero size (→ land on centre, no jitter).
pub async fn resolve_element_center(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(f64, f64, f64, f64, String), String> {
    if let Some(ref_id) = parse_ref(selector_or_ref) {
        let entry = ref_map
            .get(&ref_id)
            .ok_or_else(|| format!("Unknown ref: {}", ref_id))?;

        let effective_session_id =
            resolve_frame_session(entry.frame_id.as_deref(), session_id, iframe_sessions);

        // Try cached backend_node_id first (fast path)
        if let Some(backend_node_id) = entry.backend_node_id {
            let mut active_id = backend_node_id;
            // Identity check: React often re-uses the same DOM node when
            // re-rendering — backendNodeId stays the same but accessibleName
            // / role changes. Without this verification, `click @e20` (saved
            // when the button said "Add post") happily clicks the *same*
            // node that now says "Post all", silently submitting the thread.
            //
            // On mismatch, try adaptive fingerprint relocation before failing:
            // a confident high-score/high-margin match is a stronger identity
            // signal than role+name, and lets a moved+renamed element still
            // resolve. If relocation isn't confident, surface the original
            // identity error. Set AGENT_BROWSER_VERIFY_REF=0 to skip the check
            // (and thus relocation) entirely.
            if std::env::var("AGENT_BROWSER_VERIFY_REF").as_deref() != Ok("0") {
                if let Err(e) = verify_ref_identity(
                    client,
                    effective_session_id,
                    backend_node_id,
                    &ref_id,
                    &entry.role,
                    &entry.name,
                )
                .await
                {
                    match relocate_stale_ref(client, &ref_id, entry, session_id, iframe_sessions)
                        .await
                    {
                        Some(id) => active_id = id,
                        None => return Err(e),
                    }
                }
            }

            let result: Result<DomGetBoxModelResult, String> = client
                .send_command_typed(
                    "DOM.getBoxModel",
                    &DomGetBoxModelParams {
                        backend_node_id: Some(active_id),
                        node_id: None,
                        object_id: None,
                    },
                    Some(effective_session_id),
                )
                .await;

            if let Ok(r) = result {
                let (x, y, w, h) = box_model_dims(&r.model);
                // Occlusion check: a transient overlay (X.com's "click
                // outside to close" mask, modal backdrop, sticky banner,
                // etc.) can land on top of our target between snapshot
                // and click. Coordinates are correct, but
                // `document.elementFromPoint(x, y)` returns the overlay
                // — and the click goes to the overlay's handler, not
                // ours. Catch it here so the user gets "occluded by
                // DIV[testid=mask]" instead of "modal silently closed +
                // thread submitted by accident".
                //
                // Set AGENT_BROWSER_VERIFY_CLICK_TARGET=0 to skip.
                if std::env::var("AGENT_BROWSER_VERIFY_CLICK_TARGET").as_deref() != Ok("0") {
                    verify_click_target(client, effective_session_id, active_id, &ref_id, x, y)
                        .await?;
                }
                return Ok((x, y, w, h, effective_session_id.to_string()));
            }
            // backend_node_id is stale; re-query the accessibility tree below
        }

        // Fallback: re-query the accessibility tree to find a fresh node by role/name.
        // If that fails, try adaptive fingerprint relocation before giving up.
        let fresh_id = match find_node_id_by_role_name(
            client,
            session_id,
            &entry.role,
            &entry.name,
            entry.nth,
            entry.frame_id.as_deref(),
            iframe_sessions,
        )
        .await
        {
            Ok(id) => id,
            Err(e) => match relocate_stale_ref(client, &ref_id, entry, session_id, iframe_sessions)
                .await
            {
                Some(id) => id,
                None => return Err(e),
            },
        };
        let result: DomGetBoxModelResult = client
            .send_command_typed(
                "DOM.getBoxModel",
                &DomGetBoxModelParams {
                    backend_node_id: Some(fresh_id),
                    node_id: None,
                    object_id: None,
                },
                Some(effective_session_id),
            )
            .await?;
        let (x, y, w, h) = box_model_dims(&result.model);
        return Ok((x, y, w, h, effective_session_id.to_string()));
    }

    // CSS selector
    let (x, y) = resolve_by_selector(client, session_id, selector_or_ref).await?;
    // No box model on the CSS-selector fast path → zero size → land on centre.
    Ok((x, y, 0.0, 0.0, session_id.to_string()))
}

pub async fn resolve_element_object_id(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(String, String), String> {
    if let Some(ref_id) = parse_ref(selector_or_ref) {
        let entry = ref_map
            .get(&ref_id)
            .ok_or_else(|| format!("Unknown ref: {}", ref_id))?;

        let effective_session_id =
            resolve_frame_session(entry.frame_id.as_deref(), session_id, iframe_sessions);

        // Try cached backend_node_id first (fast path)
        if let Some(backend_node_id) = entry.backend_node_id {
            let mut active_id = backend_node_id;
            // Same identity guard as resolve_element_center — see that
            // function for why React DOM-node-reuse breaks ref-based
            // interactions if we skip this, and why a confident adaptive
            // relocation is allowed to override an identity mismatch.
            if std::env::var("AGENT_BROWSER_VERIFY_REF").as_deref() != Ok("0") {
                if let Err(e) = verify_ref_identity(
                    client,
                    effective_session_id,
                    backend_node_id,
                    &ref_id,
                    &entry.role,
                    &entry.name,
                )
                .await
                {
                    match relocate_stale_ref(client, &ref_id, entry, session_id, iframe_sessions)
                        .await
                    {
                        Some(id) => active_id = id,
                        None => return Err(e),
                    }
                }
            }

            let result: Result<DomResolveNodeResult, String> = client
                .send_command_typed(
                    "DOM.resolveNode",
                    &DomResolveNodeParams {
                        backend_node_id: Some(active_id),
                        node_id: None,
                        object_group: Some("chrome-use".to_string()),
                    },
                    Some(effective_session_id),
                )
                .await;

            if let Ok(r) = result {
                if let Some(object_id) = r.object.object_id {
                    return Ok((object_id, effective_session_id.to_string()));
                }
            }
            // backend_node_id is stale; re-query the accessibility tree below
        }

        // Fallback: re-query the accessibility tree to find a fresh node by role/name.
        // If that fails, try adaptive fingerprint relocation before giving up.
        let fresh_id = match find_node_id_by_role_name(
            client,
            session_id,
            &entry.role,
            &entry.name,
            entry.nth,
            entry.frame_id.as_deref(),
            iframe_sessions,
        )
        .await
        {
            Ok(id) => id,
            Err(e) => match relocate_stale_ref(client, &ref_id, entry, session_id, iframe_sessions)
                .await
            {
                Some(id) => id,
                None => return Err(e),
            },
        };
        let result: DomResolveNodeResult = client
            .send_command_typed(
                "DOM.resolveNode",
                &DomResolveNodeParams {
                    backend_node_id: Some(fresh_id),
                    node_id: None,
                    object_group: Some("chrome-use".to_string()),
                },
                Some(effective_session_id),
            )
            .await?;
        let object_id = result
            .object
            .object_id
            .ok_or_else(|| format!("No objectId for ref {}", ref_id))?;
        return Ok((object_id, effective_session_id.to_string()));
    }

    // Selector fallback (CSS or XPath)
    let js = build_find_element_js(selector_or_ref);
    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: js,
                return_by_value: Some(false),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;

    // A syntactically-invalid selector makes `document.querySelector` THROW.
    // With returnByValue:false, Runtime.evaluate then returns the thrown
    // DOMException as a remote object *with* an objectId — which would otherwise
    // be mistaken for "the element" and silently no-op a `.click()` on it. Treat
    // any thrown exception as a hard error so a typo'd selector fails loudly.
    if let Some(ex) = result.exception_details {
        return Err(format!(
            "Invalid selector '{}': {}",
            selector_or_ref, ex.text
        ));
    }

    let Some(object_id) = result.result.object_id else {
        // Element not found. If the selector carries a quoted text — most often a
        // `[placeholder="…"]` — Element-Plus-style widgets render that text on an
        // inner <span>, not the real <input>, so the CSS selector matches nothing
        // and the generic "closed shadow root / cross-origin iframe" hint sends
        // people down the wrong path (#90.4). Point at the closest textual match.
        if let Some(hint) = suggest_textual_match(client, session_id, selector_or_ref).await {
            return Err(format!("Element not found: {}. {}", selector_or_ref, hint));
        }
        return Err(format!("Element not found: {}", selector_or_ref));
    };
    Ok((object_id, session_id.to_string()))
}

/// On a CSS-selector miss, if the selector carries a quoted string (usually a
/// `placeholder`), scan the page for the closest textual match and describe it,
/// so the error nudges toward `snapshot -i` + `@ref` instead of the misleading
/// shadow/iframe explanation (#90.4). Best-effort: returns None on any failure
/// or when the selector has no quoted text to look for.
async fn suggest_textual_match(
    client: &CdpClient,
    session_id: &str,
    selector: &str,
) -> Option<String> {
    // Pull the first quoted literal out of the selector (e.g. the value of a
    // `placeholder="…"` clause). No quoted text → nothing to search for.
    let want = extract_quoted(selector)?;
    if want.chars().count() < 2 {
        return None;
    }

    let js = format!(
        r#"
(function() {{
    var want = {want};
    function clean(s){{ return (s||'').replace(/\s+/g,' ').trim(); }}
    function vis(el){{ var r = el.getBoundingClientRect(); return r.width>0 || r.height>0; }}
    // 1. An element whose placeholder attribute holds the text (the common case:
    //    Element Plus mirrors it onto an inner span, not the <input>).
    var all = document.querySelectorAll('[placeholder]');
    for (var i = 0; i < all.length; i++) {{
        var p = all[i].getAttribute('placeholder') || '';
        if (p.indexOf(want) >= 0 && vis(all[i]))
            return {{ tag: all[i].tagName.toLowerCase(), via: 'placeholder', input: all[i].tagName === 'INPUT' || all[i].tagName === 'TEXTAREA' }};
    }}
    // 2. Any visible leaf element whose text equals/contains the wanted string.
    var leaves = document.querySelectorAll('span, div, label, button, a, p');
    for (var j = 0; j < leaves.length; j++) {{
        var el = leaves[j];
        if (el.children.length) continue;
        if (clean(el.textContent).indexOf(want) >= 0 && vis(el))
            return {{ tag: el.tagName.toLowerCase(), via: 'text', input: false }};
    }}
    return null;
}})()
"#,
        want = serde_json::to_string(&want).unwrap_or_default(),
    );

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: js,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await
        .ok()?;

    let v = result.result.value?;
    if v.is_null() {
        return None;
    }
    let tag = v.get("tag").and_then(|t| t.as_str()).unwrap_or("element");
    let via = v.get("via").and_then(|t| t.as_str()).unwrap_or("text");
    let is_input = v.get("input").and_then(|b| b.as_bool()).unwrap_or(false);
    if via == "placeholder" && !is_input {
        Some(format!(
            "No <input> matched, but a <{}> carries that placeholder \
             (Element Plus and similar render the placeholder on an inner element, \
             not the <input>). Run `snapshot -i` and act on the @ref instead of a CSS selector.",
            tag
        ))
    } else {
        Some(format!(
            "No element matched that selector, but a <{}> contains that text. \
             Run `snapshot -i` and act on the @ref (it names controls by placeholder/label).",
            tag
        ))
    }
}

/// Extract the first single- or double-quoted literal from a selector string,
/// e.g. `input[placeholder="请选择"]` → `请选择`. Returns None if unquoted.
fn extract_quoted(selector: &str) -> Option<String> {
    let bytes = selector.as_bytes();
    for (i, &c) in bytes.iter().enumerate() {
        if c == b'"' || c == b'\'' {
            let rest = &selector[i + 1..];
            if let Some(end) = rest.find(c as char) {
                let inner = &rest[..end];
                if !inner.is_empty() {
                    return Some(inner.to_string());
                }
            }
            return None;
        }
    }
    None
}

/// Determine which CDP session and parameters to use for an AX tree query.
/// Cross-origin iframes have a dedicated session (no frameId needed);
/// same-origin iframes use the parent session with a frameId parameter.
pub(super) fn resolve_ax_session<'a>(
    frame_id: Option<&str>,
    session_id: &'a str,
    iframe_sessions: &'a HashMap<String, String>,
) -> (serde_json::Value, &'a str) {
    if let Some(frame_id) = frame_id {
        if let Some(iframe_sid) = iframe_sessions.get(frame_id) {
            (serde_json::json!({}), iframe_sid.as_str())
        } else {
            (serde_json::json!({ "frameId": frame_id }), session_id)
        }
    } else {
        (serde_json::json!({}), session_id)
    }
}

/// Resolve the effective CDP session for an element's frame.
/// If the element's frame_id has a dedicated cross-origin iframe session, return it.
/// Otherwise, return the parent session.
fn resolve_frame_session<'a>(
    frame_id: Option<&str>,
    session_id: &'a str,
    iframe_sessions: &'a HashMap<String, String>,
) -> &'a str {
    frame_id
        .and_then(|fid| iframe_sessions.get(fid))
        .map(|s| s.as_str())
        .unwrap_or(session_id)
}

/// Verify that the cached backendNodeId still has the same accessible role
/// and name it had when the snapshot ran. Catches the case where React (or
/// any reconciler) reused the DOM node for a different component instance
/// — same physical node, different semantics.
///
/// On mismatch, returns an actionable error naming both the snapshot label
/// and the current label so the agent can re-snapshot intelligently.
/// On any CDP failure (e.g. node deleted), returns Ok(()) so the caller's
/// existing fallback (`find_node_id_by_role_name`) takes over.
async fn verify_ref_identity(
    client: &CdpClient,
    session_id: &str,
    backend_node_id: i64,
    ref_id: &str,
    expected_role: &str,
    expected_name: &str,
) -> Result<(), String> {
    let params = serde_json::json!({
        "backendNodeId": backend_node_id,
        "fetchRelatives": false,
    });
    // Tight 1s timeout: this is a defensive guard, not a critical path.
    // The default 30s CDP timeout was the dominant factor in the
    // "click hangs 5+ minutes" report — three CDP calls (verify +
    // resolveNode + paint-settle) at 30s each, multiplied by parallel
    // click invocations queueing on the daemon, totalled multi-minute
    // user-visible hangs. Cap our own helper so a stuck AX query
    // doesn't make `click` worse than the no-guard version was.
    let resp: Result<GetFullAXTreeResult, String> = match tokio::time::timeout(
        std::time::Duration::from_secs(1),
        client.send_command_typed("Accessibility.getPartialAXTree", &params, Some(session_id)),
    )
    .await
    {
        Ok(r) => r,
        // Timeout: skip identity verification rather than block the click.
        Err(_) => return Ok(()),
    };
    let Ok(tree) = resp else {
        // Node likely gone; let the box-model call fail and trigger fallback.
        return Ok(());
    };
    // Find the AXNode for our backendNodeId. fetchRelatives=false still
    // returns ancestors; the target node has the matching backendNodeId.
    let Some(node) = tree
        .nodes
        .iter()
        .find(|n| n.backend_d_o_m_node_id == Some(backend_node_id))
    else {
        return Ok(());
    };
    let actual_role = extract_ax_string(&node.role);
    let actual_name = extract_ax_string(&node.name);
    if actual_role == expected_role && actual_name == expected_name {
        return Ok(());
    }
    Err(format!(
        "Ref {} no longer matches its snapshot. Was [{} \"{}\"], now [{} \"{}\"].\n\
         The DOM mutated between snapshot and interaction (typical with React/Vue \
         reusing nodes during re-render). Fix: take a fresh `snapshot` and re-target \
         with the new ref. For SPAs where refs churn every interaction, drive the \
         element directly with `eval` (e.g. `eval \"document.querySelector(...).click()\"`), \
         which doesn't depend on refs.\n\
         (Last resort: AGENT_BROWSER_VERIFY_REF=0 disables this safety check — only \
         if you accept clicks may land on a re-rendered/wrong node.)",
        ref_id, expected_role, expected_name, actual_role, actual_name,
    ))
}

/// At the moment we'd dispatch the click, ask the page itself which element
/// occupies (x, y). If it's not our target (and not a descendant or
/// ancestor), an overlay has appeared between snapshot and click — we'd
/// silently click the overlay otherwise. Returns Err with details about
/// the occluding element so the caller can wait + re-snapshot.
///
/// Implemented as a single Runtime.callFunctionOn: resolve the cached
/// backendNodeId to a remote object, then run a function on it that
/// compares with elementFromPoint. The function returns null when the
/// click is safe and a JSON string with diagnostic info when it isn't.
async fn verify_click_target(
    client: &CdpClient,
    session_id: &str,
    backend_node_id: i64,
    ref_id: &str,
    x: f64,
    y: f64,
) -> Result<(), String> {
    use serde::Deserialize;

    // Resolve once. backendNodeId is stable across renders; only the
    // element under (x, y) is what changes when an overlay flickers.
    let resolve_params = DomResolveNodeParams {
        backend_node_id: Some(backend_node_id),
        node_id: None,
        object_group: Some("chrome-use-occlusion".to_string()),
    };
    let resolve_fut = client.send_command_typed::<_, serde_json::Value>(
        "DOM.resolveNode",
        &resolve_params,
        Some(session_id),
    );
    let Ok(resolve_resp) =
        tokio::time::timeout(std::time::Duration::from_millis(500), resolve_fut).await
    else {
        return Ok(());
    };
    let Ok(resolved) = resolve_resp else {
        return Ok(());
    };
    let Some(object_id) = resolved
        .get("object")
        .and_then(|o| o.get("objectId"))
        .and_then(|v| v.as_str())
    else {
        return Ok(());
    };

    // Auto-retry on transient occlusion. Many real-world overlays
    // (modal backdrops, focus rings, click-outside masks) blink in for
    // a frame or two during state transitions and clear on their own.
    // Without retries the user gets an "occluded" error and has to
    // wrap every click in their own retry loop. With retries the
    // common case is invisible — only persistent overlays surface.
    //
    // AGENT_BROWSER_OCCLUSION_RETRIES         (default 3, 0 disables)
    // AGENT_BROWSER_OCCLUSION_RETRY_DELAY_MS  (default 200)
    let max_retries: u32 = std::env::var("AGENT_BROWSER_OCCLUSION_RETRIES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let retry_delay_ms: u64 = std::env::var("AGENT_BROWSER_OCCLUSION_RETRY_DELAY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);

    #[derive(Deserialize)]
    struct Occluder {
        tag: Option<String>,
        testid: Option<String>,
        role: Option<String>,
        #[serde(rename = "ariaLabel")]
        aria_label: Option<String>,
        text: Option<String>,
        reason: Option<String>,
    }

    // function(x, y) { ... } where `this` is the target element.
    // Return null  → click is safe.
    // Return JSON  → describes the occluding element.
    let function_decl = "function(x, y) { \
        const at = document.elementFromPoint(x, y); \
        if (!at) return JSON.stringify({reason:'no-element-at-point'}); \
        if (at === this || this.contains(at) || at.contains(this)) return null; \
        return JSON.stringify({ \
            tag: at.tagName, \
            testid: (at.dataset && at.dataset.testid) || null, \
            role: at.getAttribute('role'), \
            ariaLabel: at.getAttribute('aria-label'), \
            text: ((at.textContent||'').trim().slice(0, 60)) \
        }); \
    }";

    let mut last_occ: Option<Occluder> = None;
    for attempt in 0..=max_retries {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(retry_delay_ms)).await;
        }
        let call_params = serde_json::json!({
            "objectId": object_id,
            "functionDeclaration": function_decl,
            "arguments": [{"value": x}, {"value": y}],
            "returnByValue": true,
        });
        let call_fut = client.send_command_typed::<_, serde_json::Value>(
            "Runtime.callFunctionOn",
            &call_params,
            Some(session_id),
        );
        let Ok(call_resp) =
            tokio::time::timeout(std::time::Duration::from_millis(500), call_fut).await
        else {
            return Ok(()); // probe itself stalled — fall through to click
        };
        let Ok(call_result) = call_resp else {
            return Ok(());
        };
        let value = call_result.get("result").and_then(|r| r.get("value"));
        let json_str = match value {
            Some(serde_json::Value::String(s)) => s.clone(),
            // null / undefined → element at point IS our target. Safe.
            _ => return Ok(()),
        };
        let occ: Occluder = match serde_json::from_str(&json_str) {
            Ok(v) => v,
            Err(_) => return Ok(()),
        };
        last_occ = Some(occ);
    }

    // All retries exhausted — overlay is sticky. Build the descriptive error.
    let occ = last_occ.expect("loop ran at least once");
    if let Some(reason) = occ.reason {
        return Err(format!(
            "Ref {} cannot be clicked at its computed position: {}. \
             The element may have moved off-screen — re-run snapshot.",
            ref_id, reason
        ));
    }
    let mut desc = occ.tag.unwrap_or_else(|| "unknown".to_string());
    if let Some(t) = occ.testid {
        desc.push_str(&format!("[testid={}]", t));
    }
    if let Some(r) = occ.role {
        desc.push_str(&format!("[role={}]", r));
    }
    if let Some(a) = occ.aria_label {
        desc.push_str(&format!("[aria-label=\"{}\"]", a));
    }
    if let Some(t) = occ.text {
        if !t.is_empty() {
            desc.push_str(&format!(" text=\"{}\"", t));
        }
    }
    let waited_ms = (max_retries as u64) * retry_delay_ms;
    Err(format!(
        "Ref {} is occluded by {} at the click point (still occluded after \
         {} retries / {}ms). A persistent overlay is in the way — \
         re-run snapshot, dismiss the overlay, or set \
         AGENT_BROWSER_VERIFY_CLICK_TARGET=0 to bypass.",
        ref_id, desc, max_retries, waited_ms,
    ))
}

/// Re-query the accessibility tree to find a node matching role+name+nth,
/// returning its fresh backendDOMNodeId. This uses the same data source
/// (Accessibility.getFullAXTree) that built the ref map during snapshot,
/// so role/name matching is guaranteed to be consistent.
async fn find_node_id_by_role_name(
    client: &CdpClient,
    session_id: &str,
    role: &str,
    name: &str,
    nth: Option<usize>,
    frame_id: Option<&str>,
    iframe_sessions: &HashMap<String, String>,
) -> Result<i64, String> {
    let (ax_params, effective_session_id) =
        resolve_ax_session(frame_id, session_id, iframe_sessions);
    let ax_tree: GetFullAXTreeResult = client
        .send_command_typed(
            "Accessibility.getFullAXTree",
            &ax_params,
            Some(effective_session_id),
        )
        .await?;

    let nth_index = nth.unwrap_or(0);
    let mut match_count: usize = 0;

    for node in &ax_tree.nodes {
        if node.ignored.unwrap_or(false) {
            continue;
        }
        let node_role = extract_ax_string(&node.role);
        let node_name = extract_ax_string(&node.name);
        if node_role == role && node_name == name {
            if match_count == nth_index {
                return node.backend_d_o_m_node_id.ok_or_else(|| {
                    format!(
                        "AX node has no backendDOMNodeId for role={} name={}",
                        role, name
                    )
                });
            }
            match_count += 1;
        }
    }

    Err(format!(
        "Could not locate element with role={} name={}",
        role, name
    ))
}

pub(super) fn extract_ax_string(value: &Option<AXValue>) -> String {
    match value {
        Some(v) => match &v.value {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::Bool(b)) => b.to_string(),
            _ => String::new(),
        },
        None => String::new(),
    }
}

/// Build a JS expression that finds a DOM element by CSS selector or XPath.
fn build_find_element_js(selector: &str) -> String {
    if let Some(xpath) = selector.strip_prefix("xpath=") {
        return format!(
            "document.evaluate({}, document, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null).singleNodeValue",
            serde_json::to_string(xpath).unwrap_or_default()
        );
    }
    // Bare string (or explicit `text=`): try CSS first, then fall back to
    // matching an interactive element by its VISIBLE TEXT. snapshot exposes
    // buttons/links by their name, so `click "購入手続きへ"` should resolve by
    // that label — previously it was fed straight to `querySelector` as CSS and
    // failed as an invalid selector even though the button was right there
    // (issue #24-B). CSS still wins when it matches, so existing selectors are
    // unaffected; nested/non-ASCII labels now resolve too.
    let text_only = selector.strip_prefix("text=");
    let force_text = text_only.is_some();
    let sel_json = serde_json::to_string(selector).unwrap_or_default();
    let want_json = serde_json::to_string(text_only.unwrap_or(selector)).unwrap_or_default();
    format!(
        r#"(() => {{
  const sel = {sel};
  const css = {force_text} ? null : (() => {{ try {{ return document.querySelector(sel); }} catch (_e) {{ return null; }} }})();
  if (css) return css;
  const norm = s => (s == null ? '' : String(s)).replace(/\s+/g, ' ').trim();
  const w = norm({want}); if (!w) return null;
  const wl = w.toLowerCase();
  const interactive = Array.from(document.querySelectorAll(
    'button,a,[role=button],[role=link],[role=menuitem],[role=tab],[role=option],input[type=submit],input[type=button],input[type=reset],summary,label,[onclick]'));
  const textOf = e => norm(e.innerText || e.textContent) || norm(e.value) ||
    norm(e.getAttribute && e.getAttribute('aria-label')) || norm(e.getAttribute && e.getAttribute('title'));
  let hit = interactive.find(e => textOf(e) === w) || interactive.find(e => textOf(e).toLowerCase().includes(wl));
  if (hit) return hit;
  const leaves = Array.from(document.querySelectorAll('*')).filter(e => !e.children.length);
  return leaves.find(e => norm(e.textContent) === w) || leaves.find(e => norm(e.textContent).toLowerCase().includes(wl)) || null;
}})()"#,
        sel = sel_json,
        want = want_json,
        force_text = force_text
    )
}

/// Build a JS expression that counts matching DOM elements by CSS selector or XPath.
fn build_count_elements_js(selector: &str) -> String {
    if let Some(xpath) = selector.strip_prefix("xpath=") {
        format!(
            "document.evaluate({}, document, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null).snapshotLength",
            serde_json::to_string(xpath).unwrap_or_default()
        )
    } else {
        format!(
            "document.querySelectorAll({}).length",
            serde_json::to_string(selector).unwrap_or_default()
        )
    }
}

fn build_selector_js(selector: &str) -> String {
    let find_expr = build_find_element_js(selector);
    format!(
        r#"(() => {{
            const el = {find_expr};
            if (!el) return null;
            const rect = el.getBoundingClientRect();
            return {{ x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 }};
        }})()"#,
    )
}

async fn resolve_by_selector(
    client: &CdpClient,
    session_id: &str,
    selector: &str,
) -> Result<(f64, f64), String> {
    let js = build_selector_js(selector);

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: js,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;

    // A syntactically-invalid CSS selector makes querySelector throw — surface
    // that as "invalid selector" rather than a misleading "element not found".
    if let Some(ex) = result.exception_details {
        return Err(format!("Invalid selector '{}': {}", selector, ex.text));
    }

    let val = result.result.value.unwrap_or(Value::Null);
    let x = val.get("x").and_then(|v| v.as_f64());
    let y = val.get("y").and_then(|v| v.as_f64());

    match (x, y) {
        (Some(x), Some(y)) => Ok((x, y)),
        _ => Err(format!("Element not found: {}", selector)),
    }
}

fn box_model_center(model: &BoxModel) -> (f64, f64) {
    // content quad: [x1,y1, x2,y2, x3,y3, x4,y4]
    if model.content.len() >= 8 {
        let x = (model.content[0] + model.content[2] + model.content[4] + model.content[6]) / 4.0;
        let y = (model.content[1] + model.content[3] + model.content[5] + model.content[7]) / 4.0;
        (x, y)
    } else {
        (0.0, 0.0)
    }
}

/// Centre plus width/height of the content box, derived from the quad's
/// bounding extent. Width/height feed humanize's in-bounds landing jitter; a
/// degenerate quad yields zero size, which the jitter treats as "land on
/// centre" (no jitter).
fn box_model_dims(model: &BoxModel) -> (f64, f64, f64, f64) {
    let (cx, cy) = box_model_center(model);
    if model.content.len() >= 8 {
        let xs = [
            model.content[0],
            model.content[2],
            model.content[4],
            model.content[6],
        ];
        let ys = [
            model.content[1],
            model.content[3],
            model.content[5],
            model.content[7],
        ];
        let w = xs.iter().cloned().fold(f64::MIN, f64::max)
            - xs.iter().cloned().fold(f64::MAX, f64::min);
        let h = ys.iter().cloned().fold(f64::MIN, f64::max)
            - ys.iter().cloned().fold(f64::MAX, f64::min);
        (cx, cy, w.max(0.0), h.max(0.0))
    } else {
        (cx, cy, 0.0, 0.0)
    }
}

pub async fn get_element_text(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<String, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration:
                    "function() { return this.innerText || this.textContent || ''; }".to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(result
        .result
        .value
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default())
}

/// Text content collected from a single frame of the page.
#[derive(Debug, Clone)]
pub struct FrameText {
    pub frame_id: String,
    pub url: String,
    /// "top" | "inline" (same-process child frame) | "oopif" (out-of-process).
    pub kind: &'static str,
    pub text: String,
}

// The expression we run in every frame to read its visible text. innerText
// honors CSS visibility (skips display:none), textContent is the fallback.
const FRAME_INNERTEXT_JS: &str = "(function(){try{var b=document.body||document.documentElement;return b?(b.innerText||b.textContent||''):'';}catch(e){return '';}})()";

async fn eval_text_default(client: &CdpClient, session_id: &str) -> String {
    let res = client
        .send_command(
            "Runtime.evaluate",
            Some(serde_json::json!({
                "expression": FRAME_INNERTEXT_JS,
                "returnByValue": true,
            })),
            Some(session_id),
        )
        .await;
    res.ok()
        .and_then(|v| v.get("result").and_then(|r| r.get("value")).cloned())
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default()
}

// Same-process child frames share the top renderer but live in their own
// execution context. Page.createIsolatedWorld hands us a context id bound to
// that frame so Runtime.evaluate reads the child document, not the parent.
async fn eval_text_in_frame(client: &CdpClient, session_id: &str, frame_id: &str) -> String {
    let ctx = client
        .send_command(
            "Page.createIsolatedWorld",
            Some(serde_json::json!({ "frameId": frame_id, "worldName": "chrome_use_text" })),
            Some(session_id),
        )
        .await
        .ok()
        .and_then(|v| v.get("executionContextId").and_then(|c| c.as_i64()));
    let Some(ctx_id) = ctx else {
        return String::new();
    };
    let res = client
        .send_command(
            "Runtime.evaluate",
            Some(serde_json::json!({
                "expression": FRAME_INNERTEXT_JS,
                "returnByValue": true,
                "contextId": ctx_id,
            })),
            Some(session_id),
        )
        .await;
    res.ok()
        .and_then(|v| v.get("result").and_then(|r| r.get("value")).cloned())
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default()
}

fn flatten_frame_tree(node: &Value, is_top: bool, out: &mut Vec<(String, String, bool)>) {
    if let Some(frame) = node.get("frame") {
        if let Some(id) = frame.get("id").and_then(|v| v.as_str()) {
            let url = frame
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            out.push((id.to_string(), url, is_top));
        }
    }
    if let Some(children) = node.get("childFrames").and_then(|v| v.as_array()) {
        for child in children {
            flatten_frame_tree(child, false, out);
        }
    }
}

/// Collect visible text from every frame reachable in the active session,
/// including out-of-process iframes (which never appear in the top frame's
/// `Page.getFrameTree` and so are invisible to `document.body.innerText`).
///
/// Same-process child frames are read through `Page.createIsolatedWorld`;
/// OOPIFs are read through their own auto-attached debugger session
/// (`iframe_sessions`, keyed by frameId == targetId). This is the engine
/// behind `get text --all-frames` and `chrome-use frames` — the fix for
/// listing/marketplace pages whose description lives in a child frame (#27).
pub async fn collect_all_frames_text(
    client: &CdpClient,
    top_session: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<Vec<FrameText>, String> {
    let mut out: Vec<FrameText> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1. Top session: the top frame plus its same-process descendants. OOPIF
    //    frames that happen to surface here are skipped — they're read via
    //    their dedicated session in step 2 (cross-process isolated worlds fail).
    let tree = client
        .send_command_no_params("Page.getFrameTree", Some(top_session))
        .await?;
    let mut frames: Vec<(String, String, bool)> = Vec::new();
    flatten_frame_tree(&tree["frameTree"], true, &mut frames);
    for (fid, url, is_top) in frames {
        if iframe_sessions.contains_key(&fid) {
            continue;
        }
        if !seen.insert(fid.clone()) {
            continue;
        }
        let (kind, text) = if is_top {
            ("top", eval_text_default(client, top_session).await)
        } else {
            (
                "inline",
                eval_text_in_frame(client, top_session, &fid).await,
            )
        };
        out.push(FrameText {
            frame_id: fid,
            url,
            kind,
            text,
        });
    }

    // 2. Each out-of-process iframe, read through its own session.
    for (fid, sid) in iframe_sessions {
        if !seen.insert(fid.clone()) {
            continue;
        }
        let url = client
            .send_command_no_params("Page.getFrameTree", Some(sid))
            .await
            .ok()
            .and_then(|t| {
                t.get("frameTree")
                    .and_then(|ft| ft.get("frame"))
                    .and_then(|f| f.get("url"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();
        let text = eval_text_default(client, sid).await;
        out.push(FrameText {
            frame_id: fid.clone(),
            url,
            kind: "oopif",
            text,
        });
    }

    Ok(out)
}

// Readability-lite: prefer the page's semantic main-content region over the
// whole body so global header/nav/footer chrome (and, on many listing pages,
// the "related items" sidebar) doesn't drown out the actual content. Runs on
// the live, rendered tree (innerText needs layout — a detached clone returns
// empty), so we pick the densest <main>/<article> region rather than cloning
// and stripping. Falls back to <body> when no substantial main region exists.
const MAIN_CONTENT_JS: &str = r#"(function(){
  function txt(el){try{return (el.innerText||'').trim();}catch(e){return '';}}
  var sels=['main','[role=main]','article','#main','#contents','#l-content'];
  var best=null,bestLen=0;
  for(var i=0;i<sels.length;i++){
    var els=document.querySelectorAll(sels[i]);
    for(var j=0;j<els.length;j++){var l=txt(els[j]).length;if(l>bestLen){bestLen=l;best=els[j];}}
  }
  if(best&&bestLen>200)return txt(best);
  return txt(document.body);
})()"#;

/// Extract the page's main-content text (readability-lite), preferring a
/// semantic `<main>`/`<article>` region over the full body. Used by
/// `get text --main` to avoid header/nav/sidebar boilerplate (#27).
pub async fn get_main_content_text(client: &CdpClient, session_id: &str) -> Result<String, String> {
    let res = client
        .send_command(
            "Runtime.evaluate",
            Some(serde_json::json!({
                "expression": MAIN_CONTENT_JS,
                "returnByValue": true,
            })),
            Some(session_id),
        )
        .await?;
    Ok(res
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string())
}

// Text nodes whose parent is one of these carry no visible content.
fn is_noise_tag(name: &str) -> bool {
    matches!(name, "SCRIPT" | "STYLE" | "NOSCRIPT" | "TEMPLATE" | "HEAD")
}

// Walk a CDP DOM.Node tree, collecting text-node values. Unlike `innerText`
// (JS, blocked by CLOSED shadow roots), the CDP DOM tree from
// `DOM.getDocument(pierce:true)` includes closed shadow roots and child
// documents — so this reaches text JS can't. `parent_noise` carries whether an
// ancestor was <script>/<style>/etc so their text is skipped.
fn collect_dom_text(node: &Value, parent_noise: bool, out: &mut String) {
    let node_type = node.get("nodeType").and_then(|v| v.as_i64()).unwrap_or(0);
    let node_name = node.get("nodeName").and_then(|v| v.as_str()).unwrap_or("");
    if node_type == 3 {
        if !parent_noise {
            if let Some(t) = node.get("nodeValue").and_then(|v| v.as_str()) {
                let t = t.trim();
                if !t.is_empty() {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(t);
                }
            }
        }
        return;
    }
    let noise = parent_noise || is_noise_tag(node_name);
    if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
        for child in children {
            collect_dom_text(child, noise, out);
        }
    }
    if let Some(shadow) = node.get("shadowRoots").and_then(|v| v.as_array()) {
        for sr in shadow {
            collect_dom_text(sr, noise, out);
        }
    }
    if let Some(doc) = node.get("contentDocument") {
        collect_dom_text(doc, noise, out);
    }
}

/// Extract text from the page via the CDP DOM tree with `pierce:true`, which
/// reaches into CLOSED shadow roots and child documents that `innerText`/`eval`
/// cannot. Lets an agent read content rendered into a closed shadow DOM (e.g. an
/// extension's injected debug panel) without any extra Chrome permission — it
/// rides the per-tab debugger session that's already attached (#30).
pub async fn get_pierced_text(client: &CdpClient, session_id: &str) -> Result<String, String> {
    let doc = client
        .send_command(
            "DOM.getDocument",
            Some(serde_json::json!({ "depth": -1, "pierce": true })),
            Some(session_id),
        )
        .await?;
    let mut out = String::new();
    if let Some(root) = doc.get("root") {
        collect_dom_text(root, false, &mut out);
    }
    Ok(out)
}

pub async fn get_element_attribute(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    attribute: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<Value, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: format!(
                    "function() {{ return this.getAttribute({}); }}",
                    serde_json::to_string(attribute).unwrap_or_default()
                ),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(result.result.value.unwrap_or(Value::Null))
}

pub async fn is_element_visible(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<bool, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: r#"function() {
                    const rect = this.getBoundingClientRect();
                    const style = window.getComputedStyle(this);
                    return rect.width > 0 && rect.height > 0 &&
                           style.visibility !== 'hidden' &&
                           style.display !== 'none' &&
                           parseFloat(style.opacity) > 0;
                }"#
                .to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(result
        .result
        .value
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

pub async fn is_element_enabled(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<bool, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: "function() { return !this.disabled; }".to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(result
        .result
        .value
        .and_then(|v| v.as_bool())
        .unwrap_or(true))
}

pub async fn is_element_checked(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<bool, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    // Mirrors Playwright's getChecked() with follow-label retargeting:
    // 1. If element is a native checkbox/radio input, return .checked
    // 2. If element has an ARIA checked role, return aria-checked
    // 3. Follow label → input association (label.control)
    // 4. Check for nested checkbox/radio input as last resort
    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: r#"function() {
                    var el = this;
                    // Native checkbox/radio input
                    var tag = el.tagName && el.tagName.toUpperCase();
                    if (tag === 'INPUT' && (el.type === 'checkbox' || el.type === 'radio')) {
                        return el.checked;
                    }
                    // ARIA role-based checked state
                    var role = el.getAttribute && el.getAttribute('role');
                    var ariaCheckedRoles = ['checkbox','radio','switch','menuitemcheckbox','menuitemradio','option','treeitem'];
                    if (role && ariaCheckedRoles.indexOf(role) !== -1) {
                        return el.getAttribute('aria-checked') === 'true';
                    }
                    // Follow label association (Playwright follow-label retarget)
                    var label = el;
                    if (tag !== 'LABEL') {
                        label = el.closest && el.closest('label');
                    }
                    if (label && label.tagName && label.tagName.toUpperCase() === 'LABEL' && label.control) {
                        var ctrl = label.control;
                        if (ctrl.type === 'checkbox' || ctrl.type === 'radio') {
                            return ctrl.checked;
                        }
                    }
                    // Check for nested native input
                    var input = el.querySelector && el.querySelector('input[type="checkbox"], input[type="radio"]');
                    if (input) return input.checked;
                    return false;
                }"#.to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(result
        .result
        .value
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

pub async fn get_element_inner_text(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<String, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: "function() { return this.innerText || ''; }".to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(result
        .result
        .value
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default())
}

pub async fn get_element_inner_html(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<String, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: "function() { return this.innerHTML || ''; }".to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(result
        .result
        .value
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default())
}

pub async fn get_element_input_value(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<String, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                // Read rich-editor content too (issue #41): CodeMirror 5 / Monaco
                // keep their text in a model, not `.value`; contenteditable keeps
                // it as innerText. Falls back to `.value` for plain inputs.
                function_declaration: r#"function() {
                    const el = this;
                    const cm5 = el.closest && el.closest('.CodeMirror');
                    if (cm5 && cm5.CodeMirror) return cm5.CodeMirror.getValue();
                    if (window.monaco && monaco.editor) {
                        try {
                            const eds = monaco.editor.getEditors ? monaco.editor.getEditors() : [];
                            const ed = eds.find(e => e.getDomNode && e.getDomNode().contains(el)) || eds[0];
                            if (ed) return ed.getValue();
                            const m = monaco.editor.getModels ? monaco.editor.getModels() : [];
                            if (m[0]) return m[0].getValue();
                        } catch (e) {}
                    }
                    if (typeof el.value === 'string') return el.value;
                    if (el.isContentEditable) return el.innerText;
                    return '';
                }"#
                .to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(result
        .result
        .value
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default())
}

pub async fn set_element_value(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    value: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let js = format!(
        "function() {{ this.value = {}; this.dispatchEvent(new Event('input', {{bubbles: true}})); this.dispatchEvent(new Event('change', {{bubbles: true}})); }}",
        serde_json::to_string(value).unwrap_or_default()
    );

    client
        .send_command_typed::<_, EvaluateResult>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: js,
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(())
}

pub async fn get_element_bounding_box(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<Value, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: r#"function() {
                    const r = this.getBoundingClientRect();
                    const inViewport = r.bottom > 0 && r.right > 0
                        && r.top < (innerHeight || document.documentElement.clientHeight)
                        && r.left < (innerWidth || document.documentElement.clientWidth);
                    return {
                        x: r.x, y: r.y, width: r.width, height: r.height,
                        centerX: Math.round(r.x + r.width / 2),
                        centerY: Math.round(r.y + r.height / 2),
                        inViewport,
                    };
                }"#
                .to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    result
        .result
        .value
        .ok_or_else(|| format!("Could not get bounding box for: {}", selector_or_ref))
}

pub async fn get_element_count(
    client: &CdpClient,
    session_id: &str,
    selector: &str,
) -> Result<i64, String> {
    let js = build_count_elements_js(selector);

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: js,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;

    Ok(result.result.value.and_then(|v| v.as_i64()).unwrap_or(0))
}

pub async fn get_element_styles(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    properties: Option<Vec<String>>,
    iframe_sessions: &HashMap<String, String>,
) -> Result<Value, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let js = match properties {
        Some(props) => {
            let props_json = serde_json::to_string(&props).unwrap_or("[]".to_string());
            format!(
                r#"function() {{
                    const s = window.getComputedStyle(this);
                    const props = {};
                    const result = {{}};
                    for (const p of props) result[p] = s.getPropertyValue(p);
                    return result;
                }}"#,
                props_json
            )
        }
        None => r#"function() {
                    const s = window.getComputedStyle(this);
                    const result = {};
                    for (let i = 0; i < s.length; i++) {
                        const p = s[i];
                        result[p] = s.getPropertyValue(p);
                    }
                    return result;
                }"#
        .to_string(),
    };

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: js,
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(result.result.value.unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ref_at_prefix() {
        assert_eq!(parse_ref("@e1"), Some("e1".to_string()));
        assert_eq!(parse_ref("@e123"), Some("e123".to_string()));
    }

    #[test]
    fn test_extract_quoted() {
        // double- and single-quoted placeholder values (the #90.4 case)
        assert_eq!(
            extract_quoted(r#"input[placeholder="请选择试听名字"]"#),
            Some("请选择试听名字".to_string())
        );
        assert_eq!(
            extract_quoted("input[placeholder='hello world']"),
            Some("hello world".to_string())
        );
        // first literal wins when several are present
        assert_eq!(
            extract_quoted(r#"[data-x="a"][title="b"]"#),
            Some("a".to_string())
        );
        // no quotes / empty literal → nothing to search for
        assert_eq!(extract_quoted("div.foo > span"), None);
        assert_eq!(extract_quoted(r#"input[value=""]"#), None);
    }

    #[test]
    fn test_collect_dom_text_pierces_closed_shadow_and_skips_noise() {
        // A CDP DOM.Node tree: a host element whose CLOSED shadow root holds the
        // text, plus a <script> whose text must be skipped.
        let tree = serde_json::json!({
            "nodeType": 1, "nodeName": "BODY",
            "children": [
                { "nodeType": 1, "nodeName": "SCRIPT",
                  "children": [ { "nodeType": 3, "nodeName": "#text", "nodeValue": "var secret=1;" } ] },
                { "nodeType": 1, "nodeName": "DIV",
                  "shadowRoots": [
                    { "nodeType": 11, "nodeName": "#document-fragment",
                      "children": [
                        { "nodeType": 1, "nodeName": "SPAN",
                          "children": [ { "nodeType": 3, "nodeName": "#text", "nodeValue": "DECRYPTED 42" } ] }
                      ] }
                  ] }
            ]
        });
        let mut out = String::new();
        collect_dom_text(&tree, false, &mut out);
        assert_eq!(out, "DECRYPTED 42");
        assert!(!out.contains("secret"), "script text must be skipped");
    }

    #[test]
    fn test_parse_ref_equals_prefix() {
        assert_eq!(parse_ref("ref=e1"), Some("e1".to_string()));
    }

    #[test]
    fn test_parse_ref_bare() {
        assert_eq!(parse_ref("e1"), Some("e1".to_string()));
        assert_eq!(parse_ref("e42"), Some("e42".to_string()));
    }

    #[test]
    fn test_parse_ref_invalid() {
        assert_eq!(parse_ref("button"), None);
        assert_eq!(parse_ref("e"), None);
        assert_eq!(parse_ref("1"), None);
        assert_eq!(parse_ref(""), None);
    }

    #[test]
    fn test_ref_map_basic() {
        let mut map = RefMap::new();
        map.add("e1".to_string(), Some(42), "button", "Submit", None);
        assert!(map.get("e1").is_some());
        assert_eq!(map.get("e1").unwrap().role, "button");
        assert!(map.get("e2").is_none());
    }

    #[test]
    fn test_build_selector_js_css() {
        let js = build_selector_js("#submit-btn");
        // CSS is now tried via a `sel` variable, with a visible-text fallback
        // appended (issue #24-B). It must still use querySelector (not xpath).
        assert!(js.contains("const sel = \"#submit-btn\""));
        assert!(js.contains("document.querySelector(sel)"));
        assert!(!js.contains("document.evaluate"));
    }

    #[test]
    fn test_build_find_element_js_text_fallback() {
        // A bare label gets a text-matching fallback so `click "購入手続きへ"`
        // resolves by visible text, not just CSS (issue #24-B).
        let js = build_find_element_js("購入手続きへ");
        assert!(js.contains("購入手続きへ"));
        assert!(js.contains("interactive")); // the text-match branch
        assert!(js.contains("textOf"));
        // `text=` forces the text path (skips CSS).
        let forced = build_find_element_js("text=Buy now");
        assert!(forced.contains("true ? null")); // force_text => css skipped
                                                 // xpath is unchanged.
        let xp = build_find_element_js("xpath=//button");
        assert!(xp.contains("document.evaluate"));
        assert!(!xp.contains("interactive"));
    }

    #[test]
    fn test_build_selector_js_xpath() {
        let js = build_selector_js("xpath=//button[@id='ok']");
        assert!(js.contains("document.evaluate(\"//button[@id='ok']\", document, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null)"));
        assert!(!js.contains("document.querySelector"));
    }

    #[test]
    fn test_build_selector_js_xpath_empty() {
        let js = build_selector_js("xpath=");
        assert!(js.contains("document.evaluate"));
    }

    #[test]
    fn test_build_selector_js_not_xpath_prefix() {
        // "xpath" without "=" should be treated as CSS selector
        let js = build_selector_js("xpath//div");
        assert!(js.contains("document.querySelector"));
    }

    #[test]
    fn test_build_count_elements_js_css() {
        let js = build_count_elements_js(".item");
        assert!(js.contains("document.querySelectorAll(\".item\").length"));
        assert!(!js.contains("document.evaluate"));
    }

    #[test]
    fn test_build_count_elements_js_xpath() {
        let js = build_count_elements_js("xpath=//li");
        assert!(js.contains("document.evaluate(\"//li\", document, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null).snapshotLength"));
        assert!(!js.contains("querySelectorAll"));
    }

    #[test]
    fn test_box_model_center() {
        let model = BoxModel {
            content: vec![10.0, 20.0, 110.0, 20.0, 110.0, 60.0, 10.0, 60.0],
            padding: vec![],
            border: vec![],
            margin: vec![],
            width: 100,
            height: 40,
        };
        let (x, y) = box_model_center(&model);
        assert!((x - 60.0).abs() < 0.01);
        assert!((y - 40.0).abs() < 0.01);
    }

    // -----------------------------------------------------------------------
    // resolve_frame_session tests (Issue #925)
    // Cross-origin iframe elements must resolve to the dedicated session.
    // -----------------------------------------------------------------------

    #[test]
    fn test_cross_origin_element_uses_dedicated_session() {
        let mut iframe_sessions = HashMap::new();
        iframe_sessions.insert(
            "cross-origin-frame".to_string(),
            "iframe-session".to_string(),
        );

        let session = resolve_frame_session(
            Some("cross-origin-frame"),
            "parent-session",
            &iframe_sessions,
        );

        assert_eq!(session, "iframe-session");
    }

    #[test]
    fn test_same_origin_element_uses_parent_session() {
        let iframe_sessions = HashMap::new();

        let session = resolve_frame_session(
            Some("same-origin-frame"),
            "parent-session",
            &iframe_sessions,
        );

        assert_eq!(session, "parent-session");
    }

    #[test]
    fn test_main_frame_element_uses_parent_session() {
        let iframe_sessions = HashMap::new();

        let session = resolve_frame_session(None, "parent-session", &iframe_sessions);

        assert_eq!(session, "parent-session");
    }
}
