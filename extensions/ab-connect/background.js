import { createConversationWindows } from './conversation-window.js';
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

import { getRelayHealth, observeDebuggerCommand } from './relay-health.js';
import { ScheduleNotifications } from './schedule-notifications.js';
import { TurnNotifications, openFromNotification } from './turn-notifications.js';
import { AgentPanelBridge, isAgentPanelSender, pageOperationAllowed } from './agent-panel-bridge.js';
import { InspectController } from './inspect-controller.js';
import { TeachController } from './teach-controller.js';
import { configureAgentPanel } from './sidepanel-entry.js';
import { cliSwitchPending, compareVersions } from './version-notice.js';
import { t } from './i18n.js';
import { STEALTH_DEFAULT, cursorAllowed, stealthBlockReason } from './stealth-mode.js';
import { createCursorOverlay } from './cursor-overlay.js';
import { extensionInstallIdentity } from './relay-identity.js';
import { duplicateTab as runDuplicateTab } from './tab-duplicate.js';
import {
  agentTabPredicate,
  confirmAttachedPopup,
  isVerifiedAgentPopup,
  migratePopupRecord,
} from './agent-window.js';
import { attachTabById } from './attach-by-id.js';
import { attachCreatedTab, createTabAttachmentQueue } from './tab-attachment.js';
import { releaseTab } from './release-tab.js';
import { blurFocusedField } from './menu-blur.js';
import { NativeFocusBridge, prepareNativeFocusRecovery } from './native-focus.js';
import { SITE_ACCESS_NOTIFICATION, SITE_ACCESS_OPTIONS_PAGE, hasSiteAccess, isSiteAccessError, notifySiteAccessNeeded } from './site-access.js';
import { DWELL_MS, UserTouchedTabs } from './user-touched.js';
import { removeAgentTab } from './remove-agent-tab.js';
import { shouldForwardEvent } from './cdp-event-filter.js';
import { clearDownloads, listDownloads, startDownload } from './download-manager.js';
import { isRelayTimeoutError, relayUnresolvedOperations, withRelayTimeout } from './relay-timeout.js';
import {
  reconcileAttachedTabEntries,
  resolveFirstLiveTab,
  resolveSessionTab,
  forgetSessionTab,
} from './tab-liveness.js';
import {
  canUseBrowserNavigationFallback,
  navigateTabWithBrowserFallback,
} from './tab-navigation.js';
import {
  activeReloadLoop,
  newReloadState,
  recordNavigationCommit,
  resetReloadLoop,
  transferReloadState,
  RELOAD_LOOP_WINDOW_MS,
} from './reload-loop.js';
import { targetInfoForTab } from './target-info.js';
import { isMissingTabError, targetPresence } from './tab-presence.js';
import { isAgentGroupTitle, ungroupRestoredAgentGroups } from './restored-groups.js';
import {
  announceVerdict,
  createRemovedTabs,
  relayTargetsMessage,
  tabGoneEvent,
} from './relay-records.js';
import { createAgentTabQueue } from './agent-tab-queue.js';
import { createAttachmentHealth, isAttachmentCleanup, sendTabCommand } from './tab-command.js';
import { prepareScreenshotWindow } from './screenshot-window.js';
import { HostConnectionState, HostReconnectScheduler } from './host-connection.js';
import { isDebuggerAccessDenied, debuggerAccessError, foreignFrameExtensions } from './debugger-access.js';
import {
  IDLE_DETACH_DEFAULT_SECS,
  idleDetachMsFrom,
  rememberReplayable as rememberReplayableIn,
  selectIdleTabs,
} from './idle-detach.js';
import { oversizeReplyError } from './host-message-size.js';
import {
  shouldCheckForUpdate,
  updateApplyPlan,
  keepsOwnershipAcrossUpdate,
  UPDATE_HANDOFF_KEY,
} from './update-check.js';
import { attachedTargetsFrom } from './attached-targets.js';
import { executeCall, policySummary, POLICY_VERSION } from './api-passthrough.js';
import { createSitePermissions } from './site-permissions.js';
import { createUserTabOrganizer, USER_TAB_GROUPS_VERSION, USER_TAB_GROUPS_JOIN } from './user-tab-organizer.js';
import { createTabGrants, LENT_TAB_CLOSE } from './tab-grant.js';
import { bindingsFrom, openTabPanel, panelTabKey, PANEL_TAB_PREFIX, tabForConversation } from './panel-tabs.js';
import { FrontTabGuard } from './front-tab-guard.js';
import {
  followReplacement,
  forgetReplacement,
  recordReplacement,
  reviveDiscardedTab,
} from './tab-replacement.js';

let userTabOrganizer;
async function getUserTabOrganizer() {
  await loadOwnedTabs();
  await loadAgentPopups();
  userTabOrganizer ??= createUserTabOrganizer({
    tabs: chrome.tabs, groups: chrome.tabGroups, storage: chrome.storage.local,
    isAgent: id => ownedTabs.has(id) || agentPopups.has(id) || tabs.get(id)?.attached === true,
    isAgentWindow: id => id === agentWindowId,
  });
  return userTabOrganizer;
}
// The Web Store package strips `key`. This hook exists only in unpacked/signed development builds
// so acceptance can exercise the exact background integration against an isolated Chrome profile.
if (chrome.runtime.getManifest().key) {
  globalThis.__chromeUseAcceptance = {
    organizeUserTabs: async params => (await getUserTabOrganizer()).organize(params),
    undoOrganizeUserTabs: async params => (await getUserTabOrganizer()).undo(params),
    // Inspect & Ask acceptance: start a picker as the shortcut would (the
    // acceptance build adds host permissions in place of the activeTab grant).
    startInspect: async tabId => globalThis.__chromeUseInspect.start(await chrome.tabs.get(tabId)),
    inspectCapture: async tabId => {
      const instance = globalThis.__chromeUseInspect.registry.get(tabId);
      if (!instance) throw new Error('no instance');
      await globalThis.__chromeUseInspect.queueCapture(instance);
      return { images: instance.images.size };
    },
    inspectState: tabId => {
      const instance = globalThis.__chromeUseInspect.registry.get(tabId);
      return { hostReady: globalThis.__chromeUseInspect.hostReady(), resolveReady: globalThis.__chromeUseInspect.resolveReady(), instance: instance && { origin: instance.origin, frameId: instance.frameId, annotations: instance.annotations.length, images: instance.images.size, generation: instance.generation, tabTarget: instance.tabTarget || null } };
    },
  };
}
const HOST_NAME = 'com.agent_browser.connect';
let agentPanelEntry = { ready: false, error: 'Side panel entry is being configured.' };
void configureAgentPanel(chrome).then(result => { agentPanelEntry = result; });
const nativeFocusContexts = new Map();
const nativeFocusBridge = new NativeFocusBridge({
  send(message) {
    if (!port) throw new Error('Native host is not connected');
    port.postMessage(message);
  },
});
const agentPanelBridge = new AgentPanelBridge({
  send(message) {
    if (!port) throw new Error('Native host is not connected');
    port.postMessage(message);
  },
});
// Inspect & Ask: picker, input frame and delivery. Started only from the
// keyboard command or the popup button; see inspect-controller.js.
const inspectController = new InspectController({
  chrome,
  request: (operation, params) => agentPanelBridge.request(operation, params),
  hostReady: () => { const state = hostConnection.snapshot(); return !!port && state.connected && state.agentPanel?.inspect === true; },
  resolveReady: () => hostConnection.snapshot().agentPanel?.inspectResolve === true,
  repliesReady: () => hostConnection.snapshot().agentPanel?.inspectReplies >= 1,
});
inspectController.install();
// The side panel's Record button (teach by demonstration); see
// teach-controller.js. Records only while the user has it on.
const teachController = new TeachController({ chrome });
teachController.install();
if (chrome.runtime.getManifest().key) globalThis.__chromeUseInspect = inspectController;
const scheduleNotifications = new ScheduleNotifications({ storage: chrome.storage.local, notifications: chrome.notifications });
function pollScheduleNotifications() {
  const state = hostConnection.snapshot();
  if (!state.connected || !state.agentPanel?.schedules || state.agentPanel.mode !== 'full') return;
  void scheduleNotifications.poll(() => agentPanelBridge.request('scheduleList')).catch(error => console.warn('Schedule notification check failed:', String(error.message || error)));
}
// Finished panel turns. Open side panels report the conversation they show
// (port `ab-panel-presence`), so a turn the user is watching does not notify.
/** port -> { windowId, conversationId, visible } */
const panelPresence = new Map();
let turnBadge = '';
let lastFocusedWindowId = null;
let pendingPanelSelect = null;
const turnNotifications = new TurnNotifications({
  storage: chrome.storage.local,
  notifications: chrome.notifications,
  badge(text) {
    turnBadge = text;
    try {
      chrome.action.setBadgeText({ text });
      chrome.action.setBadgeBackgroundColor({ color: '#2563eb' });
      // A per-tab badge hides the global one; attached tabs show the count too.
      for (const tabId of tabs.keys()) setBadge(tabId, port ? 'on' : 'connecting');
    } catch {}
  },
});
// One panel per tab: which conversation each tab's panel shows, mirrored
// from storage.session so a notification click can open the right panel
// while its gesture lasts, and the active tab of each window.
const panelTabs = new Map();
const activeTabs = new Map();
void chrome.storage.session.get(null).then(all => { for (const [tabId, id] of bindingsFrom(all)) if (!panelTabs.has(tabId)) panelTabs.set(tabId, id); }).catch(() => {});
chrome.storage.onChanged.addListener((changes, area) => {
  if (area !== 'session') return;
  for (const [key, change] of Object.entries(changes)) {
    if (!key.startsWith(PANEL_TAB_PREFIX)) continue;
    const tabId = Number(key.slice(PANEL_TAB_PREFIX.length));
    if (typeof change.newValue === 'string' && change.newValue) panelTabs.set(tabId, change.newValue); else panelTabs.delete(tabId);
    if (typeof change.newValue === 'string' && change.newValue) void chrome.tabs.get(tabId).then(tab => conversationWindows.binding(change.newValue, tab)).catch(() => {});
  }
});
void chrome.tabs.query({ active: true }).then(list => { for (const tab of list) if (!activeTabs.has(tab.windowId)) activeTabs.set(tab.windowId, tab.id); }).catch(() => {});
chrome.tabs.onActivated.addListener(({ tabId, windowId }) => activeTabs.set(windowId, tabId));
// The toolbar button opens the panel of the tab it was clicked on.
chrome.action.onClicked.addListener(tab => {
  if (!Number.isInteger(tab?.id)) return;
  void openTabPanel(tab.id, { sidePanel: chrome.sidePanel }).catch(error => console.warn('Open the side panel:', String(error?.message || error)));
});
// A closed tab's panel goes with it; its conversation stays in the history.
function forgetTabPanel(tabId) {
  panelTabs.delete(tabId);
  void chrome.storage.session.remove(panelTabKey(tabId)).catch(() => {});
}
const conversationWindows = createConversationWindows({
  tabs: chrome.tabs, windows: chrome.windows, storage: chrome.storage,
  boundTab: async id => {
    await tabGrants.load();
    return tabGrants.forConversation(id)?.tabId ?? tabForConversation(bindingsFrom(await chrome.storage.session.get(null)), id);
  },
  beforeActivate: tabId => userTouched.agentActivating(tabId),
});
chrome.tabs.onUpdated.addListener((id, change, tab) => {
  if (change.url || change.status === 'complete') void conversationWindows.updated(tab).catch(() => {});
});
chrome.tabs.onReplaced.addListener((addedId, removedId) => {
  void chrome.tabs.get(addedId).then(tab => conversationWindows.replaced(removedId, tab)).catch(() => {});
});
const notificationPanels = {
  tabFor: conversationId => tabForConversation(panelTabs, conversationId),
  activeTab: windowId => activeTabs.get(windowId) ?? null,
  bind: (tabId, conversationId) => { panelTabs.set(tabId, conversationId); void chrome.storage.session.set({ [panelTabKey(tabId)]: conversationId }).catch(() => {}); },
  open: tabId => openTabPanel(tabId, { sidePanel: chrome.sidePanel }),
  // Return the activated Tab so notification focus follows its current window.
  show: tabId => chrome.tabs.update(tabId, { active: true }),
};
async function panelShowing(conversationId) {
  for (const presence of panelPresence.values()) {
    if (presence.conversationId !== conversationId || !presence.visible) continue;
    // A tab's panel is in view only while its tab is the active one.
    if (Number.isInteger(presence.tabId) && activeTabs.get(presence.windowId) !== presence.tabId) continue;
    try { if ((await chrome.windows.get(presence.windowId)).focused) return true; } catch {}
  }
  return false;
}
async function turnNotifyEnabled() {
  try { return (await chrome.storage.sync.get({ ab_turn_notify: true })).ab_turn_notify !== false; } catch { return true; }
}
function pollTurnNotifications() {
  const state = hostConnection.snapshot();
  if (!port || !state.connected || !state.agentPanel) return;
  void turnNotifyEnabled().then(enabled => turnNotifications.poll(() => agentPanelBridge.request('conversationList'), { enabled, showing: panelShowing }))
    .catch(error => console.warn('Turn notification check failed:', String(error.message || error)));
  // Agent replies to Inspect & Ask batches, from hosts that keep them.
  if (state.agentPanel.inspectReplies >= 1) {
    void turnNotifyEnabled().then(enabled => turnNotifications.pollReplies(() => agentPanelBridge.request('inspectReplies'), { enabled }))
      .catch(error => console.warn('Inspect reply check failed:', String(error.message || error)));
  }
}
chrome.runtime.onConnect.addListener(panelPort => {
  if (panelPort.name !== 'ab-panel-presence' || !isAgentPanelSender(panelPort.sender, chrome.runtime)) return;
  panelPresence.set(panelPort, {});
  panelPort.onDisconnect.addListener(() => panelPresence.delete(panelPort));
  panelPort.onMessage.addListener(message => {
    if (message?.type !== 'presence' || !Number.isInteger(message.windowId)) return;
    const presence = { windowId: message.windowId, tabId: Number.isInteger(message.tabId) ? message.tabId : null, conversationId: typeof message.conversationId === 'string' ? message.conversationId : '', visible: message.visible === true };
    panelPresence.set(panelPort, presence);
    // A panel opened from a notification may start before the stored selection lands.
    if (pendingPanelSelect && pendingPanelSelect.windowId === presence.windowId && Date.now() - pendingPanelSelect.at < 30000) {
      if (pendingPanelSelect.conversationId !== presence.conversationId) panelPort.postMessage({ type: 'select', conversationId: pendingPanelSelect.conversationId });
      pendingPanelSelect = null;
    }
    if (presence.visible && presence.conversationId) void turnNotifications.seen(presence.conversationId).catch(() => {});
    if (message.repliesShown === true) void turnNotifications.repliesSeen().catch(() => {});
  });
});
try {
  void chrome.windows.getLastFocused().then(window => { lastFocusedWindowId ??= window?.id ?? null; }).catch(() => {});
  chrome.windows.onFocusChanged.addListener(windowId => {
    if (windowId === chrome.windows.WINDOW_ID_NONE) return;
    lastFocusedWindowId = windowId;
    // Coming back to a window whose panel shows a finished turn is seeing it.
    for (const presence of panelPresence.values()) {
      if (presence.windowId === windowId && presence.visible && presence.conversationId) void turnNotifications.seen(presence.conversationId).catch(() => {});
    }
  });
} catch {}
// Per-site permissions (#781): approved sites, history, pending requests and
// the managed policy. The daemon decides; this keeps the person's answers.
const sitePermissions = createSitePermissions({ storage: chrome.storage, notifications: chrome.notifications, t });
try {
  chrome.storage.onChanged.addListener((_changes, area) => { if (area === 'managed') void sitePermissions.refreshManaged(); });
} catch {}
chrome.notifications.onClicked.addListener(id => {
  if (id === SITE_ACCESS_NOTIFICATION) {
    void chrome.tabs.create({ url: chrome.runtime.getURL(SITE_ACCESS_OPTIONS_PAGE) }).catch(() => {});
    void chrome.notifications.clear(id).catch(() => {});
    return;
  }
  // A site approval: its choices live on the options page (and the side panel).
  if (typeof id === 'string' && id.startsWith('ab-site-')) {
    void chrome.tabs.create({ url: chrome.runtime.getURL('options.html#permissions') }).catch(() => {});
    return;
  }
  openFromNotification(id, { windowId: lastFocusedWindowId, chrome, panels: panelPresence, pending: selection => { pendingPanelSelect = selection; }, tabs: agentPanelEntry.ready ? notificationPanels : null });
});
const SKIP_URL = /^(chrome|chrome-extension|devtools|chrome-untrusted|edge|about):/i;

/** @type {chrome.runtime.Port|null} */
let port = null;
/** Whether the native-messaging host (the local chrome-use CLI) is linked.
 *  Read by the popup status page. */
const hostConnection = new HostConnectionState();
const hostReconnect = new HostReconnectScheduler({ retry: () => connectHost() });
/** tabId -> { sessionId, targetId } */
const tabs = new Map();
/** tabId -> reload history, retained across debugger/process re-attachments. */
const reloadStates = new Map();
/** Tab ids Chrome reported removed or replaced. Late work for one (an attach
 * or announce that was awaiting Chrome when the tab closed) is refused instead
 * of recreating a record nothing will ever remove (#519). */
const removedTabs = createRemovedTabs();
/** sessionId -> tabId (main session per tab) */
const sessionToTab = new Map();
/** child (OOPIF/worker) sessionId -> tabId */
const childSessionToTab = new Map();
/** sessionId -> CDP targetId, kept ACROSS detach so a dead `cb-tab-<oldTabId>`
 * session can be recovered by its stable targetId when the cross-process nav
 * gave the tab a new Chrome tabId (issue #24). Capped to bound memory. */
const sessionTargets = new Map();
function rememberSessionTarget(sessionId, targetId) {
  if (!sessionId || !targetId) return;
  sessionTargets.delete(sessionId);
  sessionTargets.set(sessionId, targetId);
  if (sessionTargets.size > 256) sessionTargets.delete(sessionTargets.keys().next().value);
}
/** tab-group name -> chrome tabGroups id (best-effort cache) */
const groupIdByName = new Map();
// A new target's requested group is known before its command reply. Keep the
// hint until attach is announced, even if Chrome's visual grouping fails.
const creatingTabGroups = new Map();
/** Source tabId -> native duplicate API promises awaiting exact tab classification. */
const pendingNativeDuplicates = new Map();
/** Native duplicate tabs owned by an explicit duplicate transaction. */
const nativeDuplicateTabs = new Set();
const NATIVE_DUPLICATE_CLASSIFICATION_TIMEOUT_MS = 5000;
const NATIVE_DUPLICATE_REGISTRY_TIMEOUT_MS = 30000;

function duplicateTabForTransaction(sourceTabId) {
  const pending = pendingNativeDuplicates.get(sourceTabId) || new Set();
  pendingNativeDuplicates.set(sourceTabId, pending);
  const operation = chrome.tabs.duplicate(sourceTabId).then((tab) => {
    if (tab?.id != null) nativeDuplicateTabs.add(tab.id);
    return tab;
  });
  let expiry;
  const classification = Promise.race([
    operation.then(
      () => undefined,
      () => undefined
    ),
    new Promise((resolve) => {
      expiry = setTimeout(resolve, NATIVE_DUPLICATE_REGISTRY_TIMEOUT_MS);
    }),
  ]);
  pending.add(classification);
  void classification.finally(() => {
    clearTimeout(expiry);
    pending.delete(classification);
    if (pending.size === 0) pendingNativeDuplicates.delete(sourceTabId);
  });
  return operation;
}

async function isNativeDuplicateLifecycleEvent(tab) {
  if (tab?.id == null) return false;
  if (nativeDuplicateTabs.has(tab.id)) return true;
  if (typeof tab.openerTabId !== 'number') return false;
  const pending = [...(pendingNativeDuplicates.get(tab.openerTabId) || [])];
  if (pending.length === 0) return false;
  let timer;
  const classified = await Promise.race([
    Promise.allSettled(pending).then(() => true),
    new Promise((resolve) => {
      timer = setTimeout(() => resolve(false), NATIVE_DUPLICATE_CLASSIFICATION_TIMEOUT_MS);
    }),
  ]);
  clearTimeout(timer);
  if (!classified) {
    void Promise.allSettled(pending).then(async () => {
      if (nativeDuplicateTabs.has(tab.id) || tabs.has(tab.id) || !port) return;
      const liveTab = await chrome.tabs.get(tab.id).catch(() => null);
      if (!eligible(liveTab)) return;
      try {
        await attachTab(tab.id);
      } catch {}
    });
    return true;
  }
  return nativeDuplicateTabs.has(tab.id);
}

// Shared "agent window" (opt-in: the daemon sends the `dedicatedWindow` hint
// only when AGENT_BROWSER_DEDICATED_WINDOW / `--window dedicated` turns it on;
// by default agent tabs open in the user's window, in their session's group).
// When set, all agent tabs are created in this one separate window — same Chrome
// profile (so logins/cookies are shared), but out of the window the user is
// working in, instead of being interleaved with the user's own tabs. Per-session
// colored tab groups still apply inside it. The id is persisted in
// chrome.storage.local and re-validated on use, so a service-worker restart
// reuses the SAME window instead of opening a second one. Lost records are
// recovered only from windows whose tabs have verified agent ownership;
// uncertain reads and placeholder cleanup failures never create replacements.
// `null` = not resolved
// yet this SW lifetime.
const AGENT_WINDOW_KEY = 'ab_agent_window_id';
let agentWindowId = null;
// Why the last attempt to open the agent window failed (reported in
// ABExt.state and in the createTarget refusal), or null.
let agentWindowError = null;
// { windowId, tabId } of the placeholder tab the agent window was created
// with. The only blank tab the agent window check exempts (a blank tab is
// never assumed to be ours by its URL), and only while it is still untouched
// (see isUntouchedPlaceholder). Kept in chrome.storage.local only, never in a
// worker variable, so a service-worker restart neither trusts a stale copy nor
// loses the record it needs to clean up.
const AGENT_PLACEHOLDER_KEY = 'ab_agent_window_placeholder';
// Why the last remembered agent window was rejected (ABExt.state), or null.
let agentWindowRejected = null;
// Pop-ups verified as the agent's (#456): adopted through ABExt.attachTabById,
// which the daemon calls only for a tab its own click opened (session-owned
// opener or group), or opened by an agent-created tab / such a pop-up. They are
// not persisted into ownedTabs; they are kept in storage.session so a worker
// restart does not turn them into "user tabs". User tabs adopted with
// adopt/inspect are never added here.
const AGENT_POPUPS_KEY = 'ab_agent_popups';
const agentPopups = new Set();
let agentPopupsLoaded = null;
function loadAgentPopups() {
  agentPopupsLoaded ??= chrome.storage.session
    .get(AGENT_POPUPS_KEY)
    .then((g) => {
      for (const id of g[AGENT_POPUPS_KEY] || []) agentPopups.add(id);
    })
    .catch(() => {});
  return agentPopupsLoaded;
}
async function markAgentPopup(tabId) {
  await loadAgentPopups();
  if (!Number.isInteger(tabId) || agentPopups.has(tabId)) return;
  agentPopups.add(tabId);
  await chrome.storage.session.set({ [AGENT_POPUPS_KEY]: [...agentPopups] }).catch(() => {});
}
// Pop-ups whose onRemoved fired in the last few seconds: onReplaced (a
// discard or prerender swap) may arrive after it and must carry the record
// over to the new tab id, like ownedTabs.
const recentlyRemovedPopups = new Set();
async function forgetAgentPopup(tabId) {
  await loadAgentPopups();
  if (agentPopups.delete(tabId)) {
    recentlyRemovedPopups.add(tabId);
    setTimeout(() => recentlyRemovedPopups.delete(tabId), 5000);
    await chrome.storage.session.set({ [AGENT_POPUPS_KEY]: [...agentPopups] }).catch(() => {});
  }
}
async function migrateAgentPopup(removedTabId, addedTabId) {
  await loadAgentPopups();
  if (migratePopupRecord(agentPopups, recentlyRemovedPopups, removedTabId, addedTabId))
    await chrome.storage.session.set({ [AGENT_POPUPS_KEY]: [...agentPopups] }).catch(() => {});
}

// The queue bounds every Chrome/storage step and owns late-result cleanup.
// Callbacks defer access to the ownership sets until module startup completes.
const createAgentTab = createAgentTabQueue({
  chrome,
  windowKey: AGENT_WINDOW_KEY,
  placeholderKey: AGENT_PLACEHOLDER_KEY,
  getWindowId: () => agentWindowId,
  setWindowId: id => { agentWindowId = id; agentWindowError = null; },
  rejectWindow: verdict => { agentWindowRejected = verdict; },
  get ownedTabs() { return ownedTabs; },
  get agentPopups() { return agentPopups; },
  loadOwnedTabs,
  loadAgentPopups,
  persistOwnedTabs: () => chrome.storage.local.set({ ab_owned_tabs: [...ownedTabs] }),
  isAdopted: id => ownedTabs.has(id) || agentPopups.has(id) || tabs.has(id),
  onError: message => { agentWindowError = message; },
  onCleanupError: error => { agentWindowError = `expired creation cleanup failed: ${error.message}`; },
});

// Forget the agent window when the user (or Chrome) closes it, so the next agent
// tab creates a fresh one instead of targeting a dead window id.
if (chrome.windows && chrome.windows.onRemoved) {
  chrome.windows.onRemoved.addListener((windowId) => {
    if (windowId === agentWindowId) {
      agentWindowId = null;
      chrome.storage.local.remove([AGENT_WINDOW_KEY, AGENT_PLACEHOLDER_KEY]).catch(() => {});
    }
  });
}

// Which Chrome profile is the user actively using? When several profiles each
// run this extension, the daemon otherwise binds to whichever host connected
// last — often NOT the profile the user is logged into for the task, so the
// agent lands on a logged-out profile. Ping the host whenever a real window of
// THIS profile gains focus (ignoring our own background agent window); the host
// timestamps it, and the CLI defaults to the most-recently-focused profile.
// onFocusChanged wakes the MV3 worker, so the user focusing the window they
// want is exactly the signal that reaches the host.
if (chrome.windows && chrome.windows.onFocusChanged) {
  chrome.windows.onFocusChanged.addListener((windowId) => {
    if (windowId == null || windowId === chrome.windows.WINDOW_ID_NONE) return;
    if (windowId === agentWindowId) return;
    postToHost({ method: 'focus' });
  });
}

// Tabs THIS extension owns: ones the agent created (Target.createTarget) or
// explicitly adopted. We attach the debugger ONLY to these — never to the user's
// own tabs — so Chrome's "started debugging this browser" banner never appears on
// a page the user is actually working in (it only shows on attached tabs, and the
// agent's are background tabs the user doesn't look at). Persisted in
// chrome.storage.local so a service-worker restart re-attaches exactly these and
// not the whole window. (Issue: banner occludes the user's foreground tab.)
const ownedTabs = new Set();

// Agent tabs the user switched to and stayed on (see user-touched.js). The
// CLI's cleanup of an ended session leaves these open for the user. Kept in
// storage.session: a tab id means nothing after Chrome restarts.
const userTouched = new UserTouchedTabs({ isAgentTab: (id) => ownedTabs.has(id) || agentPopups.has(id) });
const USER_TOUCHED_KEY = 'ab_user_touched_tabs';
const userTouchedLoaded = chrome.storage.session
  .get(USER_TOUCHED_KEY)
  .then((g) => userTouched.restore(g?.[USER_TOUCHED_KEY]))
  .catch(() => {});
function saveUserTouched() {
  chrome.storage.session.set({ [USER_TOUCHED_KEY]: userTouched.list() }).catch(() => {});
}

// "Let the agent use this tab" (0.5.62): user tabs lent to one side-panel
// conversation, and tabs taken back from one (refused until granted again or
// closed). See tab-grant.js.
const tabGrants = createTabGrants({ storage: chrome.storage.session });
void tabGrants.load();

// removed tabId -> the tabId Chrome put in its place (a discard, a prerender
// swap). Followed by recoverSessionTab so a `cb-tab-<old>` session finds its
// tab again instead of reporting it gone. Only tabs the relay knows are
// recorded, the map is bounded, and it is mirrored to storage.session so a
// service-worker restart between the discard and the next command keeps it.
const replacedTabs = new Map();
let replacedLoaded = null;
function loadReplacedTabs() {
  replacedLoaded ??= chrome.storage.session
    .get('ab_replaced_tabs')
    .then((g) => {
      for (const [from, to] of g.ab_replaced_tabs || []) {
        if (!replacedTabs.has(from)) replacedTabs.set(from, to);
      }
    })
    .catch(() => {});
  return replacedLoaded;
}
function saveReplacedTabs() {
  chrome.storage.session.set({ ab_replaced_tabs: [...replacedTabs] }).catch(() => {});
}
function relayKnowsTab(tabId) {
  if (ownedTabs.has(tabId) || recentlyRemovedOwned.has(tabId) || tabs.has(tabId)) return true;
  if (sessionTargets.has(`cb-tab-${tabId}`)) return true;
  for (const t of sessionToTab.values()) if (t === tabId) return true;
  return false;
}
// Owned tabs whose onRemoved fired in the last few seconds: onReplaced may
// arrive after it, and must still carry the ownership over.
const recentlyRemovedOwned = new Set();
let ownedLoaded = false;
let loadOwnedPromise = null;

async function loadOwnedTabs() {
  if (ownedLoaded) return;
  if (!loadOwnedPromise) {
    loadOwnedPromise = (async () => {
      try {
        const g = await chrome.storage.local.get('ab_owned_tabs');
        for (const id of g.ab_owned_tabs || []) ownedTabs.add(id);
      } catch {}
      ownedLoaded = true;
    })();
  }
  return loadOwnedPromise;
}

// Eagerly initiate loading on worker startup
void loadOwnedTabs();

function persistOwnedTabs() {
  try {
    chrome.storage.local.set({ ab_owned_tabs: [...ownedTabs] });
  } catch {}
}

async function markOwned(tabId) {
  await loadOwnedTabs();
  if (tabId != null && !ownedTabs.has(tabId)) {
    ownedTabs.add(tabId);
    persistOwnedTabs();
  }
}

async function unmarkOwned(tabId) {
  await loadOwnedTabs();
  if (ownedTabs.delete(tabId)) persistOwnedTabs();
}

// A tab in a group chrome-use titled is the agent's even when the owned set no
// longer says so: an install or update clears it (settleOwnershipAfterInstall),
// and the tabs a running session still drives would close still grouped.
// Anything unreadable answers false, so a user's own group is never taken apart.
async function tabInAgentGroup(tabId) {
  try {
    const groupId = (await chrome.tabs.get(tabId))?.groupId;
    if (!Number.isInteger(groupId) || groupId < 0) return false;
    return isAgentGroupTitle((await chrome.tabGroups.get(groupId))?.title);
  } catch {
    return false;
  }
}

// Closing an agent tab ungroups it first so Chrome keeps no saved tab group;
// a user's own tab (`tab close --force`) keeps its group (see remove-agent-tab.js).
// Agent pop-ups join their opener's group, so they are ungrouped too.
const agentTabRemoval = {
  isAgentTab: (tabId) => ownedTabs.has(tabId) || agentPopups.has(tabId),
  inAgentGroup: (tabId) => tabInAgentGroup(tabId),
  ungroup: (tabId) => chrome.tabs.ungroup(tabId),
  remove: (tabId) => chrome.tabs.remove(tabId),
};
// A native duplicate is always one the agent made, owned yet or not.
const duplicateTabRemoval = { ...agentTabRemoval, isAgentTab: () => true };

// Deterministic color per group name so a given session keeps the same color.
const GROUP_COLORS = ['blue', 'cyan', 'green', 'yellow', 'orange', 'red', 'pink', 'purple', 'grey'];
function colorForName(name) {
  let h = 0;
  for (let i = 0; i < name.length; i++) h = (h * 31 + name.charCodeAt(i)) >>> 0;
  return GROUP_COLORS[h % GROUP_COLORS.length];
}

// Put a freshly-created tab into the agent/session's own Chrome tab group, so
// each agent's tabs are visually separated (from each other and from the user's
// own tabs) on the shared real browser. This helper is strict; callers may
// explicitly downgrade grouping failures to best-effort when their contract
// allows it.
async function groupTabInto(tabId, name) {
  if (!name || !chrome.tabGroups || !chrome.tabs.group) {
    throw new Error('groupTabInto: Chrome tab-group APIs are unavailable');
  }
  const tab = await chrome.tabs.get(tabId).catch(() => null);
  if (!tab) throw new Error(`groupTabInto: tab ${tabId} is unavailable`);
  let gid = groupIdByName.get(name);
  if (gid != null) {
    // Only a group in the tab's own window: `tabs.group({groupId})` MOVES the
    // tab into the group's window, and a session's old group may sit in the
    // user's window (from before the agent window was validated), which would
    // drag a background agent tab into the window the user is working in.
    const ok = await chrome.tabGroups
      .get(gid)
      .then((g) => g.windowId === tab.windowId)
      .catch(() => false);
    if (!ok) {
      gid = null;
      groupIdByName.delete(name);
    }
  }
  if (gid == null) {
    // Reuse a same-titled group already in this window (survives SW restarts).
    const found = await chrome.tabGroups
      .query({ windowId: tab.windowId, title: name })
      .catch(() => []);
    if (found && found[0]) gid = found[0].id;
  }
  if (gid == null) {
    // createProperties.windowId is load-bearing: without it Chrome creates the
    // group in the *current* (last focused) window and MOVES the tab there,
    // which pulled every new agent tab out of the background agent window into
    // the window the user was working in (observed live: tabs.onAttached into
    // the user's window right after creation, then the emptied agent window
    // closed).
    gid = await chrome.tabs.group({ tabIds: tabId, createProperties: { windowId: tab.windowId } });
    await chrome.tabGroups.update(gid, { title: name, color: colorForName(name) });
  } else {
    await chrome.tabs.group({ groupId: gid, tabIds: tabId });
  }
  const grouped = await chrome.tabs.get(tabId);
  const groupInfo = await chrome.tabGroups.get(grouped.groupId);
  if (grouped.groupId !== gid || groupInfo.title !== name) {
    throw new Error(`groupTabInto: tab ${tabId} did not join group ${name}`);
  }
  groupIdByName.set(name, gid);
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
  let openerTargetId = '';
  let abGroup = creatingTabGroups.get(tabId) || '';
  try {
    const t = await chrome.tabs.get(tabId);
    if (t) {
      if (typeof t.openerTabId === 'number') {
        const op = tabs.get(t.openerTabId);
        if (op) openerTargetId = op.targetId;
      }
      if (t.groupId != null && t.groupId >= 0 && chrome.tabGroups) {
        const g = await chrome.tabGroups.get(t.groupId).catch(() => null);
        if (!abGroup && g && g.title) abGroup = g.title;
      }
    }
  } catch {}
  return { openerTargetId, abGroup };
}

// Resolve a stable identifier for the Chrome profile this worker runs in, for
// the `hello` handshake (issue #60). `profileId` is a UUID minted once and kept
// in this profile's chrome.storage.local — distinct per profile, no permission
// needed. `profileEmail` is included only when the optional `identity` permission
// is present and the profile is signed in; otherwise it's omitted (never throws).
async function buildHelloIdentity() {
  const extra = await extensionInstallIdentity(chrome);
  try {
    const KEY = 'ab_profile_id';
    const got = await chrome.storage.local.get(KEY);
    let id = got && got[KEY];
    if (!id) {
      id =
        (crypto && crypto.randomUUID && crypto.randomUUID()) ||
        'p-' + Math.abs(Date.now()).toString(36);
      await chrome.storage.local.set({ [KEY]: id });
    }
    extra.profileId = id;
  } catch {}
  try {
    if (chrome.identity && chrome.identity.getProfileUserInfo) {
      const info = await new Promise((resolve) => {
        try {
          chrome.identity.getProfileUserInfo({ accountStatus: 'ANY' }, resolve);
        } catch {
          resolve(null);
        }
      });
      if (info && info.email) extra.profileEmail = info.email;
    }
  } catch {}
  return extra;
}

// No size cap here. Chrome's 1 MB limit is on messages FROM the native host;
// messages sent TO it — this direction — may be up to 64 MiB (Chrome's native
// messaging docs). A 1 MB cap here, as briefly added in #342, turned ordinary
// results into errors: a viewport screenshot of a Wikipedia article is 905 KB
// of PNG, 1.2 MB once base64'd into the reply. It also serialised every message
// a second time just to measure it.
// `method` names the command a reply answers, for the size error (#530).
function postToHost(msg, method) {
  if (!port) return;
  try {
    port.postMessage(msg);
  } catch (e) {
    // A reply over Chrome's 64 MiB message limit: answer with an error that
    // says so, instead of leaving the caller to time out (#530).
    const fallback = oversizeReplyError(msg, e, method);
    if (fallback) {
      try {
        port.postMessage(fallback);
      } catch {}
      return;
    }
    // Otherwise the port died; onDisconnect will reconnect.
  }
}

function setBadge(tabId, kind) {
  // A lent tab shows that, whatever else is true of it.
  if (tabGrants.get(tabId)) { showGrantIndicator(tabId); return; }
  const map = { on: turnBadge, connecting: '…', error: '!' };
  const colors = { on: turnBadge ? '#2563eb' : '#16a34a', connecting: '#d97706', error: '#b91c1c' };
  try {
    chrome.action.setBadgeText({ tabId, text: map[kind] ?? '' });
    if (colors[kind]) chrome.action.setBadgeBackgroundColor({ tabId, color: colors[kind] });
  } catch {}
}

// A lent tab carries its own toolbar badge and tooltip: signals outside the
// page, so nothing a site could read changes. Chrome clears a tab's action
// state when it navigates; tabs.onUpdated puts it back.
function showGrantIndicator(tabId) {
  const grant = tabGrants.get(tabId);
  try {
    if (grant) {
      chrome.action.setBadgeText({ tabId, text: t('bg_grant_badge') });
      chrome.action.setBadgeBackgroundColor({ tabId, color: '#7c3aed' });
      chrome.action.setTitle({ tabId, title: t('bg_grant_action_title', grant.conversationTitle || t('bg_grant_untitled')) });
      return;
    }
    chrome.action.setTitle({ tabId, title: t('ext_action_title') });
  } catch {}
  if (tabs.has(tabId)) { setBadge(tabId, port ? 'on' : 'connecting'); return; }
  // `null` hands the tab back to the global badge (the turn count).
  try { void chrome.action.setBadgeText({ tabId, text: null }).catch(() => {}); } catch {}
}

function publishTabGrants() {
  try { chrome.runtime.sendMessage({ type: 'ab-tab-grants', grants: tabGrants.list() }).catch(() => {}); } catch {}
}

// Tell the panel service which tab a conversation may drive (null: none).
// A failure is returned, never thrown: the extension's grant and refusal are
// what protect the tab.
async function tellServiceGrant(conversationId, grant) {
  const state = hostConnection.snapshot();
  if (!port || !state.connected || !(state.agentPanel?.tabGrant >= 1)) return { error: t('bg_grant_service_unavailable') };
  try {
    const result = await agentPanelBridge.request('conversationTabGrant', { conversationId, grant: grant ? { tabId: grant.tabId, url: grant.url, title: grant.title } : null });
    return result.tabGrant;
  } catch (error) {
    return { error: String(error?.message || error) };
  }
}

// Take a lent tab back at once: refuse it from now on, let go of the
// debugger (no relay command can re-attach a refused tab), clear its
// indicator, then tell the service.
async function takeBackTab(tabId, { tellService = true } = {}) {
  const grant = await tabGrants.revoke(tabId);
  releasedTabs.add(tabId);
  setTimeout(() => releasedTabs.delete(tabId), 10_000);
  if (tabs.has(tabId)) detachTab(tabId, true);
  await chrome.debugger.detach({ tabId }).catch(() => {});
  showGrantIndicator(tabId);
  publishTabGrants();
  const service = grant && tellService ? await tellServiceGrant(grant.conversationId, null) : null;
  return { grant, service };
}

async function lendTab({ tabId, conversationId, conversationTitle }) {
  const tab = await chrome.tabs.get(tabId).catch(() => null);
  if (!tab) throw Object.assign(new Error('The tab is closed'), { reason: 'closed' });
  await loadOwnedTabs();
  await loadAgentPopups();
  if (ownedTabs.has(tabId) || agentPopups.has(tabId)) throw Object.assign(new Error('This tab is already the agent\'s'), { reason: 'agentTab' });
  const { grant, replaced, before } = await tabGrants.grant(tab, { conversationId, conversationTitle });
  showGrantIndicator(tabId);
  if (replaced) showGrantIndicator(replaced.tabId);
  publishTabGrants();
  const service = await tellServiceGrant(conversationId, grant);
  // A service that cannot hold the grant would run turns without it. Undo it
  // exactly: a tab taken back before stays refused, an earlier tab stays lent.
  if (service?.granted !== true) {
    await tabGrants.rollback(tabId, { before, replaced });
    showGrantIndicator(tabId);
    if (replaced) showGrantIndicator(replaced.tabId);
    publishTabGrants();
    throw Object.assign(new Error(service?.error || t('bg_grant_service_unavailable')), { reason: 'service' });
  }
  if (replaced) await takeBackTab(replaced.tabId, { tellService: false });
  return { grant, service };
}

async function handleTabGrantMessage(msg) {
  await tabGrants.load();
  if (msg.action === 'list') return { grants: tabGrants.list() };
  if (msg.action === 'grant') {
    if (!Number.isInteger(msg.tabId) || typeof msg.conversationId !== 'string' || !msg.conversationId) throw new Error('A tab and a conversation are required');
    return lendTab({ tabId: msg.tabId, conversationId: msg.conversationId, conversationTitle: typeof msg.conversationTitle === 'string' ? msg.conversationTitle : '' });
  }
  if (msg.action === 'revoke') {
    const tabId = Number.isInteger(msg.tabId) ? msg.tabId : tabGrants.forConversation(String(msg.conversationId || ''))?.tabId;
    if (!Number.isInteger(tabId)) return { grant: null, service: null };
    return takeBackTab(tabId);
  }
  throw new Error('Unknown tab grant action');
}

// ---- settings + UX (cursor overlay, connection notifications) -------------

let cursorEnabled = false;
const cursorOverlay = createCursorOverlay({
  // Never call sendCdpToTab here: its recovery may attach a tab the user detached.
  evaluate: (tabId, expression) => withRelayTimeout(
    chrome.debugger.sendCommand({ tabId }, 'Runtime.evaluate', {
      expression, returnByValue: true, awaitPromise: false,
    }),
    'cursor overlay',
  ),
});
function updateCursorEnabled() {
  cursorEnabled = cursorAllowed(cursorSettings);
  void cursorOverlay.setEnabled(cursorEnabled,
    [...tabs].filter(([, entry]) => entry.attached).map(([id]) => id));
}
// Settings the cursor depends on; cursorEnabled is derived from both, so the
// overlay stays dark under stealth mode whatever ab_cursor says.
const cursorSettings = { ab_cursor: false, ab_stealth: STEALTH_DEFAULT };
// A delayed startup read must not overwrite a newer storage change.
const changedCursorSettings = new Set();
let notifyEnabled = false;
let lastConnected = null;
// Idle auto-detach (issue #201): release chrome.debugger from a tab after this
// long with no CDP traffic, so Chrome's "started debugging this browser" bar
// shows only while an agent is actually driving — like Codex's own extension —
// instead of staying up for hours after the last command. The tab stays known
// to the relay (same `cb-tab-<id>` session); the next command re-attaches and
// replays the enabled domains transparently. 0 = never detach (old behaviour).
let idleDetachMs = IDLE_DETACH_DEFAULT_SECS * 1000;

function loadUxSettings() {
  try {
    chrome.storage.sync.get(
      { ab_cursor: false, ab_stealth: STEALTH_DEFAULT, ab_notify: false, ab_idle_detach_secs: IDLE_DETACH_DEFAULT_SECS },
      (s) => {
        if (!changedCursorSettings.has('ab_cursor')) cursorSettings.ab_cursor = !!s.ab_cursor;
        if (!changedCursorSettings.has('ab_stealth')) cursorSettings.ab_stealth = s.ab_stealth;
        updateCursorEnabled();
        notifyEnabled = !!s.ab_notify;
        idleDetachMs = idleDetachMsFrom(s.ab_idle_detach_secs);
      }
    );
  } catch {}
}
loadUxSettings();
try {
  chrome.storage.onChanged.addListener((ch, area) => {
    if (area !== 'sync') return;
    for (const key of ['ab_cursor', 'ab_stealth']) if (key in ch) changedCursorSettings.add(key);
    if ('ab_cursor' in ch) cursorSettings.ab_cursor = !!ch.ab_cursor.newValue;
    if ('ab_stealth' in ch) cursorSettings.ab_stealth = ch.ab_stealth.newValue;
    if ('ab_cursor' in ch || 'ab_stealth' in ch) updateCursorEnabled();
    if ('ab_notify' in ch) notifyEnabled = !!ch.ab_notify.newValue;
    if ('ab_idle_detach_secs' in ch)
      idleDetachMs = idleDetachMsFrom(ch.ab_idle_detach_secs.newValue);
  });
} catch {}

// Fire a (silent) desktop notification when the bridge connects/disconnects —
// only on an actual transition, and only if the user opted in (options page).
function notifyConnChange(connected) {
  if (connected === lastConnected) return;
  lastConnected = connected;
  if (!notifyEnabled) return;
  try {
    chrome.notifications.create('ab-conn-' + Date.now(), {
      type: 'basic',
      iconUrl: 'icons/icon128.png',
      title: connected ? t('bg_notify_connected_title') : t('bg_notify_disconnected_title'),
      message: connected
        ? t('bg_notify_connected_message')
        : t('bg_notify_disconnected_message'),
      silent: true,
    });
  } catch {}
}

// Mirror agent mouse activity to a friendly on-page cursor, drawn IN the page via
// the EXISTING chrome.debugger session (no extra permissions). Opt-in (off by
// default) because the overlay is a visible DOM node — leaving it off keeps
// automation invisible to the page. Best-effort; never blocks the real command.
function maybeDriveCursor(tabId, method, params) {
  if (!cursorEnabled || method !== 'Input.dispatchMouseEvent') return;
  const x = params && params.x;
  const y = params && params.y;
  if (typeof x !== 'number' || typeof y !== 'number') return;
  const click = params.type === 'mousePressed';
  void cursorOverlay.draw(tabId, x, y, click);
}

// ---- native messaging transport ------------------------------------------

function publishHostStatus() {
  try {
    chrome.runtime
      .sendMessage({ type: 'ab-host-state', ...hostStatusForUi(), tabCount: tabs.size, ...versionState() })
      .catch(() => {}); // Popup may be closed; state still lives in the worker.
  } catch {}
}

// ---- keeping the CLI and this extension current ---------------------------
//
// The host reports its version record in every pong (`cli`, see
// version-notice.js). The popup and side panel show it; their "Update CLI"
// button lands here as `ab-cli-upgrade`, which asks the host to upgrade itself
// in the background. Once the installed binary is newer than the running host
// (`cli.onDisk`), the worker reconnects so Chrome starts the new one — but only
// when the relay is idle by the same rule as this extension's own update, so a
// running task is never cut.

// A button-started upgrade: { state: 'running'|'failed', error?, command?, at }.
let cliUpgrade = null;
// Fast pings while an upgrade runs, so the pong's `onDisk` arrives promptly.
const CLI_UPGRADE_PING_MS = 3000;
const CLI_UPGRADE_TIMEOUT_MS = 5 * 60 * 1000;
let cliUpgradeTimer = null;
// The on-disk version the worker last reconnected for. A host that still
// reports an older version afterwards (the install went to another path) is
// not reconnected again for the same version.
let cliSwitchedTo = null;
let cliSwitchTimer = null;
// A running host may predate the binary on disk; reconnect once before judging it.
let legacyCliRechecked = false;

function hostStatusForUi() {
  const state = hostConnection.snapshot();
  return { ...state, legacyCli: state.legacyCli && legacyCliRechecked };
}

// What the popup and side panel need to show version notices.
function versionState() {
  let update = null;
  try {
    update = { ...updateStateSnapshot(), canApplyNow: updatePending && !applyingUpdate && currentUpdatePlan().apply };
  } catch {} // Before the update state is initialised at startup.
  return { cliUpgrade, update, extensionVersion: chrome.runtime.getManifest().version };
}

function stopCliUpgradePings() {
  if (cliUpgradeTimer) clearTimeout(cliUpgradeTimer);
  cliUpgradeTimer = null;
}

function pingWhileCliUpgrades() {
  stopCliUpgradePings();
  if (cliUpgrade?.state !== 'running') return;
  if (Date.now() - cliUpgrade.at > CLI_UPGRADE_TIMEOUT_MS) {
    cliUpgrade = { state: 'failed', error: t('bg_cli_upgrade_timeout'), at: Date.now() };
    publishHostStatus();
    return;
  }
  cliUpgradeTimer = setTimeout(() => {
    cliUpgradeTimer = null;
    postToHost({ method: 'ping' });
    pingWhileCliUpgrades();
  }, CLI_UPGRADE_PING_MS);
}

function startCliUpgrade() {
  const cli = hostConnection.snapshot().cli;
  if (!port || !cli) throw new Error(t('bg_cli_upgrade_no_host'));
  if (cliUpgrade?.state === 'running') return;
  cliUpgrade = { state: 'running', at: Date.now() };
  postToHost({ method: 'upgradeCli' });
  pingWhileCliUpgrades();
  publishHostStatus();
}

function onUpgradeCliResult(params) {
  if (cliUpgrade?.state !== 'running') return;
  if (params?.started === true) return; // the pong's onDisk reports the outcome
  cliUpgrade = {
    state: 'failed',
    error: typeof params?.error === 'string' ? params.error.slice(0, 300) : '',
    ...(typeof params?.command === 'string' && params.command ? { command: params.command.slice(0, 300) } : {}),
    at: Date.now(),
  };
  stopCliUpgradePings();
  publishHostStatus();
}

// After each pong: settle a running upgrade, and hand over to a newer binary.
function afterCliStatus() {
  const cli = hostConnection.snapshot().cli;
  if (!cli) {
    switchToNewCliWhenIdle();
    return;
  }
  if (cliUpgrade && cli.latest && compareVersions(cli.version, cli.latest) !== -1 && !cliSwitchPending(cli)) {
    // The running host is current: whatever was started has landed.
    cliUpgrade = null;
    stopCliUpgradePings();
    publishHostStatus();
  } else if (cliUpgrade?.state === 'running' && cliSwitchPending(cli)) {
    cliUpgrade = null; // installed; the switch notice takes over
    stopCliUpgradePings();
    publishHostStatus();
  } else if (cliUpgrade?.state === 'running' && cli.lastAutoUpgrade && !cli.lastAutoUpgrade.ok && cli.lastAutoUpgrade.at >= cliUpgrade.at) {
    onUpgradeCliResult({ started: false, error: cli.lastAutoUpgrade.error || '' });
  }
  switchToNewCliWhenIdle();
}

function switchToNewCliWhenIdle() {
  if (cliSwitchTimer) clearTimeout(cliSwitchTimer);
  cliSwitchTimer = null;
  const state = hostConnection.snapshot();
  const cli = state.cli;
  const recheckLegacy = state.connected && state.legacyCli && !legacyCliRechecked;
  if (!port || (!recheckLegacy && (!cliSwitchPending(cli) || cliSwitchedTo === cli.onDisk))) return;
  const plan = currentUpdatePlan();
  // Side panel requests ride the same port; let them finish too.
  if (!plan.apply || agentPanelBridge.pending.size > 0) {
    if (plan.appliesInMs != null) cliSwitchTimer = setTimeout(switchToNewCliWhenIdle, plan.appliesInMs + 250);
    return; // a command's end re-arms through the next pong
  }
  if (recheckLegacy) legacyCliRechecked = true;
  else cliSwitchedTo = cli.onDisk;
  const old = port;
  try { old.disconnect(); } catch {}
  // disconnect() from this side fires no onDisconnect: clean up the same way.
  hostGone(old, recheckLegacy ? 'Rechecking the installed chrome-use CLI' : 'Switching to the updated chrome-use CLI');
  connectHost();
}

function connectHost() {
  if (port || !hostReconnect.canAttempt()) return;
  try {
    port = chrome.runtime.connectNative(HOST_NAME);
    hostConnection.begin(port);
  } catch (e) {
    port = null;
    hostConnection.end(null, String(e?.message || e));
    hostReconnect.failed();
    publishHostStatus();
    return;
  }
  const connectedPort = port;
  connectedPort.onMessage.addListener((msg) => {
    if (port !== connectedPort) return;
    if (nativeFocusBridge.receive(msg)) return;
    if (agentPanelBridge.receive(msg)) return;
    if (hostConnection.receive(connectedPort, msg)) {
      hostReconnect.confirmed();
      notifyConnChange(true);
      publishHostStatus();
      pollScheduleNotifications();
      pollTurnNotifications();
    }
    void whenReady(() => {
      if (port === connectedPort) return onHostMessage(msg);
    });
  });
  connectedPort.onDisconnect.addListener(() => {
    hostGone(connectedPort, chrome.runtime.lastError?.message || 'Native host disconnected');
  });
  // A response proves that the native host exists and can exchange messages.
  // Older hosts can instead confirm themselves with their first real command.
  publishHostStatus();
  postToHost({ method: 'ping' });
  // Report our version + a stable per-profile id so the host can tell the
  // CLI/`doctor` which extension build is live AND which Chrome profile the relay
  // is bound to. With many profiles, "logged out" on a site is otherwise
  // indistinguishable from "wrong profile" — the id removes that ambiguity
  // (issue #60). The id is a UUID persisted in this profile's chrome.storage.local
  // (no extra permission). If the profile is signed into Chrome AND the optional
  // `identity` permission is granted, we also include the account email; absent
  // that, email is simply omitted. Best-effort; ignored by older hosts.
  void buildHelloIdentity().then((extra) => {
    if (port !== connectedPort) return;
    try {
      postToHost({
        method: 'hello',
        version: chrome.runtime.getManifest().version,
        // `call:<policy>` / `state`: the generic chrome.* door and the one-call
        // state read (0.5.25). The daemon feature-detects on these names, so a
        // CLI that wants them on an older extension says "update" instead of
        // sending a method Chrome answers with "wasn't found".
        // `attachTabById` (0.5.30): attach one tab by Chrome tab id (#456).
        // `releaseTab` (0.5.34): detach from a tab a session took with
        // `--force` (the user's own) when that session ends.
        capabilities: [
          'userTabGroupsV1',
          'nativeTabDuplicate',
          'downloadsApi',
          `call:${POLICY_VERSION}`,
          'state',
          'batchCommands',
          'attachTabById',
          'releaseTab',
          // `cliUpdate` (0.5.72): shows the CLI's version notices and sends
          // `upgradeCli` from the popup / side panel button.
          'cliUpdate',
          // `sitePolicy` (#781): `ABExt.sitePolicy` and `ABExt.siteApproval`,
          // per-site permissions and the managed policy.
          'sitePolicy',
        ],
        ...extra,
      });
    } catch {}
  });
  // Tell the daemon about everything we already have attached, then re-attach
  // the tabs we own (NOT the user's tabs — that's what kept the banner off their
  // pages).
  // Then hand the host the full list of what we hold, so it drops records of
  // tabs that closed while it was not told (#519).
  void Promise.allSettled([reannounceAttachedTabs(), reattachOwnedTabs()]).then(syncRelayTargets);
  // Start the proactive heartbeat so the worker stays alive while paired.
  scheduleKeepalivePing();
}

// The host port closed, or this worker closed it to start an updated CLI
// (`disconnect()` from this side fires no onDisconnect): forget it and retry.
function hostGone(connectedPort, error) {
  if (port !== connectedPort) return;
  const wasConnected = hostConnection.snapshot().connected;
  hostConnection.end(connectedPort, error);
  nativeFocusBridge.disconnect();
  for (const context of nativeFocusContexts.values()) context.cancel();
  nativeFocusContexts.clear();
  agentPanelBridge.disconnect(error);
  port = null;
  hostReconnect.failed();
  publishHostStatus();
  if (wasConnected) notifyConnChange(false);
  // Retain tab records; the new host will rediscover them after reconnect.
  for (const tabId of tabs.keys()) setBadge(tabId, 'connecting');
}

async function onHostMessage(msg) {
  if (!msg || typeof msg !== 'object') return;
  // Active host commands refresh the keepalive window so working sessions never go cold.
  scheduleKeepalivePing();
  // Optional keepalive; a newer host's pong also carries its version record.
  if (msg.method === 'pong') {
    afterCliStatus();
    return;
  }
  if (msg.method === 'upgradeCliResult') {
    onUpgradeCliResult(msg.params);
    return;
  }
  if (msg.method === 'ping') {
    postToHost({ method: 'pong' });
    return;
  }
  // Daemon (re)connected — re-announce + re-attach OUR tabs (not the user's).
  if (msg.method === 'attachAll') {
    await Promise.allSettled([reannounceAttachedTabs(), reattachOwnedTabs()]);
    syncRelayTargets();
    return;
  }
  if (typeof msg.id !== 'undefined' && msg.method === 'forwardCDPCommand') {
    // Request ids belong to the host connection that issued them. A new host
    // may reuse those ids, so a late result must never be replayed onto it.
    const commandPort = port;
    // A pending update never reloads under a running command, tab-bound or
    // not (Target.createTarget, ABExt.*), nor right after one (#524). A state
    // read (`status`, `doctor`) is not a session working and does not count.
    const counts = msg?.params?.method !== 'ABExt.state';
    if (counts) {
      hostCommandsInFlight++;
      lastHostCommandAt = Date.now();
    }
    const method = String(msg?.params?.method || '');
    try {
      const result = await handleForwardCdpCommand(msg);
      if (port === commandPort) postToHost({ id: msg.id, result }, method);
    } catch (err) {
      if (port === commandPort)
        postToHost({ id: msg.id, error: err instanceof Error ? err.message : String(err) }, method);
    } finally {
      if (counts) {
        hostCommandsInFlight = Math.max(0, hostCommandsInFlight - 1);
        lastHostCommandAt = Date.now();
        if (updatePending) scheduleUpdateApply();
      }
    }
  }
}

// ---- CDP command dispatch -------------------------------------------------

function tabForSession(sessionId) {
  return sessionToTab.get(sessionId) ?? childSessionToTab.get(sessionId) ?? null;
}

function tabForTarget(targetId) {
  for (const [tabId, t] of tabs.entries()) if (t.targetId === targetId) return tabId;
  return null;
}

// The STABLE Chrome tabId encoded in a `cb-tab-<tabId>` session id (#17), or
// null for any other session shape (child/iframe sessions). The tabId is the
// real source of truth: it survives the renderer-process swaps (cross-origin
// OAuth/SSO navs) that tear down the page's CDP target — which is why binding to
// it (like claude-in-chrome) rides through the hop that killed the old
// target/sessionId binding (issue #23).
function tabIdFromSession(sessionId) {
  const m = /^cb-tab-(\d+)$/.exec(sessionId || '');
  return m ? Number(m[1]) : null;
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
  const m = String((e && e.message) || e);
  return /different extension|Cannot access a chrome:\/\/|must request permission|Cannot access contents|Cannot attach to (this|the) target|took this tab back/i.test(
    m
  );
}

// Ensure the debugger is attached to a `cb-tab-<tabId>` session's tab, re-attaching
// across the transient window of a process swap (with a couple of short retries).
// Returns the tabId on success, or null when the tab is genuinely gone
// (closed / restricted). (issues #20.1, #23)
async function recoverSessionTab(sessionId, beforeRecoveryTarget = () => {}) {
  const tabId = resolveSessionTab(sessionId, sessionToTab, childSessionToTab);
  // 1) Fast path: the encoded Chrome tabId still exists — re-attach it (covers
  //    the common renderer-process swap where the tabId is preserved, #23).
  if (tabId != null) {
    for (let i = 0; i < 3; i++) {
      const tab = await chrome.tabs.get(tabId).catch(() => null);
      if (!eligible(tab)) break; // tabId is gone — fall through to targetId recovery
      beforeRecoveryTarget(tabId);
      try {
        await attachTab(tabId);
        if (tabs.has(tabId)) return tabId;
      } catch (e) {
        if (isRelayTimeoutError(e)) throw e;
        if (isDebuggerAccessDenied(e)) throw debuggerAccessError(e, await listForeignFrameExtensions());
        // Permanently off-limits (other extension's page etc.) — stop the tabId
        // fast-path and let the stable-targetId path below try a different tab.
        if (isPermanentAttachError(e)) break;
        // mid-swap: tab exists but isn't attachable yet — back off and retry.
      }
      // Sized for #23: a cross-process nav (Rakuten cart → checkout) needs
      // ~0.8s before the tab is attachable again. #342 shortened this to
      // 0.36s without a reproduction; restored.
      await new Promise((r) => setTimeout(r, 120 + i * 150));
    }
  }
  // 1b) Chrome replaced the tab (discarded by Memory Saver, or a prerender
  //    swap): it lives on under a new id. A discarded tab is reloaded in the
  //    background first (never activated), then attached and aliased.
  if (tabId != null) {
    await loadReplacedTabs();
    const replacement = followReplacement(replacedTabs, tabId);
    if (replacement !== tabId) {
      let tab = await chrome.tabs.get(replacement).catch(() => null);
      if (eligible(tab)) beforeRecoveryTarget(replacement);
      if (eligible(tab) && tab.discarded) {
        await reviveDiscardedTab(chrome.tabs, replacement).catch(() => false);
        tab = await chrome.tabs.get(replacement).catch(() => null);
      }
      if (eligible(tab)) {
        beforeRecoveryTarget(replacement);
        try {
          await attachTab(replacement);
          if (tabs.has(replacement)) {
            sessionToTab.set(sessionId, replacement); // alias dead session -> live tab
            forgetReplacement(replacedTabs, tabId);
            saveReplacedTabs();
            return replacement;
          }
        } catch (e) {
          if (isRelayTimeoutError(e)) throw e;
          if (isDebuggerAccessDenied(e)) throw debuggerAccessError(e, await listForeignFrameExtensions());
          // fall through to the stable-targetId path below
        }
      }
    }
  }
  // 2) The Chrome tabId is gone, but the CDP targetId is STABLE across the nav.
  //    Some cross-process hops (Mercari's signin token exchange) give the tab a
  //    NEW tabId while keeping the same target, so `cb-tab-<oldTabId>` can't be
  //    recovered by tabId. Find the tab now hosting our remembered targetId via
  //    chrome.debugger.getTargets(), attach it, and ALIAS the dead session to it
  //    so the daemon's session id keeps resolving. Longer window: this hop can
  //    take several seconds to settle (issue #24).
  const targetId = sessionTargets.get(sessionId);
  if (targetId) {
    for (let i = 0; i < 6; i++) {
      let targets;
      try {
        targets = await withRelayTimeout(
          chrome.debugger.getTargets(),
          'chrome.debugger.getTargets'
        );
      } catch (e) {
        if (isRelayTimeoutError(e)) throw e;
        targets = null;
      }
      const t = targets && targets.find((x) => x.id === targetId && x.tabId != null);
      if (t && t.tabId != null) {
        const tab = await chrome.tabs.get(t.tabId).catch(() => null);
        if (eligible(tab)) {
          beforeRecoveryTarget(t.tabId);
          // Preserve the diagnostic before attach yields; commits during the
          // replacement window must contribute to the same reload history.
          transferReloadState(reloadStates, tabId, t.tabId);
          try {
            await attachTab(t.tabId);
            if (tabs.has(t.tabId)) {
              sessionToTab.set(sessionId, t.tabId); // alias dead session -> live tab
              return t.tabId;
            }
          } catch (e) {
            if (isRelayTimeoutError(e)) throw e;
            if (isDebuggerAccessDenied(e)) throw debuggerAccessError(e, await listForeignFrameExtensions());
            // The tab hosting our target is a page we can never attach to — no
            // amount of waiting fixes that, so give up the recovery now.
            if (isPermanentAttachError(e)) return null;
            // not attachable yet — keep waiting for the swap to settle.
          }
        }
      }
      // Sized for #24: the Mercari sign-in token hop gives the tab a new id
      // and "takes seconds to settle" — this window is ~6.3s. #342 halved it
      // to ~2.85s without a reproduction; restored.
      await new Promise((r) => setTimeout(r, 300 + i * 300));
    }
  }
  return null;
}

// Send a CDP command to a tab, riding a debugger detach that can happen between
// our attach check and the command itself (a renderer-process swap mid-flight).
// On a detached-style failure, drop the stale handle, re-attach the stable tab,
// and retry once — so a cross-process nav never surfaces as a hard error (#23).
//
// `childSessionId` (optional): the flattened CDP session of a cross-origin
// (out-of-process) iframe child. When set, the command is dispatched to that
// child via a `{tabId, sessionId}` DebuggerSession (Chrome 125+) so e.g.
// Accessibility.getFullAXTree runs INSIDE the iframe instead of on the top
// frame — this is what lets the daemon pierce the GSI sign-in iframe over the
// relay. Omitted/undefined ⇒ the top (page) session, addressed by tabId alone.
// Internal initialization and idle replay must invalidate the same attachment
// as caller commands when their renderer stops answering.
async function trackedDebuggerCommand(target, method, params) {
  try {
    return await observeDebuggerCommand(
      method,
      withRelayTimeout(chrome.debugger.sendCommand(target, method, params),
        `chrome.debugger.sendCommand(${method})`),
    );
  } catch (error) {
    if (isRelayTimeoutError(error)) attachmentHealth.mark(target.tabId);
    throw error;
  }
}

const attachmentHealth = createAttachmentHealth();

// Best effort: a failed lookup leaves the access error as it was.
async function listForeignFrameExtensions() {
  const targets = await withRelayTimeout(
    chrome.debugger.getTargets(),
    'chrome.debugger.getTargets'
  ).catch(() => []);
  return foreignFrameExtensions(targets, chrome.runtime.id);
}

function tabCommandDependencies() {
  return {
    health: attachmentHealth,
    detachDebugger: async id => {
      // Recovery may reset only a tab already authorized by an attachment or
      // persisted ownership. Chrome still enforces protected-URL permissions.
      if (!tabs.has(id) && !ownedTabs.has(id) && !agentPopups.has(id))
        throw new Error('debugger recovery refused: tab is no longer authorized');
      await chrome.debugger.detach({ tabId: id });
    },
    attachTab,
    sendCommand: (target, command, args) => chrome.debugger.sendCommand(target, command, args),
    detachTab,
    recoverSessionTab,
    reportTabIfGone,
    foreignFrameExtensions: listForeignFrameExtensions,
  };
}

async function sendCdpToTab(tabId, method, params, childSessionId) {
  const blocked = stealthBlockReason(cursorSettings, method, params);
  if (blocked) throw new Error(blocked);
  return await observeDebuggerCommand(
    method,
    sendTabCommand(tabId, method, params, childSessionId, tabCommandDependencies()),
  );
}

function anyConnectedTab() {
  const it = tabs.keys().next();
  return it.done ? null : it.value;
}

// Refuse a command on a tab the user took back, or on a lent tab from any
// relay session but the one of the conversation it was lent to. Synchronous
// (tabGrants is loaded first), so it can run again right before each step.
function guardTab(tabId, agentSession) {
  if (tabId == null) return;
  const refusal = tabGrants.refusal(tabId, agentSession);
  if (refusal) throw new Error(refusal);
}

// Read-only reports about tabs; they name a tab without acting on it.
const TAB_READS = new Set(['ABExt.inspectTab', 'ABExt.tabPresence', 'ABExt.state', 'ABExt.attachedTargets', 'ABExt.userTabMetadata']);

async function handleForwardCdpCommand(msg) {
  const method = String(msg?.params?.method || '');
  const params = msg?.params?.params || undefined;
  const sessionId = typeof msg?.params?.sessionId === 'string' ? msg.params.sessionId : undefined;
  // The relay session that sent this (CLI 1.5.221+), for lent tabs.
  const agentSession = typeof msg?.params?.agentSession === 'string' ? msg.params.agentSession : '';
  await tabGrants.load();
  if (!TAB_READS.has(method)) {
    if (sessionId) guardTab(resolveSessionTab(sessionId, sessionToTab, childSessionToTab), agentSession);
    for (const key of ['targetId', 'sourceTargetId']) if (typeof params?.[key] === 'string') guardTab(tabForTarget(params[key]), agentSession);
    if (Number.isInteger(params?.chromeTabId)) guardTab(params.chromeTabId, agentSession);
  }

  // Non-CDP extension commands (ABExt.*) the daemon sends. `ungroupTab` removes a
  // tab from its per-session tab group so a `keep`-marked tab is left for the user
  // as a normal, ungrouped tab (the group can then be cleaned up). Best-effort.
  if (method === 'ABExt.ungroupTab') {
    const tabId = resolveSessionTab(sessionId, sessionToTab, childSessionToTab);
    if (tabId != null && chrome.tabs.ungroup) {
      try {
        await chrome.tabs.ungroup(tabId);
      } catch {}
    }
    return { ungrouped: tabId ?? null };
  }

  // `done --keep` (0.5.77): a tab the agent created is handed to the user. It
  // stops being the extension's: no longer re-attached after a worker restart
  // (so no debugging banner on it), no longer closable through the agent
  // ownership gate. Only an agent-created tab; anything else is not ours to
  // hand over.
  if (method === 'ABExt.releaseOwnedTab') {
    await loadOwnedTabs();
    const tid = typeof params?.targetId === 'string' ? params.targetId : '';
    const tabId = resolveSessionTab(sessionId, sessionToTab, childSessionToTab) ?? (tid ? tabForTarget(tid) : null);
    if (tabId == null) return { released: false, reason: 'not-attached' };
    if (!ownedTabs.has(tabId)) return { released: false, tabId, reason: 'not-owned' };
    await unmarkOwned(tabId);
    if (userTouched.removed(tabId)) saveUserTouched();
    releasedTabs.add(tabId);
    setTimeout(() => releasedTabs.delete(tabId), 10_000);
    detachTab(tabId, true);
    await chrome.debugger.detach({ tabId }).catch(() => {});
    return { released: true, tabId };
  }

  // Relabel this session's existing tab group. `session name` writes the new
  // label for tabs opened from then on; without this, tabs already open would
  // keep the old label forever and the user would see the task split across two
  // differently-named groups. Matching on the old title (rather than a cached
  // group id) is what lets it work after a service-worker restart.
  if (method === 'ABExt.renameGroup') {
    const from = typeof params?.from === 'string' ? params.from.trim() : '';
    const to = typeof params?.to === 'string' ? params.to.trim() : '';
    if (!from || !to) throw new Error('renameGroup: both `from` and `to` are required');
    if (!chrome.tabGroups || !chrome.tabGroups.query) {
      throw new Error('renameGroup: Chrome tab-group APIs are unavailable');
    }
    if (from === to) return { renamed: 0 };
    const groups = await chrome.tabGroups.query({ title: from }).catch(() => []);
    let renamed = 0;
    for (const g of groups || []) {
      try {
        await chrome.tabGroups.update(g.id, { title: to, color: colorForName(to) });
        groupIdByName.set(to, g.id);
        renamed++;
      } catch {}
    }
    groupIdByName.delete(from);
    // Report the count rather than a bare ok: zero means the group was not
    // found, which the caller must be able to tell apart from a rename.
    return { renamed };
  }

  // Drive the on-page cursor explicitly. `maybeDriveCursor` below mirrors
  // `Input.dispatchMouseEvent`, but on the relay a left click is dispatched
  // through the DOM instead (see `prefer_dom_dispatch` in the daemon), which
  // never produces a mouse event for it to mirror — so those clicks were
  // invisible even with the overlay switched on. The daemon resolves the element
  // centre and calls this directly for that path. No-op when the cursor is off.
  if (method === 'ABExt.driveCursor') {
    const tabId = resolveSessionTab(sessionId, sessionToTab, childSessionToTab);
    const x = Number(params?.x);
    const y = Number(params?.y);
    // `reason` matters: the daemon caches "disabled" to stop paying for the round
    // trip, but must NOT cache a transient no-tab (a session tab not registered
    // yet on the first click) — that would switch the cursor off for the rest of
    // the daemon's life, including the hide-during-screenshot it depends on.
    if (!cursorEnabled) return { drawn: false, reason: 'disabled' };
    if (tabId == null) return { drawn: false, reason: 'no-tab' };
    if (!Number.isFinite(x) || !Number.isFinite(y)) {
      return { drawn: false, reason: 'bad-coords' };
    }
    const drawn = await cursorOverlay.draw(tabId, x, y, !!params?.click);
    return drawn ? { drawn: true } : { drawn: false, reason: 'unconfirmed' };
  }

  // Hide/show the overlay around a capture so the cursor doesn't end up baked
  // into screenshots the agent then reasons about.
  if (method === 'ABExt.setCursorVisible') {
    const tabId = resolveSessionTab(sessionId, sessionToTab, childSessionToTab);
    if (!cursorEnabled || tabId == null) return { applied: false };
    return { applied: await cursorOverlay.visible(tabId, !!params?.visible) };
  }

  // On-demand adopt of a PRE-EXISTING tab (the user's own, or another session's).
  // Since we no longer eagerly attach the user's tabs (banner!), `adopt <spec>`
  // can't find them via the already-attached target list — so discover by
  // chrome.tabs metadata (no debugger, no banner), and attach ONLY the one match
  // (which announces it to the daemon and makes it ours). `spec` is a targetId or
  // a URL substring. On no match, return the candidate URLs so the daemon can
  // print a useful error.
  // Attach exactly one tab by Chrome tab id (#456): a pop-up the session's own
  // tab opened. No URL lookup, so no other tab can be attached in its place.
  // Not persisted into ownedTabs, like the onCreated pop-up path.
  if (method === 'ABExt.attachTabById') {
    const result = await attachTabById(params, {
      getTab: (tabId) => chrome.tabs.get(tabId),
      eligible,
      attachTab: (tabId) => attachTab(tabId),
    });
    // Recorded as an agent pop-up only when the tab itself shows it is one:
    // opened by an agent tab, or in a group that holds an agent tab. The
    // daemon's request names a tab id, which proves nothing about whose it is.
    // A failed read is unknown: agentPopup false, nothing recorded.
    await loadOwnedTabs();
    await loadAgentPopups();
    return await confirmAttachedPopup(result, {
      getTab: (tabId) => chrome.tabs.get(tabId),
      queryAll: () => chrome.tabs.query({}),
      isAgentTab: agentTabPredicate(ownedTabs, agentPopups),
      mark: (tabId) => markAgentPopup(tabId),
    });
  }

  // Let go of a tab a session took with `--force` (0.5.34): detach the
  // debugger from it, never close it. Agent-created tabs are refused here;
  // their session closes them.
  if (method === 'ABExt.releaseTab') {
    await loadOwnedTabs();
    return await releaseTab(params, {
      tabForTarget,
      isOwned: (tabId) => ownedTabs.has(tabId),
      detach: (tabId) => {
        // A released tab must not be re-attached by the onDetach recovery.
        releasedTabs.add(tabId);
        setTimeout(() => releasedTabs.delete(tabId), 10_000);
        return withRelayTimeout(chrome.debugger.detach({ tabId }), 'chrome.debugger.detach');
      },
      forget: (tabId) => detachTab(tabId, true),
    });
  }

  if (method === 'ABExt.adoptByUrl') {
    const spec = String(params?.spec || '').trim();
    const specL = spec.toLowerCase();
    let all = [];
    try {
      all = await chrome.tabs.query({});
    } catch {}
    const candidates = all.filter((t) => eligible(t));
    // Prefer an already-attached target whose id matches; else match a tab URL.
    const match =
      candidates.find((t) => tabs.get(t.id)?.targetId === spec) ||
      candidates.find((t) => (t.url || '').toLowerCase().includes(specL));
    if (!match || !match.id) {
      const extra = await buildHelloIdentity();
      return {
        targetId: null,
        candidates: candidates.map((t) => ({ url: t.url || '', title: t.title || '' })),
        profileId: extra.profileId || null,
        profileEmail: extra.profileEmail || null,
      };
    }
    guardTab(match.id, agentSession);
    const entry = await attachTab(match.id); // attaches + announces attachedToTarget
    // Do NOT markOwned: an adopted tab is the USER'S — attach it for this session,
    // but never persist it into the re-attach set. Persisting would re-attach the
    // user's tab on every SW restart forever, leaving Chrome's debugger banner
    // stuck on a page they own. On SW restart the adoption simply releases (banner
    // clears); re-`adopt` if still needed. Only agent-CREATED tabs are persisted.
    return { targetId: entry.targetId, url: match.url || '', title: match.title || '' };
  }

  // Browser-level tab diagnostics that remain available even when the page's
  // renderer/main thread is stuck. This deliberately uses chrome.tabs metadata
  // only: no Runtime/Page command, no navigation, and no page-state mutation.
  // Which tabs the relay is actually holding, for `tabs` to show alongside the
  // tab a session ASKED for. The daemon reports its pin as "current"; nothing
  // could say whether the relay still had that tab, so a mismatch looked like
  // a permissions bug (issue #217).
  if (method === 'ABExt.attachedTargets') {
    return { targets: attachedTargetsFrom(tabs.entries()) };
  }

  // Whether the tab holding a target is still open (#496), for `close` to
  // confirm a tab is gone before the CLI drops its right to close it. A
  // structured answer (`tabPresenceVersion`): `absent` only when Chrome's own
  // target registry no longer lists the target AND chrome.tabs reports the
  // exact tab id missing; any API error is `unknown`.
  if (method === 'ABExt.tabPresence') {
    const rawTabId = params?.tabId;
    return targetPresence(
      {
        targetId: params?.targetId,
        tabId: rawTabId == null ? null : Number(rawTabId),
      },
      {
        getTargets: () =>
          withRelayTimeout(chrome.debugger.getTargets(), 'chrome.debugger.getTargets'),
        getTab: (id) => chrome.tabs.get(id),
      },
    );
  }

  // A password manager's menu blocks the debugger on an agent tab and the tab
  // flip cannot close it (Chrome is not the frontmost app): take focus out of
  // the field it belongs to. See menu-blur.js.
  if (method === 'ABExt.nativeFocusProbe') {
    return nativeFocusBridge.request('probe', {});
  }
  if (method === 'ABExt.nativeFocusFinish') {
    const context = nativeFocusContexts.get(params?.token);
    nativeFocusContexts.delete(params?.token);
    return context ? context.finish() : nativeFocusBridge.request('finish', { token: params?.token });
  }
  if (method === 'ABExt.nativeFocusBegin') {
    const context = nativeFocusContexts.get(params?.token);
    if (!context) throw new Error('Native focus preparation is absent or expired');
    return context.begin();
  }
  if (method === 'ABExt.nativeFocusPrepare') {
    await loadOwnedTabs();
    const tid = typeof params?.targetId === 'string' ? params.targetId : '';
    const sid = typeof params?.sessionId === 'string' ? params.sessionId : sessionId;
    const boundTab = () => {
      if (!tid || !sid) throw new Error('Native focus requires both target and session identities');
      if (childSessionToTab.has(sid)) throw new Error('Native focus refuses a child-frame session');
      const fromSession = resolveSessionTab(sid, sessionToTab, childSessionToTab);
      const fromTarget = tabForTarget(tid);
      if (!Number.isInteger(fromSession)) throw new Error('Native focus session has no browser tab binding');
      // Attachment records are not Chrome's liveness authority.
      // A missing cache entry is checked against Chrome's target registry
      // below; a conflicting entry is always refused.
      if ((Number.isInteger(fromTarget) && fromSession !== fromTarget) || fromSession !== params?.chromeTabId) throw new Error('Native focus target and session name different Chrome tabs');
      return fromSession;
    };
    const context = await prepareNativeFocusRecovery(params, {
      boundTab,
      validateBinding: async () => (await withRelayTimeout(chrome.debugger.getTargets(), 'native focus target registry'))
        .some(target => target.id === tid && target.tabId === params.chromeTabId && target.type === 'page'),
      guard: id => guardTab(id, agentSession),
      isOwned: id => ownedTabs.has(id),
      getTab: id => chrome.tabs.get(id),
      getWindow: id => chrome.windows.get(id),
      getWindowTabs: id => chrome.tabs.query({ windowId: id }),
      focusWindow: id => chrome.windows.update(id, { focused: true }),
      request: (operation, args) => nativeFocusBridge.request(operation, args),
      revoke: token => nativeFocusBridge.revoke(token),
    });
    nativeFocusContexts.set(context.token, context);
    setTimeout(() => nativeFocusContexts.delete(context.token), 145000);
    return { prepared: true, token: context.token };
  }
  if (method === 'ABExt.blurFocusedField') {
    await loadOwnedTabs();
    const tid = typeof params?.targetId === 'string' ? params.targetId : '';
    const sid = typeof params?.sessionId === 'string' ? params.sessionId : sessionId;
    const tabId = (sid ? resolveSessionTab(sid, sessionToTab, childSessionToTab) : null) ?? (tid ? tabForTarget(tid) : null);
    try {
      return await blurFocusedField(tabId, {
        isOwned: (id) => {
          // Legacy callers carry their session inside params. Authorize the
          // resolved target, including again after frame enumeration yields.
          guardTab(id, agentSession);
          return ownedTabs.has(id);
        },
        getAllFrames: (id) => chrome.webNavigation.getAllFrames({ tabId: id }),
        executeScript: (injection) => chrome.scripting.executeScript(injection),
        // The visibilitychange nudge is visible to the page: stealth mode off only.
        stealth: cursorSettings.ab_stealth !== false,
      });
    } catch (e) {
      // The agent now waits for the user: say where the switch is.
      if (isSiteAccessError(e)) void notifySiteAccessNeeded({ storage: chrome.storage.local, notifications: chrome.notifications, t }).catch(() => {});
      throw e;
    }
  }

  if (method === 'ABExt.prepareScreenshot') {
    if (!sessionId || childSessionToTab.has(sessionId))
      throw new Error('screenshot_window_unverified: an explicit top-level session is required');
    await Promise.all([loadOwnedTabs(), loadAgentPopups()]);
    const tabId = resolveSessionTab(sessionId, sessionToTab, childSessionToTab);
    if (!Number.isInteger(tabId) || (!tabs.has(tabId) && !ownedTabs.has(tabId) && !agentPopups.has(tabId) && !tabGrants.get(tabId)))
      throw new Error('screenshot_window_unverified: the bound tab authority is unavailable');
    if (params && (typeof params !== 'object' || Array.isArray(params) || Object.keys(params).some(key => key !== 'allowWindowRestore') ||
        ('allowWindowRestore' in params && typeof params.allowWindowRestore !== 'boolean')))
      throw new Error('screenshot_window_unverified: only a boolean allowWindowRestore is supported');
    return await prepareScreenshotWindow(tabId, params?.allowWindowRestore === true, {
      guard: () => guardTab(tabId, agentSession),
      getPlatformInfo: () => withRelayTimeout(new Promise((resolve, reject) => {
        chrome.runtime.getPlatformInfo(info => {
          const error = chrome.runtime.lastError;
          if (error) reject(new Error(error.message)); else resolve(info);
        });
      }), 'screenshot platform metadata'),
      getTab: id => withRelayTimeout(chrome.tabs.get(id), 'screenshot tab metadata'),
      getWindow: id => withRelayTimeout(chrome.windows.get(id), 'screenshot window metadata'),
      updateWindow: (id, update) => withRelayTimeout(chrome.windows.update(id, update), 'screenshot window restoration'),
    });
  }

  if (method === 'ABExt.inspectTab') {
    const requestedSession = String(params?.sessionId || '');
    const requestedTarget = String(params?.targetId || '');
    // A cross-process navigation can move a stable target to a new Chrome tabId.
    // Prefer the target/session maps, but verify each candidate against
    // chrome.tabs before falling back to the tabId encoded in an old session.
    const candidates = [
      tabForTarget(requestedTarget),
      tabForSession(requestedSession),
      tabIdFromSession(requestedSession),
    ];
    const resolved = await resolveFirstLiveTab(candidates, (tabId) => chrome.tabs.get(tabId));
    if (!resolved) {
      const knownTabId = candidates.find((tabId) => tabId != null);
      if (knownTabId != null) {
        throw new Error(`inspectTab: Chrome tab ${knownTabId} no longer exists`);
      }
      throw new Error('inspectTab: no tab matches the requested session or target');
    }
    const { tabId, tab } = resolved;
    const entry = tabs.get(tabId);
    return {
      relayHealth: getRelayHealth(),
      chromeTabId: tabId,
      sessionId: requestedSession || entry?.sessionId || null,
      targetId: entry?.targetId || requestedTarget || null,
      url: tab.url || tab.pendingUrl || '',
      committedUrl: tab.url || '',
      pendingUrl: tab.pendingUrl || '',
      title: tab.title || '',
      status: tab.status || null,
      active: Boolean(tab.active),
      audible: Boolean(tab.audible),
      discarded: Boolean(tab.discarded),
      frozen: Boolean(tab.frozen),
      debuggerAttached: Boolean(entry && entry.attached !== false),
      windowId: tab.windowId,
    };
  }

  // Native Chrome tab duplication. This intentionally has no URL-based
  // fallback: callers use it when they need the semantics of Chrome's Duplicate
  // tab action, including navigation history. The duplicated tab becomes owned
  // by the requesting session but the user's previously visible tab is restored.
  if (method === 'ABExt.duplicateTab') {
    return await runDuplicateTab(params, {
      tabForTarget,
      getTab: (tabId) => chrome.tabs.get(tabId),
      getWindow: (windowId) => chrome.windows.get(windowId),
      eligible,
      getLastFocusedWindow: () => chrome.windows.getLastFocused(),
      getActiveTabs: (windowId) => chrome.tabs.query({ active: true, windowId }),
      duplicateTab: duplicateTabForTransaction,
      markOwned,
      unmarkOwned,
      groupTabInto,
      attachTab,
      completeTab: (tabId) => nativeDuplicateTabs.delete(tabId),
      isolateTab: async (tabId) => {
        detachTab(tabId, true);
        await chrome.debugger.detach({ tabId }).catch(() => {});
      },
      activateTab: (tabId) => {
        userTouched.agentActivating(tabId);
        return chrome.tabs.update(tabId, { active: true });
      },
      focusWindow: (windowId) => chrome.windows.update(windowId, { focused: true }),
      removeTab: async (tabId) => {
        try {
          await removeAgentTab(tabId, duplicateTabRemoval);
        } finally {
          nativeDuplicateTabs.delete(tabId);
        }
      },
    });
  }

  if (method === 'ABExt.downloadUrl') {
    return await startDownload(params, {
      download: (options) => chrome.downloads.download(options),
      search: (query) => chrome.downloads.search(query),
      now: () => Date.now(),
      sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
      makeToken: () =>
        (globalThis.crypto?.randomUUID?.() || `${Date.now()}-${Math.random()}`).replaceAll(
          '.',
          '-'
        ),
    });
  }

  if (method === 'ABExt.listDownloads') {
    return await listDownloads(params, {
      search: (query) => chrome.downloads.search(query),
    });
  }

  if (method === 'ABExt.clearDownloads') {
    return await clearDownloads(params, {
      erase: (query) => chrome.downloads.erase(query),
    });
  }

  // Generic, allow-listed door onto chrome.* (see api-passthrough.js). The
  // point is that the NEXT CLI feature built on an existing browser API needs
  // no extension release. Ownership for mutations is the persisted
  // `ownedTabs` set only: adopted tabs are readable through here but not
  // closable or movable, same as `close` treats them.
  if (method === 'ABExt.userTabMetadata') {
    const all = await chrome.tabs.query({});
    // Titles and URLs come from the native browser inventory; no large URL/content payload here.
    return { version: 1, tabs: all.map(t => ({
      id:t.id,windowId:t.windowId,index:t.index,active:t.active,pinned:t.pinned,
      audible:t.audible,incognito:t.incognito,groupId:t.groupId,
    })) };
  }

  if (method === 'ABExt.organizeUserTabs' || method === 'ABExt.undoOrganizeUserTabs') {
    const organizer = await getUserTabOrganizer();
    return method === 'ABExt.organizeUserTabs' ? organizer.organize(params) : organizer.undo(params);
  }

  if (method === 'ABExt.call') {
    // After a service-worker restart the persisted ownership set is loaded
    // lazily; a mutation arriving before that would be refused as "not owned".
    await loadOwnedTabs();
    // An agent raising its own tab is not the user switching to it.
    if (params?.namespace === 'tabs' && params?.method === 'update' && params?.args?.[1]?.active === true) {
      for (const id of [params.args[0]].flat()) if (Number.isInteger(id)) userTouched.agentActivating(id);
    }
    return await executeCall(params, {
      api: chrome,
      isOwned: (tabId) => ownedTabs.has(tabId),
      ungroup: agentTabRemoval.ungroup,
      tabsInGroup: async (groupId) =>
        (await chrome.tabs.query({ groupId }).catch(() => [])).map((t) => t.id),
      tabsInWindow: async (windowId) =>
        (await chrome.tabs.query({ windowId }).catch(() => [])).map((t) => t.id),
    });
  }

  // Everything the daemon otherwise assembles from several calls, in one:
  // what the relay holds, what it owns, how it is configured. Ids only.
  // Per-site permissions (#781): the person's and the administrator's layers,
  // and approval requests the daemon files and polls.
  if (method === 'ABExt.sitePolicy') return sitePermissions.policy();
  if (method === 'ABExt.siteApproval') return sitePermissions.approval(params);

  if (method === 'ABExt.state') {
    await loadOwnedTabs();
    return {
      version: chrome.runtime.getManifest().version,
      relayHealth: getRelayHealth(),
      policy: policySummary(),
      userTabGroupsVersion: USER_TAB_GROUPS_VERSION,
      userTabGroupsJoin: USER_TAB_GROUPS_JOIN,
      connected: port != null,
      attachedTargets: attachedTargetsFrom(tabs.entries()),
      ownedTabs: [...ownedTabs],
      // Agent tabs the user switched to (0.5.77): cleanup leaves them open.
      userTouchedTabs: await userTouchedLoaded.then(() => userTouched.list()),
      groups: [...groupIdByName.entries()].map(([name, id]) => ({ name, id })),
      unresolvedOperations: relayUnresolvedOperations(),
      // Whether a downloaded update waits, and how many attached tabs would
      // hold it back now (#519: records of closed tabs never do).
      updatePending,
      updateBlockedBy: attachedTargetsFrom(tabs.entries()).filter((t) => t.attached).length,
      // 0.5.35 (#524): attached tabs no longer hold an update back for good;
      // it waits for a quiet relay. What it waits for, and how long.
      update: updateStateSnapshot(),
      agentWindowId,
      agentWindowError,
      agentWindowRejected,
      tabGrants: (await tabGrants.load(), tabGrants.list().map(({ tabId, conversationId }) => ({ tabId, conversationId }))),
      cursorEnabled,
      cursorCleanupPending: cursorOverlay.pendingTabs(),
      stealthMode: cursorSettings.ab_stealth !== false,
      notifyEnabled,
      idleDetachMs,
      // 0.5.92: the optional http(s) access menu-blur.js needs.
      siteAccess: await hasSiteAccess(chrome.permissions),
    };
  }

  // Atomic batch command execution: dispatches multiple commands in a single IPC roundtrip.
  if (method === 'ABExt.batchCommands') {
    const commands = Array.isArray(params?.commands) ? params.commands : [];
    const cleanup = commands.some(cmd => cmd && isAttachmentCleanup(cmd.method, cmd.params));
    if (cleanup && !sessionId) throw new Error('background cleanup requires an explicit attached session');
    const resolved = resolveSessionTab(sessionId, sessionToTab, childSessionToTab);
    const tabId = cleanup ? resolved : resolved ??
      (typeof params?.targetId === 'string' ? tabForTarget(params.targetId) : null) ??
      anyConnectedTab();
    if (tabId == null) throw new Error('no attached tab for ABExt.batchCommands');
    guardTab(tabId, agentSession);
    if (cleanup && tabs.get(tabId)?.attached !== true)
      throw new Error('background cleanup requires the existing attachment; it was not reattached');
    const childSid =
      sessionId && tabIdFromSession(sessionId) == null && childSessionToTab.has(sessionId)
        ? sessionId
        : undefined;
    const results = [];
    const stopOnError = params?.stopOnError !== false;
    for (let i = 0; i < commands.length; i++) {
      const cmd = commands[i];
      if (!cmd || typeof cmd.method !== 'string') continue;
      try {
        // Each step: the user may take the tab back while the batch runs.
        const res = await dispatchToTab(tabId, cmd.method, cmd.params, childSid, agentSession);
        results.push({ index: i, method: cmd.method, success: true, result: res });
      } catch (err) {
        results.push({
          index: i,
          method: cmd.method,
          success: false,
          error: err?.message || String(err),
        });
        if (stopOnError) break;
      }
    }
    return { results };
  }

  // Browser-level Target methods that map onto chrome.tabs.
  if (method === 'Target.createTarget') {
    const url = typeof params?.url === 'string' && params.url ? params.url : 'about:blank';
    // `dedicatedWindow` (opt-in daemon hint): put agent tabs in a separate
    // window in the same profile instead of the user's active window.
    const dedicated = params?.dedicatedWindow === true;
    const tab = dedicated
      ? await createAgentTab(url)
      : await chrome.tabs.create({ url, active: false });
    if (!tab || !tab.id) throw new Error('createTarget: no tab id');
    const group = typeof params?.agentGroup === 'string' ? params.agentGroup.trim() : '';
    if (group) creatingTabGroups.set(tab.id, group);
    try {
      void conversationWindows.observe(agentSession, tab.id).catch(() => {});
      await markOwned(tab.id); // agent-created, ours to attach after worker restart
      if (group) {
        try { await groupTabInto(tab.id, group); } catch {}
      }
      // Most targets attach immediately. Transient failures get bounded, growing
      // delays rather than one fixed sleep; permanent refusals never retry.
      const t = await attachCreatedTab(tab.id, {
        attach: attachTab,
        wait: ms => new Promise(r => setTimeout(r, ms)),
        permanent: isPermanentAttachError,
      });
      return { targetId: t.targetId };
    } finally {
      creatingTabGroups.delete(tab.id);
    }
  }
  if (method === 'Target.closeTarget') {
    const tid = typeof params?.targetId === 'string' ? params.targetId : '';
    let tabId = tid ? tabForTarget(tid) : null;
    // A tab the user lent stays theirs: the agent never closes it.
    await tabGrants.load();
    if (!Number.isInteger(tabId) && tid) {
      // A protected menu can prevent attachment without closing the tab.
      // Resolve its exact live target through Chrome, never by URL or title.
      await Promise.all([loadOwnedTabs(), loadAgentPopups()]);
      const targets = await withRelayTimeout(chrome.debugger.getTargets(), 'close target registry');
      const matches = targets.filter(target => target.id === tid && target.type === 'page' && Number.isInteger(target.tabId));
      if (matches.length !== 1) return { success: false };
      tabId = matches[0].tabId;
      // Missing attachment state cannot authorize closing a user's tab.
      if (!ownedTabs.has(tabId) && !agentPopups.has(tabId)) return { success: false };
      guardTab(tabId, agentSession);
    }
    if (!Number.isInteger(tabId)) return { success: false };
    if (tabGrants.get(tabId)) throw new Error(LENT_TAB_CLOSE);
    // Right after a worker restart both sets are still on disk; an unloaded
    // set would make an agent tab look like a user's and skip the ungroup.
    await Promise.all([loadOwnedTabs(), loadAgentPopups()]);
    try {
      await removeAgentTab(tabId, agentTabRemoval);
    } catch {
      return { success: false };
    }
    return { success: true };
  }
  if (method === 'Target.activateTarget') {
    const tid = typeof params?.targetId === 'string' ? params.targetId : '';
    const tabId = tid ? tabForTarget(tid) : null;
    if (tabId) {
      const tab = await chrome.tabs.get(tabId).catch(() => null);
      if (tab?.windowId)
        await chrome.windows.update(tab.windowId, { focused: true }).catch(() => {});
      userTouched.agentActivating(tabId);
      await chrome.tabs.update(tabId, { active: true }).catch(() => {});
    }
    return {};
  }

  // Everything else → chrome.debugger on the resolved tab.
  //
  // A daemon-supplied sessionId/targetId MUST resolve to a real attached tab.
  // The old code fell through to anyConnectedTab() when it didn't, which
  // silently ran the command (eval/screenshot/network) on an arbitrary tab —
  // exactly the "ran on the wrong page with no warning" failure in issue #8.1,
  // and the blank-screenshot symptom after a service-worker restart (#8.2).
  // Fail loudly instead so the agent sees an actionable error, not bad data.
  const cleanup = isAttachmentCleanup(method, params);
  if (cleanup && !sessionId) throw new Error('background cleanup requires an explicit attached session');

  const childSid =
    sessionId && tabIdFromSession(sessionId) == null && childSessionToTab.has(sessionId)
      ? sessionId
      : undefined;
  let tabId;
  if (sessionId) {
    // The stable Chrome tabId encoded in `cb-tab-<tabId>` is the source of truth
    // (it survives renderer-process swaps; the CDP target/sessionId does not).
    // Honor a recovered alias first, then fall back to the encoded id if the
    // detach handler cleared the maps. Ensure the debugger is attached,
    // re-attaching across a cross-process nav before failing (issues #20.1, #23).
    // `tabForSession` still covers child/iframe sessions that aren't `cb-tab-*`.
    tabId = resolveSessionTab(sessionId, sessionToTab, childSessionToTab);
    if (tabId == null) {
      throw new Error(`unknown sessionId ${sessionId} for ${method}`);
    }
    guardTab(tabId, agentSession);
    if (cleanup && tabs.get(tabId)?.attached !== true)
      throw new Error('background cleanup requires the existing attachment; it was not reattached');
    let browserNavigationReady = false;
    if (!tabs.has(tabId) && canUseBrowserNavigationFallback(method, params, childSid)) {
      let missing = false;
      const live = await withRelayTimeout(chrome.tabs.get(tabId), 'chrome.tabs.get(Page.navigate)').catch(error => {
        if (!isMissingTabError(error, tabId)) throw error;
        missing = true;
        return null;
      });
      if (!missing) {
        if (!eligible(live)) throw new Error(`Chrome tab ${tabId} is not a navigable web tab`);
        browserNavigationReady = true;
      }
      // Only a confirmed missing id enters the existing replacement recovery.
      // A live stable tab needs no attachment to navigate out of a protected page.
    }
    if (!browserNavigationReady && !tabs.has(tabId)) {
      const beforeRecoveryTarget = canUseBrowserNavigationFallback(method, params, childSid)
        ? id => guardTab(id, agentSession) : undefined;
      const recovered = await recoverSessionTab(sessionId, beforeRecoveryTarget);
      if (!recovered) {
        // A dead tab's host record goes now, so the next getTargets does not
        // list it and attaching to it fails (#519).
        await reportTabIfGone(tabId);
        throw new Error(
          `stale sessionId ${sessionId} for ${method}: its tab is gone (closed, ` +
            `navigated across processes, or lost after an extension restart). ` +
            `Re-attach by re-opening your target URL before retrying.`
        );
      }
      tabId = recovered;
      guardTab(tabId, agentSession);
    }
  } else if (typeof params?.targetId === 'string') {
    tabId = tabForTarget(params.targetId);
    if (!tabId) throw new Error(`no attached tab for targetId ${params.targetId} (${method})`);
  } else {
    // No session/target specified — a browser-level command that legitimately
    // applies to any attached tab.
    tabId = anyConnectedTab();
  }
  if (tabId == null) throw new Error(`no attached tab for ${method}`);

  // A flattened child (cross-origin iframe / worker) session — NOT a `cb-tab-*`
  // page session — is dispatched to that sub-frame directly so e.g.
  // Accessibility.getFullAXTree pierces the OOPIF instead of re-reading the top
  // frame. `cb-tab-*` ids resolve to the page (no child sessionId). (OAuth/OOPIF)

  // Idle auto-detach bookkeeping (issue #201): re-attach a released tab before
  // the command and count in-flight commands so the sweep never detaches under
  // a running one. State-setting commands are remembered for replay.
  let entry = tabs.get(tabId);
  if (entry && (method === 'Page.navigate' || method === 'Page.reload')) {
    // An explicit navigation/reload is a recovery action. Do not let the prior
    // page's diagnosis prevent the caller from escaping it.
    resetReloadLoop(entry.reloadState);
  } else if (entry) {
    const loop = activeReloadLoop(entry.reloadState);
    if (loop) {
      throw new Error(
        `reload loop detected: ${loop.count} top-level commits to ${loop.url} within ` +
          `${loop.windowMs}ms. The page is repeatedly replacing itself, so its DOM is not ` +
          `stable enough to drive. Try a deep link or adopt an already-rendered tab.`
      );
    }
  }
  // An explicit top-level navigation uses Chrome's tab API. Requiring a
  // debugger attachment first would trap the caller on a protected overlay.
  if (canUseBrowserNavigationFallback(method, params, childSid)) {
    const result = await dispatchToTab(tabId, method, params, childSid, agentSession);
    if (result?.relayFallback?.method === 'browser-level chrome.tabs.update') {
      attachmentHealth.navigationStarted(tabId);
    }
    return result;
  }
  // A timed-out capture may still be running. Its compositor reset is sent
  // once on this existing attachment, without consuming the unhealthy marker.
  const reset = cleanup ? false : await attachmentHealth.recover(tabId, tabCommandDependencies());
  if (reset && childSid)
    throw new Error('tab_reset: child session invalidated; rediscover frames before retrying');
  if (reset) entry = tabs.get(tabId);
  if (!reset && entry && entry.attached === false) await reattachTab(tabId, entry);
  const rememberAfterSuccess = method === 'Emulation.clearDeviceMetricsOverride' ||
    (method === 'Emulation.setUserAgentOverride' && params?.userAgent === '') ||
    (method === 'Emulation.setTouchEmulationEnabled' && params?.enabled === false);
  if (entry) {
    entry.inflight++;
    entry.lastActivity = Date.now();
    if (!childSid && !rememberAfterSuccess) rememberReplayable(entry, method, params);
  }
  try {
    const result = await dispatchToTab(tabId, method, params, childSid, agentSession);
    // A refused or unconfirmed reset must retain the last known override.
    if (!childSid && rememberAfterSuccess) rememberReplayable(entry, method, params);
    return result;
  } finally {
    if (entry) {
      entry.inflight = Math.max(0, entry.inflight - 1);
      entry.lastActivity = Date.now();
    }
  }
}

async function dispatchToTab(tabId, method, params, childSid, agentSession = '') {
  // Again here: recovery and earlier steps awaited Chrome, and the tab may
  // have been taken back meanwhile.
  guardTab(tabId, agentSession);
  if (isAttachmentCleanup(method, params) && tabs.get(tabId)?.attached !== true)
    throw new Error('background cleanup requires the existing attachment; it was not reattached');
  void conversationWindows.observe(agentSession, tabId).catch(() => {});
  // Re-enabling Runtime can leave a stale state; bounce it (matches upstream).
  if (method === 'Runtime.enable') {
    try {
      await sendCdpToTab(tabId, 'Runtime.disable', undefined, childSid);
      await new Promise((r) => setTimeout(r, 30));
    } catch {}
    return await sendCdpToTab(tabId, 'Runtime.enable', params, childSid);
  }
  // Mirror agent mouse activity to the friendly on-page cursor (opt-in; best-effort).
  maybeDriveCursor(tabId, method, params);
  if (canUseBrowserNavigationFallback(method, params, childSid)) {
    return await navigateTabWithBrowserFallback(params, {
      navigateWithDebugger: () => sendCdpToTab(tabId, method, params),
      updateTab: (url) => {
        guardTab(tabId, agentSession);
        return withRelayTimeout(chrome.tabs.update(tabId, { url }), 'chrome.tabs.update(Page.navigate)');
      },
    });
  }
  return await sendCdpToTab(tabId, method, params, childSid);
}

// ---- attach / detach ------------------------------------------------------

// Include target discovery and announcement in the shared attachment, so a
// second caller cannot replace an entry whose commands are already in flight.
const attachTab = createTabAttachmentQueue(attachTabOnce);

async function attachTabOnce(tabId, transactionIsActive) {
  const existing = tabs.get(tabId);
  if (existing && existing.attached !== false) return existing;
  if (existing) return await reattachTab(tabId, existing);
  const dbg = { tabId };
  try {
    const attaching = chrome.debugger.attach(dbg, '1.3');
    if (transactionIsActive) {
      // Promise.race cannot cancel attach either. A late acknowledgement must
      // not leave an expired recovery attached invisibly.
      void attaching.then(() => {
        if (!transactionIsActive())
          return withRelayTimeout(chrome.debugger.detach(dbg), 'detach late recovery attachment');
      }).catch(error => console.warn('[ab-connect] late recovery attachment cleanup', String(error)));
    }
    await withRelayTimeout(attaching, 'chrome.debugger.attach');
  } catch (e) {
    // After a service-worker restart, chrome.debugger may still be bound to
    // this tab from the previous instance — "Another debugger is already
    // attached". The tab is still controllable via {tabId}, so don't skip it
    // (skipping is why existing tabs went un-announced and the daemon opened a
    // blank tab instead). Re-announce it. Any other error (restricted page) is
    // surfaced and the caller skips this tab.
    const msg = String((e && e.message) || e);
    if (transactionIsActive || !/already attached|already being debugged/i.test(msg)) throw e;
  }
  // Resolve identity through Chrome's browser-level target registry first.
  // Unlike Target.getTargetInfo on the page session, getTargets does not wait
  // for the renderer main thread. A white-screen tab with an infinite JS loop
  // can therefore still be attached and announced instead of being mislabeled
  // as gone (issue #157).
  let targetInfo = null;
  try {
    targetInfo = targetInfoForTab(
      await withRelayTimeout(chrome.debugger.getTargets(), 'chrome.debugger.getTargets'),
      tabId
    );
  } catch {}
  if (!targetInfo) {
    const rememberedTargetId = sessionTargets.get(`cb-tab-${tabId}`);
    const tab = await chrome.tabs.get(tabId).catch(() => null);
    if (rememberedTargetId && tab) {
      targetInfo = {
        targetId: rememberedTargetId,
        type: 'page',
        url: tab.url || tab.pendingUrl || '',
        title: tab.title || '',
        attached: true,
      };
    }
  }
  if (!targetInfo) {
    const info = /** @type {any} */ (
      await trackedDebuggerCommand(dbg, 'Target.getTargetInfo')
    );
    targetInfo = info?.targetInfo;
  }
  const targetId = String(targetInfo?.targetId || '');
  if (!targetId) throw new Error('attachTab: no targetId');
  if (transactionIsActive && !transactionIsActive()) {
    await withRelayTimeout(chrome.debugger.detach(dbg), 'detach expired attachment').catch(() => {});
    throw new Error('attachTab: transaction cancelled');
  }
  // Derive the session id from the STABLE Chrome tabId, not a monotonic counter
  // (issue #17). A tab's chrome.debugger session can be torn down and
  // re-established — cross-process navigation, a service-worker restart wiping
  // these in-memory maps, DevTools stealing the debugger — and each time the tab
  // re-attaches. With a counter, re-attach minted a BRAND-NEW `cb-tab-N`, which
  // orphaned the daemon's binding (it's still pinned to the old id and the relay
  // never tells it to rebind) → permanent "stale sessionId / tab is gone". The
  // tabId is stable across all of that, so `cb-tab-<tabId>` restores the SAME
  // session the daemon already holds → eval/snapshot auto-follow the new page.
  const { openerTargetId, abGroup } = await tabScopeHints(tabId);
  if (transactionIsActive && !transactionIsActive()) {
    await withRelayTimeout(chrome.debugger.detach(dbg), 'detach expired attachment').catch(() => {});
    throw new Error('attachTab: transaction cancelled');
  }
  // The tab may have closed while this attach was awaiting Chrome. Its
  // onRemoved already ran and found nothing to remove, so registering it now
  // would leave an attached record of a dead tab forever (#519).
  if (removedTabs.has(tabId)) {
    await withRelayTimeout(chrome.debugger.detach(dbg), 'detach closed tab').catch(() => {});
    throw new Error(`attachTab: tab ${tabId} was closed while attaching`);
  }
  const sessionId = `cb-tab-${tabId}`;
  const entry = newTabEntry(tabId, sessionId, targetId, true);
  tabs.set(tabId, entry);
  void cursorOverlay.attached(tabId);
  sessionToTab.set(sessionId, tabId);
  rememberSessionTarget(sessionId, targetId);
  setBadge(tabId, port ? 'on' : 'connecting');
  const announced = await announceAttachedTab(tabId, entry, targetInfo, { openerTargetId, abGroup });
  if (announced === 'gone') throw new Error(`attachTab: tab ${tabId} was closed while attaching`);

  // Domain initialization is best-effort and must not hold the attach result
  // hostage to an unresponsive renderer. Register the stable tab/session first,
  // then arm Page and OOPIF support in the background.
  void trackedDebuggerCommand(dbg, 'Page.enable').catch(() => {});
  void trackedDebuggerCommand(dbg, 'Target.setAutoAttach', {
    autoAttach: true,
    flatten: true,
    waitForDebuggerOnStart: false,
  }).catch(() => {});
  return entry;
}

function newTabEntry(tabId, sessionId, targetId, attached) {
  let reloadState = reloadStates.get(tabId);
  if (!reloadState) {
    reloadState = newReloadState();
    reloadStates.set(tabId, reloadState);
  }
  return {
    sessionId,
    targetId,
    // false while idle-detached (issue #201): the relay still knows the tab, the
    // debugger is simply released until the next command.
    attached,
    lastActivity: Date.now(),
    inflight: 0,
    // State-setting commands (X.enable, Emulation.set*, …) the daemon sent on
    // this tab, replayed after an idle re-attach so the session looks untouched.
    replay: new Map(),
    // Top-frame commit history used to turn an otherwise silent same-URL
    // reload storm into an actionable command error (#211).
    reloadState,
  };
}

function rememberReplayable(entry, method, params) {
  if (entry && entry.replay) rememberReplayableIn(entry.replay, method, params);
}

// Re-attach an idle-detached tab and restore its session state. The `cb-tab-<id>`
// session id is stable, so the daemon's binding needs no update.
async function reattachTab(tabId, entry) {
  if (entry.attached !== false) return entry;
  if (entry.reattaching) return await entry.reattaching;
  entry.reattaching = (async () => {
    const dbg = { tabId };
    try {
      await withRelayTimeout(chrome.debugger.attach(dbg, '1.3'), 'chrome.debugger.attach');
    } catch (e) {
      const msg = String((e && e.message) || e);
      if (!/already attached|already being debugged/i.test(msg)) throw e;
    }
    entry.attached = true;
    void cursorOverlay.attached(tabId);
    entry.userDetached = false;
    entry.lastActivity = Date.now();
    setBadge(tabId, port ? 'on' : 'connecting');
    const arm = async (method, params) => {
      try {
        await trackedDebuggerCommand(dbg, method, params);
      } catch {}
    };
    await arm('Page.enable');
    await arm('Target.setAutoAttach', {
      autoAttach: true,
      flatten: true,
      waitForDebuggerOnStart: false,
    });
    for (const { method, params } of entry.replay.values()) {
      if (method === 'Page.enable' || method === 'Target.setAutoAttach') continue;
      // A binding exposed with stealth off is not re-added once it is on again.
      if (stealthBlockReason(cursorSettings, method, params)) continue;
      await arm(method, params);
    }
    return entry;
  })();
  try {
    return await entry.reattaching;
  } finally {
    entry.reattaching = null;
  }
}

// Release the debugger from a tab with no CDP traffic for `idleDetachMs`, but
// keep the relay's record of it: no Target.detachedFromTarget goes to the host,
// so the daemon keeps its session and the next command transparently
// re-attaches. Child (OOPIF) sessions die with the attachment; Target.setAutoAttach
// re-announces them on re-attach.
function softDetachTab(tabId, entry, reason) {
  if (!entry || entry.attached === false) return;
  entry.attached = false;
  entry.lastActivity = Date.now();
  if (reason === 'user') entry.userDetached = true;
  for (const [sid, tid] of childSessionToTab.entries())
    if (tid === tabId) childSessionToTab.delete(sid);
  if (reason !== 'user') chrome.debugger.detach({ tabId }).catch(() => {});
}

async function sweepIdleTabs() {
  for (const tabId of selectIdleTabs(tabs.entries(), Date.now(), idleDetachMs)) {
    // Chrome reports an active tab in each window. Preserve all of them,
    // including a page the user is reading without producing CDP traffic.
    // An unavailable status is not evidence that a tab is safe to release.
    const tab = await chrome.tabs.get(tabId).catch(() => null);
    if (!tab || tab.active) continue;
    // Commands or a reattachment can start while tabs.get is outstanding.
    const entry = tabs.get(tabId);
    if (selectIdleTabs([[tabId, entry]], Date.now(), idleDetachMs).length)
      softDetachTab(tabId, entry, 'idle');
  }
}

function detachTab(tabId, notify, preserveRecovery = false) {
  // Only the recovery's own intermediate reset may retain its health token.
  if (!preserveRecovery) attachmentHealth.clear(tabId);
  const entry = tabs.get(tabId);
  if (!entry) return;
  tabs.delete(tabId);
  forgetSessionTab(sessionToTab, tabId);
  for (const [sid, tid] of childSessionToTab.entries())
    if (tid === tabId) childSessionToTab.delete(sid);
  if (notify) {
    postToHost({
      method: 'forwardCDPEvent',
      params: {
        sessionId: entry.sessionId,
        method: 'Target.detachedFromTarget',
        params: { sessionId: entry.sessionId },
      },
    });
  }
}

function eligible(tab) {
  return !!tab && !!tab.id && typeof tab.url === 'string' && !SKIP_URL.test(tab.url);
}

// Re-attach ONLY the tabs this extension owns (agent-created / adopted) — never
// the user's own tabs. This is what keeps the debugger banner off the user's
// foreground pages. Runs on (re)connect and from the keepalive alarm, so an
// agent's tabs survive a service-worker restart without dragging the whole window
// (and its banners) along. Prunes ids whose tab is gone.
async function reattachOwnedTabs() {
  await loadOwnedTabs();
  for (const tabId of [...ownedTabs]) {
    const tab = await chrome.tabs.get(tabId).catch(() => null);
    if (!tab) {
      detachTab(tabId, true);
      unmarkOwned(tabId);
      continue;
    }
    if (!eligible(tab)) {
      // A live but temporarily restricted page (including a deliberately staged
      // about:blank) can become eligible later. Keep ownership so a later pass
      // retries it; only a browser-confirmed missing tab is phantom (#196).
      continue;
    }
    if (nativeDuplicateTabs.has(tabId)) continue;
    if (!tabs.has(tabId)) await announceOwnedTabLazily(tabId);
  }
}

// Make an owned tab known to the relay again WITHOUT attaching the debugger
// (issue #201): the targetId comes from chrome.debugger.getTargets(), which
// needs no attachment and shows no banner. The first command re-attaches. Falls
// back to a real attach only when the target can't be identified that way.
async function announceOwnedTabLazily(tabId) {
  let info = null;
  try {
    info = targetInfoForTab(
      await withRelayTimeout(chrome.debugger.getTargets(), 'chrome.debugger.getTargets'),
      tabId
    );
  } catch {}
  if (!info || !info.targetId) {
    try {
      await attachTab(tabId);
    } catch {
      // Restricted page or transient — leave owned; next pass retries.
    }
    return;
  }
  // A debugger attachment lingering from a previous worker instance keeps the
  // banner up with nobody driving; release it. (An attachment that isn't ours —
  // DevTools — makes the call fail harmlessly.)
  await chrome.debugger.detach({ tabId }).catch(() => {});
  if (removedTabs.has(tabId) || tabs.has(tabId)) return; // closed, or attached meanwhile
  const sessionId = `cb-tab-${tabId}`;
  const entry = newTabEntry(tabId, sessionId, String(info.targetId), false);
  tabs.set(tabId, entry);
  void cursorOverlay.attached(tabId);
  sessionToTab.set(sessionId, tabId);
  rememberSessionTarget(sessionId, entry.targetId);
  setBadge(tabId, port ? 'on' : 'connecting');
  await announceAttachedTab(tabId, entry, info);
}

// Returns 'announce' (sent), 'superseded' (another entry replaced this one and
// announces itself) or 'gone' (the tab closed meanwhile; nothing is sent).
async function announceAttachedTab(tabId, entry, targetInfo, scopeHints) {
  let tab = null;
  let missing = false;
  try {
    tab = await chrome.tabs.get(tabId);
  } catch (error) {
    missing = isMissingTabError(error, tabId);
  }
  const { openerTargetId, abGroup } = scopeHints || (await tabScopeHints(tabId));
  // Decide only now, after the awaits: tabs.onRemoved may have run while they
  // were pending and already told the host the tab is gone. Announcing after
  // that recreated the record for good — the url-less, title-less "attached"
  // pages of #519.
  const verdict = announceVerdict({
    current: tabs.get(tabId),
    entry,
    missing,
    removed: removedTabs.has(tabId),
  });
  if (verdict === 'gone') {
    // Whatever marked the tab removed (onRemoved, a replacement, a reconcile)
    // has already told the host; otherwise this read is the first to know.
    if (!removedTabs.has(tabId)) {
      removedTabs.add(tabId);
      cursorOverlay.forget(tabId);
      forgetTab(tabId);
    }
    return verdict;
  }
  if (verdict !== 'announce') return verdict;
  postToHost({
    method: 'forwardCDPEvent',
    params: {
      sessionId: entry.sessionId,
      method: 'Target.attachedToTarget',
      params: {
        sessionId: entry.sessionId,
        targetInfo: {
          targetId: entry.targetId,
          type: targetInfo?.type || 'page',
          url: tab?.url || tab?.pendingUrl || targetInfo?.url || '',
          title: tab?.title || targetInfo?.title || '',
          attached: true,
          openerTargetId,
          abGroup,
        },
      },
    },
  });
  return verdict;
}

// Drop a tab's record here and at the host. Unlike detachTab, it tells the
// host even when this worker holds no entry but the relay has known the tab
// (or `always`): the host may still keep a record of it. A tab the relay never
// knew (the user's own) costs no message.
function forgetTab(tabId, always = false) {
  const held = tabs.has(tabId);
  // Always: detachTab also clears the tab's attachment health without an entry.
  detachTab(tabId, true);
  if (!held && (always || relayKnowsTab(tabId))) postToHost(tabGoneEvent(tabId));
}

// When a command found no tab for a session, tell the host if Chrome confirms
// the tab is gone (Chrome's own "No tab with id"; any other error proves
// nothing).
async function reportTabIfGone(tabId) {
  if (tabId == null) return;
  try {
    await chrome.tabs.get(tabId);
  } catch (error) {
    if (!isMissingTabError(error, tabId)) return;
    removedTabs.add(tabId);
    // A command named this tab's session, so the host may hold a record of it
    // even when this worker (restarted since) never knew the tab.
    forgetTab(tabId, true);
  }
}

// The host keeps exactly the records this worker holds (#519). Hosts from
// before #519 ignore the message.
function syncRelayTargets() {
  postToHost(relayTargetsMessage(tabs.entries()));
}

async function reannounceAttachedTabs() {
  // The worker can miss `tabs.onRemoved` while its native-messaging port is
  // down. Validate browser-level tab ids before rebuilding relay state so a
  // dead bootstrap about:blank cannot be re-announced as the active target
  // beside the real recovered page (#196).
  const { live } = await reconcileAttachedTabEntries([...tabs.entries()], {
    getTab: (tabId) => chrome.tabs.get(tabId),
    isMissing: isMissingTabError,
    detach: (tabId) => {
      removedTabs.add(tabId);
      detachTab(tabId, true);
    },
    unmarkOwned,
  });
  for (const [tabId, entry] of live) {
    // Re-send live browser metadata and the group hint so the relay can rebuild
    // target state without asking the possibly frozen renderer.
    await announceAttachedTab(tabId, entry);
  }
}

// ---- chrome.debugger events ----------------------------------------------

chrome.debugger.onEvent.addListener(
  (source, method, params) =>
    void whenReady(() => {
      const tabId = source.tabId;
      if (!tabId) return;
      const entry = tabs.get(tabId);
      if (!entry) return;
      if (method === 'Target.attachedToTarget' && params?.sessionId) {
        childSessionToTab.set(String(params.sessionId), tabId);
        // Auto-attach reaches only immediate related targets. Each OOP frame
        // must arm its own descendants before nested frames can be routed.
        if (params.targetInfo?.type === 'iframe') {
          void trackedDebuggerCommand({ tabId, sessionId: String(params.sessionId) }, 'Target.setAutoAttach', {
            autoAttach: true, flatten: true, waitForDebuggerOnStart: false,
            filter: [{ type: 'iframe', exclude: false }],
          }).catch(() => {});
        }
      }
      if (method === 'Target.detachedFromTarget' && params?.sessionId) {
        childSessionToTab.delete(String(params.sessionId));
      }
      if (!shouldForwardEvent(method)) return;
      postToHost({
        method: 'forwardCDPEvent',
        params: { sessionId: source.sessionId || entry.sessionId, method, params },
      });
    })
);

// Tabs `ABExt.releaseTab` let go of: their detach is ours and final.
const releasedTabs = new Set();

chrome.debugger.onDetach.addListener(
  (source, reason) =>
    void whenReady(async () => {
      const tabId = source.tabId;
      if (!tabId) return;
      if (releasedTabs.has(tabId)) return;
      if (attachmentHealth.isRecovering(tabId)) return;
      const known = tabs.get(tabId);
      // Our own idle release (#201) — nothing to recover, the entry is kept.
      if (known && known.attached === false) return;
      // The user clicked Cancel on the debugger bar: honour it. Keep the tab known
      // (no detachedFromTarget to the host, no owned-tab re-attach from the alarm —
      // that is what made the bar come back within seconds) and re-attach only when
      // the agent's next command needs the tab.
      if (reason === 'canceled_by_user' && known) {
        softDetachTab(tabId, known, 'user');
        return;
      }
      detachTab(tabId, true);
      // A cross-process navigation (e.g. an SSO redirect like
      // login.account.rakuten.com that swaps the render process / spawns OOPIFs)
      // detaches the debugger, but the TAB survives. Without re-attaching, the
      // session goes permanently stale and even open/navigate fails — exactly the
      // #19 follow-up. So proactively re-attach (the stable `cb-tab-<tabId>`
      // session id then restores the daemon's binding). Don't fight a detach the
      // user or DevTools initiated.
      if (reason === 'canceled_by_user' || reason === 'replaced_with_devtools') return;
      if (!port) return;
      if (nativeDuplicateTabs.has(tabId)) return;
      const detached = await chrome.tabs.get(tabId).catch(() => null);
      if (!detached || !eligible(detached) || detached.discarded) return;
      // The swapped-in process needs a moment to settle; retry with backoff.
      for (let i = 0; i < 6; i++) {
        await new Promise((r) => setTimeout(r, 250 + i * 200));
        if (tabs.get(tabId)?.attached) return; // already re-attached (e.g. via onUpdated)
        if (nativeDuplicateTabs.has(tabId)) return;
        const tab = await chrome.tabs.get(tabId).catch(() => null);
        if (!tab || !eligible(tab) || tab.discarded) return; // gone, restricted or discarded
        try {
          await attachTab(tabId);
          return;
        } catch (e) {
          // The tab became a page we can never attach to (another extension's page,
          // chrome://, restricted). Retrying can't help — bail quietly instead of
          // logging the same failure 6×. (`eligible()` filters most of these, but a
          // tab can navigate into one between our check and the attach.)
          if (isPermanentAttachError(e)) return;
          console.warn(`ab-connect: reattach attempt ${i + 1} for tab ${tabId} failed:`, e);
        }
      }
    })
);

// ---- tab lifecycle --------------------------------------------------------

// EARLY-attach a popup our own tab opened (window.open / OAuth account-chooser),
// the moment Chrome creates it — before it finishes loading. The old path only
// attached on chrome.tabs.onUpdated('complete'), which for a cross-origin OAuth
// window lands too late: the daemon tries to drive the popup first and fails with
// "tab is not responding (session re-attaching)" / "tab can no longer be
// resolved". Attaching at creation makes the account-chooser debuggable
// immediately so the daemon can select the Google account and finish the login.
//
// Gated to popups whose opener is a tab WE drive (owned/attached) so we never
// attach the USER's own window.open, and so the relay can tag the popup into the
// opener's tab-group (the synthesized targetInfo carries openerTargetId) —
// keeping multi-agent isolation intact: only the session that opened it sees it.
// Not persisted into ownedTabs (an OAuth popup is transient and usually self-
// closes; persisting would re-attach a dead tab and stick the debugger banner) —
// it's adopted for this session only, like an explicitly-adopted tab.
chrome.tabs.onCreated.addListener(
  (tab) =>
    void whenReady(async () => {
      if (!port || !tab || tab.id == null) return;
      if (await isNativeDuplicateLifecycleEvent(tab)) return;
      const opener = tab.openerTabId;
      if (typeof opener !== 'number') return;
      if (!ownedTabs.has(opener) && !tabs.has(opener)) return;
      // Opened by an agent tab (created by the agent, or a verified agent
      // pop-up): the agent's. Opened by a merely attached tab (a user tab taken
      // with adopt/inspect): attached as before, but it keeps user identity.
      await loadAgentPopups();
      // A failed query is unknown (null), never an empty list.
      const all = await chrome.tabs.query({}).catch(() => null);
      if (isVerifiedAgentPopup(tab, all, agentTabPredicate(ownedTabs, agentPopups)))
        await markAgentPopup(tab.id);
      if (tabs.has(tab.id)) return;
      // A fresh popup is often still at about:blank (no url yet) — that's fine to
      // attach; only bail on a clearly-restricted scheme. attachTab tolerates the
      // rest, and the cross-process onDetach reattach picks up the OAuth nav.
      if (typeof tab.url === 'string' && tab.url && SKIP_URL.test(tab.url)) return;
      try {
        await attachTab(tab.id);
      } catch {
        // Restricted/transient — onDetach reattach + onUpdated('complete') retry.
      }
    })
);
chrome.tabs.onUpdated.addListener(
  (tabId, changeInfo, tab) =>
    void whenReady(async () => {
      if (tabGrants.get(tabId) && (changeInfo.url || changeInfo.title || changeInfo.status)) {
        // The grant stays with the tab across navigations; its page is shown.
        if (await tabGrants.navigated(tabId, { url: tab?.url, title: tab?.title })) publishTabGrants();
        showGrantIndicator(tabId);
      }
      if (await isNativeDuplicateLifecycleEvent(tab)) return;
      const attached = tabs.get(tabId);
      if (attached) {
        await announceAttachedTab(tabId, attached);
        return;
      }
      // Only tabs that are ours (agent-created) or opened BY ours (an OAuth popup
      // that was still about:blank when created) — never the user's own tabs.
      const ours =
        ownedTabs.has(tabId) ||
        (typeof tab?.openerTabId === 'number' &&
          (ownedTabs.has(tab.openerTabId) || tabs.has(tab.openerTabId)));
      if (changeInfo.status === 'complete' && eligible(tab) && !tabs.has(tabId) && port && ours) {
        try {
          await attachTab(tabId);
        } catch {}
      }
    })
);
chrome.tabs.onReplaced.addListener(
  (addedTabId, removedTabId) =>
    void whenReady(async () => {
      await loadOwnedTabs();
      if (relayKnowsTab(removedTabId)) {
        await loadReplacedTabs();
        recordReplacement(replacedTabs, removedTabId, addedTabId);
        saveReplacedTabs();
      }
      // A confirmed agent pop-up stays one under its new id.
      await migrateAgentPopup(removedTabId, addedTabId);
      // A lent tab stays lent (or refused) under its new id.
      const moved = await tabGrants.replaced(removedTabId, addedTabId);
      if (moved) {
        showGrantIndicator(addedTabId);
        publishTabGrants();
        void tellServiceGrant(moved.conversationId, moved);
      }
      // Ownership follows the tab, so the session may still drive (and close)
      // it under its new id.
      if (ownedTabs.has(removedTabId) || recentlyRemovedOwned.has(removedTabId)) {
        recentlyRemovedOwned.delete(removedTabId);
        await unmarkOwned(removedTabId);
        await markOwned(addedTabId);
      }
      transferReloadState(reloadStates, removedTabId, addedTabId);
      // The old tab is gone (no onRemoved follows a replacement), and so is
      // its target. Drop its record here and at the host; a session still
      // naming it follows replacedTabs to the new tab on its next command.
      removedTabs.add(removedTabId);
      cursorOverlay.forget(removedTabId);
      forgetTab(removedTabId);
    })
);

chrome.tabs.onRemoved.addListener(
  (tabId) =>
    void whenReady(() => {
      // First, synchronously: an attach or announce still awaiting Chrome
      // checks this before it registers or announces the tab (#519).
      removedTabs.add(tabId);
      nativeDuplicateTabs.delete(tabId);
      cursorOverlay.forget(tabId);
      void forgetAgentPopup(tabId);
      // Keep a short tombstone for stable-target recovery after onRemoved.
      // Genuine closed tabs expire, while a replacement can transfer the state.
      const removedState = reloadStates.get(tabId);
      setTimeout(() => {
        if (reloadStates.get(tabId) === removedState) reloadStates.delete(tabId);
      }, RELOAD_LOOP_WINDOW_MS);
      const removedSessionId = `cb-tab-${tabId}`;
      const removedTargetId = sessionTargets.get(removedSessionId);
      if (removedTargetId) {
        setTimeout(() => {
          if (sessionTargets.get(removedSessionId) === removedTargetId) {
            sessionTargets.delete(removedSessionId);
          }
        }, RELOAD_LOOP_WINDOW_MS);
      }
      if (ownedTabs.has(tabId)) {
        recentlyRemovedOwned.add(tabId);
        setTimeout(() => recentlyRemovedOwned.delete(tabId), 5000);
      }
      unmarkOwned(tabId);
      forgetTab(tabId);
      forgetTabPanel(tabId);
      void tabGrants.removed(tabId).then(grant => {
        if (!grant) return;
        publishTabGrants();
        void tellServiceGrant(grant.conversationId, null);
      });
    })
);

// `tabs.onUpdated` can collapse repeated reloads into ambiguous loading states.
// webNavigation gives one committed event per top-level document, which is the
// stable signal needed to diagnose a rapid same-URL loop (#211).
chrome.webNavigation.onCommitted.addListener((details) => {
  if (!details || details.frameId !== 0) return;
  const entry = tabs.get(details.tabId);
  if (!entry && !ownedTabs.has(details.tabId) && !reloadStates.has(details.tabId)) return;
  let reloadState = entry?.reloadState || reloadStates.get(details.tabId);
  if (!reloadState) {
    reloadState = newReloadState();
    reloadStates.set(details.tabId, reloadState);
  }
  recordNavigationCommit(reloadState, details.url);
});

// ---- bootstrap + keepalive ------------------------------------------------

chrome.runtime.onInstalled.addListener((details) => {
  // Also a browser start: Chrome updated itself, or the extension is loaded
  // from the command line. Not an extension update, which can land while a
  // session works in its group.
  if (details?.reason === 'install' || details?.reason === 'chrome_update') ungroupRestoredGroupsAfterStartup();
  // One-time purge: a prior build persisted ADOPTED user tabs into the owned set,
  // which then re-attached them on every restart and left the debugger banner
  // stuck on the user's pages. Clear the persisted set on install/update — agent
  // tabs from a previous SW are dead anyway, and createTarget re-marks new ones.
  // An update this extension applied itself (#524) left a handoff note: its
  // tabs are live agent tabs of a session that continues, so they stay owned.
  if (details && (details.reason === 'update' || details.reason === 'install')) {
    void settleOwnershipAfterInstall(details.reason);
    // When this version arrived, for `doctor` (#524): "updated 3 h ago from
    // 0.5.34" tells a stuck update from one that never came.
    installRecord = {
      version: chrome.runtime.getManifest().version,
      at: Date.now(),
      reason: details.reason,
      previousVersion: details.previousVersion ?? null,
    };
    try {
      void chrome.storage.local.set({ [INSTALL_RECORD_KEY]: installRecord }).catch(() => {});
    } catch {}
  }
  void whenReady(connectHost);
});

async function settleOwnershipAfterInstall(reason) {
  let handoff = null;
  try {
    handoff = (await chrome.storage.local.get(UPDATE_HANDOFF_KEY))?.[UPDATE_HANDOFF_KEY] ?? null;
    await chrome.storage.local.remove(UPDATE_HANDOFF_KEY);
  } catch {}
  // Kept: the connect path re-announces them (reattachOwnedTabs waits for
  // the persisted set to load), without attaching the debugger.
  if (keepsOwnershipAcrossUpdate(reason, handoff, Date.now())) return;
  // Wait for the startup load so it cannot re-add what is purged here.
  await loadOwnedTabs();
  try {
    await chrome.storage.local.remove('ab_owned_tabs');
  } catch {}
  ownedTabs.clear();
}
chrome.runtime.onStartup.addListener(() => {
  void whenReady(connectHost);
  ungroupRestoredGroupsAfterStartup();
});

// A restored session brings agent groups back open, and Chrome saves a group
// that is open when the browser quits (#692): take their tabs out. Restore
// finishes after startup, so look again a little later.
function ungroupRestoredGroupsAfterStartup() {
  for (const ms of [2000, 10000, 30000]) {
    setTimeout(() => {
      void ungroupRestoredAgentGroups({
        // A tab an agent created in this browser run is owned before it is
        // grouped; a restored tab is not (tab ids change across a restart).
        groups: chrome.tabGroups, tabs: chrome.tabs, inUse: tabId => tabs.has(tabId) || ownedTabs.has(tabId),
      }).catch(() => {});
    }, ms);
  }
}

// ---- self-update ----------------------------------------------------------
//
// Users who never open chrome://extensions can sit on an old build for a long
// time, because Chrome checks the store on its own slow schedule and only while
// the browser runs. Ask it to look now, and apply a downloaded update once
// nothing is being driven. `requestUpdateCheck` needs no permission.
//
// "Nothing is being driven" used to mean "no tab attached", but a tab the agent
// drove stays attached (idle-detach defaults to off) and the keepalive keeps
// this worker alive while paired, so one long-lived session held every update
// back for good (#524). Now: no command in flight, and the relay quiet for
// UPDATE_IDLE_GRACE_MS. Then the attached tabs are released and the worker
// reloads; the tabs it created keep their ownership (a handoff record read by
// onInstalled), are re-announced without attaching, and the CLI re-attaches
// on its next command. Nothing is activated or focused, and a user's own tab
// the agent held is only released, never touched.
let lastUpdateCheckAt = 0;
// The last chrome.runtime.requestUpdateCheck: {at, status, version?, error?}.
let lastUpdateCheck = null;
// When the running version was installed or updated to (0.5.35+ records it).
const INSTALL_RECORD_KEY = 'ab_installed';
let installRecord = null;
try {
  chrome.storage.local
    .get(INSTALL_RECORD_KEY)
    .then((g) => {
      const r = g?.[INSTALL_RECORD_KEY];
      // onInstalled may already have written a newer one.
      if (!installRecord && r && r.version === chrome.runtime.getManifest().version) installRecord = r;
    })
    .catch(() => {});
} catch {}
let updatePending = false;
let updateVersion = null;
let updatePendingSince = 0;
let applyingUpdate = false;
let updateApplyTimer = null;
// After a failed handoff write, wait this long before trying again.
const UPDATE_HANDOFF_RETRY_MS = 30 * 1000;
let updateRetryAfter = 0;
// Host commands being handled now, and when the last one started or ended.
// The worker's start counts as activity: tabs re-announced at startup belong
// to a session that may be about to continue.
let hostCommandsInFlight = 0;
let lastHostCommandAt = Date.now();

function currentUpdatePlan() {
  return updateApplyPlan(tabs.entries(), {
    now: Date.now(),
    lastActivityAt: lastHostCommandAt,
    commandsInFlight: hostCommandsInFlight,
  });
}

// What `ABExt.state` reports under `update` (0.5.35+): whether an update
// waits, for which version, and what holds it back right now.
function updateStateSnapshot() {
  const plan = currentUpdatePlan();
  return {
    pending: updatePending,
    version: updateVersion,
    pendingForMs: updatePending && updatePendingSince ? Date.now() - updatePendingSince : null,
    appliesAfterIdleMs: plan.graceMs,
    reason: updatePending ? (applyingUpdate ? 'applying' : plan.reason) : null,
    commandsInFlight: plan.commandsInFlight,
    attachedTabs: plan.attachedTabs,
    idleForMs: plan.idleForMs,
    appliesInMs: updatePending ? plan.appliesInMs : null,
    lastCheck: lastUpdateCheck && {
      ...lastUpdateCheck,
      ageMs: Date.now() - lastUpdateCheck.at,
    },
    installed: installRecord && {
      ...installRecord,
      ageMs: Date.now() - installRecord.at,
    },
  };
}

function markUpdatePending(version) {
  if (!updatePending) updatePendingSince = Date.now();
  updatePending = true;
  if (typeof version === 'string' && version) updateVersion = version;
  publishHostStatus();
  void applyUpdateWhenIdle();
}

// Re-check exactly when the quiet period would end (the keepalive alarm is a
// coarse backstop). Re-armed after every command, so it tracks the latest.
function scheduleUpdateApply() {
  if (!updatePending) return;
  const plan = currentUpdatePlan();
  if (updateApplyTimer) clearTimeout(updateApplyTimer);
  updateApplyTimer = null;
  if (plan.apply) {
    void applyUpdateWhenIdle();
    return;
  }
  if (plan.appliesInMs == null) return; // a command is running; its end re-arms
  updateApplyTimer = setTimeout(() => {
    updateApplyTimer = null;
    void applyUpdateWhenIdle();
  }, plan.appliesInMs + 250);
}

async function applyUpdateWhenIdle() {
  if (!updatePending || applyingUpdate) return;
  // A failed handoff write is retried by the keepalive alarm, not in a loop.
  if (Date.now() < updateRetryAfter) return;
  // Records of closed tabs are not "being driven": prune them first so they
  // never hold the update back (#519).
  await pruneDeadTabRecords();
  if (!updatePending || applyingUpdate) return;
  if (!currentUpdatePlan().apply) {
    scheduleUpdateApply();
    return;
  }
  applyingUpdate = true;
  try {
    // Leave a note for the worker that starts after the reload: this update
    // was applied on purpose, so the tabs this extension created stay owned.
    // Without the note the next worker would forget them and the session
    // could not continue, so no note, no reload: try again later.
    try {
      await withRelayTimeout(
        chrome.storage.local.set({
          [UPDATE_HANDOFF_KEY]: {
            at: Date.now(),
            from: chrome.runtime.getManifest().version,
            to: updateVersion,
          },
        }),
        'chrome.storage.local.set'
      );
    } catch {
      updateRetryAfter = Date.now() + UPDATE_HANDOFF_RETRY_MS;
      return;
    }
    // A command may have arrived while the note was written.
    if (!currentUpdatePlan().apply) return;
    // Release every attached tab (idle agent tabs; a user tab a session
    // forced is released, not closed). The relay records stay, so nothing
    // tells the host the tabs are gone; the reload drops the port and the
    // daemon reconnects to the new worker, which re-announces its own tabs.
    const releases = [];
    for (const [tabId, entry] of tabs.entries()) {
      if (!entry || entry.attached === false) continue;
      entry.attached = false;
      for (const [sid, tid] of childSessionToTab.entries())
        if (tid === tabId) childSessionToTab.delete(sid);
      releases.push(chrome.debugger.detach({ tabId }).catch(() => {}));
    }
    await Promise.race([Promise.allSettled(releases), new Promise((r) => setTimeout(r, 2000))]);
    // Last check, in the same task as the reload: nothing can arrive between.
    // A command that came in during the releases re-attached its tab (and
    // reset the quiet period), so the update waits for the next one.
    if (!updatePending || !currentUpdatePlan().apply) return;
    updatePending = false;
    chrome.runtime.reload();
  } catch {
  } finally {
    applyingUpdate = false;
    if (updatePending) scheduleUpdateApply();
  }
}

// Test hook for the live check (#524): there is no way to make Chrome fire
// onUpdateAvailable on demand. Reachable only from this extension's own
// worker (e.g. DevTools / the debugging protocol on a test browser).
globalThis.__abConnectSimulateUpdateAvailable = (version) => markUpdatePending(version || null);

// Drop every entry whose tab Chrome confirms gone, here and at the host.
async function pruneDeadTabRecords() {
  await reconcileAttachedTabEntries([...tabs.entries()], {
    getTab: (tabId) => chrome.tabs.get(tabId),
    isMissing: isMissingTabError,
    detach: (tabId) => {
      removedTabs.add(tabId);
      detachTab(tabId, true);
    },
    unmarkOwned,
  });
}

chrome.runtime.onUpdateAvailable.addListener((details) => {
  markUpdatePending(details?.version);
});

function maybeCheckForUpdate() {
  const now = Date.now();
  if (!shouldCheckForUpdate(lastUpdateCheckAt, now)) return;
  lastUpdateCheckAt = now;
  // Remembered for `ABExt.state` (`update.lastCheck`), so `status`/`doctor`
  // can tell "Chrome never found an update" from "one waits" (#524).
  const record = (fields) => {
    lastUpdateCheck = { at: now, ...fields };
  };
  try {
    // Unpacked/dev installs have no update url: this rejects or reports
    // `throttled`. A downloaded update that waits is reported again as
    // `update_available`, so a missed onUpdateAvailable is caught here too.
    const p = chrome.runtime.requestUpdateCheck?.();
    if (p && typeof p.then === 'function') {
      record({ status: 'checking' });
      p.then((r) => {
        record({ status: r?.status ?? null, version: r?.version ?? null });
        if (r?.status === 'update_available') markUpdatePending(r?.version);
      }).catch((e) => record({ status: 'error', error: String(e?.message || e).slice(0, 200) }));
    } else {
      record({ status: 'unavailable' });
    }
  } catch (e) {
    record({ status: 'error', error: String(e?.message || e).slice(0, 200) });
  }
}

// Wake-from-sleep / return-from-idle fast reconnect. After the machine sleeps (or
// the user is away long enough), the MV3 worker is killed and its native-messaging
// port dies; previously only the keepalive alarm revived it — and chrome.alarms
// clamps to a ~30s floor, so the relay stayed dead for up to ~30s after wake (the
// "first command after coming back hangs/reconnects slowly" feeling). chrome.idle
// fires on the transition back to "active" and is wake-eligible (it restarts a
// suspended worker), so we reconnect the instant the user returns instead of
// waiting for the alarm. `idle` is a no-warning permission, so this auto-updates
// with no re-consent.
try {
  chrome.idle.setDetectionInterval(15);
  chrome.idle.onStateChanged.addListener((state) => {
    if (state === 'active')
      void whenReady(() => {
        if (!port) connectHost();
      });
  });
} catch {}

// Popup status page asks for the live pairing state. Attempt a (re)connect on
// demand so opening the popup also nudges the link awake, then report.
chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  // The side panel and the options page answer site approvals and edit the
  // approved-sites list; nothing else may (#781).
  if (msg?.type === 'ab-site-permission') {
    if (!isAgentPanelSender(_sender, chrome.runtime)) {
      sendResponse({ success: false, error: 'Site permissions change only from the chrome-use side panel or options page' });
      return false;
    }
    sitePermissions.onMessage(msg).then(
      result => sendResponse({ success: true, ...result }),
      error => sendResponse({ success: false, error: String(error?.message || error) }),
    );
    return true;
  }
  if (msg?.type === 'teach:event') {
    sendResponse({ ok: teachController.onEvent(msg, _sender) });
    return false;
  }
  // The side panel's Record button: start or stop recording its own tab.
  if (msg?.type === 'ab-teach-start' || msg?.type === 'ab-teach-stop' || msg?.type === 'ab-teach-status') {
    if (!isAgentPanelSender(_sender, chrome.runtime) || !Number.isInteger(msg.tabId)) {
      sendResponse({ success: false, error: 'Recording starts from the extension panel' });
      return false;
    }
    const run = msg.type === 'ab-teach-start' ? teachController.start(msg.tabId)
      : msg.type === 'ab-teach-stop' ? teachController.stop(msg.tabId)
      : Promise.resolve({ recording: teachController.isRecording(msg.tabId) });
    run.then(
      result => sendResponse({ success: true, ...result }),
      error => sendResponse({ success: false, error: String(error.message || error) }),
    );
    return true;
  }
  if (typeof msg?.type === 'string' && msg.type.startsWith('inspect:')) {
    return inspectController.onPickerMessage(msg, _sender, sendResponse);
  }
  if (msg?.type === 'ab-inspect-start') {
    if (_sender?.id !== chrome.runtime.id || _sender.tab || _sender.url !== chrome.runtime.getURL('popup.html')) {
      sendResponse({ success: false, error: 'Inspect & Ask starts from the popup or the keyboard shortcut' });
      return false;
    }
    inspectController.start(null).then(
      () => sendResponse({ success: true }),
      error => sendResponse({ success: false, error: String(error.message || error) }),
    );
    return true;
  }
  // The side panel's Pick button: a picker on the panel's own tab.
  if (msg?.type === 'ab-inspect-pick' || msg?.type === 'ab-inspect-pick-cancel') {
    if (!isAgentPanelSender(_sender, chrome.runtime) || !Number.isInteger(msg.tabId)) {
      sendResponse({ success: false, error: 'Picking starts from the extension panel' });
      return false;
    }
    (msg.type === 'ab-inspect-pick' ? inspectController.startDirect(msg.tabId, { keep: msg.keep === true }) : inspectController.cancelDirect(msg.tabId)).then(
      result => sendResponse({ success: true, ...result }),
      error => sendResponse({ success: false, error: String(error.message || error) }),
    );
    return true;
  }
  if (msg?.type === 'ab-conversation-window') {
    if (!isAgentPanelSender(_sender, chrome.runtime)) {
      sendResponse({ success: false, error: 'Window navigation requires the packaged extension panel' });
      return false;
    }
    conversationWindows.show(msg.conversationId, msg.destination).then(
      result => sendResponse({ success: true, ...result }),
      error => sendResponse({ success: false, error: String(error.message || error) }),
    );
    return true;
  }
  if (msg?.type === 'ab-tab-grant') {
    if (!isAgentPanelSender(_sender, chrome.runtime)) {
      sendResponse({ success: false, error: 'Agent services require the packaged extension panel' });
      return false;
    }
    handleTabGrantMessage(msg).then(
      result => sendResponse({ success: true, ...result }),
      error => sendResponse({ success: false, error: String(error.message || error), reason: error.reason || null }),
    );
    return true;
  }
  if (msg?.type === 'ab-agent-request') {
    if (!isAgentPanelSender(_sender, chrome.runtime)) {
      sendResponse({ success: false, error: 'Agent services require the packaged extension panel' });
      return false;
    }
    if (!pageOperationAllowed(msg.operation)) {
      sendResponse({ success: false, error: 'This Inspect & Ask operation is only available from the input panel' });
      return false;
    }
    agentPanelBridge.request(msg.operation, msg.params ?? {}).then(
      async result => {
        if (msg.operation === 'conversationDelete' && result.deleted?.id) {
          try { await conversationWindows.forget(result.deleted.id); }
          catch (error) { result.deleted.pageCleanup = { error: String(error.message || error) }; }
        }
        sendResponse({ success: true, result });
      },
      error => sendResponse({ success: false, error: String(error.message || error) }),
    );
    return true;
  }
  // Version banners in the popup and side panel (version-notice.js).
  if (msg?.type === 'ab-cli-upgrade' || msg?.type === 'ab-apply-update') {
    const popup = _sender?.id === chrome.runtime.id && !_sender.tab && _sender.url === chrome.runtime.getURL('popup.html');
    if (!popup && !isAgentPanelSender(_sender, chrome.runtime)) {
      sendResponse({ success: false, error: 'Updates start from the extension popup or panel' });
      return false;
    }
    try {
      if (msg.type === 'ab-cli-upgrade') startCliUpgrade();
      else if (versionState().update?.canApplyNow) void applyUpdateWhenIdle();
      else throw new Error(t('bg_update_not_now'));
      sendResponse({ success: true, ...versionState() });
    } catch (error) {
      sendResponse({ success: false, error: String(error.message || error) });
    }
    return false;
  }
  // The setup guide (onboarding.js) polls this while the CLI is missing:
  // retry the native host now instead of after the backoff.
  if (msg?.type === 'ab-host-retry') {
    const popup = _sender?.id === chrome.runtime.id && !_sender.tab && _sender.url === chrome.runtime.getURL('popup.html');
    if (!popup && !isAgentPanelSender(_sender, chrome.runtime)) {
      sendResponse({ success: false, error: 'Host retries start from the extension popup or panel' });
      return false;
    }
    if (!port) {
      hostReconnect.reset();
      try { connectHost(); } catch (e) {}
    }
    sendResponse({ success: true, ...hostStatusForUi() });
    return false;
  }
  if (msg && msg.type === 'ab-status') {
    if (!port) {
      try {
        connectHost();
      } catch (e) {}
    }
    sendResponse({ ...hostStatusForUi(), tabCount: tabs.size, host: HOST_NAME, agentPanelEntry,
      cursorCleanupPending: cursorOverlay.pendingTabs(), ...versionState() });
  }
  return false;
});

// Hot-path keepalive: while a native-messaging port is open, post a tiny ping
// every 20s. Each port message resets the MV3 service-worker idle timer, so the
// worker never reaches the ~30s idle-kill while we're paired and the relay stays
// live — instead of dropping during any quiet stretch (no CDP traffic) and making
// the next command hang until a reconnect. The setTimeout chain only survives as
// long as the worker does, which is exactly what the ping guarantees; on a cold
// worker restart the alarm below re-runs connectHost() which restarts this loop.
const KEEPALIVE_PING_MS = 20000;
let keepaliveTimer = null;
function scheduleKeepalivePing() {
  if (keepaliveTimer) clearTimeout(keepaliveTimer);
  keepaliveTimer = setTimeout(() => {
    keepaliveTimer = null;
    sweepIdleTabs();
    if (!port) return; // disconnected; connectHost() will restart the loop
    pollScheduleNotifications();
    pollTurnNotifications();
    postToHost({ method: 'ping' }); // host pongs (or ignores); the send is what matters
    scheduleKeepalivePing();
  }, KEEPALIVE_PING_MS);
}

// Cold-restart backstop. MV3 service workers get suspended; an alarm wakes us to
// reconnect the host link and restart the heartbeat. NOTE: chrome.alarms clamps
// sub-minute periods (effective floor ~30s+), so the alarm alone can't outpace the
// 30s idle-kill — that's why the 20s ping above is the real keeper, not this.
chrome.alarms.create('keepalive', { periodInMinutes: 0.4 });
chrome.alarms.onAlarm.addListener((a) => {
  if (a.name !== 'keepalive') return;
  void whenReady(() => {
    sweepIdleTabs();
    maybeCheckForUpdate();
    if (!port) {
      void applyUpdateWhenIdle();
      connectHost();
    } else {
      // Prune dead records, re-announce owned tabs, then resync the host's
      // list: a periodic backstop for anything a race left behind (#519).
      void Promise.allSettled([pruneDeadTabRecords(), reattachOwnedTabs()]).then(() => {
        syncRelayTargets();
        return applyUpdateWhenIdle();
      });
      scheduleKeepalivePing();
    }
  });
});

// Gate placeholder so future async state-rehydration can hook in.
async function whenReady(fn) {
  return fn();
}

// Kick a connection attempt as soon as the worker starts.
connectHost();

// An agent click that opens a tab must not take the user's page away (see
// front-tab-guard.js): Chrome fronts the pop-up, the tab that was in front
// comes back, and the pop-up stays open behind it for the agent.
const frontTabGuard = new FrontTabGuard();
const bringBack = tabId => { if (Number.isInteger(tabId)) void chrome.tabs.update(tabId, { active: true }).catch(() => {}); };
void chrome.tabs.query({ active: true }).then(list => { for (const tab of list) frontTabGuard.activated(tab.windowId, tab.id); }).catch(() => {});
chrome.tabs.onActivated.addListener(({ tabId, windowId }) => bringBack(frontTabGuard.activated(windowId, tabId)));
chrome.tabs.onRemoved.addListener(tabId => frontTabGuard.removed(tabId));
// Which agent tabs the user switched to and stayed on (see userTouched).
chrome.tabs.onActivated.addListener(({ tabId }) => {
  void (async () => {
    await Promise.all([loadOwnedTabs(), loadAgentPopups(), userTouchedLoaded]);
    if (!userTouched.activated(tabId)) return;
    setTimeout(async () => {
      const tab = await chrome.tabs.get(tabId).catch(() => null);
      if (userTouched.confirm(tabId, tab?.active === true)) saveUserTouched();
    }, DWELL_MS);
  })().catch(() => {});
});
chrome.tabs.onRemoved.addListener((tabId) => {
  if (userTouched.removed(tabId)) saveUserTouched();
});
// The same event is the one trustworthy record of who opened the tab, so it
// also marks the pop-up as the agent's and keeps it in the source's group: in
// a window with user tabs, the click's attach check could not tell otherwise.
chrome.webNavigation?.onCreatedNavigationTarget?.addListener(({ sourceTabId, tabId }) =>
  void whenReady(async () => {
    await loadAgentPopups();
    if (!ownedTabs.has(sourceTabId) && !agentPopups.has(sourceTabId)) return;
    const tab = await chrome.tabs.get(tabId).catch(() => null);
    if (!tab) return;
    bringBack(frontTabGuard.agentOpened(tabId, tab.windowId, tab.active));
    await markAgentPopup(tabId);
    const source = await chrome.tabs.get(sourceTabId).catch(() => null);
    if (source?.groupId >= 0 && source.windowId === tab.windowId && tab.groupId !== source.groupId)
      await chrome.tabs.group({ groupId: source.groupId, tabIds: tabId }).catch(() => {});
  }));
