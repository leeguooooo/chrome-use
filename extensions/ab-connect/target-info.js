export function targetInfoForTab(targets, tabId) {
  if (!Array.isArray(targets) || tabId == null) return null
  const candidates = targets.filter((target) => target?.tabId === tabId)
  const target = candidates.find((candidate) => candidate.type === 'page') || candidates[0]
  if (!target?.id) return null
  return {
    targetId: String(target.id),
    type: String(target.type || 'page'),
    url: String(target.url || ''),
    title: String(target.title || ''),
    attached: true,
  }
}
