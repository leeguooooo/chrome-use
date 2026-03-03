const REQUEST_TYPE = 'AB_TAB_GROUP_REQUEST';
const DEFAULT_GROUP_TITLE = 'Agent Browser Stealth';
const sessionGroupCache = new Map();

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

function cacheKey(windowId, session) {
  return `${windowId}:${session}`;
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
      // Ignore stale group references and continue.
    }
  }

  return null;
}

async function ensureSessionGroup(tabId, windowId, session, groupTitle) {
  const key = cacheKey(windowId, session);
  let groupId = sessionGroupCache.get(key);

  if (typeof groupId === 'number') {
    try {
      await chrome.tabGroups.get(groupId);
    } catch {
      groupId = undefined;
    }
  }

  if (typeof groupId !== 'number') {
    const existing = await findExistingGroup(windowId, groupTitle);
    if (typeof existing === 'number') {
      groupId = existing;
    }
  }

  if (typeof groupId === 'number') {
    await chrome.tabs.group({ groupId, tabIds: [tabId] });
  } else {
    groupId = await chrome.tabs.group({
      tabIds: [tabId],
      createProperties: { windowId },
    });
  }

  await chrome.tabGroups.update(groupId, {
    title: groupTitle,
    color: 'blue',
    collapsed: false,
  });

  sessionGroupCache.set(key, groupId);
  return groupId;
}

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  if (!message || message.type !== REQUEST_TYPE) {
    return;
  }

  const tabId = sender.tab?.id;
  const windowId = sender.tab?.windowId;
  const nonce = typeof message.nonce === 'string' ? message.nonce : undefined;

  if (typeof tabId !== 'number' || typeof windowId !== 'number') {
    sendResponse({
      ok: false,
      error: 'missing-tab-context',
      extensionId: chrome.runtime.id,
      nonce,
    });
    return;
  }

  if (typeof message.pluginId === 'string' && message.pluginId !== chrome.runtime.id) {
    sendResponse({
      ok: false,
      error: 'plugin-id-mismatch',
      extensionId: chrome.runtime.id,
      nonce,
    });
    return;
  }

  const session = normalizeSession(message.session);
  const groupTitle = normalizeGroupTitle(message.groupTitle);

  ensureSessionGroup(tabId, windowId, session, groupTitle)
    .then((groupId) => {
      sendResponse({
        ok: true,
        groupId,
        extensionId: chrome.runtime.id,
        nonce,
      });
    })
    .catch((error) => {
      const errorMessage = error instanceof Error ? error.message : String(error);
      sendResponse({
        ok: false,
        error: errorMessage,
        extensionId: chrome.runtime.id,
        nonce,
      });
    });

  return true;
});
