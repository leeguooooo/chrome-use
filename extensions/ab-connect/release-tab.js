// ABExt.releaseTab (0.5.34): let go of a tab the daemon took with `--force`
// (the user's own tab, or another session's) when its session ends. The
// session never closes such a tab; without this the debugger stayed attached
// (and Chrome's "is debugging this browser" bar stayed on the user's page)
// until the service worker restarted.
//
// Only a tab the extension did NOT create is released: an agent-created tab
// belongs to the session that made it, which closes it itself. Kept free of
// `chrome.*` globals so the rule is unit-testable.
//
// deps: { tabForTarget(targetId) -> tabId|null, isOwned(tabId) -> bool,
//         detach(tabId) -> Promise, forget(tabId) -> void }

/**
 * Resolves to `{ released: true, tabId }`, or `{ released: false, reason }`
 * with reason `bad-request`, `not-attached`, `agent-owned` or Chrome's error.
 */
export async function releaseTab(params, deps) {
  const targetId = typeof params?.targetId === 'string' ? params.targetId : '';
  if (!targetId) return { released: false, reason: 'bad-request' };
  const tabId = deps.tabForTarget(targetId);
  if (tabId == null) return { released: false, reason: 'not-attached' };
  if (deps.isOwned(tabId)) return { released: false, tabId, reason: 'agent-owned' };
  try {
    await deps.detach(tabId);
  } catch (e) {
    const message = String((e && e.message) || e);
    // Already detached (the user cancelled the bar, DevTools took it): the
    // tab is released all the same.
    if (!/not attached|no debugger|is not being debugged/i.test(message)) {
      return { released: false, tabId, reason: message };
    }
  }
  deps.forget(tabId);
  return { released: true, tabId };
}
