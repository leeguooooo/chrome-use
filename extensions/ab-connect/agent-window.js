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

/** Pages that do not make a window the user's: the window's own placeholder. */
function isPlaceholderUrl(url) {
  return !url || url === 'about:blank'
}

/**
 * Decide whether `win` (from chrome.windows.get, with `tabs` from
 * chrome.tabs.query({windowId})) may keep receiving agent tabs.
 *
 * Not an agent window any more when:
 * - it is full screen (the agent never makes its window full screen), or
 * - it holds a tab the agent neither owns nor opened: the user's own tab.
 *   A tab opened by an agent tab (a pop-up) still belongs to the agent.
 */
export function agentWindowStillOurs(win, tabs, isOwned) {
  if (!win) return { ours: false, reason: 'gone' }
  if (win.state === 'fullscreen') return { ours: false, reason: 'fullscreen' }
  for (const tab of tabs || []) {
    if (tab == null || tab.id == null) continue
    if (isOwned(tab.id)) continue
    if (tab.openerTabId != null && isOwned(tab.openerTabId)) continue
    if (isPlaceholderUrl(tab.pendingUrl || tab.url)) continue
    return { ours: false, reason: 'user-tab' }
  }
  return { ours: true, reason: null }
}
