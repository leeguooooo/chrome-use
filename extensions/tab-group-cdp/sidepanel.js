const summaryEl = document.getElementById('summary');
const sessionsEl = document.getElementById('sessions');
const downloadsEl = document.getElementById('downloads');
const refreshBtn = document.getElementById('refresh-btn');
const cleanupBtn = document.getElementById('cleanup-btn');

async function send(message) {
  return chrome.runtime.sendMessage(message);
}

function createTag(text) {
  const span = document.createElement('span');
  span.className = 'tag';
  span.textContent = text;
  return span;
}

function renderSummary(state) {
  summaryEl.innerHTML = '';
  const title = document.createElement('div');
  title.innerHTML = `<strong>Overview</strong> · extensionId: <code>${state.extensionId}</code>`;

  const tags = document.createElement('div');
  tags.className = 'tags';
  tags.appendChild(createTag(`sessions: ${state.totals.sessions}`));
  tags.appendChild(createTag(`tabs: ${state.totals.tabs}`));

  summaryEl.appendChild(title);
  summaryEl.appendChild(tags);
}

function renderSessions(state) {
  sessionsEl.innerHTML = '';

  if (!state.sessions || state.sessions.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'card empty';
    empty.textContent = 'No managed sessions yet.';
    sessionsEl.appendChild(empty);
    return;
  }

  for (const session of state.sessions) {
    const card = document.createElement('article');
    card.className = 'card';

    const titleRow = document.createElement('div');
    titleRow.className = 'session-title';

    const titleLeft = document.createElement('strong');
    titleLeft.textContent = session.session;

    const actions = document.createElement('div');
    actions.className = 'row-actions';

    const focusBtn = document.createElement('button');
    focusBtn.type = 'button';
    focusBtn.textContent = 'Focus';
    focusBtn.addEventListener('click', async () => {
      await send({ type: 'AB_PANEL_FOCUS_SESSION', session: session.session });
      await refresh();
    });

    const keepBtn = document.createElement('button');
    keepBtn.type = 'button';
    keepBtn.textContent = 'Keep Only This';
    keepBtn.addEventListener('click', async () => {
      await send({ type: 'AB_PANEL_CLOSE_OTHER_SESSION_TABS', session: session.session });
      await refresh();
    });

    const policyBtn = document.createElement('button');
    policyBtn.type = 'button';
    policyBtn.textContent = 'Set Allowlist';
    policyBtn.addEventListener('click', async () => {
      const current = (session.allowedDomains || []).join(',');
      const input = window.prompt('Allowed domains (comma-separated)', current);
      if (input === null) return;
      const allowedDomains = input
        .split(',')
        .map((item) => item.trim().toLowerCase())
        .filter((item) => item.length > 0);
      await send({ type: 'AB_PANEL_SET_POLICY', session: session.session, allowedDomains });
      await refresh();
    });

    actions.appendChild(focusBtn);
    actions.appendChild(keepBtn);
    actions.appendChild(policyBtn);

    titleRow.appendChild(titleLeft);
    titleRow.appendChild(actions);

    const tags = document.createElement('div');
    tags.className = 'tags';
    tags.appendChild(createTag(`window: ${session.windowId ?? 'n/a'}`));
    tags.appendChild(createTag(`tabs: ${session.tabs.length}`));

    if (session.group) {
      tags.appendChild(createTag(`group: ${session.group.title || session.group.id}`));
      tags.appendChild(createTag(`color: ${session.group.color}`));
      tags.appendChild(createTag(`collapsed: ${session.group.collapsed}`));
    }

    if (session.allowedDomains && session.allowedDomains.length > 0) {
      tags.appendChild(createTag(`allowlist: ${session.allowedDomains.join(', ')}`));
    }

    if (session.riskHints && session.riskHints.length > 0) {
      for (const hint of session.riskHints) {
        tags.appendChild(createTag(`risk: ${hint}`));
      }
    }

    const list = document.createElement('div');
    list.className = 'list';
    for (const tab of session.tabs.slice(0, 10)) {
      const item = document.createElement('div');
      item.className = 'item';

      const t = document.createElement('div');
      t.className = 'item-title';
      t.textContent = `${tab.active ? '● ' : ''}${tab.title || '(untitled)'}`;

      const u = document.createElement('div');
      u.className = 'item-url';
      u.textContent = tab.url || 'about:blank';

      item.appendChild(t);
      item.appendChild(u);
      list.appendChild(item);
    }

    card.appendChild(titleRow);
    card.appendChild(tags);
    card.appendChild(list);
    sessionsEl.appendChild(card);
  }
}

function renderDownloads(state) {
  downloadsEl.innerHTML = '<strong>Recent Downloads</strong>';

  const list = document.createElement('div');
  list.className = 'list';

  const entries = state.downloads || [];
  if (entries.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'empty';
    empty.textContent = 'No download events yet.';
    list.appendChild(empty);
  } else {
    for (const entry of entries.slice(0, 8)) {
      const item = document.createElement('div');
      item.className = 'item';
      item.innerHTML = `<div class="item-title">#${entry.id} · ${entry.state || 'updated'}</div><div class="item-url">${entry.filename || ''}</div>`;
      list.appendChild(item);
    }
  }

  downloadsEl.appendChild(list);
}

async function refresh() {
  const response = await send({ type: 'AB_PANEL_GET_STATE' });
  if (!response || response.ok !== true || !response.state) {
    summaryEl.textContent = response?.error || 'Failed to load extension state.';
    sessionsEl.innerHTML = '';
    downloadsEl.innerHTML = '';
    return;
  }

  renderSummary(response.state);
  renderSessions(response.state);
  renderDownloads(response.state);
}

refreshBtn.addEventListener('click', refresh);
cleanupBtn.addEventListener('click', async () => {
  await send({ type: 'AB_PANEL_CLEAN_EMPTY_GROUPS' });
  await refresh();
});

refresh();
setInterval(refresh, 5000);
