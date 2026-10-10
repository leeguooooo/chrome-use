// What the native host (the relay) records about our tabs, and how it learns
// that one is gone (#519).
//
// The host keeps one record per `Target.attachedToTarget` it is sent and drops
// it only on a matching `Target.detachedFromTarget`. Anything that announces a
// tab after its detach was sent — an announce still awaiting Chrome when the
// tab closed, an attach that finished after `tabs.onRemoved` — leaves a record
// no later event removes. Those are the phantom pages `Target.getTargets`
// returned. The helpers here are the worker's side of keeping the two in step:
// a tombstone set so late work for a closed tab is refused, the event that
// tells the host a tab is gone, and the full list of live records the host
// keeps on (re)connect, dropping everything else.
//
// Kept free of `chrome.*` so the rules are unit-testable.

/** How many closed tab ids to remember. Chrome never reuses a tab id within a
 * browser session, so the set only has to outlive the races it guards. */
export const REMOVED_TAB_LIMIT = 4096;

/** A bounded set of tab ids Chrome reported removed (or replaced). */
export function createRemovedTabs(limit = REMOVED_TAB_LIMIT) {
  const ids = new Set();
  return {
    add(tabId) {
      if (!Number.isInteger(tabId)) return;
      ids.delete(tabId);
      ids.add(tabId);
      while (ids.size > limit) ids.delete(ids.values().next().value);
    },
    has(tabId) {
      return ids.has(tabId);
    },
    get size() {
      return ids.size;
    },
  };
}

/** The relay session id of a Chrome tab. */
export function tabSessionId(tabId) {
  return `cb-tab-${tabId}`;
}

/**
 * The event that makes the host drop every record of a tab's session. Sent
 * even when the worker no longer holds an entry for the tab: the host may
 * still have one the worker has already forgotten.
 */
export function tabGoneEvent(tabId) {
  const sessionId = tabSessionId(tabId);
  return {
    method: 'forwardCDPEvent',
    params: {
      sessionId,
      method: 'Target.detachedFromTarget',
      params: { sessionId },
    },
  };
}

/**
 * The full list of tab records this worker holds, for the host to keep while
 * dropping every other page record (hosts from before #519 ignore it).
 * `tabEntries` is the worker's `tabs` map (tabId -> entry).
 */
export function relayTargetsMessage(tabEntries) {
  const targets = [];
  for (const [, entry] of tabEntries) {
    if (!entry || !entry.targetId || !entry.sessionId) continue;
    targets.push({ targetId: String(entry.targetId), sessionId: String(entry.sessionId) });
  }
  return { method: 'relayTargets', targets };
}

/**
 * What to do with an announce once its awaits are over: `gone` when the tab
 * was closed meanwhile (never announce it: the host would keep it forever),
 * `superseded` when another entry replaced this one (that one announces
 * itself), else `announce`.
 */
export function announceVerdict({ current, entry, missing, removed }) {
  if (missing || removed) return 'gone';
  if (current !== entry) return 'superseded';
  return 'announce';
}
