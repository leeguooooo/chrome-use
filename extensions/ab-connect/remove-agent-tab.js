// Close an agent tab without leaving a saved tab group behind.
//
// Chrome keeps every tab group as a saved group, synced to the user's account
// and listed in the Tab groups menu. Closing a group's last tab while its
// window stays open deletes the group, but closing the last tab of a window
// closes the window, and Chrome keeps the groups of a closing window. A
// session's tabs are often the last ones in the agent window, so a plain
// `tabs.remove` left one permanent saved group per session.
//
// Taking the tab out of its group first leaves nothing to save: ungrouping the
// last tab of a group deletes the group, as Chrome's own "Ungroup" does. The
// ungroup is best-effort (a tab may already be ungrouped, or the API missing);
// the removal is the effect the caller asked for, so only its error surfaces.
// Kept free of `chrome.*` globals so the order is unit-testable.
//
// deps: { ungroup(tabId) -> Promise, remove(tabId) -> Promise }

export async function removeAgentTab(tabId, deps) {
  try {
    await deps.ungroup(tabId);
  } catch {
    // Not grouped, or no tab-group API: removing it still closes the tab.
  }
  await deps.remove(tabId);
}
