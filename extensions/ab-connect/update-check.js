// Pull Chrome's update check forward, and decide when applying one is safe.
//
// Chrome polls the Web Store on its own schedule (hours), and only while the
// browser is running — so a fix can sit unshipped on a machine for a long time,
// and the people most affected are the ones who never open chrome://extensions.
// `chrome.runtime.requestUpdateCheck()` asks Chrome to look now. It needs no
// permission, so this does not widen what the extension may do.
//
// Kept free of `chrome.*` so the timing and safety rules are unit-testable.

/** Don't ask Chrome more often than this; it throttles and returns `throttled`. */
export const UPDATE_CHECK_INTERVAL_MS = 60 * 60 * 1000;

/** Whether enough time has passed since the last check. */
export function shouldCheckForUpdate(lastCheckedAt, now, intervalMs = UPDATE_CHECK_INTERVAL_MS) {
  if (!Number.isFinite(lastCheckedAt) || lastCheckedAt <= 0) return true;
  return now - lastCheckedAt >= intervalMs;
}

/**
 * Whether a downloaded update may be applied right now.
 *
 * Applying means `chrome.runtime.reload()`, which drops the native-messaging
 * port and every debugger attachment. Doing that under a running task would
 * turn a silent background improvement into a visible failure, so the bar is
 * "nothing is attached": no tab is being driven, so nothing can be interrupted.
 * A pending update that has to wait is not lost — Chrome applies it on its own
 * once the worker stops. (But the keepalive keeps the worker alive while
 * paired, so an attached tab alone used to hold an update back for good: #524.
 * `updateApplyPlan` below is the rule the worker now applies.)
 */
export function canApplyUpdateNow(tabEntries) {
  for (const [, entry] of tabEntries) {
    if (!entry) continue;
    if (entry.attached !== false) return false;
    if (entry.inflight > 0) return false;
  }
  return true;
}

/**
 * How long the relay must have been quiet (no command from the host, none in
 * flight) before a pending update may release the attached tabs and reload
 * (#524). Long enough that a task between two commands is not cut in half;
 * short enough that a Chrome with a long-lived driven tab still updates.
 */
export const UPDATE_IDLE_GRACE_MS = 60 * 1000;

/**
 * Whether, and when, a downloaded update may be applied (#524).
 *
 * - Never while a command is in flight (a host command, a tab command, or a
 *   re-attach), so nothing is cut mid-command.
 * - At once when no tab is attached: nothing is being driven.
 * - Otherwise once the relay has been quiet for `graceMs`: the attached tabs
 *   are agent tabs an idle session left behind (idle-detach defaults to off).
 *   They are released and the worker reloads; the CLI re-attaches on the next
 *   command, and the tabs this extension created keep their ownership across
 *   the reload (`keepsOwnershipAcrossUpdate`).
 *
 * `lastActivityAt` is the last host command's start or end (or the worker's
 * start); an attached tab's own `lastActivity` (commands, attaches) counts too.
 * The result is what `ABExt.state` reports under `update`, plus `apply`.
 */
export function updateApplyPlan(
  tabEntries,
  { now, lastActivityAt = 0, commandsInFlight = 0, graceMs = UPDATE_IDLE_GRACE_MS } = {}
) {
  let inFlight = Math.max(0, Number(commandsInFlight) || 0);
  let attachedTabs = 0;
  let reattaching = 0;
  let last = Number.isFinite(lastActivityAt) ? lastActivityAt : 0;
  for (const [, entry] of tabEntries) {
    if (!entry) continue;
    if (entry.inflight > 0) inFlight += entry.inflight;
    if (entry.reattaching) reattaching++;
    if (entry.attached === false) continue;
    attachedTabs++;
    if (Number.isFinite(entry.lastActivity) && entry.lastActivity > last) last = entry.lastActivity;
  }
  const idleForMs = Math.max(0, now - last);
  const base = { commandsInFlight: inFlight + reattaching, attachedTabs, idleForMs, graceMs };
  if (inFlight > 0 || reattaching > 0)
    return { apply: false, reason: 'command_in_flight', appliesInMs: null, ...base };
  if (attachedTabs === 0) return { apply: true, reason: 'nothing_attached', appliesInMs: 0, ...base };
  if (idleForMs >= graceMs) return { apply: true, reason: 'idle', appliesInMs: 0, ...base };
  return { apply: false, reason: 'recent_activity', appliesInMs: graceMs - idleForMs, ...base };
}

/** Storage key the worker writes right before it reloads into an update. */
export const UPDATE_HANDOFF_KEY = 'ab_update_handoff';

/** A handoff older than this is not trusted (a crash, a much later manual reload). */
export const UPDATE_HANDOFF_MAX_AGE_MS = 10 * 60 * 1000;

/**
 * Whether the worker that starts after an update keeps the persisted set of
 * tabs it created. An install, or an update Chrome applied on its own, purges
 * it as before; an update this extension applied itself (a fresh handoff)
 * keeps it, so the session's tabs are re-announced and keep working (#524).
 */
export function keepsOwnershipAcrossUpdate(reason, handoff, now) {
  if (reason !== 'update' || !handoff || typeof handoff !== 'object') return false;
  const at = Number(handoff.at);
  if (!Number.isFinite(at) || at > now + 60 * 1000) return false;
  return now - at <= UPDATE_HANDOFF_MAX_AGE_MS;
}
