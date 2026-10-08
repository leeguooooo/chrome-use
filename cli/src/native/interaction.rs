use std::collections::HashMap;

use serde_json::{json, Value};

use super::cdp::client::CdpClient;
use super::cdp::types::*;
use super::element::{
    parse_ref, read_editable_value_function, resolve_element_center, resolve_element_object_id,
    RefMap, MONACO_CANDIDATES_FUNCTION,
};
use super::humanize;

/// Whether a pointer interaction should be DOM-dispatched (invoke the event on
/// the element in its own session) rather than dispatched at a viewport
/// coordinate via `Input.dispatchMouseEvent`. True when the target is inside an
/// iframe (an OOPIF element's box can't be mapped to a top-viewport point) or we
/// drive over the extension relay (a coordinate Input event isn't confined to the
/// target tab on a busy real Chrome — it drifts onto the foreground tab; issues
/// #31/#36). DOM-dispatch always hits the right element in the right tab.
fn prefer_dom_dispatch(ref_map: &RefMap, selector_or_ref: &str) -> bool {
    ref_map.ref_is_in_iframe(selector_or_ref) || crate::connect::relay_url().is_some()
}

/// How long the cursor is shown travelling to the target before the click fires.
/// Without a lead the overlay and the click land in the same frame and the click
/// reads as instantaneous — you see the aftermath, never the movement.
const CURSOR_LEAD_MS: u64 = 250;

/// Whether the extension's cursor overlay is switched on: 0 = unknown, 1 = on,
/// 2 = off. The overlay is an extension-side setting the daemon can't read, so we
/// learn it from the first `ABExt.driveCursor` reply and then stop paying for the
/// round trip when it's off (the common case). Toggling the setting takes effect
/// on the next daemon start.
static CURSOR_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// True once we've confirmed the overlay is switched off, so callers can skip
/// cursor round trips entirely on the default path.
pub fn cursor_known_off() -> bool {
    CURSOR_STATE.load(std::sync::atomic::Ordering::Relaxed) == 2
}

/// Consecutive inconclusive `ABExt.driveCursor` replies (`no-tab`, `bad-coords`,
/// a shape we don't recognize) before the cursor is treated as off. Those are
/// transient in principle, so one of them must not disable the cursor — but an
/// extension that returns them *every* time would otherwise make every click pay
/// a rect lookup, two round trips and [`CURSOR_LEAD_MS`] forever.
const CURSOR_INCONCLUSIVE_LIMIT: u8 = 3;

static CURSOR_INCONCLUSIVE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// What one `ABExt.driveCursor` reply says about the overlay: `Some(state)` to
/// move [`CURSOR_STATE`] there, `None` when the reply proves nothing.
///
/// Split out from the round trip so the decision table is unit-testable — the
/// caller needs a live CDP client, this doesn't.
fn cursor_state_from_reply(drawn: Option<bool>, reason: Option<&str>) -> Option<u8> {
    match (drawn, reason) {
        (Some(true), _) => Some(1),
        // Only an explicit `disabled` proves the overlay is off. `no-tab` and
        // `bad-coords` are transient — caching them on the first sighting would
        // silently kill the cursor (and the hide-during-screenshot that depends
        // on it) for the daemon's whole life.
        (Some(false), Some("disabled")) => Some(2),
        _ => None,
    }
}

/// Mirror a DOM-dispatched click onto the on-page cursor overlay.
///
/// The extension mirrors `Input.dispatchMouseEvent` automatically, but a relay
/// click goes through the DOM and produces no such event — so with the overlay
/// enabled the pointer sat motionless while things got clicked. Best-effort
/// throughout: a failure here must never fail the click.
async fn drive_cursor(client: &CdpClient, session_id: &str, x: f64, y: f64, click: bool) {
    use std::sync::atomic::Ordering;
    if CURSOR_STATE.load(Ordering::Relaxed) == 2 {
        return;
    }
    let reply = client
        .send_command_typed::<_, serde_json::Value>(
            "ABExt.driveCursor",
            &serde_json::json!({ "sessionId": session_id, "x": x, "y": y, "click": click }),
            None,
        )
        .await;
    match reply {
        Ok(v) => {
            let drawn = v.get("drawn").and_then(serde_json::Value::as_bool);
            let reason = v.get("reason").and_then(serde_json::Value::as_str);
            match cursor_state_from_reply(drawn, reason) {
                Some(next) => {
                    CURSOR_INCONCLUSIVE.store(0, Ordering::Relaxed);
                    CURSOR_STATE.store(next, Ordering::Relaxed);
                }
                // Inconclusive: keep the state, but stop retrying forever.
                None => {
                    let seen = CURSOR_INCONCLUSIVE.fetch_add(1, Ordering::Relaxed) + 1;
                    if seen >= CURSOR_INCONCLUSIVE_LIMIT {
                        let _ = CURSOR_STATE.compare_exchange(
                            0,
                            2,
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        );
                    }
                }
            }
        }
        // An extension predating `ABExt.driveCursor` errors every time. Stop
        // paying the rect lookup + two round trips + the lead delay on every
        // click — but only if the cursor has never worked; a hiccup shouldn't
        // disable a cursor we've already seen drawing.
        Err(_) => {
            let _ = CURSOR_STATE.compare_exchange(0, 2, Ordering::Relaxed, Ordering::Relaxed);
        }
    }
}

pub async fn click(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    button: &str,
    click_count: i32,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    // Internal callers (check, download, sign-in) keep the DOM fallback for a
    // covered target: a styled checkbox covering its own hidden input is the
    // normal case there, and they verify the result themselves.
    click_reporting(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        button,
        click_count,
        iframe_sessions,
        false,
    )
    .await
    .map(|_| ())
}

/// How a click reached the page, so the caller can say when it was one a page
/// may ignore (issue #358).
pub struct ClickOutcome {
    /// `pointer` (trusted `Input.dispatchMouseEvent`), `keyboard` (trusted
    /// Enter/Space on the focused element) or `dom` (`element.click()`,
    /// `isTrusted: false`).
    pub dispatch: &'static str,
    /// Set whenever the click may not have done what a user's click does.
    pub warning: Option<String>,
}

impl ClickOutcome {
    fn trusted(dispatch: &'static str, warning: Option<String>) -> Self {
        Self { dispatch, warning }
    }

    fn dom(reason: &str, warning: Option<String>) -> Self {
        let dom = format!(
            "the click was dispatched through the DOM (element.click(), isTrusted=false) because \
             {reason}; a page that only honours real input ignores it. If nothing changed, \
             retry with `click --observe` to see whether a request went out"
        );
        Self {
            dispatch: "dom",
            warning: Some(match warning {
                Some(w) => format!("{w}; {dom}"),
                None => dom,
            }),
        }
    }
}

/// The message for a click on a control the browser will not deliver a click
/// to. A disabled `<button>` receives no click event at all, so "clicking" it
/// used to report `✓ Done` while the page did nothing (issue #358: a dialog's
/// Save button that stayed disabled because the page never registered the
/// edit before it).
pub(crate) fn disabled_click_error(selector_or_ref: &str) -> String {
    format!(
        "{selector_or_ref} is disabled, so clicking it does nothing: the browser delivers no \
         click to a disabled control. The page has not enabled it yet. If an earlier fill or \
         type was supposed to enable it, the page did not register that edit: check the \
         field with `get value`, re-enter it with `fill` (trusted input) or `type --key-events`, \
         and snapshot again before clicking"
    )
}

/// `click` that also reports how the click was delivered. A disabled target
/// is refused rather than clicked.
#[allow(clippy::too_many_arguments)]
pub async fn click_reporting(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    button: &str,
    click_count: i32,
    iframe_sessions: &HashMap<String, String>,
    // Refuse a target something else covers (the `click` command), instead of
    // clicking it through the DOM.
    refuse_covered: bool,
) -> Result<ClickOutcome, String> {
    // AGENT_BROWSER_CLICK_MODE: "" (default) = coordinate click; a target that
    // something else covers is refused, other coordinate failures fall back to
    // the DOM; "dom-fallback" (= `click --allow-dom`) = also DOM-dispatch a
    // covered target; "coord" = strict coordinate only (no fallback); "dom" =
    // always dispatch through the DOM.
    let mode = std::env::var("AGENT_BROWSER_CLICK_MODE").unwrap_or_default();
    let refuse_covered = refuse_covered && mode != "dom-fallback";

    // (A) Scroll the target into view first so the computed coordinates land
    // inside the viewport. Without this, an element below the fold (or revealed
    // after scroll/popup) yields off-viewport coordinates and the click lands on
    // whatever currently occupies that point. Best-effort: ignore failures.
    // The same round trip reports whether the control is disabled.
    let disabled = scroll_into_view_if_needed(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await;
    let mut warning = None;
    match disabled.as_deref() {
        Some("disabled") => return Err(disabled_click_error(selector_or_ref)),
        // aria-disabled is advisory: the browser still delivers the click and
        // some pages answer it (a validation message), so warn, don't refuse.
        Some("aria-disabled") => {
            warning = Some(format!(
                "{selector_or_ref} is marked aria-disabled=\"true\": the page says it is not \
                 available yet, so the click probably did nothing. If an earlier fill or type \
                 was supposed to enable it, the page did not register that edit"
            ));
        }
        _ => {}
    }

    if mode == "dom" {
        dom_click(
            client,
            session_id,
            ref_map,
            selector_or_ref,
            iframe_sessions,
        )
        .await?;
        return Ok(ClickOutcome::dom(
            "AGENT_BROWSER_CLICK_MODE=dom is set",
            warning,
        ));
    }

    // An element INSIDE an iframe needs a TRUSTED activation: a DOM `.click()` is
    // `isTrusted:false`, which security-sensitive embedded forms reject — Google
    // Payments' enabled `保存` button silently no-ops on a synthetic click (issue
    // #39). A coordinate `Input.dispatchMouseEvent` can't help either: `getBoxModel`
    // for a sub-frame node returns frame-local coordinates that don't compose the
    // iframe's offset, so the click lands in the wrong place. The frame-agnostic
    // trusted path is keyboard activation — focus the element in its own frame, then
    // dispatch a real Enter on the page session; Chrome routes the key to the
    // focused element regardless of frame (same as `type --focused`), and Enter on a
    // focused button/link fires a trusted `click`. `coord` mode opts out.
    let in_iframe = ref_map.ref_is_in_iframe(selector_or_ref);
    if mode != "coord" && button == "left" && click_count == 1 && in_iframe {
        let keyboard = dom_activate(
            client,
            session_id,
            ref_map,
            selector_or_ref,
            iframe_sessions,
        )
        .await?;
        return Ok(if keyboard {
            ClickOutcome::trusted("keyboard", warning)
        } else {
            ClickOutcome::dom(
                "the target is inside an iframe and has no keyboard-activatable role",
                warning,
            )
        });
    }
    // Coordinate clicks dispatch trusted Input.dispatchMouseEvent events (isTrusted: true),
    // which security-sensitive buttons and forms require. If coordinate resolution fails
    // or the target is occluded, execution automatically falls back to DOM dispatch below.

    let resolved = resolve_element_center(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await;

    match resolved {
        Ok((cx, cy, w, h, effective_session_id)) => {
            // Occlusion guard for the CSS-selector path. `@ref` clicks are already
            // occlusion-checked in resolve_element_center, but a plain selector
            // resolves to coordinates without that check — so an overlay (modal
            // backdrop, sticky banner, the getByText located node sitting under a
            // full-screen layer) would make the coordinate click land on the
            // overlay and still report success. If the click point doesn't hit the
            // target, dispatch through the DOM instead (targets the element
            // directly). Skipped for strict `coord` mode and non-left/multi-clicks.
            let covered = if mode != "coord"
                && button == "left"
                && click_count == 1
                && parse_ref(selector_or_ref).is_none()
            {
                point_misses_element(client, &effective_session_id, selector_or_ref).await
            } else {
                None
            };
            if let Some(cover) = covered {
                // A DOM `.click()` on a covered target reports success for a click
                // a user could not have made, and the agent then believes it hit
                // the real control. Refuse unless the caller opted in.
                if refuse_covered {
                    return Err(occluded_refusal(selector_or_ref, &cover));
                }
                eprintln!(
                    "[click] target occluded at its click point; dispatching through \
                     the DOM (set AGENT_BROWSER_CLICK_MODE=coord to disable)"
                );
                dom_click(
                    client,
                    session_id,
                    ref_map,
                    selector_or_ref,
                    iframe_sessions,
                )
                .await?;
                return Ok(ClickOutcome::dom(
                    "something else covers its click point",
                    warning,
                ));
            }
            // Land on a jittered point inside the element rather than its exact
            // centre (Fast/Human). Zero size or Off → exact centre.
            let (tx, ty) = humanize::landing_point(
                (cx - w / 2.0, cy - h / 2.0, w, h),
                humanize::active_level(),
                humanize::next_seed(),
            );
            if super::element::semantic_pin_active(selector_or_ref) {
                let object =
                    super::element::resolve_semantic_pin(client, &effective_session_id).await?;
                let check = client.send_command("Runtime.callFunctionOn", Some(serde_json::json!({
                    "objectId":object,"functionDeclaration":"function(x,y){const hit=this.ownerDocument.elementFromPoint(x,y);return hit===this || this.contains(hit);}",
                    "arguments":[{"value":tx},{"value":ty}],"returnByValue":true
                })),Some(&effective_session_id)).await?;
                if check["result"]["value"] != true {
                    return Err(
                        "semantic target moved or is covered at dispatch; refusing click".into(),
                    );
                }
            }
            dispatch_click(client, &effective_session_id, tx, ty, button, click_count).await?;
            Ok(ClickOutcome::trusted("pointer", warning))
        }
        Err(e) => {
            // (B) The coordinate path failed — typically a persistent overlay
            // failing the occlusion guard, or coordinates that won't resolve.
            // Fall back to a DOM-dispatched `.click()` on the intended element,
            // which targets the element directly instead of a screen point.
            // Skipped for strict "coord" mode and for non-left / multi-clicks
            // (a DOM `.click()` can't express right/middle/double semantics).
            if mode == "coord" || button != "left" || click_count != 1 {
                return Err(e);
            }
            // A persistent overlay (the @ref occlusion guard gave up): refuse
            // unless the caller opted in, for the same reason as above.
            if e.contains(" is occluded by ") && refuse_covered {
                return Err(format!(
                    "{e} To click the covered element anyway through the DOM \
                     (element.click(), isTrusted=false), add --allow-dom."
                ));
            }
            eprintln!(
                "[click] coordinate click failed ({e}); falling back to DOM dispatch \
                 (set AGENT_BROWSER_CLICK_MODE=coord to disable)"
            );
            dom_click(
                client,
                session_id,
                ref_map,
                selector_or_ref,
                iframe_sessions,
            )
            .await
            .map_err(|dom_err| {
                // The same cause (an unknown ref) fails both paths; say it once.
                if dom_err == e {
                    e.clone()
                } else {
                    format!("{e}\n(DOM-dispatch fallback also failed: {dom_err})")
                }
            })?;
            // The daemon's stderr is not the caller's: the reason has to travel
            // in the response or nobody sees that the click was not a real one.
            let first_line = e.lines().next().unwrap_or("").trim().to_string();
            Ok(ClickOutcome::dom(
                &format!("a pointer click could not be placed ({first_line})"),
                warning,
            ))
        }
    }
}

fn occluded_refusal(target: &str, cover: &str) -> String {
    format!(
        "click refused: {target} is covered by {cover} at its click point, so a click there \
         would hit that instead. Dismiss the overlay (a cookie banner, a modal backdrop, a \
         sticky header) and click again, or add --allow-dom to click the covered element \
         through the DOM (element.click(), isTrusted=false; pages that only honour real \
         input ignore it)."
    )
}

/// What covers the selector's centre, when a coordinate click there would land
/// on something OTHER than the element (an overlay on top). `None` when not
/// occluded, the element is missing, or the probe fails (so we never block a
/// click on a flaky probe — the normal coordinate path runs).
async fn point_misses_element(
    client: &CdpClient,
    session_id: &str,
    selector: &str,
) -> Option<String> {
    if super::element::semantic_pin_active(selector) {
        let object = match super::element::resolve_semantic_pin(client, session_id).await {
            Ok(o) => o,
            Err(_) => return Some("semantic target changed".into()),
        };
        let reply=client.send_command("Runtime.callFunctionOn",Some(serde_json::json!({"objectId":object,"functionDeclaration":"function(){const r=this.getBoundingClientRect();const hit=this.ownerDocument.elementFromPoint(r.x+r.width/2,r.y+r.height/2);return hit===this || this.contains(hit);}","returnByValue":true})),Some(session_id)).await;
        return match reply {
            Ok(r) if r["result"]["value"] == true => None,
            _ => Some("semantic target is covered".into()),
        };
    }
    let js = format!(
        r#"(() => {{
            const el = document.querySelector({sel});
            if (!el) return false;
            const r = el.getBoundingClientRect();
            if (r.width === 0 || r.height === 0) return false;
            const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
            if (!hit) return false;
            // Not occluded if the hit is the element, a descendant, or an ancestor
            // wrapper (clicking those still reaches the element's handlers).
            if (hit === el || el.contains(hit) || hit.contains(el)) return false;
            let d = '<' + hit.tagName.toLowerCase();
            if (hit.id) d += ' id="' + hit.id + '"';
            const cls = typeof hit.className === 'string' ? hit.className.trim().split(/\s+/).slice(0, 2).join(' ') : '';
            if (cls) d += ' class="' + cls + '"';
            d += '>';
            const t = (hit.innerText || '').trim().replace(/\s+/g, ' ').slice(0, 40);
            return t ? d + ' "' + t + '"' : d;
        }})()"#,
        sel = serde_json::to_string(selector).unwrap_or_default()
    );
    match client
        .send_command_typed::<_, EvaluateResult>(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: js,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await
    {
        Ok(r) => r
            .result
            .value
            .and_then(|v| v.as_str().map(String::from))
            .filter(|s| !s.is_empty()),
        Err(_) => None,
    }
}

/// Best-effort scroll-into-view before a coordinate click. Uses Chrome's
/// `scrollIntoViewIfNeeded` (only scrolls when not already fully visible),
/// falling back to centered `scrollIntoView`. Resolution failures are ignored —
/// the subsequent resolve will surface a real "not found" error.
async fn scroll_into_view_if_needed(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Option<String> {
    let Ok((object_id, effective_session_id)) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await
    else {
        return None;
    };
    let reply = client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: SCROLL_AND_PROBE_DISABLED_JS.to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await;
    // Let the scroll settle so the following getBoxModel sees final coordinates.
    wait_for_paint_settled(client, &effective_session_id).await;
    reply
        .ok()?
        .get("result")?
        .get("value")?
        .as_str()
        .map(String::from)
}

/// Scroll the element into view, then say whether it can take a click:
/// `"disabled"` for a form control the browser will not click (`:disabled`,
/// which also covers a `<fieldset disabled>` ancestor), `"aria-disabled"`
/// when it or an ancestor declares `aria-disabled="true"`, `""` otherwise.
const SCROLL_AND_PROBE_DISABLED_JS: &str = "function() { try { \
        if (typeof this.scrollIntoViewIfNeeded === 'function') { this.scrollIntoViewIfNeeded(true); } \
        else { this.scrollIntoView({ block: 'center', inline: 'center' }); } \
    } catch (e) {} \
    try { \
        if (this.matches && this.matches(':disabled')) return 'disabled'; \
        if (this.closest && this.closest('[aria-disabled=\"true\"]')) return 'aria-disabled'; \
    } catch (e) {} \
    return ''; }";

/// Dispatch a click through the DOM (`element.click()`) instead of via screen
/// coordinates. Targets the intended element directly, so it works when a
/// floating layer occludes the click point or the element sits in a portal that
/// confuses `elementFromPoint`. Used as the fallback for `click` and when
/// `AGENT_BROWSER_CLICK_MODE=dom`.
async fn dom_click(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
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
    show_cursor_travelling_to(client, session_id, &effective_session_id, &object_id).await;
    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: DOM_CLICK_WITH_FOCUS_JS.to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;
    wait_for_paint_settled(client, &effective_session_id).await;
    Ok(())
}

/// `element.click()` plus the focus move a REAL mouse click performs.
///
/// A synthetic `.click()` fires the click handlers but never moves keyboard
/// focus, so on the relay (where a left click is DOM-dispatched by default)
/// `click <input>` followed by `press Meta+a` / `press Backspace` landed on
/// whichever field was focused BEFORE the click — one step behind, and on a
/// half-filled form that select-all + delete wiped the wrong field (issue #204).
/// Mirror what a trusted click does: focus the nearest focusable element
/// (the input itself, or a focusable ancestor such as a `<label>`'s control /
/// a `[tabindex]` wrapper) unless the click handler already moved focus
/// elsewhere. Focus is deliberately NOT touched when nothing focusable is
/// involved: blurring the current field on a click into empty space would fire
/// blur-validation the page did not ask for.
const DOM_CLICK_WITH_FOCUS_JS: &str = r#"function() {
    const el = this;
    const doc = el.ownerDocument || document;
    const deepActive = () => {
        let ae = doc.activeElement;
        while (ae && ae.shadowRoot && ae.shadowRoot.activeElement) ae = ae.shadowRoot.activeElement;
        return ae;
    };
    const before = deepActive();
    el.click();
    const after = deepActive();
    if (after !== before) return 'moved-by-handler';
    const focusable = el.closest
        ? el.closest('input, textarea, select, button, a[href], [contenteditable=""], [contenteditable="true"], [tabindex], summary')
        : null;
    let target = focusable;
    if (!target && el.tagName === 'LABEL' && el.control) target = el.control;
    if (!target) return 'no-focusable';
    if (target.disabled) return 'disabled';
    if (target === after || (target.contains && target.contains(after))) return 'already';
    try { target.focus({ preventScroll: true }); } catch (e) { return 'focus-failed'; }
    return 'focused';
}"#;

/// Move the on-page cursor onto the element a DOM click is about to hit, then
/// mark the click — the visible half of what `Input.dispatchMouseEvent` gives
/// for free on the coordinate path.
///
/// The centre comes from the object we already resolved for the click, not from
/// a second `resolve_element_center`: that would re-run identity verification
/// and occlusion checks for a purely cosmetic overlay, and could resolve a
/// *different* node than the one being clicked if the page moved in between.
///
/// Skipped for an element inside an iframe: `getBoundingClientRect` there is
/// frame-local, and the overlay lives in the top document, so the cursor would
/// be drawn somewhere the element isn't. Best-effort throughout — no cursor is
/// always better than no click.
async fn show_cursor_travelling_to(
    client: &CdpClient,
    page_session_id: &str,
    effective_session_id: &str,
    object_id: &str,
) {
    // The overlay is an extension feature: on a browser we launched ourselves
    // there is nobody to answer `ABExt.driveCursor`.
    if CURSOR_STATE.load(std::sync::atomic::Ordering::Relaxed) == 2
        || effective_session_id != page_session_id
        || crate::connect::relay_url().is_none()
    {
        return;
    }
    let rect = client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: "function() { const r = this.getBoundingClientRect(); \
                     return { x: r.left + r.width / 2, y: r.top + r.height / 2 }; }"
                    .to_string(),
                object_id: Some(object_id.to_string()),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(effective_session_id),
        )
        .await;
    let Some(value) = rect
        .ok()
        .and_then(|v| v.get("result")?.get("value").cloned())
    else {
        return;
    };
    let (Some(x), Some(y)) = (
        value.get("x").and_then(Value::as_f64),
        value.get("y").and_then(Value::as_f64),
    ) else {
        return;
    };
    drive_cursor(client, page_session_id, x, y, false).await;
    tokio::time::sleep(std::time::Duration::from_millis(CURSOR_LEAD_MS)).await;
    drive_cursor(client, page_session_id, x, y, true).await;
}

/// Trusted activation of an element inside an iframe (issue #39). Focuses the
/// element in its own frame session, then dispatches a real Enter/Space on the
/// page session — Chrome routes the key to the focused element across frames, and
/// Enter/Space on a focused button/link/checkbox fires a `click` with
/// `isTrusted: true`, which security-sensitive embedded forms (Google Payments
/// `保存`) require. Non-activatable roles (a `div[onclick]`) can't be keyboard-
/// activated, so they fall back to a DOM `.click()`.
async fn dom_activate(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<bool, String> {
    let role = parse_ref(selector_or_ref)
        .and_then(|r| ref_map.get(&r).map(|e| e.role.clone()))
        .unwrap_or_default();
    // Space toggles checkbox-like controls; Enter activates buttons/links/menus.
    let key = match role.as_str() {
        "checkbox" | "radio" | "switch" | "option" | "menuitemcheckbox" | "menuitemradio" => {
            Some("space")
        }
        "button" | "link" | "menuitem" | "tab" | "treeitem" => Some("enter"),
        _ => None,
    };
    let Some(key) = key else {
        // Not keyboard-activatable — best effort via DOM .click() (untrusted).
        dom_click(
            client,
            session_id,
            ref_map,
            selector_or_ref,
            iframe_sessions,
        )
        .await?;
        return Ok(false);
    };

    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;
    // Focus the element in its OWN frame session so the keystroke lands on it.
    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: "function() { this.focus(); }".to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;
    // Trusted key on the page session — routed to the focused (in-frame) element.
    press_key(client, session_id, key).await?;
    wait_for_paint_settled(client, &effective_session_id).await;
    Ok(true)
}

/// DOM-dispatch a double-click on the element in its own session (no coordinates)
/// — the relay/iframe-safe counterpart to a coordinate dblclick. Fires the full
/// click,click,dblclick sequence so handlers bound to any of them respond.
async fn dom_dblclick(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
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
    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: r#"function() {
                    const opts = { bubbles: true, cancelable: true, view: window };
                    this.dispatchEvent(new MouseEvent('click', opts));
                    this.dispatchEvent(new MouseEvent('click', { ...opts, detail: 2 }));
                    this.dispatchEvent(new MouseEvent('dblclick', opts));
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
    wait_for_paint_settled(client, &effective_session_id).await;
    Ok(())
}

pub async fn dblclick(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    // Same relay/iframe drift hazard as a single click — DOM-dispatch the
    // double-click there instead of a coordinate one (issues #31/#36).
    if std::env::var("AGENT_BROWSER_CLICK_MODE").as_deref() != Ok("coord")
        && prefer_dom_dispatch(ref_map, selector_or_ref)
    {
        return dom_dblclick(
            client,
            session_id,
            ref_map,
            selector_or_ref,
            iframe_sessions,
        )
        .await;
    }
    click(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        "left",
        2,
        iframe_sessions,
    )
    .await
}

/// DOM-dispatch a hover (pointer/mouse enter+move) on the element in its own
/// session — reaches OOPIF elements and never drifts to the foreground tab over
/// the relay, unlike a coordinate `mouseMoved` (issues #31/#36).
async fn dom_hover(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
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
    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: r#"function() {
                    const r = this.getBoundingClientRect();
                    const cx = r.left + r.width / 2, cy = r.top + r.height / 2;
                    const base = { bubbles: true, cancelable: true, view: window, clientX: cx, clientY: cy };
                    this.dispatchEvent(new PointerEvent('pointerover', base));
                    this.dispatchEvent(new PointerEvent('pointerenter', { ...base, bubbles: false }));
                    this.dispatchEvent(new MouseEvent('mouseover', base));
                    this.dispatchEvent(new MouseEvent('mouseenter', { ...base, bubbles: false }));
                    this.dispatchEvent(new MouseEvent('mousemove', base));
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
    Ok(())
}

pub async fn hover(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    // Coordinate `mouseMoved` drifts to the foreground tab over the relay and
    // can't reach an OOPIF — DOM-dispatch the hover there (issues #31/#36).
    if prefer_dom_dispatch(ref_map, selector_or_ref) {
        return dom_hover(
            client,
            session_id,
            ref_map,
            selector_or_ref,
            iframe_sessions,
        )
        .await;
    }
    let (x, y, _w, _h, effective_session_id) = resolve_element_center(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;
    restore_rendering_if_hidden(client, &effective_session_id).await;
    client
        .send_command_typed::<_, Value>(
            "Input.dispatchMouseEvent",
            &DispatchMouseEventParams {
                event_type: "mouseMoved".to_string(),
                x,
                y,
                button: None,
                buttons: None,
                click_count: None,
                delta_x: None,
                delta_y: None,
                modifiers: None,
            },
            Some(&effective_session_id),
        )
        .await?;
    Ok(())
}

/// DOM-dispatch an HTML5 drag-and-drop from `source` to `target` in their shared
/// session — the relay/iframe-safe counterpart to the coordinate drag, which
/// drifts to the foreground tab over the relay and can't reach an OOPIF (issues
/// #31/#36). Covers HTML5 DnD (sortable lists, file/card boards); pointer-driven
/// drag (canvas, sliders) still needs the coordinate path. Errors if source and
/// target live in different frames — a synthetic cross-frame DnD isn't reliable.
pub async fn dom_drag(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    source: &str,
    target: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let (src_obj, src_session) =
        resolve_element_object_id(client, session_id, ref_map, source, iframe_sessions).await?;
    let (tgt_obj, tgt_session) =
        resolve_element_object_id(client, session_id, ref_map, target, iframe_sessions).await?;
    if src_session != tgt_session {
        return Err(
            "drag source and target are in different frames; cross-frame drag-and-drop over the \
             relay isn't supported — drag within a single frame, or use a launched browser with \
             AGENT_BROWSER_CLICK_MODE=coord"
                .to_string(),
        );
    }
    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: r#"function(target) {
                    const dt = new DataTransfer();
                    const ev = (type, el) => el.dispatchEvent(
                        new DragEvent(type, { bubbles: true, cancelable: true, dataTransfer: dt }));
                    ev('dragstart', this);
                    ev('drag', this);
                    ev('dragenter', target);
                    ev('dragover', target);
                    ev('drop', target);
                    ev('dragend', this);
                }"#
                .to_string(),
                object_id: Some(src_obj),
                arguments: Some(vec![CallArgument {
                    value: None,
                    object_id: Some(tgt_obj),
                }]),
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&src_session),
        )
        .await?;
    wait_for_paint_settled(client, &src_session).await;
    Ok(())
}

pub async fn fill(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    value: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<String, String> {
    fill_reporting(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        value,
        iframe_sessions,
    )
    .await
    .map(|outcome| outcome.engine)
}

/// What `fill` did, beyond "the field now holds the value".
pub struct FillOutcome {
    /// The path that wrote the value (`input`, `contenteditable`, `monaco`, ...).
    pub engine: String,
    /// Set when the value is in the field but was written in a way some pages
    /// do not register (issue #358): the caller must surface it, not print `✓`.
    pub warning: Option<String>,
}

/// Input types whose value a trusted `Input.insertText` can replace: they
/// support `select()` and take typed text. Date/time/color/range pickers,
/// checkboxes and file inputs have no text to select, so they keep the
/// native-setter path.
const TRUSTED_FILL_INPUT_TYPES: &str =
    "['text', 'search', 'url', 'tel', 'email', 'password', 'number']";

/// `fill` for plain `<input>`/`<textarea>` used to write the value with the
/// prototype setter and dispatch synthetic `input`/`change` events. Those
/// events carry `isTrusted: false`. React and Vue accept them, but a page that
/// only honours real input (checks `event.isTrusted`, or listens for
/// `beforeinput`, which the old path never fired) saw nothing: the field
/// displayed the new text, the read-back matched, `fill` printed `✓`, and the
/// page's own state (the dialog's Save button, the value it submits) never
/// changed (issue #358, LinkedIn's edit-intro dialog).
///
/// So the text-like fields now go the way a user's edit goes: focus, select
/// the current value, then CDP `Input.insertText`, which replaces the
/// selection and fires trusted `beforeinput` + `input`; the blur that follows
/// fires a trusted `change`. The synthetic path remains the fallback when the
/// trusted insert does not produce the value (a `maxlength` shorter than the
/// value, an input mask), and then the caller is told.
pub async fn fill_reporting(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    value: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<FillOutcome, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;
    let first = fill_object(client, &effective_session_id, &object_id, value).await;
    let err = match first {
        Err(e) if is_stale_object_error(&e) => e,
        other => return other,
    };

    // The remote object the fill was working on stopped existing part-way
    // through (observed on the relay as "Could not find object with given id"
    // after the trusted insert had already landed). The fill may or may not
    // have written the value, so a blind replay could type it twice. Re-resolve
    // the element once and READ it first: if the value is already there,
    // report that instead of touching the field again; only a field that does
    // not hold it gets one more fill, on the fresh handle.
    let (fresh_id, fresh_session) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await
    .map_err(|e2| {
        format!("{err} (the element handle went stale mid-fill; re-resolving it failed: {e2})")
    })?;
    let note = "the element handle went stale mid-fill (the page's remote object was \
                discarded); the element was re-resolved";
    match read_value_of(client, &fresh_session, &fresh_id).await {
        Some(current) if current == value => Ok(FillOutcome {
            // Nothing was written on this pass; the value was read back.
            engine: "reread".to_string(),
            warning: Some(format!(
                "{note} and already holds the requested value, so it was not re-typed. \
                 Confirm any page state that depends on the input events before relying on it"
            )),
        }),
        // Unknown is not "different": a field that cannot be read back may
        // already hold the value, and writing again could type it twice.
        None => Err(format!(
            "{err} (the element handle went stale mid-fill; the field's value is unknown: it \
             could not be read back after the element was re-resolved, so nothing was written \
             again. Check it with `get value {selector_or_ref}` before repeating the fill)"
        )),
        Some(_) => {
            let mut outcome = fill_object(client, &fresh_session, &fresh_id, value)
                .await
                .map_err(|e2| format!("{err} (re-resolved once and filled again: {e2})"))?;
            outcome.warning = Some(join_warnings(
                format!("{note}, did not hold the value, and was filled once more"),
                outcome.warning.take(),
            ));
            Ok(outcome)
        }
    }
}

/// A CDP error meaning a remote object id we hold is no longer valid: the
/// inspector session or execution context that minted it is gone. The element
/// itself may well still be there under a fresh handle.
pub fn is_stale_object_error(e: &str) -> bool {
    e.contains("Could not find object with given id") || e.contains("Invalid remote object id")
}

/// [`fill_reporting`] for an element already resolved to a remote object on
/// `session_id`, e.g. one found in a child frame's isolated world (`auth
/// login` in a sign-in iframe, #449).
pub async fn fill_object(
    client: &CdpClient,
    session_id: &str,
    object_id: &str,
    value: &str,
) -> Result<FillOutcome, String> {
    let object_id = object_id.to_string();
    let effective_session_id = session_id.to_string();
    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: fill_function(value, true),
                object_id: Some(object_id.clone()),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    if let Some(ex) = result.exception_details {
        return Err(format!("fill failed: {}", ex.text));
    }

    let mut engine = result
        .result
        .value
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "input".to_string());

    if engine == "input-trusted" {
        return fill_input_trusted(client, &effective_session_id, &object_id, value).await;
    }

    finish_fill(
        client,
        &effective_session_id,
        &object_id,
        value,
        &mut engine,
    )
    .await
    .map(|warning| FillOutcome { engine, warning })
}

/// The trusted half of [`fill_reporting`] for a text `<input>`/`<textarea>`
/// that the page script has already focused and selected.
async fn fill_input_trusted(
    client: &CdpClient,
    session_id: &str,
    object_id: &str,
    value: &str,
) -> Result<FillOutcome, String> {
    let inserted = client
        .send_command_typed::<_, Value>(
            "Input.insertText",
            &InsertTextParams {
                text: value.to_string(),
            },
            Some(session_id),
        )
        .await;
    // An empty insert does not delete a selection; Delete does, with the same
    // trusted beforeinput/input a user's key produces.
    let inserted = match inserted {
        Ok(_) if value.is_empty() => press_key(client, session_id, "Delete").await,
        other => other.map(|_| ()),
    };
    if inserted.is_ok() {
        let _ = call_on(client, session_id, object_id, FILL_TRUSTED_TAIL_JS).await;
        if let Ok(actual) = verify_fill_value(client, session_id, object_id, value, "input").await {
            return Ok(FillOutcome {
                engine: "input".to_string(),
                warning: reformatted_warning(value, &actual),
            });
        }
    }
    let trusted_read = read_value_of(client, session_id, object_id).await;

    // The trusted insert did not leave the value in the field. Write it the
    // old way so the field still ends up holding what was asked for, but say
    // so: a page that ignores untrusted input will not have registered it.
    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: fill_function(value, false),
                object_id: Some(object_id.to_string()),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;
    if let Some(ex) = result.exception_details {
        return Err(format!("fill failed: {}", ex.text));
    }
    let mut engine = "input".to_string();
    let formatted = finish_fill(client, session_id, object_id, value, &mut engine).await?;
    // Never echo a card number or password in the explanation (#372).
    let sensitive = Box::pin(super::sensitive::is_sensitive_object(
        client, session_id, object_id,
    ))
    .await;
    let why = match (&inserted, trusted_read) {
        (Err(e), _) => format!("the trusted insert failed ({e})"),
        (Ok(()), Some(actual)) => format!(
            "typing it the way a user does left {} in the field (a maxlength, mask or \
             formatter rewrote it)",
            if sensitive {
                super::sensitive::masked(&actual)
            } else {
                quote_short(&actual)
            }
        ),
        (Ok(()), None) => "typing it the way a user does did not produce it".to_string(),
    };
    Ok(FillOutcome {
        engine: format!("{engine}-synthetic"),
        warning: Some(join_warnings(
            format!(
            "the field holds the value, but {why}, so it was written with the value setter and \
             synthetic events (isTrusted=false). A page that only honours real input will not \
             have registered it; check the page's own state (e.g. a Save button enabling) \
             before relying on it"
            ),
            formatted,
        )),
    })
}

/// After a trusted insert: leave the field the way a user leaving it would.
/// `blur()` fires a trusted `change` + `focusout` (the value changed since
/// focus), which is what blur-triggered validation and lookups listen for;
/// then focus goes back to the field when the blur left it on `<body>`, so a
/// following `press Enter` still reaches it (#167).
const FILL_TRUSTED_TAIL_JS: &str = r#"function() {
    const el = this;
    try { el.blur(); } catch (e) {}
    try {
        const doc = el.ownerDocument;
        let ae = doc && doc.activeElement;
        while (ae && ae.shadowRoot && ae.shadowRoot.activeElement) ae = ae.shadowRoot.activeElement;
        if (!ae || ae === doc.body || ae === doc.documentElement) el.focus({ preventScroll: true });
    } catch (e) {}
    return true;
}"#;

async fn call_on(
    client: &CdpClient,
    session_id: &str,
    object_id: &str,
    function_declaration: &str,
) -> Result<EvaluateResult, String> {
    client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: function_declaration.to_string(),
                object_id: Some(object_id.to_string()),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await
}

async fn read_value_of(client: &CdpClient, session_id: &str, object_id: &str) -> Option<String> {
    let result = call_on(
        client,
        session_id,
        object_id,
        &read_editable_value_function(),
    )
    .await
    .ok()?;
    let data = result.result.value?;
    if !data.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    Some(data.get("value").and_then(Value::as_str)?.to_string())
}

/// The page-side half of `fill`. With `allow_trusted`, a text-like
/// `<input>`/`<textarea>` is only focused and selected and `'input-trusted'`
/// comes back, telling the caller to insert the text through CDP; without it,
/// the value goes in through the prototype setter and synthetic events.
fn fill_function(value: &str, allow_trusted: bool) -> String {
    // Emulate a real edit so framework-controlled inputs (React/Vue) and
    // site-side listeners actually see the change (issue #25): set the value
    // through the element's PROTOTYPE setter (which React's _valueTracker hooks),
    // then dispatch input → change → blur/focusout. Beyond plain inputs, detect
    // rich editors and use their own API/events (issue #41): CodeMirror 5 and
    // Monaco have a model that `.value`/`textContent` can't touch; ProseMirror /
    // contenteditable need `execCommand('insertText')` so beforeinput/input fire
    // (a raw `textContent =` corrupts PM's doc and skips React composers).
    // Returns the engine used so the caller can report it. `type <sel> <text>`
    // remains for sites that need per-keystroke events.
    format!(
        r#"function() {{
            let el = this;
            const v = {val};
            const monacoCandidates = {monaco_candidates};
            const monacoRoot = (el.closest && el.closest('.monaco-editor'))
                || (el.querySelector && el.querySelector('.monaco-editor'));

            // Monaco: only use its authoritative model API. The hidden
            // textarea.inputarea surfaced by the accessibility tree is an input
            // transport, not the model. Synthetic paste events on it are untrusted
            // and may be ignored while still looking successful (#138).
            if (monacoRoot) {{
                for (const api of monacoCandidates()) {{
                    try {{
                        const editors = api.getEditors ? api.getEditors() : [];
                        const editor = editors.find(candidate => {{
                            const node = candidate.getDomNode && candidate.getDomNode();
                            return node && (node === monacoRoot || node.contains(el));
                        }});
                        if (editor && editor.setValue && editor.getValue) {{
                            editor.setValue(v);
                            return 'monaco';
                        }}

                        const models = api.getModels ? api.getModels() : [];
                        const roots = document.querySelectorAll('.monaco-editor');
                        if (models.length === 1 && roots.length === 1
                            && models[0].setValue && models[0].getValue) {{
                            models[0].setValue(v);
                            return 'monaco';
                        }}
                    }} catch (e) {{}}
                }}
                return 'monaco-unsupported';
            }}

            // If the ref anchored a WRAPPER rather than the field itself (common when
            // a controlled input lives inside a shadow/portal and snapshot pinned the
            // host), retarget to the nested editable so the native setter lands on the
            // real input instead of no-op'ing on a div (#105.2).
            const editable = n => n && (n.tagName === 'INPUT' || n.tagName === 'TEXTAREA' || n.tagName === 'SELECT' || n.isContentEditable);
            if (!editable(el) && el.querySelector) {{
                const inner = el.querySelector('input, textarea, select, [contenteditable]');
                if (inner) el = inner;
            }}
            const tag = el.tagName;
            try {{ el.focus(); }} catch (e) {{}}
            const fire = (type, ctor) => el.dispatchEvent(new (ctor || Event)(type, {{ bubbles: true }}));

            // CodeMirror 5: a hidden <textarea> inside .CodeMirror with a live instance.
            const cm5 = el.closest && el.closest('.CodeMirror');
            if (cm5 && cm5.CodeMirror) {{ cm5.CodeMirror.setValue(v); return 'codemirror5'; }}

            if (tag === 'SELECT') {{ el.value = v; fire('input'); fire('change'); return 'select'; }}

            if (el.isContentEditable) {{
                // Rich React composers (DraftJS / Lexical / ProseMirror) only commit
                // an edit — and only flip their send button to enabled — from a
                // TRUSTED beforeinput/input. execCommand('insertText') fires UNtrusted
                // events: the text appears but the editor's model (and React's button
                // state) never updates, so e.g. X's "Post" / Reddit's submit stay
                // disabled. So here we just focus + select-all; the Rust caller follows
                // up with CDP Input.insertText (trusted), which replaces the selection
                // and fires the events these editors honor. Returns 'contenteditable'
                // to signal the caller to do the trusted insert.
                try {{
                    const sel = window.getSelection();
                    const range = document.createRange();
                    range.selectNodeContents(el);
                    sel.removeAllRanges();
                    sel.addRange(range);
                }} catch (e) {{}}
                return 'contenteditable';
            }}

            // A user's edit: select what is there so the caller's trusted
            // Input.insertText replaces it (issue #358). Read-only and disabled
            // fields take no typed input, so they keep the setter path below.
            const kind = tag === 'TEXTAREA' ? 'textarea' : String(el.type || 'text').toLowerCase();
            // Input.insertText goes to whatever has focus, so only take this
            // path when the focus call above actually left focus on the field;
            // a focus handler that moves it (a combobox popping a search box)
            // would otherwise receive the text.
            let deep = el.ownerDocument && el.ownerDocument.activeElement;
            while (deep && deep.shadowRoot && deep.shadowRoot.activeElement) deep = deep.shadowRoot.activeElement;
            if ({allow_trusted} && (tag === 'TEXTAREA' || {trusted_types}.includes(kind))
                && !el.readOnly && !el.disabled && deep === el) {{
                try {{ el.select(); }} catch (e) {{}}
                return 'input-trusted';
            }}

            const proto = tag === 'TEXTAREA' ? window.HTMLTextAreaElement.prototype
                                             : window.HTMLInputElement.prototype;
            const desc = Object.getOwnPropertyDescriptor(proto, 'value');
            const set = desc && desc.set ? (x) => desc.set.call(el, x) : (x) => {{ el.value = x; }};
            set('');                                   // reset the framework tracker
            fire('input', window.InputEvent || Event);
            set(v);                                    // native setter → React/Vue registers
            fire('input', window.InputEvent || Event);
            fire('change');
            try {{ el.blur(); }} catch (e) {{}}
            fire('focusout');                          // blur-triggered lookups/validation
            // ...then put focus BACK on the field. The blur above left
            // document.activeElement on <body>, so the very next `press Enter`
            // was dispatched at the document and the form never submitted —
            // while press still reported success (#167). Only restore when the
            // blur didn't hand focus to something else: a page that
            // deliberately advances focus on blur keeps its own target.
            // Resolve the DEEPEST active element from the document, not from
            // el's own root: a blur handler that moves focus into a *different*
            // shadow root leaves el's root empty, and restoring off that would
            // steal the focus the page just placed.
            try {{
                const doc = el.ownerDocument;
                let ae = doc && doc.activeElement;
                while (ae && ae.shadowRoot && ae.shadowRoot.activeElement) {{
                    ae = ae.shadowRoot.activeElement;
                }}
                if (!ae || ae === doc.body || ae === doc.documentElement) {{
                    el.focus({{ preventScroll: true }});
                }}
            }} catch (e) {{}}
            return 'input';
        }}"#,
        val = serde_json::to_string(value).unwrap_or_default(),
        monaco_candidates = MONACO_CANDIDATES_FUNCTION,
        allow_trusted = allow_trusted,
        trusted_types = TRUSTED_FILL_INPUT_TYPES,
    )
}

/// The state of the control that would commit a field's form or dialog —
/// its Save / Submit / 保存 button — as seen from that field.
#[derive(Debug, Clone, PartialEq)]
pub struct CommitControl {
    /// The button's accessible-ish name (text, `aria-label` or `value`).
    pub name: String,
    pub disabled: bool,
    /// Other required fields in the same container that are still empty. A
    /// Save that waits on those is disabled for a reason unrelated to the fill.
    pub other_empty_required: u64,
}

/// Find the field's form or dialog and its commit button. Returns `null` when
/// the field is in neither, or no button there looks like a commit.
const COMMIT_CONTROL_JS: &str = r#"function() {
    const field = this;
    const box = field.closest && field.closest('form, dialog, [role="dialog"], [role="alertdialog"], [aria-modal="true"]');
    if (!box) return null;
    const label = b => ((b.getAttribute('aria-label') || b.textContent || b.value || '') + '').replace(/\s+/g, ' ').trim();
    const commit = /^(save|submit|done|apply|update|confirm|post|send|publish|保存|提交|完成|确定|确认|应用|更新|发布|发送|送信|完了|登録)/i;
    const visible = b => { const r = b.getBoundingClientRect(); return r.width > 0 && r.height > 0; };
    const buttons = Array.from(box.querySelectorAll('button, input[type="submit"], [role="button"]')).filter(visible);
    const pick = buttons.find(b => (b.type === 'submit' && b.tagName !== 'BUTTON') || commit.test(label(b)))
        || buttons.find(b => b.getAttribute('type') === 'submit');
    if (!pick) return null;
    const disabled = !!((pick.matches && pick.matches(':disabled')) || pick.getAttribute('aria-disabled') === 'true');
    let otherEmpty = 0;
    for (const f of box.querySelectorAll('input, textarea, select, [contenteditable="true"]')) {
        if (f === field || !visible(f)) continue;
        const required = f.required || f.getAttribute('aria-required') === 'true';
        const value = f.isContentEditable ? f.textContent : f.value;
        if (required && !(value || '').trim()) otherEmpty++;
    }
    return { name: label(pick).slice(0, 80), disabled, otherEmptyRequired: otherEmpty };
}"#;

/// Read [`CommitControl`] for the field `selector_or_ref` names. `None` when
/// there is no such control or the probe fails — the diagnostic is
/// best-effort and must never fail the fill it annotates.
pub async fn commit_control_state(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Option<CommitControl> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await
    .ok()?;
    let result = call_on(client, &effective_session_id, &object_id, COMMIT_CONTROL_JS)
        .await
        .ok()?;
    let v = result.result.value?;
    Some(CommitControl {
        name: sanitize_descriptor(v.get("name")?.as_str()?),
        disabled: v.get("disabled")?.as_bool()?,
        other_empty_required: v.get("otherEmptyRequired")?.as_u64()?,
    })
}

/// The warning `fill` attaches when the field now holds the value but the
/// form's commit button did not react: disabled before, still disabled after,
/// with no other required field left empty to explain it. That is the
/// signature of a page whose own state never saw the edit (issue #358).
pub(crate) fn commit_control_warning(
    before: Option<&CommitControl>,
    after: Option<&CommitControl>,
) -> Option<String> {
    let (before, after) = (before?, after?);
    if !(before.disabled && after.disabled) || after.other_empty_required > 0 {
        return None;
    }
    Some(format!(
        "the field holds the value, but {:?} in the same form/dialog was disabled before the fill \
         and still is: the page did not react to the edit, so its own state may not hold the \
         value and saving will do nothing. Try `type --key-events`, and check the button again \
         with `snapshot` before clicking it",
        after.name
    ))
}

/// Everything `fill` does after the page script ran: the Monaco clipboard
/// fallback, the trusted contenteditable insert, and the read-back.
async fn finish_fill(
    client: &CdpClient,
    session_id: &str,
    object_id: &str,
    value: &str,
    engine: &mut String,
) -> Result<Option<String>, String> {
    let effective_session_id = session_id.to_string();
    let object_id = object_id.to_string();
    if engine.as_str() == "monaco-unsupported" {
        *engine =
            fill_monaco_via_clipboard(client, &effective_session_id, &object_id, value).await?;
        return Ok(None);
    }

    // Contenteditable rich editors (DraftJS / Lexical / ProseMirror): the JS above
    // only focused + selected-all. Do the actual edit through CDP so the events are
    // TRUSTED — that is what flips X's "Post" / Reddit's submit out of the disabled
    // state (execCommand's untrusted events don't). Input.insertText replaces the
    // selected (all) content; an empty value replaces the selection with nothing,
    // i.e. clears the field (verified: fill replaces, and fill "" clears).
    if engine.as_str() == "contenteditable" {
        // Trusted insert replaces the (all-)selected content. An empty value
        // replaces the selection with nothing, i.e. clears the field.
        let inserted = client
            .send_command_typed::<_, Value>(
                "Input.insertText",
                &InsertTextParams {
                    text: value.to_string(),
                },
                Some(&effective_session_id),
            )
            .await
            .map(|_| ());

        match inserted {
            Ok(()) => {
                // input/beforeinput already fired (trusted). Fire change + focusout
                // for parity with the plain-input path; don't blur (would collapse
                // composers that close on blur).
                let _ = client
                    .send_command_typed::<_, Value>(
                        "Runtime.callFunctionOn",
                        &CallFunctionOnParams {
                            function_declaration: "function() { try { this.dispatchEvent(new Event('change', { bubbles: true })); this.dispatchEvent(new Event('focusout', { bubbles: true })); } catch (e) {} }".to_string(),
                            object_id: Some(object_id.clone()),
                            arguments: None,
                            return_by_value: Some(true),
                            await_promise: Some(false),
                        },
                        Some(&effective_session_id),
                    )
                    .await;
            }
            Err(_) => {
                // CDP insert unavailable (rare: some Electron webviews). Fall back to
                // the old untrusted execCommand path so the field still fills.
                let fallback_js = format!(
                    r#"function() {{
                        const el = this; const v = {val};
                        let ok = false;
                        try {{ ok = document.execCommand('insertText', false, v); }} catch (e) {{}}
                        if (!ok) {{ el.textContent = v; el.dispatchEvent(new (window.InputEvent || Event)('input', {{ bubbles: true }})); }}
                        el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                        return ok ? 'contenteditable' : 'contenteditable-fallback';
                    }}"#,
                    val = serde_json::to_string(value).unwrap_or_default()
                );
                let fb: EvaluateResult = client
                    .send_command_typed(
                        "Runtime.callFunctionOn",
                        &CallFunctionOnParams {
                            function_declaration: fallback_js,
                            object_id: Some(object_id.clone()),
                            arguments: None,
                            return_by_value: Some(true),
                            await_promise: Some(false),
                        },
                        Some(&effective_session_id),
                    )
                    .await?;
                *engine = fb
                    .result
                    .value
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_else(|| "contenteditable-fallback".to_string());
            }
        }
    }

    // A control whose `onFocus` resets its own state wipes what we just wrote.
    // The fill path deliberately blurs (so blur-triggered validation and
    // lookups run) and then restores focus (#167) — and that restore is what
    // re-fires `onFocus`. On a hand-rolled combobox that clears its query on
    // focus, the value is gone by the time we read it back (issue #280).
    //
    // So: one re-apply, without touching focus, and then the SAME verification.
    // Nothing is reported as filled that the field does not actually hold.
    match verify_fill_value(
        client,
        &effective_session_id,
        &object_id,
        value,
        engine.as_str(),
    )
    .await
    {
        Ok(actual) => Ok(reformatted_warning(value, &actual)),
        Err(first) => {
            if !value.is_empty() && first.contains("read back an empty value") {
                let reapplied = reapply_value_without_focus_change(
                    client,
                    &effective_session_id,
                    &object_id,
                    value,
                )
                .await
                .is_ok();
                if !reapplied {
                    return Err(first);
                }
                let actual = verify_fill_value(
                    client,
                    &effective_session_id,
                    &object_id,
                    value,
                    engine.as_str(),
                )
                .await?;
                // Say which path produced the value: a control that needed this is
                // one whose focus handler fights writes, and the caller may need to
                // know that before pressing Enter into it.
                *engine = format!("{engine}+refocus-reset");
                return Ok(reformatted_warning(value, &actual));
            }
            Err(first)
        }
    }
}

/// Write the value once more with the native setter, firing `input`/`change`
/// and leaving focus exactly where it is.
///
/// Deliberately no blur and no focus restore: those already ran once, and
/// repeating them is what erased the value in the first place.
async fn reapply_value_without_focus_change(
    client: &CdpClient,
    session_id: &str,
    object_id: &str,
    value: &str,
) -> Result<(), String> {
    let js = format!(
        r#"function() {{
            const el = this;
            const v = {v};
            const tag = el.tagName;
            if (tag !== 'INPUT' && tag !== 'TEXTAREA') {{
                el.textContent = v;
                el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                return true;
            }}
            const proto = tag === 'TEXTAREA' ? window.HTMLTextAreaElement.prototype
                                             : window.HTMLInputElement.prototype;
            const desc = Object.getOwnPropertyDescriptor(proto, 'value');
            if (desc && desc.set) {{ desc.set.call(el, v); }} else {{ el.value = v; }}
            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
            return true;
        }}"#,
        v = serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string()),
    );
    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: js,
                object_id: Some(object_id.to_string()),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await
        .map_err(|e| e.to_string())?;
    if let Some(ex) = result.exception_details {
        return Err(ex.text);
    }
    Ok(())
}

async fn fill_monaco_via_clipboard(
    client: &CdpClient,
    session_id: &str,
    object_id: &str,
    expected: &str,
) -> Result<String, String> {
    const TRANSACTION_KEY: &str = "__chromeUseMonacoClipboardFill";
    let prepare_js = format!(
        r#"async function() {{
            const anchor = this;
            const root = (anchor.closest && anchor.closest('.monaco-editor'))
                || (anchor.querySelector && anchor.querySelector('.monaco-editor'));
            const input = root && root.querySelector('textarea.inputarea');
            if (!input) return {{ ok: false, error: 'Monaco textarea.inputarea was not found' }};

            const clipboard = input.ownerDocument.defaultView.navigator.clipboard;
            if (!clipboard || !clipboard.read || !clipboard.readText
                || !clipboard.write || !clipboard.writeText) {{
                return {{ ok: false, error: 'browser clipboard read/write APIs are unavailable' }};
            }}

            let backup;
            let backupText;
            try {{
                backup = await clipboard.read();
                backupText = await clipboard.readText();
            }} catch (error) {{
                return {{
                    ok: false,
                    error: 'browser clipboard backup was denied: ' + String(error && error.message || error)
                }};
            }}

            const state = {{ paste: null, copy: null }};
            const onPaste = event => {{
                queueMicrotask(() => {{
                    state.paste = {{
                        trusted: event.isTrusted,
                        prevented: event.defaultPrevented
                    }};
                }});
            }};
            const onCopy = event => {{
                queueMicrotask(() => {{
                    state.copy = {{
                        trusted: event.isTrusted,
                        prevented: event.defaultPrevented,
                        text: event.clipboardData ? event.clipboardData.getData('text/plain') : ''
                    }};
                }});
            }};
            input.addEventListener('paste', onPaste);
            input.addEventListener('copy', onCopy);
            anchor[{key}] = {{ input, clipboard, backup, backupText, state, onPaste, onCopy }};

            try {{
                await clipboard.writeText({value});
            }} catch (error) {{
                input.removeEventListener('paste', onPaste);
                input.removeEventListener('copy', onCopy);
                delete anchor[{key}];
                return {{
                    ok: false,
                    error: 'browser clipboard staging was denied: ' + String(error && error.message || error)
                }};
            }}

            input.focus();
            return {{ ok: true }};
        }}"#,
        key = serde_json::to_string(TRANSACTION_KEY).unwrap_or_default(),
        value = serde_json::to_string(expected).unwrap_or_default(),
    );

    let prepared: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: prepare_js,
                object_id: Some(object_id.to_string()),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(true),
            },
            Some(session_id),
        )
        .await?;
    if let Some(ex) = prepared.exception_details {
        return Err(format!("Monaco clipboard fill setup failed: {}", ex.text));
    }
    let prepared_data = prepared.result.value.unwrap_or(Value::Null);
    if !prepared_data
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let reason = prepared_data
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("clipboard transaction could not start");
        return Err(format!(
            "fill cannot safely replace this Monaco editor because its model API is not \
             accessible and {reason}; no text was written"
        ));
    }

    let modifier = if cfg!(target_os = "macos") { 4 } else { 2 };
    let operation = async {
        dispatch_editor_command(client, session_id, "a", modifier, "selectAll").await?;
        dispatch_editor_command(client, session_id, "v", modifier, "paste").await?;
        wait_for_paint_settled(client, session_id).await;

        let paste_check: EvaluateResult = client
            .send_command_typed(
                "Runtime.callFunctionOn",
                &CallFunctionOnParams {
                    function_declaration: format!(
                        "function() {{ return this[{}]?.state.paste || null; }}",
                        serde_json::to_string(TRANSACTION_KEY).unwrap_or_default()
                    ),
                    object_id: Some(object_id.to_string()),
                    arguments: None,
                    return_by_value: Some(true),
                    await_promise: Some(false),
                },
                Some(session_id),
            )
            .await?;
        let paste = paste_check.result.value.unwrap_or(Value::Null);
        if !paste
            .get("trusted")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || !paste
                .get("prevented")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            return Err(format!(
                "Monaco clipboard paste was not handled as a trusted editor operation \
                 (trusted={}, prevented={})",
                paste
                    .get("trusted")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                paste
                    .get("prevented")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            ));
        }

        dispatch_editor_command(client, session_id, "a", modifier, "selectAll").await?;
        dispatch_editor_command(client, session_id, "c", modifier, "copy").await?;

        let copy_check: EvaluateResult = client
            .send_command_typed(
                "Runtime.callFunctionOn",
                &CallFunctionOnParams {
                    function_declaration: format!(
                        "function() {{ return this[{}]?.state.copy || null; }}",
                        serde_json::to_string(TRANSACTION_KEY).unwrap_or_default()
                    ),
                    object_id: Some(object_id.to_string()),
                    arguments: None,
                    return_by_value: Some(true),
                    await_promise: Some(false),
                },
                Some(session_id),
            )
            .await?;
        let copy = copy_check.result.value.unwrap_or(Value::Null);
        if !copy
            .get("trusted")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || !copy
                .get("prevented")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            return Err(
                "Monaco clipboard readback was not produced by the editor model".to_string(),
            );
        }

        let actual = copy.get("text").and_then(Value::as_str).unwrap_or("");
        let normalized_expected = expected.replace("\r\n", "\n");
        let normalized_actual = actual.replace("\r\n", "\n");
        if normalized_actual != normalized_expected {
            let detail = if actual.is_empty() && !expected.is_empty() {
                "read back an empty value".to_string()
            } else {
                format!(
                    "read back {} characters after writing {}",
                    actual.chars().count(),
                    expected.chars().count()
                )
            };
            return Err(format!(
                "fill verification failed for monaco-clipboard: {detail}"
            ));
        }

        Ok(())
    }
    .await;

    let restore_js = format!(
        r#"async function() {{
            const tx = this[{key}];
            if (!tx) return {{ ok: false, error: 'clipboard transaction state was lost' }};
            tx.input.removeEventListener('paste', tx.onPaste);
            tx.input.removeEventListener('copy', tx.onCopy);
            delete this[{key}];
            try {{
                await tx.clipboard.write(tx.backup);
                return {{ ok: true }};
            }} catch (error) {{
                try {{ await tx.clipboard.writeText(tx.backupText); }} catch (ignored) {{}}
                return {{
                    ok: false,
                    error: 'original browser clipboard could not be fully restored: '
                        + String(error && error.message || error)
                }};
            }}
        }}"#,
        key = serde_json::to_string(TRANSACTION_KEY).unwrap_or_default(),
    );
    let restored: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: restore_js,
                object_id: Some(object_id.to_string()),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(true),
            },
            Some(session_id),
        )
        .await?;
    let restore_data = restored.result.value.unwrap_or(Value::Null);
    if !restore_data
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let reason = restore_data
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("original browser clipboard could not be restored");
        return Err(format!("Monaco clipboard fill failed: {reason}"));
    }

    operation?;
    Ok("monaco-clipboard".to_string())
}

pub(crate) async fn read_monaco_via_clipboard(
    client: &CdpClient,
    session_id: &str,
    object_id: &str,
) -> Result<String, String> {
    const TRANSACTION_KEY: &str = "__chromeUseMonacoClipboardRead";
    let prepare_js = format!(
        r#"async function() {{
            const anchor = this;
            const root = (anchor.closest && anchor.closest('.monaco-editor'))
                || (anchor.querySelector && anchor.querySelector('.monaco-editor'));
            const input = root && root.querySelector('textarea.inputarea');
            if (!input) return {{ ok: false, error: 'Monaco textarea.inputarea was not found' }};

            const clipboard = input.ownerDocument.defaultView.navigator.clipboard;
            if (!clipboard || !clipboard.read || !clipboard.readText
                || !clipboard.write || !clipboard.writeText) {{
                return {{ ok: false, error: 'browser clipboard read/write APIs are unavailable' }};
            }}

            let backup;
            let backupText;
            try {{
                backup = await clipboard.read();
                backupText = await clipboard.readText();
            }} catch (error) {{
                return {{
                    ok: false,
                    error: 'browser clipboard backup was denied: ' + String(error && error.message || error)
                }};
            }}

            const state = {{ copy: null }};
            const onCopy = event => {{
                queueMicrotask(() => {{
                    state.copy = {{
                        trusted: event.isTrusted,
                        prevented: event.defaultPrevented,
                        text: event.clipboardData ? event.clipboardData.getData('text/plain') : ''
                    }};
                }});
            }};
            input.addEventListener('copy', onCopy);
            anchor[{key}] = {{ input, clipboard, backup, backupText, state, onCopy }};
            input.focus();
            return {{ ok: true }};
        }}"#,
        key = serde_json::to_string(TRANSACTION_KEY).unwrap_or_default(),
    );

    let prepared: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: prepare_js,
                object_id: Some(object_id.to_string()),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(true),
            },
            Some(session_id),
        )
        .await?;
    if let Some(ex) = prepared.exception_details {
        return Err(format!("Monaco clipboard read setup failed: {}", ex.text));
    }
    let prepared_data = prepared.result.value.unwrap_or(Value::Null);
    if !prepared_data
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let reason = prepared_data
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("clipboard transaction could not start");
        return Err(format!("Monaco model API is not accessible and {reason}"));
    }

    let modifier = if cfg!(target_os = "macos") { 4 } else { 2 };
    let operation = async {
        dispatch_editor_command(client, session_id, "a", modifier, "selectAll").await?;
        dispatch_editor_command(client, session_id, "c", modifier, "copy").await?;

        let copy_check: EvaluateResult = client
            .send_command_typed(
                "Runtime.callFunctionOn",
                &CallFunctionOnParams {
                    function_declaration: format!(
                        "function() {{ return this[{}]?.state.copy || null; }}",
                        serde_json::to_string(TRANSACTION_KEY).unwrap_or_default()
                    ),
                    object_id: Some(object_id.to_string()),
                    arguments: None,
                    return_by_value: Some(true),
                    await_promise: Some(false),
                },
                Some(session_id),
            )
            .await?;
        let copy = copy_check.result.value.unwrap_or(Value::Null);
        if !copy
            .get("trusted")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || !copy
                .get("prevented")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            return Err(
                "Monaco clipboard readback was not produced by the editor model".to_string(),
            );
        }

        Ok(copy
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .replace("\r\n", "\n"))
    }
    .await;

    let restore_js = format!(
        r#"async function() {{
            const tx = this[{key}];
            if (!tx) return {{ ok: false, error: 'clipboard transaction state was lost' }};
            tx.input.removeEventListener('copy', tx.onCopy);
            delete this[{key}];
            try {{
                await tx.clipboard.write(tx.backup);
                return {{ ok: true }};
            }} catch (error) {{
                try {{ await tx.clipboard.writeText(tx.backupText); }} catch (ignored) {{}}
                return {{
                    ok: false,
                    error: 'original browser clipboard could not be fully restored: '
                        + String(error && error.message || error)
                }};
            }}
        }}"#,
        key = serde_json::to_string(TRANSACTION_KEY).unwrap_or_default(),
    );
    let restored: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: restore_js,
                object_id: Some(object_id.to_string()),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(true),
            },
            Some(session_id),
        )
        .await?;
    let restore_data = restored.result.value.unwrap_or(Value::Null);
    if !restore_data
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let reason = restore_data
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("original browser clipboard could not be restored");
        return Err(format!("Monaco clipboard read failed: {reason}"));
    }

    operation
}

async fn dispatch_editor_command(
    client: &CdpClient,
    session_id: &str,
    key: &str,
    modifiers: i32,
    command: &str,
) -> Result<(), String> {
    let (key_name, code, key_code) = named_key_info(key);
    client
        .send_command(
            "Input.dispatchKeyEvent",
            Some(serde_json::json!({
                "type": "keyDown",
                "key": key_name,
                "code": code,
                "windowsVirtualKeyCode": key_code,
                "nativeVirtualKeyCode": key_code,
                "modifiers": modifiers,
                "commands": [command],
            })),
            Some(session_id),
        )
        .await?;
    client
        .send_command(
            "Input.dispatchKeyEvent",
            Some(serde_json::json!({
                "type": "keyUp",
                "key": key_name,
                "code": code,
                "windowsVirtualKeyCode": key_code,
                "nativeVirtualKeyCode": key_code,
                "modifiers": modifiers,
            })),
            Some(session_id),
        )
        .await?;
    Ok(())
}

async fn verify_fill_value(
    client: &CdpClient,
    session_id: &str,
    object_id: &str,
    expected: &str,
    engine: &str,
) -> Result<String, String> {
    // Let framework-controlled inputs and editor models finish their synchronous
    // update plus the next paint before reading the authoritative value back.
    wait_for_paint_settled(client, session_id).await;

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: read_editable_value_function(),
                object_id: Some(object_id.to_string()),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;

    if let Some(ex) = result.exception_details {
        return Err(format!(
            "fill verification failed for {engine}: {}",
            ex.text
        ));
    }

    let data = result.result.value.unwrap_or(Value::Null);
    if !data.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        let reason = data
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("the edited value could not be read back");
        return Err(format!("fill verification failed for {engine}: {reason}"));
    }

    let actual = data
        .get("value")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("fill verification failed for {engine}: no text was read back"))?;

    // HTML text controls normalize CRLF to LF. Compare that standardized form
    // while preserving every other byte, including leading spaces in YAML.
    if !fill_values_match(expected, actual, engine) {
        // Card numbers, CVCs and passwords stay out of the error text (#372).
        let detail = if actual.is_empty() && !expected.is_empty() {
            // Keep this marker: `finish_fill` keys its refocus-reset recovery
            // on it. It carries no value.
            "read back an empty value after writing (value hidden)".to_string()
        } else if Box::pin(super::sensitive::is_sensitive_object(
            client, session_id, object_id,
        ))
        .await
        {
            format!(
                "the field holds {} chars after writing {} (values hidden: card / password field)",
                actual.chars().count(),
                expected.chars().count()
            )
        } else {
            fill_mismatch_detail(expected, actual)
        };
        return Err(format!("fill verification failed for {engine}: {detail}"));
    }

    Ok(actual.to_string())
}

/// Whether `actual` is `expected` as a formatting input shows it: the same
/// characters once spaces and the usual separators are dropped. Stripe turns
/// an expiry `1234` into `12 / 34` and a card number into groups of four
/// (#374); the value took, the field only displays it differently.
///
/// Single-line inputs only, and only when the page ADDED separators: a field
/// that lost characters (YAML indentation, a newline in an editor) is still a
/// failed fill.
fn same_after_formatting(expected: &str, actual: &str, engine: &str) -> bool {
    // Single-line only, on both sides: a newline the page inserted is a
    // change to the content, not formatting.
    if !engine.starts_with("input")
        || expected.contains(['\n', '\r'])
        || actual.contains(['\n', '\r'])
        || expected.is_empty()
    {
        return false;
    }
    let is_sep = |c: char| c.is_whitespace() || matches!(c, '/' | '-' | '.' | '(' | ')');
    // Every requested character must still be there, in order; the only
    // extra characters allowed are separators the page inserted. A dropped
    // sign or decimal point cannot be made up for by an added space.
    let mut want = expected.chars().peekable();
    for c in actual.chars() {
        if want.peek() == Some(&c) {
            want.next();
        } else if !is_sep(c) {
            return false;
        }
    }
    want.peek().is_none()
}

/// The synthetic-write note plus a reformatting note, when there is one.
fn join_warnings(main: String, extra: Option<String>) -> String {
    match extra {
        Some(e) => format!("{main}. Also: {e}"),
        None => main,
    }
}

/// A note for a fill the page reformatted, without echoing either value.
fn reformatted_warning(expected: &str, actual: &str) -> Option<String> {
    (expected.replace("\r\n", "\n") != actual.replace("\r\n", "\n")).then(|| {
        format!(
            "the page reformatted the value ({} chars written, the field shows {}); \
             compared ignoring spaces and separators",
            expected.chars().count(),
            actual.chars().count()
        )
    })
}

/// Describe a fill/type read-back mismatch so truncation is VISIBLE: what was
/// written, what the field holds now, and — when every non-ASCII character
/// vanished while the ASCII survived — that the page filtered the input (a
/// Latin-only address field, issue #203) rather than the keystrokes failing.
pub(crate) fn fill_mismatch_detail(expected: &str, actual: &str) -> String {
    let mut detail = if actual.is_empty() && !expected.is_empty() {
        format!(
            "read back an empty value after writing {}",
            quote_short(expected)
        )
    } else {
        format!(
            "read back {} ({} chars) after writing {} ({} chars)",
            quote_short(actual),
            actual.chars().count(),
            quote_short(expected),
            expected.chars().count()
        )
    };
    if non_ascii_was_dropped(expected, actual) {
        detail.push_str(
            ". Only the ASCII characters survived: the page rejected the non-Latin text (a \
             Latin-only / masked field), so re-typing won't help — check the field's input \
             rules or supply a romanized value",
        );
    }
    detail
}

/// True when `expected` carried non-ASCII characters and none of them made it
/// into `actual`, while `actual` is otherwise consistent with the ASCII part.
pub(crate) fn non_ascii_was_dropped(expected: &str, actual: &str) -> bool {
    let non_ascii: Vec<char> = expected.chars().filter(|c| !c.is_ascii()).collect();
    if non_ascii.is_empty() {
        return false;
    }
    !actual.chars().any(|c| non_ascii.contains(&c))
}

fn quote_short(s: &str) -> String {
    const MAX: usize = 60;
    if s.chars().count() <= MAX {
        format!("{s:?}")
    } else {
        let head: String = s.chars().take(MAX).collect();
        format!("{:?}…", head)
    }
}

/// How long `type` waits before reading the field a second time. A form that
/// is still hydrating right after load re-renders its inputs from state and
/// wipes what was just typed; the first read, one paint later, still shows the
/// text (zhihu.com/signin, #355).
const TYPE_REREAD_DELAY_MS: u64 = 250;

/// Verify a `type` against the field it targeted, filling `readBack` (and a
/// soft `warning`) into `out`. Silent when the value can't be read
/// (non-editable target, probe failure). `type` appends, so "holds what was
/// typed" is containment, not equality.
///
/// `before` is the field's value before typing (`Some("")` when the field was
/// cleared first). When the field ends up exactly where it started — nothing
/// typed survived — this is an error, not a warning: the input went somewhere
/// else, or the page threw it away, and `✓ Done` there sent agents off to
/// submit empty login forms (#355). A partial rewrite (mask, formatter, input
/// filter) stays a warning, as in #203.
#[allow(clippy::too_many_arguments)]
pub async fn verify_type_read_back(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    typed: &str,
    before: Option<&str>,
    iframe_sessions: &HashMap<String, String>,
    out: &mut Value,
) -> Result<(), String> {
    if typed.trim().is_empty() {
        return Ok(());
    }
    let Some(first) = read_editable_value(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await
    else {
        return Ok(());
    };
    let first_held = type_read_back_warning(typed, &first).is_none();
    let actual = if first_held {
        tokio::time::sleep(std::time::Duration::from_millis(TYPE_REREAD_DELAY_MS)).await;
        read_editable_value(
            client,
            session_id,
            ref_map,
            selector_or_ref,
            iframe_sessions,
        )
        .await
        .unwrap_or_else(|| first.clone())
    } else {
        first.clone()
    };
    out["readBack"] = json!(actual);

    let Some(warning) = type_read_back_warning(typed, &actual) else {
        return Ok(());
    };
    if type_left_field_unchanged(typed, before, &actual) {
        let focus = active_element_descriptor(client, session_id).await;
        return Err(type_not_kept_error(
            selector_or_ref,
            typed,
            &actual,
            first_held,
            focus.as_deref(),
        ));
    }
    out["warning"] = json!(warning);
    Ok(())
}

/// True when the field holds exactly what it held before `type` ran (or is
/// empty), i.e. none of the typed text survived. Text carrying Enter/Tab is
/// exempt: those keys can legitimately submit a form or move focus, and a
/// field that clears on submit is not a failure.
pub(crate) fn type_left_field_unchanged(typed: &str, before: Option<&str>, actual: &str) -> bool {
    if typed.contains(['\n', '\r', '\t']) {
        return false;
    }
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let a = norm(actual);
    a.is_empty() || before.is_some_and(|b| norm(b) == a)
}

pub(crate) fn type_not_kept_error(
    selector_or_ref: &str,
    typed: &str,
    actual: &str,
    appeared_then_cleared: bool,
    focus: Option<&str>,
) -> String {
    let what = if actual.is_empty() {
        format!(
            "the field is still empty after typing {}",
            quote_short(typed)
        )
    } else {
        format!(
            "the field still holds {} after typing {}",
            quote_short(actual),
            quote_short(typed)
        )
    };
    let why = if appeared_then_cleared {
        "The text appeared and was then wiped: the page re-rendered the field (a form still \
         hydrating right after load does this)."
            .to_string()
    } else {
        match focus {
            Some(f) if f != "none" => format!(
                "Keyboard focus is on <{f}>, so the keystrokes may have gone there, or the widget \
                 ignores synthetic keyboard input."
            ),
            _ => "Nothing has keyboard focus: the keystrokes went nowhere.".to_string(),
        }
    };
    format!(
        "type did not take: {what}. {why} Use `fill {selector_or_ref} <text>` (sets the value the \
         way a framework expects and verifies it), or `click {selector_or_ref}` then \
         `keyboard type <text>`."
    )
}

/// The field's current value as `get value` would report it (input/textarea/
/// select value, contenteditable text, Monaco/CodeMirror model). `None` when the
/// target is not editable or cannot be resolved.
pub async fn read_editable_value(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Option<String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await
    .ok()?;
    wait_for_paint_settled(client, &effective_session_id).await;
    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: read_editable_value_function(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await
        .ok()?;
    if result.exception_details.is_some() {
        return None;
    }
    let data = result.result.value?;
    if !data.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    Some(data.get("value").and_then(Value::as_str)?.to_string())
}

/// The warning `type` attaches when the field does not hold what was typed.
/// Whitespace is normalized (textarea/contenteditable read-backs fold it), and
/// a CRLF/LF difference never counts.
pub(crate) fn type_read_back_warning(typed: &str, actual: &str) -> Option<String> {
    let norm = |s: &str| {
        s.replace("\r\n", "\n")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let t = norm(typed);
    let a = norm(actual);
    if t.is_empty() || a.contains(&t) {
        return None;
    }
    Some(format!(
        "the field does not contain what was typed: {}. The keystrokes were delivered; \
         the page rewrote or rejected them (an input filter, mask or formatter). Verify with \
         `get value` before moving on",
        fill_mismatch_detail(typed, actual)
    ))
}

fn fill_values_match(expected: &str, actual: &str, engine: &str) -> bool {
    let normalized_expected = expected.replace("\r\n", "\n");
    let normalized_actual = actual.replace("\r\n", "\n");
    if engine.starts_with("contenteditable") {
        // Rich editors may render text as nested blocks, and innerText follows
        // layout whitespace rules. Validate the same non-whitespace content and
        // ordering without rejecting a successful edit over cosmetic line breaks.
        normalized_actual
            .split_whitespace()
            .eq(normalized_expected.split_whitespace())
    } else {
        normalized_actual == normalized_expected
            || same_after_formatting(&normalized_expected, &normalized_actual, engine)
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn type_text(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    text: &str,
    clear: bool,
    delay_ms: Option<u64>,
    iframe_sessions: &HashMap<String, String>,
    key_events: bool,
) -> Result<(), String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    // Focus
    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: "function() { this.focus(); }".to_string(),
                object_id: Some(object_id.clone()),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    if clear {
        // Clear the way a user does — select everything, then a trusted Delete
        // — so a page that ignores synthetic input sees the field emptied
        // (issue #358). Only when that leaves text behind (no selection API on
        // this control) fall back to the value setter.
        let selected = call_on(
            client,
            &effective_session_id,
            &object_id,
            r#"function() {
                try {
                    if (typeof this.select === 'function') { this.select(); return true; }
                    if (this.isContentEditable) {
                        const r = document.createRange();
                        r.selectNodeContents(this);
                        const s = window.getSelection();
                        s.removeAllRanges();
                        s.addRange(r);
                        return true;
                    }
                } catch (e) {}
                return false;
            }"#,
        )
        .await?
        .result
        .value
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
        if selected {
            press_key(client, session_id, "Delete").await?;
        }
        let left = read_value_of(client, &effective_session_id, &object_id).await;
        if left.as_deref().is_some_and(|v| !v.is_empty()) {
            call_on(
                client,
                &effective_session_id,
                &object_id,
                r#"function() {
                    this.value = '';
                    this.dispatchEvent(new Event('input', { bubbles: true }));
                }"#,
            )
            .await?;
        }
    }

    type_text_into_active_context(client, session_id, text, delay_ms, key_events).await
}

pub async fn type_text_into_active_context(
    client: &CdpClient,
    session_id: &str,
    text: &str,
    delay_ms: Option<u64>,
    key_events: bool,
) -> Result<(), String> {
    // Per-character timing: an explicit `delay_ms` wins (caller asked for a
    // fixed cadence); otherwise fall back to humanize — variable, human-like
    // inter-keystroke gaps at Fast/Human, all-zero (instant) at Off.
    let chars: Vec<char> = text.chars().collect();
    let cadence: Vec<std::time::Duration> = match delay_ms {
        Some(d) => vec![std::time::Duration::from_millis(d); chars.len()],
        None => {
            humanize::keystroke_delays(chars.len(), humanize::active_level(), humanize::next_seed())
        }
    };

    for (i, ch) in chars.into_iter().enumerate() {
        if matches!(ch, '\n' | '\r' | '\t') {
            let (key, code, key_code) = char_to_key_info(ch);
            let text_str = key_text(&key);
            client
                .send_command_typed::<_, Value>(
                    "Input.dispatchKeyEvent",
                    &DispatchKeyEventParams {
                        event_type: "keyDown".to_string(),
                        key: Some(key.clone()),
                        code: Some(code.clone()),
                        text: text_str.clone(),
                        unmodified_text: text_str,
                        windows_virtual_key_code: Some(key_code),
                        native_virtual_key_code: Some(key_code),
                        modifiers: None,
                    },
                    Some(session_id),
                )
                .await?;

            client
                .send_command_typed::<_, Value>(
                    "Input.dispatchKeyEvent",
                    &DispatchKeyEventParams {
                        event_type: "keyUp".to_string(),
                        key: Some(key),
                        code: Some(code),
                        text: None,
                        unmodified_text: None,
                        windows_virtual_key_code: Some(key_code),
                        native_virtual_key_code: Some(key_code),
                        modifiers: None,
                    },
                    Some(session_id),
                )
                .await?;
        } else if key_events {
            // Real keystrokes (keyDown+keyUp carrying `text`) for autocomplete /
            // combobox widgets that only react to key events and ignore the
            // `input` that `Input.insertText` fires — e.g. Google's address
            // postal-code → city/prefecture lookup (issue #36 / #4). The keyDown's
            // `text` still inserts the character, so the field also fills.
            let (key, code, key_code) = char_to_key_info(ch);
            let s = ch.to_string();
            client
                .send_command_typed::<_, Value>(
                    "Input.dispatchKeyEvent",
                    &DispatchKeyEventParams {
                        event_type: "keyDown".to_string(),
                        key: Some(key.clone()),
                        code: Some(code.clone()),
                        text: Some(s.clone()),
                        unmodified_text: Some(s),
                        windows_virtual_key_code: Some(key_code),
                        native_virtual_key_code: Some(key_code),
                        modifiers: None,
                    },
                    Some(session_id),
                )
                .await?;
            client
                .send_command_typed::<_, Value>(
                    "Input.dispatchKeyEvent",
                    &DispatchKeyEventParams {
                        event_type: "keyUp".to_string(),
                        key: Some(key),
                        code: Some(code),
                        text: None,
                        unmodified_text: None,
                        windows_virtual_key_code: Some(key_code),
                        native_virtual_key_code: Some(key_code),
                        modifiers: None,
                    },
                    Some(session_id),
                )
                .await?;
        } else {
            // VS Code/Electron webviews reject repeated dispatchKeyEvent calls
            // carrying printable `text`. Insert printable characters directly
            // and reserve key events for controls like Enter and Tab.
            client
                .send_command_typed::<_, Value>(
                    "Input.insertText",
                    &InsertTextParams {
                        text: ch.to_string(),
                    },
                    Some(session_id),
                )
                .await?;
        }

        let gap = cadence[i];
        if !gap.is_zero() {
            tokio::time::sleep(gap).await;
        }
    }

    Ok(())
}

/// Commit the value just typed into an async-autocomplete / tag widget by pressing
/// Enter (issue #50: juejin's 「添加标签」 input). Such widgets query their suggestion
/// list off the per-character key events `type --key-events` fires, but the
/// dropdown lands a tick later — so we let the page settle (RAF + microtask) so the
/// candidate is mounted/highlighted before the Enter, which the widget reads as
/// "accept the current candidate". Used by `type --enter`, which forces key-events
/// typing on (a bulk insertText never triggers the dropdown Enter would commit).
pub async fn commit_with_enter(client: &CdpClient, session_id: &str) -> Result<(), String> {
    wait_for_paint_settled(client, session_id).await;
    press_key(client, session_id, "enter").await?;
    wait_for_paint_settled(client, session_id).await;
    Ok(())
}

/// The CDP modifier bit for the platform's command key: Meta (Cmd) on macOS,
/// Control elsewhere.
fn platform_command_modifier() -> i32 {
    if cfg!(target_os = "macos") {
        4
    } else {
        2
    }
}

/// Is this chord the platform's select-all — Cmd+A on macOS, Ctrl+A elsewhere,
/// with no other modifier held?
///
/// Deliberately exact. Ctrl+A on macOS is not select-all (it moves to the start
/// of the line in a text field), Cmd+Shift+A is a different chord, and only the
/// one command whose failure was observed is mapped: copy, paste, cut and undo
/// may well have the same macOS gap, but they touch the clipboard and history,
/// and changing them without a reproduction would be a guess.
fn is_platform_select_all(key: &str, modifiers: Option<i32>) -> bool {
    key.eq_ignore_ascii_case("a") && modifiers == Some(platform_command_modifier())
}

pub async fn press_key(client: &CdpClient, session_id: &str, key: &str) -> Result<(), String> {
    press_key_with_modifiers(client, session_id, key, None).await
}

/// `press_key` for a key whose key-down submits something (Enter in a login
/// form). Once the key-down was delivered, a failed key-up is not reported:
/// the caller must not press the key a second time (#449).
pub async fn press_key_once(client: &CdpClient, session_id: &str, key: &str) -> Result<(), String> {
    let (key_name, code, key_code) = named_key_info(key);
    let text = key_text(&key_name);
    client
        .send_command_typed::<_, Value>(
            "Input.dispatchKeyEvent",
            &DispatchKeyEventParams {
                event_type: "keyDown".to_string(),
                key: Some(key_name.clone()),
                code: Some(code.clone()),
                text: text.clone(),
                unmodified_text: text,
                windows_virtual_key_code: Some(key_code),
                native_virtual_key_code: Some(key_code),
                modifiers: None,
            },
            Some(session_id),
        )
        .await?;
    let _ = client
        .send_command_typed::<_, Value>(
            "Input.dispatchKeyEvent",
            &DispatchKeyEventParams {
                event_type: "keyUp".to_string(),
                key: Some(key_name),
                code: Some(code),
                text: None,
                unmodified_text: None,
                windows_virtual_key_code: Some(key_code),
                native_virtual_key_code: Some(key_code),
                modifiers: None,
            },
            Some(session_id),
        )
        .await;
    Ok(())
}

/// Dispatch a keyDown+keyUp sequence for `key` with an optional CDP modifier bitmask.
///
/// Modifier values follow the CDP `Input.dispatchKeyEvent` spec:
/// 1 = Alt, 2 = Control, 4 = Meta (Cmd), 8 = Shift.
///
/// Callers that need a platform-appropriate modifier (e.g. Cmd on macOS,
/// Ctrl elsewhere) must choose the value themselves -- see `cfg!(target_os)`.
pub async fn press_key_with_modifiers(
    client: &CdpClient,
    session_id: &str,
    key: &str,
    modifiers: Option<i32>,
) -> Result<(), String> {
    // The platform's select-all chord has to carry the editing command. On macOS
    // Chrome resolves Cmd+A through the OS text system, which a synthetic CDP key
    // event never reaches: the keyDown is delivered, nothing is selected, and the
    // caret stays where it was. The next `insertText` then appends instead of
    // replacing — observed through `jev run`, where re-filling a field that
    // already read `casey@example.test` produced
    // `casey@example.testcasey@example.test`, and every attempt to correct it
    // appended again until the run gave up. `commands: ["selectAll"]` is how CDP
    // asks the editor itself to select, and fill's own select-all already uses it.
    if is_platform_select_all(key, modifiers) {
        return dispatch_editor_command(
            client,
            session_id,
            "a",
            platform_command_modifier(),
            "selectAll",
        )
        .await;
    }
    let (key_name, code, key_code) = named_key_info(key);

    // Suppress text insertion when Control (2) or Meta (4) modifiers are active,
    // since these are command chords (e.g. Ctrl+A = select-all), not text input.
    let has_command_modifier = modifiers.is_some_and(|m| m & (2 | 4) != 0);
    let text = if has_command_modifier {
        None
    } else {
        key_text(&key_name)
    };

    client
        .send_command_typed::<_, Value>(
            "Input.dispatchKeyEvent",
            &DispatchKeyEventParams {
                event_type: "keyDown".to_string(),
                key: Some(key_name.clone()),
                code: Some(code.clone()),
                text: text.clone(),
                unmodified_text: text.clone(),
                windows_virtual_key_code: Some(key_code),
                native_virtual_key_code: Some(key_code),
                modifiers,
            },
            Some(session_id),
        )
        .await?;

    client
        .send_command_typed::<_, Value>(
            "Input.dispatchKeyEvent",
            &DispatchKeyEventParams {
                event_type: "keyUp".to_string(),
                key: Some(key_name),
                code: Some(code),
                text: None,
                unmodified_text: None,
                windows_virtual_key_code: Some(key_code),
                native_virtual_key_code: Some(key_code),
                modifiers,
            },
            Some(session_id),
        )
        .await?;

    Ok(())
}

/// Dispatch a SINGLE key event (`keyDown` or `keyUp`) carrying the full key
/// descriptor — `key`, `code`, `windowsVirtualKeyCode`/`nativeVirtualKeyCode`,
/// and (on key-down) printable `text`. Powers the `keydown`/`keyup` commands.
///
/// The previous implementation sent only `{key}`, so games and shortcut handlers
/// that read `event.code` (e.g. `"KeyD"`, `"ArrowRight"`) or `event.keyCode` saw
/// nothing — a held key set no movement flag and did nothing (dogfood: holding a
/// direction in a canvas platformer barely nudged the player). Sending the same
/// descriptor `press` uses makes hold-to-move work regardless of which field the
/// page keys off.
pub async fn dispatch_single_key(
    client: &CdpClient,
    session_id: &str,
    key: &str,
    event_type: &str,
) -> Result<(), String> {
    dispatch_single_key_with_modifiers(client, session_id, key, event_type, None).await
}

/// [`dispatch_single_key`] carrying a CDP modifier bitmask (1 = Alt, 2 = Control,
/// 4 = Meta, 8 = Shift).
///
/// `press <chord> --hold` used to drop the chord's modifiers, so
/// `press Control+a --hold 500` held a plain `a` while still reporting the chord
/// back as pressed.
pub async fn dispatch_single_key_with_modifiers(
    client: &CdpClient,
    session_id: &str,
    key: &str,
    event_type: &str,
    modifiers: Option<i32>,
) -> Result<(), String> {
    let (key_name, code, key_code) = named_key_info(key);
    // Printable text is only meaningful on key-down; key-up never inserts. A
    // Control/Meta chord is a command (Ctrl+A = select-all), not text input, so
    // it carries no text either — same rule as `press_key_with_modifiers`.
    let has_command_modifier = modifiers.is_some_and(|m| m & (2 | 4) != 0);
    let text = if event_type == "keyDown" && !has_command_modifier {
        key_text(&key_name)
    } else {
        None
    };
    client
        .send_command_typed::<_, Value>(
            "Input.dispatchKeyEvent",
            &DispatchKeyEventParams {
                event_type: event_type.to_string(),
                key: Some(key_name),
                code: Some(code),
                text: text.clone(),
                unmodified_text: text,
                windows_virtual_key_code: Some(key_code),
                native_virtual_key_code: Some(key_code),
                modifiers,
            },
            Some(session_id),
        )
        .await?;
    Ok(())
}

pub async fn scroll(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: Option<&str>,
    delta_x: f64,
    delta_y: f64,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    if let Some(sel) = selector_or_ref {
        let (object_id, effective_session_id) =
            resolve_element_object_id(client, session_id, ref_map, sel, iframe_sessions).await?;
        let js = "function(dx, dy) { this.scrollBy(dx, dy); }".to_string();
        client
            .send_command_typed::<_, Value>(
                "Runtime.callFunctionOn",
                &CallFunctionOnParams {
                    function_declaration: js,
                    object_id: Some(object_id),
                    arguments: Some(vec![
                        CallArgument {
                            value: Some(serde_json::json!(delta_x)),
                            object_id: None,
                        },
                        CallArgument {
                            value: Some(serde_json::json!(delta_y)),
                            object_id: None,
                        },
                    ]),
                    return_by_value: Some(true),
                    await_promise: Some(false),
                },
                Some(&effective_session_id),
            )
            .await?;
    } else {
        let js = format!("window.scrollBy({}, {})", delta_x, delta_y);
        client
            .send_command_typed::<_, Value>(
                "Runtime.evaluate",
                &EvaluateParams {
                    expression: js,
                    return_by_value: Some(true),
                    await_promise: Some(false),
                },
                Some(session_id),
            )
            .await?;
    }
    Ok(())
}

pub async fn select_option(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    values: &[String],
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

    // Count matches so an unmatched value is a loud error (with the available
    // options) instead of a silent no-op. (Ported from vercel-labs/agent-browser #1432.)
    //
    // Native <select>: set the matching <option> + dispatch change. For a CUSTOM
    // combobox (react-select / ARIA listbox that is a <div>, not a <select>), the
    // old `Array.from(this.options)` threw (a div has no `.options`) — and because
    // the caller never inspected `exceptionDetails`, `select` returned a SILENT
    // success while nothing was selected (#105.1). Now non-<select> targets fall
    // through to the same portal-aware open→poll→click routine `pick` uses, so
    // `select @ref "Pageview"` actually lands on react-select and friends.
    let js = r#"async function(vals) {
            // Zero-width characters out, any whitespace run (NBSP included)
            // to one space: a label that reads "Tokyo" must match "Tokyo"
            // (after upstream vercel-labs/agent-browser #1736).
            const norm = s => String(s ?? '')
                .replace(/[\u200B\u200C\u200D\u2060\uFEFF]/g, '')
                .replace(/\s+/g, ' ')
                .trim();
            const el = this;

            if (el.tagName === 'SELECT') {
                const options = Array.from(el.options);
                const matches = [];
                for (const v of vals) {
                    // An exact value wins: the native setter selects by value,
                    // and one option's label may equal another's value.
                    let found = options.filter(opt => v === opt.value);
                    if (found.length === 0) {
                        found = options.filter(opt =>
                            v === opt.label.trim() || v === opt.textContent.trim()
                        );
                    }
                    if (found.length === 0) {
                        const nv = norm(v);
                        // The label is what the option shows (it defaults to
                        // the text; a `label` attribute overrides it).
                        found = options.filter(opt => norm(opt.label) === nv);
                        // Two options that only differ in whitespace: picking
                        // either would be a guess.
                        if (found.length > 1) {
                            return { ok: false, kind: 'select', ambiguous: v,
                                     available: options.map(o => ({ value: o.value, label: norm(o.label) })) };
                        }
                    }
                    // Every requested value has to match: selecting the ones
                    // that did and reporting success hides the missing one.
                    if (found.length === 0) {
                        return { ok: false, kind: 'select', missing: v,
                                 available: options.map(o => ({ value: o.value, label: norm(o.label) })) };
                    }
                    for (const opt of found) if (!matches.includes(opt)) matches.push(opt);
                }
                if (matches.length === 0) {
                    return { ok: false, kind: 'select', available: options.map(o => ({ value: o.value, label: norm(o.textContent) })) };
                }

                // Use the platform setters, not an instance-level `.value =` /
                // `.selected =`. React installs value trackers on controlled
                // controls; calling an instance setter can update that tracker
                // before the event arrives, making React conclude that nothing
                // changed. The native setters mutate the DOM while leaving the
                // framework tracker stale, exactly like a user selection.
                if (el.multiple) {
                    const selectedSetter = Object.getOwnPropertyDescriptor(
                        window.HTMLOptionElement.prototype, 'selected'
                    )?.set;
                    for (const opt of options) {
                        const selected = matches.includes(opt);
                        if (selectedSetter) selectedSetter.call(opt, selected);
                        else opt.selected = selected;
                    }
                } else {
                    const valueSetter = Object.getOwnPropertyDescriptor(
                        window.HTMLSelectElement.prototype, 'value'
                    )?.set;
                    const value = matches[0].value;
                    if (valueSetter) valueSetter.call(el, value);
                    else el.value = value;
                }

                // React, Vue and browser-native listeners differ on which event
                // they observe. Fire both after the native mutation.
                el.dispatchEvent(new Event('input', { bubbles: true }));
                el.dispatchEvent(new Event('change', { bubbles: true }));
                return { ok: true, matched: el.multiple ? matches.length : 1, kind: 'select', value: el.value };
            }

            // Custom combobox: open the control (so a portalled menu mounts), poll
            // for the option by visible text anywhere in the document, then click it.
            const want = norm(vals[0] || '');
            const matches = o => norm(o.textContent).toLowerCase().includes(want.toLowerCase());
            const fire = (n, t) => n.dispatchEvent(new MouseEvent(t, { bubbles: true, cancelable: true, view: window }));
            (el.focus && el.focus());
            ['pointerdown', 'mousedown', 'mouseup', 'click'].forEach(t => fire(el, t));
            const sel = '[role=option], [role=listbox] [role=option], li[role=option], [class*=option], [class*=item]';
            const find = () => [...document.querySelectorAll(sel)].find(o => o.offsetParent !== null && matches(o));
            const deadline = Date.now() + 2500;
            let opt = find();
            while (!opt && Date.now() < deadline) { await new Promise(r => setTimeout(r, 80)); opt = find(); }
            if (!opt) {
                const seen = [...document.querySelectorAll(sel)].filter(o => o.offsetParent !== null).map(o => norm(o.textContent)).filter(Boolean);
                return { ok: false, kind: 'custom', available: [...new Set(seen)].map(t => ({ value: '', label: t })) };
            }
            (opt.scrollIntoView && opt.scrollIntoView({ block: 'center' }));
            ['pointermove', 'pointerover', 'mouseover', 'pointerdown', 'mousedown', 'mouseup', 'click'].forEach(t => fire(opt, t));
            return { ok: true, matched: 1, kind: 'custom', picked: norm(opt.textContent) };
        }"#
    .to_string();

    let resp = client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: js,
                object_id: Some(object_id),
                arguments: Some(vec![CallArgument {
                    value: Some(serde_json::json!(values)),
                    object_id: None,
                }]),
                return_by_value: Some(true),
                await_promise: Some(true),
            },
            Some(&effective_session_id),
        )
        .await?;

    // A thrown exception (or a missing result) must be a loud error, never a silent
    // success — this is what let the old `select` no-op on custom comboboxes (#105.1).
    if let Some(ex) = resp.get("exceptionDetails") {
        let text = ex
            .get("exception")
            .and_then(|e| e.get("description"))
            .and_then(|d| d.as_str())
            .or_else(|| ex.get("text").and_then(|t| t.as_str()))
            .unwrap_or("select failed");
        return Err(format!("select {:?} failed: {}", values, text));
    }
    let result = resp.get("result").and_then(|r| r.get("value"));
    let ok = result
        .and_then(|v| v.get("ok"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !ok {
        let avail = result
            .and_then(|v| v.get("available"))
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|o| {
                        let val = o.get("value").and_then(|v| v.as_str()).unwrap_or("");
                        let label = o.get("label").and_then(|v| v.as_str()).unwrap_or("");
                        if val.is_empty() && label.is_empty() {
                            None
                        } else if label.is_empty() || label == val {
                            Some(val.to_string())
                        } else {
                            Some(format!("{} ({})", val, label))
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        if let Some(v) = result
            .and_then(|v| v.get("ambiguous"))
            .and_then(|v| v.as_str())
        {
            return Err(format!(
                "Multiple options matched {v:?} after whitespace normalization; pass the \
                 option's value instead. available options: {avail}"
            ));
        }
        let what = "No option matched";
        let custom = result.and_then(|v| v.get("kind")).and_then(|v| v.as_str()) == Some("custom");
        if custom {
            // Not a native <select>: `select` only opens it and looks. An
            // autocomplete field lists nothing until typed into, and `pick`
            // is the verb that types, waits for suggestions and clicks one.
            let first = values.first().map(String::as_str).unwrap_or("<text>");
            return Err(format!(
                "{} {:?}: {} is not a native <select>, so `select` can only open it and look \
                 (visible options: {}). For a custom or autocomplete (type-to-search) combobox use \
                 `pick {} --option {:?}`: it types the text, waits for the suggestions and clicks \
                 the match",
                what,
                values,
                selector_or_ref,
                if avail.is_empty() { "none" } else { &avail },
                selector_or_ref,
                first
            ));
        }
        return Err(format!(
            "{} {:?}. available options: {}",
            what, values, avail
        ));
    }

    Ok(())
}

pub async fn check(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let is_checked = super::element::is_element_checked(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;
    if !is_checked {
        click(
            client,
            session_id,
            ref_map,
            selector_or_ref,
            "left",
            1,
            iframe_sessions,
        )
        .await?;

        // Verify the click changed the state (Playwright parity: _setChecked re-checks).
        // If the coordinate-based click missed (e.g. hidden input, overlay), retry
        // with a JS .click() on the element and its associated input.
        if !super::element::is_element_checked(
            client,
            session_id,
            ref_map,
            selector_or_ref,
            iframe_sessions,
        )
        .await?
        {
            js_click_checkbox(
                client,
                session_id,
                ref_map,
                selector_or_ref,
                iframe_sessions,
            )
            .await?;
        }
    }
    Ok(())
}

pub async fn uncheck(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let is_checked = super::element::is_element_checked(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;
    if is_checked {
        click(
            client,
            session_id,
            ref_map,
            selector_or_ref,
            "left",
            1,
            iframe_sessions,
        )
        .await?;

        // Same verify-and-retry as check().
        if super::element::is_element_checked(
            client,
            session_id,
            ref_map,
            selector_or_ref,
            iframe_sessions,
        )
        .await?
        {
            js_click_checkbox(
                client,
                session_id,
                ref_map,
                selector_or_ref,
                iframe_sessions,
            )
            .await?;
        }
    }
    Ok(())
}

/// Fallback for when the coordinate-based CDP click did not toggle the
/// checkbox/radio state. This mirrors how Playwright dispatches clicks
/// through the DOM rather than via raw Input.dispatchMouseEvent coordinates.
///
/// Uses the same follow-label resolution as `is_element_checked`:
/// 1. If the element is a native input → `.click()` it directly.
/// 2. If the element is inside a `<label>` → `.click()` the label's `.control`.
/// 3. If the element has a nested `<input>` → `.click()` that input.
/// 4. Otherwise → `.click()` the element itself (handles ARIA role controls).
async fn js_click_checkbox(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
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

    let js = r#"function() {
            var el = this;
            var tag = el.tagName && el.tagName.toUpperCase();
            // 1. Native input — click it directly
            if (tag === 'INPUT' && (el.type === 'checkbox' || el.type === 'radio')) {
                el.click();
                return;
            }
            // 2. Follow label → control association
            var label = tag === 'LABEL' ? el : (el.closest && el.closest('label'));
            if (label && label.tagName && label.tagName.toUpperCase() === 'LABEL' && label.control) {
                label.control.click();
                return;
            }
            // 3. Nested native input
            var input = el.querySelector && el.querySelector('input[type="checkbox"], input[type="radio"]');
            if (input) {
                input.click();
                return;
            }
            // 4. ARIA role control — click the element itself
            el.click();
        }"#;

    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: js.to_string(),
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

pub async fn focus(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    focus_reporting_session(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await
    .map(|_| ())
}

/// Focus an element and return the CDP session it resolved in.
///
/// Callers that follow the focus with an input event — `press --selector` — need
/// the frame's session so the key is dispatched where the element actually
/// lives, not at the main frame (#167).
pub async fn focus_reporting_session(
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

    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: "function() { this.focus(); }".to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(effective_session_id)
}

/// A short CSS-ish descriptor of `document.activeElement` — `textarea[name="q"]`,
/// `button#send`, or `body` when nothing is focused.
///
/// Keyboard events go to whatever holds focus, so a key command that reports
/// success tells the agent nothing about *where* the key landed. Reporting the
/// target turns a silent no-op into an observable one (#167).
pub async fn active_element_descriptor(client: &CdpClient, session_id: &str) -> Option<String> {
    let js = r#"(() => {
        // Walk into shadow roots: document.activeElement stops at the host.
        let el = document.activeElement;
        while (el && el.shadowRoot && el.shadowRoot.activeElement) el = el.shadowRoot.activeElement;
        if (!el) return 'none';
        let out = el.tagName ? el.tagName.toLowerCase() : String(el);
        if (el.id) out += '#' + el.id;
        const name = el.getAttribute && el.getAttribute('name');
        if (name) out += '[name="' + name + '"]';
        return out;
    })()"#;

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: js.to_string(),
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await
        .ok()?;

    if result.exception_details.is_some() {
        return None;
    }
    result.result.value?.as_str().map(sanitize_descriptor)
}

/// The element keyboard input goes to, followed through shadow roots and
/// same-origin iframes, with its editable value when it has one.
#[derive(Debug, Clone, PartialEq)]
pub struct FocusedField {
    /// `tag#id[name="…"]`, sanitized.
    pub descriptor: String,
    /// `Some` when the focused element is a text field or contenteditable.
    pub value: Option<String>,
}

const FOCUSED_FIELD_JS: &str = r#"(() => {
    let doc = document;
    let el = doc.activeElement;
    for (let i = 0; i < 16 && el; i++) {
        if (el.shadowRoot && el.shadowRoot.activeElement) { el = el.shadowRoot.activeElement; continue; }
        if ((el.tagName === 'IFRAME' || el.tagName === 'FRAME')) {
            let inner = null;
            try { inner = el.contentDocument; } catch (e) {}
            if (inner && inner.activeElement) { doc = inner; el = inner.activeElement; continue; }
        }
        break;
    }
    if (!el || el === doc.body || el === doc.documentElement) return { descriptor: 'none', value: null };
    let d = el.tagName ? el.tagName.toLowerCase() : String(el);
    if (el.id) d += '#' + el.id;
    const n = el.getAttribute && el.getAttribute('name');
    if (n) d += '[name="' + n + '"]';
    // A cross-origin frame, or an editor whose model is not its input
    // transport's .value (Monaco, CodeMirror), hides the edited value: no
    // verdict can be drawn from it.
    const opaque = el.tagName === 'IFRAME' || el.tagName === 'FRAME'
        || !!(el.closest && el.closest('.monaco-editor, .CodeMirror'));
    if (opaque) return { descriptor: d, value: null, opaque: true };
    const textInput = el.tagName === 'TEXTAREA'
        || (el.tagName === 'INPUT' && !['checkbox', 'radio', 'button', 'submit', 'reset', 'file', 'image', 'range', 'color', 'hidden'].includes(String(el.type).toLowerCase()));
    const value = textInput ? String(el.value) : (el.isContentEditable ? (el.innerText || el.textContent || '') : null);
    return { descriptor: d, value };
})()"#;

/// What has keyboard focus right now. `None` when the probe fails.
pub async fn focused_field(client: &CdpClient, session_id: &str) -> Option<FocusedField> {
    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: FOCUSED_FIELD_JS.to_string(),
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await
        .ok()?;
    if result.exception_details.is_some() {
        return None;
    }
    let v = result.result.value?;
    if v.get("opaque").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    Some(FocusedField {
        descriptor: sanitize_descriptor(v.get("descriptor")?.as_str()?),
        value: v.get("value").and_then(Value::as_str).map(String::from),
    })
}

/// Judge a `keyboard type` against where focus was before and after it
/// (issue #358: `keyboard type` into a dialog field changed nothing and
/// printed `✓ Done`). `Err` when a text field had focus and its value is
/// exactly what it was, `Ok(Some(warning))` when nothing editable had focus,
/// `Ok(None)` when the text landed or there is nothing to judge (Enter/Tab
/// in the text, whitespace only, a probe that failed).
pub(crate) fn keyboard_type_verdict(
    typed: &str,
    before: Option<&FocusedField>,
    after: Option<&FocusedField>,
) -> Result<Option<String>, String> {
    if typed.trim().is_empty() || typed.contains(['\n', '\r', '\t']) {
        return Ok(None);
    }
    let (Some(before), Some(after)) = (before, after) else {
        return Ok(None);
    };
    match (&before.value, &after.value) {
        (Some(b), Some(a)) if before.descriptor == after.descriptor && a == b => Err(format!(
            "keyboard type did not take: <{}> had focus and still holds {} after typing {}. The \
             keystrokes were delivered but the field did not change (the page rejects this input, \
             or focus is on a field that ignores it). Use `fill <ref> <text>` or \
             `type <ref> <text> --key-events`, which target the field and verify it",
            after.descriptor,
            quote_short(a),
            quote_short(typed)
        )),
        (None, _) => Ok(Some(if before.descriptor == "none" {
            "nothing had keyboard focus, so the keystrokes went nowhere. Click the field first, \
             or use `type <ref> <text>`, which focuses it"
                .to_string()
        } else {
            format!(
                "keyboard focus was on <{}>, which is not a text field, so no text was entered. \
                 Click the field first, or use `type <ref> <text>`, which focuses it",
                before.descriptor
            )
        })),
        _ => Ok(None),
    }
}

/// Strip control characters out of a descriptor built from page attributes.
///
/// `id` and `name` are whatever the page put there, and the descriptor is
/// printed straight to a terminal in a warning. An ANSI escape in an iframe's
/// name could otherwise rewrite the line around it -- repainting a warning as a
/// success is precisely the outcome every other change here exists to prevent.
/// Length is capped for the same reason: a descriptor is an identifier, not a
/// payload.
fn sanitize_descriptor(raw: &str) -> String {
    const MAX: usize = 200;
    let mut out: String = raw
        .chars()
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .take(MAX)
        .collect();
    if raw.chars().count() > MAX {
        out.push('\u{2026}');
    }
    out
}

/// Does a key landing on `<body>` mean it provably did nothing?
///
/// Enter is the one key with no document-level default action: with nothing
/// focused it neither submits, activates, nor scrolls. Arrows scroll, Tab moves
/// focus, Backspace navigates back — those are all legitimate on `<body>`, so
/// warning about them would be noise (#167).
pub fn key_needs_focus(key_name: &str) -> bool {
    key_name.eq_ignore_ascii_case("enter") || key_name.eq_ignore_ascii_case("return")
}

/// Is `descriptor` (from [`active_element_descriptor`]) a "nothing is focused" value?
pub fn descriptor_is_unfocused(descriptor: &str) -> bool {
    matches!(descriptor, "body" | "html" | "none")
}

/// Whether a focus/press landed on an `<iframe>` element itself.
///
/// Focus stops at the frame boundary: giving an `<iframe>` focus does not focus
/// anything inside it, and keystrokes dispatched afterwards go to the container,
/// not to the control the caller could see through it (issue #218). The command
/// still "worked" by every check we had — an element was focused, a key was
/// dispatched — which is exactly what makes it a silent wrong target.
pub fn descriptor_is_iframe(descriptor: &str) -> bool {
    let tag = descriptor
        .split(['[', '#', '.', ':'])
        .next()
        .unwrap_or(descriptor)
        .trim();
    tag.eq_ignore_ascii_case("iframe") || tag.eq_ignore_ascii_case("frame")
}

/// What to say when a key or a focus landed on a frame container.
pub fn frame_boundary_warning(action: &str, descriptor: &str) -> String {
    format!(
        "{action} landed on <{descriptor}>, the frame element itself — focus and keystrokes do \
         not cross into a frame, so nothing inside it received this. List the frames with \
         `frames`, then drive the element inside: `frame <id>` switches the session into it \
         (`snapshot -i` / `click` / `fill` then act inside), or `eval --frame <id>` for a \
         one-off. A cross-origin overlay (a bank/branch picker, a payment field) is always a \
         separate frame like this."
    )
}

/// Would this key do anything app-visible ONLY if the page listens for it?
///
/// Arrow/Home/End/Page keys on a text field just move the caret, Escape has no
/// default, and Enter outside a form/button/textarea submits nothing — so on a
/// target with no key listener they provably no-op (issue #202). Keys with a
/// real browser default (Tab moves focus, Backspace deletes, printable keys
/// insert, Enter in a `<form>` submits) are left alone, as are command chords
/// (Ctrl/Meta + key): select-all/copy work without any listener.
pub fn key_effect_depends_on_listeners(
    key_name: &str,
    modifiers: Option<i32>,
    target_descriptor: Option<&str>,
) -> bool {
    if modifiers.is_some_and(|m| m & (2 | 4) != 0) {
        return false;
    }
    let Some(target) = target_descriptor else {
        return false;
    };
    if descriptor_is_unfocused(target) {
        // Nothing focused: `press_result` already warns for Enter; arrows scroll.
        return false;
    }
    let tag = target.split(['#', '[', '.']).next().unwrap_or("");
    let text_like = matches!(tag, "input" | "textarea") || target.contains("contenteditable");
    match key_name.to_ascii_lowercase().as_str() {
        "arrowup" | "arrowdown" | "arrowleft" | "arrowright" | "home" | "end" | "pageup"
        | "pagedown" => text_like && tag != "select",
        "escape" => true,
        // Enter: a textarea inserts a newline, a button/link activates, and a
        // field inside a <form> submits — only a bare input has no default. The
        // form membership isn't in the descriptor, so the probe below also
        // reports it and the caller stays quiet when a form would submit.
        "enter" => tag == "input",
        _ => false,
    }
}

/// Count keydown/keyup/keypress listeners reachable from the focused element:
/// on the element itself, every ancestor (React 17+ delegates at the root
/// container, older React and jQuery at `document`), the document and the
/// window. Uses `DOMDebugger.getEventListeners`, which sees `addEventListener`
/// registrations AND `onkeydown` attributes/properties. Returns `None` when the
/// probe can't run (no focused element, budget exhausted) or when the element
/// is inside a `<form>` where Enter has a submit default — the caller then
/// stays silent rather than guessing. Best-effort and capped: one
/// `getEventListeners` per ancestor, at most ~40 calls, 1.5s overall.
pub async fn count_key_listeners_on_active_element(
    client: &CdpClient,
    session_id: &str,
) -> Option<usize> {
    tokio::time::timeout(
        std::time::Duration::from_millis(1500),
        count_key_listeners_inner(client, session_id),
    )
    .await
    .ok()
    .flatten()
}

async fn count_key_listeners_inner(client: &CdpClient, session_id: &str) -> Option<usize> {
    // Collect [active element, ...ancestors, document, window] as ONE remote
    // array so the listener walk needs no per-node DOM traversal round-trips.
    let chain: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: r#"(() => {
                    let el = document.activeElement;
                    while (el && el.shadowRoot && el.shadowRoot.activeElement) el = el.shadowRoot.activeElement;
                    if (!el || el === document.body || el === document.documentElement) return null;
                    if (el.form || (el.closest && el.closest('form'))) return null;
                    const out = [];
                    let n = el;
                    while (n && out.length < 40) {
                        out.push(n);
                        n = n.parentNode || (n.host ? n.host : null);
                        if (n && n.nodeType === 11) n = n.host;
                    }
                    if (!out.includes(document)) out.push(document);
                    out.push(window);
                    return out;
                })()"#
                    .to_string(),
                return_by_value: Some(false),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await
        .ok()?;
    if chain.exception_details.is_some() {
        return None;
    }
    let array_id = chain.result.object_id?;
    let props: Value = client
        .send_command(
            "Runtime.getProperties",
            Some(json!({ "objectId": array_id, "ownProperties": true })),
            Some(session_id),
        )
        .await
        .ok()?;
    let mut node_ids: Vec<String> = Vec::new();
    for p in props.get("result").and_then(Value::as_array)? {
        let name = p.get("name").and_then(Value::as_str).unwrap_or("");
        if name.parse::<usize>().is_err() {
            continue;
        }
        if let Some(oid) = p
            .get("value")
            .and_then(|v| v.get("objectId"))
            .and_then(Value::as_str)
        {
            node_ids.push(oid.to_string());
        }
    }
    if node_ids.is_empty() {
        return None;
    }
    let mut found = 0usize;
    for oid in node_ids {
        let listeners: Value = client
            .send_command(
                "DOMDebugger.getEventListeners",
                Some(json!({ "objectId": oid })),
                Some(session_id),
            )
            .await
            .ok()?;
        if let Some(list) = listeners.get("listeners").and_then(Value::as_array) {
            found += list
                .iter()
                .filter(|l| {
                    matches!(
                        l.get("type").and_then(Value::as_str),
                        Some("keydown") | Some("keyup") | Some("keypress")
                    )
                })
                .count();
        }
        if found > 0 {
            break;
        }
    }
    Some(found)
}

pub async fn clear(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
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

    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: r#"function() {
                    this.focus();
                    this.value = '';
                    this.dispatchEvent(new Event('input', { bubbles: true }));
                    this.dispatchEvent(new Event('change', { bubbles: true }));
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

    Ok(())
}

pub async fn select_all(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
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

    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: r#"function() {
                    this.focus();
                    if (typeof this.select === 'function') {
                        this.select();
                    } else {
                        const range = document.createRange();
                        range.selectNodeContents(this);
                        const sel = window.getSelection();
                        sel.removeAllRanges();
                        sel.addRange(range);
                    }
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

    Ok(())
}

pub async fn scroll_into_view(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
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

    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration:
                    "function() { this.scrollIntoView({ block: 'center', inline: 'center' }); }"
                        .to_string(),
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

pub async fn dispatch_event(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    event_type: &str,
    event_init: Option<&Value>,
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

    let init_json = event_init
        .map(|v| serde_json::to_string(v).unwrap_or("{}".to_string()))
        .unwrap_or_else(|| "{ bubbles: true }".to_string());

    let js = format!(
        "function() {{ this.dispatchEvent(new Event({}, {})); }}",
        serde_json::to_string(event_type).unwrap_or_default(),
        init_json
    );

    client
        .send_command_typed::<_, Value>(
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

pub async fn highlight(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
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

    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: r#"function() {
                    this.style.outline = '2px solid red';
                    this.style.outlineOffset = '2px';
                    const el = this;
                    setTimeout(() => {
                        el.style.outline = '';
                        el.style.outlineOffset = '';
                    }, 3000);
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

    Ok(())
}

pub async fn tap_touch(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let (x, y, _w, _h, effective_session_id) = resolve_element_center(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    client
        .send_command(
            "Input.dispatchTouchEvent",
            Some(serde_json::json!({
                "type": "touchStart",
                "touchPoints": [{ "x": x, "y": y }],
            })),
            Some(&effective_session_id),
        )
        .await?;

    client
        .send_command(
            "Input.dispatchTouchEvent",
            Some(serde_json::json!({
                "type": "touchEnd",
                "touchPoints": [],
            })),
            Some(&effective_session_id),
        )
        .await?;

    Ok(())
}

/// After a click is dispatched, give the page two animation frames + a
/// microtask boundary to let React/Vue/Svelte commit any state update
/// scheduled by the click handler. Without this wait, follow-up commands
/// (e.g. `inserttext` against the textbox the click was supposed to mount)
/// race the renderer and can land on stale or wrong elements.
///
/// The wait is bounded to ~33ms in the common case (two RAFs at 60fps) and
/// returns immediately on any error — never an exception path.
///
/// Set `AGENT_BROWSER_CLICK_WAIT_STABLE=0` to disable for perf-sensitive
/// scripts that don't drive SPA UIs.
async fn wait_for_paint_settled(client: &CdpClient, session_id: &str) {
    if std::env::var("AGENT_BROWSER_CLICK_WAIT_STABLE").as_deref() == Ok("0") {
        return;
    }
    let script = "document.hidden ? Promise.resolve(true) : new Promise(resolve => \
        requestAnimationFrame(() => \
            requestAnimationFrame(() => \
                queueMicrotask(() => resolve(true)))))";
    // Tight 500ms timeout. RAF normally fires at 16ms, two RAFs total ~33ms.
    // If the tab is hidden / throttled / page is doing something pathological
    // and RAF doesn't fire in 500ms, we'd rather return now than stall the
    // user's click. Without this cap, a stuck RAF inherited the default 30s
    // CDP timeout and was the main contributor to the "click hangs 5+ min"
    // user report.
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        client.send_command_typed::<_, Value>(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: script.to_string(),
                return_by_value: Some(true),
                await_promise: Some(true),
            },
            Some(session_id),
        ),
    )
    .await;
}

/// Whether a `document.visibilityState` reply says the page is hidden.
/// Anything else (visible, prerender, an unreadable reply) leaves the page
/// alone: re-asserting focus emulation is only worth it for a hidden page.
fn visibility_reply_is_hidden(reply: &Value) -> bool {
    reply
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(Value::as_str)
        == Some("hidden")
}

/// Make a hidden page render again before a coordinate mouse event.
///
/// Background tabs are kept rendering by `Emulation.setFocusEmulationEnabled`
/// (see `enable_domains`): Chrome then counts the tab as captured, so it stays
/// `visible` while it is not the tab in front. That state does not always
/// last. On a login page in a profile with Bitwarden installed, the page went
/// `hidden` the moment `fill` focused the email field (the same `fill` on a
/// page without a login form kept it `visible`), and the menu recovery for
/// #373 hides the tab on purpose. Chrome delivers a coordinate mouse event to
/// a hidden page only after about 5 seconds: the first
/// `Input.dispatchMouseEvent` of each click (the move) took 5.0s while the
/// page itself was idle, so every click after such a `fill` took over 5s.
///
/// Turning focus emulation off and on again takes the capture back and the
/// page is `visible` again (measured: 5.2s clicks became 0.2s). Sending
/// `enabled: true` alone does nothing, because Chrome ignores a request for
/// the state it believes is already set. The page sees a window blur/focus
/// pair and a `visibilitychange`, the same as a user switching back to it,
/// which is what the click is about to imitate anyway. Keyboard input and
/// `Input.insertText` are not hit-tested, so they do not need this.
///
/// One evaluate when the page is visible. Best-effort throughout: a failure
/// here leaves the click as it was. `AGENT_BROWSER_KEEP_HIDDEN=1` opts out.
pub(crate) async fn restore_rendering_if_hidden(client: &CdpClient, session_id: &str) -> bool {
    if std::env::var("AGENT_BROWSER_KEEP_HIDDEN").as_deref() == Ok("1") {
        return false;
    }
    let probe = tokio::time::timeout(
        std::time::Duration::from_millis(1000),
        client.send_command(
            "Runtime.evaluate",
            Some(json!({ "expression": "document.visibilityState", "returnByValue": true })),
            Some(session_id),
        ),
    )
    .await;
    match probe {
        Ok(Ok(reply)) if visibility_reply_is_hidden(&reply) => {}
        _ => return false,
    }
    for enabled in [false, true] {
        if client
            .send_command(
                "Emulation.setFocusEmulationEnabled",
                Some(json!({ "enabled": enabled })),
                Some(session_id),
            )
            .await
            .is_err()
        {
            return false;
        }
    }
    true
}

/// Click at a raw viewport coordinate, bypassing element/selector resolution
/// (issue #8.4 first-class coordinate click). Honors the humanize trajectory and
/// press dwell exactly like a selector click — it shares `dispatch_click`.
pub async fn click_at_point(
    client: &CdpClient,
    session_id: &str,
    x: f64,
    y: f64,
    button: &str,
    click_count: i32,
) -> Result<(), String> {
    dispatch_click(client, session_id, x, y, button, click_count).await
}

async fn dispatch_click(
    client: &CdpClient,
    session_id: &str,
    x: f64,
    y: f64,
    button: &str,
    click_count: i32,
) -> Result<(), String> {
    // Move toward the target along a human-like path. At HumanizeLevel::Off this
    // is a single zero-delay step to (x, y) — identical to the old teleport — so
    // the default behaviour is unchanged. At Fast/Human it's a curved,
    // decelerating trajectory starting from where the cursor last landed, which
    // removes the "instant jump to exact centre, no prior movement" tell that
    // behavioural anti-bot systems flag.
    restore_rendering_if_hidden(client, session_id).await;
    let level = humanize::active_level();
    let start = humanize::last_cursor();
    let seed = humanize::next_seed();
    for step in humanize::move_path(start, (x, y), level, seed) {
        client
            .send_command_typed::<_, Value>(
                "Input.dispatchMouseEvent",
                &DispatchMouseEventParams {
                    event_type: "mouseMoved".to_string(),
                    x: step.x,
                    y: step.y,
                    button: None,
                    buttons: None,
                    click_count: None,
                    delta_x: None,
                    delta_y: None,
                    modifiers: None,
                },
                Some(session_id),
            )
            .await?;
        if !step.delay.is_zero() {
            tokio::time::sleep(step.delay).await;
        }
    }
    humanize::set_last_cursor((x, y));

    let button_value = match button {
        "right" => 2,
        "middle" => 4,
        _ => 1,
    };

    // Press
    client
        .send_command_typed::<_, Value>(
            "Input.dispatchMouseEvent",
            &DispatchMouseEventParams {
                event_type: "mousePressed".to_string(),
                x,
                y,
                button: Some(button.to_string()),
                buttons: Some(button_value),
                click_count: Some(click_count),
                delta_x: None,
                delta_y: None,
                modifiers: None,
            },
            Some(session_id),
        )
        .await?;

    // Hold briefly before releasing — a real click isn't instantaneous. Zero at
    // HumanizeLevel::Off.
    let dwell = humanize::press_dwell(level, seed);
    if !dwell.is_zero() {
        tokio::time::sleep(dwell).await;
    }

    // Release
    client
        .send_command_typed::<_, Value>(
            "Input.dispatchMouseEvent",
            &DispatchMouseEventParams {
                event_type: "mouseReleased".to_string(),
                x,
                y,
                button: Some(button.to_string()),
                buttons: Some(0),
                click_count: Some(click_count),
                delta_x: None,
                delta_y: None,
                modifiers: None,
            },
            Some(session_id),
        )
        .await?;

    wait_for_paint_settled(client, session_id).await;
    Ok(())
}

fn char_to_key_info(ch: char) -> (String, String, i32) {
    match ch {
        '\n' | '\r' => ("Enter".to_string(), "Enter".to_string(), 13),
        '\t' => ("Tab".to_string(), "Tab".to_string(), 9),
        ' ' => (" ".to_string(), "Space".to_string(), 32),
        _ => {
            let key = ch.to_string();
            if ch.is_ascii_alphabetic() {
                // For letters the Windows VK code equals the uppercase ASCII value.
                let upper = ch.to_ascii_uppercase();
                let code = format!("Key{}", upper);
                let key_code = upper as i32;
                (key, code, key_code)
            } else if ch.is_ascii_digit() {
                let code = format!("Digit{}", ch);
                let key_code = ch as i32;
                (key, code, key_code)
            } else {
                let (code, key_code) = punctuation_key_info(ch);
                (key, code.to_string(), key_code)
            }
        }
    }
}

/// Return the DOM `KeyboardEvent.code` value and Windows virtual-key code for
/// a punctuation / symbol character assuming a US keyboard layout.
///
/// The Windows virtual-key codes (VK_OEM_*) differ from ASCII values for
/// punctuation.  Using the raw ASCII code would misidentify characters – e.g.
/// '.' (ASCII 46) collides with VK_DELETE (0x2E = 46), causing the period to
/// be swallowed.
fn punctuation_key_info(ch: char) -> (&'static str, i32) {
    match ch {
        // VK_OEM_1 (0xBA = 186) — ";:" key on US layout
        ';' | ':' => ("Semicolon", 186),
        // VK_OEM_PLUS (0xBB = 187) — "=+" key
        '=' | '+' => ("Equal", 187),
        // VK_OEM_COMMA (0xBC = 188) — ",<" key
        ',' | '<' => ("Comma", 188),
        // VK_OEM_MINUS (0xBD = 189) — "-_" key
        '-' | '_' => ("Minus", 189),
        // VK_OEM_PERIOD (0xBE = 190) — ".>" key
        '.' | '>' => ("Period", 190),
        // VK_OEM_2 (0xBF = 191) — "/?" key
        '/' | '?' => ("Slash", 191),
        // VK_OEM_3 (0xC0 = 192) — "`~" key
        '`' | '~' => ("Backquote", 192),
        // VK_OEM_4 (0xDB = 219) — "[{" key
        '[' | '{' => ("BracketLeft", 219),
        // VK_OEM_5 (0xDC = 220) — "\\|" key
        '\\' | '|' => ("Backslash", 220),
        // VK_OEM_6 (0xDD = 221) — "]}" key
        ']' | '}' => ("BracketRight", 221),
        // VK_OEM_7 (0xDE = 222) — "'\""" key
        '\'' | '"' => ("Quote", 222),
        _ => ("", 0),
    }
}

/// Return the `text` value that CDP `Input.dispatchKeyEvent` needs on the
/// `keyDown` event so that Chrome performs the default action for the key.
/// For example Enter needs `"\r"` to actually submit a form, and Tab needs
/// `"\t"` to move focus.  Non-printable / navigation keys return `None`.
fn key_text(key_name: &str) -> Option<String> {
    match key_name {
        "Enter" => Some("\r".to_string()),
        "Tab" => Some("\t".to_string()),
        " " => Some(" ".to_string()),
        _ => {
            // Single printable characters carry themselves as text.
            if key_name.len() == 1 {
                Some(key_name.to_string())
            } else {
                None
            }
        }
    }
}

fn named_key_info(key: &str) -> (String, String, i32) {
    match key.to_lowercase().as_str() {
        "enter" | "return" => ("Enter".to_string(), "Enter".to_string(), 13),
        "tab" => ("Tab".to_string(), "Tab".to_string(), 9),
        "escape" | "esc" => ("Escape".to_string(), "Escape".to_string(), 27),
        "backspace" => ("Backspace".to_string(), "Backspace".to_string(), 8),
        "delete" => ("Delete".to_string(), "Delete".to_string(), 46),
        "arrowup" | "up" => ("ArrowUp".to_string(), "ArrowUp".to_string(), 38),
        "arrowdown" | "down" => ("ArrowDown".to_string(), "ArrowDown".to_string(), 40),
        "arrowleft" | "left" => ("ArrowLeft".to_string(), "ArrowLeft".to_string(), 37),
        "arrowright" | "right" => ("ArrowRight".to_string(), "ArrowRight".to_string(), 39),
        "home" => ("Home".to_string(), "Home".to_string(), 36),
        "end" => ("End".to_string(), "End".to_string(), 35),
        "pageup" => ("PageUp".to_string(), "PageUp".to_string(), 33),
        "pagedown" => ("PageDown".to_string(), "PageDown".to_string(), 34),
        "space" | " " => (" ".to_string(), "Space".to_string(), 32),
        _ => {
            if key.len() == 1 {
                let ch = key.chars().next().unwrap();
                char_to_key_info(ch)
            } else {
                (key.to_string(), key.to_string(), 0)
            }
        }
    }
}

/// Where to leave the caret once the match is found (issue #226).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectTextMode {
    /// Select the matched text itself.
    Text,
    /// Collapse to just before the match — a cursor, not a selection.
    CursorBefore,
    /// Collapse to just after the match, so a following `type` appends there.
    CursorAfter,
}

impl SelectTextMode {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "text" => Ok(Self::Text),
            "cursor_before" | "cursor-before" => Ok(Self::CursorBefore),
            "cursor_after" | "cursor-after" => Ok(Self::CursorAfter),
            other => Err(format!(
                "Unknown selection type '{other}'. Use text, cursor-before or cursor-after."
            )),
        }
    }

    fn as_js(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::CursorBefore => "cursor_before",
            Self::CursorAfter => "cursor_after",
        }
    }
}

/// Turn the page-side refusal into the sentence the agent needs (issue #226).
///
/// "Not found" and "too many candidates" are different problems with different
/// fixes, and this codebase has already paid for conflating them (#224): an
/// ambiguous match reported as "no match" sends the agent looking for a typo
/// that is not there. So each reason gets its own wording, and the ambiguous
/// one names the disambiguators.
fn select_text_error(result: &Value, selector: &str, text: &str) -> String {
    let reason = result
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("failed");
    let occurrences = result
        .get("textOccurrences")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let matches = result.get("matches").and_then(|v| v.as_u64()).unwrap_or(0);
    let detail = result.get("detail").and_then(|v| v.as_str()).unwrap_or("");
    match reason {
        "not-found" => format!(
            "No text matching \"{text}\" in {selector}. The field holds {} characters; \
             read it with `get value {selector}` to see what is actually there.",
            result.get("length").and_then(|v| v.as_u64()).unwrap_or(0)
        ),
        "context-mismatch" => format!(
            "\"{text}\" appears {occurrences} time(s) in {selector}, but never with the \
             prefix/suffix given. The prefix and suffix must sit immediately before and after \
             the text, and they are not part of what gets selected."
        ),
        "ambiguous" => format!(
            "\"{text}\" matches {matches} places in {selector} — refusing to guess which. \
             Disambiguate with --prefix / --suffix (the text immediately before or after the \
             one you mean)."
        ),
        "not-editable" => format!(
            "select-text needs an <input>, <textarea> or contenteditable element; {selector} \
             is {detail}."
        ),
        "unsupported-editor" => format!(
            "select-text cannot address {detail} — it keeps its own selection model, and a DOM \
             selection there would look applied while doing nothing. Use `fill` to replace the \
             whole value."
        ),
        "selection-unsupported" => format!(
            "{selector} is {detail}, which does not support text selection at all \
             (`setSelectionRange` throws on it). Use `fill` to replace the value."
        ),
        "not-applied" => format!(
            "The selection did not take on {selector} — the element may have re-rendered \
             between the match and the selection. Re-read the page and try again."
        ),
        other => format!("select-text failed on {selector}: {other}"),
    }
}

/// Select a run of text inside an editable element, or place the caret next to
/// it (issue #226).
///
/// `prefix`/`suffix` disambiguate a repeated phrase; they are context, not part
/// of the selection: `select_text(el, "确认", prefix = "请")` selects `确认`,
/// not `请确认`. A match that is missing, or present more than once with no way
/// to tell which was meant, is an error naming which of the two it was — never
/// a silent pick of the first one.
#[allow(clippy::too_many_arguments)]
pub async fn select_text(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    text: &str,
    prefix: &str,
    suffix: &str,
    mode: SelectTextMode,
    iframe_sessions: &HashMap<String, String>,
) -> Result<Value, String> {
    if text.is_empty() {
        return Err("select-text needs the text to select (it cannot be empty).".to_string());
    }
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    // Two different selection APIs, because the two element families have
    // nothing in common: `<input>`/`<textarea>` own a flat string and take
    // `setSelectionRange`; contenteditable content lives in text nodes, so the
    // offsets have to be mapped back onto them and applied through a Range.
    let js = format!(
        r#"function() {{
            const TEXT = {text};
            const PREFIX = {prefix};
            const SUFFIX = {suffix};
            const MODE = {mode};
            let el = this;
            if (el.shadowRoot) {{
                const inner = el.shadowRoot.querySelector('input, textarea, [contenteditable]');
                if (inner) el = inner;
            }}
            const editorRoot = el.closest && (el.closest('.monaco-editor') || el.closest('.CodeMirror') || el.closest('.cm-editor'));
            if (editorRoot) {{
                const kind = editorRoot.classList.contains('monaco-editor') ? 'a Monaco editor'
                    : (editorRoot.classList.contains('CodeMirror') ? 'a CodeMirror 5 editor' : 'a CodeMirror 6 editor');
                return {{ ok: false, reason: 'unsupported-editor', detail: kind }};
            }}
            const tag = el.tagName;
            const isField = tag === 'INPUT' || tag === 'TEXTAREA';
            if (!isField && !el.isContentEditable) {{
                return {{ ok: false, reason: 'not-editable', detail: 'a <' + tag.toLowerCase() + '>' }};
            }}

            // Offsets are computed over the raw text: the field's value, or the
            // concatenated text nodes for contenteditable (which is what a Range
            // addresses).
            let nodes = [];
            let hay;
            if (isField) {{
                hay = el.value;
            }} else {{
                const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
                let acc = 0;
                hay = '';
                while (walker.nextNode()) {{
                    const n = walker.currentNode;
                    nodes.push([acc, n]);
                    hay += n.data;
                    acc += n.data.length;
                }}
            }}

            const countOf = (needle) => {{
                if (!needle) return 0;
                let n = 0, at = hay.indexOf(needle);
                while (at !== -1) {{ n++; at = hay.indexOf(needle, at + 1); }}
                return n;
            }};
            const needle = PREFIX + TEXT + SUFFIX;
            const hits = [];
            let at = hay.indexOf(needle);
            while (at !== -1) {{ hits.push(at); at = hay.indexOf(needle, at + 1); }}
            const bare = countOf(TEXT);
            if (hits.length === 0) {{
                return {{
                    ok: false,
                    reason: bare > 0 ? 'context-mismatch' : 'not-found',
                    textOccurrences: bare,
                    length: hay.length
                }};
            }}
            if (hits.length > 1) {{
                return {{ ok: false, reason: 'ambiguous', matches: hits.length, textOccurrences: bare }};
            }}
            const start = hits[0] + PREFIX.length;
            const end = start + TEXT.length;
            const from = MODE === 'cursor_after' ? end : start;
            const to = MODE === 'text' ? end : from;

            try {{ el.focus({{ preventScroll: true }}); }} catch (e) {{}}

            if (isField) {{
                try {{
                    el.setSelectionRange(from, to);
                }} catch (e) {{
                    return {{
                        ok: false,
                        reason: 'selection-unsupported',
                        detail: 'an <' + tag.toLowerCase() + (el.type ? ' type=' + el.type : '') + '>'
                    }};
                }}
                // Verify the effect, not the call: a field can refuse the range
                // (or be re-rendered under us) and report nothing.
                const okSel = el.selectionStart === from && el.selectionEnd === to;
                return {{
                    ok: okSel,
                    reason: okSel ? null : 'not-applied',
                    engine: 'input',
                    start: from,
                    end: to,
                    selected: el.value.slice(from, to)
                }};
            }}

            const locate = (off) => {{
                for (let k = nodes.length - 1; k >= 0; k--) {{
                    if (off >= nodes[k][0]) return [nodes[k][1], off - nodes[k][0]];
                }}
                return [el, 0];
            }};
            const range = document.createRange();
            const [sn, so] = locate(from);
            const [en, eo] = locate(to);
            try {{
                range.setStart(sn, so);
                range.setEnd(en, eo);
            }} catch (e) {{
                return {{ ok: false, reason: 'not-applied' }};
            }}
            const view = el.ownerDocument.defaultView || window;
            const sel = view.getSelection();
            sel.removeAllRanges();
            sel.addRange(range);
            const got = sel.toString();
            const okSel = MODE === 'text' ? got === TEXT : sel.isCollapsed;
            return {{
                ok: okSel,
                reason: okSel ? null : 'not-applied',
                engine: 'contenteditable',
                start: from,
                end: to,
                selected: MODE === 'text' ? got : ''
            }};
        }}"#,
        text = serde_json::to_string(text).unwrap_or_default(),
        prefix = serde_json::to_string(prefix).unwrap_or_default(),
        suffix = serde_json::to_string(suffix).unwrap_or_default(),
        mode = serde_json::to_string(mode.as_js()).unwrap_or_default(),
    );

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

    if let Some(ex) = result.exception_details {
        return Err(format!("select-text failed: {}", ex.text));
    }
    let value = result.result.value.unwrap_or(Value::Null);
    if value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return Err(select_text_error(&value, selector_or_ref, text));
    }
    Ok(value)
}

/// What `paste` put on the synthetic clipboard (issue #227).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteFormat {
    /// Plain text.
    Text,
    /// Markdown *source*, inserted as plain text — the same thing the other
    /// tool does. Rendering it to HTML first would make the result depend on a
    /// renderer the caller cannot see; inserting the source is predictable.
    Markdown,
    /// Rich text: the payload rides as `text/html`, with the same string as the
    /// `text/plain` fallback for editors that only read plain text.
    Html,
}

impl PasteFormat {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "text" | "plain" => Ok(Self::Text),
            "md" | "markdown" => Ok(Self::Markdown),
            "html" => Ok(Self::Html),
            other => Err(format!(
                "Unknown paste format '{other}'. Use text, md or html."
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Markdown => "md",
            Self::Html => "html",
        }
    }
}

/// Paste content into the page without going anywhere near the user's real
/// clipboard (issue #227).
///
/// Why this exists: in a rich-text editor, pasting `text/html` and typing the
/// same characters produce different documents — `type` of `<b>bold</b>` gives
/// you those eleven characters, a paste gives you bold text. Multi-line text is
/// the same story: `type` turns a newline into Enter, which in most editors
/// submits or starts a new block, while a paste inserts the line break.
///
/// The clipboard is deliberately untouched. We drive the user's real Chrome, so
/// overwriting what they had copied is not an acceptable side effect — the
/// payload is carried by a `DataTransfer` on a synthetic `ClipboardEvent`
/// instead, and no `navigator.clipboard` call and no Ctrl+V is involved.
///
/// A synthetic paste event is untrusted, so it has no default action: an editor
/// that listens for `paste` handles it, and a plain field ignores it. That is
/// why the effect is read back, and why an unhandled paste falls through to a
/// real insert rather than reporting a success nothing produced.
pub async fn paste_content(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: Option<&str>,
    text: &str,
    format: PasteFormat,
    iframe_sessions: &HashMap<String, String>,
) -> Result<Value, String> {
    let want_html = matches!(format, PasteFormat::Html);
    let probe = format!(
        r#"function() {{
            const TEXT = {text};
            const WANT_HTML = {want_html};
            let el = this;
            if (!el || el.nodeType !== 1) {{
                return {{ ok: false, reason: 'no-target' }};
            }}
            if (el.shadowRoot) {{
                const inner = el.shadowRoot.querySelector('input, textarea, [contenteditable]');
                if (inner) el = inner;
            }}
            const isField = el.tagName === 'INPUT' || el.tagName === 'TEXTAREA';
            const read = () => (isField ? el.value : (el.isContentEditable ? el.innerHTML : el.textContent));
            const before = read();
            try {{ el.focus({{ preventScroll: true }}); }} catch (e) {{}}
            let handled = false;
            try {{
                const dt = new DataTransfer();
                dt.setData('text/plain', TEXT);
                if (WANT_HTML) dt.setData('text/html', TEXT);
                const ev = new ClipboardEvent('paste', {{
                    clipboardData: dt,
                    bubbles: true,
                    cancelable: true
                }});
                handled = el.dispatchEvent(ev) === false;
            }} catch (e) {{
                return {{ ok: false, reason: 'dispatch-failed', detail: String(e && e.message || e) }};
            }}
            return {{
                ok: true,
                handled,
                before,
                changed: read() !== before,
                isField,
                contentEditable: !!el.isContentEditable
            }};
        }}"#,
        text = serde_json::to_string(text).unwrap_or_default(),
        want_html = want_html,
    );

    let (object_id, effective_session_id) = match selector_or_ref {
        Some(sel) => {
            resolve_element_object_id(client, session_id, ref_map, sel, iframe_sessions).await?
        }
        None => (
            active_element_object_id(client, session_id).await?,
            session_id.to_string(),
        ),
    };

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: probe,
                object_id: Some(object_id.clone()),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;
    if let Some(ex) = result.exception_details {
        return Err(format!("paste failed: {}", ex.text));
    }
    let probe_result = result.result.value.unwrap_or(Value::Null);
    if probe_result.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let reason = probe_result
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("failed");
        return Err(match reason {
            "no-target" => {
                "paste needs an element: pass a selector/@ref, or focus a field first.".to_string()
            }
            other => format!("paste failed: {other}"),
        });
    }

    let changed = probe_result
        .get("changed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let content_editable = probe_result
        .get("contentEditable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if changed {
        return Ok(json!({
            "engine": "paste-event",
            "format": format.as_str(),
            "chars": text.chars().count(),
        }));
    }

    // Nothing listened. Insert for real, matching what the format promised:
    // rich content through `insertHTML` (contenteditable only — a `<textarea>`
    // holds a string, so its "html" is that string), everything else through a
    // TRUSTED `Input.insertText`, which respects the current selection and puts
    // newlines in as newlines instead of Enter.
    let engine = if want_html && content_editable {
        let insert = format!(
            r#"function() {{
                try {{
                    this.focus({{ preventScroll: true }});
                    return document.execCommand('insertHTML', false, {html});
                }} catch (e) {{ return false; }}
            }}"#,
            html = serde_json::to_string(text).unwrap_or_default(),
        );
        let _: EvaluateResult = client
            .send_command_typed(
                "Runtime.callFunctionOn",
                &CallFunctionOnParams {
                    function_declaration: insert,
                    object_id: Some(object_id.clone()),
                    arguments: None,
                    return_by_value: Some(true),
                    await_promise: Some(false),
                },
                Some(&effective_session_id),
            )
            .await?;
        "insert-html"
    } else {
        client
            .send_command_typed::<_, Value>(
                "Input.insertText",
                &InsertTextParams {
                    text: text.to_string(),
                },
                Some(&effective_session_id),
            )
            .await?;
        "insert-text"
    };

    // Verify the effect, not the call. An editor that swallowed the paste event
    // AND ignored the insert must not come back as a success — that is exactly
    // the silent no-op this codebase keeps paying for.
    let verify: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: r#"function() {
                    const isField = this.tagName === 'INPUT' || this.tagName === 'TEXTAREA';
                    return isField ? this.value : (this.isContentEditable ? this.innerHTML : this.textContent);
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
    let after = verify
        .result
        .value
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    let before = probe_result
        .get("before")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    // Unchanged content is the failure, full stop. The earlier form let a
    // target that ALREADY held the payload pass verification — `after` contains
    // the text, but so did `before`, so the paste demonstrably did nothing and
    // still reported ✓.
    if after == before {
        return Err(
            "The paste did not take: the target neither handled the paste event nor accepted an \
             insert. Its content is unchanged. For Monaco or a similar editor with its own model, \
             use `fill`."
                .to_string(),
        );
    }

    Ok(json!({
        "engine": engine,
        "format": format.as_str(),
        "chars": text.chars().count(),
    }))
}

/// The object id of `document.activeElement`, for a `paste` with no selector.
async fn active_element_object_id(client: &CdpClient, session_id: &str) -> Result<String, String> {
    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: "(() => { let a = document.activeElement; \
                             while (a && a.shadowRoot && a.shadowRoot.activeElement) \
                             a = a.shadowRoot.activeElement; return a; })()"
                    .to_string(),
                return_by_value: Some(false),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;
    result.result.object_id.ok_or_else(|| {
        "paste needs a target: pass a selector/@ref, or focus a field first.".to_string()
    })
}

#[cfg(test)]
mod select_all_chord_tests {
    use super::is_platform_select_all;

    // CDP modifier bits: 1 Alt, 2 Control, 4 Meta, 8 Shift.

    #[test]
    #[cfg(target_os = "macos")]
    fn cmd_a_is_select_all_on_macos_and_ctrl_a_is_not() {
        assert!(is_platform_select_all("a", Some(4)));
        assert!(is_platform_select_all("A", Some(4)));
        // Ctrl+A on macOS moves to the start of the line; mapping it would
        // turn "go to line start" into "select everything".
        assert!(!is_platform_select_all("a", Some(2)));
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn ctrl_a_is_select_all_off_macos_and_meta_a_is_not() {
        assert!(is_platform_select_all("a", Some(2)));
        assert!(is_platform_select_all("A", Some(2)));
        assert!(!is_platform_select_all("a", Some(4)));
    }

    #[test]
    fn only_the_exact_chord_is_select_all() {
        let cmd = if cfg!(target_os = "macos") { 4 } else { 2 };
        assert!(
            !is_platform_select_all("a", None),
            "a bare `a` types a letter"
        );
        assert!(
            !is_platform_select_all("a", Some(cmd | 8)),
            "Shift makes it another chord"
        );
        assert!(
            !is_platform_select_all("a", Some(cmd | 1)),
            "Alt makes it another chord"
        );
        assert!(
            !is_platform_select_all("c", Some(cmd)),
            "copy is deliberately not mapped"
        );
        assert!(
            !is_platform_select_all("v", Some(cmd)),
            "paste is deliberately not mapped"
        );
        assert!(!is_platform_select_all("Enter", Some(cmd)));
    }
}

#[cfg(test)]
mod tests {
    /// The error the comparison run hit on a batch `fill`, verbatim, must take
    /// the re-resolve-and-verify path; ordinary failures must not.
    #[test]
    fn stale_object_errors_are_recognised() {
        assert!(super::is_stale_object_error(
            r#"CDP error (Runtime.callFunctionOn): {"code":-32000,"message":"Could not find object with given id"}"#
        ));
        assert!(super::is_stale_object_error("Invalid remote object id"));
        assert!(!super::is_stale_object_error("Element not found: #x"));
        assert!(!super::is_stale_object_error(
            "Debugger is not attached to the tab with id: 1"
        ));
    }

    /// Focus stops at a frame boundary, so a key dispatched after focusing an
    /// `<iframe>` reaches the container and nothing inside it — a success by
    /// every check the command had, and the wrong target every time (#218).
    #[test]
    fn a_frame_container_is_recognised_as_a_wrong_target() {
        assert!(descriptor_is_iframe("iframe"));
        assert!(descriptor_is_iframe(
            "iframe[name=\"stripe-connect-ui-layer-1\"]"
        ));
        assert!(descriptor_is_iframe("iframe#checkout"));
        assert!(descriptor_is_iframe("IFRAME"));
        // Not every element whose name merely starts with the letters.
        assert!(!descriptor_is_iframe("input[name=\"iframe\"]"));
        assert!(!descriptor_is_iframe("body"));
        assert!(!descriptor_is_iframe("textarea[name=\"q\"]"));
    }

    /// The descriptor is built from page-controlled `id`/`name` and printed to a
    /// terminal. An escape sequence in it could repaint the warning line.
    #[test]
    fn a_descriptor_cannot_carry_terminal_escapes_out_of_the_page() {
        let hostile = sanitize_descriptor("iframe#a\u{1b}[2K\u{1b}[Gok\r\n");
        assert!(!hostile.contains('\u{1b}'), "{hostile}");
        assert!(!hostile.contains('\r'), "{hostile}");
        assert!(!hostile.contains('\n'), "{hostile}");
        // Still recognisable as the element it names.
        assert!(hostile.starts_with("iframe#a"), "{hostile}");
        assert!(descriptor_is_iframe(&hostile), "{hostile}");

        // Ordinary descriptors pass through untouched.
        assert_eq!(
            sanitize_descriptor("iframe#inner[name=\"inner\"]"),
            "iframe#inner[name=\"inner\"]"
        );

        // And a descriptor is an identifier, not a place to hide a payload.
        let long = sanitize_descriptor(&format!("iframe#{}", "a".repeat(500)));
        assert!(long.chars().count() <= 201, "{}", long.chars().count());
    }

    #[test]
    fn the_frame_boundary_warning_points_at_the_way_in() {
        let w = frame_boundary_warning("Enter", "iframe[name=\"pay\"]");
        assert!(w.contains("frame element itself"), "{w}");
        assert!(w.contains("do not cross into a frame"), "{w}");
        // The escape hatch has to be a command that exists.
        assert!(w.contains("`frames`") && w.contains("frame <id>"), "{w}");
        assert!(w.contains("eval --frame"), "{w}");
    }

    use super::*;

    #[test]
    fn test_fill_verification_tolerates_contenteditable_layout_whitespace() {
        assert!(fill_values_match(
            "first line\nsecond line",
            "first line\n\n  second line\n",
            "contenteditable"
        ));
        assert!(fill_values_match(
            "first line second line",
            "first line\nsecond line",
            "contenteditable-fallback"
        ));
        assert!(!fill_values_match(
            "first line second line",
            "first line different line",
            "contenteditable"
        ));
    }

    #[test]
    fn test_fill_mismatch_detail_shows_both_values_and_flags_dropped_cjk() {
        // Issue #203: the old message said "read back an empty value" and nothing
        // else, so a Latin-only field silently eating CJK looked like a transport bug.
        let d = fill_mismatch_detail("千代田区", "");
        assert!(d.contains("\"千代田区\""), "{d}");
        assert!(d.contains("Only the ASCII characters survived"), "{d}");
        let d = fill_mismatch_detail("千代田1-2-3", "1-2-3");
        assert!(
            d.contains("\"1-2-3\"") && d.contains("\"千代田1-2-3\""),
            "{d}"
        );
        assert!(d.contains("Latin-only"), "{d}");
        // Pure-ASCII mismatch: no CJK diagnosis.
        let d = fill_mismatch_detail("hello", "hell");
        assert!(!d.contains("ASCII"), "{d}");
        assert!(d.contains("(4 chars)") && d.contains("(5 chars)"), "{d}");
    }

    #[test]
    fn test_type_read_back_warning_only_when_text_is_missing() {
        assert!(type_read_back_warning("千代田区", "東京都千代田区").is_none());
        let w = type_read_back_warning("千代田1-2-3", "1-2-3").unwrap();
        assert!(w.contains("rewrote or rejected"), "{w}");
        assert!(w.contains("ASCII"), "{w}");
        assert!(type_read_back_warning("", "x").is_none());
    }

    #[test]
    fn test_type_left_field_unchanged_only_when_nothing_landed() {
        // Empty after typing: nothing landed (#355).
        assert!(type_left_field_unchanged("13800000000", Some(""), ""));
        assert!(type_left_field_unchanged("13800000000", None, ""));
        // Same as before: the keystrokes went elsewhere.
        assert!(type_left_field_unchanged("abc", Some("old"), "old"));
        // A mask rewrote it: something landed, stays a warning (#203).
        assert!(!type_left_field_unchanged("千代田1-2-3", Some(""), "1-2-3"));
        assert!(!type_left_field_unchanged("abc", Some("old"), "oldab"));
        // Enter/Tab may submit or move focus; a cleared field is not proof.
        assert!(!type_left_field_unchanged("q\n", Some(""), ""));
    }

    #[test]
    fn test_type_not_kept_error_names_the_way_out() {
        let e = type_not_kept_error("@e41", "13800000000", "", true, None);
        assert!(e.contains("still empty"), "{e}");
        assert!(e.contains("re-rendered"), "{e}");
        assert!(e.contains("fill @e41"), "{e}");
        assert!(e.contains("keyboard type"), "{e}");
        let e = type_not_kept_error("#q", "x", "", false, Some("input#other"));
        assert!(e.contains("input#other"), "{e}");
        let e = type_not_kept_error("#q", "x", "", false, Some("none"));
        assert!(e.contains("Nothing has keyboard focus"), "{e}");
    }

    #[test]
    fn test_key_effect_depends_on_listeners() {
        // Arrows on a bare text input do nothing without a handler (#202).
        assert!(key_effect_depends_on_listeners(
            "ArrowDown",
            None,
            Some("input#q")
        ));
        assert!(key_effect_depends_on_listeners(
            "Enter",
            None,
            Some("input[name=\"x\"]")
        ));
        assert!(key_effect_depends_on_listeners(
            "Escape",
            None,
            Some("div#modal")
        ));
        // Native defaults / chords / unfocused targets are left alone.
        assert!(!key_effect_depends_on_listeners(
            "ArrowDown",
            None,
            Some("select#s")
        ));
        assert!(!key_effect_depends_on_listeners(
            "Enter",
            None,
            Some("textarea#t")
        ));
        assert!(!key_effect_depends_on_listeners(
            "Enter",
            None,
            Some("button#go")
        ));
        assert!(!key_effect_depends_on_listeners(
            "Tab",
            None,
            Some("input#q")
        ));
        assert!(!key_effect_depends_on_listeners(
            "a",
            Some(4),
            Some("input#q")
        ));
        assert!(!key_effect_depends_on_listeners(
            "ArrowDown",
            None,
            Some("body")
        ));
        assert!(!key_effect_depends_on_listeners("ArrowDown", None, None));
    }

    #[test]
    fn test_fill_verification_remains_exact_for_value_backed_engines() {
        assert!(fill_values_match(
            "first\r\nsecond",
            "first\nsecond",
            "monaco"
        ));
        assert!(!fill_values_match(
            "first second",
            "first\nsecond",
            "monaco"
        ));
        assert!(!fill_values_match("  yaml", " yaml", "input"));
    }

    #[test]
    fn test_fill_verification_accepts_a_page_that_only_added_separators() {
        // Stripe Elements (#374): the value took, the field displays it grouped.
        assert!(fill_values_match("1234", "12 / 34", "input"));
        assert!(fill_values_match(
            "4242424242424242",
            "4242 4242 4242 4242",
            "input"
        ));
        assert!(fill_values_match("5551234567", "(555) 123-4567", "input"));
        // A different value, a dropped character, or an editor: still a failure.
        assert!(!fill_values_match("1234", "12 / 35", "input"));
        assert!(!fill_values_match("12345", "12 / 34", "input"));
        assert!(!fill_values_match("1234", "12 / 34", "monaco"));
        assert!(!fill_values_match("a b", "ab", "input"));
        // A removed sign or decimal point, or a replaced space, is a change.
        assert!(!fill_values_match("-12", "12 ", "input"));
        assert!(!fill_values_match("1.5", "1 5", "input"));
        assert!(!fill_values_match("12 34", "12-34", "input"));
        assert!(fill_values_match("12/34", "12 / 34", "input"));
        assert!(!fill_values_match("1234", "12\n34", "input"));
    }

    /// Verify that `char_to_key_info` returns the correct (key, code,
    /// windowsVirtualKeyCode) triple for every character in Playwright's
    /// USKeyboardLayout.  The expected values below are taken verbatim from
    /// playwright-core/lib/server/usKeyboardLayout.js so that any drift from
    /// Playwright's behaviour is caught immediately.
    #[test]
    fn test_char_to_key_info_matches_playwright_layout() {
        // (character, expected_code, expected_vk_code)
        let cases: &[(char, &str, i32)] = &[
            // Letters – VK code must equal the uppercase ASCII value.
            ('a', "KeyA", 65),
            ('z', "KeyZ", 90),
            ('A', "KeyA", 65),
            // Digits
            ('0', "Digit0", 48),
            ('9', "Digit9", 57),
            // Punctuation – these are the values from Playwright's layout.
            // The bug that prompted this test sent '.' as VK 46 (= VK_DELETE).
            ('.', "Period", 190),
            (',', "Comma", 188),
            ('/', "Slash", 191),
            (';', "Semicolon", 186),
            ('\'', "Quote", 222),
            ('[', "BracketLeft", 219),
            (']', "BracketRight", 221),
            ('\\', "Backslash", 220),
            ('`', "Backquote", 192),
            ('-', "Minus", 189),
            ('=', "Equal", 187),
            // Shifted variants produced by the same physical keys.
            ('>', "Period", 190),
            ('<', "Comma", 188),
            ('?', "Slash", 191),
            (':', "Semicolon", 186),
            ('"', "Quote", 222),
            ('{', "BracketLeft", 219),
            ('}', "BracketRight", 221),
            ('|', "Backslash", 220),
            ('~', "Backquote", 192),
            ('_', "Minus", 189),
            ('+', "Equal", 187),
            // Whitespace / control
            (' ', "Space", 32),
            ('\n', "Enter", 13),
            ('\t', "Tab", 9),
        ];

        for &(ch, expected_code, expected_vk) in cases {
            let (key, code, vk) = char_to_key_info(ch);
            assert_eq!(
                code, expected_code,
                "char {:?}: expected code {:?}, got {:?}",
                ch, expected_code, code
            );
            assert_eq!(
                vk, expected_vk,
                "char {:?}: expected VK {}, got {} (ASCII would be {})",
                ch, expected_vk, vk, ch as i32
            );
            // key should be the character itself (except control chars).
            if !ch.is_control() {
                assert_eq!(key, ch.to_string(), "char {:?}: key mismatch", ch);
            }
        }
    }

    /// Regression test: period must NEVER map to VK 46 (VK_DELETE).
    #[test]
    fn test_period_is_not_vk_delete() {
        let (_, _, vk) = char_to_key_info('.');
        assert_ne!(
            vk, 46,
            "Period must not use VK code 46 (VK_DELETE); expected 190 (VK_OEM_PERIOD)"
        );
        assert_eq!(vk, 190);
    }

    /// Characters outside the US keyboard layout should return (key, "", 0)
    /// so that `type_text` falls back to `Input.insertText`.
    #[test]
    fn test_unmapped_chars_return_zero_keycode() {
        for ch in ['@', '#', '$', '%', '^', '&', '*', '(', ')', '€', '£', '你'] {
            let (key, code, vk) = char_to_key_info(ch);
            assert_eq!(
                code, "",
                "char {:?}: unmapped char should have empty code, got {:?}",
                ch, code
            );
            assert_eq!(
                vk, 0,
                "char {:?}: unmapped char should have VK 0, got {}",
                ch, vk
            );
            assert_eq!(key, ch.to_string());
        }
    }

    #[test]
    fn test_key_text_returns_correct_text_for_special_keys() {
        assert_eq!(key_text("Enter"), Some("\r".to_string()));
        assert_eq!(key_text("Tab"), Some("\t".to_string()));
        assert_eq!(key_text(" "), Some(" ".to_string()));
        // Single printable characters carry themselves.
        assert_eq!(key_text("a"), Some("a".to_string()));
        assert_eq!(key_text("Z"), Some("Z".to_string()));
        // Non-printable named keys return None.
        assert_eq!(key_text("Escape"), None);
        assert_eq!(key_text("ArrowUp"), None);
        assert_eq!(key_text("Backspace"), None);
        assert_eq!(key_text("Delete"), None);
    }

    #[test]
    fn a_drawn_cursor_marks_the_overlay_on() {
        assert_eq!(cursor_state_from_reply(Some(true), None), Some(1));
        // `drawn: true` wins regardless of any reason riding along.
        assert_eq!(cursor_state_from_reply(Some(true), Some("no-tab")), Some(1));
    }

    #[test]
    fn only_an_explicit_disabled_switches_the_cursor_off() {
        assert_eq!(
            cursor_state_from_reply(Some(false), Some("disabled")),
            Some(2)
        );
    }

    #[test]
    fn transient_refusals_prove_nothing_about_the_overlay() {
        // `no-tab` happens when the session's tab isn't registered yet on the
        // first click; `bad-coords` when the element resolved to something
        // degenerate. Treating either as "off" would kill the cursor — and the
        // hide-during-screenshot that rides on the same state — for the rest of
        // the daemon's life, so both must stay inconclusive.
        assert_eq!(cursor_state_from_reply(Some(false), Some("no-tab")), None);
        assert_eq!(
            cursor_state_from_reply(Some(false), Some("bad-coords")),
            None
        );
        // An unrecognized shape (a newer/older extension) is inconclusive too,
        // never a silent "off".
        assert_eq!(cursor_state_from_reply(None, None), None);
        assert_eq!(cursor_state_from_reply(Some(false), None), None);
        assert_eq!(cursor_state_from_reply(None, Some("something-new")), None);
    }
}

#[cfg(test)]
mod trusted_input_diagnostics_tests {
    //! Issue #358: the verdicts behind fill's commit-control warning and
    //! `keyboard type`'s read-back.
    use super::{commit_control_warning, keyboard_type_verdict, CommitControl, FocusedField};

    fn save(disabled: bool, other_empty_required: u64) -> CommitControl {
        CommitControl {
            name: "Save".to_string(),
            disabled,
            other_empty_required,
        }
    }

    fn field(descriptor: &str, value: Option<&str>) -> FocusedField {
        FocusedField {
            descriptor: descriptor.to_string(),
            value: value.map(String::from),
        }
    }

    #[test]
    fn unmoved_disabled_save_warns() {
        let w = commit_control_warning(Some(&save(true, 0)), Some(&save(true, 0)));
        assert!(w.unwrap().contains("did not react"));
    }

    #[test]
    fn save_that_enabled_or_waits_on_other_fields_is_quiet() {
        assert!(commit_control_warning(Some(&save(true, 0)), Some(&save(false, 0))).is_none());
        assert!(commit_control_warning(Some(&save(false, 0)), Some(&save(false, 0))).is_none());
        // Another required field is still empty: that explains the disabled Save.
        assert!(commit_control_warning(Some(&save(true, 1)), Some(&save(true, 1))).is_none());
        assert!(commit_control_warning(None, Some(&save(true, 0))).is_none());
    }

    #[test]
    fn keyboard_type_into_unchanged_field_fails() {
        let f = field("input#q", Some("old"));
        let err = keyboard_type_verdict("new", Some(&f), Some(&f)).unwrap_err();
        assert!(err.contains("keyboard type did not take"), "{err}");
    }

    #[test]
    fn keyboard_type_that_landed_is_quiet() {
        let before = field("input#q", Some("old"));
        let after = field("input#q", Some("oldnew"));
        assert_eq!(
            keyboard_type_verdict("new", Some(&before), Some(&after)),
            Ok(None)
        );
    }

    #[test]
    fn keyboard_type_without_a_text_field_warns() {
        let none = field("none", None);
        let w = keyboard_type_verdict("abc", Some(&none), Some(&none)).unwrap();
        assert!(w.unwrap().contains("nothing had keyboard focus"));
        let div = field("div#app", None);
        let w = keyboard_type_verdict("abc", Some(&div), Some(&div)).unwrap();
        assert!(w.unwrap().contains("not a text field"));
    }

    #[test]
    fn enter_tab_and_whitespace_are_not_judged() {
        let f = field("input#q", Some("x"));
        assert_eq!(keyboard_type_verdict("a\n", Some(&f), Some(&f)), Ok(None));
        assert_eq!(keyboard_type_verdict("\t", Some(&f), Some(&f)), Ok(None));
        assert_eq!(keyboard_type_verdict("  ", Some(&f), Some(&f)), Ok(None));
    }
}

#[cfg(test)]
mod hidden_page_tests {
    use super::{restore_rendering_if_hidden, visibility_reply_is_hidden};
    use crate::native::cdp::client::CdpClient;
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    #[test]
    fn only_a_hidden_reply_counts_as_hidden() {
        let reply = |v: Value| json!({ "result": { "type": "string", "value": v } });
        assert!(visibility_reply_is_hidden(&reply(json!("hidden"))));
        assert!(!visibility_reply_is_hidden(&reply(json!("visible"))));
        assert!(!visibility_reply_is_hidden(&reply(json!("prerender"))));
        assert!(!visibility_reply_is_hidden(&reply(json!(null))));
        assert!(!visibility_reply_is_hidden(&json!({})));
    }

    type Seen = Arc<Mutex<Vec<(String, Value)>>>;

    /// A fake page answering `document.visibilityState` with `state`; returns
    /// the client and every `(method, params)` it was sent.
    async fn fake_page(state: &'static str) -> (CdpClient, Seen) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(msg)) = ws.next().await {
                let Message::Text(text) = msg else { continue };
                let cmd: Value = serde_json::from_str(&text).unwrap();
                let method = cmd["method"].as_str().unwrap_or("").to_string();
                log.lock()
                    .unwrap()
                    .push((method.clone(), cmd["params"].clone()));
                let result = if method == "Runtime.evaluate" {
                    json!({ "result": { "type": "string", "value": state } })
                } else {
                    json!({})
                };
                let reply =
                    json!({ "id": cmd["id"], "result": result, "sessionId": cmd["sessionId"] });
                ws.send(Message::Text(reply.to_string().into()))
                    .await
                    .unwrap();
            }
        });
        let client = CdpClient::connect(&format!("ws://127.0.0.1:{port}"))
            .await
            .unwrap();
        (client, seen)
    }

    /// A hidden page gets focus emulation turned off and on again (sending
    /// `true` alone is ignored by Chrome), on the page's own session.
    #[tokio::test]
    async fn a_hidden_page_has_focus_emulation_reasserted() {
        let (client, seen) = fake_page("hidden").await;
        assert!(restore_rendering_if_hidden(&client, "S1").await);
        let seen = seen.lock().unwrap().clone();
        let methods: Vec<&str> = seen.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(
            methods,
            [
                "Runtime.evaluate",
                "Emulation.setFocusEmulationEnabled",
                "Emulation.setFocusEmulationEnabled"
            ]
        );
        assert_eq!(seen[1].1["enabled"], json!(false));
        assert_eq!(seen[2].1["enabled"], json!(true));
    }

    /// A visible page (the normal case for a background tab) costs one
    /// evaluate and nothing else: no focus/blur churn for the page to see.
    #[tokio::test]
    async fn a_visible_page_is_left_alone() {
        let (client, seen) = fake_page("visible").await;
        assert!(!restore_rendering_if_hidden(&client, "S1").await);
        let seen = seen.lock().unwrap().clone();
        let methods: Vec<&str> = seen.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(methods, ["Runtime.evaluate"]);
    }
}

#[cfg(test)]
mod stale_fill_tests {
    use super::fill_reporting;
    use crate::native::cdp::client::CdpClient;
    use crate::native::element::RefMap;
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    /// When the first element handle (`obj-1`) stops resolving.
    #[derive(Clone, Copy)]
    enum Stale {
        /// After the trusted insert landed: the case the comparison run hit.
        AfterInsert,
        /// Before anything was written.
        FromTheStart,
    }

    /// How reads on the re-resolved handle (`obj-2`) answer.
    #[derive(Clone, Copy, Debug)]
    enum FreshRead {
        Works,
        /// The read itself fails at the CDP level.
        CdpError,
        /// The page-side read reports it could not read the field.
        NotOk,
        /// The read answers without a value.
        Missing,
    }

    #[derive(Default)]
    struct Page {
        value: String,
        inserts: usize,
        resolves: usize,
        /// Calls of the page-side fill function on any handle.
        fills: usize,
    }

    /// A fake page with one text field. Each selector lookup mints a fresh
    /// handle (`obj-1`, `obj-2`, ...); `obj-1` goes stale as `stale` says,
    /// answering the way Chrome does: "Could not find object with given id".
    async fn fake_page(stale: Stale) -> (CdpClient, Arc<Mutex<Page>>) {
        fake_page_reading(stale, FreshRead::Works).await
    }

    async fn fake_page_reading(
        stale: Stale,
        fresh_read: FreshRead,
    ) -> (CdpClient, Arc<Mutex<Page>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let page = Arc::new(Mutex::new(Page::default()));
        let state = page.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(msg)) = ws.next().await {
                let Message::Text(text) = msg else { continue };
                let cmd: Value = serde_json::from_str(&text).unwrap();
                let method = cmd["method"].as_str().unwrap_or("");
                let p = &cmd["params"];
                let reply: Result<Value, &str> = {
                    let mut s = state.lock().unwrap();
                    match method {
                        "Runtime.evaluate" if p["returnByValue"] == json!(false) => {
                            s.resolves += 1;
                            Ok(json!({ "result": { "type": "object",
                            "objectId": format!("obj-{}", s.resolves) } }))
                        }
                        "Runtime.evaluate" => {
                            Ok(json!({ "result": { "type": "boolean", "value": true } }))
                        }
                        "Input.insertText" => {
                            s.value = p["text"].as_str().unwrap_or("").to_string();
                            s.inserts += 1;
                            Ok(json!({}))
                        }
                        "Runtime.callFunctionOn" => {
                            let stale_now = p["objectId"] == json!("obj-1")
                                && match stale {
                                    Stale::AfterInsert => s.inserts > 0,
                                    Stale::FromTheStart => true,
                                };
                            let f = p["functionDeclaration"].as_str().unwrap_or("");
                            if stale_now {
                                Err("Could not find object with given id")
                            } else if f.contains("const v = ") {
                                // The page-side half of fill: focused + selected.
                                s.fills += 1;
                                Ok(
                                    json!({ "result": { "type": "string", "value": "input-trusted" } }),
                                )
                            } else if p["objectId"] == json!("obj-2") {
                                match fresh_read {
                                    FreshRead::Works => Ok(json!({ "result": { "type": "object",
                                        "value": { "ok": true, "value": s.value } } })),
                                    FreshRead::CdpError => Err("Execution context was destroyed."),
                                    FreshRead::NotOk => Ok(json!({ "result": { "type": "object",
                                        "value": { "ok": false } } })),
                                    FreshRead::Missing => Ok(json!({ "result": { "type": "object",
                                        "value": { "ok": true } } })),
                                }
                            } else {
                                // Reads (and the blur tail, whose answer is ignored).
                                Ok(json!({ "result": { "type": "object",
                                "value": { "ok": true, "value": s.value } } }))
                            }
                        }
                        _ => Ok(json!({})),
                    }
                };
                let msg = match reply {
                    Ok(result) => {
                        json!({ "id": cmd["id"], "result": result, "sessionId": cmd["sessionId"] })
                    }
                    Err(e) => json!({ "id": cmd["id"], "error": { "code": -32000, "message": e },
                        "sessionId": cmd["sessionId"] }),
                };
                ws.send(Message::Text(msg.to_string().into()))
                    .await
                    .unwrap();
            }
        });
        let client = CdpClient::connect(&format!("ws://127.0.0.1:{port}"))
            .await
            .unwrap();
        (client, page)
    }

    /// The handle died after the text went in: the field already holds the
    /// value, so it is reported, not typed a second time.
    #[tokio::test]
    async fn a_handle_lost_after_the_insert_is_not_typed_twice() {
        let (client, page) = fake_page(Stale::AfterInsert).await;
        let out = fill_reporting(
            &client,
            "S1",
            &RefMap::new(),
            "#email",
            "alex@example.invalid",
            &HashMap::new(),
        )
        .await
        .expect("a field that holds the value is a success");
        let p = page.lock().unwrap();
        assert_eq!(p.inserts, 1, "must not re-type");
        assert_eq!(p.value, "alex@example.invalid");
        assert_eq!(p.resolves, 2, "re-resolved exactly once");
        assert_eq!(out.engine, "reread");
        let w = out.warning.expect("the stale handle must be reported");
        assert!(w.contains("not re-typed"), "{w}");
    }

    /// The handle died before anything was written: re-resolve, see the field
    /// does not hold the value, and fill exactly once.
    #[tokio::test]
    async fn a_handle_lost_before_the_insert_is_filled_once() {
        let (client, page) = fake_page(Stale::FromTheStart).await;
        let out = fill_reporting(
            &client,
            "S1",
            &RefMap::new(),
            "#email",
            "alex@example.invalid",
            &HashMap::new(),
        )
        .await
        .expect("the second handle works");
        let p = page.lock().unwrap();
        assert_eq!(p.inserts, 1);
        assert_eq!(p.value, "alex@example.invalid");
        assert_eq!(p.resolves, 2);
        let w = out.warning.expect("the retry must be reported");
        assert!(w.contains("filled once more"), "{w}");
    }

    /// A re-resolved field whose value cannot be read is unknown, not
    /// different: nothing is written again and the command fails, saying how
    /// to check. Covers a CDP error on the read, `ok:false`, and a missing value,
    /// whether or not the first attempt had already inserted the text.
    #[tokio::test]
    async fn an_unreadable_field_after_a_stale_handle_is_never_written_again() {
        for stale in [Stale::AfterInsert, Stale::FromTheStart] {
            for read in [FreshRead::CdpError, FreshRead::NotOk, FreshRead::Missing] {
                let (client, page) = fake_page_reading(stale, read).await;
                let err = fill_reporting(
                    &client,
                    "S1",
                    &RefMap::new(),
                    "#email",
                    "alex@example.invalid",
                    &HashMap::new(),
                )
                .await
                .err()
                .unwrap_or_else(|| panic!("{read:?}: an unknown value must fail"));
                let p = page.lock().unwrap();
                let (inserts, fills) = match stale {
                    Stale::AfterInsert => (1, 1),
                    Stale::FromTheStart => (0, 0),
                };
                assert_eq!(p.inserts, inserts, "{read:?}: no second insert");
                assert_eq!(p.fills, fills, "{read:?}: no fill on the fresh handle");
                assert_eq!(p.resolves, 2, "{read:?}: re-resolved exactly once");
                assert!(err.contains("value is unknown"), "{read:?}: {err}");
                assert!(err.contains("get value #email"), "{read:?}: {err}");
            }
        }
    }
}
