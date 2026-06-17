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
/** sessionId -> CDP targetId, kept ACROSS detach so a dead `cb-tab-<oldTabId>`
 * session can be recovered by its stable targetId when the cross-process nav
 * gave the tab a new Chrome tabId (issue #24). Capped to bound memory. */
const sessionTargets = new Map()
function rememberSessionTarget(sessionId, targetId) {
  if (!sessionId || !targetId) return
  sessionTargets.delete(sessionId)
  sessionTargets.set(sessionId, targetId)
  if (sessionTargets.size > 256) sessionTargets.delete(sessionTargets.keys().next().value)
}
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

// Group-scoped relay isolation hints (issue #40). The relay scopes
// Target.getTargets per agent by tab group; report two things in the synthesized
// targetInfo so it can attribute each tab:
//   - abGroup: the tab's Chrome tab-group TITLE (= the owning session name), so
//     the relay can re-attribute existing tabs after a restart (createTarget
//     tagging won't re-run for already-open tabs).
//   - openerTargetId: the targetId of the tab that opened this one, so a pop-up
//     (window.open / target=_blank / OAuth result) inherits its opener's group
//     and the agent that opened it can follow it — without foreign tabs leaking.
// Best-effort: any failure yields empty strings, which the relay ignores.
async function tabScopeHints(tabId) {
  let openerTargetId = ''
  let abGroup = ''
  try {
    const t = await chrome.tabs.get(tabId)
    if (t) {
      if (typeof t.openerTabId === 'number') {
        const op = tabs.get(t.openerTabId)
        if (op) openerTargetId = op.targetId
      }
      if (t.groupId != null && t.groupId >= 0 && chrome.tabGroups) {
        const g = await chrome.tabGroups.get(t.groupId).catch(() => null)
        if (g && g.title) abGroup = g.title
      }
    }
  } catch {}
  return { openerTargetId, abGroup }
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
  void reannounceAttachedTabs()
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
    void reannounceAttachedTabs()
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

// The STABLE Chrome tabId encoded in a `cb-tab-<tabId>` session id (#17), or
// null for any other session shape (child/iframe sessions). The tabId is the
// real source of truth: it survives the renderer-process swaps (cross-origin
// OAuth/SSO navs) that tear down the page's CDP target — which is why binding to
// it (like claude-in-chrome) rides through the hop that killed the old
// target/sessionId binding (issue #23).
function tabIdFromSession(sessionId) {
  const m = /^cb-tab-(\d+)$/.exec(sessionId || '')
  return m ? Number(m[1]) : null
}

// Ensure the debugger is attached to a `cb-tab-<tabId>` session's tab, re-attaching
// across the transient window of a process swap (with a couple of short retries).
// Returns the tabId on success, or null when the tab is genuinely gone
// (closed / restricted). (issues #20.1, #23)
async function recoverSessionTab(sessionId) {
  const tabId = tabIdFromSession(sessionId)
  // 1) Fast path: the encoded Chrome tabId still exists — re-attach it (covers
  //    the common renderer-process swap where the tabId is preserved, #23).
  if (tabId != null) {
    for (let i = 0; i < 3; i++) {
      const tab = await chrome.tabs.get(tabId).catch(() => null)
      if (!eligible(tab)) break // tabId is gone — fall through to targetId recovery
      try {
        await attachTab(tabId)
        if (tabs.has(tabId)) return tabId
      } catch {
        // mid-swap: tab exists but isn't attachable yet — back off and retry.
      }
      await new Promise((r) => setTimeout(r, 120 + i * 150))
    }
  }
  // 2) The Chrome tabId is gone, but the CDP targetId is STABLE across the nav.
  //    Some cross-process hops (Mercari's signin token exchange) give the tab a
  //    NEW tabId while keeping the same target, so `cb-tab-<oldTabId>` can't be
  //    recovered by tabId. Find the tab now hosting our remembered targetId via
  //    chrome.debugger.getTargets(), attach it, and ALIAS the dead session to it
  //    so the daemon's session id keeps resolving. Longer window: this hop can
  //    take several seconds to settle (issue #24).
  const targetId = sessionTargets.get(sessionId)
  if (targetId) {
    for (let i = 0; i < 6; i++) {
      const targets = await chrome.debugger.getTargets().catch(() => null)
      const t = targets && targets.find((x) => x.id === targetId && x.tabId != null)
      if (t && t.tabId != null) {
        const tab = await chrome.tabs.get(t.tabId).catch(() => null)
        if (eligible(tab)) {
          try {
            await attachTab(t.tabId)
            if (tabs.has(t.tabId)) {
              sessionToTab.set(sessionId, t.tabId) // alias dead session -> live tab
              return t.tabId
            }
          } catch {
            // not attachable yet — keep waiting for the swap to settle.
          }
        }
      }
      await new Promise((r) => setTimeout(r, 300 + i * 300))
    }
  }
  return null
}

// Send a CDP command to a tab, riding a debugger detach that can happen between
// our attach check and the command itself (a renderer-process swap mid-flight).
// On a detached-style failure, drop the stale handle, re-attach the stable tab,
// and retry once — so a cross-process nav never surfaces as a hard error (#23).
async function sendCdpToTab(tabId, method, params) {
  const dbg = { tabId }
  try {
    return await chrome.debugger.sendCommand(dbg, method, params)
  } catch (e) {
    const msg = String((e && e.message) || e)
    if (!/detached|not attached|target.*(closed|gone)|no target|cannot access|frame.*detached/i.test(msg)) {
      throw e
    }
    detachTab(tabId, false)
    const ok = await recoverSessionTab(`cb-tab-${tabId}`)
    if (!ok) throw e
    return await chrome.debugger.sendCommand(dbg, method, params)
  }
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
    // The stable Chrome tabId encoded in `cb-tab-<tabId>` is the source of truth
    // (it survives renderer-process swaps; the CDP target/sessionId does not).
    // Resolve via it primarily — don't depend on a session→tab map entry that the
    // detach handler may have cleared — and ensure the debugger is attached,
    // re-attaching across a cross-process nav before failing (issues #20.1, #23).
    // `tabForSession` still covers child/iframe sessions that aren't `cb-tab-*`.
    tabId = tabIdFromSession(sessionId) ?? tabForSession(sessionId)
    if (tabId == null) {
      throw new Error(`unknown sessionId ${sessionId} for ${method}`)
    }
    if (!tabs.has(tabId)) {
      const recovered = await recoverSessionTab(sessionId)
      if (!recovered) {
        throw new Error(
          `stale sessionId ${sessionId} for ${method}: its tab is gone (closed, ` +
            `navigated across processes, or lost after an extension restart). ` +
            `Re-attach by re-opening your target URL before retrying.`,
        )
      }
      tabId = recovered
    }
  } else if (typeof params?.targetId === 'string') {
    tabId = tabForTarget(params.targetId)
    if (!tabId) throw new Error(`no attached tab for targetId ${params.targetId} (${method})`)
  } else {
    // No session/target specified — a browser-level command that legitimately
    // applies to any attached tab.
    tabId = anyConnectedTab()
  }
  if (tabId == null) throw new Error(`no attached tab for ${method}`)

  // Re-enabling Runtime can leave a stale state; bounce it (matches upstream).
  if (method === 'Runtime.enable') {
    try {
      await sendCdpToTab(tabId, 'Runtime.disable', undefined)
      await new Promise((r) => setTimeout(r, 30))
    } catch {}
    return await sendCdpToTab(tabId, 'Runtime.enable', params)
  }
  return await sendCdpToTab(tabId, method, params)
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
  rememberSessionTarget(sessionId, targetId)
  setBadge(tabId, port ? 'on' : 'connecting')
  const { openerTargetId, abGroup } = await tabScopeHints(tabId)
  postToHost({
    method: 'forwardCDPEvent',
    params: {
      sessionId,
      method: 'Target.attachedToTarget',
      params: {
        sessionId,
        targetInfo: { ...targetInfo, attached: true, openerTargetId, abGroup },
      },
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

async function reannounceAttachedTabs() {
  for (const [tabId, entry] of tabs.entries()) {
    // Re-send the group hint too (issue #40) so the relay can rebuild its
    // targetId→group map after its own restart (createTarget tagging won't
    // re-run for tabs that are already open).
    const { openerTargetId, abGroup } = await tabScopeHints(tabId)
    postToHost({
      method: 'forwardCDPEvent',
      params: {
        sessionId: entry.sessionId,
        method: 'Target.attachedToTarget',
        params: {
          sessionId: entry.sessionId,
          targetInfo: { targetId: entry.targetId, type: 'page', attached: true, openerTargetId, abGroup },
        },
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
