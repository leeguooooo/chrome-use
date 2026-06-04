//! Adaptive @ref relocation.
//!
//! When a saved `@ref`'s DOM node is gone (stale `backendNodeId`) and the
//! role/name/nth re-query also fails, we score the current page's candidate
//! elements against the ref's stored [`ElementFingerprint`] and relocate to the
//! best match — but ONLY when confident: the best candidate must clear a high
//! absolute threshold AND beat the runner-up by a clear margin. This matches the
//! project's "fail loudly rather than mis-click" posture (see the identity and
//! occlusion guards in `element.rs`).
//!
//! Everything in this module is pure and browser-free so the scoring can be
//! unit-tested directly.

use std::collections::BTreeMap;

/// Minimum absolute similarity (0..1) for a relocation candidate to be accepted.
pub const ADAPTIVE_THRESHOLD: f64 = 0.70;
/// Minimum gap between the best and second-best candidate to avoid ambiguity.
pub const ADAPTIVE_MARGIN: f64 = 0.15;

/// A structural/semantic fingerprint of an element, captured at snapshot time so
/// a moved element can be re-identified after the page mutates.
///
/// Populated purely from the accessibility tree we already walk (`TreeNode`), so
/// capturing it costs no extra CDP round-trips — `TreeNode` has no DOM tag or
/// attributes (those would need an N×`DOM.describeNode` storm per snapshot), so
/// `tag` holds the AX **role** and `attrs` holds discriminating AX properties
/// (value/url/level/checked), not DOM `id`/`class`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ElementFingerprint {
    /// AX role, e.g. "button" (used where a DOM tag would otherwise go).
    pub tag: String,
    /// Accessible name / visible text — the dominant identity signal.
    pub text: String,
    /// Discriminating AX properties: value, url, level, checked. Keyed by name.
    pub attrs: BTreeMap<String, String>,
    /// Ancestor role signatures from nearest to farthest, e.g. "form" / "list".
    pub ancestors: Vec<String>,
    /// Parent role.
    pub parent_tag: String,
    /// Parent accessible name / text.
    pub parent_text: String,
    /// Index among same-role siblings.
    pub sibling_index: u32,
    /// Count of same-role siblings.
    pub sibling_count: u32,
}

/// Component weights. They sum to 1.0 so the total score lands in 0..1.
/// Tuned for AX-derived fingerprints: the accessible name dominates, with role
/// and tree structure carrying disambiguation when the name has changed (which
/// is exactly when the exact role+name+nth fallback failed and we got here).
const W_TAG: f64 = 0.20;
const W_TEXT: f64 = 0.40;
const W_ATTRS: f64 = 0.10;
const W_ANCESTORS: f64 = 0.20;
const W_PARENT_SIBLING: f64 = 0.10;

/// Per-attribute importance for the attribute-overlap score. Strong identity
/// signals (a link's url) outweigh weak ones (heading level).
fn attr_weight(name: &str) -> f64 {
    match name {
        "url" | "value" => 3.0,
        "checked" => 2.0,
        _ => 1.0,
    }
}

/// Levenshtein-based string similarity in 0..1 (1.0 = identical). Two empty
/// strings are treated as a perfect match (consistent absence of text).
pub fn string_similarity(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let max_len = a.len().max(b.len());
    if max_len == 0 {
        return 1.0;
    }
    let dist = levenshtein(&a, &b);
    1.0 - (dist as f64 / max_len as f64)
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Jaccard similarity over whitespace-separated tokens (used for `class`).
fn token_jaccard(a: &str, b: &str) -> f64 {
    let sa: std::collections::BTreeSet<&str> = a.split_whitespace().collect();
    let sb: std::collections::BTreeSet<&str> = b.split_whitespace().collect();
    if sa.is_empty() && sb.is_empty() {
        return 1.0;
    }
    let inter = sa.intersection(&sb).count() as f64;
    let union = sa.union(&sb).count() as f64;
    if union == 0.0 {
        1.0
    } else {
        inter / union
    }
}

/// Length-ratio of the longest common subsequence over two ancestor sequences.
fn lcs_ratio(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut dp = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in 0..a.len() {
        for j in 0..b.len() {
            dp[i + 1][j + 1] = if a[i] == b[j] {
                dp[i][j] + 1
            } else {
                dp[i][j + 1].max(dp[i + 1][j])
            };
        }
    }
    let lcs = dp[a.len()][b.len()] as f64;
    (2.0 * lcs) / (a.len() + b.len()) as f64
}

fn attr_score(base: &BTreeMap<String, String>, cand: &BTreeMap<String, String>) -> f64 {
    let mut names: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    names.extend(base.keys().map(|s| s.as_str()));
    names.extend(cand.keys().map(|s| s.as_str()));
    if names.is_empty() {
        return 1.0; // no attributes on either side — neutral
    }
    let mut total = 0.0;
    let mut got = 0.0;
    for name in names {
        let w = attr_weight(name);
        total += w;
        // present on only one side → no credit
        if let (Some(a), Some(b)) = (base.get(name), cand.get(name)) {
            if name == "class" {
                got += w * token_jaccard(a, b);
            } else if a == b {
                got += w;
            }
        }
    }
    if total == 0.0 {
        1.0
    } else {
        got / total
    }
}

fn parent_sibling_score(base: &ElementFingerprint, cand: &ElementFingerprint) -> f64 {
    // Split the 0.10 budget: parent tag 0.4, parent text 0.3, sibling pos 0.3.
    let parent_tag = if base.parent_tag == cand.parent_tag {
        1.0
    } else {
        0.0
    };
    let parent_text = string_similarity(&base.parent_text, &cand.parent_text);
    let span = base.sibling_count.max(1) as f64;
    let delta = (base.sibling_index as i64 - cand.sibling_index as i64).unsigned_abs() as f64;
    let sibling = 1.0 - (delta / span).min(1.0);
    0.4 * parent_tag + 0.3 * parent_text + 0.3 * sibling
}

/// Similarity score in 0..1 between a stored baseline and a candidate element.
pub fn score(base: &ElementFingerprint, cand: &ElementFingerprint) -> f64 {
    let tag = if base.tag == cand.tag { 1.0 } else { 0.0 };
    let text = string_similarity(&base.text, &cand.text);
    let attrs = attr_score(&base.attrs, &cand.attrs);
    let ancestors = lcs_ratio(&base.ancestors, &cand.ancestors);
    let parent_sibling = parent_sibling_score(base, cand);

    W_TAG * tag
        + W_TEXT * text
        + W_ATTRS * attrs
        + W_ANCESTORS * ancestors
        + W_PARENT_SIBLING * parent_sibling
}

/// Why a relocation was rejected.
#[derive(Debug, Clone, PartialEq)]
pub enum RejectReason {
    /// No candidates to score.
    NoCandidates,
    /// Best score below [`ADAPTIVE_THRESHOLD`].
    LowScore { best: f64 },
    /// Best score too close to the runner-up (below [`ADAPTIVE_MARGIN`]).
    Ambiguous { best: f64, second: f64 },
}

/// A successful relocation decision.
#[derive(Debug, Clone, PartialEq)]
pub struct Relocation {
    /// Chosen candidate's backend node id.
    pub backend_node_id: i64,
    /// Winning score.
    pub score: f64,
    /// Runner-up score (0.0 when there was only one candidate).
    pub second_score: f64,
}

/// Pick the best candidate, accepting only when confident. `candidates` is a
/// list of `(backend_node_id, fingerprint)` for the current page.
pub fn pick_best(
    base: &ElementFingerprint,
    candidates: &[(i64, ElementFingerprint)],
    threshold: f64,
    margin: f64,
) -> Result<Relocation, RejectReason> {
    if candidates.is_empty() {
        return Err(RejectReason::NoCandidates);
    }
    let mut scored: Vec<(i64, f64)> = candidates
        .iter()
        .map(|(id, fp)| (*id, score(base, fp)))
        .collect();
    // Highest score first; stable enough for deterministic ties.
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let (best_id, best) = scored[0];
    let second = scored.get(1).map(|(_, s)| *s).unwrap_or(0.0);

    if best < threshold {
        return Err(RejectReason::LowScore { best });
    }
    if best - second < margin {
        return Err(RejectReason::Ambiguous { best, second });
    }
    Ok(Relocation {
        backend_node_id: best_id,
        score: best,
        second_score: second,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(tag: &str, text: &str, attrs: &[(&str, &str)]) -> ElementFingerprint {
        ElementFingerprint {
            tag: tag.to_string(),
            text: text.to_string(),
            attrs: attrs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn identical_fingerprints_score_one() {
        let a = fp("button", "Submit", &[("id", "go"), ("class", "btn primary")]);
        assert!((score(&a, &a) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn different_tag_caps_score_below_threshold() {
        let a = fp("button", "Submit", &[("id", "go")]);
        let b = fp("a", "Submit", &[("id", "go")]);
        // Same text + same attrs but different role: must lose the role weight
        // (W_TAG = 0.20), landing around 0.80 and below a perfect match.
        let s = score(&a, &b);
        assert!(s < 0.85 && s > 0.75, "got {s}");
    }

    #[test]
    fn string_similarity_basics() {
        assert_eq!(string_similarity("abc", "abc"), 1.0);
        assert_eq!(string_similarity("", ""), 1.0);
        assert!(string_similarity("Submit", "Submit now") > 0.5);
        assert!(string_similarity("Add post", "Post all") < 0.6);
    }

    #[test]
    fn class_uses_token_overlap() {
        let a = fp("div", "", &[("class", "card primary big")]);
        let b = fp("div", "", &[("class", "card primary")]);
        // partial class overlap should still score high (tag+text match, attrs partial)
        let s = score(&a, &b);
        assert!(s > 0.85, "got {s}");
    }

    #[test]
    fn ancestors_lcs() {
        let mut a = fp("button", "OK", &[]);
        let mut b = fp("button", "OK", &[]);
        a.ancestors = vec!["form#f".into(), "div.col".into(), "body".into()];
        // b wrapped in an extra div — DOM path changed but mostly preserved
        b.ancestors = vec!["form#f".into(), "div.wrap".into(), "div.col".into(), "body".into()];
        let s = score(&a, &b);
        assert!(s > 0.85, "got {s}");
    }

    #[test]
    fn pick_best_accepts_clear_winner() {
        let base = fp("button", "Submit", &[("id", "go")]);
        let winner = fp("button", "Submit", &[("id", "go")]);
        let other = fp("a", "Home", &[("href", "/")]);
        let out = pick_best(
            &base,
            &[(10, other), (20, winner)],
            ADAPTIVE_THRESHOLD,
            ADAPTIVE_MARGIN,
        )
        .expect("should accept");
        assert_eq!(out.backend_node_id, 20);
        assert!(out.score > out.second_score);
    }

    #[test]
    fn pick_best_rejects_ambiguous_twins() {
        let base = fp("button", "Delete", &[("class", "btn danger")]);
        // Two near-identical delete buttons — must refuse to guess.
        let twin_a = fp("button", "Delete", &[("class", "btn danger")]);
        let twin_b = fp("button", "Delete", &[("class", "btn danger")]);
        let err = pick_best(
            &base,
            &[(1, twin_a), (2, twin_b)],
            ADAPTIVE_THRESHOLD,
            ADAPTIVE_MARGIN,
        )
        .unwrap_err();
        assert!(matches!(err, RejectReason::Ambiguous { .. }), "got {err:?}");
    }

    #[test]
    fn pick_best_rejects_low_score() {
        let base = fp("button", "Submit order", &[("id", "checkout")]);
        let junk = fp("span", "unrelated footer text", &[("class", "muted")]);
        let err = pick_best(&base, &[(1, junk)], ADAPTIVE_THRESHOLD, ADAPTIVE_MARGIN).unwrap_err();
        assert!(matches!(err, RejectReason::LowScore { .. }), "got {err:?}");
    }

    #[test]
    fn pick_best_no_candidates() {
        let base = fp("button", "x", &[]);
        assert_eq!(
            pick_best(&base, &[], ADAPTIVE_THRESHOLD, ADAPTIVE_MARGIN).unwrap_err(),
            RejectReason::NoCandidates
        );
    }
}
