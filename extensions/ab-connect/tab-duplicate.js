// Testable native Duplicate tab transaction. The service worker supplies Chrome
// adapters; tests supply deterministic fakes at the same system boundary.
const DEFAULT_TRANSACTION_TIMEOUT_MS = 5000
const DEFAULT_CLEANUP_TIMEOUT_MS = 2000
const TIMED_OUT = Symbol('timed-out')

// Every deadline below reads time through this clock. Production uses the real
// one; tests inject a fake (`deps.now`, `deps.setTimeout`, `deps.clearTimeout`)
// and advance it explicitly, so no test depends on how busy the machine is.
function clockFrom(deps) {
  return {
    now: deps?.now ?? (() => Date.now()),
    setTimeout: deps?.setTimeout ?? ((callback, ms) => setTimeout(callback, ms)),
    clearTimeout: deps?.clearTimeout ?? ((timer) => clearTimeout(timer)),
  }
}

async function withTimeout(clock, operation, timeoutMs) {
  let timer
  try {
    return await Promise.race([
      Promise.resolve().then(operation),
      new Promise((resolve) => {
        timer = clock.setTimeout(() => resolve(TIMED_OUT), timeoutMs)
      }),
    ])
  } finally {
    clock.clearTimeout(timer)
  }
}

async function bestEffort(clock, operation, timeoutMs) {
  try {
    await withTimeout(clock, operation, timeoutMs)
  } catch {}
}

async function observeWithin(clock, operation, timeoutMs) {
  if (timeoutMs <= 0) return false
  try {
    const result = await withTimeout(clock, operation, timeoutMs)
    return result !== TIMED_OUT && Boolean(result)
  } catch {
    return false
  }
}

function remainingTime(clock, deadline) {
  return Math.max(0, deadline - clock.now())
}

async function completeBefore(clock, operation, deadline, stage) {
  const timeoutMs = remainingTime(clock, deadline)
  // An expired deadline has to stop the stage before it starts. `withTimeout`
  // with 0ms does not: an operation that settles in a microtask still beats a
  // 0ms timer, so a side-effecting stage (group, activate) could run after the
  // transaction was already over. #342 dropped this line; restored.
  if (timeoutMs === 0) throw new Error(`duplicateTab: ${stage} timed out`)
  const result = await withTimeout(clock, operation, timeoutMs)
  if (result !== TIMED_OUT) return result
  // A timer can fire while `now()` still reads short of the deadline it
  // was set for. The next stage then saw a millisecond left, passed the check
  // above and ran. Wait the difference out so a timed-out stage always leaves
  // the deadline expired.
  while (clock.now() < deadline) await new Promise((resolve) => clock.setTimeout(resolve, 1))
  throw new Error(`duplicateTab: ${stage} timed out`)
}

async function settleOrVerify(clock, operation, verify, deadline, operationTimeoutMs, stage) {
  try {
    await completeBefore(
      clock,
      operation,
      Math.min(deadline, clock.now() + operationTimeoutMs),
      stage,
    )
  } catch (error) {
    if (
      await observeWithin(
        clock,
        verify,
        Math.max(remainingTime(clock, deadline), Math.min(operationTimeoutMs, 25)),
      )
    )
      return
    throw error
  }
}

async function restoreForegroundBestEffort(clock, deps, tabId, windowId, wasFocused, deadline) {
  await bestEffort(clock, () => deps.activateTab(tabId), remainingTime(clock, deadline))
  if (!wasFocused) return
  const focused = await observeWithin(
    clock,
    () => deps.getWindow(windowId).then((window) => window?.focused === true),
    remainingTime(clock, deadline),
  )
  if (!focused) {
    await bestEffort(clock, () => deps.focusWindow(windowId), remainingTime(clock, deadline))
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

  const transactionTimeoutMs = deps.transactionTimeoutMs ?? DEFAULT_TRANSACTION_TIMEOUT_MS
  const cleanupTimeoutMs = deps.cleanupTimeoutMs ?? DEFAULT_CLEANUP_TIMEOUT_MS
  const clock = clockFrom(deps)
  const transactionDeadline = clock.now() + transactionTimeoutMs
  let transactionActive = true

  // These three reads are bounded by the transaction deadline (#342 — before,
  // a read that never settled held the whole duplicate) and still degrade the
  // way they did before #342: a missing tab is "not eligible", and without a
  // focused window or an active tab the restore target falls back to the
  // source. #342 let any of them reject the whole duplicate instead.
  const sourceTab = await completeBefore(
    clock,
    () => deps.getTab(sourceTabId),
    transactionDeadline,
    'source tab inspection',
  ).catch(() => null)
  if (!deps.eligible(sourceTab)) {
    throw new Error(`duplicateTab: tab ${sourceTabId} is not eligible`)
  }

  const lastFocusedWindow = await completeBefore(
    clock,
    () => deps.getLastFocusedWindow(),
    transactionDeadline,
    'window inspection',
  ).catch(() => null)
  const restoreWindowId = lastFocusedWindow?.id ?? sourceTab.windowId
  const restoreWindowFocused = lastFocusedWindow?.focused === true
  const activeTabs = await completeBefore(
    clock,
    () => deps.getActiveTabs(restoreWindowId),
    transactionDeadline,
    'active tab inspection',
  ).catch(() => [])
  const restoreTabId = activeTabs?.[0]?.id ?? sourceTabId

  let duplicateTabId = null
  const duplicatePromise = Promise.resolve().then(() => deps.duplicateTab(sourceTabId))
  try {
    const duplicate = await completeBefore(
      clock,
      () => duplicatePromise,
      transactionDeadline,
      'native duplicate',
    )
    duplicateTabId = duplicate?.id ?? null
    if (duplicateTabId == null) throw new Error('duplicateTab: no tab id')
    await deps.markOwned(duplicateTabId)
    await completeBefore(
      clock,
      () => deps.groupTabInto(duplicateTabId, group),
      transactionDeadline,
      'tab grouping',
    )
    const entry = await completeBefore(
      clock,
      () => deps.attachTab(duplicateTabId, () => transactionActive),
      transactionDeadline,
      'debugger attach',
    )
    await settleOrVerify(
      clock,
      () => deps.activateTab(restoreTabId),
      () => deps.getTab(restoreTabId).then((tab) => tab?.active === true),
      transactionDeadline,
      cleanupTimeoutMs,
      'foreground tab restore',
    )
    const windowIsFocused = () =>
      deps.getWindow(restoreWindowId).then((window) => window?.focused === true)
    // Give focus back only if that window had it. When the user is in another
    // app no Chrome window is focused, and "restoring" focus would raise Chrome
    // over the app they are working in.
    if (
      restoreWindowFocused &&
      !(await observeWithin(clock, windowIsFocused, remainingTime(clock, transactionDeadline)))
    ) {
      await settleOrVerify(
        clock,
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
    const cleanupDeadline = clock.now() + cleanupTimeoutMs
    if (duplicateTabId != null) {
      await bestEffort(
        clock,
        () => deps.unmarkOwned(duplicateTabId),
        remainingTime(clock, cleanupDeadline),
      )
      await bestEffort(
        clock,
        () => deps.isolateTab(duplicateTabId),
        remainingTime(clock, cleanupDeadline),
      )
      await bestEffort(
        clock,
        () => deps.removeTab(duplicateTabId),
        remainingTime(clock, cleanupDeadline),
      )
    } else {
      void duplicatePromise.then(
        async (duplicate) => {
          const lateTabId = duplicate?.id ?? null
          if (lateTabId == null) return
          const lateCleanupDeadline = clock.now() + cleanupTimeoutMs
          await bestEffort(
            clock,
            () => deps.isolateTab(lateTabId),
            remainingTime(clock, lateCleanupDeadline),
          )
          await bestEffort(
            clock,
            () => deps.removeTab(lateTabId),
            remainingTime(clock, lateCleanupDeadline),
          )
        },
        () => {},
      )
    }
    await restoreForegroundBestEffort(
      clock,
      deps,
      restoreTabId,
      restoreWindowId,
      restoreWindowFocused,
      cleanupDeadline,
    )
    throw error
  }
}
