// Testable native Duplicate tab transaction. The service worker supplies Chrome
// adapters; tests supply deterministic fakes at the same system boundary.
const DEFAULT_TRANSACTION_TIMEOUT_MS = 5000
const DEFAULT_CLEANUP_TIMEOUT_MS = 2000
const TIMED_OUT = Symbol('timed-out')

async function withTimeout(operation, timeoutMs) {
  let timer
  try {
    return await Promise.race([
      Promise.resolve().then(operation),
      new Promise((resolve) => {
        timer = setTimeout(() => resolve(TIMED_OUT), timeoutMs)
      }),
    ])
  } finally {
    clearTimeout(timer)
  }
}

async function bestEffort(operation, timeoutMs) {
  try {
    await withTimeout(operation, timeoutMs)
  } catch {}
}

async function observeWithin(operation, timeoutMs) {
  if (timeoutMs <= 0) return false
  try {
    const result = await withTimeout(operation, timeoutMs)
    return result !== TIMED_OUT && Boolean(result)
  } catch {
    return false
  }
}

function remainingTime(deadline) {
  return Math.max(0, deadline - Date.now())
}

async function completeBefore(operation, deadline, stage) {
  const timeoutMs = remainingTime(deadline)
  if (timeoutMs === 0) throw new Error(`duplicateTab: ${stage} timed out`)
  const result = await withTimeout(operation, timeoutMs)
  if (result !== TIMED_OUT) return result
  throw new Error(`duplicateTab: ${stage} timed out`)
}

async function settleOrVerify(operation, verify, deadline, operationTimeoutMs, stage) {
  try {
    await completeBefore(
      operation,
      Math.min(deadline, Date.now() + operationTimeoutMs),
      stage,
    )
  } catch (error) {
    if (await observeWithin(verify, remainingTime(deadline))) return
    throw error
  }
}

async function restoreForegroundBestEffort(deps, tabId, windowId, timeoutMs) {
  await bestEffort(() => deps.activateTab(tabId), timeoutMs)
  const focused = await observeWithin(
    () => deps.getWindow(windowId).then((window) => window?.focused === true),
    timeoutMs,
  )
  if (!focused) await bestEffort(() => deps.focusWindow(windowId), timeoutMs)
}

export async function duplicateTab(params, deps) {
  const sourceTargetId =
    typeof params?.sourceTargetId === 'string' ? params.sourceTargetId.trim() : ''
  const sourceTabId = sourceTargetId ? deps.tabForTarget(sourceTargetId) : null
  if (sourceTabId == null) {
    throw new Error(`duplicateTab: source target not found: ${sourceTargetId}`)
  }

  const sourceTab = await deps.getTab(sourceTabId).catch(() => null)
  if (!deps.eligible(sourceTab)) throw new Error('duplicateTab: source tab is unavailable')
  const focusedWindow = await deps.getLastFocusedWindow().catch(() => null)
  const restoreWindowId = focusedWindow?.id ?? sourceTab.windowId
  const activeTabs = await deps.getActiveTabs(restoreWindowId).catch(() => [])
  const restoreTabId = activeTabs[0]?.id ?? sourceTabId
  const group = typeof params?.agentGroup === 'string' ? params.agentGroup.trim() : ''
  if (!group) throw new Error('duplicateTab: agentGroup is required')

  const cleanupTimeoutMs = deps.cleanupTimeoutMs ?? DEFAULT_CLEANUP_TIMEOUT_MS
  const transactionTimeoutMs = deps.transactionTimeoutMs ?? DEFAULT_TRANSACTION_TIMEOUT_MS
  const transactionDeadline = Date.now() + transactionTimeoutMs
  let transactionActive = true
  let duplicateTabId = null
  const duplicatePromise = Promise.resolve().then(() => deps.duplicateTab(sourceTabId))
  try {
    const duplicate = await completeBefore(
      () => duplicatePromise,
      transactionDeadline,
      'native duplicate',
    )
    duplicateTabId = duplicate?.id ?? null
    if (duplicateTabId == null) throw new Error('duplicateTab: no tab id')
    deps.markOwned(duplicateTabId)
    await completeBefore(
      () => deps.groupTabInto(duplicateTabId, group),
      transactionDeadline,
      'tab grouping',
    )
    const entry = await completeBefore(
      () => deps.attachTab(duplicateTabId, () => transactionActive),
      transactionDeadline,
      'debugger attach',
    )
    await settleOrVerify(
      () => deps.activateTab(restoreTabId),
      () => deps.getTab(restoreTabId).then((tab) => tab?.active === true),
      transactionDeadline,
      cleanupTimeoutMs,
      'foreground tab restore',
    )
    const windowIsFocused = () =>
      deps.getWindow(restoreWindowId).then((window) => window?.focused === true)
    if (!(await observeWithin(windowIsFocused, remainingTime(transactionDeadline)))) {
      await settleOrVerify(
        () => deps.focusWindow(restoreWindowId),
        windowIsFocused,
        transactionDeadline,
        cleanupTimeoutMs,
        'foreground window restore',
      )
    }
    deps.completeTab(duplicateTabId)
    return { sourceTargetId, targetId: entry.targetId }
  } catch (error) {
    transactionActive = false
    if (duplicateTabId != null) {
      await bestEffort(() => deps.unmarkOwned(duplicateTabId), cleanupTimeoutMs)
      await bestEffort(() => deps.isolateTab(duplicateTabId), cleanupTimeoutMs)
      await bestEffort(() => deps.removeTab(duplicateTabId), cleanupTimeoutMs)
    } else {
      void duplicatePromise.then(
        async (duplicate) => {
          const lateTabId = duplicate?.id ?? null
          if (lateTabId == null) return
          await bestEffort(() => deps.isolateTab(lateTabId), cleanupTimeoutMs)
          await bestEffort(() => deps.removeTab(lateTabId), cleanupTimeoutMs)
        },
        () => {},
      )
    }
    await restoreForegroundBestEffort(
      deps,
      restoreTabId,
      restoreWindowId,
      cleanupTimeoutMs,
    )
    throw error
  }
}
