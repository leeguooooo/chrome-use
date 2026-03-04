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
  summaryEl.innerHTML = '<h3>Overview</h3>';
  const options = state.options || {};

  const idInfo = document.createElement('div');
  idInfo.style.marginBottom = '12px';
  idInfo.style.fontSize = '11px';
  idInfo.style.color = 'var(--text-muted)';
  idInfo.innerHTML = `Extension ID: <code>${state.extensionId}</code>`;
  summaryEl.appendChild(idInfo);

  const tags = document.createElement('div');
  tags.className = 'tags';
  tags.appendChild(createTag(`Sessions: ${state.totals.sessions}`));
  tags.appendChild(createTag(`Tabs: ${state.totals.tabs}`));
  tags.appendChild(
    createTag(`Isolation: ${options.strictWindowIsolation === false ? 'Off' : 'On'}`)
  );
  tags.appendChild(
    createTag(`Guard: ${options.suppressCrossWindowActivation === false ? 'Off' : 'On'}`)
  );
  tags.appendChild(
    createTag(`Auto-Clean: ${options.autoCleanEmptyGroups === false ? 'Off' : 'On'}`)
  );

  const optionActions = document.createElement('div');
  optionActions.className = 'row-actions';

  const createOptionBtn = (text, active, onClick) => {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.textContent = text;
    if (active) btn.style.borderColor = 'var(--accent)';
    btn.addEventListener('click', onClick);
    return btn;
  };

  optionActions.appendChild(
    createOptionBtn('Strict Isolation', options.strictWindowIsolation !== false, async () => {
      await send({
        type: 'AB_PANEL_SET_OPTIONS',
        options: { ...options, strictWindowIsolation: options.strictWindowIsolation === false },
      });
      await refresh();
    })
  );

  optionActions.appendChild(
    createOptionBtn('Activation Guard', options.suppressCrossWindowActivation !== false, async () => {
      await send({
        type: 'AB_PANEL_SET_OPTIONS',
        options: {
          ...options,
          suppressCrossWindowActivation: options.suppressCrossWindowActivation === false,
        },
      });
      await refresh();
    })
  );

  optionActions.appendChild(
    createOptionBtn('Auto-Clean', options.autoCleanEmptyGroups !== false, async () => {
      await send({
        type: 'AB_PANEL_SET_OPTIONS',
        options: {
          ...options,
          autoCleanEmptyGroups: options.autoCleanEmptyGroups === false,
        },
      });
      await refresh();
    })
  );

  summaryEl.appendChild(tags);
  summaryEl.appendChild(optionActions);
}

function renderSessions(state) {
  sessionsEl.innerHTML = '<h3>Active Sessions</h3>';

  if (!state.sessions || state.sessions.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'card empty';
    empty.textContent = 'No active sessions monitored.';
    sessionsEl.appendChild(empty);
    return;
  }

  for (const session of state.sessions) {
    const card = document.createElement('article');
    card.className = 'card';

    const titleRow = document.createElement('div');
    titleRow.className = 'session-title';

    const titleLeft = document.createElement('h4');
    titleLeft.textContent = session.session;

    const focusBtn = document.createElement('button');
    focusBtn.textContent = 'Focus';
    focusBtn.addEventListener('click', async () => {
      await send({ type: 'AB_PANEL_FOCUS_SESSION', session: session.session });
      await refresh();
    });

    titleRow.appendChild(titleLeft);
    titleRow.appendChild(focusBtn);

    const tags = document.createElement('div');
    tags.className = 'tags';
    tags.appendChild(createTag(`Window: ${session.windowId ?? 'N/A'}`));
    tags.appendChild(createTag(`Tabs: ${session.tabs.length}`));

    if (session.group) {
      tags.appendChild(createTag(`G: ${session.group.title || 'Untitled'}`));
    }

    if (session.allowedDomains && session.allowedDomains.length > 0) {
      tags.appendChild(createTag(`Allowlist: ${session.allowedDomains.length} domains`));
    }

    const list = document.createElement('div');
    list.className = 'list';
    for (const tab of session.tabs.slice(0, 10)) {
      const item = document.createElement('div');
      item.className = 'item';

      const t = document.createElement('div');
      t.className = 'item-title';
      if (tab.active) {
        const dot = document.createElement('span');
        dot.textContent = '●';
        dot.style.color = 'var(--success)';
        dot.style.marginRight = '6px';
        dot.style.fontSize = '10px';
        t.appendChild(dot);
      }
      t.appendChild(document.createTextNode(tab.title || '(Untitled)'));

      const u = document.createElement('div');
      u.className = 'item-url';
      u.textContent = tab.url || 'about:blank';

      item.appendChild(t);
      item.appendChild(u);
      list.appendChild(item);
    }

    const footerActions = document.createElement('div');
    footerActions.className = 'row-actions';

    const keepBtn = document.createElement('button');
    keepBtn.textContent = 'Isolate Session';
    keepBtn.addEventListener('click', async () => {
      await send({ type: 'AB_PANEL_CLOSE_OTHER_SESSION_TABS', session: session.session });
      await refresh();
    });

    const policyBtn = document.createElement('button');
    policyBtn.textContent = 'Config Policy';
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

    footerActions.appendChild(keepBtn);
    footerActions.appendChild(policyBtn);

    card.appendChild(titleRow);
    card.appendChild(tags);
    card.appendChild(list);
    card.appendChild(footerActions);
    sessionsEl.appendChild(card);
  }
}

function renderDownloads(state) {
  downloadsEl.innerHTML = '<h3>Recent Downloads</h3>';

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
