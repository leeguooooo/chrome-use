// Exact tab presence for `ABExt.tabPresence` (#496): is Chrome tab N open,
// gone, or unknown? `chrome-use close` drops a tab's ownership record only on
// `absent`, so absence must be proven, not inferred from a failed read: only
// Chrome's own "No tab with id: N" for that exact id means the tab is gone.
// Any other error (a refusal, a transient failure, a timeout) is `unknown`.
// Kept free of `chrome.*` globals so the rule is unit-testable.

/**
 * Version of the structured `tabPresence` contract. The CLI trusts `absent`
 * only from a reply that carries this field, so an extension without it
 * (0.5.32 and older) can never have a tab counted as closed.
 */
export const TAB_PRESENCE_VERSION = 1

/** Chrome's exact error for a tab id that does not exist. */
export function isMissingTabError(error, tabId) {
  const message = typeof error === 'string' ? error : error?.message
  return message === `No tab with id: ${tabId}.` || message === `No tab with id: ${tabId}`
}

/**
 * Read one exact tab id. Returns `{ tabPresenceVersion, tabId, presence }`
 * with `presence` one of `present`, `absent`, `unknown`; `url` when present,
 * `error` when unknown.
 */
export async function exactTabPresence(tabId, getTab) {
  const base = { tabPresenceVersion: TAB_PRESENCE_VERSION, tabId }
  if (!Number.isSafeInteger(tabId) || tabId < 0) {
    return { ...base, tabId: null, presence: 'unknown', error: 'tabId must be a non-negative integer' }
  }
  let tab
  try {
    tab = await getTab(tabId)
  } catch (error) {
    if (isMissingTabError(error, tabId)) return { ...base, presence: 'absent' }
    return { ...base, presence: 'unknown', error: String(error?.message ?? error) }
  }
  if (tab && tab.id === tabId) {
    return { ...base, presence: 'present', url: tab.url || tab.pendingUrl || '' }
  }
  return { ...base, presence: 'unknown', error: 'chrome.tabs.get answered without that tab' }
}

/**
 * Read and validate Chrome's whole target registry. Every record must carry a
 * non-empty string `id`, ids must be unique, and a `tabId`, when present,
 * must be a non-negative integer. Anything else (a non-list, a `null` record,
 * a record without an id, a duplicate) makes the whole list unusable as
 * evidence: a malformed list proves nothing about which targets are gone.
 * Returns `{ ok: true, byId }` or `{ ok: false, error }`.
 */
export async function readTargetRegistry(getTargets) {
  let targets
  try {
    targets = await getTargets()
  } catch (error) {
    return { ok: false, error: `chrome.debugger.getTargets failed: ${String(error?.message ?? error)}` }
  }
  if (!Array.isArray(targets)) {
    return { ok: false, error: 'chrome.debugger.getTargets answered without a list' }
  }
  const byId = new Map()
  for (const record of targets) {
    if (record === null || typeof record !== 'object') {
      return { ok: false, error: 'chrome.debugger.getTargets listed a record that is not an object' }
    }
    if (typeof record.id !== 'string' || record.id === '') {
      return { ok: false, error: 'chrome.debugger.getTargets listed a record without a target id' }
    }
    if (record.tabId !== undefined && (!Number.isSafeInteger(record.tabId) || record.tabId < 0)) {
      return { ok: false, error: `chrome.debugger.getTargets listed target ${record.id} with an invalid tab id` }
    }
    if (byId.has(record.id)) {
      return { ok: false, error: `chrome.debugger.getTargets listed target ${record.id} twice` }
    }
    byId.set(record.id, record)
  }
  return { ok: true, byId }
}

/** The answer for a target the registry lists. */
async function listedPresence(base, listed, getTab) {
  if (listed.tabId === undefined) {
    // Listed, so it exists; it just is not a tab we can read back.
    return { ...base, tabId: null, presence: 'present' }
  }
  const tab = await exactTabPresence(listed.tabId, getTab)
  if (tab.presence === 'present') {
    return { ...base, tabId: listed.tabId, presence: 'present', url: tab.url }
  }
  return {
    ...base,
    tabId: listed.tabId,
    presence: 'unknown',
    error: `the target is listed but its tab ${listed.tabId} could not be read: ${tab.error ?? tab.presence}`,
  }
}

/**
 * Whether the tab holding CDP target `targetId` is still open, for
 * `ABExt.tabPresence`. Identity comes from Chrome's own target registry
 * (`chrome.debugger.getTargets`), not the relay's alias maps: a target that
 * survived a tab replacement (discard, prerender swap) under a new Chrome tab
 * id is found at its new tab and reported `present` with that `tabId`.
 *
 * `absent` takes all of: a valid registry that does not list the target, the
 * exact `tabId` reported missing by Chrome, and a second valid registry read
 * AFTER that, which still does not list it (a replacement can move the target
 * into a new tab while the old one is being read). Anything short of that is
 * `unknown`, or `present` when the target turns up.
 *
 * `deps.getTargets()` resolves to Chrome's target list; `deps.getTab(id)` is
 * `chrome.tabs.get`.
 */
export async function targetPresence({ targetId, tabId }, deps) {
  const base = { tabPresenceVersion: TAB_PRESENCE_VERSION, targetId: targetId ?? null }
  if (typeof targetId !== 'string' || targetId === '') {
    return { ...base, targetId: null, tabId: null, presence: 'unknown', error: 'targetId is required' }
  }
  const first = await readTargetRegistry(deps.getTargets)
  if (!first.ok) return { ...base, tabId: tabId ?? null, presence: 'unknown', error: first.error }
  const listed = first.byId.get(targetId)
  if (listed) return listedPresence(base, listed, deps.getTab)
  if (tabId == null) {
    return { ...base, tabId: null, presence: 'unknown', error: 'the target is not listed and no tab id was given to confirm it' }
  }
  const tab = await exactTabPresence(tabId, deps.getTab)
  if (tab.presence === 'present') {
    return {
      ...base,
      tabId,
      presence: 'unknown',
      error: `tab ${tabId} still exists but no longer lists this target`,
    }
  }
  if (tab.presence !== 'absent') {
    return { ...base, tabId: tab.tabId, presence: 'unknown', error: tab.error }
  }
  // The old tab is gone. Read the registry again: the target may have moved
  // into a new tab while the old one was being read.
  const second = await readTargetRegistry(deps.getTargets)
  if (!second.ok) {
    return { ...base, tabId, presence: 'unknown', error: `tab ${tabId} is gone, but ${second.error}` }
  }
  const moved = second.byId.get(targetId)
  if (moved) return listedPresence(base, moved, deps.getTab)
  return { ...base, tabId, presence: 'absent' }
}
