const REQUEST_TYPE = 'AB_TAB_GROUP_REQUEST';
const PANEL_GET_STATE = 'AB_PANEL_GET_STATE';
const PANEL_CLOSE_OTHER_TABS = 'AB_PANEL_CLOSE_OTHER_SESSION_TABS';
const PANEL_FOCUS_SESSION = 'AB_PANEL_FOCUS_SESSION';
const PANEL_CLEAN_EMPTY_GROUPS = 'AB_PANEL_CLEAN_EMPTY_GROUPS';
const PANEL_SET_POLICY = 'AB_PANEL_SET_POLICY';
const PANEL_SET_OPTIONS = 'AB_PANEL_SET_OPTIONS';

const PANEL_RUN_ACTION = 'AB_PANEL_RUN_ACTION';
const PANEL_CLEAR_ACTIVITY = 'AB_PANEL_CLEAR_ACTIVITY';
const PANEL_START_RECORDING = 'AB_PANEL_START_RECORDING';
const PANEL_STOP_RECORDING = 'AB_PANEL_STOP_RECORDING';
const PANEL_SAVE_RECORDING = 'AB_PANEL_SAVE_RECORDING';
const PANEL_RUN_WORKFLOW = 'AB_PANEL_RUN_WORKFLOW';
const PANEL_DELETE_WORKFLOW = 'AB_PANEL_DELETE_WORKFLOW';
const PANEL_SET_SHORTCUT = 'AB_PANEL_SET_SHORTCUT';
const PANEL_DELETE_SHORTCUT = 'AB_PANEL_DELETE_SHORTCUT';
const PANEL_RUN_SHORTCUT = 'AB_PANEL_RUN_SHORTCUT';
const PANEL_CREATE_SCHEDULE = 'AB_PANEL_CREATE_SCHEDULE';
const PANEL_DELETE_SCHEDULE = 'AB_PANEL_DELETE_SCHEDULE';
const PANEL_TOGGLE_SCHEDULE = 'AB_PANEL_TOGGLE_SCHEDULE';

const CONTENT_EVENT_TYPE = 'AB_CONTENT_EVENT';
const CONTENT_EXECUTE_ACTION = 'AB_CONTENT_EXECUTE_ACTION';
const CONTENT_GET_DOM_STATE = 'AB_CONTENT_GET_DOM_STATE';
const CONTENT_PING = 'AB_CONTENT_PING';

const DEFAULT_GROUP_TITLE = 'Agent Browser Stealth';
const DOWNLOAD_ARCHIVE_ROOT = 'agent-browser-stealth';
const STORAGE_POLICY_KEY = 'abSessionPoliciesV1';
const STORAGE_OPTIONS_KEY = 'abExtensionOptionsV1';
const STORAGE_WORKFLOWS_KEY = 'abWorkflowsV1';
const STORAGE_SHORTCUTS_KEY = 'abShortcutsV1';
const STORAGE_SCHEDULES_KEY = 'abSchedulesV1';

const CLEANUP_ALARM_NAME = 'ab-clean-empty-groups';
const WORKFLOW_ALARM_PREFIX = 'ab-workflow-schedule:';

const GROUP_COLORS = ['blue', 'green', 'pink', 'orange', 'purple', 'cyan', 'red', 'yellow'];
const RISKY_TLDS = new Set(['zip', 'mov', 'click', 'top', 'gq', 'tk', 'country']);
const RISKY_HOST_KEYWORDS = ['secure-login', 'account-verify', 'wallet-verify', 'airdrop-claim'];

const DEFAULT_EXTENSION_OPTIONS = {
  strictWindowIsolation: true,
  suppressCrossWindowActivation: true,
  autoCleanEmptyGroups: true,
  pageBridgeEnabled: false,
};

const MAX_ACTIVITY_EVENTS = 500;
const COMMAND_HISTORY_LIMIT = 120;

const sessionGroupCache = new Map();
const sessionGroupTitleMap = new Map();
const sessionWindowMap = new Map();
const tabSessionMap = new Map();
const tabMetaById = new Map();
const downloadEvents = [];
const sessionPolicies = new Map();
const workflows = new Map();
const shortcuts = new Map();
const schedules = new Map();

const activityEvents = [];
const commandHistory = [];
let latestDomState = null;

let extensionOptions = { ...DEFAULT_EXTENSION_OPTIONS };
// Transient workflow recorder state. Persisted only when saved as a workflow.
let recordingState = null;
let bootstrapPromise = bootstrapState();
let eventCounter = 0;

function now() {
  return Date.now();
}

function uid(prefix) {
  const rand = Math.random().toString(36).slice(2, 10);
  return `${prefix}-${Date.now().toString(36)}-${rand}`;
}

function clampInt(value, min, max, fallback) {
  const parsed = Number.parseInt(String(value ?? ''), 10);
  if (!Number.isFinite(parsed)) return fallback;
  if (parsed < min) return min;
  if (parsed > max) return max;
  return parsed;
}

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

function normalizeUrl(rawUrl) {
  if (typeof rawUrl !== 'string') return null;
  const trimmed = rawUrl.trim();
  if (!trimmed) return null;

  if (/^(https?|file|about|data|blob|chrome-extension):/i.test(trimmed)) {
    return trimmed;
  }

  return `https://${trimmed}`;
}

function normalizeShortcutName(name) {
  if (typeof name !== 'string') return null;
  const trimmed = name.trim().toLowerCase();
  if (!trimmed) return null;
  const normalized = trimmed.replace(/[^a-z0-9:_-]/g, '-').replace(/-+/g, '-').slice(0, 48);
  return normalized || null;
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

function defaultGroupTitleForSession(session) {
  const normalized = normalizeSession(session);
  if (normalized === 'default') {
    return DEFAULT_GROUP_TITLE;
  }
  return normalizeGroupTitle(`${DEFAULT_GROUP_TITLE} • ${normalized}`);
}

function getGroupTitleForSession(session) {
  const normalized = normalizeSession(session);
  return sessionGroupTitleMap.get(normalized) || defaultGroupTitleForSession(normalized);
}

function createActivityEvent(kind, payload, meta = {}) {
  return {
    id: ++eventCounter,
    kind,
    payload,
    tabId: typeof meta.tabId === 'number' ? meta.tabId : null,
    session: typeof meta.session === 'string' ? meta.session : null,
    url: typeof meta.url === 'string' ? meta.url : '',
    title: typeof meta.title === 'string' ? meta.title : '',
    source: typeof meta.source === 'string' ? meta.source : 'extension',
    timestamp: now(),
  };
}

function pushActivityEvent(kind, payload, meta = {}) {
  activityEvents.push(createActivityEvent(kind, payload, meta));
  if (activityEvents.length > MAX_ACTIVITY_EVENTS) {
    activityEvents.splice(0, activityEvents.length - MAX_ACTIVITY_EVENTS);
  }
}

function pushCommandHistory(entry) {
  commandHistory.push({
    ...entry,
    timestamp: now(),
  });
  if (commandHistory.length > COMMAND_HISTORY_LIMIT) {
    commandHistory.splice(0, commandHistory.length - COMMAND_HISTORY_LIMIT);
  }
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
    pageBridgeEnabled: raw.pageBridgeEnabled === true,
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
  const merged = {
    ...extensionOptions,
    ...(nextOptions && typeof nextOptions === 'object' ? nextOptions : {}),
  };
  extensionOptions = normalizeOptions(merged);
  await persistOptions();
  await syncCleanupAlarm();
  return extensionOptions;
}

function normalizeWorkflowStep(rawStep) {
  if (!rawStep || typeof rawStep !== 'object') return null;
  if (typeof rawStep.action !== 'string' || rawStep.action.trim().length === 0) return null;

  const action = rawStep.action.trim();
  return {
    id: typeof rawStep.id === 'string' ? rawStep.id : uid('step'),
    action,
    args: rawStep.args && typeof rawStep.args === 'object' ? rawStep.args : {},
    timeoutMs: clampInt(rawStep.timeoutMs, 0, 120_000, 0),
    retries: clampInt(rawStep.retries, 0, 5, 0),
  };
}

function normalizeWorkflow(rawWorkflow) {
  if (!rawWorkflow || typeof rawWorkflow !== 'object') return null;
  if (typeof rawWorkflow.id !== 'string' || rawWorkflow.id.length === 0) return null;

  const steps = Array.isArray(rawWorkflow.steps)
    ? rawWorkflow.steps.map((step) => normalizeWorkflowStep(step)).filter(Boolean)
    : [];

  return {
    id: rawWorkflow.id,
    name:
      typeof rawWorkflow.name === 'string' && rawWorkflow.name.trim().length > 0
        ? rawWorkflow.name.trim().slice(0, 120)
        : rawWorkflow.id,
    steps,
    createdAt: clampInt(rawWorkflow.createdAt, 0, Number.MAX_SAFE_INTEGER, now()),
    updatedAt: clampInt(rawWorkflow.updatedAt, 0, Number.MAX_SAFE_INTEGER, now()),
  };
}

function normalizeCadence(rawCadence) {
  if (!rawCadence || typeof rawCadence !== 'object') {
    return {
      kind: 'daily',
      hour: 9,
      minute: 0,
    };
  }

  const kind =
    rawCadence.kind === 'weekly' ||
    rawCadence.kind === 'monthly' ||
    rawCadence.kind === 'yearly' ||
    rawCadence.kind === 'daily'
      ? rawCadence.kind
      : 'daily';

  const cadence = {
    kind,
    hour: clampInt(rawCadence.hour, 0, 23, 9),
    minute: clampInt(rawCadence.minute, 0, 59, 0),
  };

  if (kind === 'weekly') {
    const weekdays = Array.isArray(rawCadence.weekdays)
      ? rawCadence.weekdays.map((day) => clampInt(day, 0, 6, 1))
      : [clampInt(rawCadence.weekday, 0, 6, 1)];
    cadence.weekdays = [...new Set(weekdays)].slice(0, 7);
  }

  if (kind === 'monthly') {
    cadence.dayOfMonth = clampInt(rawCadence.dayOfMonth, 1, 31, 1);
  }

  if (kind === 'yearly') {
    cadence.month = clampInt(rawCadence.month, 1, 12, 1);
    cadence.dayOfMonth = clampInt(rawCadence.dayOfMonth, 1, 31, 1);
  }

  return cadence;
}

function daysInMonth(year, monthIndex) {
  return new Date(year, monthIndex + 1, 0).getDate();
}

function computeNextRun(cadence, fromTs = now()) {
  const from = new Date(fromTs + 1000);
  const hour = clampInt(cadence.hour, 0, 23, 9);
  const minute = clampInt(cadence.minute, 0, 59, 0);

  if (cadence.kind === 'weekly') {
    const weekdays = Array.isArray(cadence.weekdays) && cadence.weekdays.length > 0
      ? cadence.weekdays.map((day) => clampInt(day, 0, 6, 1))
      : [1];

    for (let offset = 0; offset < 14; offset += 1) {
      const candidate = new Date(from);
      candidate.setDate(from.getDate() + offset);
      candidate.setHours(hour, minute, 0, 0);
      if (candidate <= from) continue;
      if (weekdays.includes(candidate.getDay())) {
        return candidate.getTime();
      }
    }
  }

  if (cadence.kind === 'monthly') {
    const dayOfMonth = clampInt(cadence.dayOfMonth, 1, 31, 1);
    const candidate = new Date(from);

    for (let i = 0; i < 24; i += 1) {
      const monthDate = new Date(candidate.getFullYear(), candidate.getMonth() + i, 1);
      const maxDay = daysInMonth(monthDate.getFullYear(), monthDate.getMonth());
      monthDate.setDate(Math.min(dayOfMonth, maxDay));
      monthDate.setHours(hour, minute, 0, 0);
      if (monthDate > from) {
        return monthDate.getTime();
      }
    }
  }

  if (cadence.kind === 'yearly') {
    const month = clampInt(cadence.month, 1, 12, 1) - 1;
    const dayOfMonth = clampInt(cadence.dayOfMonth, 1, 31, 1);

    for (let yearOffset = 0; yearOffset < 5; yearOffset += 1) {
      const year = from.getFullYear() + yearOffset;
      const maxDay = daysInMonth(year, month);
      const candidate = new Date(year, month, Math.min(dayOfMonth, maxDay), hour, minute, 0, 0);
      if (candidate > from) {
        return candidate.getTime();
      }
    }
  }

  const dailyCandidate = new Date(from);
  dailyCandidate.setHours(hour, minute, 0, 0);
  if (dailyCandidate <= from) {
    dailyCandidate.setDate(dailyCandidate.getDate() + 1);
  }
  return dailyCandidate.getTime();
}

function normalizeSchedule(rawSchedule) {
  if (!rawSchedule || typeof rawSchedule !== 'object') return null;
  if (typeof rawSchedule.id !== 'string' || rawSchedule.id.length === 0) return null;
  if (typeof rawSchedule.workflowId !== 'string' || rawSchedule.workflowId.length === 0) return null;

  const cadence = normalizeCadence(rawSchedule.cadence);

  const nextRunAt =
    typeof rawSchedule.nextRunAt === 'number' && Number.isFinite(rawSchedule.nextRunAt)
      ? rawSchedule.nextRunAt
      : computeNextRun(cadence);

  return {
    id: rawSchedule.id,
    name:
      typeof rawSchedule.name === 'string' && rawSchedule.name.trim().length > 0
        ? rawSchedule.name.trim().slice(0, 120)
        : rawSchedule.id,
    workflowId: rawSchedule.workflowId,
    cadence,
    enabled: rawSchedule.enabled !== false,
    createdAt: clampInt(rawSchedule.createdAt, 0, Number.MAX_SAFE_INTEGER, now()),
    updatedAt: clampInt(rawSchedule.updatedAt, 0, Number.MAX_SAFE_INTEGER, now()),
    lastRunAt:
      typeof rawSchedule.lastRunAt === 'number' && Number.isFinite(rawSchedule.lastRunAt)
        ? rawSchedule.lastRunAt
        : null,
    nextRunAt,
  };
}

async function persistWorkflows() {
  const payload = [...workflows.values()];
  await chrome.storage.local.set({ [STORAGE_WORKFLOWS_KEY]: payload });
}

async function persistShortcuts() {
  const payload = Object.fromEntries(shortcuts.entries());
  await chrome.storage.local.set({ [STORAGE_SHORTCUTS_KEY]: payload });
}

async function persistSchedules() {
  const payload = [...schedules.values()];
  await chrome.storage.local.set({ [STORAGE_SCHEDULES_KEY]: payload });
}

async function loadAutomationState() {
  try {
    const result = await chrome.storage.local.get([
      STORAGE_WORKFLOWS_KEY,
      STORAGE_SHORTCUTS_KEY,
      STORAGE_SCHEDULES_KEY,
    ]);

    const workflowEntries = Array.isArray(result?.[STORAGE_WORKFLOWS_KEY])
      ? result[STORAGE_WORKFLOWS_KEY]
      : [];

    workflows.clear();
    for (const entry of workflowEntries) {
      const workflow = normalizeWorkflow(entry);
      if (!workflow) continue;
      workflows.set(workflow.id, workflow);
    }

    const shortcutEntries =
      result?.[STORAGE_SHORTCUTS_KEY] && typeof result[STORAGE_SHORTCUTS_KEY] === 'object'
        ? result[STORAGE_SHORTCUTS_KEY]
        : {};

    shortcuts.clear();
    for (const [name, workflowId] of Object.entries(shortcutEntries)) {
      const shortcutName = normalizeShortcutName(name);
      if (!shortcutName) continue;
      if (typeof workflowId !== 'string') continue;
      if (!workflows.has(workflowId)) continue;
      shortcuts.set(shortcutName, workflowId);
    }

    const scheduleEntries = Array.isArray(result?.[STORAGE_SCHEDULES_KEY])
      ? result[STORAGE_SCHEDULES_KEY]
      : [];

    schedules.clear();
    for (const entry of scheduleEntries) {
      const schedule = normalizeSchedule(entry);
      if (!schedule) continue;
      if (!workflows.has(schedule.workflowId)) continue;
      schedules.set(schedule.id, schedule);
    }
  } catch {
    workflows.clear();
    shortcuts.clear();
    schedules.clear();
  }
}

async function scheduleWorkflowAlarm(schedule) {
  const alarmName = `${WORKFLOW_ALARM_PREFIX}${schedule.id}`;
  await chrome.alarms.clear(alarmName);

  if (!schedule.enabled) {
    return;
  }

  if (typeof schedule.nextRunAt !== 'number' || !Number.isFinite(schedule.nextRunAt)) {
    schedule.nextRunAt = computeNextRun(schedule.cadence);
    schedule.updatedAt = now();
  }

  await chrome.alarms.create(alarmName, {
    when: Math.max(schedule.nextRunAt, now() + 5_000),
  });
}

async function syncAllWorkflowAlarms() {
  for (const schedule of schedules.values()) {
    await scheduleWorkflowAlarm(schedule);
  }
}

async function bootstrapState() {
  await loadPolicies();
  await loadOptions();
  await loadAutomationState();
  await syncCleanupAlarm();
  await syncAllWorkflowAlarms();
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
    lastSeenAt: now(),
  });
}

function pruneDownloadEvents() {
  const maxSize = 100;
  if (downloadEvents.length > maxSize) {
    downloadEvents.splice(0, downloadEvents.length - maxSize);
  }
}

function recordDownloadEvent(event) {
  downloadEvents.push({ ...event, timestamp: now() });
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
  sessionGroupTitleMap.set(session, groupTitle);

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

async function ensureManagedTab(tabId, sessionHint) {
  if (typeof tabId !== 'number') return null;

  let tab;
  try {
    tab = await chrome.tabs.get(tabId);
  } catch {
    return null;
  }

  if (typeof tab.windowId !== 'number') {
    return null;
  }

  const session = normalizeSession(sessionHint || getManagedSessionForTab(tabId) || 'default');
  const groupTitle = getGroupTitleForSession(session);
  tabSessionMap.set(tabId, session);
  updateTabMeta(tab);

  try {
    const grouping = await ensureSessionGroup(tabId, tab.windowId, session, groupTitle);
    return { session, groupTitle, grouping };
  } catch {
    // Best-effort grouping: keep session mapping even if group APIs fail.
    return { session, groupTitle, grouping: null };
  }
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

function buildWorkflowSummary() {
  return [...workflows.values()].map((workflow) => ({
    id: workflow.id,
    name: workflow.name,
    stepCount: workflow.steps.length,
    steps: workflow.steps,
    createdAt: workflow.createdAt,
    updatedAt: workflow.updatedAt,
  }));
}

function buildShortcutSummary() {
  return [...shortcuts.entries()].map(([name, workflowId]) => ({
    name,
    workflowId,
    workflowName: workflows.get(workflowId)?.name || workflowId,
  }));
}

function buildScheduleSummary() {
  return [...schedules.values()].map((schedule) => ({
    ...schedule,
    workflowName: workflows.get(schedule.workflowId)?.name || schedule.workflowId,
  }));
}

function buildActivitySummary() {
  const recent = activityEvents.slice(-200).reverse();
  return {
    events: recent,
    console: recent.filter((event) => event.kind === 'console').slice(0, 80),
    network: recent.filter((event) => event.kind === 'network').slice(0, 120),
    commandHistory: commandHistory.slice(-80).reverse(),
  };
}

async function getControlState() {
  const tabs = await chrome.tabs.query({ currentWindow: true });
  const activeTab = tabs.find((tab) => tab.active === true) || tabs[0] || null;

  return {
    activeTab:
      activeTab && typeof activeTab.id === 'number'
        ? {
            id: activeTab.id,
            title: activeTab.title || '',
            url: activeTab.url || '',
            windowId: activeTab.windowId,
          }
        : null,
    tabs: tabs.map((tab) => ({
      id: tab.id,
      index: tab.index,
      title: tab.title || '(Untitled)',
      url: tab.url || '',
      active: tab.active === true,
      windowId: tab.windowId,
      session: getManagedSessionForTab(tab.id),
    })),
  };
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

  const control = await getControlState();

  return {
    extensionId: chrome.runtime.id,
    options: { ...extensionOptions },
    totals: {
      sessions: sessions.length,
      tabs: sessions.reduce((sum, session) => sum + session.tabs.length, 0),
    },
    sessions,
    downloads: downloadEvents.slice(-25).reverse(),
    latestDomState,
    control,
    activity: buildActivitySummary(),
    automation: {
      recording: recordingState
        ? {
            id: recordingState.id,
            name: recordingState.name,
            startedAt: recordingState.startedAt,
            stoppedAt: recordingState.stoppedAt,
            stepCount: recordingState.steps.length,
            steps: recordingState.steps,
          }
        : null,
      workflows: buildWorkflowSummary(),
      shortcuts: buildShortcutSummary(),
      schedules: buildScheduleSummary(),
    },
  };
}

async function waitForTabSettled(tabId, timeoutMs = 15_000) {
  return new Promise((resolve, reject) => {
    const deadline = now() + timeoutMs;

    const timer = setTimeout(() => {
      chrome.tabs.onUpdated.removeListener(onUpdated);
      reject(new Error('tab-load-timeout'));
    }, timeoutMs);

    const onUpdated = (updatedTabId, changeInfo) => {
      if (updatedTabId !== tabId) return;
      if (changeInfo.status === 'complete') {
        clearTimeout(timer);
        chrome.tabs.onUpdated.removeListener(onUpdated);
        resolve(true);
      }
    };

    chrome.tabs.onUpdated.addListener(onUpdated);

    chrome.tabs
      .get(tabId)
      .then((tab) => {
        if (tab.status === 'complete') {
          clearTimeout(timer);
          chrome.tabs.onUpdated.removeListener(onUpdated);
          resolve(true);
          return;
        }

        if (now() > deadline) {
          clearTimeout(timer);
          chrome.tabs.onUpdated.removeListener(onUpdated);
          reject(new Error('tab-load-timeout'));
        }
      })
      .catch(() => {
        clearTimeout(timer);
        chrome.tabs.onUpdated.removeListener(onUpdated);
        reject(new Error('tab-not-found'));
      });
  });
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function getOrCreateActionTab(tabId) {
  if (typeof tabId === 'number') {
    try {
      const tab = await chrome.tabs.get(tabId);
      return tab;
    } catch {
      // fall through
    }
  }

  const [activeTab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (activeTab && typeof activeTab.id === 'number') {
    return activeTab;
  }

  return chrome.tabs.create({ url: 'about:blank', active: true });
}

async function sendContentCommand(tabId, message) {
  try {
    return await chrome.tabs.sendMessage(tabId, message);
  } catch (error) {
    return {
      ok: false,
      error: error instanceof Error ? error.message : String(error),
    };
  }
}

function shouldRecordAction(action) {
  return ![
    'tabs:list',
    'dom-state',
    'snapshot',
    'shortcut:run',
    'workflow:run',
  ].includes(action);
}

function recordWorkflowStep(action, args) {
  if (!recordingState) return;
  if (!shouldRecordAction(action)) return;

  recordingState.steps.push({
    id: uid('step'),
    action,
    args: args && typeof args === 'object' ? { ...args } : {},
    retries: 0,
    timeoutMs: 0,
  });
}

async function runShortcutByName(name, options = {}) {
  const shortcutName = normalizeShortcutName(name);
  if (!shortcutName) {
    return { ok: false, error: 'shortcut-name-required' };
  }

  const workflowId = shortcuts.get(shortcutName);
  if (!workflowId) {
    return { ok: false, error: `shortcut-not-found:${shortcutName}` };
  }

  const runResult = await runWorkflowById(workflowId, {
    source: options.source || 'shortcut',
    tabId: options.tabId,
  });

  return {
    ...runResult,
    shortcut: shortcutName,
    workflowId,
  };
}

async function runActionInternal(request, options = {}) {
  const action = typeof request.action === 'string' ? request.action.trim() : '';
  const args = request.args && typeof request.args === 'object' ? request.args : {};
  const source = options.source || 'panel';

  if (!action) {
    return { ok: false, error: 'action-required' };
  }

  if (action.startsWith('/')) {
    return runShortcutByName(action.slice(1), {
      source,
      tabId: request.tabId,
    });
  }

  let tab = null;
  let tabId = typeof request.tabId === 'number' ? request.tabId : undefined;
  let sourceSession = undefined;

  if (!['tabs:list'].includes(action)) {
    tab = await getOrCreateActionTab(tabId);
    tabId = tab.id;
    const managed = await ensureManagedTab(tabId, getManagedSessionForTab(tabId));
    sourceSession = managed?.session;
  }

  let result;

  switch (action) {
    case 'open': {
      const url = normalizeUrl(args.url || request.url);
      if (!url) {
        result = { ok: false, error: 'url-required' };
        break;
      }
      const updatedTab = await chrome.tabs.update(tabId, { url, active: true });
      await waitForTabSettled(updatedTab.id, clampInt(args.timeoutMs, 1000, 60_000, 12_000)).catch(
        () => {}
      );
      result = {
        ok: true,
        action,
        tabId: updatedTab.id,
        url,
      };
      break;
    }

    case 'back': {
      if (typeof chrome.tabs.goBack === 'function') {
        await chrome.tabs.goBack(tabId);
      } else {
        await sendContentCommand(tabId, {
          type: CONTENT_EXECUTE_ACTION,
          command: 'eval',
          args: { expression: '(() => { history.back(); return true; })()' },
        });
      }
      result = { ok: true, action, tabId };
      break;
    }

    case 'forward': {
      if (typeof chrome.tabs.goForward === 'function') {
        await chrome.tabs.goForward(tabId);
      } else {
        await sendContentCommand(tabId, {
          type: CONTENT_EXECUTE_ACTION,
          command: 'eval',
          args: { expression: '(() => { history.forward(); return true; })()' },
        });
      }
      result = { ok: true, action, tabId };
      break;
    }

    case 'reload': {
      await chrome.tabs.reload(tabId);
      await waitForTabSettled(tabId, clampInt(args.timeoutMs, 1000, 60_000, 10_000)).catch(() => {});
      result = { ok: true, action, tabId };
      break;
    }

    case 'wait': {
      const ms = clampInt(args.ms, 0, 120_000, 1000);
      await delay(ms);
      result = { ok: true, action, waitedMs: ms, tabId };
      break;
    }

    case 'click':
    case 'fill':
    case 'press':
    case 'eval':
    case 'snapshot': {
      // DOM-level commands are delegated to the page content-script.
      const commandArgs =
        action === 'eval'
          ? { expression: args.expression }
          : {
              selector: args.selector,
              value: args.value,
              key: args.key,
              interactiveOnly: args.interactiveOnly === true,
              maxNodes: args.maxNodes,
            };

      const command = action === 'eval' ? 'eval' : action;
      const response = await sendContentCommand(tabId, {
        type: CONTENT_EXECUTE_ACTION,
        command,
        args: commandArgs,
      });

      result = {
        ...(response && typeof response === 'object' ? response : { ok: false, error: 'invalid-response' }),
        action,
        tabId,
      };
      break;
    }

    case 'dom-state': {
      const response = await sendContentCommand(tabId, {
        type: CONTENT_GET_DOM_STATE,
        options: {
          selector: args.selector,
          interactiveOnly: args.interactiveOnly === true,
          maxNodes: args.maxNodes,
        },
      });

      result = {
        ...(response && typeof response === 'object' ? response : { ok: false, error: 'invalid-response' }),
        action,
        tabId,
      };
      break;
    }

    case 'tabs:list': {
      const tabs = await chrome.tabs.query({ currentWindow: true });
      result = {
        ok: true,
        action,
        tabs: tabs.map((entry) => ({
          id: entry.id,
          index: entry.index,
          title: entry.title,
          url: entry.url,
          active: entry.active === true,
          session: getManagedSessionForTab(entry.id),
        })),
      };
      break;
    }

    case 'tabs:new': {
      const url = normalizeUrl(args.url) || 'about:blank';
      const created = await chrome.tabs.create({ url, active: true });
      const managed = await ensureManagedTab(created.id, sourceSession || 'default');
      result = {
        ok: true,
        action,
        tabId: created.id,
        url,
        session: managed?.session || null,
        groupId: managed?.grouping?.groupId ?? null,
      };
      break;
    }

    case 'tabs:switch': {
      if (typeof args.tabId === 'number') {
        const switched = await chrome.tabs.update(args.tabId, { active: true });
        if (typeof switched.windowId === 'number') {
          await chrome.windows.update(switched.windowId, { focused: true }).catch(() => {});
        }
        const managed = await ensureManagedTab(switched.id, sourceSession || 'default');
        result = {
          ok: true,
          action,
          tabId: switched.id,
          session: managed?.session || null,
        };
        break;
      }

      const index = clampInt(args.index, 0, 500, 0);
      const tabs = await chrome.tabs.query({ currentWindow: true });
      const target = tabs.find((item) => item.index === index);
      if (!target || typeof target.id !== 'number') {
        result = { ok: false, error: `tab-index-not-found:${index}` };
        break;
      }

      const switched = await chrome.tabs.update(target.id, { active: true });
      const managed = await ensureManagedTab(switched.id, sourceSession || 'default');
      result = {
        ok: true,
        action,
        tabId: switched.id,
        session: managed?.session || null,
      };
      break;
    }

    case 'tabs:close': {
      await chrome.tabs.remove(tabId);
      result = { ok: true, action, tabId };
      break;
    }

    case 'shortcut:run': {
      result = await runShortcutByName(args.name, {
        source,
        tabId,
      });
      break;
    }

    default:
      result = { ok: false, error: `unknown-action:${action}` };
  }

  pushCommandHistory({
    action,
    args,
    ok: result?.ok === true,
    source,
    error: result?.ok === true ? null : result?.error || 'unknown',
  });

  if (options.record !== false && result?.ok === true) {
    recordWorkflowStep(action, args);
  }

  if (
    (action === 'dom-state' || action === 'snapshot') &&
    result?.ok === true &&
    result?.state &&
    typeof result.state === 'object'
  ) {
    latestDomState = result.state;
  }

  pushActivityEvent('command', {
    action,
    ok: result?.ok === true,
    error: result?.ok === true ? null : result?.error || 'unknown',
  }, {
    source,
    tabId,
    session: typeof tabId === 'number' ? getManagedSessionForTab(tabId) : null,
    url: tab?.url || '',
    title: tab?.title || '',
  });

  return result;
}

async function runWorkflowById(workflowId, options = {}) {
  const workflow = workflows.get(workflowId);
  if (!workflow) {
    return { ok: false, error: `workflow-not-found:${workflowId}` };
  }

  let currentTabId = typeof options.tabId === 'number' ? options.tabId : undefined;
  const results = [];

  for (const step of workflow.steps) {
    // Retry each step with bounded backoff for transient page timing issues.
    let stepResult = null;
    const retries = clampInt(step.retries, 0, 5, 0);

    for (let attempt = 0; attempt <= retries; attempt += 1) {
      stepResult = await runActionInternal(
        {
          action: step.action,
          args: step.args,
          tabId: currentTabId,
        },
        {
          source: options.source || 'workflow',
          record: false,
        }
      );

      if (stepResult?.ok === true) {
        break;
      }

      if (attempt < retries) {
        await delay(300 * (attempt + 1));
      }
    }

    results.push({
      action: step.action,
      ok: stepResult?.ok === true,
      error: stepResult?.ok === true ? null : stepResult?.error || 'unknown',
    });

    if (stepResult?.ok !== true) {
      pushActivityEvent('workflow', {
        workflowId: workflow.id,
        workflowName: workflow.name,
        ok: false,
        failedAction: step.action,
        error: stepResult?.error || 'unknown',
      }, {
        source: options.source || 'workflow',
      });

      return {
        ok: false,
        workflowId: workflow.id,
        workflowName: workflow.name,
        error: `workflow-step-failed:${step.action}`,
        results,
      };
    }

    if (typeof stepResult.tabId === 'number') {
      currentTabId = stepResult.tabId;
    }

    if (step.timeoutMs > 0) {
      await delay(step.timeoutMs);
    }
  }

  pushActivityEvent('workflow', {
    workflowId: workflow.id,
    workflowName: workflow.name,
    ok: true,
    steps: workflow.steps.length,
  }, {
    source: options.source || 'workflow',
  });

  return {
    ok: true,
    workflowId: workflow.id,
    workflowName: workflow.name,
    results,
  };
}

async function startRecording(name) {
  const normalizedName =
    typeof name === 'string' && name.trim().length > 0 ? name.trim().slice(0, 120) : 'Recorded Workflow';

  recordingState = {
    id: uid('recording'),
    name: normalizedName,
    startedAt: now(),
    stoppedAt: null,
    steps: [],
  };

  pushActivityEvent('recording', {
    event: 'start',
    name: normalizedName,
  }, {
    source: 'panel',
  });

  return { ok: true, recording: recordingState };
}

async function stopRecording() {
  if (!recordingState) {
    return { ok: false, error: 'recording-not-active' };
  }

  recordingState.stoppedAt = now();

  pushActivityEvent('recording', {
    event: 'stop',
    steps: recordingState.steps.length,
    name: recordingState.name,
  }, {
    source: 'panel',
  });

  return { ok: true, recording: recordingState };
}

async function saveRecordingAsWorkflow(name) {
  if (!recordingState) {
    return { ok: false, error: 'recording-not-active' };
  }

  if (recordingState.steps.length === 0) {
    return { ok: false, error: 'recording-has-no-steps' };
  }

  const workflowName =
    typeof name === 'string' && name.trim().length > 0
      ? name.trim().slice(0, 120)
      : recordingState.name || 'Recorded Workflow';

  const workflow = {
    id: uid('workflow'),
    name: workflowName,
    steps: recordingState.steps.map((step) => normalizeWorkflowStep(step)).filter(Boolean),
    createdAt: now(),
    updatedAt: now(),
  };

  workflows.set(workflow.id, workflow);
  await persistWorkflows();

  pushActivityEvent('recording', {
    event: 'saved',
    workflowId: workflow.id,
    workflowName: workflow.name,
    steps: workflow.steps.length,
  }, {
    source: 'panel',
  });

  recordingState = null;
  return { ok: true, workflow };
}

async function deleteWorkflow(workflowId) {
  if (!workflows.has(workflowId)) {
    return { ok: false, error: `workflow-not-found:${workflowId}` };
  }

  workflows.delete(workflowId);

  for (const [name, mappedWorkflowId] of [...shortcuts.entries()]) {
    if (mappedWorkflowId === workflowId) {
      shortcuts.delete(name);
    }
  }

  for (const [scheduleId, schedule] of [...schedules.entries()]) {
    if (schedule.workflowId === workflowId) {
      schedules.delete(scheduleId);
      await chrome.alarms.clear(`${WORKFLOW_ALARM_PREFIX}${scheduleId}`);
    }
  }

  await persistWorkflows();
  await persistShortcuts();
  await persistSchedules();

  return { ok: true };
}

async function setShortcut(name, workflowId) {
  const shortcutName = normalizeShortcutName(name);
  if (!shortcutName) {
    return { ok: false, error: 'invalid-shortcut-name' };
  }

  if (!workflows.has(workflowId)) {
    return { ok: false, error: `workflow-not-found:${workflowId}` };
  }

  shortcuts.set(shortcutName, workflowId);
  await persistShortcuts();

  return {
    ok: true,
    shortcut: {
      name: shortcutName,
      workflowId,
      workflowName: workflows.get(workflowId)?.name || workflowId,
    },
  };
}

async function deleteShortcut(name) {
  const shortcutName = normalizeShortcutName(name);
  if (!shortcutName) {
    return { ok: false, error: 'invalid-shortcut-name' };
  }

  shortcuts.delete(shortcutName);
  await persistShortcuts();
  return { ok: true };
}

async function createSchedule(input) {
  if (!input || typeof input !== 'object') {
    return { ok: false, error: 'schedule-input-required' };
  }

  if (typeof input.workflowId !== 'string' || !workflows.has(input.workflowId)) {
    return { ok: false, error: 'invalid-workflow-id' };
  }

  const cadence = normalizeCadence(input.cadence);
  const schedule = {
    id: uid('schedule'),
    name:
      typeof input.name === 'string' && input.name.trim().length > 0
        ? input.name.trim().slice(0, 120)
        : `Schedule ${workflows.get(input.workflowId)?.name || input.workflowId}`,
    workflowId: input.workflowId,
    cadence,
    enabled: input.enabled !== false,
    createdAt: now(),
    updatedAt: now(),
    lastRunAt: null,
    nextRunAt: computeNextRun(cadence),
  };

  schedules.set(schedule.id, schedule);
  await persistSchedules();
  await scheduleWorkflowAlarm(schedule);

  return { ok: true, schedule };
}

async function deleteSchedule(scheduleId) {
  if (!schedules.has(scheduleId)) {
    return { ok: false, error: `schedule-not-found:${scheduleId}` };
  }

  schedules.delete(scheduleId);
  await chrome.alarms.clear(`${WORKFLOW_ALARM_PREFIX}${scheduleId}`);
  await persistSchedules();
  return { ok: true };
}

async function toggleSchedule(scheduleId, enabled) {
  const schedule = schedules.get(scheduleId);
  if (!schedule) {
    return { ok: false, error: `schedule-not-found:${scheduleId}` };
  }

  schedule.enabled = enabled !== false;
  schedule.updatedAt = now();
  if (schedule.enabled && (!schedule.nextRunAt || schedule.nextRunAt <= now())) {
    schedule.nextRunAt = computeNextRun(schedule.cadence);
  }

  schedules.set(schedule.id, schedule);
  await persistSchedules();
  await scheduleWorkflowAlarm(schedule);

  return { ok: true, schedule };
}

async function runSchedule(scheduleId) {
  const schedule = schedules.get(scheduleId);
  if (!schedule) {
    return;
  }

  if (!schedule.enabled) {
    await scheduleWorkflowAlarm(schedule);
    return;
  }

  const runResult = await runWorkflowById(schedule.workflowId, {
    source: `schedule:${schedule.id}`,
  });

  schedule.lastRunAt = now();
  schedule.updatedAt = now();
  schedule.nextRunAt = computeNextRun(schedule.cadence, schedule.lastRunAt);
  schedules.set(schedule.id, schedule);

  await persistSchedules();
  await scheduleWorkflowAlarm(schedule);

  pushActivityEvent('schedule', {
    scheduleId: schedule.id,
    scheduleName: schedule.name,
    workflowId: schedule.workflowId,
    ok: runResult.ok === true,
    error: runResult.ok === true ? null : runResult.error || 'unknown',
  }, {
    source: 'alarm',
  });
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

  if (type === CONTENT_EVENT_TYPE) {
    const tabId = sender.tab?.id;
    const session = typeof tabId === 'number' ? getManagedSessionForTab(tabId) : null;
    const kind = typeof message.kind === 'string' ? message.kind : 'unknown';

    pushActivityEvent(kind, message.payload || {}, {
      source: 'content-script',
      tabId,
      session,
      url: sender.tab?.url || message.url || '',
      title: sender.tab?.title || message.title || '',
    });

    sendResponse({ ok: true });
    return;
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

  if (type === PANEL_RUN_ACTION) {
    bootstrapPromise
      .then(() =>
        runActionInternal(
          {
            action: message.action,
            args: message.args || {},
            tabId: message.tabId,
          },
          {
            source: 'panel',
            record: true,
          }
        )
      )
      .then((result) => sendResponse(result))
      .catch((error) => {
        sendResponse({
          ok: false,
          error: error instanceof Error ? error.message : String(error),
        });
      });
    return true;
  }

  if (type === PANEL_CLEAR_ACTIVITY) {
    activityEvents.length = 0;
    sendResponse({ ok: true });
    return;
  }

  if (type === PANEL_START_RECORDING) {
    startRecording(message.name)
      .then((result) => sendResponse(result))
      .catch((error) => {
        sendResponse({ ok: false, error: error instanceof Error ? error.message : String(error) });
      });
    return true;
  }

  if (type === PANEL_STOP_RECORDING) {
    stopRecording()
      .then((result) => sendResponse(result))
      .catch((error) => {
        sendResponse({ ok: false, error: error instanceof Error ? error.message : String(error) });
      });
    return true;
  }

  if (type === PANEL_SAVE_RECORDING) {
    saveRecordingAsWorkflow(message.name)
      .then((result) => sendResponse(result))
      .catch((error) => {
        sendResponse({ ok: false, error: error instanceof Error ? error.message : String(error) });
      });
    return true;
  }

  if (type === PANEL_RUN_WORKFLOW) {
    runWorkflowById(message.workflowId, {
      source: 'panel-workflow',
      tabId: message.tabId,
    })
      .then((result) => sendResponse(result))
      .catch((error) => {
        sendResponse({ ok: false, error: error instanceof Error ? error.message : String(error) });
      });
    return true;
  }

  if (type === PANEL_DELETE_WORKFLOW) {
    deleteWorkflow(message.workflowId)
      .then((result) => sendResponse(result))
      .catch((error) => {
        sendResponse({ ok: false, error: error instanceof Error ? error.message : String(error) });
      });
    return true;
  }

  if (type === PANEL_SET_SHORTCUT) {
    setShortcut(message.name, message.workflowId)
      .then((result) => sendResponse(result))
      .catch((error) => {
        sendResponse({ ok: false, error: error instanceof Error ? error.message : String(error) });
      });
    return true;
  }

  if (type === PANEL_DELETE_SHORTCUT) {
    deleteShortcut(message.name)
      .then((result) => sendResponse(result))
      .catch((error) => {
        sendResponse({ ok: false, error: error instanceof Error ? error.message : String(error) });
      });
    return true;
  }

  if (type === PANEL_RUN_SHORTCUT) {
    runShortcutByName(message.name, {
      source: 'panel-shortcut',
      tabId: message.tabId,
    })
      .then((result) => sendResponse(result))
      .catch((error) => {
        sendResponse({ ok: false, error: error instanceof Error ? error.message : String(error) });
      });
    return true;
  }

  if (type === PANEL_CREATE_SCHEDULE) {
    createSchedule(message.schedule)
      .then((result) => sendResponse(result))
      .catch((error) => {
        sendResponse({ ok: false, error: error instanceof Error ? error.message : String(error) });
      });
    return true;
  }

  if (type === PANEL_DELETE_SCHEDULE) {
    deleteSchedule(message.scheduleId)
      .then((result) => sendResponse(result))
      .catch((error) => {
        sendResponse({ ok: false, error: error instanceof Error ? error.message : String(error) });
      });
    return true;
  }

  if (type === PANEL_TOGGLE_SCHEDULE) {
    toggleSchedule(message.scheduleId, message.enabled)
      .then((result) => sendResponse(result))
      .catch((error) => {
        sendResponse({ ok: false, error: error instanceof Error ? error.message : String(error) });
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

  if (type === CONTENT_PING) {
    sendResponse({ ok: true, extensionId: chrome.runtime.id });
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
    sessionGroupTitleMap.delete(session);
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
  if (!alarm || typeof alarm.name !== 'string') return;

  if (alarm.name === CLEANUP_ALARM_NAME) {
    cleanEmptyGroups().catch(() => {});
    return;
  }

  if (alarm.name.startsWith(WORKFLOW_ALARM_PREFIX)) {
    const scheduleId = alarm.name.slice(WORKFLOW_ALARM_PREFIX.length);
    runSchedule(scheduleId).catch(() => {});
  }
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
