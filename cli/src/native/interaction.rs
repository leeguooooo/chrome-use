use std::collections::HashMap;

use serde_json::Value;

use super::cdp::client::CdpClient;
use super::cdp::types::*;
use super::element::{parse_ref, resolve_element_center, resolve_element_object_id, RefMap};
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

pub async fn click(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    button: &str,
    click_count: i32,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    // AGENT_BROWSER_CLICK_MODE: "" (default) = coordinate click with a DOM
    // fallback; "coord" = strict coordinate only (no fallback); "dom" = always
    // dispatch through the DOM.
    let mode = std::env::var("AGENT_BROWSER_CLICK_MODE").unwrap_or_default();

    // (A) Scroll the target into view first so the computed coordinates land
    // inside the viewport. Without this, an element below the fold (or revealed
    // after scroll/popup) yields off-viewport coordinates and the click lands on
    // whatever currently occupies that point. Best-effort: ignore failures.
    scroll_into_view_if_needed(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await;

    if mode == "dom" {
        return dom_click(
            client,
            session_id,
            ref_map,
            selector_or_ref,
            iframe_sessions,
        )
        .await;
    }

    // Over the extension relay we drive the user's real, in-use Chrome, where a
    // coordinate `Input.dispatchMouseEvent` is NOT reliably confined to our target
    // tab — it can be delivered to whatever tab is in the foreground, and an OOPIF
    // element's box can't be mapped to a top-viewport point at all. This twice
    // opened an unrelated tab on the user's busy Chrome (issues #31/#36). So on the
    // relay, never use coordinates for a normal left click: DOM-dispatch invokes
    // the element's click in its own (frame) session, always hitting the right
    // element in the right tab. Double/right clicks still need true pointer
    // semantics, and `coord` mode is an explicit opt-out.
    if mode != "coord" && button == "left" && click_count == 1 && prefer_dom_dispatch(ref_map, selector_or_ref) {
        return dom_click(
            client,
            session_id,
            ref_map,
            selector_or_ref,
            iframe_sessions,
        )
        .await;
    }

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
            if mode != "coord"
                && button == "left"
                && click_count == 1
                && parse_ref(selector_or_ref).is_none()
                && point_misses_element(client, &effective_session_id, selector_or_ref).await
            {
                eprintln!(
                    "[click] target occluded at its click point; dispatching through \
                     the DOM (set AGENT_BROWSER_CLICK_MODE=coord to disable)"
                );
                return dom_click(
                    client,
                    session_id,
                    ref_map,
                    selector_or_ref,
                    iframe_sessions,
                )
                .await;
            }
            // Land on a jittered point inside the element rather than its exact
            // centre (Fast/Human). Zero size or Off → exact centre.
            let (tx, ty) = humanize::landing_point(
                (cx - w / 2.0, cy - h / 2.0, w, h),
                humanize::active_level(),
                humanize::next_seed(),
            );
            dispatch_click(client, &effective_session_id, tx, ty, button, click_count).await
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
            .map_err(|dom_err| format!("{e}\n(DOM-dispatch fallback also failed: {dom_err})"))
        }
    }
}

/// True if a coordinate click at the selector's centre would land on something
/// OTHER than the element (an overlay on top), i.e. the element is occluded.
/// `false` when not occluded, the element is missing, or the probe fails (so we
/// never block a click on a flaky probe — the normal coordinate path runs).
async fn point_misses_element(client: &CdpClient, session_id: &str, selector: &str) -> bool {
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
            return !(hit === el || el.contains(hit) || hit.contains(el));
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
        Ok(r) => r.result.value.and_then(|v| v.as_bool()).unwrap_or(false),
        Err(_) => false,
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
) {
    let Ok((object_id, effective_session_id)) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await
    else {
        return;
    };
    let js = "function() { try { \
        if (typeof this.scrollIntoViewIfNeeded === 'function') { this.scrollIntoViewIfNeeded(true); } \
        else { this.scrollIntoView({ block: 'center', inline: 'center' }); } \
    } catch (e) {} }";
    let _ = client
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
        .await;
    // Let the scroll settle so the following getBoxModel sees final coordinates.
    wait_for_paint_settled(client, &effective_session_id).await;
}

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
    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: "function() { this.click(); }".to_string(),
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
        return dom_dblclick(client, session_id, ref_map, selector_or_ref, iframe_sessions).await;
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
        return dom_hover(client, session_id, ref_map, selector_or_ref, iframe_sessions).await;
    }
    let (x, y, _w, _h, effective_session_id) = resolve_element_center(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;
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
) -> Result<(), String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    // Emulate a real edit so framework-controlled inputs (React/Vue) and
    // site-side listeners actually see the change (issue #25): the old path set
    // `this.value` directly and used Input.insertText, which left React's
    // internal value-tracker out of sync and never fired change/blur — so
    // dependent logic (e.g. Mercari's postal-code → 都道府県 autocomplete) never
    // ran even though the value was visible. Set the value through the element's
    // PROTOTYPE setter (which React's _valueTracker hooks), then dispatch
    // input → change → blur/focusout. `type <sel> <text>` remains for sites that
    // need per-keystroke events.
    let fill_js = format!(
        r#"function() {{
            const el = this;
            const v = {val};
            try {{ el.focus(); }} catch (e) {{}}
            const tag = el.tagName;
            const fire = (type, ctor) => el.dispatchEvent(new (ctor || Event)(type, {{ bubbles: true }}));
            if (tag === 'SELECT') {{
                el.value = v; fire('input'); fire('change'); return true;
            }}
            if (el.isContentEditable) {{
                el.textContent = v; fire('input', window.InputEvent || Event); fire('change');
                try {{ el.blur(); }} catch (e) {{}} fire('focusout'); return true;
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
            return true;
        }}"#,
        val = serde_json::to_string(value).unwrap_or_default()
    );

    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: fill_js,
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
        client
            .send_command_typed::<_, Value>(
                "Runtime.callFunctionOn",
                &CallFunctionOnParams {
                    function_declaration: r#"function() {
                        this.select && this.select();
                        this.value = '';
                        this.dispatchEvent(new Event('input', { bubbles: true }));
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

pub async fn press_key(client: &CdpClient, session_id: &str, key: &str) -> Result<(), String> {
    press_key_with_modifiers(client, session_id, key, None).await
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
    let (key_name, code, key_code) = named_key_info(key);
    // Printable text is only meaningful on key-down; key-up never inserts.
    let text = if event_type == "keyDown" {
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
                modifiers: None,
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

    let js = r#"function(vals) {
            const options = Array.from(this.options);
            for (const opt of options) {
                opt.selected = vals.includes(opt.value) || vals.includes(opt.textContent.trim());
            }
            this.dispatchEvent(new Event('change', { bubbles: true }));
        }"#
    .to_string();

    client
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
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

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

    Ok(())
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
    let script = "new Promise(resolve => \
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
