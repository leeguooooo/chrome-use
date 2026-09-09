use std::collections::HashMap;

use serde_json::Value;

use super::adaptive::ElementFingerprint;
use super::cdp::client::CdpClient;
use super::cdp::types::{
    AXNode, AXProperty, AXValue, EvaluateParams, EvaluateResult, GetFullAXTreeResult,
};
use super::element::{resolve_ax_session, RefMap};

const INTERACTIVE_ROLES: &[&str] = &[
    "button",
    "link",
    "textbox",
    "checkbox",
    "radio",
    "combobox",
    "listbox",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "searchbox",
    "slider",
    "spinbutton",
    "switch",
    "tab",
    "treeitem",
    // `<summary>`. Chrome computes this role for the one control whose entire
    // purpose is to be clicked, yet it was absent — so a disclosure got no ref
    // and could not be expanded through `@ref` at all.
    "DisclosureTriangle",
    "Iframe",
];

/// Whether `role` is one `snapshot -i` lists as actionable (shared with the
/// DOM-walk fallback so both paths label controls the same way).
pub fn is_interactive_role(role: &str) -> bool {
    INTERACTIVE_ROLES.contains(&role)
}

const CONTENT_ROLES: &[&str] = &[
    "heading",
    "cell",
    "gridcell",
    "columnheader",
    "rowheader",
    "listitem",
    "article",
    "region",
    "main",
    "navigation",
];

const STRUCTURAL_ROLES: &[&str] = &[
    "generic",
    "group",
    "list",
    "table",
    "row",
    "rowgroup",
    "grid",
    "treegrid",
    "menu",
    "menubar",
    "toolbar",
    "tablist",
    "tree",
    "directory",
    "document",
    "application",
    "presentation",
    "none",
    "WebArea",
    "RootWebArea",
];

const INVISIBLE_CHARS: &[char] = &[
    '\u{FEFF}', // BOM / Zero Width No-Break Space
    '\u{200B}', // Zero Width Space
    '\u{200C}', // Zero Width Non-Joiner
    '\u{200D}', // Zero Width Joiner
    '\u{2060}', // Word Joiner
    '\u{00A0}', // Non-Breaking Space (&nbsp;)
];

#[derive(Default)]
pub struct SnapshotOptions {
    pub selector: Option<String>,
    pub interactive: bool,
    pub compact: bool,
    pub depth: Option<usize>,
    pub urls: bool,
}

struct TreeNode {
    role: String,
    name: String,
    /// The raw accessible name from the AX tree, before any of the display-only
    /// fallback passes below rewrite `name` (cursor text, control labels,
    /// StaticText aggregation). Ref identity has to compare against what the
    /// live AX tree will report at interaction time, and it has to be the same
    /// value in `snapshot` and `snapshot -i` — otherwise a synthesized label
    /// churns the ref between the two modes or fails its own identity check.
    ax_name: String,
    level: Option<i64>,
    checked: Option<String>,
    expanded: Option<bool>,
    selected: Option<bool>,
    disabled: Option<bool>,
    required: Option<bool>,
    value_text: Option<String>,
    backend_node_id: Option<i64>,
    children: Vec<usize>,
    parent_idx: Option<usize>,
    has_ref: bool,
    ref_id: Option<String>,
    depth: usize,
    cursor_info: Option<CursorElementInfo>,
    url: Option<String>,
    // True when this node lives inside the topmost open modal/drawer (issue #90):
    // rendered as a `modal` attr so drawer controls are distinguishable from the
    // still-present background page controls that `snapshot -i` also lists.
    in_top_layer: bool,
}

impl TreeNode {
    // Create an empty node
    fn empty() -> Self {
        Self {
            role: String::new(),
            name: String::new(),
            ax_name: String::new(),
            level: None,
            checked: None,
            expanded: None,
            selected: None,
            disabled: None,
            required: None,
            value_text: None,
            backend_node_id: None,
            children: Vec::new(),
            parent_idx: None,
            has_ref: false,
            ref_id: None,
            depth: 0,
            cursor_info: None,
            url: None,
            in_top_layer: false,
        }
    }

    fn clear(&mut self) {
        self.role = String::new();
        self.name = String::new();
        self.ax_name = String::new();
        self.level = None;
        self.checked = None;
        self.expanded = None;
        self.selected = None;
        self.disabled = None;
        self.required = None;
        self.value_text = None;
        self.backend_node_id = None;
        self.children.clear();
        self.parent_idx = None;
        self.has_ref = false;
        self.url = None;
        self.ref_id = None;
        self.depth = 0;
        self.cursor_info = None;
    }
}

/// Build an AX fingerprint for a tree node, used by adaptive @ref relocation.
/// Pulls only data already in the AX tree (no extra CDP calls): role as `tag`,
/// accessible name as `text`, a few discriminating AX properties as `attrs`, and
/// the ancestor/parent/sibling structure from the tree links.
fn build_ax_fingerprint(tree_nodes: &[TreeNode], idx: usize) -> ElementFingerprint {
    let node = &tree_nodes[idx];

    let mut attrs = std::collections::BTreeMap::new();
    if let Some(v) = &node.value_text {
        if !v.is_empty() {
            attrs.insert("value".to_string(), v.clone());
        }
    }
    if let Some(u) = &node.url {
        if !u.is_empty() {
            attrs.insert("url".to_string(), u.clone());
        }
    }
    if let Some(l) = node.level {
        attrs.insert("level".to_string(), l.to_string());
    }
    if let Some(c) = &node.checked {
        attrs.insert("checked".to_string(), c.clone());
    }

    // Ancestor roles, nearest first, capped to keep the signature stable.
    let mut ancestors = Vec::new();
    let mut cur = node.parent_idx;
    while let Some(pidx) = cur {
        if ancestors.len() >= 6 {
            break;
        }
        let role = tree_nodes[pidx].role.clone();
        if !role.is_empty() {
            ancestors.push(role);
        }
        cur = tree_nodes[pidx].parent_idx;
    }

    let (parent_tag, parent_text) = node
        .parent_idx
        .map(|pidx| (tree_nodes[pidx].role.clone(), tree_nodes[pidx].name.clone()))
        .unwrap_or_default();

    // Position among same-role siblings under the same parent.
    let (sibling_index, sibling_count) = match node.parent_idx {
        Some(pidx) => {
            let mut count = 0u32;
            let mut index = 0u32;
            for &child in &tree_nodes[pidx].children {
                if tree_nodes[child].role == node.role {
                    if child == idx {
                        index = count;
                    }
                    count += 1;
                }
            }
            (index, count)
        }
        None => (0, 0),
    };

    ElementFingerprint {
        tag: node.role.clone(),
        text: node.name.clone(),
        attrs,
        ancestors,
        parent_tag,
        parent_text,
        sibling_index,
        sibling_count,
    }
}

/// Collect AX fingerprints for every node that has a backend node id, used as the
/// candidate set when relocating a stale @ref. Reuses the same extraction as the
/// baseline so the two are scored in the same space.
fn collect_fingerprints(tree_nodes: &[TreeNode]) -> Vec<(i64, ElementFingerprint)> {
    tree_nodes
        .iter()
        .enumerate()
        .filter_map(|(idx, n)| {
            n.backend_node_id
                .map(|bid| (bid, build_ax_fingerprint(tree_nodes, idx)))
        })
        .collect()
}

/// Fetch a fresh AX tree for the given frame and return `(backend_node_id,
/// fingerprint)` for every node — the candidate set for adaptive @ref
/// relocation. One `getFullAXTree` call, no per-element work.
pub(super) async fn collect_current_fingerprints(
    client: &CdpClient,
    session_id: &str,
    frame_id: Option<&str>,
    iframe_sessions: &HashMap<String, String>,
) -> Result<Vec<(i64, ElementFingerprint)>, String> {
    let (ax_params, effective_session_id) =
        resolve_ax_session(frame_id, session_id, iframe_sessions);
    let _ = client
        .send_command_no_params("DOM.enable", Some(effective_session_id))
        .await;
    let _ = client
        .send_command_no_params("Accessibility.enable", Some(effective_session_id))
        .await;
    let ax_tree: GetFullAXTreeResult = client
        .send_command_typed(
            "Accessibility.getFullAXTree",
            &ax_params,
            Some(effective_session_id),
        )
        .await?;
    let (tree_nodes, _roots) = build_tree(&ax_tree.nodes);
    Ok(collect_fingerprints(&tree_nodes))
}

/// The type of a hidden form input found inside a cursor-interactive element.
#[derive(Clone, Copy)]
enum HiddenInputKind {
    Radio,
    Checkbox,
}

impl HiddenInputKind {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "radio" => Some(Self::Radio),
            "checkbox" => Some(Self::Checkbox),
            _ => None,
        }
    }

    fn as_role(&self) -> &str {
        match self {
            Self::Radio => "radio",
            Self::Checkbox => "checkbox",
        }
    }
}

/// Information about a cursor-interactive element (elements with cursor:pointer, onclick, tabindex, etc.)
#[derive(Clone)]
struct CursorElementInfo {
    kind: String, // "clickable", "focusable", "editable"
    hints: Vec<String>,
    text: String, // textContent from the DOM element (fallback when ARIA name is empty)
    fallback_name: String,
    hidden_input_kind: Option<HiddenInputKind>,
    hidden_input_checked: Option<String>, // "true", "false", or "mixed" (tristate)
}

/// An inline validation/error message detected via DOM scan (issue #57).
/// Surfaced in `-i` mode where the non-interactive message node is otherwise
/// filtered out.
#[derive(Clone)]
struct ErrorElement {
    backend_node_id: Option<i64>,
    text: String,
}

/// Pick which error messages to surface, in order. Drops any whose element
/// already earned a ref elsewhere in the snapshot (matched by `backend_node_id`)
/// so the same message is never listed twice, plus any exact-duplicate text.
/// Pure so it can be unit-tested without a browser.
fn select_error_texts(
    error_elements: &[ErrorElement],
    already_reffed: &std::collections::HashSet<i64>,
) -> Vec<String> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut texts = Vec::new();
    for err in error_elements {
        if let Some(bid) = err.backend_node_id {
            if already_reffed.contains(&bid) {
                continue;
            }
        }
        if seen.insert(err.text.as_str()) {
            texts.push(err.text.clone());
        }
    }
    texts
}

/// Render a single error message as a top-level `alert` snapshot line.
///
/// Deliberately ref-less: the element's real ARIA role is usually `none`/generic
/// (a styled `<span>`), so attaching a `[ref=...]` whose stored role is `alert`
/// would make the ref-verifier reject any later use as a phantom DOM mutation.
/// The message is informational — the agent reads it, it doesn't click it.
fn render_error_line(text: &str) -> String {
    let display = serde_json::to_string(text).unwrap_or_else(|_| format!("\"{}\"", text));
    format!("- alert {}", display.replace(INVISIBLE_CHARS, ""))
}

struct RoleNameTracker {
    counts: HashMap<String, usize>,
    entries: Vec<(usize, String)>,
}

impl RoleNameTracker {
    fn new() -> Self {
        Self {
            counts: HashMap::new(),
            entries: Vec::new(),
        }
    }

    fn track(&mut self, role: &str, name: &str, node_idx: usize) -> usize {
        let key = format!("{}:{}", role, name);
        let count = self.counts.entry(key.clone()).or_insert(0);
        let nth = *count;
        *count += 1;
        self.entries.push((node_idx, key));
        nth
    }

    fn get_duplicates(&self) -> HashMap<String, usize> {
        self.counts
            .iter()
            .filter(|(_, &count)| count > 1)
            .map(|(key, &count)| (key.clone(), count))
            .collect()
    }
}

/// Max iframe nesting depth `take_snapshot` expands. Embedded payment/checkout
/// widgets nest a few frames deep (e.g. AdSense → payments.google.com → an inner
/// form frame); expanding past the first level is what gives those inner refs a
/// `frame_id` so clicks resolve into the right frame (issue #36). Capped to keep
/// a pathological frame tree from blowing up the snapshot.
const MAX_IFRAME_DEPTH: usize = 3;

/// DOM-walk fallback snapshot (issue #206). Reads the whole document with
/// `pierce: true` — open AND closed shadow roots, same-process child documents
/// — and lists the actionable elements with refs keyed by `backendNodeId`, so
/// `click @ref` / `type @ref` / `fill @ref` work as with AX-minted refs.
/// Returns the rendered listing and the number of elements found. Used when
/// the AX tree came back empty on a page that clearly has content, or on
/// demand via `snapshot --dom`.
pub async fn take_dom_snapshot(
    client: &CdpClient,
    session_id: &str,
    ref_map: &mut RefMap,
    frame_id: Option<&str>,
) -> Result<(String, usize), String> {
    client
        .send_command_no_params("DOM.enable", Some(session_id))
        .await?;
    let doc: Value = client
        .send_command(
            "DOM.getDocument",
            Some(serde_json::json!({ "depth": -1, "pierce": true })),
            Some(session_id),
        )
        .await?;
    let root = doc
        .get("root")
        .ok_or_else(|| "DOM.getDocument returned no root".to_string())?;
    let elems = super::dom_snapshot::collect_interactive(root);
    let mut tracker = RoleNameTracker::new();
    let mut refs: Vec<String> = Vec::with_capacity(elems.len());
    let mut nths: Vec<usize> = Vec::with_capacity(elems.len());
    for (idx, el) in elems.iter().enumerate() {
        nths.push(tracker.track(&el.role, &el.name, idx));
    }
    let duplicates = tracker.get_duplicates();
    for (el, nth) in elems.iter().zip(nths) {
        let key = format!("{}:{}", el.role, el.name);
        let actual_nth = duplicates.contains_key(&key).then_some(nth);
        let ref_id = ref_map.snapshot_ref(Some(el.backend_node_id), frame_id, &el.role, &el.name);
        ref_map.add_with_frame(
            ref_id.clone(),
            Some(el.backend_node_id),
            &el.role,
            &el.name,
            actual_nth,
            frame_id,
        );
        ref_map.mark_dom_sourced(&ref_id);
        refs.push(ref_id);
    }
    let count = elems.len();
    Ok((super::dom_snapshot::render(&elems, &refs), count))
}

pub async fn take_snapshot(
    client: &CdpClient,
    session_id: &str,
    options: &SnapshotOptions,
    ref_map: &mut RefMap,
    frame_id: Option<&str>,
    iframe_sessions: &HashMap<String, String>,
) -> Result<String, String> {
    take_snapshot_at_depth(
        client,
        session_id,
        options,
        ref_map,
        frame_id,
        iframe_sessions,
        0,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn take_snapshot_at_depth(
    client: &CdpClient,
    session_id: &str,
    options: &SnapshotOptions,
    ref_map: &mut RefMap,
    frame_id: Option<&str>,
    iframe_sessions: &HashMap<String, String>,
    depth: usize,
) -> Result<String, String> {
    client
        .send_command_no_params("DOM.enable", Some(session_id))
        .await?;
    client
        .send_command_no_params("Accessibility.enable", Some(session_id))
        .await?;

    // If a CSS selector is provided, resolve the set of backendNodeIds that
    // belong to the DOM subtree rooted at the matched element.  We use this
    // set to pick the right AX subtree root(s) later.
    let selector_backend_ids: Option<std::collections::HashSet<i64>> =
        if let Some(ref selector) = options.selector {
            let js = format!(
                "document.querySelector({})",
                serde_json::to_string(selector).unwrap_or_default()
            );
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

            let object_id = result
                .result
                .object_id
                .ok_or_else(|| format!("Selector '{}' did not match any element", selector))?;

            // Request the full DOM subtree (depth: -1) so we can collect all
            // backendNodeIds that live under the matched element.
            let describe: Value = client
                .send_command(
                    "DOM.describeNode",
                    Some(serde_json::json!({ "objectId": object_id, "depth": -1 })),
                    Some(session_id),
                )
                .await?;

            let root_node = describe
                .get("node")
                .ok_or_else(|| format!("Could not resolve DOM node for selector '{}'", selector))?;

            let mut ids = std::collections::HashSet::new();
            collect_backend_node_ids(root_node, &mut ids);

            if ids.is_empty() {
                return Err(format!(
                    "Could not resolve backendNodeId for selector '{}'",
                    selector
                ));
            }

            Some(ids)
        } else {
            None
        };

    let (ax_params, effective_session_id) =
        resolve_ax_session(frame_id, session_id, iframe_sessions);
    // Ensure domains are enabled on the iframe session (defensive fallback
    // in case the attach-time enable in execute_command was missed).
    if effective_session_id != session_id {
        let _ = client
            .send_command_no_params("DOM.enable", Some(effective_session_id))
            .await;
        let _ = client
            .send_command_no_params("Accessibility.enable", Some(effective_session_id))
            .await;
    }
    let ax_tree: GetFullAXTreeResult = client
        .send_command_typed(
            "Accessibility.getFullAXTree",
            &ax_params,
            Some(effective_session_id),
        )
        .await?;

    let (mut tree_nodes, root_indices) = build_tree(&ax_tree.nodes);

    // When a selector is given, find AX nodes whose backendDOMNodeId falls
    // within the target DOM subtree and pick the top-level ones as roots.
    let effective_roots = if let Some(ref id_set) = selector_backend_ids {
        // Mark which tree_nodes belong to the target DOM subtree.
        let in_subtree: Vec<bool> = tree_nodes
            .iter()
            .map(|n| n.backend_node_id.is_some_and(|bid| id_set.contains(&bid)))
            .collect();

        // An AX node is a "top-level" match if it is in the subtree but its
        // parent (in the AX tree) is not.
        let mut roots = Vec::new();
        for (idx, node) in tree_nodes.iter().enumerate() {
            if !in_subtree[idx] {
                continue;
            }
            let parent_in_subtree = node.parent_idx.is_some_and(|pidx| in_subtree[pidx]);
            if !parent_in_subtree {
                roots.push(idx);
            }
        }

        if roots.is_empty() {
            return Err(format!(
                "No accessibility node found for selector '{}'",
                options.selector.as_deref().unwrap_or("")
            ));
        }
        roots
    } else {
        root_indices
    };

    let mut tracker = RoleNameTracker::new();

    let mut nodes_with_refs: Vec<(usize, usize)> = Vec::new();

    // Pre-collect cursor-interactive elements so we can mark them with refs during tree building
    let cursor_elements: HashMap<i64, CursorElementInfo> =
        find_cursor_interactive_elements(client, session_id)
            .await
            .unwrap_or_default();

    // Collect inline validation/error messages so `-i` (interactive-only) mode
    // still surfaces *why* a submit was rejected (issue #57). These are usually
    // non-interactive styled spans (`.is-error`, `[role=alert]`, `aria-live`
    // regions) that `-i` otherwise filters out, leaving the agent blind exactly
    // when a form fails. Only at depth 0 (main frame): the scan runs on the main
    // session, so running it during iframe recursion would re-surface the same
    // main-frame errors at every nested level.
    let error_elements: Vec<ErrorElement> = if options.interactive && depth == 0 {
        find_error_elements(client, session_id)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    promote_hidden_inputs(&mut tree_nodes, &cursor_elements);
    for node in tree_nodes.iter_mut() {
        if node.name.is_empty() {
            if let Some(fallback_name) = node
                .backend_node_id
                .and_then(|bid| cursor_elements.get(&bid))
                .map(|info| info.fallback_name.as_str())
                .filter(|name| !name.is_empty())
            {
                node.name = fallback_name.to_string();
            }
        }
    }

    // Name unlabeled interactive controls from their placeholder / nearest label
    // (issue #90): el-select and custom comboboxes leave the AX name empty, so
    // `snapshot -i` otherwise shows bare, indistinguishable `combobox [ref=eN]`.
    // One batched scan, main frame only; applied ONLY where the AX name is empty,
    // so named nodes are untouched (zero regression).
    if options.interactive && depth == 0 {
        let label_fallbacks = find_control_label_fallbacks(client, session_id)
            .await
            .unwrap_or_default();
        if !label_fallbacks.is_empty() {
            for node in tree_nodes.iter_mut() {
                if node.name.is_empty() && INTERACTIVE_ROLES.contains(&node.role.as_str()) {
                    if let Some(label) = node
                        .backend_node_id
                        .and_then(|bid| label_fallbacks.get(&bid))
                    {
                        node.name = label.clone();
                    }
                }
            }
        }

        // Tag nodes inside the topmost open modal/drawer (issue #90). When a drawer
        // or dialog is open, `snapshot -i` otherwise lists its controls flat next to
        // the still-present background page controls — identical `link "编辑"` lines
        // from both, with no way to tell which is the drawer's. Marking the top-layer
        // subtree with a `modal` attr makes the boundary greppable.
        let top_layer = find_top_layer_backend_ids(client, session_id)
            .await
            .unwrap_or_default();
        if !top_layer.is_empty() {
            for node in tree_nodes.iter_mut() {
                if node
                    .backend_node_id
                    .is_some_and(|bid| top_layer.contains(&bid))
                {
                    node.in_top_layer = true;
                }
            }
        }
    }

    for (idx, node) in tree_nodes.iter().enumerate() {
        let role = node.role.as_str();
        let mut should_ref = if INTERACTIVE_ROLES.contains(&role) {
            true
        } else if CONTENT_ROLES.contains(&role) {
            !node.name.is_empty()
        } else {
            false
        };

        if node
            .backend_node_id
            .is_some_and(|bid| cursor_elements.contains_key(&bid))
        {
            // ref elements that are cursor-interactive
            should_ref = true;
        }

        if should_ref {
            let nth = tracker.track(role, &node.name, idx);
            nodes_with_refs.push((idx, nth));
        }
    }

    let duplicates = tracker.get_duplicates();

    for (idx, nth) in &nodes_with_refs {
        let node = &tree_nodes[*idx];
        let key = format!("{}:{}", node.role, node.name);
        let actual_nth = if duplicates.contains_key(&key) {
            Some(*nth)
        } else {
            None
        };

        // Identity for ref reuse is the RAW AX name, not the rendered one: the
        // fallback label passes run differently for `snapshot` and
        // `snapshot -i`, so keying on the display name would hand the same DOM
        // node a different ref depending on which command asked (issue #155).
        let ref_id = ref_map.snapshot_ref(
            tree_nodes[*idx].backend_node_id,
            frame_id,
            &tree_nodes[*idx].role,
            &tree_nodes[*idx].ax_name,
        );

        ref_map.add_with_frame(
            ref_id.clone(),
            tree_nodes[*idx].backend_node_id,
            &tree_nodes[*idx].role,
            &tree_nodes[*idx].name,
            actual_nth,
            frame_id,
        );
        ref_map.set_fingerprint(&ref_id, build_ax_fingerprint(&tree_nodes, *idx));

        tree_nodes[*idx].has_ref = true;
        tree_nodes[*idx].ref_id = Some(ref_id);
    }

    // Populate cursor_info for ref-bearing nodes
    for (idx, _) in &nodes_with_refs {
        if let Some(bid) = tree_nodes[*idx].backend_node_id {
            if let Some(cursor_info) = cursor_elements.get(&bid) {
                tree_nodes[*idx].cursor_info = Some((*cursor_info).clone());
            }
        }
    }

    // Render inline validation/error messages as top-level `alert` lines (issue
    // #57), appended after the tree so the agent always sees *why* a submit was
    // rejected even in `-i` mode. Deduped against elements that already earned a
    // ref (e.g. an alert Chrome surfaced as an AX node) so nothing lists twice.
    let already_reffed: std::collections::HashSet<i64> = tree_nodes
        .iter()
        .filter(|n| n.has_ref)
        .filter_map(|n| n.backend_node_id)
        .collect();
    let error_lines: Vec<String> = select_error_texts(&error_elements, &already_reffed)
        .iter()
        .map(|text| render_error_line(text))
        .collect();

    if options.urls {
        let link_nodes: Vec<(usize, i64)> = tree_nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.role == "link" && n.has_ref && n.backend_node_id.is_some())
            .filter_map(|(i, n)| n.backend_node_id.map(|bid| (i, bid)))
            .collect();

        if !link_nodes.is_empty() {
            // CDP has no batch resolve API, so we parallelize individual calls.
            // Phase 1: resolve all backend node IDs to JS object IDs in parallel.
            let resolve_futs = link_nodes.iter().map(|&(idx, bid)| async move {
                let resolved = client
                    .send_command(
                        "DOM.resolveNode",
                        Some(serde_json::json!({ "backendNodeId": bid })),
                        Some(session_id),
                    )
                    .await;
                let obj_id = resolved.ok().and_then(|r| {
                    r.get("object")
                        .and_then(|o| o.get("objectId"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                });
                (idx, obj_id)
            });
            let resolved: Vec<(usize, Option<String>)> =
                futures_util::future::join_all(resolve_futs).await;

            // Phase 2: fetch hrefs for all resolved objects in parallel.
            let href_futs: Vec<_> = resolved
                .iter()
                .filter_map(|(idx, obj_id)| {
                    let oid = obj_id.as_ref()?;
                    Some(async move {
                        let result = client
                            .send_command(
                                "Runtime.callFunctionOn",
                                Some(serde_json::json!({
                                    "objectId": oid,
                                    "functionDeclaration": "function() { return this.href || ''; }",
                                    "returnByValue": true,
                                })),
                                Some(session_id),
                            )
                            .await;
                        let href = result.ok().and_then(|r| {
                            r.get("result")
                                .and_then(|r| r.get("value"))
                                .and_then(|v| v.as_str())
                                .filter(|s| !s.is_empty())
                                .map(|s| s.to_string())
                        });
                        (*idx, href)
                    })
                })
                .collect();
            let hrefs: Vec<(usize, Option<String>)> =
                futures_util::future::join_all(href_futs).await;

            for (idx, href) in hrefs {
                if let Some(url) = href {
                    tree_nodes[idx].url = Some(url);
                }
            }
        }
    }

    let mut output = String::new();
    for &root_idx in &effective_roots {
        render_tree(&tree_nodes, root_idx, 0, &mut output, options);
    }

    // Recurse into child iframes: for each Iframe node with a backend_node_id,
    // resolve the child frame ID and snapshot its content. Recurse to
    // MAX_IFRAME_DEPTH (not just the main frame) so refs inside nested
    // payment/checkout widgets get a `frame_id` and clicks resolve into the right
    // frame (issue #36); the cap bounds a pathological frame tree.
    if depth < MAX_IFRAME_DEPTH {
        let mut iframe_snapshots: Vec<(String, String)> = Vec::new(); // (ref_id, child_snapshot)
        for node in tree_nodes.iter() {
            if node.role != "Iframe" || !node.has_ref {
                continue;
            }
            let Some(bid) = node.backend_node_id else {
                continue;
            };
            let ref_id = node.ref_id.as_deref().unwrap_or("");
            // The Iframe node came out of THIS level's AX tree, which for a
            // cross-origin frame lives on `effective_session_id`, not the page
            // session. Backend node ids are per-target, so describing it on the
            // page session fails — silently — and nested cross-origin frames
            // (a bank picker popup inside Stripe Connect's onboarding frame,
            // #214) were never descended into even once their session was
            // attached. Resolve the child's frame id where the node exists.
            if let Ok(child_fid) = resolve_iframe_frame_id(client, effective_session_id, bid).await
            {
                // Snapshot the child frame; errors are silently ignored
                // (e.g. cross-origin iframes)
                if let Ok(child_text) = Box::pin(take_snapshot_at_depth(
                    client,
                    session_id,
                    options,
                    ref_map,
                    Some(&child_fid),
                    iframe_sessions,
                    depth + 1,
                ))
                .await
                {
                    if !child_text.is_empty()
                        && child_text != "(empty page)"
                        && child_text != "(no interactive elements)"
                    {
                        iframe_snapshots.push((ref_id.to_string(), child_text));
                    }
                }
            }
        }

        // Insert each child snapshot after its Iframe line in the output
        for (ref_id, child_text) in iframe_snapshots {
            if let Some(pos) = find_snapshot_ref(&output, &ref_id) {
                // Find the end of the Iframe line
                let line_end = output[pos..]
                    .find('\n')
                    .map(|i| pos + i)
                    .unwrap_or(output.len());
                // Determine the indent of the Iframe line
                let line_start = output[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
                let iframe_line = &output[line_start..line_end];
                let iframe_indent = iframe_line.len() - iframe_line.trim_start().len();
                let child_indent = iframe_indent + 2; // one level deeper
                let prefix = " ".repeat(child_indent);

                let indented_child: String = child_text
                    .lines()
                    .map(|line| format!("{}{}\n", prefix, line))
                    .collect();

                // Ensure there's a newline to insert after
                if line_end == output.len() {
                    output.push('\n');
                    output.push_str(&indented_child);
                } else {
                    output.insert_str(line_end + 1, &indented_child);
                }
            }
        }
    }

    // Additive pass (issue #92): iframes with `role="presentation"`/`none` are
    // stripped from the AX tree, so the loop above (keyed on AX `Iframe` nodes)
    // misses them — reCAPTCHA / hCaptcha anchor frames, some SDK widgets. Find
    // `<iframe>`s via the DOM, recurse into any the AX pass didn't already
    // handle, and append their content (ref-less header line; the inner controls
    // still earn real refs from the child frame's AX tree).
    //
    // Main frame only: `find_dom_iframes` runs `DOM.getDocument` on the session,
    // which always returns the top document — it is not frame-scoped, so running
    // it at depth > 0 would re-scan the main frame's iframes and recurse
    // redundantly. Nested presentation iframes are rare; the AX-`Iframe` loop
    // above still handles nested a11y-visible iframes at every depth.
    if depth == 0 {
        let handled: std::collections::HashSet<i64> = tree_nodes
            .iter()
            .filter(|n| n.role == "Iframe")
            .filter_map(|n| n.backend_node_id)
            .collect();
        for (bid, title) in find_dom_iframes(client, session_id).await {
            if handled.contains(&bid) {
                continue;
            }
            let Ok(child_fid) = resolve_iframe_frame_id(client, session_id, bid).await else {
                continue;
            };
            let Ok(child_text) = Box::pin(take_snapshot_at_depth(
                client,
                session_id,
                options,
                ref_map,
                Some(&child_fid),
                iframe_sessions,
                depth + 1,
            ))
            .await
            else {
                continue;
            };
            if child_text.is_empty()
                || child_text == "(empty page)"
                || child_text == "(no interactive elements)"
            {
                continue;
            }
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(&if title.is_empty() {
                "- iframe".to_string()
            } else {
                format!("- iframe \"{title}\"")
            });
            output.push('\n');
            for line in child_text.lines() {
                output.push_str("  ");
                output.push_str(line);
                output.push('\n');
            }
        }
    }

    // Append inline validation/error messages as top-level `alert` lines (issue
    // #57). Done after iframe recursion so the byte-offset insertion above isn't
    // disturbed, and before compaction so the `[ref=...]` markers survive `-c`.
    if !error_lines.is_empty() {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        for line in &error_lines {
            output.push_str(line);
            output.push('\n');
        }
    }

    if options.compact {
        output = compact_tree(&output, options.interactive);
    }

    let trimmed = output.trim().to_string();

    if trimmed.is_empty() {
        if options.interactive {
            return Ok("(no interactive elements)".to_string());
        }
        return Ok("(empty page)".to_string());
    }

    Ok(trimmed)
}

/// Resolve the child frame ID for an iframe element given its backendNodeId.
async fn resolve_iframe_frame_id(
    client: &CdpClient,
    session_id: &str,
    backend_node_id: i64,
) -> Result<String, String> {
    // depth: 1 ensures contentDocument is included in the response
    let describe: Value = client
        .send_command(
            "DOM.describeNode",
            Some(serde_json::json!({ "backendNodeId": backend_node_id, "depth": 1 })),
            Some(session_id),
        )
        .await?;

    // Try contentDocument.frameId first (standard for iframes)
    if let Some(frame_id) = describe
        .get("node")
        .and_then(|n| n.get("contentDocument"))
        .and_then(|cd| cd.get("frameId"))
        .and_then(|v| v.as_str())
    {
        return Ok(frame_id.to_string());
    }

    // Fallback: the node itself may have a frameId
    describe
        .get("node")
        .and_then(|n| n.get("frameId"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "Could not resolve iframe frame ID".to_string())
}

/// Find every `<iframe>` element via the DOM (not the AX tree), returning
/// `(backendNodeId, title)`. Unlike the AX `Iframe` role nodes the main
/// recursion keys off, this also catches iframes stripped from the
/// accessibility tree by `role="presentation"` — e.g. reCAPTCHA / hCaptcha
/// anchor frames and some third-party SDK widgets (issue #92) — so their
/// content can still be merged into `snapshot -i`.
async fn find_dom_iframes(client: &CdpClient, session_id: &str) -> Vec<(i64, String)> {
    let mut out = Vec::new();
    let Ok(doc) = client
        .send_command(
            "DOM.getDocument",
            Some(serde_json::json!({ "depth": 0 })),
            Some(session_id),
        )
        .await
    else {
        return out;
    };
    let Some(root) = doc
        .get("root")
        .and_then(|r| r.get("nodeId"))
        .and_then(|v| v.as_i64())
    else {
        return out;
    };
    let Ok(q) = client
        .send_command(
            "DOM.querySelectorAll",
            Some(serde_json::json!({ "nodeId": root, "selector": "iframe" })),
            Some(session_id),
        )
        .await
    else {
        return out;
    };
    let node_ids: Vec<i64> = q
        .get("nodeIds")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_i64()).collect())
        .unwrap_or_default();
    if node_ids.is_empty() {
        return out;
    }
    let futs: Vec<_> = node_ids
        .iter()
        .map(|&nid| {
            client.send_command(
                "DOM.describeNode",
                Some(serde_json::json!({ "nodeId": nid })),
                Some(session_id),
            )
        })
        .collect();
    for desc in futures_util::future::join_all(futs)
        .await
        .into_iter()
        .flatten()
    {
        let Some(node) = desc.get("node") else {
            continue;
        };
        let Some(bid) = node.get("backendNodeId").and_then(|v| v.as_i64()) else {
            continue;
        };
        // attributes is a flat [name, value, name, value, ...] array.
        let mut title = String::new();
        if let Some(attrs) = node.get("attributes").and_then(|a| a.as_array()) {
            let mut it = attrs.iter();
            while let (Some(n), Some(v)) = (it.next(), it.next()) {
                if n.as_str() == Some("title") {
                    title = v.as_str().unwrap_or("").to_string();
                    break;
                }
            }
        }
        out.push((bid, title));
    }
    out
}

async fn find_cursor_interactive_elements(
    client: &CdpClient,
    session_id: &str,
) -> Result<HashMap<i64, CursorElementInfo>, String> {
    // Single JS evaluation that matches the v0.19.0 Node.js findCursorInteractiveElements():
    // - Uses querySelectorAll('*') to walk all elements
    // - Surfaces deliberate non-default cursor values (pointer/grab/resize/text/etc.)
    // - Checks onclick attribute/handler and tabindex
    // - Skips interactiveTags (a, button, input, select, textarea, details, summary)
    // - Skips elements with interactive ARIA roles
    // - Deduplicates cursor styles inherited unchanged from a parent
    // - Skips empty text and zero-size elements
    // - Tags each matched element with data-__ab-ci for batch backendNodeId resolution
    let js = r#"
(function() {
    var results = [];
    if (!document.body) return results;

    var interactiveRoles = {
        'button':1, 'link':1, 'textbox':1, 'checkbox':1, 'radio':1, 'combobox':1, 'listbox':1,
        'menuitem':1, 'menuitemcheckbox':1, 'menuitemradio':1, 'option':1, 'searchbox':1,
        'slider':1, 'spinbutton':1, 'switch':1, 'tab':1, 'treeitem':1
    };
    var interactiveTags = {
        'a':1, 'button':1, 'input':1, 'select':1, 'textarea':1, 'details':1, 'summary':1
    };

    var allElements = document.body.querySelectorAll('*');
    for (var i = 0; i < allElements.length; i++) {
        var el = allElements[i];

        if (el.closest && el.closest('[hidden], [aria-hidden="true"]')) continue;

        var tagName = el.tagName.toLowerCase();
        if (interactiveTags[tagName]) continue;

        var role = el.getAttribute('role');
        if (role && interactiveRoles[role.toLowerCase()]) continue;

        var computedStyle = getComputedStyle(el);
        var cursor = computedStyle.cursor || 'auto';
        var hasCursorPointer = cursor === 'pointer';
        var hasMeaningfulCursor = cursor !== 'auto' && cursor !== 'default'
            && cursor !== 'none' && cursor !== 'not-allowed';
        var hasOnClick = el.hasAttribute('onclick') || el.onclick !== null;
        var tabIndex = el.getAttribute('tabindex');
        var hasTabIndex = tabIndex !== null && tabIndex !== '-1';
        var ce = el.getAttribute('contenteditable');
        var isEditable = ce === '' || ce === 'true';

        if (!hasMeaningfulCursor && !hasOnClick && !hasTabIndex && !isEditable) continue;

        // Skip elements that only inherit the same cursor from an ancestor.
        if (hasMeaningfulCursor && !hasOnClick && !hasTabIndex && !isEditable) {
            var parent = el.parentElement;
            if (parent && getComputedStyle(parent).cursor === cursor) continue;
        }

        var text = (el.textContent || '').trim().slice(0, 100);
        var directText = Array.from(el.childNodes || [])
            .filter(function(n) { return n.nodeType === Node.TEXT_NODE; })
            .map(function(n) { return n.textContent || ''; })
            .join(' ')
            .replace(/\s+/g, ' ')
            .trim()
            .slice(0, 100);
        var fallbackName = (
            el.getAttribute('aria-label')
            || el.getAttribute('title')
            || directText
            || text
            || ''
        ).trim().slice(0, 100);
        var classNames = Array.from(el.classList || []).slice(0, 2);

        var rect = el.getBoundingClientRect();
        if (rect.width === 0 || rect.height === 0) continue;

        // Detect hidden radio/checkbox inputs inside this element (common pattern:
        // <label> wrapping a display:none <input type="radio"> styled as a card).
        // Note: we only check display/visibility/hidden, NOT opacity:0 or sr-only,
        // because those inputs remain in Chrome's AX tree and already appear as
        // role="radio" without promotion.
        var hiddenInputType = null;
        var hiddenInputChecked = null;
        var hiddenInput = el.querySelector('input[type="radio"], input[type="checkbox"]');
        if (hiddenInput) {
            var hiddenInputStyle = getComputedStyle(hiddenInput);
            var isInputHidden = hiddenInputStyle.display === 'none' || hiddenInputStyle.visibility === 'hidden' || hiddenInput.hidden;
            if (isInputHidden) {
                hiddenInputType = hiddenInput.type;
                hiddenInputChecked = hiddenInput.indeterminate ? 'mixed' : String(hiddenInput.checked);
            }
        }

        el.setAttribute('data-__ab-ci', String(results.length));
        results.push({
            text: text,
            tagName: tagName,
            cursor: cursor,
            hasOnClick: hasOnClick,
            hasCursorPointer: hasCursorPointer,
            hasMeaningfulCursor: hasMeaningfulCursor,
            hasTabIndex: hasTabIndex,
            isEditable: isEditable,
            fallbackName: fallbackName,
            id: (el.id || '').slice(0, 80),
            testId: (el.getAttribute('data-testid') || '').slice(0, 80),
            classNames: classNames,
            hiddenInputType: hiddenInputType,
            hiddenInputChecked: hiddenInputChecked
        });
    }
    return results;
})()
"#;

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
        .await?;

    let elements: Vec<Value> = result
        .result
        .value
        .and_then(|v| serde_json::from_value::<Vec<Value>>(v).ok())
        .unwrap_or_default();

    if elements.is_empty() {
        return Ok(HashMap::new());
    }

    // Batch-resolve backendNodeIds: use DOM.getDocument to get the root nodeId,
    // then DOM.querySelectorAll to get all tagged elements in a single call.
    let doc: Value = client
        .send_command(
            "DOM.getDocument",
            Some(serde_json::json!({ "depth": 0 })),
            Some(session_id),
        )
        .await?;

    let root_node_id = doc
        .get("root")
        .and_then(|r| r.get("nodeId"))
        .and_then(|v| v.as_i64())
        .ok_or("DOM.getDocument did not return root nodeId")?;

    let query_result: Value = client
        .send_command(
            "DOM.querySelectorAll",
            Some(serde_json::json!({
                "nodeId": root_node_id,
                "selector": "[data-__ab-ci]"
            })),
            Some(session_id),
        )
        .await?;

    let node_ids: Vec<i64> = query_result
        .get("nodeIds")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect())
        .unwrap_or_default();

    // Resolve backendNodeIds for each DOM node using concurrent CDP calls.
    let describe_futures: Vec<_> = node_ids
        .iter()
        .map(|&node_id| {
            client.send_command(
                "DOM.describeNode",
                Some(serde_json::json!({ "nodeId": node_id })),
                Some(session_id),
            )
        })
        .collect();

    let describe_results = futures_util::future::join_all(describe_futures).await;

    // Build a map from data-__ab-ci index to backendNodeId.
    let mut idx_to_backend: HashMap<usize, i64> = HashMap::new();
    for desc in describe_results.into_iter().flatten() {
        let backend_id = desc
            .get("node")
            .and_then(|n| n.get("backendNodeId"))
            .and_then(|v| v.as_i64());
        let ci_attr = desc
            .get("node")
            .and_then(|n| n.get("attributes"))
            .and_then(|a| a.as_array())
            .and_then(|attrs| {
                // attributes is a flat array: [name, value, name, value, ...]
                attrs
                    .iter()
                    .enumerate()
                    .find(|(_, v)| v.as_str() == Some("data-__ab-ci"))
                    .and_then(|(i, _)| attrs.get(i + 1))
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<usize>().ok())
            });
        if let (Some(bid), Some(idx)) = (backend_id, ci_attr) {
            idx_to_backend.insert(idx, bid);
        }
    }

    // Clean up the data attributes we injected for backendNodeId resolution.
    let cleanup_js =
        r#"(function(){ var els = document.querySelectorAll('[data-__ab-ci]'); for (var i = 0; i < els.length; i++) els[i].removeAttribute('data-__ab-ci'); return els.length; })()"#.to_string();
    if let Err(e) = client
        .send_command_typed::<EvaluateParams, EvaluateResult>(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: cleanup_js,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await
    {
        eprintln!("[chrome-use] Warning: failed to clean up data-__ab-ci attributes: {e}");
    }

    // Build the map
    let mut map: HashMap<i64, CursorElementInfo> = HashMap::new();
    for (i, elem) in elements.iter().enumerate() {
        let backend_node_id = idx_to_backend.get(&i).copied();

        // Role differentiation: v0.19.0 uses 'clickable' for cursor:pointer or onclick,
        // 'focusable' for tabindex-only elements.
        let has_cursor_pointer = elem
            .get("hasCursorPointer")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let has_meaningful_cursor = elem
            .get("hasMeaningfulCursor")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let cursor = elem
            .get("cursor")
            .and_then(|v| v.as_str())
            .unwrap_or("auto");
        let has_on_click = elem
            .get("hasOnClick")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let has_tab_index = elem
            .get("hasTabIndex")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let is_editable = elem
            .get("isEditable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let kind = if matches!(cursor, "grab" | "grabbing") {
            "draggable"
        } else if has_cursor_pointer || has_on_click {
            "clickable"
        } else if is_editable {
            "editable"
        } else if has_tab_index {
            "focusable"
        } else {
            "interactive"
        };

        let mut hints: Vec<String> = Vec::new();
        if has_meaningful_cursor {
            hints.push(format!("cursor:{cursor}"));
        }
        if has_on_click {
            hints.push("onclick".to_string());
        }
        if has_tab_index {
            hints.push("tabindex".to_string());
        }
        if is_editable {
            hints.push("contenteditable".to_string());
        }
        if let Some(id) = elem
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            hints.push(format!("id={id}"));
        }
        if let Some(test_id) = elem
            .get("testId")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            hints.push(format!("testid={test_id}"));
        }
        if let Some(classes) = elem.get("classNames").and_then(|v| v.as_array()) {
            let classes = classes
                .iter()
                .filter_map(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>();
            if !classes.is_empty() {
                hints.push(format!("class={}", classes.join(".")));
            }
        }

        let text = elem
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let fallback_name = elem
            .get("fallbackName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        let hidden_input_kind = elem
            .get("hiddenInputType")
            .and_then(|v| v.as_str())
            .and_then(HiddenInputKind::parse);
        let hidden_input_checked = elem
            .get("hiddenInputChecked")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        if let Some(bid) = backend_node_id {
            map.insert(
                bid,
                CursorElementInfo {
                    kind: kind.to_string(),
                    hints,
                    text,
                    fallback_name,
                    hidden_input_kind,
                    hidden_input_checked,
                },
            );
        }
    }

    Ok(map)
}

/// CSS selector union for inline validation/error message containers. Combines
/// ARIA semantics (`role=alert`, live regions, `aria-invalid`) with the common
/// class-name conventions frameworks use for field errors. Kept in one place so
/// the JS scan and the leaf-match test below agree.
const ERROR_SELECTOR: &str = "[role=\"alert\"],[aria-live=\"assertive\"],[aria-live=\"polite\"],[aria-invalid=\"true\"],.is-error,.is-invalid,.invalid-feedback,.field-error,.form-error,.error-message,.errorMessage,.help-block,[class*=\"error\"],[class*=\"invalid\"]";

/// Scan the page for inline validation/error messages (issue #57).
///
/// `snapshot -i` keeps only interactive nodes, but a rejected submit usually
/// reports its reason through a non-interactive styled span (`.is-error`, a
/// `[role=alert]`, an `aria-live` region) that `-i` filters out — the agent then
/// sees the field and a disabled submit but no clue why. This finds those
/// message elements directly in the DOM (independent of how the AX tree happens
/// to represent them) so the snapshot can surface them.
///
/// To stay precise it keeps only *leaf* matches (no matching descendant), skips
/// containers that wrap form controls (page-level error regions, not a field's
/// message), skips hidden/zero-size nodes and empty/over-long text, and dedupes
/// identical messages.
async fn find_error_elements(
    client: &CdpClient,
    session_id: &str,
) -> Result<Vec<ErrorElement>, String> {
    let js = format!(
        r#"
(function() {{
    var results = [];
    if (!document.body) return results;
    var SEL = {sel};
    var seen = {{}};
    var matched = document.body.querySelectorAll(SEL);
    for (var i = 0; i < matched.length; i++) {{
        var el = matched[i];
        if (el.closest && el.closest('[hidden], [aria-hidden="true"]')) continue;
        // Keep the most specific match: skip if a descendant also matches.
        if (el.querySelector(SEL)) continue;
        // A field's error message doesn't wrap controls; skip big regions that do.
        if (el.querySelector('input, button, select, textarea, a')) continue;
        var st = getComputedStyle(el);
        if (st.display === 'none' || st.visibility === 'hidden') continue;
        var rect = el.getBoundingClientRect();
        if (rect.width === 0 || rect.height === 0) continue;
        var text = (el.textContent || '').replace(/\s+/g, ' ').trim();
        if (!text) continue;
        if (text.length > 200) text = text.slice(0, 200) + '…';
        if (seen[text]) continue;
        seen[text] = 1;
        el.setAttribute('data-__ab-err', String(results.length));
        results.push({{ text: text }});
    }}
    return results;
}})()
"#,
        sel = serde_json::to_string(ERROR_SELECTOR).unwrap_or_default()
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
        .await?;

    let elements: Vec<Value> = result
        .result
        .value
        .and_then(|v| serde_json::from_value::<Vec<Value>>(v).ok())
        .unwrap_or_default();

    if elements.is_empty() {
        return Ok(Vec::new());
    }

    let idx_to_backend = resolve_tagged_backend_ids(client, session_id, "data-__ab-err").await;

    Ok(elements
        .iter()
        .enumerate()
        .filter_map(|(i, elem)| {
            let text = elem
                .get("text")
                .and_then(|v| v.as_str())?
                .trim()
                .to_string();
            if text.is_empty() {
                return None;
            }
            Some(ErrorElement {
                backend_node_id: idx_to_backend.get(&i).copied(),
                text,
            })
        })
        .collect())
}

/// Derive a fallback accessible name for interactive controls that have none in
/// the AX tree (issue #90): Element-Plus `el-select` / custom comboboxes render
/// their label as a placeholder span or a sibling form-item label, so the AX
/// name is empty and `snapshot -i` shows a bare, indistinguishable
/// `combobox [ref=eN]`. One batched scan tags each such control and computes a
/// label from (in order) its placeholder, an inner input's placeholder, an
/// associated `<label>`, the nearest `.el-form-item__label` / `.ant-form-item`
/// label, or leading sibling text. Returns backendNodeId → label; the caller
/// applies it only where the AX name is actually empty.
async fn find_control_label_fallbacks(
    client: &CdpClient,
    session_id: &str,
) -> Result<HashMap<i64, String>, String> {
    let js = r#"
(function() {
    var results = [];
    if (!document.body) return results;
    var SEL = 'input:not([type=hidden]):not([type=submit]):not([type=button]),' +
              'select,textarea,[contenteditable="true"],' +
              '[role=combobox],[role=textbox],[role=searchbox],[role=listbox],[role=spinbutton]';
    function clean(s){ return (s||'').replace(/\s+/g,' ').trim(); }
    function labelFor(el){
        // 1. own / inner placeholder
        var ph = el.getAttribute && el.getAttribute('placeholder');
        if (ph) return ph;
        var inner = el.querySelector && el.querySelector('input[placeholder],textarea[placeholder],[placeholder]');
        if (inner) { var ip = inner.getAttribute('placeholder'); if (ip) return ip; }
        var phSpan = el.querySelector && el.querySelector('.el-select__placeholder,.el-input__inner');
        if (phSpan) { var t = clean(phSpan.getAttribute && phSpan.getAttribute('placeholder')) || clean(phSpan.textContent); if (t) return t; }
        // 2. associated <label>
        try { if (el.labels && el.labels[0]) { var lt = clean(el.labels[0].textContent); if (lt) return lt; } } catch(e){}
        if (el.id) { var lf = document.querySelector('label[for="'+CSS.escape(el.id)+'"]'); if (lf) { var lft = clean(lf.textContent); if (lft) return lft; } }
        var wrapLabel = el.closest && el.closest('label');
        if (wrapLabel) { var wt = clean(wrapLabel.textContent); if (wt) return wt; }
        // 3. framework form-item label (Element Plus / Ant Design / generic)
        var fi = el.closest && el.closest('.el-form-item,.ant-form-item,.el-form-item__content,.form-item');
        if (fi) { var lbl = fi.querySelector('.el-form-item__label,.ant-form-item-label,label,.form-item__label'); if (lbl) { var flt = clean(lbl.textContent); if (flt) return flt; } }
        // 4. aria-label on the element itself (AX sometimes still misses it)
        var al = el.getAttribute && el.getAttribute('aria-label'); if (al) return al;
        return '';
    }
    var matched = document.body.querySelectorAll(SEL);
    for (var i = 0; i < matched.length; i++) {
        var el = matched[i];
        if (el.closest && el.closest('[hidden],[aria-hidden="true"]')) continue;
        var rect = el.getBoundingClientRect();
        if (rect.width === 0 && rect.height === 0) continue;
        var label = clean(labelFor(el));
        if (!label) continue;
        if (label.length > 40) label = label.slice(0, 40) + '…';
        el.setAttribute('data-__ab-lbl', String(results.length));
        results.push({ label: label });
    }
    return results;
})()
"#
    .to_string();

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

    let elements: Vec<Value> = result
        .result
        .value
        .and_then(|v| serde_json::from_value::<Vec<Value>>(v).ok())
        .unwrap_or_default();

    if elements.is_empty() {
        return Ok(HashMap::new());
    }

    let idx_to_backend = resolve_tagged_backend_ids(client, session_id, "data-__ab-lbl").await;

    let mut out: HashMap<i64, String> = HashMap::new();
    for (i, elem) in elements.iter().enumerate() {
        if let (Some(bid), Some(label)) = (
            idx_to_backend.get(&i).copied(),
            elem.get("label").and_then(|v| v.as_str()),
        ) {
            let label = label.trim();
            if !label.is_empty() {
                out.insert(bid, label.to_string());
            }
        }
    }
    Ok(out)
}

/// Find the `backendNodeId`s of every element inside the topmost open
/// modal/drawer (issue #90), so `snapshot -i` can mark them with a `modal` attr.
///
/// Element-Plus / Ant / native `<dialog>` all leave the background page mounted
/// and interactive underneath an open overlay, so the AX tree (and thus the flat
/// snapshot) mixes drawer controls with background controls. We pick the topmost
/// visible modal container by effective z-index (DOM order breaks ties — a later
/// popup stacks on top), tag its whole subtree, and resolve the backend ids with
/// the same batch dance the label/cursor scans use. Best-effort: any CDP failure
/// yields an empty set (snapshot just omits the marker).
async fn find_top_layer_backend_ids(
    client: &CdpClient,
    session_id: &str,
) -> Result<std::collections::HashSet<i64>, String> {
    let js = r#"
(function() {
    if (!document.body) return 0;
    var SEL = '[role=dialog],[aria-modal="true"],dialog[open],' +
              '.el-dialog,.el-drawer,.el-message-box,.ant-modal,.ant-drawer';
    function vis(el){
        var r = el.getBoundingClientRect();
        if (r.width === 0 && r.height === 0) return false;
        var cs = getComputedStyle(el);
        return cs.visibility !== 'hidden' && cs.display !== 'none';
    }
    var cands = [].slice.call(document.querySelectorAll(SEL)).filter(vis);
    try {
        var modals = [].slice.call(document.querySelectorAll(':modal'));
        for (var m = 0; m < modals.length; m++)
            if (cands.indexOf(modals[m]) < 0 && vis(modals[m])) cands.push(modals[m]);
    } catch (e) {}
    if (!cands.length) return 0;
    function zed(el){
        var z = 0, n = el;
        while (n && n !== document.body) {
            var v = parseInt(getComputedStyle(n).zIndex, 10);
            if (!isNaN(v)) z = Math.max(z, v);
            n = n.parentElement;
        }
        return z;
    }
    // Topmost = highest effective z-index; ties resolve to the later candidate
    // (DOM order ≈ stacking order for equal z-index), so a nested dialog opened
    // inside a drawer wins over the drawer.
    var top = cands[0], topZ = zed(cands[0]);
    for (var i = 1; i < cands.length; i++) {
        var z = zed(cands[i]);
        if (z >= topZ) { topZ = z; top = cands[i]; }
    }
    var all = [top].concat([].slice.call(top.querySelectorAll('*')));
    var n = 0;
    for (var j = 0; j < all.length; j++) all[j].setAttribute('data-__ab-top', String(n++));
    return n;
})()
"#
    .to_string();

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

    let count = result.result.value.and_then(|v| v.as_i64()).unwrap_or(0);
    if count == 0 {
        return Ok(std::collections::HashSet::new());
    }

    let idx_to_backend = resolve_tagged_backend_ids(client, session_id, "data-__ab-top").await;
    Ok(idx_to_backend.into_values().collect())
}

/// Resolve the `backendNodeId` for every element previously tagged with a
/// numeric-indexed data attribute (e.g. `data-__ab-err="3"`), then strip the
/// attribute. Returns a map from that index to its `backendNodeId`.
///
/// Mirrors the batch-resolution dance used by the cursor scan: `DOM.getDocument`
/// for the root, one `DOM.querySelectorAll` to find tagged nodes, concurrent
/// `DOM.describeNode` calls to read each backendNodeId + index, then a cleanup
/// pass. On any CDP failure it returns an empty map (best-effort: callers still
/// surface the text, just without a clickable ref).
async fn resolve_tagged_backend_ids(
    client: &CdpClient,
    session_id: &str,
    attr: &str,
) -> HashMap<usize, i64> {
    let mut idx_to_backend: HashMap<usize, i64> = HashMap::new();

    let Ok(doc) = client
        .send_command(
            "DOM.getDocument",
            Some(serde_json::json!({ "depth": 0 })),
            Some(session_id),
        )
        .await
    else {
        return idx_to_backend;
    };
    let Some(root_node_id) = doc
        .get("root")
        .and_then(|r| r.get("nodeId"))
        .and_then(|v| v.as_i64())
    else {
        return idx_to_backend;
    };

    let Ok(query_result) = client
        .send_command(
            "DOM.querySelectorAll",
            Some(serde_json::json!({
                "nodeId": root_node_id,
                "selector": format!("[{}]", attr),
            })),
            Some(session_id),
        )
        .await
    else {
        return idx_to_backend;
    };

    let node_ids: Vec<i64> = query_result
        .get("nodeIds")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect())
        .unwrap_or_default();

    let describe_futures: Vec<_> = node_ids
        .iter()
        .map(|&node_id| {
            client.send_command(
                "DOM.describeNode",
                Some(serde_json::json!({ "nodeId": node_id })),
                Some(session_id),
            )
        })
        .collect();
    let describe_results = futures_util::future::join_all(describe_futures).await;

    for desc in describe_results.into_iter().flatten() {
        let backend_id = desc
            .get("node")
            .and_then(|n| n.get("backendNodeId"))
            .and_then(|v| v.as_i64());
        let idx = desc
            .get("node")
            .and_then(|n| n.get("attributes"))
            .and_then(|a| a.as_array())
            .and_then(|attrs| {
                // attributes is a flat [name, value, name, value, ...] array.
                attrs
                    .iter()
                    .enumerate()
                    .find(|(_, v)| v.as_str() == Some(attr))
                    .and_then(|(i, _)| attrs.get(i + 1))
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<usize>().ok())
            });
        if let (Some(bid), Some(idx)) = (backend_id, idx) {
            idx_to_backend.insert(idx, bid);
        }
    }

    // Strip the data attributes we injected.
    let cleanup_js = format!(
        r#"(function(){{ var els = document.querySelectorAll('[{attr}]'); for (var i = 0; i < els.length; i++) els[i].removeAttribute('{attr}'); return els.length; }})()"#,
    );
    if let Err(e) = client
        .send_command_typed::<EvaluateParams, EvaluateResult>(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: cleanup_js,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await
    {
        eprintln!("[chrome-use] Warning: failed to clean up {attr} attributes: {e}");
    }

    idx_to_backend
}

/// Promote LabelText/generic nodes that wrap a hidden radio/checkbox input.
/// When a `<label>` contains a `display:none` `<input type="radio">`, Chrome excludes
/// the input from the AX tree entirely, leaving only the label with role="LabelText"
/// and an empty name. We detect these via cursor-interactive scanning and promote
/// the label to the correct input role so consumers see role="radio" in data.refs.
fn promote_hidden_inputs(
    tree_nodes: &mut [TreeNode],
    cursor_elements: &HashMap<i64, CursorElementInfo>,
) {
    for node in tree_nodes.iter_mut() {
        if !matches!(node.role.as_str(), "LabelText" | "generic") {
            continue;
        }
        let cursor_info = match node
            .backend_node_id
            .and_then(|bid| cursor_elements.get(&bid))
        {
            Some(info) => info,
            None => continue,
        };
        if let Some(input_kind) = cursor_info.hidden_input_kind {
            node.role = input_kind.as_role().to_string();
            if node.name.is_empty() && !cursor_info.text.is_empty() {
                node.name = cursor_info.text.clone();
            }
            if let Some(ref checked) = cursor_info.hidden_input_checked {
                node.checked = Some(checked.clone());
            }
        }
    }
}

fn build_tree(nodes: &[AXNode]) -> (Vec<TreeNode>, Vec<usize>) {
    let mut tree_nodes: Vec<TreeNode> = Vec::with_capacity(nodes.len());
    let mut id_to_idx: HashMap<String, usize> = HashMap::new();

    for (i, node) in nodes.iter().enumerate() {
        let role = extract_ax_string(&node.role);
        let name = extract_ax_string(&node.name);
        let value_text = extract_ax_string_opt(&node.value);

        let (level, checked, expanded, selected, disabled, required) =
            extract_properties(&node.properties);

        if (node.ignored.unwrap_or(false) && role != "RootWebArea") || role == "InlineTextBox" {
            tree_nodes.push(TreeNode::empty());
            id_to_idx.insert(node.node_id.clone(), i);
            continue;
        }

        tree_nodes.push(TreeNode {
            role,
            ax_name: name.clone(),
            name,
            level,
            checked,
            expanded,
            selected,
            disabled,
            required,
            value_text,
            backend_node_id: node.backend_d_o_m_node_id,
            children: Vec::new(),
            parent_idx: None,
            has_ref: false,
            ref_id: None,
            depth: 0,
            cursor_info: None,
            url: None,
            in_top_layer: false,
        });
        id_to_idx.insert(node.node_id.clone(), i);
    }

    // Build parent-child relationships
    for (i, node) in nodes.iter().enumerate() {
        if let Some(ref child_ids) = node.child_ids {
            for cid in child_ids {
                if let Some(&child_idx) = id_to_idx.get(cid) {
                    tree_nodes[i].children.push(child_idx);
                    tree_nodes[child_idx].parent_idx = Some(i);
                }
            }
        }
    }

    // Process StaticText aggregation
    for i in 0..tree_nodes.len() {
        if tree_nodes[i].role.is_empty() || tree_nodes[i].children.is_empty() {
            continue;
        }

        let children_indices: Vec<usize> = tree_nodes[i].children.clone();

        // Continuous StaticText nodes at the same level are an artifact of HTML structure rather than semantic meaning.
        // They typically represent a single continuous piece of text on the page that was split due to inline elements, formatting tags, or other structural reasons.
        // Thus, continuous StaticText children are aggregated into the first one.
        let mut start = 0;
        while start < children_indices.len() {
            // Skip non-StaticText nodes
            if tree_nodes[children_indices[start]].role != "StaticText" {
                start += 1;
                continue;
            }

            // Find the end of the current StaticText sequence
            let mut end = start + 1;
            while end < children_indices.len()
                && tree_nodes[children_indices[end]].role == "StaticText"
            {
                end += 1;
            }

            // If we have a sequence of at least two StaticText
            if end > start + 1 {
                // Collect and aggregate all names from the sequence
                let aggregated_name: String = (start..end)
                    .map(|idx| tree_nodes[children_indices[idx]].name.clone())
                    .collect();
                // Always aggregate into the first node of the sequence
                tree_nodes[children_indices[start]].name = aggregated_name;
                // Clear the rest of the nodes in the sequence (from start+1 to end-1)
                for j in (start + 1)..end {
                    tree_nodes[children_indices[j]].clear();
                }
            }
            start = end;
        }

        // Deduplicate redundant StaticText
        if children_indices.len() == 1
            && tree_nodes[children_indices[0]].role == "StaticText"
            && tree_nodes[i].name == tree_nodes[children_indices[0]].name
        {
            tree_nodes[children_indices[0]].clear();
        }
    }

    // Set depths
    let mut root_indices = Vec::new();
    let children_exist: Vec<bool> = nodes.iter().map(|_| false).collect();
    let mut is_child = children_exist;
    for node in &tree_nodes {
        for &child in &node.children {
            is_child[child] = true;
        }
    }
    for (i, &is_c) in is_child.iter().enumerate() {
        if !is_c {
            root_indices.push(i);
        }
    }

    fn set_depth(nodes: &mut [TreeNode], idx: usize, depth: usize) {
        nodes[idx].depth = depth;
        let children: Vec<usize> = nodes[idx].children.clone();
        for child_idx in children {
            set_depth(nodes, child_idx, depth + 1);
        }
    }

    for &root in &root_indices {
        set_depth(&mut tree_nodes, root, 0);
    }

    (tree_nodes, root_indices)
}

/// Recover short, local text lost by interactive filtering without changing
/// accessible names or ref identity. Never cross a nested item boundary or
/// walk an unbounded subtree merely to annotate one control.
/// Context for a control whose name is not unique on the page.
///
/// [`control_context`] only speaks for containers it can vouch for — a semantic
/// `article`/`listitem`/`row`, one with a heading, or a linked product card.
/// A card header that is just a title and a button matches none of those, so
/// three cards produced three identical `button "进入"` lines with nothing to
/// tell them apart, and the caller had to walk the DOM by hand (issue #281).
///
/// Duplication is exactly when the risk of adding noise is worth taking: a
/// unique name needs no help, and an ambiguous one is unusable without it. So
/// this runs only for repeated (role, name) pairs, and accepts a looser
/// container — text plus this one control.
///
/// The result must actually separate them. If a sibling duplicate would get
/// the same words, the context has told the reader nothing and is dropped:
/// a second identical line is worse than a short one.
fn disambiguating_context(nodes: &[TreeNode], idx: usize) -> Option<String> {
    let me = &nodes[idx];
    if me.ref_id.is_none() || me.name.trim().is_empty() || !is_interactive_role(&me.role) {
        return None;
    }
    let twins: Vec<usize> = nodes
        .iter()
        .enumerate()
        .filter(|(j, n)| *j != idx && n.ref_id.is_some() && n.role == me.role && n.name == me.name)
        .map(|(j, _)| j)
        .collect();
    if twins.is_empty() {
        return None;
    }
    let mine = nearest_labelling_text(nodes, idx)?;
    if twins
        .iter()
        .any(|&j| nearest_labelling_text(nodes, j).as_deref() == Some(mine.as_str()))
    {
        return None;
    }
    Some(mine)
}

/// Text from the nearest ancestor that holds this control and nothing else
/// interactive — a card header, a table cell, a labelled row.
///
/// Stops at the first such ancestor rather than climbing to the widest one:
/// the outer grid holds every card, and its text describes all of them equally.
fn nearest_labelling_text(nodes: &[TreeNode], idx: usize) -> Option<String> {
    let mut parent = nodes[idx].parent_idx;
    for _ in 0..4 {
        let root = parent?;
        let node = &nodes[root];
        if matches!(
            node.role.as_str(),
            "RootWebArea" | "WebArea" | "dialog" | "document"
        ) {
            return None;
        }
        parent = node.parent_idx;

        let mut pending: Vec<usize> = node.children.iter().rev().copied().collect();
        let mut visited = 0;
        let mut controls = 0;
        let mut text: Vec<String> = Vec::new();
        while let Some(child) = pending.pop() {
            visited += 1;
            if visited > 64 {
                return None;
            }
            let n = &nodes[child];
            if is_interactive_role(&n.role) || n.cursor_info.is_some() {
                controls += 1;
                // More than this one control means the ancestor covers
                // several actions, so its text does not belong to ours.
                if controls > 1 {
                    break;
                }
                continue;
            }
            if matches!(n.role.as_str(), "heading" | "StaticText") && !n.name.trim().is_empty() {
                let value = n.name.split_whitespace().collect::<Vec<_>>().join(" ");
                // The control's own name is not context for itself.
                if value != nodes[idx].name && !text.contains(&value) {
                    text.push(value);
                }
            } else {
                pending.extend(n.children.iter().rev().copied());
            }
        }
        if controls > 1 || text.is_empty() {
            continue;
        }
        let joined = text.join(" | ");
        let trimmed: String = joined.chars().take(120).collect();
        return Some(if trimmed.len() < joined.len() {
            format!("{trimmed} [truncated]")
        } else {
            trimmed
        });
    }
    None
}

fn control_context(nodes: &[TreeNode], idx: usize) -> Option<String> {
    if !is_interactive_role(&nodes[idx].role) {
        return None;
    }
    let mut parent = nodes[idx].parent_idx;
    for _ in 0..4 {
        let root = parent?;
        let node = &nodes[root];
        if matches!(
            node.role.as_str(),
            "RootWebArea" | "WebArea" | "dialog" | "document"
        ) {
            return None;
        }
        parent = node.parent_idx;
        if !matches!(
            node.role.as_str(),
            "article" | "listitem" | "row" | "group" | "generic" | ""
        ) {
            continue;
        }
        let semantic = matches!(node.role.as_str(), "article" | "listitem" | "row");
        let mut pending: Vec<usize> = node.children.iter().rev().copied().collect();
        let mut visited = 0;
        let mut headings = 0;
        let mut link_names = Vec::new();
        let mut controls = 0;
        let mut text = Vec::new();
        while let Some(child) = pending.pop() {
            visited += 1;
            if visited > 64 {
                return None;
            }
            let n = &nodes[child];
            if matches!(n.role.as_str(), "article" | "listitem" | "row") {
                // An outer container must not mix sibling products or rows.
                return None;
            }
            if n.role == "link" && !n.name.trim().is_empty() {
                if !link_names.contains(&n.name) {
                    link_names.push(n.name.clone());
                }
                if !text.contains(&n.name) {
                    text.push(n.name.clone());
                }
                continue;
            }
            if is_interactive_role(&n.role) || n.cursor_info.is_some() {
                controls += 1;
                continue;
            }
            if n.role == "heading" {
                headings += 1;
                if headings > 1 {
                    return None;
                }
            }
            if matches!(n.role.as_str(), "heading" | "StaticText") && !n.name.trim().is_empty() {
                let value = n.name.split_whitespace().collect::<Vec<_>>().join(" ");
                if !text.contains(&value) {
                    text.push(value);
                }
            } else {
                pending.extend(n.children.iter().rev().copied());
            }
        }
        let linked_item = link_names.len() == 1 && controls == 1;
        if (!semantic && headings == 0 && !linked_item) || text.is_empty() {
            continue;
        }
        // A linked card's action already carries this context. Keep its named
        // navigation links concise instead of repeating the same product block.
        if linked_item && nodes[idx].role == "link" && nodes[idx].name == link_names[0] {
            return None;
        }
        if text.len() == 1 && text[0] == nodes[idx].name {
            return None;
        }
        // Short fields (for example price and availability) must not disappear
        // behind a long description. Stable ordering preserves peers' order.
        text.sort_by_key(|value| value.len() > 96);
        let mut context = text.join(" | ");
        if context.len() > 256 {
            let mut end = 256;
            while !context.is_char_boundary(end) {
                end -= 1;
            }
            context.truncate(end);
            context.push_str(" [truncated]");
        }
        return Some(context);
    }
    None
}

/// Keep live status receipts visible even when ordinary static text is
/// filtered. Bound both traversal and UTF-8 output, and never mint action refs.
fn status_summary(nodes: &[TreeNode], idx: usize) -> String {
    let mut pending = vec![idx];
    let mut parts = Vec::new();
    let mut remaining = 512;
    let mut truncated = false;
    for _ in 0..64 {
        let Some(current) = pending.pop() else { break };
        let node = &nodes[current];
        if current != idx && is_interactive_role(&node.role) {
            continue;
        }
        if !node.name.is_empty() && (current == idx || node.role == "StaticText") {
            let mut end = node.name.len().min(remaining);
            while !node.name.is_char_boundary(end) {
                end -= 1;
            }
            if end > 0 {
                parts.push(node.name[..end].to_string());
            }
            remaining = remaining.saturating_sub(end + 3);
            if end < node.name.len() || remaining == 0 {
                truncated = true;
                break;
            }
        }
        pending.extend(node.children.iter().rev().copied());
    }
    truncated |= !pending.is_empty();
    let mut text = parts.join(" | ");
    if truncated {
        text.push_str(" [truncated]");
    }
    text
}

/// Ref attributes can be followed by context, modal, or future metadata.
/// Match the attribute boundary so e2 never selects e20.
fn find_snapshot_ref(output: &str, ref_id: &str) -> Option<usize> {
    let prefix = format!("[ref={}", ref_id);
    output.match_indices(&prefix).find_map(|(pos, _)| {
        matches!(output.as_bytes().get(pos + prefix.len()), Some(b']' | b',')).then_some(pos)
    })
}

fn render_tree(
    nodes: &[TreeNode],
    idx: usize,
    indent: usize,
    output: &mut String,
    options: &SnapshotOptions,
) {
    let node = &nodes[idx];

    // Reduce unnecessary indentation and rendering
    if node.role.is_empty()
        || (node.role == "generic" && !node.has_ref && node.children.len() <= 1)
        || (node.role == "StaticText" && node.name.replace(INVISIBLE_CHARS, "").is_empty())
    {
        // Ignored node -- still render children
        for &child in &node.children {
            render_tree(nodes, child, indent, output, options);
        }
        return;
    }

    if let Some(max_depth) = options.depth {
        if indent > max_depth {
            return;
        }
    }

    let role = &node.role;

    // Skip root WebArea wrapper
    if role == "RootWebArea" || role == "WebArea" {
        for &child in &node.children {
            render_tree(nodes, child, indent, output, options);
        }
        return;
    }

    if options.interactive && role == "status" {
        let summary = status_summary(nodes, idx);
        if !summary.is_empty() {
            output.push_str(&format!(
                "{}- status: {}\n",
                "  ".repeat(indent),
                serde_json::to_string(&summary).unwrap_or_default()
            ));
        }
        // Clickable status nodes must retain their own ref and cursor metadata.
        // Non-actionable status nodes only need the summary and child controls.
        if !node.has_ref {
            for &child in &node.children {
                render_tree(nodes, child, indent, output, options);
            }
            return;
        }
    }

    if options.interactive && !node.has_ref {
        // In interactive mode, skip non-interactive but render children
        for &child in &node.children {
            render_tree(nodes, child, indent, output, options);
        }
        return;
    }

    let prefix = "  ".repeat(indent);
    let mut line = format!("{}- {}", prefix, role);

    // Use ARIA name if available, only fall back to cursor-interactive textContent in interactive mode since their visible text in child nodes is filtered out
    let unescaped_display_name = if !node.name.is_empty() {
        &node.name
    } else if options.interactive {
        if let Some(ref ci) = node.cursor_info {
            &ci.text
        } else {
            &node.name
        }
    } else {
        &node.name
    };
    if !unescaped_display_name.is_empty() {
        if let Ok(display_name) = serde_json::to_string(&unescaped_display_name) {
            line.push_str(&format!(" {}", display_name.replace(INVISIBLE_CHARS, "")));
        }
    }

    // Properties
    let mut attrs = Vec::new();

    if let Some(level) = node.level {
        attrs.push(format!("level={}", level));
    }
    if let Some(ref checked) = node.checked {
        attrs.push(format!("checked={}", checked));
    }
    if let Some(expanded) = node.expanded {
        attrs.push(format!("expanded={}", expanded));
    }
    if let Some(selected) = node.selected {
        if selected {
            attrs.push("selected".to_string());
        }
    }
    if let Some(disabled) = node.disabled {
        if disabled {
            attrs.push("disabled".to_string());
        }
    }
    if let Some(required) = node.required {
        if required {
            attrs.push("required".to_string());
        }
    }

    if let Some(ref ref_id) = node.ref_id {
        attrs.push(format!("ref={}", ref_id));
    }

    if options.interactive {
        if let Some(context) =
            control_context(nodes, idx).or_else(|| disambiguating_context(nodes, idx))
        {
            if let Ok(encoded) = serde_json::to_string(&context) {
                attrs.push(format!("context={}", encoded));
            }
        }
    }

    // Top-layer marker (issue #90): distinguishes controls inside the open
    // modal/drawer from the background page controls `snapshot -i` also lists.
    if node.in_top_layer {
        attrs.push("modal".to_string());
    }

    if let Some(ref url) = node.url {
        attrs.push(format!("url={}", url));
    }

    if !attrs.is_empty() {
        line.push_str(&format!(" [{}]", attrs.join(", ")));
    }

    // Add cursor-interactive kind & hints
    if let Some(ref cursor_info) = node.cursor_info {
        line.push_str(&format!(
            " {} [{}]",
            cursor_info.kind,
            cursor_info.hints.join(", ")
        ));
    }

    // Value
    if let Some(ref val) = node.value_text {
        if !val.is_empty() && val != &node.name {
            line.push_str(&format!(": {}", val));
        }
    }

    output.push_str(&line);
    output.push('\n');

    for &child in &node.children {
        render_tree(nodes, child, indent + 1, output, options);
    }
}

/// True if a snapshot line names an interactive ARIA role. Compaction keeps
/// these even without a `ref=`/`": "` marker, so a clickable control never gets
/// dropped from `-c` output (the dogfood reports saw a button present in the full
/// snapshot vanish from compact, leaving the agent clicking an empty ref).
fn is_interactive_line(line: &str) -> bool {
    const ROLES: &[&str] = &[
        "button",
        "link",
        "textbox",
        "checkbox",
        "radio",
        "combobox",
        "listbox",
        "menuitem",
        "menuitemcheckbox",
        "menuitemradio",
        "option",
        "switch",
        "slider",
        "spinbutton",
        "searchbox",
        "tab ",
        "clickable",
        "focusable",
        "editable",
        "alert",
    ];
    let t = line.trim_start();
    // Lines look like `- button "Label" [ref=e1]`; match the role token after the
    // leading "- " marker.
    let t = t.strip_prefix("- ").unwrap_or(t);
    ROLES.iter().any(|r| t.starts_with(r))
}

fn compact_tree(tree: &str, interactive: bool) -> String {
    let lines: Vec<&str> = tree.lines().collect();
    if lines.is_empty() {
        return String::new();
    }

    let mut keep = vec![false; lines.len()];

    for (i, line) in lines.iter().enumerate() {
        if line.contains("ref=") || line.contains(": ") || is_interactive_line(line) {
            keep[i] = true;
            // Mark ancestors
            let my_indent = count_indent(line);
            for j in (0..i).rev() {
                let ancestor_indent = count_indent(lines[j]);
                if ancestor_indent < my_indent {
                    keep[j] = true;
                    if ancestor_indent == 0 {
                        break;
                    }
                }
            }
        }
    }

    let result: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| keep[*i])
        .map(|(_, line)| *line)
        .collect();

    let output = result.join("\n");
    if output.trim().is_empty() && interactive {
        return "(no interactive elements)".to_string();
    }
    output
}

fn count_indent(line: &str) -> usize {
    let trimmed = line.trim_start();
    (line.len() - trimmed.len()) / 2
}

/// Keep only snapshot lines matching `pattern` (case-insensitive regex), plus the
/// ancestor chain of each match for window/section context. Refs on kept lines
/// stay valid (assigned at snapshot time, independent of filtering).
///
/// For desktop-shell web apps (Synology DSM, NAS/router admin panels, ExtJS) one
/// snapshot holds many independent app windows, so `snapshot -i` blows past the
/// token cap and buries the target controls. `--filter "SSH|端口|应用|确定"` cuts
/// it down to the few relevant lines (issue #65) — the productized form of the
/// `| tail -80` workaround, but tree-aware and ref-preserving.
/// What a budgeted snapshot left out, so the caller knows the tree is partial
/// and how to read the rest.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeBudget {
    /// The kept slice of the tree.
    pub tree: String,
    /// Total nodes (lines) in the full tree, before budgeting.
    pub total_nodes: usize,
    /// Index of the first node in `tree`.
    pub from: usize,
    /// Index one past the last node in `tree`; also the cursor to resume at.
    pub next: usize,
}

impl TreeBudget {
    pub fn truncated(&self) -> bool {
        self.from > 0 || self.next < self.total_nodes
    }
}

/// Cut a snapshot to a byte budget on whole-node boundaries.
///
/// A snapshot of a comment thread or a long feed can run to hundreds of
/// kilobytes, nearly all of it prose that has nothing to do with the element
/// the caller is after. Truncating the string would leave the caller unable to
/// tell a short page from a clipped one, and could sever a node mid-line so the
/// last `[ref=eN]` no longer parses. So cut between nodes and report the range
/// that was dropped plus the cursor to resume from.
///
/// At least one node is always returned, even when it alone exceeds the budget:
/// an empty tree would read as "nothing on the page".
pub fn budget_tree(tree: &str, max_bytes: usize, from: usize) -> TreeBudget {
    let lines: Vec<&str> = tree.lines().collect();
    let total_nodes = lines.len();
    let from = from.min(total_nodes);
    let mut used = 0usize;
    let mut next = from;
    for line in &lines[from..] {
        // +1 for the newline this line will be joined with.
        let cost = line.len() + 1;
        if used + cost > max_bytes && next > from {
            break;
        }
        used += cost;
        next += 1;
    }
    TreeBudget {
        tree: lines[from..next].join("\n"),
        total_nodes,
        from,
        next,
    }
}

pub fn filter_tree(tree: &str, pattern: &str) -> Result<String, String> {
    let re = regex_lite::Regex::new(&format!("(?i){pattern}"))
        .map_err(|e| format!("invalid --filter regex '{pattern}': {e}"))?;
    let lines: Vec<&str> = tree.lines().collect();
    let mut keep = vec![false; lines.len()];
    for (i, line) in lines.iter().enumerate() {
        if !re.is_match(line) {
            continue;
        }
        keep[i] = true;
        // Walk back to the root keeping only the strictly-shallower ancestor chain
        // (direct parent, grandparent, …) — not shallower siblings.
        let mut want = count_indent(line);
        for j in (0..i).rev() {
            let ind = count_indent(lines[j]);
            if ind < want {
                keep[j] = true;
                want = ind;
                if ind == 0 {
                    break;
                }
            }
        }
    }
    let kept: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| keep[*i])
        .map(|(_, l)| *l)
        .collect();
    if kept.is_empty() {
        return Ok(format!("(no elements match /{pattern}/)"));
    }
    Ok(kept.join("\n"))
}

fn extract_ax_string(value: &Option<AXValue>) -> String {
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

fn extract_ax_string_opt(value: &Option<AXValue>) -> Option<String> {
    match value {
        Some(v) => match &v.value {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        },
        None => None,
    }
}

type NodeProperties = (
    Option<i64>,    // level
    Option<String>, // checked
    Option<bool>,   // expanded
    Option<bool>,   // selected
    Option<bool>,   // disabled
    Option<bool>,   // required
);

fn extract_properties(props: &Option<Vec<AXProperty>>) -> NodeProperties {
    let mut level = None;
    let mut checked = None;
    let mut expanded = None;
    let mut selected = None;
    let mut disabled = None;
    let mut required = None;

    if let Some(properties) = props {
        for prop in properties {
            match prop.name.as_str() {
                "level" => {
                    level = prop.value.value.as_ref().and_then(|v| v.as_i64());
                }
                "checked" => {
                    checked = prop.value.value.as_ref().map(|v| match v {
                        Value::String(s) => s.clone(),
                        Value::Bool(b) => b.to_string(),
                        _ => "false".to_string(),
                    });
                }
                "expanded" => {
                    expanded = prop.value.value.as_ref().and_then(|v| v.as_bool());
                }
                "selected" => {
                    selected = prop.value.value.as_ref().and_then(|v| v.as_bool());
                }
                "disabled" => {
                    disabled = prop.value.value.as_ref().and_then(|v| v.as_bool());
                }
                "required" => {
                    required = prop.value.value.as_ref().and_then(|v| v.as_bool());
                }
                // AX_DUMP_PROPS: ground truth for what Chrome actually sends on
                // real pages. CDP has no "available actions" list, so any
                // secondary-action support has to be derived from these — and
                // the protocol reference is not evidence of what a given build
                // emits for a given element.
                other => {
                    if let Ok(path) = std::env::var("AGENT_BROWSER_AX_DUMP_PROPS") {
                        use std::io::Write;
                        if let Ok(mut f) = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&path)
                        {
                            let _ = writeln!(
                                f,
                                "{other}\t{}",
                                prop.value
                                    .value
                                    .as_ref()
                                    .map(|v| v.to_string())
                                    .unwrap_or_else(|| "-".into())
                            );
                        }
                    }
                }
            }
        }
    }

    (level, checked, expanded, selected, disabled, required)
}

/// What an element exposes beyond a plain click, derived from its computed
/// accessibility properties.
///
/// CDP has no "available actions" list the way the macOS accessibility API
/// does, but it does report the properties those actions follow from. Measured
/// on Chrome (see `AGENT_BROWSER_AX_DUMP_PROPS`), a page of assorted controls
/// yields `hasPopup` ("menu"/"listbox"/"dialog"), `pressed`, `valuemin`,
/// `valuemax`, `valuetext`, `settable`, `readonly`, `multiselectable` and
/// `orientation` — everything the actions below are inferred from.
///
/// The list is a contract, not a suggestion: an action that is not derived here
/// must be refused rather than attempted, so a caller can never invoke an action
/// this element does not actually support and read the resulting no-op as
/// success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SecondaryAction {
    Expand,
    Collapse,
    ShowMenu,
    Increment,
    Decrement,
    Toggle,
}

impl SecondaryAction {
    pub fn as_str(self) -> &'static str {
        match self {
            SecondaryAction::Expand => "expand",
            SecondaryAction::Collapse => "collapse",
            SecondaryAction::ShowMenu => "showMenu",
            SecondaryAction::Increment => "increment",
            SecondaryAction::Decrement => "decrement",
            SecondaryAction::Toggle => "toggle",
        }
    }
}

/// The accessibility facts an action is derived from. Values are the raw CDP
/// property values; `expanded`/`pressed` are three-state because "absent" and
/// "present and false" mean different things — absent means the control does not
/// expand at all.
#[derive(Debug, Default, Clone)]
pub struct AxActionFacts {
    pub disabled: bool,
    pub readonly: bool,
    pub expanded: Option<bool>,
    pub pressed: Option<bool>,
    pub has_popup: Option<String>,
    pub has_value_range: bool,
}

/// Derive the actions an element supports besides clicking it.
///
/// A disabled element supports none: offering `expand` on a control that cannot
/// be operated invites a caller to spend a round trip discovering that.
pub fn secondary_actions(facts: &AxActionFacts) -> Vec<SecondaryAction> {
    if facts.disabled {
        return Vec::new();
    }
    let mut out = Vec::new();
    match facts.expanded {
        Some(false) => out.push(SecondaryAction::Expand),
        Some(true) => out.push(SecondaryAction::Collapse),
        None => {}
    }
    // `hasPopup: "false"` is how Chrome says "no popup" on some builds; only a
    // real popup kind is an action.
    if let Some(kind) = facts.has_popup.as_deref() {
        if !kind.is_empty() && kind != "false" {
            out.push(SecondaryAction::ShowMenu);
        }
    }
    if facts.has_value_range && !facts.readonly {
        out.push(SecondaryAction::Increment);
        out.push(SecondaryAction::Decrement);
    }
    if facts.pressed.is_some() {
        out.push(SecondaryAction::Toggle);
    }
    out
}

/// Read the action-relevant facts out of a node's computed AX properties.
///
/// Kept separate from `extract_properties` (which feeds the rendered tree) so
/// adding a fact here never changes what a snapshot prints — the action set is
/// queried on demand, deliberately not stamped onto every line.
pub fn action_facts_from_properties(props: &Option<Vec<AXProperty>>) -> AxActionFacts {
    let mut f = AxActionFacts::default();
    let (mut has_min, mut has_max) = (false, false);
    if let Some(properties) = props {
        for prop in properties {
            let v = prop.value.value.as_ref();
            match prop.name.as_str() {
                "disabled" => f.disabled = v.and_then(|v| v.as_bool()).unwrap_or(false),
                "readonly" => f.readonly = v.and_then(|v| v.as_bool()).unwrap_or(false),
                "expanded" => f.expanded = v.and_then(|v| v.as_bool()),
                // Chrome sends `pressed` as the string "true"/"false", not a bool.
                "pressed" => {
                    f.pressed = v.and_then(|v| match v {
                        Value::Bool(b) => Some(*b),
                        Value::String(s) => Some(s == "true"),
                        _ => None,
                    })
                }
                "hasPopup" => f.has_popup = v.and_then(|v| v.as_str()).map(str::to_string),
                "valuemin" => has_min = true,
                "valuemax" => has_max = true,
                _ => {}
            }
        }
    }
    f.has_value_range = has_min && has_max;
    f
}

/// Build the set of texts to de-duplicate cursor-interactive elements against.
///
/// All ref-bearing ARIA tree nodes have their names stored in `ref_map` during
/// tree construction, so the ref-map entries are the single source of truth.
/// This avoids fragile parsing of the rendered tree text.
fn build_dedup_set(ref_map: &RefMap) -> std::collections::HashSet<String> {
    ref_map
        .entries_sorted()
        .into_iter()
        .filter(|(_, entry)| !entry.name.is_empty())
        .map(|(_, entry)| entry.name.to_lowercase())
        .collect()
}

/// Recursively collect all `backendNodeId` values from a CDP DOM node tree
/// (as returned by `DOM.describeNode` with `depth: -1`).
fn collect_backend_node_ids(node: &Value, ids: &mut std::collections::HashSet<i64>) {
    if let Some(id) = node.get("backendNodeId").and_then(|v| v.as_i64()) {
        ids.insert(id);
    }
    if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
        for child in children {
            collect_backend_node_ids(child, ids);
        }
    }
    // Shadow DOM and content documents
    if let Some(shadow) = node.get("shadowRoots").and_then(|v| v.as_array()) {
        for child in shadow {
            collect_backend_node_ids(child, ids);
        }
    }
    if let Some(doc) = node.get("contentDocument") {
        collect_backend_node_ids(doc, ids);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interactive_roles() {
        assert!(INTERACTIVE_ROLES.contains(&"button"));
        assert!(INTERACTIVE_ROLES.contains(&"textbox"));
        assert!(!INTERACTIVE_ROLES.contains(&"heading"));
    }

    #[test]
    fn test_content_roles() {
        assert!(CONTENT_ROLES.contains(&"heading"));
        assert!(!CONTENT_ROLES.contains(&"button"));
    }

    #[test]
    fn test_compact_tree_basic() {
        let tree = "- navigation\n  - link \"Home\" [ref=e1]\n  - link \"About\" [ref=e2]\n- main\n  - heading \"Title\"\n  - paragraph\n    - text: Hello\n";
        let result = compact_tree(tree, false);
        assert!(result.contains("[ref=e1]"));
        assert!(result.contains("[ref=e2]"));
        assert!(result.contains("Hello"));
    }

    #[test]
    fn test_compact_tree_radio_checkbox() {
        // Radio/checkbox lines have attributes before ref (e.g. [checked=false, ref=e1])
        // so "ref=" appears without a leading "[" — compact_tree must still keep them.
        let tree = "- form\n  - radio \"Single unit\" [checked=false, ref=e1]\n  - checkbox \"I agree\" [checked=false, ref=e2]\n  - button \"Submit\" [ref=e3]\n";
        let result = compact_tree(tree, true);
        assert!(
            result.contains("radio \"Single unit\""),
            "radio should be kept"
        );
        assert!(
            result.contains("checkbox \"I agree\""),
            "checkbox should be kept"
        );
        assert!(
            result.contains("button \"Submit\""),
            "button should be kept"
        );
    }

    #[test]
    fn test_compact_tree_empty_interactive() {
        let result = compact_tree("- generic\n", true);
        assert_eq!(result, "(no interactive elements)");
    }

    #[test]
    fn test_filter_tree_keeps_matches_with_ancestors_and_refs() {
        // Desktop-shell shape: two app windows; filter should keep only the
        // matching controls + their window ancestor, dropping the other window.
        let tree = "\
- application
  - group \"控制面板\"
    - checkbox \"启动 SSH 功能\" [checked=true, ref=e5]
    - textbox \"端口：\" [ref=e6]: 10022
    - button \"应用\" [ref=e7]
  - group \"Package Center\"
    - listitem \"some package\" [ref=e20]
    - listitem \"another\" [ref=e21]";
        let out = filter_tree(tree, "SSH|端口|应用").unwrap();
        // matched controls kept, with refs intact
        assert!(out.contains("checkbox \"启动 SSH 功能\" [checked=true, ref=e5]"));
        assert!(out.contains("textbox \"端口：\" [ref=e6]: 10022"));
        assert!(out.contains("button \"应用\" [ref=e7]"));
        // ancestor window kept for context
        assert!(out.contains("控制面板"));
        assert!(out.contains("- application"));
        // the unrelated window + its items are gone
        assert!(!out.contains("Package Center"));
        assert!(!out.contains("some package"));
    }

    #[test]
    fn test_filter_tree_case_insensitive_and_no_match() {
        let tree = "- button \"Apply\" [ref=e1]";
        assert!(filter_tree(tree, "apply").unwrap().contains("[ref=e1]"));
        assert_eq!(
            filter_tree(tree, "nonexistent").unwrap(),
            "(no elements match /nonexistent/)"
        );
    }

    #[test]
    fn test_filter_tree_bad_regex_errors() {
        assert!(filter_tree("- x", "[unclosed").is_err());
    }

    #[test]
    fn test_count_indent() {
        assert_eq!(count_indent("- heading"), 0);
        assert_eq!(count_indent("  - link"), 1);
        assert_eq!(count_indent("    - text"), 2);
    }

    #[test]
    fn test_role_name_tracker() {
        let mut tracker = RoleNameTracker::new();
        assert_eq!(tracker.track("button", "Submit", 0), 0);
        assert_eq!(tracker.track("button", "Submit", 1), 1);
        assert_eq!(tracker.track("button", "Cancel", 2), 0);

        let dups = tracker.get_duplicates();
        assert!(dups.contains_key("button:Submit"));
        assert!(!dups.contains_key("button:Cancel"));
    }

    // -----------------------------------------------------------------------
    // Cursor-interactive text dedup (Issue #841 regression guard)
    // -----------------------------------------------------------------------

    #[test]
    fn test_dedup_set_from_ref_map_names() {
        let mut ref_map = RefMap::new();
        ref_map.add("e1".to_string(), Some(1), "link", "Example Link", None);
        ref_map.add("e2".to_string(), Some(2), "button", "Submit", None);

        let set = build_dedup_set(&ref_map);
        assert!(set.contains("example link"));
        assert!(set.contains("submit"));
        assert!(!set.contains("other text"));
    }

    #[test]
    fn test_dedup_set_case_insensitive() {
        let mut ref_map = RefMap::new();
        ref_map.add("e1".to_string(), Some(1), "button", "Submit Form", None);

        let set = build_dedup_set(&ref_map);
        assert!(set.contains("submit form"));
        assert!(!set.contains("Submit Form"));
    }

    #[test]
    fn test_dedup_set_empty_inputs() {
        let ref_map = RefMap::new();
        let set = build_dedup_set(&ref_map);
        assert!(set.is_empty());
    }

    #[test]
    fn test_dedup_set_skips_empty_names() {
        let mut ref_map = RefMap::new();
        ref_map.add("e1".to_string(), Some(1), "generic", "", None);
        ref_map.add("e2".to_string(), Some(2), "button", "OK", None);

        let set = build_dedup_set(&ref_map);
        assert_eq!(set.len(), 1);
        assert!(set.contains("ok"));
    }

    // -----------------------------------------------------------------------
    // resolve_ax_session tests (Issue #925 regression guard)
    // Cross-origin iframes must use a dedicated session without frameId.
    // Same-origin iframes must use the parent session with frameId.
    // -----------------------------------------------------------------------

    #[test]
    fn test_cross_origin_iframe_uses_dedicated_session() {
        let parent_session = "parent-session";
        let iframe_frame_id = "cross-origin-iframe-frame";
        let iframe_session = "cross-origin-iframe-session";

        let mut iframe_sessions = HashMap::new();
        iframe_sessions.insert(iframe_frame_id.to_string(), iframe_session.to_string());

        let (params, session) =
            resolve_ax_session(Some(iframe_frame_id), parent_session, &iframe_sessions);

        assert_eq!(session, iframe_session);
        assert_eq!(params, serde_json::json!({}));
    }

    #[test]
    fn test_same_origin_iframe_uses_parent_session_with_frame_id() {
        let parent_session = "parent-session";
        let iframe_frame_id = "same-origin-iframe-frame";
        let iframe_sessions = HashMap::new();

        let (params, session) =
            resolve_ax_session(Some(iframe_frame_id), parent_session, &iframe_sessions);

        assert_eq!(session, parent_session);
        assert_eq!(params, serde_json::json!({ "frameId": iframe_frame_id }));
    }

    #[test]
    fn test_main_frame_uses_parent_session() {
        let parent_session = "parent-session";
        let iframe_sessions = HashMap::new();

        let (params, session) = resolve_ax_session(None, parent_session, &iframe_sessions);

        assert_eq!(session, parent_session);
        assert_eq!(params, serde_json::json!({}));
    }

    // -----------------------------------------------------------------------
    // promote_hidden_inputs
    // -----------------------------------------------------------------------

    fn make_node(role: &str, name: &str, backend_node_id: Option<i64>) -> TreeNode {
        let mut node = TreeNode::empty();
        node.role = role.to_string();
        node.name = name.to_string();
        node.backend_node_id = backend_node_id;
        node
    }

    #[test]
    fn iframe_ref_marker_accepts_metadata_without_matching_longer_ids() {
        let output = "- Iframe [ref=e20]\n- Iframe [ref=e2, context=\"Frame\"]\n";
        assert_eq!(find_snapshot_ref(output, "e2"), output.find("[ref=e2,"));
        assert_eq!(find_snapshot_ref("- Iframe [ref=e2]", "e2"), Some(9));
        assert_eq!(find_snapshot_ref("- Iframe [ref=e20]", "e2"), None);
    }

    #[test]
    fn interactive_status_receipt_survives_compaction() {
        let mut nodes = vec![
            make_node("status", "", None),
            make_node("StaticText", "Cart: folder", None),
            make_node("button", "Undo", Some(7)),
        ];
        nodes[0].children = vec![1, 2];
        nodes[2].has_ref = true;
        nodes[2].ref_id = Some("e7".to_string());
        let options = SnapshotOptions {
            interactive: true,
            ..Default::default()
        };
        let mut output = String::new();
        render_tree(&nodes, 0, 0, &mut output, &options);
        let compact = compact_tree(&output, true);
        assert!(compact.contains("- status: \"Cart: folder\""));
        assert!(compact.contains("button \"Undo\" [ref=e7]"));
        nodes[0].has_ref = true;
        nodes[0].ref_id = Some("e8".to_string());
        let mut clickable = String::new();
        render_tree(&nodes, 0, 0, &mut clickable, &options);
        assert!(clickable.contains("status [ref=e8]"));
        assert!(clickable.contains("Cart: folder"));
        nodes[1].name = "字".repeat(600);
        let summary = status_summary(&nodes, 0);
        assert!(summary.len() <= 524 && summary.ends_with(" [truncated]"));
    }

    /// The real shape from #281: a card grid where every card header is just a
    /// title and one button. `control_context` vouches for none of these
    /// containers, so three cards rendered three identical `button "进入"`.
    #[test]
    fn duplicate_button_names_get_the_text_that_separates_them() {
        let mut nodes = vec![
            make_node("RootWebArea", "", None),
            make_node("generic", "", Some(0)), // grid
            make_node("generic", "", Some(1)), // card header A
            make_node("StaticText", "飞行棋", Some(2)),
            make_node("button", "进入", Some(2)),
            make_node("generic", "", Some(1)), // card header B
            make_node("StaticText", "H5 Games", Some(5)),
            make_node("button", "进入", Some(5)),
        ];
        nodes[0].children = vec![1];
        nodes[1].children = vec![2, 5];
        nodes[2].children = vec![3, 4];
        nodes[5].children = vec![6, 7];
        nodes[4].ref_id = Some("e1".to_string());
        nodes[7].ref_id = Some("e2".to_string());

        assert_eq!(
            control_context(&nodes, 4),
            None,
            "the plain shape is unchanged"
        );
        assert_eq!(disambiguating_context(&nodes, 4).as_deref(), Some("飞行棋"));
        assert_eq!(
            disambiguating_context(&nodes, 7).as_deref(),
            Some("H5 Games")
        );
    }

    /// A unique name needs no help — adding context there is pure noise, which
    /// is why this is gated on duplication rather than applied everywhere.
    #[test]
    fn a_unique_name_gets_no_disambiguating_context() {
        let mut nodes = vec![
            make_node("RootWebArea", "", None),
            make_node("generic", "", Some(0)),
            make_node("StaticText", "飞行棋", Some(1)),
            make_node("button", "进入", Some(1)),
        ];
        nodes[0].children = vec![1];
        nodes[1].children = vec![2, 3];
        nodes[3].ref_id = Some("e1".to_string());
        assert_eq!(disambiguating_context(&nodes, 3), None);
    }

    /// Context that does not separate them is worse than none: it makes each
    /// line longer while leaving the reader exactly as stuck.
    #[test]
    fn context_that_does_not_distinguish_is_dropped() {
        let mut nodes = vec![
            make_node("RootWebArea", "", None),
            make_node("generic", "", Some(0)),
            make_node("generic", "", Some(1)),
            make_node("StaticText", "操作", Some(2)),
            make_node("button", "删除", Some(2)),
            make_node("generic", "", Some(1)),
            make_node("StaticText", "操作", Some(5)),
            make_node("button", "删除", Some(5)),
        ];
        nodes[0].children = vec![1];
        nodes[1].children = vec![2, 5];
        nodes[2].children = vec![3, 4];
        nodes[5].children = vec![6, 7];
        nodes[4].ref_id = Some("e1".to_string());
        nodes[7].ref_id = Some("e2".to_string());
        assert_eq!(disambiguating_context(&nodes, 4), None);
        assert_eq!(disambiguating_context(&nodes, 7), None);
    }

    /// An ancestor holding several controls describes all of them equally, so
    /// its text is not this control's label — climb no further.
    #[test]
    fn a_container_with_several_controls_is_not_a_label() {
        let mut nodes = vec![
            make_node("RootWebArea", "", None),
            make_node("generic", "", Some(0)),
            make_node("StaticText", "工具栏", Some(1)),
            make_node("button", "保存", Some(1)),
            make_node("button", "保存", Some(1)),
        ];
        nodes[0].children = vec![1];
        nodes[1].children = vec![2, 3, 4];
        nodes[3].ref_id = Some("e1".to_string());
        nodes[4].ref_id = Some("e2".to_string());
        assert_eq!(disambiguating_context(&nodes, 3), None);
    }

    #[test]
    fn control_context_keeps_price_in_its_own_card() {
        let mut nodes = vec![
            make_node("RootWebArea", "", None),
            make_node("article", "", None),
            make_node("heading", "Folder", None),
            make_node("StaticText", "Price: $9.00", None),
            make_node("button", "Add to cart", Some(1)),
            make_node("article", "", None),
            make_node("StaticText", "Price: $4.00", None),
        ];
        nodes[0].children = vec![1, 5];
        nodes[1].parent_idx = Some(0);
        nodes[1].children = vec![2, 3, 4];
        nodes[4].parent_idx = Some(1);
        nodes[5].children = vec![6];
        assert_eq!(
            control_context(&nodes, 4).as_deref(),
            Some("Folder | Price: $9.00")
        );
        assert_eq!(nodes[4].name, "Add to cart");
        // A page-wide wrapper cannot combine two independent cards.
        nodes[4].parent_idx = Some(0);
        assert_eq!(control_context(&nodes, 4), None);
    }

    #[test]
    fn control_context_rejects_ambiguous_generic_groups() {
        let mut nodes = vec![
            make_node("generic", "", None),
            make_node("heading", "Folder", None),
            make_node("StaticText", "$9.00", None),
            make_node("button", "Buy", Some(1)),
            make_node("heading", "Notebook", None),
        ];
        nodes[0].children = vec![1, 2, 3];
        nodes[3].parent_idx = Some(0);
        assert_eq!(
            control_context(&nodes, 3).as_deref(),
            Some("Folder | $9.00")
        );
        // Ignored AX containers retain structural children but have no role.
        nodes[0].role.clear();
        assert_eq!(
            control_context(&nodes, 3).as_deref(),
            Some("Folder | $9.00")
        );
        nodes[2].name = "Description ".repeat(100);
        nodes.push(make_node("StaticText", "Price: $9.00", None));
        nodes[0].children.push(5);
        assert!(control_context(&nodes, 3).unwrap().contains("Price: $9.00"));
        nodes[0].children.push(4);
        assert_eq!(control_context(&nodes, 3), None);
    }

    #[test]
    fn control_context_linked_card_keeps_one_product() {
        let mut nodes = vec![
            make_node("generic", "", None),
            make_node("link", "Folder", None),
            make_node("link", "Folder", None),
            make_node("StaticText", "$9.00", None),
            make_node("button", "Add", Some(1)),
        ];
        nodes[0].children = vec![1, 2, 3, 4];
        nodes[4].parent_idx = Some(0);
        assert_eq!(
            control_context(&nodes, 4).as_deref(),
            Some("Folder | $9.00")
        );
        nodes[1].parent_idx = Some(0);
        nodes[2].parent_idx = Some(0);
        assert_eq!(control_context(&nodes, 1), None);
        assert_eq!(control_context(&nodes, 2), None);
        nodes[2].name = "Notebook".to_string();
        assert_eq!(control_context(&nodes, 4), None);
        nodes[2].name = "Folder".to_string();
        nodes.push(make_node("button", "Another product", Some(2)));
        nodes[0].children.push(5);
        assert_eq!(control_context(&nodes, 4), None);
        // Cursor-discovered controls count too; otherwise a mixed UI with one
        // native button is mistaken for a single-action product card.
        nodes[5].role = "generic".to_string();
        nodes[5].cursor_info = Some(make_cursor_info(None, None, "Custom action"));
        assert_eq!(control_context(&nodes, 4), None);
    }

    #[test]
    fn control_context_keeps_unique_link_details_but_omits_its_own_name() {
        let mut nodes = vec![
            make_node("listitem", "", None),
            make_node("link", "Product", None),
            make_node("StaticText", "$9.00", None),
        ];
        nodes[0].children = vec![1];
        nodes[1].parent_idx = Some(0);
        assert_eq!(control_context(&nodes, 1), None);
        nodes[0].children.push(2);
        assert_eq!(
            control_context(&nodes, 1).as_deref(),
            Some("Product | $9.00")
        );
    }

    #[test]
    fn control_context_bounds_text_and_subtree_work() {
        let mut nodes = vec![
            make_node("article", "", None),
            make_node("StaticText", &"字".repeat(200), None),
            make_node("button", "Buy", Some(1)),
        ];
        nodes[0].children = vec![1, 2];
        nodes[2].parent_idx = Some(0);
        let text = control_context(&nodes, 2).unwrap();
        assert!(text.len() <= 268 && text.ends_with(" [truncated]"));
        nodes[0].children = vec![1; 65];
        assert_eq!(control_context(&nodes, 2), None);
    }

    fn make_cursor_info(
        hidden_kind: Option<HiddenInputKind>,
        hidden_checked: Option<&str>,
        text: &str,
    ) -> CursorElementInfo {
        CursorElementInfo {
            kind: "clickable".to_string(),
            hints: vec!["cursor:pointer".to_string()],
            text: text.to_string(),
            fallback_name: text.to_string(),
            hidden_input_kind: hidden_kind,
            hidden_input_checked: hidden_checked.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_promote_label_with_hidden_radio() {
        let mut nodes = vec![
            make_node("LabelText", "", Some(1)),
            make_node("LabelText", "", Some(2)),
            make_node("button", "Submit", Some(3)),
        ];
        let mut cursor_elements = HashMap::new();
        cursor_elements.insert(
            1,
            make_cursor_info(Some(HiddenInputKind::Radio), Some("false"), "Option A"),
        );
        cursor_elements.insert(
            2,
            make_cursor_info(Some(HiddenInputKind::Radio), Some("true"), "Option B"),
        );

        promote_hidden_inputs(&mut nodes, &cursor_elements);

        assert_eq!(nodes[0].role, "radio");
        assert_eq!(nodes[0].name, "Option A");
        assert_eq!(nodes[0].checked, Some("false".to_string()));
        assert_eq!(nodes[1].role, "radio");
        assert_eq!(nodes[1].name, "Option B");
        assert_eq!(nodes[1].checked, Some("true".to_string()));
        // button should be untouched
        assert_eq!(nodes[2].role, "button");
    }

    #[test]
    fn test_promote_preserves_existing_name() {
        // If AX tree already has a name, don't overwrite with textContent
        let mut nodes = vec![make_node("LabelText", "AX Name", Some(1))];
        let mut cursor_elements = HashMap::new();
        cursor_elements.insert(
            1,
            make_cursor_info(Some(HiddenInputKind::Radio), Some("false"), "Text Content"),
        );

        promote_hidden_inputs(&mut nodes, &cursor_elements);

        assert_eq!(nodes[0].role, "radio");
        assert_eq!(nodes[0].name, "AX Name"); // preserved, not overwritten
    }

    // -----------------------------------------------------------------------
    // Inline validation/error surfacing in `-i` mode (issue #57)
    // -----------------------------------------------------------------------

    fn err(text: &str, backend_node_id: Option<i64>) -> ErrorElement {
        ErrorElement {
            backend_node_id,
            text: text.to_string(),
        }
    }

    #[test]
    fn test_select_error_texts_preserves_order() {
        let elems = vec![
            err("字数已超过 8 个字", Some(10)),
            err("Required", Some(11)),
        ];
        let texts = select_error_texts(&elems, &std::collections::HashSet::new());
        assert_eq!(texts, vec!["字数已超过 8 个字", "Required"]);
    }

    #[test]
    fn test_select_error_texts_dedupes_already_reffed() {
        // An error element whose node already earned a ref (e.g. an alert Chrome
        // surfaced as an AX node, or a clickable error) must not list twice.
        let elems = vec![err("already shown", Some(10)), err("new error", Some(11))];
        let already: std::collections::HashSet<i64> = [10].into_iter().collect();
        let texts = select_error_texts(&elems, &already);
        assert_eq!(texts, vec!["new error"]);
    }

    #[test]
    fn test_select_error_texts_dedupes_identical_text() {
        // Two distinct nodes rendering the same message collapse to one line.
        let elems = vec![err("Required", Some(10)), err("Required", Some(11))];
        let texts = select_error_texts(&elems, &std::collections::HashSet::new());
        assert_eq!(texts, vec!["Required"]);
    }

    #[test]
    fn test_select_error_texts_keeps_messages_without_backend_id() {
        // A message we couldn't resolve a backendNodeId for is still surfaced
        // (best-effort: showing the text beats going blind).
        let elems = vec![err("no backend", None)];
        let texts = select_error_texts(&elems, &std::collections::HashSet::new());
        assert_eq!(texts, vec!["no backend"]);
    }

    #[test]
    fn test_render_error_line_format() {
        let line = render_error_line("字数已超过 8 个字");
        assert_eq!(line, "- alert \"字数已超过 8 个字\"");
        // alert lines must read as interactive so compaction never drops them.
        assert!(is_interactive_line(&line));
    }

    #[test]
    fn test_render_error_line_strips_invisible_chars() {
        let line = render_error_line("error\u{200B}text");
        assert_eq!(line, "- alert \"errortext\"");
    }

    #[test]
    fn test_render_error_line_escapes_quotes() {
        let line = render_error_line("say \"hi\"");
        assert_eq!(line, "- alert \"say \\\"hi\\\"\"");
    }

    #[test]
    fn test_promote_skips_without_hidden_input() {
        // Cursor-interactive label WITHOUT a hidden input should not be promoted
        let mut nodes = vec![make_node("LabelText", "", Some(1))];
        let mut cursor_elements = HashMap::new();
        cursor_elements.insert(1, make_cursor_info(None, None, "Click me"));

        promote_hidden_inputs(&mut nodes, &cursor_elements);

        assert_eq!(nodes[0].role, "LabelText"); // unchanged
    }
    /// `<summary>` is the one control that exists purely to be clicked; leaving
    /// it out of the interactive set meant a disclosure never got a ref.
    #[test]
    fn a_disclosure_triangle_is_interactive() {
        assert!(is_interactive_role("DisclosureTriangle"));
    }

    /// Measured: Chrome sends `pressed` as the JSON string "true"/"false", not
    /// a bool. Parsing it as a bool would silently drop every toggle button.
    #[test]
    fn pressed_is_parsed_from_chromes_string_form() {
        let props = Some(vec![AXProperty {
            name: "pressed".into(),
            value: AXValue {
                value_type: "booleanOrUndefined".into(),
                value: Some(Value::String("true".into())),
            },
        }]);
        assert_eq!(action_facts_from_properties(&props).pressed, Some(true));
    }

    /// A range needs both ends: `valuemin` alone (Chrome reports it on controls
    /// with no upper bound) is not something you can step through.
    #[test]
    fn a_value_range_needs_both_ends() {
        let one = Some(vec![AXProperty {
            name: "valuemin".into(),
            value: AXValue {
                value_type: "number".into(),
                value: Some(serde_json::json!(0)),
            },
        }]);
        assert!(!action_facts_from_properties(&one).has_value_range);
    }

    fn facts() -> AxActionFacts {
        AxActionFacts::default()
    }

    /// Measured on Chrome: `aria-haspopup="menu"` on a collapsed button reports
    /// both `hasPopup: "menu"` and `expanded: false`.
    #[test]
    fn menu_button_offers_expand_and_show_menu() {
        let f = AxActionFacts {
            expanded: Some(false),
            has_popup: Some("menu".into()),
            ..facts()
        };
        assert_eq!(
            secondary_actions(&f),
            vec![SecondaryAction::Expand, SecondaryAction::ShowMenu]
        );
    }

    /// An already-open disclosure offers the opposite action, not the same one.
    #[test]
    fn an_expanded_element_offers_collapse() {
        let f = AxActionFacts {
            expanded: Some(true),
            ..facts()
        };
        assert_eq!(secondary_actions(&f), vec![SecondaryAction::Collapse]);
    }

    /// Absent and false are different: a control with no `expanded` property
    /// does not expand at all, and must not be offered `expand`.
    #[test]
    fn an_element_without_the_property_is_not_expandable() {
        assert!(secondary_actions(&facts()).is_empty());
    }

    /// Measured: `<input type=number min max>` reports valuemin/valuemax.
    #[test]
    fn a_value_range_offers_increment_and_decrement() {
        let f = AxActionFacts {
            has_value_range: true,
            ..facts()
        };
        assert_eq!(
            secondary_actions(&f),
            vec![SecondaryAction::Increment, SecondaryAction::Decrement]
        );
    }

    /// A readonly control has a range but cannot be moved through it.
    #[test]
    fn a_readonly_range_is_not_incrementable() {
        let f = AxActionFacts {
            has_value_range: true,
            readonly: true,
            ..facts()
        };
        assert!(secondary_actions(&f).is_empty());
    }

    /// Measured: `aria-pressed` reports "true"/"false" — either way the control
    /// toggles.
    #[test]
    fn a_pressed_property_offers_toggle_in_both_states() {
        for state in [true, false] {
            let f = AxActionFacts {
                pressed: Some(state),
                ..facts()
            };
            assert_eq!(secondary_actions(&f), vec![SecondaryAction::Toggle]);
        }
    }

    /// Chrome reports `hasPopup: "false"` on some builds to mean "no popup".
    /// Treating that string as a popup kind would offer `showMenu` on ordinary
    /// buttons.
    #[test]
    fn has_popup_false_is_not_a_popup() {
        for v in ["false", ""] {
            let f = AxActionFacts {
                has_popup: Some(v.into()),
                ..facts()
            };
            assert!(secondary_actions(&f).is_empty(), "hasPopup={v:?}");
        }
    }

    /// Offering actions on a disabled control invites a caller to spend a round
    /// trip discovering it cannot be operated.
    #[test]
    fn a_disabled_element_offers_nothing() {
        let f = AxActionFacts {
            disabled: true,
            expanded: Some(false),
            has_popup: Some("menu".into()),
            has_value_range: true,
            pressed: Some(false),
            ..facts()
        };
        assert!(secondary_actions(&f).is_empty());
    }

    /// A budget must cut between nodes: a severed line would leave a dangling
    /// `[ref=eN]` the caller cannot use, and no way to tell it was severed.
    #[test]
    fn budget_tree_cuts_on_node_boundaries() {
        let tree = "- a [ref=e1]\n- b [ref=e2]\n- c [ref=e3]\n";
        let b = budget_tree(tree, 20, 0);
        assert!(b.truncated());
        assert!(b.tree.lines().all(|l| l.ends_with(']')));
        assert_eq!(b.total_nodes, 3);
        assert_eq!(b.from, 0);
    }

    /// A tree that fits is not reported as truncated — otherwise every caller
    /// passing a budget would be told to page through a complete page.
    #[test]
    fn budget_tree_untruncated_when_it_fits() {
        let tree = "- a\n- b\n";
        let b = budget_tree(tree, 10_000, 0);
        assert!(!b.truncated());
        assert_eq!(b.next, b.total_nodes);
        assert_eq!(b.tree, "- a\n- b");
    }

    /// `--from` must resume exactly where the previous page stopped, with no
    /// node repeated and none skipped.
    #[test]
    fn budget_tree_from_cursor_covers_every_node_once() {
        let tree = (0..50)
            .map(|i| format!("- node{i} [ref=e{i}]"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut seen: Vec<String> = Vec::new();
        let mut from = 0;
        loop {
            let b = budget_tree(&tree, 64, from);
            seen.extend(b.tree.lines().map(String::from));
            if b.next >= b.total_nodes {
                break;
            }
            assert!(b.next > from, "cursor must advance");
            from = b.next;
        }
        assert_eq!(seen, tree.lines().collect::<Vec<_>>());
    }

    /// A single node bigger than the whole budget still comes back. Returning
    /// nothing would read as an empty page.
    #[test]
    fn budget_tree_always_returns_at_least_one_node() {
        let tree = "- an extremely long node label that alone blows the budget\n- b";
        let b = budget_tree(tree, 5, 0);
        assert_eq!(b.tree.lines().count(), 1);
        assert!(b.truncated());
        assert_eq!(b.next, 1);
    }
}
