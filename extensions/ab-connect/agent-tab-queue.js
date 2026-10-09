import { agentTabPredicate, agentWindowStillOurs, isUntouchedPlaceholder, placeholderCleanup } from './agent-window.js'
import { RELAY_COMMAND_TIMEOUT_MS, withRelayTimeout } from './relay-timeout.js'

// Every Chrome await belongs to a request token. Expiry stops continuations,
// while late resource creation is reconciled independently of the queue.
export function createAgentTabQueue(deps) {
  const { chrome, windowKey, placeholderKey } = deps
  const budget = deps.timeoutMs ?? RELAY_COMMAND_TIMEOUT_MS
  let chain = Promise.resolve()
  let persistedWindow
  const report = error => deps.onCleanupError?.(error)
  const bounded = (operation, label) => withRelayTimeout(operation, `createTarget: ${label}`, budget)
  const adopted = id => deps.isAdopted(id)
  async function discard(tab) {
    if (tab?.id == null || adopted(tab.id)) return
    await bounded(chrome.tabs.remove(tab.id), 'remove expired tab')
  }
  async function discardWindow(win) {
    // Never remove a whole window: it may now contain the user's tabs.
    for (const tab of win?.tabs || []) await discard(tab)
  }
  function create(url) {
    const run = chain.then(async () => {
      const tx = { active: true }
      let created
      let committed = false
      async function step(label, start, late) {
        if (!tx.active) throw new Error('createTarget: expired transaction')
        const operation = Promise.resolve().then(start)
        operation.then(value => {
          if (!tx.active && late) Promise.resolve(late(value)).catch(report)
        }, () => {})
        try {
          return await bounded(operation, label)
        } catch (error) {
          tx.active = false
          throw error
        }
      }
      const optional = (label, start) => step(label, () => Promise.resolve().then(start).catch(() => null))
      async function writeWindow(value) {
        persistedWindow = value
        const reconcile = async () => {
          if (persistedWindow !== value) {
            await bounded(chrome.storage.local.set(persistedWindow), 'reconcile window record')
          }
        }
        await step('persist agent window', () => chrome.storage.local.set(value), reconcile)
      }
      async function record() {
        const got = await optional('read placeholder', () => chrome.storage.local.get(placeholderKey))
        const rec = got?.[placeholderKey]
        return Number.isInteger(rec?.windowId) && Number.isInteger(rec?.tabId) ? rec : null
      }
      async function usable(id) {
        const win = await optional('get agent window', () => chrome.windows.get(id))
        await step('load tab ownership', deps.loadOwnedTabs)
        const tabs = win ? await optional('query agent window', () => chrome.tabs.query({ windowId: id })) : null
        const rec = await record()
        await step('load agent popups', deps.loadAgentPopups)
        const verdict = agentWindowStillOurs(win, tabs,
          agentTabPredicate(deps.ownedTabs, deps.agentPopups), rec)
        if (!verdict.ours) deps.rejectWindow({ windowId: id, reason: verdict.reason })
        if (rec?.windowId === id && Array.isArray(tabs) &&
            !isUntouchedPlaceholder(tabs.find(t => t?.id === rec.tabId), rec)) {
          // Keep the window being validated: after a worker restart the
          // in-memory id is still null while the persisted window is checked.
          await writeWindow({ [windowKey]: id, [placeholderKey]: null })
        }
        return verdict.ours
      }
      try {
        let winId = deps.getWindowId()
        if (winId != null && !await usable(winId)) {
          deps.setWindowId(null)
          winId = null
          await writeWindow({ [windowKey]: null, [placeholderKey]: null })
        }
        if (winId == null) {
          const got = await step('read agent window', () => chrome.storage.local.get(windowKey))
          const saved = got?.[windowKey]
          if (saved != null && await usable(saved)) winId = saved
        }
        if (winId == null) {
          if (!chrome.windows?.create) throw new Error('createTarget: chrome.windows API is unavailable')
          const win = await step('create agent window',
            () => chrome.windows.create({ focused: false, url: 'about:blank' }), discardWindow)
          if (win?.id == null) throw new Error('createTarget: chrome.windows.create returned no window')
          winId = win.id
          // Publish only from the still-current transaction, never a late callback.
          deps.setWindowId(winId)
          const placeholder = win.tabs?.[0]?.id
          await writeWindow({
            [windowKey]: winId,
            [placeholderKey]: Number.isInteger(placeholder) ? { windowId: winId, tabId: placeholder } : null,
          })
        }
        deps.setWindowId(winId)
        created = await step('create tab', () => chrome.tabs.create({ url, active: false, windowId: winId }), discard)
        if (created?.id == null) throw new Error('createTarget: chrome.tabs.create returned no tab')
        await step('load tab ownership', deps.loadOwnedTabs)
        // No await between the token check and the in-memory ownership commit.
        deps.ownedTabs.add(created.id)
        committed = true
        await step('persist tab ownership', deps.persistOwnedTabs,
          () => bounded(deps.persistOwnedTabs(), 'reconcile tab ownership'))
        const rec = await record()
        if (rec?.windowId === winId) {
          const tab = await optional('get placeholder', () => chrome.tabs.get(rec.tabId))
          if (placeholderCleanup(tab, rec, winId) === 'remove') {
            await optional('remove placeholder', () => chrome.tabs.remove(rec.tabId))
          }
          await writeWindow({ [windowKey]: winId, [placeholderKey]: null })
        }
        return created
      } catch (error) {
        tx.active = false
        // A tab published as owned can already have been adopted by another
        // request. Leave it discoverable; only uncommitted resources are reaped.
        if (created && !committed) void discard(created).catch(report)
        deps.onError?.(String(error.message || error))
        throw error
      } finally {
        tx.active = false
      }
    })
    chain = run.catch(() => {})
    return run
  }
  return create
}
