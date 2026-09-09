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
 * once the worker stops.
 */
export function canApplyUpdateNow(tabEntries) {
  for (const [, entry] of tabEntries) {
    if (!entry) continue;
    if (entry.attached !== false) return false;
    if (entry.inflight > 0) return false;
  }
  return true;
}
