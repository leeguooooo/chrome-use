import { RELAY_COMMAND_TIMEOUT_MS, isRelayTimeoutError, relayCommandBudgetMs, withRelayTimeout } from './relay-timeout.js'
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

// Kept per relay (and injectable in tests), never a global cross-tab queue.
export function createAttachmentHealth() {
  const unhealthy = new Set()
  const recovering = new Map()
  const failed = new Map()
  return {
    mark(tabId) { unhealthy.add(tabId) },
    isRecovering(tabId) { return recovering.has(tabId) },
    // Dropping an attachment also invalidates pending work for that attachment.
    clear(tabId) {
      unhealthy.delete(tabId)
      failed.delete(tabId)
      const token = recovering.get(tabId)
      if (token) token.active = false
      recovering.delete(tabId)
    },
    async recover(tabId, deps) {
      if (failed.has(tabId)) throw failed.get(tabId)
      if (!unhealthy.has(tabId)) return false
      if (!recovering.has(tabId)) {
        const token = { active: true, promise: null }
        const isCurrent = () => token.active && recovering.get(tabId) === token
        const operation = (async () => {
          try {
            await deps.detachDebugger(tabId)
          } catch (error) {
            // Only Chrome's explicit absent-attachment error confirms this step.
            if (!/^Debugger is not attached to (?:the )?tab\b/i.test(String(error?.message || error))) throw error
          }
          if (!isCurrent()) throw new Error('recovery expired')
          // The reset drops session maps but keeps this recovery token alive.
          deps.detachTab(tabId, false, true)
          await deps.attachTab(tabId, isCurrent)
          if (!isCurrent()) throw new Error('recovery expired')
        })()
        const recovery = withRelayTimeout(operation, `debugger recovery for tab ${tabId}`,
          deps.recoveryTimeoutMs ?? RELAY_COMMAND_TIMEOUT_MS).then(() => {
            if (!isCurrent()) throw new Error('recovery expired')
            unhealthy.delete(tabId)
          }).catch(error => {
            const failure = new Error(`tab_reset_failed: tab ${tabId} was reset; debugger recovery could not be confirmed. ` +
              `Reopen the tab before retrying. ${error.message}`)
            if (isCurrent()) failed.set(tabId, failure)
            throw failure
          }).finally(() => {
            token.active = false
            if (recovering.get(tabId) === token) recovering.delete(tabId)
          })
        token.promise = recovery
        recovering.set(tabId, token)
      }
      await recovering.get(tabId).promise
      return true
    },
  }
}

/**
 * Dispatch to one debugger target. A child-frame failure must not tear down its
 * parent: a restricted OOPIF can fail while the top-level page is healthy, and
 * child session ids cannot be reused after a parent reattachment.
 */
export async function sendTabCommand(tabId, method, params, childSessionId, deps) {
  const reset = await deps.health?.recover(tabId, deps)
  if (reset && childSessionId) {
    throw new Error('tab_reset: child session invalidated; rediscover frames before retrying')
  }
  const dbg = childSessionId ? { tabId, sessionId: childSessionId } : { tabId }
  try {
    return await withRelayTimeout(
      deps.sendCommand(dbg, method, params),
      `chrome.debugger.sendCommand(${method})`,
      deps.commandTimeoutMs ?? relayCommandBudgetMs(method, params),
      { payloadScaled: method === 'Input.insertText' && relayCommandBudgetMs(method, params) !== RELAY_COMMAND_TIMEOUT_MS },
    )
  } catch (e) {
    if (isRelayTimeoutError(e)) {
      deps.health?.mark(tabId)
      throw e
    }
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
        deps.commandTimeoutMs ?? relayCommandBudgetMs(method, params),
        { payloadScaled: method === 'Input.insertText' && relayCommandBudgetMs(method, params) !== RELAY_COMMAND_TIMEOUT_MS },
      )
    } catch (retryError) {
      if (isRelayTimeoutError(retryError)) deps.health?.mark(recoveredTabId)
      // The initial attempt may have been rejected before dispatch, but the
      // attempt after reattachment can execute before losing its response.
      if (!REPEATABLE_COMMAND.test(method)) throw unconfirmedActionError(method, retryError)
      throw retryError
    }
  }
}
