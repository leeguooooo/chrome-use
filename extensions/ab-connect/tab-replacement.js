// Chrome replaces a tab's id when it discards the tab (Memory Saver, or
// chrome.tabs.discard) and when it swaps in a prerendered page. The relay's
// sessions are keyed on the tab id (`cb-tab-<id>`), so without following the
// replacement a session's tab just "disappears" (upstream agent-browser #1543
// fixed the same symptom another way).

/** How many replacements to follow before giving up (a loop guard). */
const MAX_HOPS = 16

/**
 * The tab id now standing in for `tabId`, following old -> new replacements,
 * or `tabId` itself when it was never replaced. Pure.
 */
export function followReplacement(replaced, tabId) {
  let current = tabId
  for (let i = 0; i < MAX_HOPS; i++) {
    const next = replaced.get(current)
    if (next == null || next === current) return current
    current = next
  }
  return current
}

/**
 * Bring a discarded tab back without activating it: chrome.tabs.reload loads
 * it in the background (the agent never brings a tab to the front), then wait
 * for it to finish loading, bounded. `api` is chrome.tabs-shaped for tests.
 */
export async function reviveDiscardedTab(api, tabId, { timeoutMs = 8000, stepMs = 200 } = {}) {
  await api.reload(tabId)
  const until = Date.now() + timeoutMs
  for (;;) {
    const tab = await api.get(tabId).catch(() => null)
    if (!tab) return false
    if (!tab.discarded && tab.status === 'complete') return true
    if (Date.now() >= until) return !tab.discarded
    await new Promise((r) => setTimeout(r, stepMs))
  }
}
