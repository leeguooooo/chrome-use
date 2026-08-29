// Browser-level tab liveness helpers. Keeping them free of `chrome.*` globals
// makes the service-worker recovery rules unit-testable.

export async function resolveFirstLiveTab(candidateIds, getTab) {
  const seen = new Set()
  for (const tabId of candidateIds) {
    if (tabId == null || seen.has(tabId)) continue
    seen.add(tabId)
    const tab = await getTab(tabId).catch(() => null)
    if (tab) return { tabId, tab }
  }
  return null
}

export async function reconcileAttachedTabEntries(entries, deps) {
  const live = []
  const removed = []
  for (const [tabId, entry] of entries) {
    const tab = await deps.getTab(tabId).catch(() => null)
    if (tab) {
      live.push([tabId, entry])
      continue
    }
    deps.detach(tabId)
    deps.unmarkOwned(tabId)
    removed.push({ tabId, targetId: entry.targetId, sessionId: entry.sessionId })
  }
  return { live, removed }
}
