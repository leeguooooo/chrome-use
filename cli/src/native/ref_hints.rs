//! Honest `@ref` resolution: say when a ref moved, and say where to look when
//! it cannot be resolved.
//!
//! `@ref`s stay stable across snapshots, and a ref whose node was replaced is
//! re-found by role + name (+ nth), by stable DOM attributes, or by adaptive
//! fingerprint scoring (`adaptive.rs`). A replacement is acted on only when it
//! is the same control — same role, same normalised name — and then the
//! command's response carries a `relocated` entry naming the ref, how it was
//! re-found and what it landed on: a relocation is never silent.
//!
//! When a ref cannot be resolved — including a confident match whose name
//! changed — the error offers that match (as a freshly minted ref) and up to
//! three refs from the current snapshot whose role + name are closest. They
//! are suggestions only: nothing ever acts on one.
//!
//! The ranking and message formatting here are pure so they can be unit-tested.

use std::cell::RefCell;
use std::future::Future;

use serde_json::{json, Value};

use super::adaptive;

/// How a ref was re-found on a node other than the one the snapshot recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocationHow {
    /// Re-query of the accessibility tree by the ref's exact role + name (+ nth).
    RoleName,
    /// The detached node's stable DOM attributes (`id`, `name`, `data-testid`…)
    /// matched exactly one live replacement.
    DomIdentity,
    /// Fingerprint similarity scoring (`adaptive.rs`).
    Adaptive,
}

impl RelocationHow {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RoleName => "role-name",
            Self::DomIdentity => "dom-identity",
            Self::Adaptive => "adaptive",
        }
    }
}

/// One ref that resolved to a different node than the snapshot recorded.
#[derive(Debug, Clone, PartialEq)]
pub struct RefRelocation {
    /// Ref id without the `@`, e.g. `e5`.
    pub ref_id: String,
    pub how: RelocationHow,
    /// Similarity score, for [`RelocationHow::Adaptive`] only.
    pub score: Option<f64>,
    /// Role + name the snapshot recorded for the ref.
    pub was_role: String,
    pub was_name: String,
    /// Role + name of the node the command acted on.
    pub role: String,
    pub name: String,
}

impl RefRelocation {
    pub fn to_json(&self) -> Value {
        let mut v = json!({
            "ref": format!("@{}", self.ref_id),
            "how": self.how.as_str(),
            "role": self.role,
            "name": self.name,
            "was": { "role": self.was_role, "name": self.was_name },
        });
        if let Some(s) = self.score {
            v["score"] = json!((s * 100.0).round() / 100.0);
        }
        v
    }
}

/// A ref minted for a live node that is offered as a suggestion but was not
/// in any snapshot — the guess a relocation refused to act on because its
/// name differs. Adopted into the session's ref map once the command ends, so
/// `try @eN` works on the next command.
#[derive(Debug, Clone, PartialEq)]
pub struct MintedRef {
    pub ref_id: String,
    pub backend_node_id: i64,
    pub frame_id: Option<String>,
    pub role: String,
    pub name: String,
    pub fingerprint: Option<adaptive::ElementFingerprint>,
}

/// What resolving `@ref`s did while serving one command.
#[derive(Debug, Default)]
pub struct RefEffects {
    pub relocations: Vec<RefRelocation>,
    pub mints: Vec<MintedRef>,
}

tokio::task_local! {
    static EFFECTS: RefCell<RefEffects>;
}

/// Note a relocation for the running command. A no-op outside
/// [`collect_ref_effects`] (unit tests, background tasks).
pub fn record_relocation(r: RefRelocation) {
    let _ = EFFECTS.try_with(|cell| {
        let list = &mut cell.borrow_mut().relocations;
        // A command can resolve the same ref more than once (centre, then
        // object id; or a recovery retry). Report it once, latest wins.
        list.retain(|x| x.ref_id != r.ref_id);
        list.push(r);
    });
}

/// Give a live node a ref for a suggestion. `known` is the ref the session
/// already holds for this node with this identity, if any; otherwise a new id
/// is taken from `next_free` onwards. `None` outside a command, where nothing
/// could adopt the ref afterwards — the caller then offers no such suggestion.
pub fn mint_ref(
    known: Option<String>,
    next_free: usize,
    backend_node_id: i64,
    frame_id: Option<&str>,
    role: &str,
    name: &str,
    fingerprint: Option<adaptive::ElementFingerprint>,
) -> Option<String> {
    EFFECTS
        .try_with(|cell| {
            let mints = &mut cell.borrow_mut().mints;
            if let Some(m) = mints
                .iter()
                .find(|m| m.backend_node_id == backend_node_id && m.frame_id.as_deref() == frame_id)
            {
                return m.ref_id.clone();
            }
            let ref_id = known.unwrap_or_else(|| {
                let taken = mints
                    .iter()
                    .filter_map(|m| m.ref_id.strip_prefix('e').and_then(|n| n.parse().ok()))
                    .map(|n: usize| n + 1)
                    .max()
                    .unwrap_or(0);
                format!("e{}", next_free.max(taken))
            });
            mints.push(MintedRef {
                ref_id: ref_id.clone(),
                backend_node_id,
                frame_id: frame_id.map(str::to_string),
                role: role.to_string(),
                name: name.to_string(),
                fingerprint,
            });
            ref_id
        })
        .ok()
}

/// Run `f` collecting every relocation and minted suggestion it makes.
pub async fn collect_ref_effects<F: Future>(f: F) -> (F::Output, RefEffects) {
    EFFECTS
        .scope(RefCell::new(RefEffects::default()), async move {
            let out = f.await;
            let effects = EFFECTS.with(|cell| cell.take());
            (out, effects)
        })
        .await
}

/// Put the relocations on the response's `data.relocated` — on a failure too,
/// so "it moved, then the action failed" is not lost.
pub fn attach_relocations(response: &mut Value, relocations: &[RefRelocation]) {
    if relocations.is_empty() {
        return;
    }
    let list = Value::Array(relocations.iter().map(RefRelocation::to_json).collect());
    let Some(obj) = response.as_object_mut() else {
        return;
    };
    let data = obj.entry("data".to_string()).or_insert(Value::Null);
    if data.is_null() {
        *data = json!({});
    }
    if let Some(d) = data.as_object_mut() {
        d.insert("relocated".to_string(), list);
    }
}

/// Accessible names compared the way a reader would: trimmed, inner
/// whitespace collapsed, case-insensitive.
pub fn normalize_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Whether a relocation candidate is the control the ref named: same role,
/// same name after [`normalize_name`]. Only such a candidate is acted on; any
/// other is a guess, offered as a suggestion instead.
pub fn same_identity(want_role: &str, want_name: &str, role: &str, name: &str) -> bool {
    want_role == role && normalize_name(want_name) == normalize_name(name)
}

/// Put the refused best match first, then the ranked refs, without
/// duplicates, capped at [`MAX_SUGGESTIONS`].
pub fn merge_suggestions(
    first: Option<RefSuggestion>,
    rest: Vec<RefSuggestion>,
) -> Vec<RefSuggestion> {
    let mut out: Vec<RefSuggestion> = first.into_iter().collect();
    for s in rest {
        if !out.iter().any(|x| x.ref_id == s.ref_id) {
            out.push(s);
        }
    }
    out.truncate(MAX_SUGGESTIONS);
    out
}

/// A ref from the current snapshot offered in place of one that could not be
/// resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct RefSuggestion {
    pub ref_id: String,
    pub role: String,
    pub name: String,
    pub score: f64,
}

/// How many refs to suggest.
pub const MAX_SUGGESTIONS: usize = 3;
/// Below this a candidate is not "close" — a same-role control needs a name
/// that is at least loosely related, another role needs a near-identical name.
pub const SUGGESTION_FLOOR: f64 = 0.5;
const W_ROLE: f64 = 0.35;
const W_NAME: f64 = 0.65;

/// Name similarity tolerant of a label that grew or shrank: "Save" →
/// "Save changes" is the same control to a reader, but plain edit distance
/// scores it 0.33.
pub fn name_similarity(a: &str, b: &str) -> f64 {
    let a = a.trim().to_lowercase();
    let b = b.trim().to_lowercase();
    let edit = adaptive::string_similarity(&a, &b);
    if !a.is_empty() && !b.is_empty() && (a.contains(&b) || b.contains(&a)) {
        edit.max(0.8)
    } else {
        edit
    }
}

/// Role + name closeness in 0..1.
pub fn suggestion_score(want_role: &str, want_name: &str, role: &str, name: &str) -> f64 {
    let role_score = if want_role == role { 1.0 } else { 0.0 };
    W_ROLE * role_score + W_NAME * name_similarity(want_name, name)
}

/// The up-to-three candidates closest to `want_role` + `want_name`, best
/// first; ties go to the lower ref number. `candidates` are
/// `(ref_id, role, name)`.
pub fn rank_suggestions<'a>(
    want_role: &str,
    want_name: &str,
    candidates: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>,
) -> Vec<RefSuggestion> {
    let mut scored: Vec<RefSuggestion> = candidates
        .into_iter()
        .map(|(ref_id, role, name)| RefSuggestion {
            ref_id: ref_id.to_string(),
            role: role.to_string(),
            name: name.to_string(),
            score: suggestion_score(want_role, want_name, role, name),
        })
        // The name is the identity the agent asked for: a same-role control
        // with an unrelated label is not "close", however alike its role.
        .filter(|s| {
            s.score >= SUGGESTION_FLOOR
                && name_similarity(want_name, &s.name) >= adaptive::ADAPTIVE_MIN_NAME
        })
        .collect();
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| ref_number(&a.ref_id).cmp(&ref_number(&b.ref_id)))
    });
    scored.truncate(MAX_SUGGESTIONS);
    scored
}

fn ref_number(ref_id: &str) -> usize {
    ref_id
        .strip_prefix('e')
        .and_then(|n| n.parse().ok())
        .unwrap_or(usize::MAX)
}

/// The block appended to an unresolvable-ref error. Never empty: with nothing
/// to suggest it still says how to get fresh refs.
pub fn format_suggestions(suggestions: &[RefSuggestion]) -> String {
    if suggestions.is_empty() {
        return "No close match among the current snapshot's refs — run `snapshot -i` to \
                refresh them."
            .to_string();
    }
    let mut out = String::from(
        "Closest refs on the page (suggestions only — nothing was acted on; check one is the \
         control you meant):",
    );
    for s in suggestions {
        out.push_str(&format!(
            "\n  try @{} [{}] \"{}\"",
            s.ref_id, s.role, s.name
        ));
    }
    out.push_str("\nOr run `snapshot -i` to refresh the refs.");
    out
}

/// Insert `block` ahead of `marker` in `err` (the "last resort" escape hatch
/// stays last), or append it when the marker is absent.
pub fn insert_before(err: &str, marker: &str, block: &str) -> String {
    match err.find(marker) {
        Some(i) => format!("{}{}\n{}", &err[..i], block, &err[i..]),
        None => format!("{err}\n{block}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reloc(id: &str, how: RelocationHow, score: Option<f64>) -> RefRelocation {
        RefRelocation {
            ref_id: id.into(),
            how,
            score,
            was_role: "button".into(),
            was_name: "Save".into(),
            role: "button".into(),
            name: "Save now".into(),
        }
    }

    #[test]
    fn a_relocation_serialises_what_it_landed_on() {
        let v = reloc("e5", RelocationHow::Adaptive, Some(0.8234)).to_json();
        assert_eq!(v["ref"], "@e5");
        assert_eq!(v["how"], "adaptive");
        assert_eq!(v["score"], 0.82);
        assert_eq!(v["role"], "button");
        assert_eq!(v["name"], "Save now");
        assert_eq!(v["was"]["name"], "Save");
        let v = reloc("e5", RelocationHow::RoleName, None).to_json();
        assert_eq!(v["how"], "role-name");
        assert!(v.get("score").is_none());
    }

    #[test]
    fn relocations_ride_on_success_and_failure() {
        let mut ok = json!({"id": "1", "success": true, "data": {"clicked": true}});
        attach_relocations(&mut ok, &[reloc("e1", RelocationHow::RoleName, None)]);
        assert_eq!(ok["data"]["clicked"], true);
        assert_eq!(ok["data"]["relocated"][0]["ref"], "@e1");

        let mut null_data = json!({"id": "1", "success": true, "data": null});
        attach_relocations(
            &mut null_data,
            &[reloc("e1", RelocationHow::RoleName, None)],
        );
        assert_eq!(null_data["data"]["relocated"][0]["how"], "role-name");

        let mut err = json!({"id": "1", "success": false, "error": "x"});
        attach_relocations(&mut err, &[reloc("e1", RelocationHow::RoleName, None)]);
        assert_eq!(err["data"]["relocated"][0]["ref"], "@e1");
        assert_eq!(err["error"], "x");

        let mut untouched = json!({"id": "1", "success": true, "data": {}});
        attach_relocations(&mut untouched, &[]);
        assert!(untouched["data"].get("relocated").is_none());
    }

    #[tokio::test]
    async fn the_collector_reports_each_ref_once() {
        let ((), fx) = collect_ref_effects(async {
            record_relocation(reloc("e1", RelocationHow::RoleName, None));
            record_relocation(reloc("e2", RelocationHow::Adaptive, Some(0.9)));
            record_relocation(reloc("e1", RelocationHow::Adaptive, Some(0.75)));
        })
        .await;
        let list = fx.relocations;
        assert_eq!(list.len(), 2);
        assert_eq!(list[1].ref_id, "e1");
        assert_eq!(list[1].how, RelocationHow::Adaptive);
        // Outside a command it is a silent no-op.
        record_relocation(reloc("e9", RelocationHow::RoleName, None));
    }

    #[tokio::test]
    async fn minted_suggestions_take_fresh_ids_once_per_node() {
        let ((a, b, again, known), fx) = collect_ref_effects(async {
            let a = mint_ref(None, 10, 100, None, "button", "Save now", None);
            let b = mint_ref(None, 10, 101, None, "button", "Save all", None);
            let again = mint_ref(None, 10, 100, None, "button", "Save now", None);
            let known = mint_ref(Some("e3".into()), 10, 102, None, "link", "Save", None);
            (a, b, again, known)
        })
        .await;
        assert_eq!(a.as_deref(), Some("e10"));
        assert_eq!(b.as_deref(), Some("e11"));
        assert_eq!(again.as_deref(), Some("e10"), "same node, same ref");
        assert_eq!(
            known.as_deref(),
            Some("e3"),
            "a ref the session holds is reused"
        );
        assert_eq!(fx.mints.len(), 3);
        // Nothing could adopt a ref minted outside a command.
        assert!(mint_ref(None, 10, 100, None, "button", "x", None).is_none());
    }

    #[test]
    fn identity_compares_names_like_a_reader() {
        assert!(same_identity(
            "button",
            " Save  changes ",
            "button",
            "save changes"
        ));
        assert!(!same_identity("button", "Save", "button", "Save now"));
        assert!(!same_identity("button", "Delete", "button", "Delete all"));
        assert!(!same_identity("button", "Save", "link", "Save"));
    }

    #[test]
    fn the_refused_best_match_leads_the_suggestions() {
        let first = RefSuggestion {
            ref_id: "e10".into(),
            role: "button".into(),
            name: "Save now".into(),
            score: 0.8,
        };
        let rest = rank_suggestions(
            "button",
            "Save",
            [
                ("e9", "button", "Save draft"),
                ("e10", "button", "Save now"),
                ("e11", "button", "Save as"),
                ("e12", "button", "Saved"),
            ],
        );
        let ids: Vec<String> = merge_suggestions(Some(first), rest)
            .into_iter()
            .map(|s| s.ref_id)
            .collect();
        assert_eq!(ids, vec!["e10", "e9", "e11"]);
    }

    #[test]
    fn a_grown_label_is_still_close() {
        assert!(name_similarity("Save", "Save changes") >= 0.8);
        assert!(name_similarity("save", "SAVE") == 1.0);
        assert!(name_similarity("Save", "Cancel") < 0.5);
    }

    #[test]
    fn suggestions_rank_by_role_and_name_and_drop_unrelated() {
        let cands = [
            ("e3", "button", "Cancel"),
            ("e14", "button", "Save changes"),
            ("e7", "link", "Save"),
            ("e9", "button", "Save draft"),
            ("e2", "textbox", "Search"),
            ("e11", "button", "Save changes"),
        ];
        let got = rank_suggestions("button", "Save", cands);
        let ids: Vec<&str> = got.iter().map(|s| s.ref_id.as_str()).collect();
        // Same role + a label containing "Save" first (ties go to the lower
        // ref), capped at 3 — so the other-role exact name `e7` is cut, and
        // "Cancel" / "Search" are not close enough to offer at all.
        assert_eq!(ids, vec!["e9", "e11", "e14"]);
        assert!(got.iter().all(|s| s.score >= SUGGESTION_FLOOR));
    }

    #[test]
    fn another_role_needs_a_near_identical_name() {
        let got = rank_suggestions("button", "Save", [("e7", "link", "Save")]);
        assert_eq!(
            got.len(),
            1,
            "same name, other role is still worth offering"
        );
        let got = rank_suggestions("button", "Save", [("e8", "link", "Sales")]);
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn suggestion_block_lists_refs_and_the_refresh_route() {
        let s = rank_suggestions("button", "Save", [("e14", "button", "Save changes")]);
        let block = format_suggestions(&s);
        assert!(
            block.contains("try @e14 [button] \"Save changes\""),
            "{block}"
        );
        assert!(block.contains("snapshot -i"), "{block}");
        assert!(block.contains("nothing was acted on"), "{block}");

        let none = format_suggestions(&[]);
        assert!(none.contains("snapshot -i"), "{none}");
        assert!(!none.contains("try @"), "{none}");
    }

    #[test]
    fn the_block_goes_before_the_last_resort() {
        let err = "Ref e5 gone.\nFix: snapshot.\n(Last resort: disable it.)";
        let out = insert_before(err, "(Last resort:", "BLOCK");
        assert!(out.find("BLOCK").unwrap() < out.find("(Last resort:").unwrap());
        assert_eq!(insert_before("plain", "(Last resort:", "B"), "plain\nB");
    }
}
