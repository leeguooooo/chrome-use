// Bound how long a position marker can outlive the CLI that drew it (#850).
// The daemon draws the marker with `Overlay.setShowHinge` through the relay
// and clears it itself about 2 s after the last point. A daemon that is
// stopped, hung or gone cannot, and the relay keeps the debugger attached, so
// the marker would stay until the tab navigated. The worker watches the
// command go by and clears the marker itself when no newer draw for that tab
// arrives in time. It only ever sends `Overlay.setShowHinge {}` on an
// existing attachment: nothing the page can see.
export const MARKER_WATCHDOG_MS = 4000;

export function createMarkerWatchdog({ clear, schedule = setTimeout, cancel = clearTimeout, delay = MARKER_WATCHDOG_MS }) {
  const timers = new Map();
  function forget(tabId) {
    const timer = timers.get(tabId);
    if (timer !== undefined) cancel(timer);
    timers.delete(tabId);
  }
  return {
    /** Every relayed command on a tab's own session goes past here. */
    observe(tabId, method, params, childSessionId) {
      if (method !== 'Overlay.setShowHinge' || childSessionId || tabId == null) return;
      forget(tabId);
      if (!params || !params.hingeConfig) return;
      timers.set(tabId, schedule(() => {
        timers.delete(tabId);
        Promise.resolve().then(() => clear(tabId)).catch(() => {});
      }, delay));
    },
    forget,
    pending: () => [...timers.keys()],
  };
}
