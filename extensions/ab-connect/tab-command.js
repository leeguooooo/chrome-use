import { withRelayTimeout } from './relay-timeout.js'
import { isDebuggerAccessDenied, debuggerAccessError } from './debugger-access.js'

// Reads and domain subscriptions can be repeated after a transport failure.
// Runtime.evaluate/callFunctionOn and Input commands can change application state;
// a detach acknowledgement does not prove that those actions did not execute.
const REPEATABLE_COMMAND = /^(?:Accessibility\.(?:getFullAXTree|getPartialAXTree|getRootAXNode|queryAXTree)|Page\.(?:getFrameTree|getLayoutMetrics|captureScreenshot)|DOM\.(?:getDocument|getFlattenedDocument|describeNode|resolveNode|requestNode|getBoxModel|getContentQuads|getAttributes|querySelector|querySelectorAll)|Runtime\.getProperties|Browser\.getVersion|Target\.(?:getTargetInfo|setAutoAttach)|[A-Za-z]+\.(?:enable|disable))$/

function unconfirmedActionError(method, error) {
  const message = String(error?.message || error)
  return new Error(
    `action_outcome_unknown: ${method} was not replayed because it may already have executed. ` +
      `Read the current page before deciding whether to repeat the action. Original error: ${message}`,
  )
}

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
    if (isDebuggerAccessDenied(e)) throw debuggerAccessError(e)
    if (childSessionId) throw e
    const msg = String((e && e.message) || e)
    if (!/detached|not attached|target.*(closed|gone)|no target|cannot access|frame.*detached/i.test(msg)) {
      throw e
    }
    const rejectedBeforeDispatch = /Debugger is not attached to (?:the )?tab/i.test(msg)
    if (!rejectedBeforeDispatch && !REPEATABLE_COMMAND.test(method)) {
      throw unconfirmedActionError(method, e)
    }
    deps.detachTab(tabId, false)
    const recoveredTabId = await deps.recoverSessionTab(`cb-tab-${tabId}`)
    if (recoveredTabId == null) throw e
    try {
      return await withRelayTimeout(
        deps.sendCommand({ tabId: recoveredTabId }, method, params),
        `chrome.debugger.sendCommand(${method}) retry`,
      )
    } catch (retryError) {
      // The initial attempt may have been rejected before dispatch, but the
      // attempt after reattachment can execute before losing its response.
      if (!REPEATABLE_COMMAND.test(method)) throw unconfirmedActionError(method, retryError)
      throw retryError
    }
  }
}
