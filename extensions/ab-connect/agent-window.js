// Whether a remembered "agent window" is still one. Free of `chrome.*` globals
// so the rule is unit-testable.
//
// The agent window is an ordinary Chrome window, and its id is remembered in
// chrome.storage.local. Nothing used to check that it stayed the agent's: once
// the user started working in it (opened their own tabs there, took it full
// screen, closed their other window), every later agent tab was created in the
// window the user was looking at. Each agent tab that closed while in front
// then handed the front to a neighbouring agent tab, and the password-manager
// recovery flipped tabs in that window, so the user's view kept switching to
// pages they never opened. Found on a real profile whose remembered agent
// window was the user's only, full-screen window holding 96 tabs.
//
// The rule refuses whenever it cannot prove the window is the agent's: an
// unreadable tab list, a tab without an id, a tab that claims another window,
// or any tab the agent neither owns nor opened. A blank tab is a user tab like
// any other; the only exemption is the placeholder this agent window was
// created with, identified by the tab and window ids recorded at creation,
// never by its URL.

/**
 * The agent-tab predicate for the agent-window check: tabs the agent created
 * (`owned`) and pop-ups verified as the agent's (`agentPopups`: adopted through
 * ABExt.attachTabById for a session-owned opener/group, or opened by an agent
 * tab). Deliberately NOT "any tab the relay is attached to": that map also
 * holds user tabs taken with `adopt` / `inspect`, which stay the user's.
 */
export function agentTabPredicate(owned, agentPopups) {
  return (tabId) => owned.has(tabId) || agentPopups.has(tabId)
}

/**
 * Carry a confirmed agent pop-up over to its replacement tab id (a discard or
 * prerender swap fires onReplaced, sometimes after onRemoved already dropped
 * the old id into `recentlyRemoved`). Mutates `popups`; returns whether the
 * record moved.
 */
export function migratePopupRecord(popups, recentlyRemoved, removedTabId, addedTabId) {
  if (!popups.has(removedTabId) && !recentlyRemoved.has(removedTabId)) return false
  recentlyRemoved.delete(removedTabId)
  popups.delete(removedTabId)
  popups.add(addedTabId)
  return true
}

/**
 * Whether `tab` (a chrome.tabs.Tab just attached by tab id) may be recorded as
 * an agent pop-up. A tab id alone is not verification: the tab must have been
 * opened by an agent tab, or sit in a tab group that holds an agent tab (Chrome
 * puts a pop-up in its opener's group, and often reports the window's front
 * tab as the opener). `allTabs` is chrome.tabs.query({}).
 */
export function isVerifiedAgentPopup(tab, allTabs, isAgentTab) {
  // Unknown is never "no user tabs": a tab list that could not be read, or
  // tab data with holes or contradictions, refuses.
  if (!tab || !Number.isInteger(tab.id) || !Number.isInteger(tab.windowId)) return false
  if (!Array.isArray(allTabs)) return false
  for (const t of allTabs) {
    if (!t || !Number.isInteger(t.id) || !Number.isInteger(t.windowId)) return false
  }
  const self = allTabs.filter((t) => t.id === tab.id)
  if (self.length !== 1 || self[0].windowId !== tab.windowId) return false
  if (tab.groupId != null && self[0].groupId != null && self[0].groupId !== tab.groupId) return false
  // Chrome names the window's FRONT tab as opener (and uses its group) when
  // the click landed in a background tab. In a window that also holds a user
  // tab, a pop-up the user's tab opened therefore looks like ours. Only a
  // window holding nothing but agent tabs makes opener and group trustworthy.
  const sameWindow = allTabs.filter((t) => t.id !== tab.id && t.windowId === tab.windowId)
  if (sameWindow.some((t) => !isAgentTab(t.id))) return false
  if (tab.openerTabId != null && isAgentTab(tab.openerTabId)) return true
  if (!Number.isInteger(tab.groupId) || tab.groupId === -1) return false
  return allTabs.some((t) => t.id !== tab.id && t.groupId === tab.groupId && isAgentTab(t.id))
}

/**
 * After ABExt.attachTabById attached a tab, decide whether it is recorded as an
 * agent pop-up and say so in `result.agentPopup` (the daemon upgrades the tab
 * to session-created only on `true`). A failed read is unknown: `false`, and
 * nothing is recorded.
 *
 * deps: { getTab(id), queryAll(), isAgentTab(id), mark(id) }
 */
export async function confirmAttachedPopup(result, deps) {
  if (!result || result.attached !== true) return result
  const tab = await Promise.resolve()
    .then(() => deps.getTab(result.chromeTabId))
    .catch(() => null)
  const all = await Promise.resolve()
    .then(() => deps.queryAll())
    .catch(() => null)
  const verified = isVerifiedAgentPopup(tab, all, deps.isAgentTab)
  if (verified) await deps.mark(result.chromeTabId)
  result.agentPopup = verified
  return result
}

/**
 * Whether `tab` is still the untouched placeholder `record` describes: the
 * recorded tab in the recorded window, still on about:blank, with no
 * navigation pending. Once the user navigates it (or starts to), it is theirs.
 */
export function isUntouchedPlaceholder(tab, record) {
  if (!tab || !record) return false
  if (!Number.isInteger(tab.id) || tab.id !== record.tabId) return false
  if (tab.windowId !== record.windowId) return false
  if (tab.url !== 'about:blank') return false
  if (tab.pendingUrl != null && tab.pendingUrl !== '' && tab.pendingUrl !== 'about:blank') return false
  return true
}

/**
 * What to do with the recorded placeholder once a real agent tab exists in
 * window `windowId`: 'remove' only when it is still untouched; otherwise
 * 'forget' (drop the record, leave the tab alone: it may be the user's now).
 */
export function placeholderCleanup(tab, record, windowId) {
  if (!record || record.windowId !== windowId) return 'keep'
  return isUntouchedPlaceholder(tab, record) ? 'remove' : 'forget'
}

/**
 * Decide whether `win` (from chrome.windows.get) may keep receiving agent tabs.
 *
 * @param win          the window, or null if it is gone
 * @param tabs         chrome.tabs.query({windowId}) result, or null if the
 *                     query failed (unknown is not empty)
 * @param isOwned      tabId -> whether the agent owns that tab
 * @param placeholder  { windowId, tabId } recorded when this window was
 *                     created, or null when no record exists
 */
export function agentWindowStillOurs(win, tabs, isOwned, placeholder = null) {
  if (!win || win.id == null) return { ours: false, reason: 'gone' }
  if (win.state === 'fullscreen') return { ours: false, reason: 'fullscreen' }
  if (!Array.isArray(tabs)) return { ours: false, reason: 'tabs-unknown' }
  for (const tab of tabs) {
    if (tab == null || !Number.isInteger(tab.id)) return { ours: false, reason: 'tab-unknown' }
    if (tab.windowId !== win.id) return { ours: false, reason: 'contradictory' }
    if (isOwned(tab.id)) continue
    if (tab.openerTabId != null && isOwned(tab.openerTabId)) continue
    if (placeholder != null && placeholder.windowId === win.id && isUntouchedPlaceholder(tab, placeholder))
      continue
    return { ours: false, reason: 'user-tab' }
  }
  return { ours: true, reason: null }
}
