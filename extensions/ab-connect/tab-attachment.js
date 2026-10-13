/**
 * Share an ordinary attachment only while its complete workflow is pending.
 * Recovery tokens own cancellation cleanup, so different tokens wait for the
 * prior workflow and then resolve the current tab state independently.
 */
export function createTabAttachmentQueue(attach) {
  const pending = new Map()
  return function attachTab(tabId, transactionIsActive) {
    const previous = pending.get(tabId)
    if (previous && previous.token === transactionIsActive) return previous.promise
    const operation = { token: transactionIsActive, promise: null }
    operation.promise = (async () => {
      if (previous) await previous.promise.catch(() => {})
      if (transactionIsActive && !transactionIsActive())
        throw new Error('attachTab: transaction cancelled')
      return attach(tabId, transactionIsActive)
    })().finally(() => {
      if (pending.get(tabId) === operation) pending.delete(tabId)
    })
    pending.set(tabId, operation)
    return operation.promise
  }
}
