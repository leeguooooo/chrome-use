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

// Resolve a stable identifier for the Chrome profile this worker runs in, for
// the `hello` handshake (issue #60). `profileId` is a UUID minted once and kept
// in this profile's chrome.storage.local — distinct per profile, no permission
// needed. `profileEmail` is included only when the optional `identity` permission
// is present and the profile is signed in; otherwise it's omitted (never throws).
async function buildHelloIdentity() {
  const extra = {}
  try {
    const KEY = 'ab_profile_id'
    const got = await chrome.storage.local.get(KEY)
    let id = got && got[KEY]
    if (!id) {
      id =
        (crypto && crypto.randomUUID && crypto.randomUUID()) ||
        'p-' + Math.abs(Date.now()).toString(36)
      await chrome.storage.local.set({ [KEY]: id })
    }
    extra.profileId = id
  } catch {}
  try {
    if (chrome.identity && chrome.identity.getProfileUserInfo) {
      const info = await new Promise((resolve) => {
        try {
          chrome.identity.getProfileUserInfo({ accountStatus: 'ANY' }, resolve)
        } catch {
          resolve(null)
        }
      })
      if (info && info.email) extra.profileEmail = info.email
    }
  } catch {}
  return extra
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

// ---- settings + UX (cursor overlay, connection notifications) -------------

let cursorEnabled = false
let notifyEnabled = false
let lastConnected = null

function loadUxSettings() {
  try {
    chrome.storage.sync.get({ ab_cursor: false, ab_notify: false }, (s) => {
      cursorEnabled = !!s.ab_cursor
      notifyEnabled = !!s.ab_notify
    })
  } catch {}
}
loadUxSettings()
try {
  chrome.storage.onChanged.addListener((ch, area) => {
    if (area !== 'sync') return
    if ('ab_cursor' in ch) cursorEnabled = !!ch.ab_cursor.newValue
    if ('ab_notify' in ch) notifyEnabled = !!ch.ab_notify.newValue
  })
} catch {}

// Fire a (silent) desktop notification when the bridge connects/disconnects —
// only on an actual transition, and only if the user opted in (options page).
function notifyConnChange(connected) {
  if (connected === lastConnected) return
  lastConnected = connected
  if (!notifyEnabled) return
  try {
    chrome.notifications.create('ab-conn-' + Date.now(), {
      type: 'basic',
      iconUrl: 'icons/icon128.png',
      title: connected ? 'chrome-use connected' : 'chrome-use disconnected',
      message: connected
        ? 'Bridged to your local CLI — ready to drive your tabs.'
        : 'The bridge to your local CLI dropped.',
      silent: true,
    })
  } catch {}
}

// Mirror agent mouse activity to a friendly on-page cursor, drawn IN the page via
// the EXISTING chrome.debugger session (no extra permissions). Opt-in (off by
// default) because the overlay is a visible DOM node — leaving it off keeps
// automation invisible to the page. Best-effort; never blocks the real command.
function maybeDriveCursor(tabId, method, params) {
  if (!cursorEnabled || method !== 'Input.dispatchMouseEvent') return
  const x = params && params.x
  const y = params && params.y
  if (typeof x !== 'number' || typeof y !== 'number') return
  const click = params.type === 'mousePressed'
  const expr = cursorOverlayExpression(x, y, click)
  sendCdpToTab(tabId, 'Runtime.evaluate', {
    expression: expr,
    returnByValue: false,
    awaitPromise: false,
  }).catch(() => {})
}

function cursorOverlayExpression(x, y, click) {
  return (
    '(function(){try{' +
    'var X=' + x + ',Y=' + y + ',CLICK=' + (click ? 'true' : 'false') + ';' +
    'var d=document,w=window,c=w.__abCursor;' +
    'if(!c||!c.host||!c.host.isConnected){' +
    'var host=d.createElement("div");host.style.cssText="position:fixed;left:0;top:0;width:0;height:0;z-index:2147483647;pointer-events:none";' +
    '(d.documentElement||d.body).appendChild(host);' +
    'var sr=host.attachShadow?host.attachShadow({mode:"open"}):host;' +
    'var glow=d.createElement("div");glow.style.cssText="position:fixed;left:0;top:0;width:34px;height:34px;margin:-17px 0 0 -17px;border-radius:50%;pointer-events:none;opacity:0;background:radial-gradient(circle,rgba(247,168,35,.55),rgba(247,168,35,0) 70%);transition:transform .13s cubic-bezier(.2,.7,.3,1),opacity .3s";' +
    'var cur=d.createElement("div");cur.style.cssText="position:fixed;left:0;top:0;width:24px;height:24px;margin:-2px 0 0 -2px;pointer-events:none;opacity:0;transition:transform .13s cubic-bezier(.2,.7,.3,1),opacity .3s";' +
    'cur.innerHTML=\'<svg width="24" height="24" viewBox="0 0 24 24" fill="none"><path d="M4 2 L4 19 L9 14.5 L12 21 L14.6 19.8 L11.6 13.4 L18 13 Z" fill="#fff" stroke="#1bb6c9" stroke-width="1.6" stroke-linejoin="round"/></svg>\';' +
    'sr.appendChild(glow);sr.appendChild(cur);c=w.__abCursor={host:host,sr:sr,cur:cur,glow:glow,t:0};' +
    '}' +
    'var el=c.cur,gl=c.glow;el.style.opacity="1";gl.style.opacity="1";' +
    'el.style.transform="translate("+X+"px,"+Y+"px)";gl.style.transform="translate("+X+"px,"+Y+"px)";' +
    'if(CLICK){var r=d.createElement("div");r.style.cssText="position:fixed;left:"+X+"px;top:"+Y+"px;width:10px;height:10px;margin:-5px 0 0 -5px;border:2px solid #f7a823;border-radius:50%;pointer-events:none;opacity:.9;transition:transform .5s ease-out,opacity .5s ease-out";c.sr.appendChild(r);requestAnimationFrame(function(){r.style.transform="scale(4)";r.style.opacity="0"});setTimeout(function(){r.remove()},520);}' +
    'clearTimeout(c.t);c.t=setTimeout(function(){el.style.opacity="0";gl.style.opacity="0"},2200);' +
    '}catch(e){}})()'
  )
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
    notifyConnChange(false)
    // Sessions are stale once the host is gone; the daemon re-discovers on
    // reconnect. Keep chrome.debugger attached so reconnect is cheap.
    for (const tabId of tabs.keys()) setBadge(tabId, 'connecting')
  })
  // Report our version + a stable per-profile id so the host can tell the
  // CLI/`doctor` which extension build is live AND which Chrome profile the relay
  // is bound to. With many profiles, "logged out" on a site is otherwise
  // indistinguishable from "wrong profile" — the id removes that ambiguity
  // (issue #60). The id is a UUID persisted in this profile's chrome.storage.local
  // (no extra permission). If the profile is signed into Chrome AND the optional
  // `identity` permission is granted, we also include the account email; absent
  // that, email is simply omitted. Best-effort; ignored by older hosts.
  void buildHelloIdentity().then((extra) => {
    try {
      postToHost({
        method: 'hello',
        version: chrome.runtime.getManifest().version,
        ...extra,
      })
    } catch {}
  })
  // Tell the daemon about everything we already have attached, then attach
  // anything new.
  void reannounceAttachedTabs()
  void attachAllTabs()
  notifyConnChange(true)
  // Start the proactive heartbeat so the worker stays alive while paired.
  scheduleKeepalivePing()
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

// A chrome.debugger.attach failure that will NEVER succeed for this tab, no
// matter how long we wait: the tab is another extension's page, a chrome:// page,
// or otherwise off-limits to us. Distinct from the transient "detached mid
// process-swap" errors (handled in sendCdpToTab), which DO clear on retry — so we
// match the specific permanent phrases, not a bare "cannot access". Used to bail
// out of the reattach retry loops instead of burning 6 attempts (and 6 warnings)
// on a page we can't touch (issue: reattach warning storm on multi-extension
// Chrome).
function isPermanentAttachError(e) {
  const m = String((e && e.message) || e)
  return /different extension|Cannot access a chrome:\/\/|must request permission|Cannot access contents|Cannot attach to (this|the) target/i.test(
    m,
  )
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
      } catch (e) {
        // Permanently off-limits (other extension's page etc.) — stop the tabId
        // fast-path and let the stable-targetId path below try a different tab.
        if (isPermanentAttachError(e)) break
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
          } catch (e) {
            // The tab hosting our target is a page we can never attach to — no
            // amount of waiting fixes that, so give up the recovery now.
            if (isPermanentAttachError(e)) return null
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

  // Non-CDP extension commands (ABExt.*) the daemon sends. `ungroupTab` removes a
  // tab from its per-session tab group so a `keep`-marked tab is left for the user
  // as a normal, ungrouped tab (the group can then be cleaned up). Best-effort.
  if (method === 'ABExt.ungroupTab') {
    const tabId = tabIdFromSession(sessionId) ?? tabForSession(sessionId)
    if (tabId != null && chrome.tabs.ungroup) {
      try {
        await chrome.tabs.ungroup(tabId)
      } catch {}
    }
    return { ungrouped: tabId ?? null }
  }

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
  // Mirror agent mouse activity to the friendly on-page cursor (opt-in; best-effort).
  maybeDriveCursor(tabId, method, params)
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
    // re-run for tabs that are already open). Include the live url/title so the
    // relay's target list stays matchable by URL after a reconnect (otherwise a
    // reannounced tab shows a blank url and `adopt <url>` can't find it).
    const { openerTargetId, abGroup } = await tabScopeHints(tabId)
    let url = ''
    let title = ''
    try {
      const t = await chrome.tabs.get(tabId)
      if (t) {
        url = t.url || t.pendingUrl || ''
        title = t.title || ''
      }
    } catch {}
    postToHost({
      method: 'forwardCDPEvent',
      params: {
        sessionId: entry.sessionId,
        method: 'Target.attachedToTarget',
        params: {
          sessionId: entry.sessionId,
          targetInfo: { targetId: entry.targetId, type: 'page', url, title, attached: true, openerTargetId, abGroup },
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
        // The tab became a page we can never attach to (another extension's page,
        // chrome://, restricted). Retrying can't help — bail quietly instead of
        // logging the same failure 6×. (`eligible()` filters most of these, but a
        // tab can navigate into one between our check and the attach.)
        if (isPermanentAttachError(e)) return
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

// Hot-path keepalive: while a native-messaging port is open, post a tiny ping
// every 20s. Each port message resets the MV3 service-worker idle timer, so the
// worker never reaches the ~30s idle-kill while we're paired and the relay stays
// live — instead of dropping during any quiet stretch (no CDP traffic) and making
// the next command hang until a reconnect. The setTimeout chain only survives as
// long as the worker does, which is exactly what the ping guarantees; on a cold
// worker restart the alarm below re-runs connectHost() which restarts this loop.
const KEEPALIVE_PING_MS = 20000
let keepaliveTimer = null
function scheduleKeepalivePing() {
  if (keepaliveTimer) clearTimeout(keepaliveTimer)
  keepaliveTimer = setTimeout(() => {
    keepaliveTimer = null
    if (!port) return // disconnected; connectHost() will restart the loop
    postToHost({ method: 'ping' }) // host pongs (or ignores); the send is what matters
    scheduleKeepalivePing()
  }, KEEPALIVE_PING_MS)
}

// Cold-restart backstop. MV3 service workers get suspended; an alarm wakes us to
// reconnect the host link and restart the heartbeat. NOTE: chrome.alarms clamps
// sub-minute periods (effective floor ~30s+), so the alarm alone can't outpace the
// 30s idle-kill — that's why the 20s ping above is the real keeper, not this.
chrome.alarms.create('keepalive', { periodInMinutes: 0.4 })
chrome.alarms.onAlarm.addListener((a) => {
  if (a.name !== 'keepalive') return
  void whenReady(() => {
    if (!port) connectHost()
    else {
      void attachAllTabs()
      scheduleKeepalivePing()
    }
  })
})

// Gate placeholder so future async state-rehydration can hook in.
async function whenReady(fn) {
  return fn()
}

// Kick a connection attempt as soon as the worker starts.
connectHost()
