//! Strict, scoped semantic discovery. Names and roles come from Chrome AX,
//! rather than a second implementation of the accessible-name standard.
use super::{actions::DaemonState, cdp::client::CdpClient, element};
use serde_json::{json, Value};

const LIMIT: usize = 256;
const GROUP: &str = "chrome-use-semantic-locator";

async fn call(
    client: &CdpClient,
    sid: &str,
    object: &str,
    function: &str,
    arguments: Value,
) -> Result<Value, String> {
    let reply = client
        .send_command(
            "Runtime.callFunctionOn",
            Some(json!({
                "objectId": object, "functionDeclaration": function, "arguments": arguments,
                "returnByValue": true,
            })),
            Some(sid),
        )
        .await?;
    if reply.get("exceptionDetails").is_some() {
        return Err(
            "semantic locator could not inspect a live element; nothing was dispatched".into(),
        );
    }
    Ok(reply["result"]["value"].clone())
}

async fn array_objects(
    client: &CdpClient,
    sid: &str,
    expression: &str,
    scope: Option<&str>,
) -> Result<Vec<String>, String> {
    let reply = if let Some(scope) = scope {
        client.send_command("Runtime.callFunctionOn", Some(json!({
            "objectId": scope, "functionDeclaration": format!("function(){{return ({expression}).filter(e=>this.contains(e));}}"),
            "objectGroup": GROUP, "returnByValue": false,
        })), Some(sid)).await?
    } else {
        client
            .send_command(
                "Runtime.evaluate",
                Some(json!({
                    "expression": expression, "objectGroup": GROUP, "returnByValue": false,
                })),
                Some(sid),
            )
            .await?
    };
    objects_from_reply(client, sid, reply).await
}

async fn objects_from_reply(
    client: &CdpClient,
    sid: &str,
    reply: Value,
) -> Result<Vec<String>, String> {
    if reply.get("exceptionDetails").is_some() {
        return Err("invalid scope, selector, or clickable ancestor outside the scope; nothing was dispatched".into());
    }
    let object = reply["result"]["objectId"]
        .as_str()
        .ok_or("locator query returned no array")?;
    let props = client
        .send_command(
            "Runtime.getProperties",
            Some(json!({"objectId": object, "ownProperties": true})),
            Some(sid),
        )
        .await?;
    let rows = props["result"]
        .as_array()
        .ok_or("locator query returned invalid properties")?;
    let length = rows
        .iter()
        .find(|p| p["name"] == "length")
        .and_then(|p| p["value"]["value"].as_u64())
        .unwrap_or(0);
    if length > LIMIT as u64 {
        return Err("too many semantic candidates (over 256); narrow --within or the locator; nothing was dispatched".into());
    }
    let mut objects = Vec::new();
    for row in rows {
        if row["name"]
            .as_str()
            .is_some_and(|s| s.parse::<usize>().is_ok())
        {
            if let Some(id) = row["value"]["objectId"].as_str() {
                objects.push(id.to_string());
            }
        }
    }
    Ok(objects)
}

/// Locate a unique visible node; read-only locate also reports ambiguity as an
/// error with bounded candidates. No input values or supplied fill text appear
/// in diagnostics. Hidden nodes are not a disambiguator for visible duplicates.
pub struct Located {
    pub extra: Value,
    pub pin: element::SemanticPin,
}

pub async fn locate(cmd: &Value, state: &DaemonState) -> Result<Located, String> {
    if state.active_frame_id.is_some() {
        return Err("semantic find currently supports the main document only; use the frame's direct @refs or frame main before locating; nothing was dispatched".into());
    }
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let sid = mgr.active_session_id()?.to_string();
    let _ = mgr
        .client
        .send_command(
            "Runtime.releaseObjectGroup",
            Some(json!({"objectGroup": GROUP})),
            Some(&sid),
        )
        .await;
    let result = locate_inner(cmd, state, &sid, true).await;
    let _ = mgr
        .client
        .send_command(
            "Runtime.releaseObjectGroup",
            Some(json!({"objectGroup": GROUP})),
            Some(&sid),
        )
        .await;
    result
}

async fn locate_inner(
    cmd: &Value,
    state: &DaemonState,
    sid: &str,
    mark_target: bool,
) -> Result<Located, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let client = &mgr.client;
    let scope = if let Some(within) = cmd["within"].as_str() {
        if let Some(ref_id) = element::parse_ref(within) {
            if state
                .ref_map
                .get(&ref_id)
                .is_some_and(|entry| entry.frame_id.is_some())
            {
                return Err("--within ref belongs to an iframe; use its existing direct @ref actions, or a scope in the active document; nothing was dispatched".into());
            }
            let (object, frame_sid) = element::resolve_element_object_id(
                client,
                sid,
                &state.ref_map,
                within,
                &state.iframe_sessions,
            )
            .await.map_err(|_| "scope identity is unavailable; take a fresh snapshot and select the scope again; nothing was dispatched".to_string())?;
            if frame_sid != sid {
                return Err("--within ref belongs to another frame; use a scope in the active document; nothing was dispatched".into());
            }
            object
        } else {
            let objects = array_objects(
                client,
                sid,
                &format!("Array.from(document.querySelectorAll({}))", json!(within)),
                None,
            )
            .await?;
            if objects.len() != 1 {
                return Err(format!("--within must identify exactly one container, matched {}; narrow its CSS selector; nothing was dispatched", objects.len()));
            }
            objects[0].clone()
        }
    } else {
        let objects = array_objects(client, sid, "[document.documentElement]", None).await?;
        objects.first().ok_or("document has no root")?.clone()
    };
    if call(
        client,
        sid,
        &scope,
        "function(){return !!this.isConnected}",
        json!([]),
    )
    .await?
        != true
    {
        return Err("scope is detached; read the screen again; nothing was dispatched".into());
    }
    let action = cmd["action"].as_str().unwrap_or("");
    let exact = cmd["exact"].as_bool().unwrap_or(false);
    let mut candidates: Vec<(String, Option<(String, String)>)> = Vec::new();
    let mut ax_descriptions = std::collections::HashMap::new();
    if action == "getbyrole" || action == "getbylabel" {
        let described = client
            .send_command(
                "DOM.describeNode",
                Some(json!({"objectId": scope})),
                Some(sid),
            )
            .await?;
        let root_id = described["node"]["backendNodeId"]
            .as_i64()
            .ok_or("scope has no DOM identity")?;
        let tree = client
            .send_command(
                "Accessibility.queryAXTree",
                Some(json!({"backendNodeId": root_id})),
                Some(sid),
            )
            .await?;
        let nodes = tree["nodes"]
            .as_array()
            .ok_or("Chrome did not return accessible candidates")?;
        for node in nodes {
            if node["ignored"].as_bool() == Some(true) {
                continue;
            }
            let role = node["role"]["value"].as_str().unwrap_or("");
            let name = node["name"]["value"].as_str().unwrap_or("");
            let role_match = if action == "getbyrole" {
                let requested = cmd["role"].as_str().unwrap_or("");
                role == requested || (requested == "img" && role == "image")
            } else {
                ![
                    "StaticText",
                    "InlineTextBox",
                    "RootWebArea",
                    "generic",
                    "none",
                ]
                .contains(&role)
            };
            let wanted = if action == "getbyrole" {
                cmd["name"].as_str()
            } else {
                cmd["label"].as_str()
            };
            if !role_match
                || !wanted
                    .map(|w| if exact { name == w } else { name.contains(w) })
                    .unwrap_or(true)
            {
                continue;
            }
            let Some(backend) = node["backendDOMNodeId"].as_i64() else {
                continue;
            };
            if candidates.len() == LIMIT {
                return Err(
                    "too many semantic candidates; narrow --within; nothing was dispatched".into(),
                );
            }
            ax_descriptions.insert(backend, (role.to_string(), name.to_string()));
            let resolved = client
                .send_command(
                    "DOM.resolveNode",
                    Some(json!({"backendNodeId": backend, "objectGroup": GROUP})),
                    Some(sid),
                )
                .await?;
            if let Some(object) = resolved["object"]["objectId"].as_str() {
                candidates.push((
                    object.to_string(),
                    Some((role.to_string(), name.to_string())),
                ));
            }
        }
    } else {
        let (attr, want) = match action {
            "getbyplaceholder" => ("placeholder", &cmd["placeholder"]),
            "getbyalttext" => ("alt", &cmd["text"]),
            "getbytitle" => ("title", &cmd["text"]),
            "getbytestid" => ("data-testid", &cmd["testId"]),
            _ => ("", &cmd["text"]),
        };
        let want = want.as_str().ok_or("locator needs text")?;
        let script = if attr.is_empty() {
            format!(
                r#"(() => {{ const want={want}; const norm=s=>String(s||'').replace(/\s+/g,' ').trim(); const matches=e=>{cmp}; const skipped=new Set(['SCRIPT','STYLE','NOSCRIPT','TEMPLATE','HEAD']); return Array.from(document.querySelectorAll('body *')).filter(e=>!skipped.has(e.tagName) && matches(e) && !Array.from(e.children).some(matches)); }})()"#,
                want = json!(want),
                cmp = if exact {
                    "norm(e.textContent) === norm(want)"
                } else {
                    "norm(e.textContent).includes(norm(want))"
                }
            )
        } else {
            format!("Array.from(document.querySelectorAll('[{attr}]')).filter(e=> {{ const v=e.getAttribute({attr_json}) || ''; return {cmp}; }})", attr=attr, attr_json=json!(attr), cmp=if exact || action == "getbytestid" {format!("v === {}",json!(want))}else{format!("v.includes({})",json!(want))})
        };
        candidates.extend(
            array_objects(client, sid, &script, Some(&scope))
                .await?
                .into_iter()
                .map(|o| (o, None)),
        );
    }
    // Collapse text leaves to their actual clickable target before strictness.
    // Two spans in one button identify one dispatch target, while two buttons
    // remain ambiguous. A clickable ancestor outside --within is never used.
    if cmd["subaction"] == "click" {
        let arguments: Vec<Value> = candidates
            .iter()
            .map(|(object, _)| json!({"objectId":object}))
            .collect();
        let reply = client.send_command("Runtime.callFunctionOn", Some(json!({
            "objectId": scope,
            "functionDeclaration": "function(...elements){const targets=new Set();for(const element of elements){if(!element.isConnected || !this.contains(element))continue;const box=element.getBoundingClientRect();if(!(box.width>0 && box.height>0) || (element.checkVisibility && !element.checkVisibility({checkOpacity:true,checkVisibilityCSS:true})))continue;const target=element.closest('a[href],button,summary,label,select,input,textarea,[role=button],[role=link],[role=menuitem],[role=tab],[role=option],[onclick]') || element;if(!this.contains(target))throw new Error('clickable ancestor outside scope');targets.add(target);}return Array.from(targets);}",
            "arguments": arguments, "objectGroup": GROUP, "returnByValue": false,
        })), Some(sid)).await?;
        candidates = objects_from_reply(client, sid, reply)
            .await?
            .into_iter()
            .map(|object| (object, None))
            .collect();
    }
    let inspect = r#"function(scope,label){ if(label && !(this.labels?.length || this.hasAttribute('aria-label') || this.hasAttribute('aria-labelledby'))) return null; if(!this.isConnected || !scope.isConnected || !scope.contains(this)) return null; const safeText=element=>{if(element.isContentEditable || ['textbox','searchbox'].includes(element.getAttribute('role')) || ['INPUT','TEXTAREA','SELECT'].includes(element.tagName))return '';const copy=element.cloneNode(true);copy.querySelectorAll('input,textarea,select,[contenteditable],[role=textbox],[role=searchbox]').forEach(e=>e.remove());return copy.textContent || '';}; const r=this.getBoundingClientRect(); const visible=r.width>0 && r.height>0 && (typeof this.checkVisibility!=='function' || this.checkVisibility({checkOpacity:true,checkVisibilityCSS:true})); const editorSelector='input,textarea,select,[contenteditable],[role=textbox],[role=searchbox]';const ids=(this.getAttribute('aria-labelledby') || '').trim();const explicitSafe=!!(this.getAttribute('aria-label') || '').trim() || !!ids && ids.split(/\s+/).every(id=>{const label=this.ownerDocument.getElementById(id);return label && !label.isContentEditable && !label.matches(editorSelector) && !label.querySelector(editorSelector);});return {_explicitNameSafe:explicitSafe,_editableDescendants:!!this.querySelector(editorSelector), tag:this.tagName.toLowerCase(), visible, role:this.getAttribute('role') || (this.tagName==='BUTTON' ? 'button' : ''), name:(this.getAttribute('aria-label') || this.getAttribute('title') || (safeText(this)) || '').slice(0,80), selector:this.id ? '#' + CSS.escape(this.id) : this.tagName.toLowerCase(), context:(()=>{const heading=this.closest('article,section,fieldset,[role=group]')?.querySelector('h1,h2,h3,legend');if(!heading || heading.isContentEditable || ['textbox','searchbox'].includes(heading.getAttribute('role')))return '';return safeText(heading).trim().slice(0,80);})()}; }"#;
    let mut visible = Vec::new();
    let mut matched_count = 0;
    let mut details = Vec::new();
    let mut selected_description = None;
    for (object, ax) in candidates {
        let mut info = call(
            client,
            sid,
            &object,
            inspect,
            json!([{"objectId": scope}, {"value": action == "getbylabel"}]),
        )
        .await?;
        if info.is_null() {
            continue;
        }
        matched_count += 1;
        if let Some((role, name)) = ax {
            info["role"] = json!(role);
            if info["_editableDescendants"] != true || info["_explicitNameSafe"] == true {
                info["name"] = json!(name.chars().take(80).collect::<String>());
            }
        }
        if info["visible"] == true {
            visible.push(object.clone());
            selected_description = Some(info.clone());
        }
        info.as_object_mut().unwrap().remove("_editableDescendants");
        info.as_object_mut().unwrap().remove("_explicitNameSafe");
        if details.len() < 8 {
            details.push(info);
        }
    }
    if visible.is_empty() {
        return Err("No element found: semantic locator has no visible match in its scope; nothing was dispatched".into());
    }
    if visible.len() != 1 {
        return Err(format!("semantic locator matched {} visible elements; nothing was dispatched. Narrow --within or --name/--exact, or deliberately use find first/nth. Candidates (at most 8; values omitted): {}", visible.len(), json!(details)));
    }
    let described = client
        .send_command(
            "DOM.describeNode",
            Some(json!({"objectId":visible[0]})),
            Some(sid),
        )
        .await?;
    let scope_described = client
        .send_command(
            "DOM.describeNode",
            Some(json!({"objectId":scope})),
            Some(sid),
        )
        .await?;
    let pin = element::SemanticPin {
        session: sid.to_string(),
        scope: scope_described["node"]["backendNodeId"]
            .as_i64()
            .ok_or("scope identity unavailable")?,
        target: described["node"]["backendNodeId"]
            .as_i64()
            .ok_or("target identity unavailable")?,
    };
    let mut description = selected_description.ok_or("visible target description unavailable")?;
    if let Some((role, name)) = ax_descriptions.get(&pin.target) {
        description["role"] = json!(role);
        if description["_editableDescendants"] != true || description["_explicitNameSafe"] == true {
            description["name"] = json!(name.chars().take(80).collect::<String>());
        }
    }
    description
        .as_object_mut()
        .unwrap()
        .remove("_editableDescendants");
    description
        .as_object_mut()
        .unwrap()
        .remove("_explicitNameSafe");
    if mark_target {
        let click = cmd["subaction"] == "click";
        let mark = call(client, sid, &visible[0], r#"function(scope,click){if(!this.isConnected || !scope.isConnected || !scope.contains(this))return false; let target=this;if(click){const ancestor=this.closest('a[href],button,summary,label,select,input,textarea,[role=button],[role=link],[role=menuitem],[role=tab],[role=option],[onclick]');if(ancestor){if(!scope.contains(ancestor))return false;target=ancestor;}}document.querySelectorAll('[data-chrome-use-located]').forEach(e=>e.removeAttribute('data-chrome-use-located'));target.setAttribute('data-chrome-use-located','true');return true;}"#, json!([{"objectId": scope},{"value":click}])).await?;
        if mark != true {
            return Err("semantic target changed before dispatch; read the screen and retry discovery; nothing was dispatched".into());
        }
    }
    Ok(Located {
        extra: json!({"count": matched_count, "visibleCount": 1, "selectedDescription":description}),
        pin,
    })
}

/// Reconfirm uniqueness after marker observers have run, without marking again.
/// A replacement is not a continuation of the original target's identity.
pub async fn revalidate(
    cmd: &Value,
    state: &DaemonState,
    pin: &element::SemanticPin,
) -> Result<(), String> {
    let current = locate_inner(cmd, state, &pin.session, false)
        .await
        .map_err(|error| {
            if error.starts_with("No element found") {
                "semantic target changed after selection; refusing dispatch".to_string()
            } else {
                error
            }
        })?;
    if current.pin != *pin {
        return Err(
            "semantic target or scope identity changed before dispatch; no action was resent"
                .into(),
        );
    }
    Ok(())
}
