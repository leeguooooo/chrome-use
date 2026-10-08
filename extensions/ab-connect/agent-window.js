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
    if (
      placeholder != null &&
      placeholder.windowId === win.id &&
      placeholder.tabId === tab.id
    )
      continue
    return { ours: false, reason: 'user-tab' }
  }
  return { ours: true, reason: null }
}
