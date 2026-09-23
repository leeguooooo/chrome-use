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
    if (
      await observeWithin(
        verify,
        Math.max(remainingTime(deadline), Math.min(operationTimeoutMs, 25)),
      )
    )
      return
    throw error
  }
}

async function restoreForegroundBestEffort(deps, tabId, windowId, deadline) {
  await bestEffort(() => deps.activateTab(tabId), remainingTime(deadline))
  const focused = await observeWithin(
    () => deps.getWindow(windowId).then((window) => window?.focused === true),
    remainingTime(deadline),
  )
  if (!focused) {
    await bestEffort(() => deps.focusWindow(windowId), remainingTime(deadline))
  }
}

export async function duplicateTab(params, deps) {
  const sourceTargetId =
    typeof params?.sourceTargetId === 'string' ? params.sourceTargetId.trim() : ''
  if (!sourceTargetId) throw new Error('duplicateTab: no sourceTargetId')
  const group = typeof params?.agentGroup === 'string' ? params.agentGroup.trim() : ''
  if (!group) throw new Error('duplicateTab: no agentGroup')
  const sourceTabId = deps.tabForTarget(sourceTargetId)
  if (sourceTabId == null) throw new Error(`duplicateTab: unknown target ${sourceTargetId}`)

  const transactionTimeoutMs = deps.transactionTimeoutMs ?? 5000
  const cleanupTimeoutMs = deps.cleanupTimeoutMs ?? 2000
  const transactionDeadline = Date.now() + transactionTimeoutMs
  let transactionActive = true

  const sourceTab = await completeBefore(
    () => deps.getTab(sourceTabId),
    transactionDeadline,
    'source tab inspection',
  )
  if (!deps.eligible(sourceTab)) {
    throw new Error(`duplicateTab: tab ${sourceTabId} is not eligible`)
  }

  const lastFocusedWindow = await completeBefore(
    deps.getLastFocusedWindow,
    transactionDeadline,
    'window inspection',
  )
  const restoreWindowId = lastFocusedWindow?.id ?? sourceTab.windowId
  const activeTabs = await completeBefore(
    () => deps.getActiveTabs(restoreWindowId),
    transactionDeadline,
    'active tab inspection',
  )
  const restoreTabId = activeTabs?.[0]?.id ?? sourceTabId

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
    await deps.markOwned(duplicateTabId)
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
    const cleanupDeadline = Date.now() + cleanupTimeoutMs
    if (duplicateTabId != null) {
      await bestEffort(
        () => deps.unmarkOwned(duplicateTabId),
        remainingTime(cleanupDeadline),
      )
      await bestEffort(
        () => deps.isolateTab(duplicateTabId),
        remainingTime(cleanupDeadline),
      )
      await bestEffort(
        () => deps.removeTab(duplicateTabId),
        remainingTime(cleanupDeadline),
      )
    } else {
      void duplicatePromise.then(
        async (duplicate) => {
          const lateTabId = duplicate?.id ?? null
          if (lateTabId == null) return
          const lateCleanupDeadline = Date.now() + cleanupTimeoutMs
          await bestEffort(
            () => deps.isolateTab(lateTabId),
            remainingTime(lateCleanupDeadline),
          )
          await bestEffort(
            () => deps.removeTab(lateTabId),
            remainingTime(lateCleanupDeadline),
          )
        },
        () => {},
      )
    }
    await restoreForegroundBestEffort(
      deps,
      restoreTabId,
      restoreWindowId,
      cleanupDeadline,
    )
    throw error
  }
}
