//! Adaptive wait before an observation (issue #228).
//!
//! Observing a page is only meaningful once the page has stopped changing, and
//! "how long is that" is not a constant. The two call sites this module
//! replaces both guessed:
//!
//! - `--observe` slept a hard-coded 250ms after the action and captured
//!   whatever was there. When 250ms was not enough it returned a
//!   mid-transition tree **as though it were the result** — a silent wrong
//!   answer, which is worse than being slow.
//! - `snapshot` did not wait at all.
//!
//! The fix is to wait on signals instead of on a number, and to **say so** when
//! the signals never went quiet:
//!
//! - **DOM quiet** — a `MutationObserver` reports no mutation for
//!   [`DEFAULT_QUIET_MS`].
//! - **Animations** — no finite CSS transition/animation still running.
//!   Infinitely-repeating ones (spinners) are ignored on purpose: a spinner
//!   never ends, so waiting for it means always paying the ceiling.
//! - **Network** — no request in flight that started after the action did.
//!   This is the signal a page-side wait cannot see: an XHR fired by the click
//!   leaves the DOM quiet for its whole round trip, and that quiet is exactly
//!   the mid-transition tree the old 250ms returned.
//!
//! Everything is bounded by a ceiling ([`DEFAULT_MAX_MS`]). Hitting it is not
//! silently swallowed: the outcome carries what was still busy, and the callers
//! print it.
//!
//! Waiting is the observer's job, not the caller's. An agent asked to guess a
//! sleep duration guesses conservatively, and that guess is paid on every call.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::actions::DaemonState;

/// How long the page must go without a DOM mutation (or a running animation)
/// before we call it settled.
pub const DEFAULT_QUIET_MS: u64 = 100;

/// Upper bound on a single settle. On expiry we capture anyway and report that
/// the capture may be mid-transition.
pub const DEFAULT_MAX_MS: u64 = 1000;

/// How often the network signal is re-checked while the page itself is quiet.
const NETWORK_POLL_MS: u64 = 25;

/// Time reserved out of each page-side wait for the CDP round trip that carries
/// its answer back, so the whole settle stays within the ceiling.
const ROUND_TRIP_ALLOWANCE_MS: u64 = 100;

/// A request that has been in flight this long stops counting as a settle
/// blocker. Server-sent events, long-polls and streaming responses never
/// "finish"; without this they would hold every observation to the ceiling.
const NETWORK_STALE_MS: u64 = 10_000;

/// Env override for the ceiling, in milliseconds. `0` disables waiting.
pub const ENV_MAX_MS: &str = "AGENT_BROWSER_SETTLE_MS";
/// Env override for the quiet window, in milliseconds.
pub const ENV_QUIET_MS: &str = "AGENT_BROWSER_SETTLE_QUIET_MS";

/// What a settle waited for and whether it got there.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SettleOutcome {
    /// Wall time actually spent waiting.
    pub waited_ms: u64,
    /// True when every signal went quiet within the ceiling.
    pub quiet: bool,
    /// Whether anything moved at all while waiting — a mutation, a request, an
    /// animation. `quiet` with `saw_change: false` after an action is the
    /// honest form of "that did nothing": the wait watched for a reaction and
    /// none came, rather than never having looked.
    ///
    /// Without a [`SettleArm`] it only covers what happened *while waiting*: a
    /// mutation the action made synchronously, before this observer existed,
    /// is invisible here (with an arm it is seen; see [`settle_armed`]). The
    /// caller that has a before/after diff knows better, and `--observe` folds
    /// that in (see `mark_changed`) so the reported flag never contradicts the
    /// delta printed beside it.
    pub saw_change: bool,
    /// Signals still busy when the ceiling hit: `dom`, `animation`, `network`.
    /// Empty when `quiet`.
    pub pending: Vec<String>,
}

impl SettleOutcome {
    /// A settle that was switched off (`--no-settle`, ceiling 0). Reports as
    /// quiet because nothing was asked for — a skipped wait is not a timeout.
    pub fn skipped() -> Self {
        Self {
            waited_ms: 0,
            quiet: true,
            saw_change: false,
            pending: Vec::new(),
        }
    }

    /// The warning to print when the page never went quiet, or `None`.
    ///
    /// This is the whole point of the ceiling being visible: a tree captured
    /// mid-transition looks exactly like a settled one, so the only thing that
    /// separates them is this line.
    pub fn warning(&self) -> Option<String> {
        if self.quiet {
            return None;
        }
        Some(format!(
            "Page had not settled after {}ms ({} still active) — this capture may be \
             mid-transition. Re-read to confirm, or raise the ceiling with {}.",
            self.waited_ms,
            describe_pending(&self.pending),
            ENV_MAX_MS,
        ))
    }

    /// Fold in a change the caller observed that the wait could not: a DOM
    /// mutation made synchronously during dispatch happens before the page-side
    /// observer is installed, so the delta is the authority on whether anything
    /// changed and this flag must not say otherwise.
    pub fn mark_changed(&mut self, changed: bool) {
        self.saw_change |= changed;
    }

    /// JSON shape attached to `--json` output.
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "waitedMs": self.waited_ms,
            "quiet": self.quiet,
            "sawChange": self.saw_change,
            "pending": self.pending,
        })
    }
}

/// Human-readable list of the signals that were still busy.
pub fn describe_pending(pending: &[String]) -> String {
    if pending.is_empty() {
        return "nothing".to_string();
    }
    pending
        .iter()
        .map(|p| match p.as_str() {
            "dom" => "DOM still mutating",
            "animation" => "animation running",
            "network" => "request in flight",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Read a positive-integer millisecond budget from the environment, falling
/// back to `default` when unset or unparseable. `0` is meaningful (disable), so
/// it is preserved rather than treated as "unset".
pub fn env_ms(var: &str, default: u64) -> u64 {
    match std::env::var(var) {
        Ok(s) => s.trim().parse::<u64>().unwrap_or(default),
        Err(_) => default,
    }
}

/// Resolve the ceiling for one command: an explicit per-command value
/// (`--settle-ms`, `--no-settle` → 0) wins over the env override, which wins
/// over [`DEFAULT_MAX_MS`].
pub fn resolve_max_ms(explicit: Option<u64>) -> u64 {
    explicit.unwrap_or_else(|| env_ms(ENV_MAX_MS, DEFAULT_MAX_MS))
}

/// The ceiling a command asked for, read off the action JSON that the CLI
/// stamps `--settle-ms` / `--no-settle` onto.
pub fn max_ms_for(cmd: &serde_json::Value) -> u64 {
    resolve_max_ms(cmd.get("settleMs").and_then(|v| v.as_u64()))
}

/// Fraction of the ceiling spent watching for a *first* change after an action.
///
/// A `setTimeout` that will render in 300ms emits no signal at all until it
/// fires: the DOM is still, nothing is animating, nothing is in flight. A wait
/// that accepts stillness immediately therefore reports "nothing changed" for
/// every control that reacts on a timer — the same silent wrong answer, just
/// faster. So after an action the wait keeps watching for the first sign of a
/// reaction for half the ceiling before it is willing to call the page
/// unchanged, and it scales with the ceiling so `--settle-ms` is the one knob:
/// raise it for a slow page and the reaction window grows with it.
///
/// The cost is paid only by actions that really change nothing (they wait half
/// the ceiling and then report `changed: false`). Anything that does react —
/// a mutation, a request, an animation — ends the window the moment it starts.
pub const REACTION_FRACTION: u64 = 2;

/// How long to keep watching for a first reaction, given the ceiling.
pub fn reaction_window_ms(max_ms: u64, quiet_ms: u64) -> u64 {
    (max_ms / REACTION_FRACTION).clamp(quiet_ms.min(max_ms), max_ms)
}

/// Wait until the page stops changing, or the ceiling expires.
///
/// `since` marks the point the caller cares about — for `--observe` that is the
/// instant *before* the action was dispatched, so a request the action fired is
/// counted even though it started before this call. Requests already in flight
/// beforehand (a streaming response, a long-poll) are not this observation's
/// business and are ignored.
///
/// `expect_change` is for an observation that follows an action of ours: it
/// turns on the reaction window described at [`REACTION_FRACTION`]. A plain
/// `snapshot` passes `false` — there is no action to react to, so stillness is
/// the answer, not a signal to keep waiting.
pub async fn settle(
    state: &mut DaemonState,
    max_ms: u64,
    since: Instant,
    expect_change: bool,
) -> SettleOutcome {
    settle_armed(state, max_ms, since, expect_change, None).await
}

/// [`settle`] seeded by a [`SettleArm`] installed just before the action.
///
/// Without an arm, a mutation the action made synchronously (a click handler
/// that re-renders a list before the dispatch returns) happens before the
/// page-side observer exists, so the wait sees a still page and spends the
/// whole reaction window (half the ceiling, 500ms by default) looking for a
/// reaction that has already happened. The arm was watching during dispatch:
/// when it saw a mutation, that IS the first reaction, and the quiet window is
/// measured from the last mutation it saw. The guarantee is unchanged: the
/// DOM must still have been still for the full quiet window, with no finite
/// animation running and no request in flight. The wait just stops pretending
/// it did not see the change.
///
/// An arm that cannot be read (the action navigated and the document that
/// owned it is gone) falls back to the unseeded wait.
pub async fn settle_armed(
    state: &mut DaemonState,
    max_ms: u64,
    since: Instant,
    expect_change: bool,
    arm: Option<&SettleArm>,
) -> SettleOutcome {
    let mut arm = arm;
    if max_ms == 0 {
        if let Some(a) = arm {
            release_arm(state, a).await;
        }
        return SettleOutcome::skipped();
    }
    let quiet_ms = env_ms(ENV_QUIET_MS, DEFAULT_QUIET_MS).min(max_ms);
    let start = Instant::now();
    let deadline = start + Duration::from_millis(max_ms);
    let reaction_deadline = start
        + Duration::from_millis(if expect_change {
            reaction_window_ms(max_ms, quiet_ms)
        } else {
            0
        });
    let mut last_pending: Vec<String> = Vec::new();
    let mut saw_change = false;

    loop {
        state.drain_cdp_events_background().await;
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = (deadline - now).as_millis() as u64;
        // Time left in which a first reaction is still expected. A request the
        // action fired counts as that reaction, so once anything has happened
        // this drops to zero and the wait is a plain quiet check again.
        let reaction_left = if saw_change || now >= reaction_deadline {
            0
        } else {
            (reaction_deadline - now).as_millis() as u64
        };

        // Network first: it costs no round trip, and while a request the action
        // fired is outstanding the DOM being quiet means nothing.
        if state.pending_request_count(since) > 0 {
            saw_change = true;
            last_pending = vec!["network".to_string()];
            tokio::time::sleep(Duration::from_millis(NETWORK_POLL_MS.min(remaining))).await;
            continue;
        }

        // Too little left for the page-side wait to learn anything: its budget
        // would be zero, and a zero-length quiet window is satisfied the instant
        // it is checked — a false "quiet" right at the ceiling. Wait out the
        // remainder instead and let the loop report the timeout it really is.
        if remaining <= ROUND_TRIP_ALLOWANCE_MS {
            tokio::time::sleep(Duration::from_millis(remaining)).await;
            continue;
        }

        let seeded = match arm.take() {
            Some(a) => {
                let r =
                    page_quiet_seeded(state, a, quiet_ms.min(remaining), remaining, reaction_left)
                        .await;
                release_arm(state, a).await;
                // An error means the arm's document or handle is gone (the
                // action navigated): fall through and wait the old way.
                r.ok()
            }
            None => None,
        };
        let probe = match seeded {
            Some(p) => Ok(p),
            None => {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                let remaining = (deadline - now).as_millis() as u64;
                if remaining <= ROUND_TRIP_ALLOWANCE_MS {
                    continue;
                }
                page_quiet(state, quiet_ms.min(remaining), remaining, reaction_left).await
            }
        };
        match probe {
            Ok(page) if page.pending.is_empty() => {
                saw_change |= page.saw_change;
                // The page went quiet; re-check the network, because it may have
                // started fetching while we were waiting on the DOM.
                state.drain_cdp_events_background().await;
                if state.pending_request_count(since) == 0 {
                    return SettleOutcome {
                        waited_ms: start.elapsed().as_millis() as u64,
                        quiet: true,
                        saw_change,
                        pending: Vec::new(),
                    };
                }
                saw_change = true;
                last_pending = vec!["network".to_string()];
            }
            Ok(page) => {
                saw_change |= page.saw_change;
                last_pending = page.pending;
            }
            Err(_) => {
                // The execution context went away mid-wait — a navigation, or a
                // renderer that is busy enough not to answer. Neither is a
                // settled page, so keep waiting rather than reporting quiet.
                last_pending = vec!["dom".to_string()];
                tokio::time::sleep(Duration::from_millis(NETWORK_POLL_MS.min(remaining))).await;
            }
        }
    }

    // The ceiling hit before the page was ever probed (a request held the
    // whole wait): the arm was never consumed.
    if let Some(a) = arm {
        release_arm(state, a).await;
    }
    if last_pending.is_empty() {
        last_pending.push("dom".to_string());
    }
    SettleOutcome {
        waited_ms: start.elapsed().as_millis() as u64,
        quiet: false,
        saw_change,
        pending: last_pending,
    }
}

/// What one page-side wait reported.
struct PageQuiet {
    /// Signals still busy when it gave up. Empty means the page is still.
    pending: Vec<String>,
    /// Whether anything mutated while it was watching.
    saw_change: bool,
}

/// Page-side half of the wait: resolves when the DOM has been still for
/// `quiet_ms` and no finite animation is running, or when `budget_ms` expires.
/// While `reaction_ms` has not elapsed it will not call an untouched page
/// settled — that window is what catches a render scheduled on a timer.
async fn page_quiet(
    state: &DaemonState,
    quiet_ms: u64,
    budget_ms: u64,
    reaction_ms: u64,
) -> Result<PageQuiet, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    // The page resolves a little before the deadline so the reply still fits
    // inside it: `max_ms` is documented as the ceiling on the wait, and a grace
    // period added on top of the budget would quietly overrun it (a 50ms
    // ceiling waiting ~550ms). The room comes out of the page's budget, not off
    // the end of the caller's.
    let page_budget = budget_ms.saturating_sub(ROUND_TRIP_ALLOWANCE_MS);
    if page_budget == 0 {
        // Defensive: a zero budget makes the quiet window zero, which every page
        // satisfies immediately. Saying "I could not look" beats reporting a
        // quiet the wait never established.
        return Err("no budget left for a settle probe".to_string());
    }
    let script = page_quiet_script(quiet_ms.min(page_budget), page_budget, reaction_ms);
    let value = tokio::time::timeout(
        Duration::from_millis(budget_ms),
        mgr.evaluate(&script, None),
    )
    .await
    .map_err(|_| "settle wait timed out".to_string())??;

    Ok(parse_page_quiet(&value))
}

/// The injected wait. Kept as a builder (rather than a format! at the call
/// site) so the JS can be unit-tested for the substitutions it depends on.
///
/// Note the animation filter: only animations with a finite duration and finite
/// iteration count count as "still running". A spinner declared
/// `animation: spin 1s infinite` is the normal state of a loading page, not a
/// transition that will end, and waiting on it would spend the whole ceiling on
/// every observation of every page that has one.
///
/// `reaction_ms` is the window in which stillness alone is not an answer: until
/// it elapses (or something mutates) the wait keeps going, which is what makes a
/// render scheduled on a timer visible instead of reported as "no change".
pub fn page_quiet_script(quiet_ms: u64, budget_ms: u64, reaction_ms: u64) -> String {
    format!(
        "(() => {})()",
        page_quiet_promise(quiet_ms, budget_ms, reaction_ms, false)
    )
}

/// The same wait as [`page_quiet_script`], as a function called ON a
/// [`SettleArm`] (`this` is the arm). Before its first check it takes over from
/// the arm: whatever the arm saw since it was installed, in particular the
/// mutations the action made while it was being dispatched, counts as seen,
/// with the quiet window measured from the arm's last mutation.
pub fn page_quiet_seeded_function(quiet_ms: u64, budget_ms: u64, reaction_ms: u64) -> String {
    format!(
        "function() {{ return {}; }}",
        page_quiet_promise(quiet_ms, budget_ms, reaction_ms, true)
    )
}

fn page_quiet_promise(quiet_ms: u64, budget_ms: u64, reaction_ms: u64, seeded: bool) -> String {
    // The hand-over runs AFTER this wait's own observer is installed, so no
    // mutation can fall between the two. `takeRecords` collects anything the
    // arm had queued but not yet delivered. `this` is captured before the
    // Promise executor, whose own `this` is not the arm.
    let seed = if seeded {
        r#"
  try {
    if (arm && arm.obs) {
      if (arm.obs.takeRecords().length) { arm.saw = true; arm.last = performance.now(); }
      arm.obs.disconnect();
    }
    if (arm && arm.timer) clearTimeout(arm.timer);
    // An expired arm stopped watching at some unknown point: its `last`
    // can predate later mutations, so it proves nothing. Seed only from a
    // live one; otherwise this is the plain wait with its full rules.
    if (arm && arm.saw && !arm.expired) { sawChange = true; lastChange = arm.last; }
  } catch (e) {}"#
    } else {
        ""
    };
    let capture = if seeded { "const arm = this;\n  " } else { "" };
    format!(
        r#"(() => {{ {capture}return new Promise((resolve) => {{
  const QUIET = {quiet_ms};
  const REACTION = {reaction_ms};
  const t0 = performance.now();
  const deadline = t0 + {budget_ms};
  let lastChange = t0;
  let sawChange = false;
  let observer = null;
  try {{
    observer = new MutationObserver(() => {{ lastChange = performance.now(); sawChange = true; }});
    observer.observe(document, {{ subtree: true, childList: true, attributes: true, characterData: true }});
  }} catch (e) {{}}{seed}
  const animating = () => {{
    try {{
      if (typeof document.getAnimations !== 'function') return false;
      return document.getAnimations().some((a) => {{
        if (a.playState !== 'running') return false;
        const t = a.effect && a.effect.getComputedTiming ? a.effect.getComputedTiming() : null;
        if (!t) return false;
        if (t.iterations === Infinity || t.duration === Infinity) return false;
        return true;
      }});
    }} catch (e) {{ return false; }}
  }};
  const finish = (pending) => {{
    try {{ if (observer) observer.disconnect(); }} catch (e) {{}}
    resolve({{ pending, sawChange, waitedMs: Math.round(performance.now() - t0) }});
  }};
  const tick = () => {{
    const now = performance.now();
    const domQuiet = now - lastChange >= QUIET;
    const anim = animating();
    // Stillness is only an answer once a reaction has been seen, or once the
    // window in which one was expected has passed.
    const answered = sawChange || now - t0 >= REACTION;
    if (domQuiet && !anim && answered) return finish([]);
    if (now >= deadline) {{
      const pending = [];
      if (!domQuiet) pending.push('dom');
      if (anim) pending.push('animation');
      return finish(pending);
    }}
    setTimeout(tick, 16);
  }};
  tick();
}}); }}).call(this)"#
    )
}

/// A mutation observer installed in the page just before an action, so the
/// settle that follows knows about mutations the action made while it was
/// being dispatched (see [`settle_armed`]).
///
/// It is held by a CDP remote-object handle, not stored on `window`: nothing
/// the page can enumerate changes, and the handle dies with the document. An
/// arm nobody reads (the action failed) is released by the caller, and in any
/// case disconnects itself after [`ARM_LIFETIME_MS`].
pub struct SettleArm {
    object_id: String,
    session_id: String,
}

/// How long an arm that nobody reads keeps observing before it disconnects
/// itself.
const ARM_LIFETIME_MS: u64 = 30_000;

/// The arm itself: an object holding its observer, when it last fired, and
/// whether it fired at all.
pub fn arm_script() -> String {
    format!(
        r#"(() => {{
  const arm = {{ saw: false, last: 0, obs: null, timer: 0, expired: false }};
  try {{
    arm.obs = new MutationObserver(() => {{ arm.saw = true; arm.last = performance.now(); }});
    arm.obs.observe(document, {{ subtree: true, childList: true, attributes: true, characterData: true }});
    arm.timer = setTimeout(() => {{ arm.expired = true; try {{ arm.obs.disconnect(); }} catch (e) {{}} }}, {ARM_LIFETIME_MS});
  }} catch (e) {{}}
  return arm;
}})()"#
    )
}

/// Install a [`SettleArm`] on the active page. `None` when it could not be
/// installed; the settle then waits exactly as it did before arms existed.
pub async fn arm(state: &DaemonState) -> Option<SettleArm> {
    let mgr = state.browser.as_ref()?;
    let session_id = mgr.active_session_id().ok()?.to_string();
    let v = mgr
        .client
        .send_command(
            "Runtime.evaluate",
            Some(json!({ "expression": arm_script(), "returnByValue": false })),
            Some(&session_id),
        )
        .await
        .ok()?;
    if v.get("exceptionDetails").is_some() {
        return None;
    }
    let object_id = v.pointer("/result/objectId")?.as_str()?.to_string();
    Some(SettleArm {
        object_id,
        session_id,
    })
}

/// Let go of an arm: stop its observer and release the handle. Best-effort,
/// one round trip: a document that already went away took the object with it.
pub async fn release_arm(state: &DaemonState, arm: &SettleArm) {
    if let Some(mgr) = state.browser.as_ref() {
        let _ = mgr
            .client
            .send_command(
                "Runtime.callFunctionOn",
                Some(json!({
                    "objectId": arm.object_id,
                    "functionDeclaration": "function() { try { if (this.obs) this.obs.disconnect(); clearTimeout(this.timer); } catch (e) {} }",
                    "returnByValue": true,
                    "objectGroup": "chrome-use-settle-release",
                })),
                Some(&arm.session_id),
            )
            .await;
        let _ = mgr
            .client
            .send_command(
                "Runtime.releaseObject",
                Some(json!({ "objectId": arm.object_id })),
                Some(&arm.session_id),
            )
            .await;
    }
}

/// [`page_quiet`] run on an arm.
async fn page_quiet_seeded(
    state: &DaemonState,
    arm: &SettleArm,
    quiet_ms: u64,
    budget_ms: u64,
    reaction_ms: u64,
) -> Result<PageQuiet, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let page_budget = budget_ms.saturating_sub(ROUND_TRIP_ALLOWANCE_MS);
    if page_budget == 0 {
        return Err("no budget left for a settle probe".to_string());
    }
    let function = page_quiet_seeded_function(quiet_ms.min(page_budget), page_budget, reaction_ms);
    let v = tokio::time::timeout(
        Duration::from_millis(budget_ms),
        mgr.client.send_command(
            "Runtime.callFunctionOn",
            Some(json!({
                "objectId": arm.object_id,
                "functionDeclaration": function,
                "returnByValue": true,
                "awaitPromise": true,
            })),
            Some(&arm.session_id),
        ),
    )
    .await
    .map_err(|_| "settle wait timed out".to_string())??;
    if v.get("exceptionDetails").is_some() {
        return Err("settle probe threw".to_string());
    }
    Ok(parse_page_quiet(
        v.pointer("/result/value").unwrap_or(&Value::Null),
    ))
}

fn parse_page_quiet(value: &Value) -> PageQuiet {
    let pending = value
        .get("pending")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let saw_change = value
        .get("sawChange")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    PageQuiet {
        pending,
        saw_change,
    }
}

/// How far back a standalone observation looks for in-flight requests.
///
/// `snapshot` has no action to anchor to, so it uses a short window instead: a
/// request that started a moment ago is the page still loading and worth
/// waiting for; one that started a minute ago is somebody's stream and is not.
pub const LOOKBACK_MS: u64 = 2000;

/// The `since` anchor for an observation that follows no action of ours.
pub fn lookback() -> Instant {
    let now = Instant::now();
    now.checked_sub(Duration::from_millis(LOOKBACK_MS))
        .unwrap_or(now)
}

/// Requests that started at or after `since` and have not finished, ignoring
/// ones old enough to be a stream rather than a page load.
pub fn pending_requests(in_flight: &[(String, Instant)], since: Instant) -> usize {
    let stale = Duration::from_millis(NETWORK_STALE_MS);
    in_flight
        .iter()
        .filter(|(_, started)| *started >= since && started.elapsed() < stale)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skipped_settle_is_quiet_and_warns_nothing() {
        let s = SettleOutcome::skipped();
        assert!(s.quiet);
        assert_eq!(s.waited_ms, 0);
        assert!(s.warning().is_none());
    }

    /// The ceiling exists to bound the wait, not to hide it: a capture taken
    /// after the ceiling expired must carry a line saying so, naming what was
    /// still moving. This is the "never ship a silent success" rule applied to
    /// waiting.
    #[test]
    fn timed_out_settle_names_what_was_busy() {
        let s = SettleOutcome {
            waited_ms: 1000,
            quiet: false,
            saw_change: true,
            pending: vec!["network".to_string(), "dom".to_string()],
        };
        let w = s.warning().expect("a non-quiet settle must warn");
        assert!(w.contains("1000ms"), "{w}");
        assert!(w.contains("request in flight"), "{w}");
        assert!(w.contains("DOM still mutating"), "{w}");
        assert!(w.contains(ENV_MAX_MS), "{w}");
    }

    #[test]
    fn describes_known_signals_in_prose() {
        assert_eq!(describe_pending(&[]), "nothing");
        assert_eq!(
            describe_pending(&["animation".to_string()]),
            "animation running"
        );
        assert_eq!(describe_pending(&["custom".to_string()]), "custom");
    }

    #[test]
    fn explicit_ceiling_beats_env_and_zero_survives() {
        // `--no-settle` stamps 0, which must NOT be read as "unset" — that is
        // the difference between skipping the wait and paying the default.
        assert_eq!(resolve_max_ms(Some(0)), 0);
        assert_eq!(resolve_max_ms(Some(4000)), 4000);
    }

    #[test]
    fn max_ms_reads_the_stamped_action_field() {
        assert_eq!(max_ms_for(&json!({ "settleMs": 0 })), 0);
        assert_eq!(max_ms_for(&json!({ "settleMs": 750 })), 750);
        assert_eq!(max_ms_for(&json!({})), DEFAULT_MAX_MS);
    }

    #[test]
    fn env_ms_falls_back_on_garbage() {
        assert_eq!(env_ms("AGENT_BROWSER_SETTLE_DEFINITELY_UNSET", 123), 123);
    }

    /// A streaming response (SSE, long-poll) never finishes. Counting it would
    /// hold every later observation to the ceiling, so age it out.
    #[test]
    fn stale_requests_stop_blocking_the_wait() {
        let since = Instant::now() - Duration::from_secs(60);
        let fresh = Instant::now();
        let ancient = Instant::now() - Duration::from_millis(NETWORK_STALE_MS + 500);
        let in_flight = vec![
            ("fresh".to_string(), fresh),
            ("stream".to_string(), ancient),
        ];
        assert_eq!(pending_requests(&in_flight, since), 1);
    }

    /// Requests already in flight before the action are not this observation's
    /// business — waiting on someone else's stream is how a fast page ends up
    /// paying the ceiling.
    #[test]
    fn requests_older_than_the_action_are_ignored() {
        let before = Instant::now() - Duration::from_millis(50);
        let since = Instant::now();
        let in_flight = vec![("prior".to_string(), before)];
        assert_eq!(pending_requests(&in_flight, since), 0);
    }

    /// The reaction window scales with the ceiling, so `--settle-ms` stays the
    /// single knob: a slower page gets both a longer wait and a longer window
    /// in which a timer-driven render still counts.
    #[test]
    fn reaction_window_is_half_the_ceiling_and_never_below_the_quiet_window() {
        assert_eq!(reaction_window_ms(1000, 100), 500);
        assert_eq!(reaction_window_ms(3000, 100), 1500);
        // A ceiling shorter than the quiet window cannot promise more than itself.
        assert_eq!(reaction_window_ms(50, 100), 50);
        assert_eq!(reaction_window_ms(150, 100), 100);
    }

    #[test]
    fn quiet_script_carries_its_budgets() {
        let js = page_quiet_script(100, 1000, 500);
        assert!(js.contains("const QUIET = 100;"), "{js}");
        assert!(js.contains("const REACTION = 500;"), "{js}");
        assert!(js.contains("t0 + 1000"), "{js}");
        // Infinite spinners must not be treated as a transition in progress.
        assert!(js.contains("t.iterations === Infinity"), "{js}");
        // `Promise` in the source is what makes `evaluate` await it rather than
        // returning the pending promise object.
        assert!(js.contains("Promise"), "{js}");
    }

    #[test]
    fn seeded_wait_takes_over_from_the_arm() {
        let js = page_quiet_seeded_function(100, 900, 500);
        assert!(js.starts_with("function()"), "{js}");
        assert!(js.contains("const arm = this;"), "{js}");
        // Queued-but-undelivered records count as seen.
        assert!(js.contains("takeRecords()"), "{js}");
        assert!(js.contains("lastChange = arm.last"), "{js}");
        assert!(js.contains("!arm.expired"), "{js}");
        // The unseeded form carries none of it.
        let plain = page_quiet_script(100, 900, 500);
        assert!(!plain.contains("arm"), "{plain}");
    }

    #[test]
    fn the_arm_is_not_stored_on_the_page() {
        let js = arm_script();
        assert!(!js.contains("window."), "{js}");
        assert!(!js.contains("globalThis"), "{js}");
        assert!(js.contains(&ARM_LIFETIME_MS.to_string()), "{js}");
        // Timing out marks the arm, so a stale seed is never trusted.
        assert!(js.contains("arm.expired = true"), "{js}");
    }

    /// Run a settle script under node with a stub DOM: no mutation ever
    /// arrives during the wait. Returns (waitedMs, sawChange).
    fn run_wait(js_call: &str) -> Option<(f64, bool)> {
        let node = std::env::var_os("PATH").and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("node"))
                .find(|n| n.is_file())
        })?;
        let prelude = "globalThis.MutationObserver = class { constructor(cb) {} \
             observe() {} disconnect() {} takeRecords() { return []; } }; \
             globalThis.document = { getAnimations: () => [] };";
        let js = format!(
            "{prelude} Promise.resolve({js_call}).then(r => \
             console.log(JSON.stringify([r.waitedMs, r.sawChange])));"
        );
        let out = std::process::Command::new(node)
            .arg("-e")
            .arg(js)
            .output()
            .ok()?;
        let v: (f64, bool) =
            serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).ok()?;
        Some(v)
    }

    /// The case behind the ~510ms per observed click: the action mutated the
    /// DOM while it was dispatched, before the settle's observer existed. An
    /// unseeded wait sees a still page and spends the reaction window; a wait
    /// seeded by an arm that saw the mutation ends once the quiet window since
    /// that mutation has passed, and still reports the change.
    #[test]
    fn an_arm_that_saw_the_dispatch_mutation_ends_the_reaction_window() {
        let unseeded = page_quiet_script(100, 900, 500);
        let Some((waited, saw)) = run_wait(&unseeded) else {
            return;
        };
        assert!(waited >= 500.0, "unseeded waited {waited}");
        assert!(!saw);

        // The arm saw a mutation 30ms ago: 70ms more of quiet is still owed.
        let seeded = format!(
            "({}).call({{ saw: true, last: performance.now() - 30, \
             obs: new MutationObserver(() => {{}}), timer: 0 }})",
            page_quiet_seeded_function(100, 900, 500)
        );
        let (waited, saw) = run_wait(&seeded).expect("node ran once already");
        assert!(saw, "the arm's mutation must be reported");
        assert!(
            (60.0..300.0).contains(&waited),
            "seeded wait must still owe the quiet window, waited {waited}"
        );

        // An arm that saw nothing changes nothing: the reaction window holds.
        let idle = format!(
            "({}).call({{ saw: false, last: 0, \
             obs: new MutationObserver(() => {{}}), timer: 0 }})",
            page_quiet_seeded_function(100, 900, 500)
        );
        let (waited, saw) = run_wait(&idle).expect("node ran once already");
        assert!(!saw);
        assert!(waited >= 500.0, "idle arm waited {waited}");

        // An arm that timed out (its observer was disconnected mid-action)
        // saw a mutation at some point before: it must not seed the wait,
        // which then keeps the full first-reaction rule.
        let expired = format!(
            "({}).call({{ saw: true, last: performance.now() - 30, expired: true, \
             obs: new MutationObserver(() => {{}}), timer: 0 }})",
            page_quiet_seeded_function(100, 900, 500)
        );
        let (waited, saw) = run_wait(&expired).expect("node ran once already");
        assert!(!saw, "an expired arm's mutation must not count");
        assert!(waited >= 500.0, "expired arm waited {waited}");
    }
}
