import { withRelayTimeout } from './relay-timeout.js'

/**
 * Dispatch to one debugger target. A child-frame failure must not tear down its
 * parent: a restricted OOPIF can fail while the top-level page is healthy, and
 * child session ids cannot be reused after a parent reattachment.
 */
export async function sendTabCommand(tabId, method, params, childSessionId, deps) {
  const dbg = childSessionId ? { tabId, sessionId: childSessionId } : { tabId }
  try {
    return await withRelayTimeout(
      deps.sendCommand(dbg, method, params),
      `chrome.debugger.sendCommand(${method})`,
    )
  } catch (e) {
    if (childSessionId) throw e
    const msg = String((e && e.message) || e)
    if (!/detached|not attached|target.*(closed|gone)|no target|cannot access|frame.*detached/i.test(msg)) {
      throw e
    }
    deps.detachTab(tabId, false)
    const recoveredTabId = await deps.recoverSessionTab(`cb-tab-${tabId}`)
    if (recoveredTabId == null) throw e
    return await withRelayTimeout(
      deps.sendCommand({ tabId: recoveredTabId }, method, params),
      `chrome.debugger.sendCommand(${method}) retry`,
    )
  }
}
