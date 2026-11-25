import { RELAY_COMMAND_TIMEOUT_MS, isRelayTimeoutError, isScriptEvaluation, relayCommandBudgetMs, withRelayTimeout } from './relay-timeout.js'
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

// A failed reset is not final: the attachment may recover once the page
// settles. Later commands within this window get the cached failure; the first
// one after it starts a new reset (#903).
export const RESET_RETRY_BACKOFF_MS = 5000

// An explicit reload or navigation is how a caller escapes a broken page, so it
// starts a new reset at once instead of waiting out the backoff (#903).
export function retriesResetNow(method) {
  return method === 'Page.reload' || method === 'Page.navigate'
}

// A relay timeout marks the attachment unhealthy, except for a page script that
// was merely slow: its timeout says nothing about the debugger (#903).
export function markTimedOut(health, tabId, method) {
  if (!health || isScriptEvaluation(method)) return false
  health.mark(tabId)
  return true
}

// Kept per relay (and injectable in tests), never a global cross-tab queue.
export function createAttachmentHealth({ now = () => Date.now(), retryBackoffMs = RESET_RETRY_BACKOFF_MS } = {}) {
  const unhealthy = new Set()
  const recovering = new Map()
  // tabId -> { error, at }: the last failed reset and when it failed.
  const failed = new Map()
  return {
    mark(tabId) { unhealthy.add(tabId) },
    isUnhealthy(tabId) { return unhealthy.has(tabId) },
    isRecovering(tabId) { return recovering.has(tabId) },
    // A requested navigation can remove the document that refused recovery.
    // Keep its unhealthy flag: the next debugger command must still recover.
    navigationStarted(tabId) {
      failed.delete(tabId)
      const token = recovering.get(tabId)
      if (token) token.cacheFailure = false
    },
    // Dropping an attachment also invalidates pending work for that attachment.
    clear(tabId) {
      unhealthy.delete(tabId)
      failed.delete(tabId)
      const token = recovering.get(tabId)
      if (token) token.active = false
      recovering.delete(tabId)
    },
    // `method` is the command waiting on the reset; a reload or navigation
    // retries at once.
    async recover(tabId, deps, { method } = {}) {
      const last = failed.get(tabId)
      if (last) {
        const retryNow = retriesResetNow(method)
        // Synchronous up to setting `recovering`, so concurrent commands after
        // the backoff share one new attempt.
        if (!retryNow && now() - last.at < retryBackoffMs) throw last.error
        failed.delete(tabId)
      }
      if (!unhealthy.has(tabId)) return false
      if (!recovering.has(tabId)) {
        const token = { active: true, promise: null, cacheFailure: true }
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
              `It is retried on a later command after ${Math.round(retryBackoffMs / 1000)} s; reload or navigate the tab to retry now. ${error.message}`)
            if (isCurrent() && token.cacheFailure) failed.set(tabId, { error: failure, at: now() })
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

// Screenshot teardown must not reset a timed-out capture's attachment. Only
// clearing the compositor background is cleanup; setting a color is ordinary work.
export function isAttachmentCleanup(method, params) {
  return method === 'Emulation.setDefaultBackgroundColorOverride' &&
    params !== null && typeof params === 'object' && !Array.isArray(params) &&
    Object.keys(params).length === 0
}

/**
 * Dispatch to one debugger target. A child-frame failure must not tear down its
 * parent: a restricted OOPIF can fail while the top-level page is healthy, and
 * child session ids cannot be reused after a parent reattachment.
 */
export async function sendTabCommand(tabId, method, params, childSessionId, deps) {
  const cleanup = isAttachmentCleanup(method, params)
  const reset = cleanup ? false
    : await deps.health?.recover(tabId, deps, { method })
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
      markTimedOut(deps.health, tabId, method)
      throw e
    }
    if (isDebuggerAccessDenied(e)) {
      // Candidate lookup is diagnostic only; its failure must not hide Chrome's denial.
      let candidates = []
      try { candidates = await deps.foreignFrameExtensions?.() ?? [] } catch {}
      throw debuggerAccessError(e, candidates)
    }
    // A cleanup error cannot justify moving or resetting the target.
    if (cleanup || childSessionId) throw e
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
    if (recoveredTabId == null) {
      // The entry was dropped above without telling the host. If the tab is
      // gone, its host record must go too, or it lists a dead page (#519).
      void Promise.resolve(deps.reportTabIfGone?.(tabId)).catch(() => {})
      throw e
    }
    try {
      return await withRelayTimeout(
        deps.sendCommand({ tabId: recoveredTabId }, method, params),
        `chrome.debugger.sendCommand(${method}) retry`,
        deps.commandTimeoutMs ?? relayCommandBudgetMs(method, params),
        { payloadScaled: method === 'Input.insertText' && relayCommandBudgetMs(method, params) !== RELAY_COMMAND_TIMEOUT_MS },
      )
    } catch (retryError) {
      if (isRelayTimeoutError(retryError)) markTimedOut(deps.health, recoveredTabId, method)
      // The initial attempt may have been rejected before dispatch, but the
      // attempt after reattachment can execute before losing its response.
      if (!REPEATABLE_COMMAND.test(method)) throw unconfirmedActionError(method, retryError)
      throw retryError
    }
  }
}
