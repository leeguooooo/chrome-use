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
        const got = await step('read placeholder', () => chrome.storage.local.get(placeholderKey))
        const rec = got?.[placeholderKey]
        return Number.isInteger(rec?.windowId) && Number.isInteger(rec?.tabId) ? rec : null
      }
      async function usable(id) {
        const win = await step('get agent window', () => chrome.windows.get(id).catch(error => {
          if (/no window with id|invalid window id/i.test(String(error.message || error))) return null
          throw error
        }))
        await step('load tab ownership', deps.loadOwnedTabs)
        const tabs = win ? await step('query agent window', () => chrome.tabs.query({ windowId: id })) : null
        const rec = await record()
        await step('load agent popups', deps.loadAgentPopups)
        const isAgent = agentTabPredicate(deps.ownedTabs, deps.agentPopups)
        const verdict = agentWindowStillOurs(win, tabs, isAgent, rec)
        if (!verdict.ours) deps.rejectWindow({ windowId: id, reason: verdict.reason })
        if (['tabs-unknown', 'tab-unknown', 'contradictory'].includes(verdict.reason))
          throw new Error('createTarget: agent window ownership is unknown; refusing to create another window')
        const placeholder = tabs?.find(tab => tab.id === rec?.tabId)
        if (placeholder && placeholderCleanup(placeholder, rec, id) === 'keep')
          throw new Error('createTarget: placeholder navigation is unconfirmed; refusing to create another window')
        // A user tab makes the window the user's. Agent tabs already in it stay
        // owned and keep working; new agent tabs go to a fresh shared window.
        // That adds at most one window each time the user mixes tabs in.
        if (!verdict.ours) return false
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
          // Recover an existing all-agent window before creating another one.
          // A blank/user tab is never evidence of ownership, even in an old
          // window that once held an agent session.
          const windows = await step('find agent windows', () => chrome.windows.getAll({ windowTypes: ['normal'] }))
          await step('load tab ownership', deps.loadOwnedTabs)
          await step('load agent popups', deps.loadAgentPopups)
          const isAgent = agentTabPredicate(deps.ownedTabs, deps.agentPopups)
          for (const win of windows) {
            const candidates = await step('query candidate window', () => chrome.tabs.query({ windowId: win.id }))
            if (candidates.some(tab => isAgent(tab.id)) && await usable(win.id)) {
              winId = win.id
              await writeWindow({ [windowKey]: winId, [placeholderKey]: await record() })
              break
            }
          }
        }
        if (winId == null) {
          if (!chrome.windows?.create) throw new Error('createTarget: chrome.windows API is unavailable')
          const win = await step('create agent window',
            () => chrome.windows.create({ focused: false, url }), discardWindow)
          if (win?.id == null) throw new Error('createTarget: chrome.windows.create returned no window')
          winId = win.id
          // The window's initial tab is the requested agent page itself.
          // There is no extra blank placeholder to leak or misclassify.
          created = win.tabs?.[0]
          if (!Number.isInteger(created?.id) || created.windowId !== winId)
            throw new Error(`createTarget: window ${winId} was created but its initial tab identity is unknown; inspect tab list --all before retrying`)
          await step('load tab ownership', deps.loadOwnedTabs)
          deps.ownedTabs.add(created.id)
          committed = true
          await step('persist tab ownership', deps.persistOwnedTabs,
            () => bounded(deps.persistOwnedTabs(), 'reconcile tab ownership'))
          // Publish only from the still-current transaction, never a late callback.
          deps.setWindowId(winId)
          await writeWindow({
            [windowKey]: winId,
            [placeholderKey]: null,
          })
        }
        deps.setWindowId(winId)
        created ??= await step('create tab', () => chrome.tabs.create({ url, active: false, windowId: winId }), discard)
        if (created?.id == null) throw new Error('createTarget: chrome.tabs.create returned no tab')
        await step('load tab ownership', deps.loadOwnedTabs)
        // No await between the token check and the in-memory ownership commit.
        deps.ownedTabs.add(created.id)
        committed = true
        await step('persist tab ownership', deps.persistOwnedTabs,
          () => bounded(deps.persistOwnedTabs(), 'reconcile tab ownership'))
        const rec = await record()
        if (rec?.windowId === winId) {
          // A failed read or removal must keep the record for the next call.
          // Forgetting it would make our own blank page look like a user tab,
          // causing every later session to abandon this window.
          const current = await step('query placeholder', () => chrome.tabs.query({ windowId: winId }))
          const tab = current.find(tab => tab.id === rec.tabId)
          const cleanup = placeholderCleanup(tab, rec, winId)
          if (cleanup === 'keep')
            throw new Error('createTarget: placeholder navigation is unconfirmed; its record was retained')
          if (cleanup === 'remove') {
            await step('remove placeholder', () => chrome.tabs.remove(rec.tabId))
            const remaining = await step('verify placeholder removal', () => chrome.tabs.query({ windowId: winId }))
            if (remaining.some(tab => tab.id === rec.tabId))
              throw new Error('createTarget: placeholder removal is unconfirmed; its record was retained')
          }
          await writeWindow({ [windowKey]: winId, [placeholderKey]: null })
        }
        return created
      } catch (error) {
        tx.active = false
        // A tab published as owned can already have been adopted by another
        // request. Leave it discoverable; only uncommitted resources are reaped.
        if (created && !committed) void discard(created).catch(report)
        const failure = created && committed
          ? new Error(`createTarget: agent tab ${created.id} was created in window ${created.windowId}, but setup is incomplete (${error.message || error}); its ownership was retained; inspect tab list --all before retrying`)
          : error
        deps.onError?.(String(failure.message || failure))
        throw failure
      } finally {
        tx.active = false
      }
    })
    chain = run.catch(() => {})
    return run
  }
  return create
}
