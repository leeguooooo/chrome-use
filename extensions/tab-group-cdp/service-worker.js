const REQUEST_TYPE = 'AB_TAB_GROUP_REQUEST';
const PANEL_GET_STATE = 'AB_PANEL_GET_STATE';
const PANEL_CLOSE_OTHER_TABS = 'AB_PANEL_CLOSE_OTHER_SESSION_TABS';
const PANEL_FOCUS_SESSION = 'AB_PANEL_FOCUS_SESSION';
const PANEL_CLEAN_EMPTY_GROUPS = 'AB_PANEL_CLEAN_EMPTY_GROUPS';
const PANEL_SET_POLICY = 'AB_PANEL_SET_POLICY';
const PANEL_SET_OPTIONS = 'AB_PANEL_SET_OPTIONS';

const DEFAULT_GROUP_TITLE = 'Agent Browser Stealth';
const DOWNLOAD_ARCHIVE_ROOT = 'agent-browser-stealth';
const STORAGE_POLICY_KEY = 'abSessionPoliciesV1';
const STORAGE_OPTIONS_KEY = 'abExtensionOptionsV1';
const CLEANUP_ALARM_NAME = 'ab-clean-empty-groups';
const GROUP_COLORS = ['blue', 'green', 'pink', 'orange', 'purple', 'cyan', 'red', 'yellow'];
const RISKY_TLDS = new Set(['zip', 'mov', 'click', 'top', 'gq', 'tk', 'country']);
const RISKY_HOST_KEYWORDS = ['secure-login', 'account-verify', 'wallet-verify', 'airdrop-claim'];

const DEFAULT_EXTENSION_OPTIONS = {
  strictWindowIsolation: true,
  suppressCrossWindowActivation: true,
  autoCleanEmptyGroups: true,
};

const sessionGroupCache = new Map();
const sessionWindowMap = new Map();
const tabSessionMap = new Map();
const tabMetaById = new Map();
const downloadEvents = [];
const sessionPolicies = new Map();
let extensionOptions = { ...DEFAULT_EXTENSION_OPTIONS };

let bootstrapPromise = bootstrapState();

function normalizeSession(session) {
  if (typeof session !== 'string') return 'default';
  const trimmed = session.trim();
  return trimmed.length > 0 ? trimmed.slice(0, 64) : 'default';
}

function normalizeGroupTitle(title) {
  if (typeof title !== 'string') return DEFAULT_GROUP_TITLE;
  const trimmed = title.trim();
  return trimmed.length > 0 ? trimmed.slice(0, 80) : DEFAULT_GROUP_TITLE;
}

function normalizeAllowedDomains(domains) {
  if (!Array.isArray(domains)) return [];
  return domains
    .map((item) => (typeof item === 'string' ? item.trim().toLowerCase() : ''))
    .filter((item) => item.length > 0)
    .slice(0, 256);
}

function parseHostname(rawUrl) {
  if (typeof rawUrl !== 'string' || rawUrl.length === 0) return null;
  try {
    const parsed = new URL(rawUrl);
    if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') return null;
    return parsed.hostname.toLowerCase();
  } catch {
    return null;
  }
}

function domainMatches(hostname, pattern) {
  if (!hostname || !pattern) return false;
  if (pattern.startsWith('*.')) {
    const suffix = pattern.slice(2);
    return hostname === suffix || hostname.endsWith(`.${suffix}`);
  }
  if (pattern.startsWith('.')) {
    const suffix = pattern.slice(1);
    return hostname === suffix || hostname.endsWith(pattern);
  }
  return hostname === pattern || hostname.endsWith(`.${pattern}`);
}

function isDomainAllowed(hostname, patterns) {
  if (!hostname) return true;
  if (!patterns || patterns.length === 0) return true;
  return patterns.some((pattern) => domainMatches(hostname, pattern));
}

function collectRiskHints(rawUrl, allowedDomains) {
  const hints = [];
  const hostname = parseHostname(rawUrl);
  if (!hostname) return hints;

  if (allowedDomains.length > 0 && !isDomainAllowed(hostname, allowedDomains)) {
    hints.push(`domain-not-allowed:${hostname}`);
  }

  const tld = hostname.split('.').pop();
  if (tld && RISKY_TLDS.has(tld)) {
    hints.push(`high-risk-tld:.${tld}`);
  }

  for (const keyword of RISKY_HOST_KEYWORDS) {
    if (hostname.includes(keyword)) {
      hints.push(`suspicious-host-keyword:${keyword}`);
    }
  }

  return [...new Set(hints)].slice(0, 10);
}

function cacheKey(windowId, session) {
  return `${windowId}:${session}`;
}

function sanitizeSegment(input, fallback = 'default') {
  const raw = typeof input === 'string' ? input : '';
  const cleaned = raw
    .replace(/[\\/:*?"<>|\u0000-\u001f]/g, '-')
    .replace(/\s+/g, '_')
    .replace(/\.+/g, '.')
    .trim();
  if (!cleaned) return fallback;
  return cleaned.slice(0, 80);
}

function sanitizeFilename(filename, fallback = 'download.bin') {
  const name = typeof filename === 'string' ? filename.split('/').pop() : '';
  return sanitizeSegment(name, fallback);
}

function pickColorForSession(session) {
  let hash = 0;
  for (let i = 0; i < session.length; i += 1) {
    hash = (hash * 31 + session.charCodeAt(i)) >>> 0;
  }
  return GROUP_COLORS[hash % GROUP_COLORS.length];
}

function shouldCollapseGroup(session) {
  return session !== 'default';
}

async function loadPolicies() {
  try {
    const result = await chrome.storage.local.get([STORAGE_POLICY_KEY]);
    const entries = result?.[STORAGE_POLICY_KEY];
    if (!entries || typeof entries !== 'object') return;

    for (const [session, domains] of Object.entries(entries)) {
      const normalizedSession = normalizeSession(session);
      sessionPolicies.set(normalizedSession, normalizeAllowedDomains(domains));
    }
  } catch {
    // Ignore storage load failures.
  }
}

function normalizeOptions(raw) {
  if (!raw || typeof raw !== 'object') {
    return { ...DEFAULT_EXTENSION_OPTIONS };
  }
  return {
    strictWindowIsolation: raw.strictWindowIsolation !== false,
    suppressCrossWindowActivation: raw.suppressCrossWindowActivation !== false,
    autoCleanEmptyGroups: raw.autoCleanEmptyGroups !== false,
  };
}

async function loadOptions() {
  try {
    const result = await chrome.storage.local.get([STORAGE_OPTIONS_KEY]);
    extensionOptions = normalizeOptions(result?.[STORAGE_OPTIONS_KEY]);
  } catch {
    extensionOptions = { ...DEFAULT_EXTENSION_OPTIONS };
  }
}

async function persistOptions() {
  await chrome.storage.local.set({ [STORAGE_OPTIONS_KEY]: extensionOptions });
}

async function setExtensionOptions(nextOptions) {
  extensionOptions = {
    ...extensionOptions,
    ...normalizeOptions(nextOptions),
  };
  await persistOptions();
  await syncCleanupAlarm();
  return extensionOptions;
}

async function bootstrapState() {
  await loadPolicies();
  await loadOptions();
  await syncCleanupAlarm();
}

async function persistPolicies() {
  const serialized = {};
  for (const [session, domains] of sessionPolicies.entries()) {
    serialized[session] = [...domains];
  }
  await chrome.storage.local.set({ [STORAGE_POLICY_KEY]: serialized });
}

async function setSessionPolicy(session, allowedDomains) {
  const normalizedSession = normalizeSession(session);
  const normalizedDomains = normalizeAllowedDomains(allowedDomains);
  sessionPolicies.set(normalizedSession, normalizedDomains);
  await persistPolicies();
}

function getSessionPolicy(session) {
  const normalizedSession = normalizeSession(session);
  return sessionPolicies.get(normalizedSession) ?? [];
}

function updateTabMeta(tab) {
  if (!tab || typeof tab.id !== 'number') return;
  tabMetaById.set(tab.id, {
    id: tab.id,
    windowId: typeof tab.windowId === 'number' ? tab.windowId : -1,
    url: typeof tab.url === 'string' ? tab.url : '',
    title: typeof tab.title === 'string' ? tab.title : '',
    groupId: typeof tab.groupId === 'number' ? tab.groupId : -1,
    active: tab.active === true,
    lastSeenAt: Date.now(),
  });
}

function pruneDownloadEvents() {
  const maxSize = 100;
  if (downloadEvents.length > maxSize) {
    downloadEvents.splice(0, downloadEvents.length - maxSize);
  }
}

function recordDownloadEvent(event) {
  downloadEvents.push({ ...event, timestamp: Date.now() });
  pruneDownloadEvents();
}

function removeWindowCaches(windowId) {
  for (const key of [...sessionGroupCache.keys()]) {
    if (key.startsWith(`${windowId}:`)) {
      sessionGroupCache.delete(key);
    }
  }
  for (const [session, mappedWindowId] of [...sessionWindowMap.entries()]) {
    if (mappedWindowId === windowId) {
      sessionWindowMap.delete(session);
    }
  }
}

async function ensureSessionWindow(tabId, currentWindowId, session) {
  if (!extensionOptions.strictWindowIsolation) {
    sessionWindowMap.set(session, currentWindowId);
    return currentWindowId;
  }

  let targetWindowId = sessionWindowMap.get(session);

  if (typeof targetWindowId === 'number') {
    try {
      await chrome.windows.get(targetWindowId);
    } catch {
      sessionWindowMap.delete(session);
      targetWindowId = undefined;
    }
  }

  if (typeof targetWindowId !== 'number') {
    sessionWindowMap.set(session, currentWindowId);
    return currentWindowId;
  }

  if (targetWindowId === currentWindowId) {
    return targetWindowId;
  }

  await chrome.tabs.move(tabId, { windowId: targetWindowId, index: -1 });
  await chrome.tabs.update(tabId, { active: false }).catch(() => {});
  return targetWindowId;
}

async function findExistingGroup(windowId, groupTitle) {
  const tabs = await chrome.tabs.query({ windowId });
  const checked = new Set();

  for (const tab of tabs) {
    if (typeof tab.groupId !== 'number' || tab.groupId < 0 || checked.has(tab.groupId)) {
      continue;
    }

    checked.add(tab.groupId);
    try {
      const group = await chrome.tabGroups.get(tab.groupId);
      if (group.title === groupTitle) {
        return tab.groupId;
      }
    } catch {
      // Ignore stale group references.
    }
  }

  return null;
}

async function ensureSessionGroup(tabId, windowId, session, groupTitle) {
  const targetWindowId = await ensureSessionWindow(tabId, windowId, session);
  const key = cacheKey(targetWindowId, session);
  let groupId = sessionGroupCache.get(key);

  if (typeof groupId === 'number') {
    try {
      await chrome.tabGroups.get(groupId);
    } catch {
      groupId = undefined;
    }
  }

  if (typeof groupId !== 'number') {
    const existing = await findExistingGroup(targetWindowId, groupTitle);
    if (typeof existing === 'number') {
      groupId = existing;
    }
  }

  if (typeof groupId === 'number') {
    await chrome.tabs.group({ groupId, tabIds: [tabId] });
  } else {
    groupId = await chrome.tabs.group({
      tabIds: [tabId],
      createProperties: { windowId: targetWindowId },
    });
  }

  const color = pickColorForSession(session);
  const collapsed = shouldCollapseGroup(session);
  await chrome.tabGroups.update(groupId, {
    title: groupTitle,
    color,
    collapsed,
  });

  sessionGroupCache.set(key, groupId);
  sessionWindowMap.set(session, targetWindowId);

  return {
    groupId,
    windowId: targetWindowId,
    color,
    collapsed,
  };
}

async function applySessionDomainFallback(tabId, session) {
  const allowedDomains = getSessionPolicy(session);
  if (allowedDomains.length === 0) {
    return { enforced: false, blocked: false };
  }

  let tab;
  try {
    tab = await chrome.tabs.get(tabId);
  } catch {
    return { enforced: true, blocked: false };
  }

  const hostname = parseHostname(tab.url);
  if (!hostname) {
    return { enforced: true, blocked: false };
  }

  if (isDomainAllowed(hostname, allowedDomains)) {
    return { enforced: true, blocked: false };
  }

  await chrome.tabs.update(tabId, { url: 'about:blank' }).catch(() => {});
  return {
    enforced: true,
    blocked: true,
    reason: `${hostname} is not in allowed domains`,
  };
}

function getManagedSessionForTab(tabId) {
  if (typeof tabId !== 'number') return undefined;
  return tabSessionMap.get(tabId);
}

function collectSessionTabIds(session) {
  const result = [];
  for (const [tabId, tabSession] of tabSessionMap.entries()) {
    if (tabSession === session) {
      result.push(tabId);
    }
  }
  return result;
}

async function closeOtherSessionTabs(session) {
  const normalized = normalizeSession(session);
  const closeIds = [];

  for (const [tabId, tabSession] of tabSessionMap.entries()) {
    if (tabSession !== normalized) {
      closeIds.push(tabId);
    }
  }

  if (closeIds.length > 0) {
    await chrome.tabs.remove(closeIds);
  }

  return { closed: closeIds.length };
}

async function focusSession(session) {
  const normalized = normalizeSession(session);
  const tabIds = collectSessionTabIds(normalized);
  if (tabIds.length === 0) {
    return { focused: false };
  }

  let tab;
  try {
    tab = await chrome.tabs.get(tabIds[0]);
  } catch {
    return { focused: false };
  }

  if (typeof tab.windowId === 'number') {
    await chrome.windows.update(tab.windowId, { focused: true }).catch(() => {});
  }
  await chrome.tabs.update(tab.id, { active: true }).catch(() => {});
  return { focused: true, tabId: tab.id };
}

async function cleanEmptyGroups() {
  let removedGroups = 0;
  let removedWindows = 0;

  for (const [key, groupId] of [...sessionGroupCache.entries()]) {
    const [windowIdRaw] = key.split(':');
    const windowId = Number(windowIdRaw);

    let groupExists = true;
    try {
      await chrome.tabGroups.get(groupId);
    } catch {
      groupExists = false;
    }

    if (!groupExists) {
      sessionGroupCache.delete(key);
      removedGroups += 1;
      continue;
    }

    const tabs = await chrome.tabs.query({ windowId }).catch(() => []);
    const hasMembers = tabs.some((tab) => tab.groupId === groupId);
    if (!hasMembers) {
      sessionGroupCache.delete(key);
      removedGroups += 1;
    }
  }

  for (const [session, windowId] of [...sessionWindowMap.entries()]) {
    try {
      await chrome.windows.get(windowId);
    } catch {
      sessionWindowMap.delete(session);
      removedWindows += 1;
    }
  }

  return { removedGroups, removedWindows };
}

async function syncCleanupAlarm() {
  try {
    await chrome.alarms.clear(CLEANUP_ALARM_NAME);
    if (extensionOptions.autoCleanEmptyGroups) {
      await chrome.alarms.create(CLEANUP_ALARM_NAME, { periodInMinutes: 1 });
    }
  } catch {
    // Ignore alarms API failures.
  }
}

async function enforceSessionWindowAffinity(tabId) {
  if (!extensionOptions.suppressCrossWindowActivation) return { moved: false };

  const session = getManagedSessionForTab(tabId);
  if (!session) return { moved: false };
  if (!extensionOptions.strictWindowIsolation) return { moved: false };

  let tab;
  try {
    tab = await chrome.tabs.get(tabId);
  } catch {
    return { moved: false };
  }

  const mappedWindowId = sessionWindowMap.get(session);
  if (typeof mappedWindowId !== 'number' || mappedWindowId === tab.windowId) {
    if (typeof tab.windowId === 'number') {
      sessionWindowMap.set(session, tab.windowId);
    }
    return { moved: false };
  }

  try {
    await chrome.tabs.move(tabId, { windowId: mappedWindowId, index: -1 });
    await chrome.tabs.update(tabId, { active: false }).catch(() => {});
    return { moved: true, toWindowId: mappedWindowId };
  } catch {
    return { moved: false };
  }
}

async function updateRiskBadge(tabId) {
  let text = '';
  let title = 'agent-browser-stealth';

  const session = getManagedSessionForTab(tabId);
  if (session) {
    let tab;
    try {
      tab = await chrome.tabs.get(tabId);
    } catch {
      tab = undefined;
    }

    if (tab) {
      const hints = collectRiskHints(tab.url, getSessionPolicy(session));
      if (hints.length > 0) {
        text = '!';
        title = `Risk hints (${hints.length}): ${hints.join(', ')}`;
      }
    }
  }

  await chrome.action.setBadgeText({ text }).catch(() => {});
  await chrome.action.setBadgeBackgroundColor({ color: '#dc2626' }).catch(() => {});
  await chrome.action.setTitle({ title }).catch(() => {});
}

async function buildPanelState() {
  const allTabs = await chrome.tabs.query({});
  for (const tab of allTabs) {
    updateTabMeta(tab);
  }

  const sessionMap = new Map();

  for (const tab of allTabs) {
    if (typeof tab.id !== 'number') continue;
    const session = getManagedSessionForTab(tab.id);
    if (!session) continue;

    if (!sessionMap.has(session)) {
      sessionMap.set(session, {
        session,
        windowId: sessionWindowMap.get(session) ?? tab.windowId,
        allowedDomains: getSessionPolicy(session),
        tabs: [],
        riskHints: [],
      });
    }

    const entry = sessionMap.get(session);
    entry.tabs.push({
      id: tab.id,
      windowId: tab.windowId,
      title: tab.title ?? '',
      url: tab.url ?? '',
      active: tab.active === true,
      groupId: typeof tab.groupId === 'number' ? tab.groupId : -1,
    });

    const hints = collectRiskHints(tab.url, entry.allowedDomains);
    for (const hint of hints) {
      if (!entry.riskHints.includes(hint)) {
        entry.riskHints.push(hint);
      }
    }
  }

  const sessions = [];
  for (const sessionEntry of sessionMap.values()) {
    sessionEntry.tabs.sort((a, b) => Number(b.active) - Number(a.active));
    const key = cacheKey(sessionEntry.windowId, sessionEntry.session);
    const cachedGroupId = sessionGroupCache.get(key);

    let group;
    if (typeof cachedGroupId === 'number') {
      try {
        const groupInfo = await chrome.tabGroups.get(cachedGroupId);
        group = {
          id: cachedGroupId,
          title: groupInfo.title,
          color: groupInfo.color,
          collapsed: groupInfo.collapsed,
        };
      } catch {
        // Group may no longer exist.
      }
    }

    sessions.push({
      ...sessionEntry,
      group,
    });
  }

  sessions.sort((a, b) => a.session.localeCompare(b.session));

  return {
    extensionId: chrome.runtime.id,
    options: { ...extensionOptions },
    totals: {
      sessions: sessions.length,
      tabs: sessions.reduce((sum, session) => sum + session.tabs.length, 0),
    },
    sessions,
    downloads: downloadEvents.slice(-25).reverse(),
  };
}

async function handleTabGroupRequest(message, sender) {
  await bootstrapPromise;

  const tabId = sender.tab?.id;
  const windowId = sender.tab?.windowId;
  const nonce = typeof message.nonce === 'string' ? message.nonce : undefined;

  if (typeof tabId !== 'number' || typeof windowId !== 'number') {
    return {
      ok: false,
      error: 'missing-tab-context',
      extensionId: chrome.runtime.id,
      nonce,
    };
  }

  if (typeof message.pluginId === 'string' && message.pluginId !== chrome.runtime.id) {
    return {
      ok: false,
      error: 'plugin-id-mismatch',
      extensionId: chrome.runtime.id,
      nonce,
    };
  }

  const session = normalizeSession(message.session);
  const groupTitle = normalizeGroupTitle(message.groupTitle);
  const allowedDomains = normalizeAllowedDomains(message.allowedDomains);
  if (allowedDomains.length > 0) {
    await setSessionPolicy(session, allowedDomains);
  }

  tabSessionMap.set(tabId, session);
  updateTabMeta(sender.tab);

  const grouping = await ensureSessionGroup(tabId, windowId, session, groupTitle);
  const policy = await applySessionDomainFallback(tabId, session);
  const riskHints = collectRiskHints(sender.tab?.url, getSessionPolicy(session));
  if (policy.blocked && policy.reason) {
    riskHints.push(`policy-blocked:${policy.reason}`);
  }

  return {
    ok: true,
    extensionId: chrome.runtime.id,
    nonce,
    ...grouping,
    policy,
    riskHints: [...new Set(riskHints)].slice(0, 10),
  };
}

chrome.runtime.onInstalled.addListener(() => {
  chrome.sidePanel.setPanelBehavior({ openPanelOnActionClick: true }).catch(() => {});
  bootstrapPromise = bootstrapState();
});

chrome.runtime.onStartup.addListener(() => {
  bootstrapPromise = bootstrapState();
});

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  if (!message || typeof message !== 'object') {
    return;
  }

  const type = message.type;

  if (type === REQUEST_TYPE) {
    handleTabGroupRequest(message, sender)
      .then((response) => sendResponse(response))
      .catch((error) => {
        const errorMessage = error instanceof Error ? error.message : String(error);
        sendResponse({
          ok: false,
          error: errorMessage,
          extensionId: chrome.runtime.id,
          nonce: typeof message.nonce === 'string' ? message.nonce : undefined,
        });
      });
    return true;
  }

  if (type === PANEL_GET_STATE) {
    bootstrapPromise
      .then(() => buildPanelState())
      .then((state) => sendResponse({ ok: true, state }))
      .catch((error) => {
        const errorMessage = error instanceof Error ? error.message : String(error);
        sendResponse({ ok: false, error: errorMessage });
      });
    return true;
  }

  if (type === PANEL_CLOSE_OTHER_TABS) {
    closeOtherSessionTabs(message.session)
      .then((result) => sendResponse({ ok: true, result }))
      .catch((error) => {
        const errorMessage = error instanceof Error ? error.message : String(error);
        sendResponse({ ok: false, error: errorMessage });
      });
    return true;
  }

  if (type === PANEL_FOCUS_SESSION) {
    focusSession(message.session)
      .then((result) => sendResponse({ ok: true, result }))
      .catch((error) => {
        const errorMessage = error instanceof Error ? error.message : String(error);
        sendResponse({ ok: false, error: errorMessage });
      });
    return true;
  }

  if (type === PANEL_CLEAN_EMPTY_GROUPS) {
    cleanEmptyGroups()
      .then((result) => sendResponse({ ok: true, result }))
      .catch((error) => {
        const errorMessage = error instanceof Error ? error.message : String(error);
        sendResponse({ ok: false, error: errorMessage });
      });
    return true;
  }

  if (type === PANEL_SET_POLICY) {
    bootstrapPromise
      .then(() => setSessionPolicy(message.session, message.allowedDomains))
      .then(() => sendResponse({ ok: true }))
      .catch((error) => {
        const errorMessage = error instanceof Error ? error.message : String(error);
        sendResponse({ ok: false, error: errorMessage });
      });
    return true;
  }

  if (type === PANEL_SET_OPTIONS) {
    bootstrapPromise
      .then(() => setExtensionOptions(message.options))
      .then((options) => sendResponse({ ok: true, options }))
      .catch((error) => {
        const errorMessage = error instanceof Error ? error.message : String(error);
        sendResponse({ ok: false, error: errorMessage });
      });
    return true;
  }
});

chrome.tabs.onUpdated.addListener((tabId, changeInfo, tab) => {
  updateTabMeta(tab);
  const session = getManagedSessionForTab(tabId);
  if (!session) {
    if (changeInfo.status === 'complete' && tab.active === true) {
      updateRiskBadge(tabId).catch(() => {});
    }
    return;
  }

  if (typeof tab.windowId === 'number') {
    sessionWindowMap.set(session, tab.windowId);
  }

  if (changeInfo.status === 'complete') {
    applySessionDomainFallback(tabId, session).catch(() => {});
    if (tab.active === true) {
      updateRiskBadge(tabId).catch(() => {});
    }
  }
});

chrome.tabs.onActivated.addListener((activeInfo) => {
  enforceSessionWindowAffinity(activeInfo.tabId).catch(() => {});
  updateRiskBadge(activeInfo.tabId).catch(() => {});
});

chrome.tabs.onRemoved.addListener((tabId, removeInfo) => {
  const session = getManagedSessionForTab(tabId);
  tabSessionMap.delete(tabId);
  tabMetaById.delete(tabId);

  if (removeInfo.isWindowClosing) {
    removeWindowCaches(removeInfo.windowId);
    return;
  }

  if (!session) return;
  const remaining = collectSessionTabIds(session);
  if (remaining.length === 0) {
    sessionWindowMap.delete(session);
  }
  cleanEmptyGroups().catch(() => {});
});

chrome.tabs.onDetached.addListener((tabId) => {
  const session = getManagedSessionForTab(tabId);
  if (!session) return;
  tabMetaById.delete(tabId);
});

chrome.tabs.onAttached.addListener((tabId, attachInfo) => {
  const session = getManagedSessionForTab(tabId);
  if (!session) return;
  sessionWindowMap.set(session, attachInfo.newWindowId);
});

chrome.windows.onRemoved.addListener((windowId) => {
  removeWindowCaches(windowId);
  cleanEmptyGroups().catch(() => {});
});

chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm?.name !== CLEANUP_ALARM_NAME) return;
  cleanEmptyGroups().catch(() => {});
});

chrome.downloads.onDeterminingFilename.addListener((item, suggest) => {
  const session = getManagedSessionForTab(item.tabId);
  if (!session) {
    suggest();
    return;
  }

  const safeSession = sanitizeSegment(session, 'default');
  const safeFilename = sanitizeFilename(item.filename, `download-${item.id}.bin`);
  const filename = `${DOWNLOAD_ARCHIVE_ROOT}/${safeSession}/${safeFilename}`;

  recordDownloadEvent({
    id: item.id,
    tabId: item.tabId,
    session,
    state: 'routing',
    filename,
  });

  suggest({
    filename,
    conflictAction: 'uniquify',
  });
});

chrome.downloads.onChanged.addListener((delta) => {
  if (!delta || typeof delta.id !== 'number') return;

  const state = delta.state?.current;
  if (!state) return;

  recordDownloadEvent({
    id: delta.id,
    state,
    filename: delta.filename?.current,
  });
});
