//! DOM-walk fallback for `snapshot` (issue #206).
//!
//! `snapshot` reads the CDP accessibility tree, which normally pierces open AND
//! closed shadow roots. On some web-component SPAs (developer.apple.com/contact
//! — a fully rendered page whose entire content lives inside shadow roots) the
//! AX tree nevertheless comes back as a handful of bare `generic` nodes, so
//! `snapshot` printed three lines and `snapshot -i` said "(no interactive
//! elements)" while a screenshot showed a normal page and the agent had to
//! hand-write a recursive shadow-root walker in `eval` to get anything done.
//!
//! This module builds the same kind of listing from `DOM.getDocument(pierce:
//! true)` — which reaches open and closed shadow roots and same-process child
//! documents — with refs keyed by `backendNodeId`, so `click @ref` / `type
//! @ref` / `fill @ref` work exactly as with AX-minted refs. The tree walk and
//! rendering are pure so they can be unit-tested on a synthetic DOM.Node JSON.

use serde_json::Value;

/// One interactive (or heading) element found by the DOM walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomElem {
    pub backend_node_id: i64,
    pub role: String,
    pub name: String,
    /// `heading` level (h1..h6) when the element is a heading.
    pub level: Option<u8>,
    pub disabled: bool,
    /// Nesting depth of the child document the element sits in (0 = main).
    pub frame_depth: usize,
    /// Title/URL of the child document, set on the first element of each frame
    /// so the renderer can print an `iframe` header line.
    pub frame_label: Option<String>,
    /// Extra attributes worth printing: `[checked]`, `[selected]`, `[expanded]`.
    pub attrs: Vec<String>,
}

/// Walk a `DOM.getDocument` tree (any depth, `pierce: true`) and collect the
/// elements an agent can act on, in document order.
pub fn collect_interactive(doc: &Value) -> Vec<DomElem> {
    let mut labels = std::collections::HashMap::new();
    collect_label_texts(doc, &mut labels);
    let mut out = Vec::new();
    walk(doc, &labels, 0, &mut None, &mut out);
    out
}

/// Render the collected elements as snapshot lines (`- role "name" [ref=eN]`).
/// `refs` must be parallel to `elems` (the ref id minted for each).
pub fn render(elems: &[DomElem], refs: &[String]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for (el, ref_id) in elems.iter().zip(refs) {
        if let Some(label) = &el.frame_label {
            let indent = "  ".repeat(el.frame_depth.saturating_sub(1));
            if label.is_empty() {
                lines.push(format!("{indent}- iframe"));
            } else {
                lines.push(format!("{indent}- iframe {:?}", label));
            }
        }
        let indent = "  ".repeat(el.frame_depth);
        let mut attrs: Vec<String> = Vec::new();
        if let Some(l) = el.level {
            attrs.push(format!("level={l}"));
        }
        attrs.extend(el.attrs.iter().cloned());
        if el.disabled {
            attrs.push("disabled".to_string());
        }
        attrs.push(format!("ref={ref_id}"));
        let name = if el.name.is_empty() {
            String::new()
        } else {
            format!(" {:?}", el.name)
        };
        lines.push(format!(
            "{indent}- {}{name} [{}]",
            el.role,
            attrs.join(", ")
        ));
    }
    lines.join("\n")
}

fn attr<'a>(node: &'a Value, name: &str) -> Option<&'a str> {
    let attrs = node.get("attributes")?.as_array()?;
    let mut i = 0;
    while i + 1 < attrs.len() {
        if attrs[i].as_str()?.eq_ignore_ascii_case(name) {
            return attrs[i + 1].as_str();
        }
        i += 2;
    }
    None
}

fn has_attr(node: &Value, name: &str) -> bool {
    attr(node, name).is_some()
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cap(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        s
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    }
}

/// Text of a subtree (pierced), skipping script/style/template/noscript.
fn text_of(node: &Value, out: &mut String) {
    let node_type = node.get("nodeType").and_then(Value::as_i64).unwrap_or(0);
    let name = node.get("nodeName").and_then(Value::as_str).unwrap_or("");
    if node_type == 3 {
        if let Some(t) = node.get("nodeValue").and_then(Value::as_str) {
            out.push(' ');
            out.push_str(t);
        }
        return;
    }
    if node_type == 1 && is_skipped_tag(name) {
        return;
    }
    if node_type == 1 && attr(node, "aria-hidden") == Some("true") {
        return;
    }
    if node_type == 1 && name.eq_ignore_ascii_case("IMG") {
        if let Some(alt) = attr(node, "alt") {
            out.push(' ');
            out.push_str(alt);
        }
        return;
    }
    for child in node
        .get("children")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        text_of(child, out);
    }
    for sr in node
        .get("shadowRoots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        text_of(sr, out);
    }
}

fn is_skipped_tag(tag: &str) -> bool {
    matches!(
        tag.to_ascii_uppercase().as_str(),
        "SCRIPT" | "STYLE" | "TEMPLATE" | "NOSCRIPT" | "HEAD" | "TITLE" | "META" | "LINK"
    )
}

/// `<label for=id>` texts, so an input can be named by its label.
fn collect_label_texts(node: &Value, out: &mut std::collections::HashMap<String, String>) {
    let name = node.get("nodeName").and_then(Value::as_str).unwrap_or("");
    if name.eq_ignore_ascii_case("LABEL") {
        if let Some(target) = attr(node, "for") {
            let mut t = String::new();
            text_of(node, &mut t);
            let t = norm(&t);
            if !t.is_empty() {
                out.entry(target.to_string()).or_insert(t);
            }
        }
    }
    for child in node
        .get("children")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        collect_label_texts(child, out);
    }
    for sr in node
        .get("shadowRoots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        collect_label_texts(sr, out);
    }
    if let Some(doc) = node.get("contentDocument") {
        collect_label_texts(doc, out);
    }
}

fn hidden_by_style(node: &Value) -> bool {
    if has_attr(node, "hidden") || attr(node, "aria-hidden") == Some("true") {
        return true;
    }
    let style = attr(node, "style")
        .unwrap_or("")
        .to_ascii_lowercase()
        .replace(' ', "");
    style.contains("display:none") || style.contains("visibility:hidden")
}

/// Role + whether the element is one we list, from tag/type/role attributes.
/// Mirrors the AX roles `snapshot -i` prints so refs read the same either way.
fn classify(node: &Value, labels: &std::collections::HashMap<String, String>) -> Option<DomElem> {
    let tag = node
        .get("nodeName")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_uppercase();
    let explicit_role = attr(node, "role").map(|r| r.trim().to_ascii_lowercase());
    let mut level = None;
    let mut attrs = Vec::new();
    let role: String = match explicit_role.as_deref() {
        Some(r)
            if crate::native::snapshot::is_interactive_role(r)
                || matches!(r, "heading" | "dialog" | "alertdialog") =>
        {
            if r == "heading" {
                level = attr(node, "aria-level").and_then(|l| l.parse().ok());
            }
            r.to_string()
        }
        Some("presentation") | Some("none") => return None,
        _ => match tag.as_str() {
            "A" | "AREA" => {
                if has_attr(node, "href") {
                    "link".to_string()
                } else {
                    return None;
                }
            }
            "BUTTON" | "SUMMARY" => "button".to_string(),
            "INPUT" => match attr(node, "type")
                .unwrap_or("text")
                .to_ascii_lowercase()
                .as_str()
            {
                "hidden" => return None,
                "checkbox" => "checkbox".to_string(),
                "radio" => "radio".to_string(),
                "submit" | "button" | "reset" | "image" => "button".to_string(),
                "range" => "slider".to_string(),
                "number" => "spinbutton".to_string(),
                "search" => "searchbox".to_string(),
                "file" => "button".to_string(),
                _ => "textbox".to_string(),
            },
            "TEXTAREA" => "textbox".to_string(),
            "SELECT" => "combobox".to_string(),
            "OPTION" => "option".to_string(),
            "H1" | "H2" | "H3" | "H4" | "H5" | "H6" => {
                level = tag[1..].parse().ok();
                "heading".to_string()
            }
            _ => {
                let ce = attr(node, "contenteditable").map(|v| v.to_ascii_lowercase());
                if matches!(
                    ce.as_deref(),
                    Some("") | Some("true") | Some("plaintext-only")
                ) {
                    "textbox".to_string()
                } else if has_attr(node, "onclick") {
                    "button".to_string()
                } else {
                    return None;
                }
            }
        },
    };
    if hidden_by_style(node) {
        return None;
    }
    let backend_node_id = node.get("backendNodeId").and_then(Value::as_i64)?;

    // Accessible name, in roughly the AX precedence: aria-label, <label for>,
    // placeholder, value (buttons), title, then visible text.
    let mut name = attr(node, "aria-label").map(norm).unwrap_or_default();
    if name.is_empty() {
        if let Some(id) = attr(node, "id") {
            if let Some(l) = labels.get(id) {
                name = l.clone();
            }
        }
    }
    if name.is_empty() {
        if let Some(p) = attr(node, "placeholder") {
            name = norm(p);
        }
    }
    if name.is_empty() && role == "button" && tag == "INPUT" {
        if let Some(v) = attr(node, "value") {
            name = norm(v);
        }
    }
    if name.is_empty() {
        if let Some(t) = attr(node, "title") {
            name = norm(t);
        }
    }
    if name.is_empty() && !matches!(role.as_str(), "textbox" | "searchbox" | "combobox") {
        let mut t = String::new();
        text_of(node, &mut t);
        name = norm(&t);
    }
    if name.is_empty() {
        if let Some(n) = attr(node, "name") {
            name = norm(n);
        }
    }
    let name = cap(name, 80);

    if role == "checkbox" || role == "radio" {
        if has_attr(node, "checked") || attr(node, "aria-checked") == Some("true") {
            attrs.push("checked".to_string());
        }
    }
    if role == "option" && has_attr(node, "selected") {
        attrs.push("selected".to_string());
    }
    if let Some(exp) = attr(node, "aria-expanded") {
        attrs.push(format!("expanded={exp}"));
    }
    if has_attr(node, "required") || attr(node, "aria-required") == Some("true") {
        attrs.push("required".to_string());
    }
    let disabled = has_attr(node, "disabled") || attr(node, "aria-disabled") == Some("true");

    Some(DomElem {
        backend_node_id,
        role,
        name,
        level,
        disabled,
        frame_depth: 0,
        frame_label: None,
        attrs,
    })
}

fn walk(
    node: &Value,
    labels: &std::collections::HashMap<String, String>,
    frame_depth: usize,
    pending_frame_label: &mut Option<String>,
    out: &mut Vec<DomElem>,
) {
    let node_type = node.get("nodeType").and_then(Value::as_i64).unwrap_or(0);
    let tag = node.get("nodeName").and_then(Value::as_str).unwrap_or("");
    if node_type == 1 {
        if is_skipped_tag(tag) {
            return;
        }
        if let Some(mut el) = classify(node, labels) {
            el.frame_depth = frame_depth;
            el.frame_label = pending_frame_label.take();
            out.push(el);
        }
        // A hidden container hides everything under it; listing its controls
        // would offer refs that can't be clicked.
        if hidden_by_style(node) {
            return;
        }
    }
    for child in node
        .get("children")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        walk(child, labels, frame_depth, pending_frame_label, out);
    }
    for sr in node
        .get("shadowRoots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        walk(sr, labels, frame_depth, pending_frame_label, out);
    }
    if let Some(doc) = node.get("contentDocument") {
        let title = attr(node, "title")
            .or_else(|| attr(node, "name"))
            .or_else(|| attr(node, "src"))
            .unwrap_or("")
            .to_string();
        let mut label = Some(cap(norm(&title), 60));
        walk(doc, labels, frame_depth + 1, &mut label, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn el(tag: &str, bid: i64, attrs: &[(&str, &str)], children: Vec<Value>) -> Value {
        let flat: Vec<Value> = attrs
            .iter()
            .flat_map(|(k, v)| [json!(k), json!(v)])
            .collect();
        json!({ "nodeType": 1, "nodeName": tag, "backendNodeId": bid, "attributes": flat, "children": children })
    }
    fn text(t: &str) -> Value {
        json!({ "nodeType": 3, "nodeName": "#text", "nodeValue": t })
    }
    fn shadow(children: Vec<Value>) -> Value {
        json!({ "nodeType": 11, "nodeName": "#document-fragment", "children": children })
    }

    #[test]
    fn lists_controls_inside_shadow_roots_with_names_and_refs() {
        // developer.apple.com/contact shape: everything under a custom element's
        // shadow root, light DOM empty.
        let mut host = el("APP-ROOT", 1, &[], vec![]);
        host["shadowRoots"] = json!([shadow(vec![
            el("H1", 2, &[], vec![text("Contact Us")]),
            el(
                "A",
                3,
                &[("href", "/topic")],
                vec![text(" Developer  Program ")]
            ),
            el("BUTTON", 4, &[("disabled", "")], vec![text("Continue")]),
            el("LABEL", 5, &[("for", "email")], vec![text("Email address")]),
            el(
                "INPUT",
                6,
                &[("id", "email"), ("type", "email"), ("required", "")],
                vec![]
            ),
            el("INPUT", 7, &[("type", "hidden"), ("name", "csrf")], vec![]),
            el(
                "DIV",
                8,
                &[("style", "display: none")],
                vec![el("BUTTON", 9, &[], vec![text("hidden")])]
            ),
            el("SCRIPT", 10, &[], vec![text("var x = 1")]),
        ])]);
        let doc = json!({ "nodeType": 9, "nodeName": "#document", "children": [el("BODY", 0, &[], vec![host])] });
        let elems = collect_interactive(&doc);
        let refs: Vec<String> = (1..=elems.len()).map(|i| format!("e{i}")).collect();
        let out = render(&elems, &refs);
        assert_eq!(
            out,
            "- heading \"Contact Us\" [level=1, ref=e1]\n\
             - link \"Developer Program\" [ref=e2]\n\
             - button \"Continue\" [disabled, ref=e3]\n\
             - textbox \"Email address\" [required, ref=e4]"
        );
        assert_eq!(elems[3].backend_node_id, 6);
    }

    #[test]
    fn roles_from_type_role_and_contenteditable() {
        let body = el(
            "BODY",
            0,
            &[],
            vec![
                el(
                    "INPUT",
                    1,
                    &[
                        ("type", "checkbox"),
                        ("checked", ""),
                        ("aria-label", "Agree"),
                    ],
                    vec![],
                ),
                el("INPUT", 2, &[("type", "submit"), ("value", "Send")], vec![]),
                el("DIV", 3, &[("role", "button")], vec![text("Custom")]),
                el(
                    "DIV",
                    4,
                    &[("contenteditable", "true"), ("placeholder", "Write…")],
                    vec![],
                ),
                el(
                    "SELECT",
                    5,
                    &[("name", "country")],
                    vec![el("OPTION", 6, &[("selected", "")], vec![text("JP")])],
                ),
                el("DIV", 7, &[("role", "presentation")], vec![text("nope")]),
                el("SPAN", 8, &[], vec![text("plain")]),
                el("A", 9, &[], vec![text("no href")]),
            ],
        );
        let doc = json!({ "nodeType": 9, "nodeName": "#document", "children": [body] });
        let elems = collect_interactive(&doc);
        let got: Vec<(String, String)> = elems
            .iter()
            .map(|e| (e.role.clone(), e.name.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("checkbox".into(), "Agree".into()),
                ("button".into(), "Send".into()),
                ("button".into(), "Custom".into()),
                ("textbox".into(), "Write…".into()),
                ("combobox".into(), "country".into()),
                ("option".into(), "JP".into()),
            ]
        );
        assert_eq!(elems[0].attrs, vec!["checked".to_string()]);
    }

    #[test]
    fn child_documents_get_an_iframe_header_and_indent() {
        let mut iframe = el("IFRAME", 1, &[("title", "Sign in")], vec![]);
        iframe["contentDocument"] = json!({ "nodeType": 9, "nodeName": "#document", "children": [
            el("BODY", 2, &[], vec![el("BUTTON", 3, &[], vec![text("Go")])])
        ]});
        let doc = json!({ "nodeType": 9, "nodeName": "#document", "children": [el("BODY", 0, &[], vec![iframe])] });
        let elems = collect_interactive(&doc);
        let out = render(&elems, &["e1".to_string()]);
        assert_eq!(out, "- iframe \"Sign in\"\n  - button \"Go\" [ref=e1]");
    }

    #[test]
    fn empty_dom_yields_nothing() {
        let doc = json!({ "nodeType": 9, "nodeName": "#document", "children": [el("BODY", 0, &[], vec![text("hi")])] });
        assert!(collect_interactive(&doc).is_empty());
        assert_eq!(render(&[], &[]), "");
    }
}
