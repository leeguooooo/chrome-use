// chrome-use connect — MV3 service worker.
//
// Bridges the user's real Chrome tabs to the local chrome-use daemon over a
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
/** Whether the native-messaging host (the local chrome-use CLI) is linked.
 *  Read by the popup status page. */
let hostConnected = false
/** tabId -> { sessionId, targetId } */
const tabs = new Map()
/** sessionId -> tabId (main session per tab) */
const sessionToTab = new Map()
/** child (OOPIF/worker) sessionId -> tabId */
const childSessionToTab = new Map()
/** tab-group name -> chrome tabGroups id (best-effort cache) */
const groupIdByName = new Map()

// Deterministic color per group name so a given session keeps the same color.
const GROUP_COLORS = ['blue', 'cyan', 'green', 'yellow', 'orange', 'red', 'pink', 'purple', 'grey']
function colorForName(name) {
  let h = 0
  for (let i = 0; i < name.length; i++) h = (h * 31 + name.charCodeAt(i)) >>> 0
  return GROUP_COLORS[h % GROUP_COLORS.length]
}

// Put a freshly-created tab into the agent/session's own Chrome tab group, so
// each agent's tabs are visually separated (from each other and from the user's
// own tabs) on the shared real browser. Best-effort: grouping failures never
// break tab creation.
async function groupTabInto(tabId, name) {
  if (!name || !chrome.tabGroups || !chrome.tabs.group) return
  const tab = await chrome.tabs.get(tabId).catch(() => null)
  if (!tab) return
  let gid = groupIdByName.get(name)
  if (gid != null) {
    const ok = await chrome.tabGroups.get(gid).then(() => true).catch(() => false)
    if (!ok) {
      gid = null
      groupIdByName.delete(name)
    }
  }
  if (gid == null) {
    // Reuse a same-titled group already in this window (survives SW restarts).
    const found = await chrome.tabGroups.query({ windowId: tab.windowId, title: name }).catch(() => [])
    if (found && found[0]) gid = found[0].id
  }
  if (gid == null) {
    gid = await chrome.tabs.group({ tabIds: tabId })
    await chrome.tabGroups.update(gid, { title: name, color: colorForName(name) }).catch(() => {})
  } else {
    await chrome.tabs.group({ groupId: gid, tabIds: tabId }).catch(() => {})
  }
  groupIdByName.set(name, gid)
}

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
    hostConnected = true
  } catch (e) {
    port = null
    hostConnected = false
    return
  }
  port.onMessage.addListener((msg) => void whenReady(() => onHostMessage(msg)))
  port.onDisconnect.addListener(() => {
    port = null
    hostConnected = false
    // Sessions are stale once the host is gone; the daemon re-discovers on
    // reconnect. Keep chrome.debugger attached so reconnect is cheap.
    for (const tabId of tabs.keys()) setBadge(tabId, 'connecting')
  })
  // Report our version so the host can tell the CLI/`doctor` which extension
  // build is live (otherwise the extension version is a black box — the user
  // can't tell they're on an old one). Best-effort; ignored by older hosts.
  try {
    postToHost({ method: 'hello', version: chrome.runtime.getManifest().version })
  } catch {}
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
  // Daemon (re)connected — (re)attach and announce every tab so it discovers
  // the user's existing tabs rather than racing an empty target list.
  if (msg.method === 'attachAll') {
    reannounceAttachedTabs()
    await attachAllTabs()
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

// Best-effort recovery for a stale `cb-tab-<tabId>` session: the handle is gone
// from our maps, but if the underlying Chrome tab still exists and is eligible,
// re-attach to it and return its id so the in-flight command can be retried.
// Returns null when the tab is genuinely gone (closed / restricted), in which
// case the caller surfaces the stale-session error. (issue #20.1)
async function recoverSessionTab(sessionId) {
  const m = /^cb-tab-(\d+)$/.exec(sessionId)
  if (!m) return null
  const tabId = Number(m[1])
  const tab = await chrome.tabs.get(tabId).catch(() => null)
  if (!eligible(tab)) return null
  try {
    await attachTab(tabId)
  } catch {
    return null
  }
  return tabs.has(tabId) ? tabId : null
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
    // Per-session tab grouping (non-CDP hint from the daemon). Best-effort.
    const group = typeof params?.agentGroup === 'string' ? params.agentGroup.trim() : ''
    if (group) {
      try {
        await groupTabInto(tab.id, group)
      } catch {}
    }
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
  //
  // A daemon-supplied sessionId/targetId MUST resolve to a real attached tab.
  // The old code fell through to anyConnectedTab() when it didn't, which
  // silently ran the command (eval/screenshot/network) on an arbitrary tab —
  // exactly the "ran on the wrong page with no warning" failure in issue #8.1,
  // and the blank-screenshot symptom after a service-worker restart (#8.2).
  // Fail loudly instead so the agent sees an actionable error, not bad data.
  let tabId
  if (sessionId) {
    tabId = tabForSession(sessionId)
    if (!tabId) {
      // The session's debugger handle is gone, but `cb-tab-<tabId>` encodes the
      // STABLE Chrome tabId (#17). A cross-process navigation (e.g. an SSO
      // redirect to another origin), a service-worker restart, or DevTools
      // briefly stealing the debugger all tear the handle down while the tab
      // itself lives on. Before failing, try to transparently re-attach to that
      // same tab and retry — so `open`/`navigate`/`eval` self-heal instead of
      // dead-ending the agent (issue #20.1). attachTab re-mints the identical
      // `cb-tab-<tabId>` session, so the daemon's binding stays valid.
      tabId = await recoverSessionTab(sessionId)
      if (!tabId) {
        throw new Error(
          `stale sessionId ${sessionId} for ${method}: its tab is gone (closed, ` +
            `navigated across processes, or lost after an extension restart). ` +
            `Re-attach by re-opening your target URL before retrying.`,
        )
      }
    }
  } else if (typeof params?.targetId === 'string') {
    tabId = tabForTarget(params.targetId)
    if (!tabId) throw new Error(`no attached tab for targetId ${params.targetId} (${method})`)
  } else {
    // No session/target specified — a browser-level command that legitimately
    // applies to any attached tab.
    tabId = anyConnectedTab()
  }
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
  try {
    await chrome.debugger.attach(dbg, '1.3')
  } catch (e) {
    // After a service-worker restart, chrome.debugger may still be bound to
    // this tab from the previous instance — "Another debugger is already
    // attached". The tab is still controllable via {tabId}, so don't skip it
    // (skipping is why existing tabs went un-announced and the daemon opened a
    // blank tab instead). Re-announce it. Any other error (restricted page) is
    // surfaced and the caller skips this tab.
    const msg = String((e && e.message) || e)
    if (!/already attached|already being debugged/i.test(msg)) throw e
  }
  await chrome.debugger.sendCommand(dbg, 'Page.enable').catch(() => {})
  const info = /** @type {any} */ (await chrome.debugger.sendCommand(dbg, 'Target.getTargetInfo'))
  const targetInfo = info?.targetInfo
  const targetId = String(targetInfo?.targetId || '')
  if (!targetId) throw new Error('attachTab: no targetId')
  // Derive the session id from the STABLE Chrome tabId, not a monotonic counter
  // (issue #17). A tab's chrome.debugger session can be torn down and
  // re-established — cross-process navigation, a service-worker restart wiping
  // these in-memory maps, DevTools stealing the debugger — and each time the tab
  // re-attaches. With a counter, re-attach minted a BRAND-NEW `cb-tab-N`, which
  // orphaned the daemon's binding (it's still pinned to the old id and the relay
  // never tells it to rebind) → permanent "stale sessionId / tab is gone". The
  // tabId is stable across all of that, so `cb-tab-<tabId>` restores the SAME
  // session the daemon already holds → eval/snapshot auto-follow the new page.
  const sessionId = `cb-tab-${tabId}`
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

chrome.debugger.onDetach.addListener((source, reason) =>
  void whenReady(async () => {
    const tabId = source.tabId
    if (!tabId) return
    detachTab(tabId, true)
    // A cross-process navigation (e.g. an SSO redirect like
    // login.account.rakuten.com that swaps the render process / spawns OOPIFs)
    // detaches the debugger, but the TAB survives. Without re-attaching, the
    // session goes permanently stale and even open/navigate fails — exactly the
    // #19 follow-up. So proactively re-attach (the stable `cb-tab-<tabId>`
    // session id then restores the daemon's binding). Don't fight a detach the
    // user or DevTools initiated.
    if (reason === 'canceled_by_user' || reason === 'replaced_with_devtools') return
    if (!port) return
    // The swapped-in process needs a moment to settle; retry with backoff.
    for (let i = 0; i < 6; i++) {
      await new Promise((r) => setTimeout(r, 250 + i * 200))
      if (tabs.has(tabId)) return // already re-attached (e.g. via onUpdated)
      const tab = await chrome.tabs.get(tabId).catch(() => null)
      if (!tab || !eligible(tab)) return // tab gone or now a restricted page
      try {
        await attachTab(tabId)
        return
      } catch (e) {
        console.warn(`ab-connect: reattach attempt ${i + 1} for tab ${tabId} failed:`, e)
      }
    }
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

// Popup status page asks for the live pairing state. Attempt a (re)connect on
// demand so opening the popup also nudges the link awake, then report.
chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg && msg.type === 'ab-status') {
    if (!port) {
      try { connectHost() } catch (e) {}
    }
    sendResponse({ connected: hostConnected, tabCount: tabs.size, host: HOST_NAME })
  }
  return true
})

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
