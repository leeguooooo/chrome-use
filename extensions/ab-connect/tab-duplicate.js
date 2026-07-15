// Testable native Duplicate tab transaction. The service worker supplies Chrome
// adapters; tests supply deterministic fakes at the same system boundary.
;(function installTabDuplicate(root) {
  async function duplicateTab(params, deps) {
    const sourceTargetId =
      typeof params?.sourceTargetId === 'string' ? params.sourceTargetId.trim() : ''
    const sourceTabId = sourceTargetId ? deps.tabForTarget(sourceTargetId) : null
    if (sourceTabId == null) {
      throw new Error(`duplicateTab: source target not found: ${sourceTargetId}`)
    }

    const sourceTab = await deps.getTab(sourceTabId).catch(() => null)
    if (!deps.eligible(sourceTab)) throw new Error('duplicateTab: source tab is unavailable')
    const focusedWindow = await deps.getLastFocusedWindow().catch(() => null)
    const restoreWindowId = focusedWindow?.id ?? sourceTab.windowId
    const activeTabs = await deps.getActiveTabs(restoreWindowId).catch(() => [])
    const restoreTabId = activeTabs[0]?.id ?? sourceTabId
    const group = typeof params?.agentGroup === 'string' ? params.agentGroup.trim() : ''
    if (!group) throw new Error('duplicateTab: agentGroup is required')

    let duplicateTabId = null
    try {
      const duplicate = await deps.duplicateTab(sourceTabId)
      duplicateTabId = duplicate?.id ?? null
      if (duplicateTabId == null) throw new Error('duplicateTab: no tab id')
      deps.markOwned(duplicateTabId)
      await deps.groupTabInto(duplicateTabId, group)
      const entry = await deps.attachTab(duplicateTabId)
      await deps.activateTab(restoreTabId)
      await deps.focusWindow(restoreWindowId)
      return { sourceTargetId, targetId: entry.targetId }
    } catch (error) {
      if (duplicateTabId != null) {
        deps.unmarkOwned(duplicateTabId)
        await deps.removeTab(duplicateTabId).catch(() => {})
      }
      await deps.activateTab(restoreTabId).catch(() => {})
      await deps.focusWindow(restoreWindowId).catch(() => {})
      throw error
    }
  }

  root.ABTabDuplicate = Object.freeze({ duplicateTab })
})(globalThis)
