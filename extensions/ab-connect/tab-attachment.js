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

/** Retry only a transient attachment of a tab already created by this command. */
export async function attachCreatedTab(tabId, { attach, wait, permanent }) {
  const delays = [0, 25, 50, 100]
  for (let attempt = 0; attempt < delays.length; attempt++) {
    if (delays[attempt]) await wait(delays[attempt])
    try {
      return await attach(tabId)
    } catch (error) {
      if (permanent(error) || attempt === delays.length - 1) throw error
    }
  }
}
