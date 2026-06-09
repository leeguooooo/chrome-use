// agent-browser connect — MV3 service worker.
//
// Bridges the user's real Chrome tabs to the local agent-browser daemon over a
// Chrome **native messaging** channel (no localhost port, no token: Chrome
// authenticates this extension to the host by id). It attaches chrome.debugger
// to eligible tabs and relays CDP both ways via a tiny envelope:
//   host → ext : {id, method:"forwardCDPCommand", params:{method,params,sessionId}}
//   ext → host : {id, result|error}                      (command reply)
//   ext → host : {method:"forwardCDPEvent", params:{sessionId,method,params}}
//
// Target/discovery semantics (getTargets/attachToTarget) are emulated on the
// daemon side; here we just attach tabs and announce them as
// Target.attachedToTarget so the daemon's CDP client sees them appear.
//
// Adapted from openclaw-browser-relay (MIT, chengyixu) — the chrome.debugger
// attach + Target handling; the transport is rewritten from WebSocket+token to
// native messaging.

const HOST_NAME = 'com.agent_browser.connect'
const SKIP_URL = /^(chrome|chrome-extension|devtools|chrome-untrusted|edge|about):/i

/** @type {chrome.runtime.Port|null} */
let port = null
let nextSession = 1
/** tabId -> { sessionId, targetId } */
const tabs = new Map()
/** sessionId -> tabId (main session per tab) */
const sessionToTab = new Map()
/** child (OOPIF/worker) sessionId -> tabId */
const childSessionToTab = new Map()

function postToHost(msg) {
  try {
    if (port) port.postMessage(msg)
  } catch (e) {
    // port died; onDisconnect will reconnect.
  }
}

function setBadge(tabId, kind) {
  const map = { on: '', connecting: '…', error: '!' }
  const colors = { on: '#16a34a', connecting: '#d97706', error: '#b91c1c' }
  try {
    chrome.action.setBadgeText({ tabId, text: map[kind] ?? '' })
    if (colors[kind]) chrome.action.setBadgeBackgroundColor({ tabId, color: colors[kind] })
  } catch {}
}

// ---- native messaging transport ------------------------------------------

function connectHost() {
  if (port) return
  try {
    port = chrome.runtime.connectNative(HOST_NAME)
  } catch (e) {
    port = null
    return
  }
  port.onMessage.addListener((msg) => void whenReady(() => onHostMessage(msg)))
  port.onDisconnect.addListener(() => {
    port = null
    // Sessions are stale once the host is gone; the daemon re-discovers on
    // reconnect. Keep chrome.debugger attached so reconnect is cheap.
    for (const tabId of tabs.keys()) setBadge(tabId, 'connecting')
  })
  // Tell the daemon about everything we already have attached, then attach
  // anything new.
  reannounceAttachedTabs()
  void attachAllTabs()
}

async function onHostMessage(msg) {
  if (!msg || typeof msg !== 'object') return
  // Optional keepalive.
  if (msg.method === 'ping') {
    postToHost({ method: 'pong' })
    return
  }
  if (typeof msg.id !== 'undefined' && msg.method === 'forwardCDPCommand') {
    try {
      const result = await handleForwardCdpCommand(msg)
      postToHost({ id: msg.id, result })
    } catch (err) {
      postToHost({ id: msg.id, error: err instanceof Error ? err.message : String(err) })
    }
  }
}

// ---- CDP command dispatch -------------------------------------------------

function tabForSession(sessionId) {
  return sessionToTab.get(sessionId) ?? childSessionToTab.get(sessionId) ?? null
}

function tabForTarget(targetId) {
  for (const [tabId, t] of tabs.entries()) if (t.targetId === targetId) return tabId
  return null
}

function anyConnectedTab() {
  const it = tabs.keys().next()
  return it.done ? null : it.value
}

async function handleForwardCdpCommand(msg) {
  const method = String(msg?.params?.method || '')
  const params = msg?.params?.params || undefined
  const sessionId = typeof msg?.params?.sessionId === 'string' ? msg.params.sessionId : undefined

  // Browser-level Target methods that map onto chrome.tabs.
  if (method === 'Target.createTarget') {
    const url = typeof params?.url === 'string' && params.url ? params.url : 'about:blank'
    const tab = await chrome.tabs.create({ url, active: false })
    if (!tab.id) throw new Error('createTarget: no tab id')
    await new Promise((r) => setTimeout(r, 100))
    const t = await attachTab(tab.id)
    return { targetId: t.targetId }
  }
  if (method === 'Target.closeTarget') {
    const tid = typeof params?.targetId === 'string' ? params.targetId : ''
    const tabId = tid ? tabForTarget(tid) : null
    if (!tabId) return { success: false }
    try {
      await chrome.tabs.remove(tabId)
    } catch {
      return { success: false }
    }
    return { success: true }
  }
  if (method === 'Target.activateTarget') {
    const tid = typeof params?.targetId === 'string' ? params.targetId : ''
    const tabId = tid ? tabForTarget(tid) : null
    if (tabId) {
      const tab = await chrome.tabs.get(tabId).catch(() => null)
      if (tab?.windowId) await chrome.windows.update(tab.windowId, { focused: true }).catch(() => {})
      await chrome.tabs.update(tabId, { active: true }).catch(() => {})
    }
    return {}
  }

  // Everything else → chrome.debugger on the resolved tab.
  const tabId =
    (sessionId ? tabForSession(sessionId) : null) ??
    (typeof params?.targetId === 'string' ? tabForTarget(params.targetId) : null) ??
    anyConnectedTab()
  if (!tabId) throw new Error(`no attached tab for ${method}`)
  const dbg = { tabId }

  // Re-enabling Runtime can leave a stale state; bounce it (matches upstream).
  if (method === 'Runtime.enable') {
    try {
      await chrome.debugger.sendCommand(dbg, 'Runtime.disable')
      await new Promise((r) => setTimeout(r, 30))
    } catch {}
    return await chrome.debugger.sendCommand(dbg, 'Runtime.enable', params)
  }
  return await chrome.debugger.sendCommand(dbg, method, params)
}

// ---- attach / detach ------------------------------------------------------

async function attachTab(tabId) {
  const existing = tabs.get(tabId)
  if (existing) return existing
  const dbg = { tabId }
  await chrome.debugger.attach(dbg, '1.3')
  await chrome.debugger.sendCommand(dbg, 'Page.enable').catch(() => {})
  const info = /** @type {any} */ (await chrome.debugger.sendCommand(dbg, 'Target.getTargetInfo'))
  const targetInfo = info?.targetInfo
  const targetId = String(targetInfo?.targetId || '')
  if (!targetId) throw new Error('attachTab: no targetId')
  const sessionId = `cb-tab-${nextSession++}`
  const entry = { sessionId, targetId }
  tabs.set(tabId, entry)
  sessionToTab.set(sessionId, tabId)
  setBadge(tabId, port ? 'on' : 'connecting')
  postToHost({
    method: 'forwardCDPEvent',
    params: {
      sessionId,
      method: 'Target.attachedToTarget',
      params: { sessionId, targetInfo: { ...targetInfo, attached: true } },
    },
  })
  return entry
}

function detachTab(tabId, notify) {
  const entry = tabs.get(tabId)
  if (!entry) return
  tabs.delete(tabId)
  sessionToTab.delete(entry.sessionId)
  for (const [sid, tid] of childSessionToTab.entries()) if (tid === tabId) childSessionToTab.delete(sid)
  if (notify) {
    postToHost({
      method: 'forwardCDPEvent',
      params: { sessionId: entry.sessionId, method: 'Target.detachedFromTarget', params: { sessionId: entry.sessionId } },
    })
  }
}

function eligible(tab) {
  return !!tab && !!tab.id && typeof tab.url === 'string' && !SKIP_URL.test(tab.url)
}

async function attachAllTabs() {
  let all = []
  try {
    all = await chrome.tabs.query({})
  } catch {
    return
  }
  for (const tab of all) {
    if (eligible(tab) && !tabs.has(tab.id)) {
      try {
        await attachTab(tab.id)
      } catch {
        // Tab may be a restricted page or already attached elsewhere.
      }
    }
  }
}

function reannounceAttachedTabs() {
  for (const [, entry] of tabs.entries()) {
    postToHost({
      method: 'forwardCDPEvent',
      params: {
        sessionId: entry.sessionId,
        method: 'Target.attachedToTarget',
        params: { sessionId: entry.sessionId, targetInfo: { targetId: entry.targetId, type: 'page', attached: true } },
      },
    })
  }
}

// ---- chrome.debugger events ----------------------------------------------

chrome.debugger.onEvent.addListener((source, method, params) =>
  void whenReady(() => {
    const tabId = source.tabId
    if (!tabId) return
    const entry = tabs.get(tabId)
    if (!entry) return
    if (method === 'Target.attachedToTarget' && params?.sessionId) {
      childSessionToTab.set(String(params.sessionId), tabId)
    }
    if (method === 'Target.detachedFromTarget' && params?.sessionId) {
      childSessionToTab.delete(String(params.sessionId))
    }
    postToHost({
      method: 'forwardCDPEvent',
      params: { sessionId: source.sessionId || entry.sessionId, method, params },
    })
  }),
)

chrome.debugger.onDetach.addListener((source) =>
  void whenReady(() => {
    if (source.tabId) detachTab(source.tabId, true)
  }),
)

// ---- tab lifecycle --------------------------------------------------------

chrome.tabs.onUpdated.addListener((tabId, changeInfo, tab) =>
  void whenReady(async () => {
    if (changeInfo.status === 'complete' && eligible(tab) && !tabs.has(tabId) && port) {
      try {
        await attachTab(tabId)
      } catch {}
    }
  }),
)
chrome.tabs.onRemoved.addListener((tabId) => void whenReady(() => detachTab(tabId, true)))

// ---- bootstrap + keepalive ------------------------------------------------

chrome.runtime.onInstalled.addListener(() => void whenReady(connectHost))
chrome.runtime.onStartup.addListener(() => void whenReady(connectHost))
chrome.action.onClicked.addListener(() => void whenReady(connectHost))

// MV3 service workers get suspended; an alarm wakes us to keep the host link
// and badges fresh.
chrome.alarms.create('keepalive', { periodInMinutes: 0.4 })
chrome.alarms.onAlarm.addListener((a) => {
  if (a.name !== 'keepalive') return
  void whenReady(() => {
    if (!port) connectHost()
    else void attachAllTabs()
  })
})

// Gate placeholder so future async state-rehydration can hook in.
async function whenReady(fn) {
  return fn()
}

// Kick a connection attempt as soon as the worker starts.
connectHost()
