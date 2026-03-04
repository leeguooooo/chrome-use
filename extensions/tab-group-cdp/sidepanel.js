const PANEL_GET_STATE = 'AB_PANEL_GET_STATE';
const PANEL_CLEAN_EMPTY_GROUPS = 'AB_PANEL_CLEAN_EMPTY_GROUPS';
const PANEL_SET_OPTIONS = 'AB_PANEL_SET_OPTIONS';
const PANEL_SET_POLICY = 'AB_PANEL_SET_POLICY';
const PANEL_CLOSE_OTHER_TABS = 'AB_PANEL_CLOSE_OTHER_SESSION_TABS';
const PANEL_FOCUS_SESSION = 'AB_PANEL_FOCUS_SESSION';
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

const summaryEl = document.getElementById('summary');
const controlEl = document.getElementById('control');
const automationEl = document.getElementById('automation');
const developerEl = document.getElementById('developer');
const sessionsEl = document.getElementById('sessions');
const downloadsEl = document.getElementById('downloads');
const statusLineEl = document.getElementById('status-line');
const refreshBtn = document.getElementById('refresh-btn');
const cleanupBtn = document.getElementById('cleanup-btn');

const viewState = {
  panelState: null,
  lastDomState: null,
};

async function send(message) {
  return chrome.runtime.sendMessage(message);
}

function normalizePanelState(rawState) {
  const state = rawState && typeof rawState === 'object' ? rawState : {};

  return {
    extensionId: typeof state.extensionId === 'string' ? state.extensionId : 'unknown',
    latestDomState:
      state.latestDomState && typeof state.latestDomState === 'object' ? state.latestDomState : null,
    options:
      state.options && typeof state.options === 'object'
        ? {
            strictWindowIsolation: state.options.strictWindowIsolation !== false,
            suppressCrossWindowActivation: state.options.suppressCrossWindowActivation !== false,
            autoCleanEmptyGroups: state.options.autoCleanEmptyGroups !== false,
            pageBridgeEnabled: state.options.pageBridgeEnabled === true,
          }
        : {
            strictWindowIsolation: true,
            suppressCrossWindowActivation: true,
            autoCleanEmptyGroups: true,
            pageBridgeEnabled: false,
          },
    totals:
      state.totals && typeof state.totals === 'object'
        ? {
            sessions: Number.isFinite(state.totals.sessions) ? state.totals.sessions : 0,
            tabs: Number.isFinite(state.totals.tabs) ? state.totals.tabs : 0,
          }
        : {
            sessions: 0,
            tabs: 0,
          },
    sessions: Array.isArray(state.sessions) ? state.sessions : [],
    downloads: Array.isArray(state.downloads) ? state.downloads : [],
    control:
      state.control && typeof state.control === 'object'
        ? {
            activeTab:
              state.control.activeTab && typeof state.control.activeTab === 'object'
                ? state.control.activeTab
                : null,
            tabs: Array.isArray(state.control.tabs) ? state.control.tabs : [],
          }
        : {
            activeTab: null,
            tabs: [],
          },
    activity:
      state.activity && typeof state.activity === 'object'
        ? {
            events: Array.isArray(state.activity.events) ? state.activity.events : [],
            console: Array.isArray(state.activity.console) ? state.activity.console : [],
            network: Array.isArray(state.activity.network) ? state.activity.network : [],
            commandHistory: Array.isArray(state.activity.commandHistory)
              ? state.activity.commandHistory
              : [],
          }
        : {
            events: [],
            console: [],
            network: [],
            commandHistory: [],
          },
    automation:
      state.automation && typeof state.automation === 'object'
        ? {
            recording:
              state.automation.recording && typeof state.automation.recording === 'object'
                ? state.automation.recording
                : null,
            workflows: Array.isArray(state.automation.workflows) ? state.automation.workflows : [],
            shortcuts: Array.isArray(state.automation.shortcuts) ? state.automation.shortcuts : [],
            schedules: Array.isArray(state.automation.schedules) ? state.automation.schedules : [],
          }
        : {
            recording: null,
            workflows: [],
            shortcuts: [],
            schedules: [],
          },
  };
}

function setStatus(text, tone = 'ok') {
  statusLineEl.textContent = text || '';
  statusLineEl.className = `status-line ${tone}`;
}

function escapeInline(value) {
  return String(value || '')
    .replace(/[\n\r\t]+/g, ' ')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/\"/g, '&quot;')
    .replace(/'/g, '&#39;')
    .slice(0, 240);
}

function formatTime(ts) {
  if (!ts) return '-';
  try {
    return new Date(ts).toLocaleString();
  } catch {
    return String(ts);
  }
}

function createTag(text) {
  const span = document.createElement('span');
  span.className = 'tag';
  span.textContent = text;
  return span;
}

function parseTimeInput(timeText) {
  const match = String(timeText || '').trim().match(/^(\d{1,2}):(\d{2})$/);
  if (!match) return null;
  const hour = Number.parseInt(match[1], 10);
  const minute = Number.parseInt(match[2], 10);
  if (Number.isNaN(hour) || Number.isNaN(minute)) return null;
  if (hour < 0 || hour > 23 || minute < 0 || minute > 59) return null;
  return { hour, minute };
}

async function runAction(action, args = {}, tabId) {
  const response = await send({
    type: PANEL_RUN_ACTION,
    action,
    args,
    tabId,
  });

  if (!response || response.ok !== true) {
    setStatus(`Action failed: ${response?.error || 'unknown error'}`, 'error');
    return response;
  }

  if (response.state) {
    viewState.lastDomState = response.state;
  }

  setStatus(`Action succeeded: ${action}`, 'ok');
  return response;
}

function renderSummary(state) {
  summaryEl.innerHTML = '';

  const title = document.createElement('h3');
  title.textContent = 'Overview';
  summaryEl.appendChild(title);

  const extensionInfo = document.createElement('div');
  extensionInfo.className = 'caption';
  extensionInfo.textContent = `Extension ID: ${state.extensionId}`;
  summaryEl.appendChild(extensionInfo);

  const tags = document.createElement('div');
  tags.className = 'tags';
  tags.appendChild(createTag(`Sessions: ${state.totals.sessions}`));
  tags.appendChild(createTag(`Tabs: ${state.totals.tabs}`));
  tags.appendChild(
    createTag(`Isolation: ${state.options.strictWindowIsolation === false ? 'Off' : 'On'}`)
  );
  tags.appendChild(
    createTag(`Guard: ${state.options.suppressCrossWindowActivation === false ? 'Off' : 'On'}`)
  );
  tags.appendChild(createTag(`PageBridge: ${state.options.pageBridgeEnabled ? 'On' : 'Off'}`));
  tags.appendChild(createTag(`Workflows: ${state.automation.workflows.length}`));
  tags.appendChild(createTag(`Schedules: ${state.automation.schedules.length}`));
  summaryEl.appendChild(tags);

  const actions = document.createElement('div');
  actions.className = 'row wrap';

  const isolationBtn = document.createElement('button');
  isolationBtn.textContent = 'Toggle Isolation';
  isolationBtn.addEventListener('click', async () => {
    await send({
      type: PANEL_SET_OPTIONS,
      options: {
        ...state.options,
        strictWindowIsolation: state.options.strictWindowIsolation === false,
      },
    });
    await refresh();
  });

  const guardBtn = document.createElement('button');
  guardBtn.textContent = 'Toggle Guard';
  guardBtn.addEventListener('click', async () => {
    await send({
      type: PANEL_SET_OPTIONS,
      options: {
        ...state.options,
        suppressCrossWindowActivation: state.options.suppressCrossWindowActivation === false,
      },
    });
    await refresh();
  });

  const cleanBtn = document.createElement('button');
  cleanBtn.textContent = 'Toggle Auto-Clean';
  cleanBtn.addEventListener('click', async () => {
    await send({
      type: PANEL_SET_OPTIONS,
      options: {
        ...state.options,
        autoCleanEmptyGroups: state.options.autoCleanEmptyGroups === false,
      },
    });
    await refresh();
  });

  actions.appendChild(isolationBtn);
  actions.appendChild(guardBtn);
  actions.appendChild(cleanBtn);
  summaryEl.appendChild(actions);
}

function renderControl(state) {
  const control = state.control || { activeTab: null, tabs: [] };

  controlEl.innerHTML = `
    <h3>Browser Control</h3>
    <div class="row">
      <input id="ctl-url" placeholder="Open URL (example.com)" class="mono" />
      <button id="ctl-open" class="primary">Open</button>
    </div>
    <div class="row wrap">
      <button id="ctl-back">Back</button>
      <button id="ctl-forward">Forward</button>
      <button id="ctl-reload">Reload</button>
      <button id="ctl-snapshot">Snapshot</button>
      <button id="ctl-dom">DOM State</button>
    </div>
    <div class="row">
      <input id="ctl-selector" placeholder="CSS selector" class="mono" />
      <input id="ctl-value" placeholder="Value / text" />
    </div>
    <div class="row wrap">
      <button id="ctl-click">Click</button>
      <button id="ctl-fill">Fill</button>
      <input id="ctl-key" placeholder="Key (Enter)" style="max-width:120px" />
      <button id="ctl-press">Press</button>
    </div>
    <div class="row">
      <input id="ctl-shortcut" placeholder="Shortcut name (/login or login)" class="mono" />
      <button id="ctl-run-shortcut">Run Shortcut</button>
    </div>
    <hr />
    <div class="section-title">
      <h4>Tabs</h4>
      <span class="caption" id="ctl-active"></span>
    </div>
    <div id="ctl-tabs" class="list"></div>
  `;

  const activeTab = control.activeTab;
  const activeText = activeTab
    ? `Active: #${activeTab.id} ${escapeInline(activeTab.title)}`
    : 'No active tab';
  controlEl.querySelector('#ctl-active').textContent = activeText;

  const urlInput = controlEl.querySelector('#ctl-url');
  const selectorInput = controlEl.querySelector('#ctl-selector');
  const valueInput = controlEl.querySelector('#ctl-value');
  const keyInput = controlEl.querySelector('#ctl-key');
  const shortcutInput = controlEl.querySelector('#ctl-shortcut');

  controlEl.querySelector('#ctl-open').addEventListener('click', async () => {
    const url = urlInput.value.trim();
    if (!url) {
      setStatus('Please enter a URL', 'warn');
      return;
    }
    await runAction('open', { url }, activeTab?.id);
    await refresh();
  });

  controlEl.querySelector('#ctl-back').addEventListener('click', async () => {
    await runAction('back', {}, activeTab?.id);
    await refresh();
  });

  controlEl.querySelector('#ctl-forward').addEventListener('click', async () => {
    await runAction('forward', {}, activeTab?.id);
    await refresh();
  });

  controlEl.querySelector('#ctl-reload').addEventListener('click', async () => {
    await runAction('reload', {}, activeTab?.id);
    await refresh();
  });

  controlEl.querySelector('#ctl-snapshot').addEventListener('click', async () => {
    const selector = selectorInput.value.trim();
    const response = await runAction(
      'snapshot',
      {
        selector: selector || undefined,
        interactiveOnly: true,
        maxNodes: 80,
      },
      activeTab?.id
    );
    if (response?.state) {
      viewState.lastDomState = response.state;
      renderDeveloper(state);
    }
  });

  controlEl.querySelector('#ctl-dom').addEventListener('click', async () => {
    const selector = selectorInput.value.trim();
    const response = await runAction(
      'dom-state',
      {
        selector: selector || undefined,
        interactiveOnly: false,
        maxNodes: 100,
      },
      activeTab?.id
    );
    if (response?.state) {
      viewState.lastDomState = response.state;
      renderDeveloper(state);
    }
  });

  controlEl.querySelector('#ctl-click').addEventListener('click', async () => {
    const selector = selectorInput.value.trim();
    if (!selector) {
      setStatus('Selector is required for click', 'warn');
      return;
    }
    await runAction('click', { selector }, activeTab?.id);
    await refresh();
  });

  controlEl.querySelector('#ctl-fill').addEventListener('click', async () => {
    const selector = selectorInput.value.trim();
    if (!selector) {
      setStatus('Selector is required for fill', 'warn');
      return;
    }
    await runAction(
      'fill',
      {
        selector,
        value: valueInput.value,
      },
      activeTab?.id
    );
    await refresh();
  });

  controlEl.querySelector('#ctl-press').addEventListener('click', async () => {
    await runAction(
      'press',
      {
        selector: selectorInput.value.trim() || undefined,
        key: keyInput.value.trim() || 'Enter',
      },
      activeTab?.id
    );
    await refresh();
  });

  controlEl.querySelector('#ctl-run-shortcut').addEventListener('click', async () => {
    const raw = shortcutInput.value.trim();
    if (!raw) {
      setStatus('Shortcut name is required', 'warn');
      return;
    }

    const name = raw.startsWith('/') ? raw.slice(1) : raw;
    const response = await send({
      type: PANEL_RUN_SHORTCUT,
      name,
      tabId: activeTab?.id,
    });

    if (!response || response.ok !== true) {
      setStatus(`Shortcut failed: ${response?.error || 'unknown error'}`, 'error');
      return;
    }

    setStatus(`Shortcut executed: /${name}`, 'ok');
    await refresh();
  });

  const tabsEl = controlEl.querySelector('#ctl-tabs');
  if (!Array.isArray(control.tabs) || control.tabs.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'empty';
    empty.textContent = 'No tabs in current window.';
    tabsEl.appendChild(empty);
  } else {
    for (const tab of control.tabs.slice(0, 20)) {
      const item = document.createElement('div');
      item.className = 'item';

      const title = document.createElement('div');
      title.className = 'item-title';
      title.textContent = `#${tab.id} [${tab.index}] ${escapeInline(tab.title)}`;

      const url = document.createElement('div');
      url.className = 'item-url';
      url.textContent = tab.url || 'about:blank';

      const row = document.createElement('div');
      row.className = 'row wrap';

      const switchBtn = document.createElement('button');
      switchBtn.textContent = tab.active ? 'Active' : 'Switch';
      switchBtn.disabled = tab.active === true;
      switchBtn.addEventListener('click', async () => {
        await runAction('tabs:switch', { tabId: tab.id });
        await refresh();
      });

      const closeBtn = document.createElement('button');
      closeBtn.className = 'danger';
      closeBtn.textContent = 'Close';
      closeBtn.addEventListener('click', async () => {
        await runAction('tabs:close', {}, tab.id);
        await refresh();
      });

      row.appendChild(switchBtn);
      row.appendChild(closeBtn);

      if (tab.session) {
        const pill = document.createElement('span');
        pill.className = 'event-pill';
        pill.textContent = `session:${tab.session}`;
        row.appendChild(pill);
      }

      item.appendChild(title);
      item.appendChild(url);
      item.appendChild(row);
      tabsEl.appendChild(item);
    }
  }
}

function renderSessions(state) {
  sessionsEl.innerHTML = '';

  const heading = document.createElement('h3');
  heading.textContent = 'Managed Sessions';
  sessionsEl.appendChild(heading);

  if (!state.sessions || state.sessions.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'card empty';
    empty.textContent = 'No active managed sessions found.';
    sessionsEl.appendChild(empty);
    return;
  }

  for (const session of state.sessions) {
    const card = document.createElement('article');
    card.className = 'card';

    const titleRow = document.createElement('div');
    titleRow.className = 'section-title';

    const title = document.createElement('h4');
    title.textContent = session.session;

    const actionRow = document.createElement('div');
    actionRow.className = 'row wrap';

    const focusBtn = document.createElement('button');
    focusBtn.textContent = 'Focus';
    focusBtn.addEventListener('click', async () => {
      await send({ type: PANEL_FOCUS_SESSION, session: session.session });
      await refresh();
    });

    const isolateBtn = document.createElement('button');
    isolateBtn.textContent = 'Isolate';
    isolateBtn.addEventListener('click', async () => {
      await send({ type: PANEL_CLOSE_OTHER_TABS, session: session.session });
      await refresh();
    });

    const policyBtn = document.createElement('button');
    policyBtn.textContent = 'Policy';
    policyBtn.addEventListener('click', async () => {
      const current = (session.allowedDomains || []).join(',');
      const input = window.prompt('Allowed domains (comma-separated)', current);
      if (input === null) return;
      const allowedDomains = input
        .split(',')
        .map((item) => item.trim().toLowerCase())
        .filter((item) => item.length > 0);
      await send({ type: PANEL_SET_POLICY, session: session.session, allowedDomains });
      await refresh();
    });

    actionRow.appendChild(focusBtn);
    actionRow.appendChild(isolateBtn);
    actionRow.appendChild(policyBtn);

    titleRow.appendChild(title);
    titleRow.appendChild(actionRow);

    const tags = document.createElement('div');
    tags.className = 'tags';
    tags.appendChild(createTag(`Window ${session.windowId ?? 'N/A'}`));
    tags.appendChild(createTag(`${session.tabs.length} tabs`));

    if (session.group?.title) {
      tags.appendChild(createTag(`Group: ${session.group.title}`));
    }

    const list = document.createElement('div');
    list.className = 'list';

    for (const tab of session.tabs.slice(0, 8)) {
      const item = document.createElement('div');
      item.className = 'item';

      const tabTitle = document.createElement('div');
      tabTitle.className = 'item-title';
      tabTitle.textContent = `${tab.active ? '● ' : ''}#${tab.id} ${escapeInline(tab.title || '(Untitled)')}`;

      const tabUrl = document.createElement('div');
      tabUrl.className = 'item-url';
      tabUrl.textContent = tab.url || 'about:blank';

      item.appendChild(tabTitle);
      item.appendChild(tabUrl);
      list.appendChild(item);
    }

    card.appendChild(titleRow);
    card.appendChild(tags);
    card.appendChild(list);
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
    for (const entry of entries.slice(0, 10)) {
      const item = document.createElement('div');
      item.className = 'item';

      const title = document.createElement('div');
      title.className = 'item-title';
      title.textContent = `#${entry.id} · ${entry.state || 'updated'}`;

      const filename = document.createElement('div');
      filename.className = 'item-url';
      filename.textContent = entry.filename || '(no filename)';

      const meta = document.createElement('div');
      meta.className = 'caption';
      meta.textContent = formatTime(entry.timestamp);

      item.appendChild(title);
      item.appendChild(filename);
      item.appendChild(meta);
      list.appendChild(item);
    }
  }

  downloadsEl.appendChild(list);
}

async function promptAndCreateSchedule(workflowId, workflowName) {
  const cadenceKind = (window.prompt('Cadence: daily | weekly | monthly | yearly', 'daily') || '')
    .trim()
    .toLowerCase();

  if (!cadenceKind) return;
  if (!['daily', 'weekly', 'monthly', 'yearly'].includes(cadenceKind)) {
    setStatus('Invalid cadence. Use daily/weekly/monthly/yearly.', 'warn');
    return;
  }

  const timeInput = parseTimeInput(window.prompt('Time (HH:MM, 24h)', '09:00'));
  if (!timeInput) {
    setStatus('Invalid time format.', 'warn');
    return;
  }

  const cadence = {
    kind: cadenceKind,
    hour: timeInput.hour,
    minute: timeInput.minute,
  };

  if (cadenceKind === 'weekly') {
    const weekday = Number.parseInt(
      window.prompt('Weekday (0=Sun .. 6=Sat)', String(new Date().getDay())) || '',
      10
    );
    if (Number.isNaN(weekday) || weekday < 0 || weekday > 6) {
      setStatus('Invalid weekday.', 'warn');
      return;
    }
    cadence.weekdays = [weekday];
  }

  if (cadenceKind === 'monthly') {
    const day = Number.parseInt(window.prompt('Day of month (1-31)', '1') || '', 10);
    if (Number.isNaN(day) || day < 1 || day > 31) {
      setStatus('Invalid day of month.', 'warn');
      return;
    }
    cadence.dayOfMonth = day;
  }

  if (cadenceKind === 'yearly') {
    const month = Number.parseInt(window.prompt('Month (1-12)', '1') || '', 10);
    const day = Number.parseInt(window.prompt('Day of month (1-31)', '1') || '', 10);
    if (Number.isNaN(month) || month < 1 || month > 12 || Number.isNaN(day) || day < 1 || day > 31) {
      setStatus('Invalid month/day.', 'warn');
      return;
    }
    cadence.month = month;
    cadence.dayOfMonth = day;
  }

  const response = await send({
    type: PANEL_CREATE_SCHEDULE,
    schedule: {
      name: `Schedule ${workflowName}`,
      workflowId,
      cadence,
      enabled: true,
    },
  });

  if (!response || response.ok !== true) {
    setStatus(`Create schedule failed: ${response?.error || 'unknown error'}`, 'error');
    return;
  }

  setStatus('Schedule created.', 'ok');
  await refresh();
}

function renderAutomation(state) {
  const automation = state.automation;
  const activeTabId = state.control?.activeTab?.id;

  automationEl.innerHTML = `
    <h3>Automation</h3>
    <div class="section-title">
      <h4>Recording</h4>
      <span class="caption" id="recording-state"></span>
    </div>
    <div class="row">
      <input id="record-name" placeholder="Workflow name" />
      <button id="record-start">Start</button>
      <button id="record-stop">Stop</button>
      <button id="record-save" class="primary">Save</button>
    </div>
    <div id="recording-steps" class="list"></div>
    <hr />
    <div class="section-title"><h4>Workflows</h4></div>
    <div id="workflow-list" class="list"></div>
    <hr />
    <div class="grid-2">
      <div>
        <div class="section-title"><h4>Shortcuts</h4></div>
        <div id="shortcut-list" class="list"></div>
      </div>
      <div>
        <div class="section-title"><h4>Schedules</h4></div>
        <div id="schedule-list" class="list"></div>
      </div>
    </div>
  `;

  const recordingStateEl = automationEl.querySelector('#recording-state');
  const recordingStepsEl = automationEl.querySelector('#recording-steps');
  const recordNameInput = automationEl.querySelector('#record-name');
  const workflowListEl = automationEl.querySelector('#workflow-list');
  const shortcutListEl = automationEl.querySelector('#shortcut-list');
  const scheduleListEl = automationEl.querySelector('#schedule-list');

  if (automation.recording) {
    recordingStateEl.textContent = `ON · ${automation.recording.stepCount} steps`;
    recordNameInput.value = automation.recording.name || '';

    for (const step of automation.recording.steps) {
      const item = document.createElement('div');
      item.className = 'item';
      item.innerHTML = `<div class="item-title">${escapeInline(step.action)}</div><div class="item-url">${escapeInline(JSON.stringify(step.args || {}))}</div>`;
      recordingStepsEl.appendChild(item);
    }

    if (automation.recording.steps.length === 0) {
      const empty = document.createElement('div');
      empty.className = 'caption';
      empty.textContent = 'Recording is active. Perform actions from Browser Control.';
      recordingStepsEl.appendChild(empty);
    }
  } else {
    recordingStateEl.textContent = 'OFF';
    const empty = document.createElement('div');
    empty.className = 'caption';
    empty.textContent = 'Start recording to capture actions into a reusable workflow.';
    recordingStepsEl.appendChild(empty);
  }

  automationEl.querySelector('#record-start').addEventListener('click', async () => {
    const result = await send({
      type: PANEL_START_RECORDING,
      name: recordNameInput.value.trim() || 'Recorded Workflow',
    });

    if (!result || result.ok !== true) {
      setStatus(`Start recording failed: ${result?.error || 'unknown error'}`, 'error');
      return;
    }

    setStatus('Recording started.', 'ok');
    await refresh();
  });

  automationEl.querySelector('#record-stop').addEventListener('click', async () => {
    const result = await send({ type: PANEL_STOP_RECORDING });
    if (!result || result.ok !== true) {
      setStatus(`Stop recording failed: ${result?.error || 'unknown error'}`, 'error');
      return;
    }

    setStatus('Recording stopped.', 'ok');
    await refresh();
  });

  automationEl.querySelector('#record-save').addEventListener('click', async () => {
    const result = await send({
      type: PANEL_SAVE_RECORDING,
      name: recordNameInput.value.trim() || undefined,
    });

    if (!result || result.ok !== true) {
      setStatus(`Save recording failed: ${result?.error || 'unknown error'}`, 'error');
      return;
    }

    setStatus(`Workflow saved: ${result.workflow?.name || ''}`, 'ok');
    await refresh();
  });

  if (!automation.workflows || automation.workflows.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'empty';
    empty.textContent = 'No workflows saved yet.';
    workflowListEl.appendChild(empty);
  } else {
    for (const workflow of automation.workflows) {
      const item = document.createElement('div');
      item.className = 'item';

      const title = document.createElement('div');
      title.className = 'item-title';
      title.textContent = `${workflow.name} (${workflow.stepCount} steps)`;

      const meta = document.createElement('div');
      meta.className = 'caption';
      meta.textContent = `Updated: ${formatTime(workflow.updatedAt)}`;

      const row = document.createElement('div');
      row.className = 'row wrap';

      const runBtn = document.createElement('button');
      runBtn.textContent = 'Run';
      runBtn.addEventListener('click', async () => {
        const response = await send({
          type: PANEL_RUN_WORKFLOW,
          workflowId: workflow.id,
          tabId: activeTabId,
        });

        if (!response || response.ok !== true) {
          setStatus(`Workflow failed: ${response?.error || 'unknown error'}`, 'error');
          return;
        }

        setStatus(`Workflow executed: ${workflow.name}`, 'ok');
        await refresh();
      });

      const shortcutBtn = document.createElement('button');
      shortcutBtn.textContent = 'Set Shortcut';
      shortcutBtn.addEventListener('click', async () => {
        const defaultName = workflow.name
          .toLowerCase()
          .replace(/[^a-z0-9]+/g, '-')
          .replace(/^-+|-+$/g, '')
          .slice(0, 32);
        const input = window.prompt('Shortcut name (without /)', defaultName || 'workflow');
        if (!input) return;

        const result = await send({
          type: PANEL_SET_SHORTCUT,
          name: input,
          workflowId: workflow.id,
        });

        if (!result || result.ok !== true) {
          setStatus(`Set shortcut failed: ${result?.error || 'unknown error'}`, 'error');
          return;
        }

        setStatus(`Shortcut saved: /${result.shortcut.name}`, 'ok');
        await refresh();
      });

      const scheduleBtn = document.createElement('button');
      scheduleBtn.textContent = 'Schedule';
      scheduleBtn.addEventListener('click', async () => {
        await promptAndCreateSchedule(workflow.id, workflow.name);
      });

      const deleteBtn = document.createElement('button');
      deleteBtn.className = 'danger';
      deleteBtn.textContent = 'Delete';
      deleteBtn.addEventListener('click', async () => {
        if (!window.confirm(`Delete workflow \"${workflow.name}\"?`)) return;
        const result = await send({ type: PANEL_DELETE_WORKFLOW, workflowId: workflow.id });
        if (!result || result.ok !== true) {
          setStatus(`Delete workflow failed: ${result?.error || 'unknown error'}`, 'error');
          return;
        }
        setStatus('Workflow deleted.', 'ok');
        await refresh();
      });

      row.appendChild(runBtn);
      row.appendChild(shortcutBtn);
      row.appendChild(scheduleBtn);
      row.appendChild(deleteBtn);

      item.appendChild(title);
      item.appendChild(meta);
      item.appendChild(row);
      workflowListEl.appendChild(item);
    }
  }

  if (!automation.shortcuts || automation.shortcuts.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'empty';
    empty.textContent = 'No shortcuts configured.';
    shortcutListEl.appendChild(empty);
  } else {
    for (const shortcut of automation.shortcuts) {
      const item = document.createElement('div');
      item.className = 'item';

      const title = document.createElement('div');
      title.className = 'item-title';
      title.textContent = `/${shortcut.name}`;

      const desc = document.createElement('div');
      desc.className = 'item-url';
      desc.textContent = shortcut.workflowName;

      const row = document.createElement('div');
      row.className = 'row wrap';

      const runBtn = document.createElement('button');
      runBtn.textContent = 'Run';
      runBtn.addEventListener('click', async () => {
        const response = await send({
          type: PANEL_RUN_SHORTCUT,
          name: shortcut.name,
          tabId: activeTabId,
        });

        if (!response || response.ok !== true) {
          setStatus(`Shortcut failed: ${response?.error || 'unknown error'}`, 'error');
          return;
        }

        setStatus(`Shortcut executed: /${shortcut.name}`, 'ok');
        await refresh();
      });

      const deleteBtn = document.createElement('button');
      deleteBtn.className = 'danger';
      deleteBtn.textContent = 'Delete';
      deleteBtn.addEventListener('click', async () => {
        const result = await send({ type: PANEL_DELETE_SHORTCUT, name: shortcut.name });
        if (!result || result.ok !== true) {
          setStatus(`Delete shortcut failed: ${result?.error || 'unknown error'}`, 'error');
          return;
        }
        setStatus('Shortcut deleted.', 'ok');
        await refresh();
      });

      row.appendChild(runBtn);
      row.appendChild(deleteBtn);

      item.appendChild(title);
      item.appendChild(desc);
      item.appendChild(row);
      shortcutListEl.appendChild(item);
    }
  }

  if (!automation.schedules || automation.schedules.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'empty';
    empty.textContent = 'No schedules configured.';
    scheduleListEl.appendChild(empty);
  } else {
    for (const schedule of automation.schedules) {
      const item = document.createElement('div');
      item.className = 'item';

      const title = document.createElement('div');
      title.className = 'item-title';
      title.textContent = schedule.name;

      const desc = document.createElement('div');
      desc.className = 'item-url';
      desc.textContent = `${schedule.workflowName} · ${schedule.cadence.kind}`;

      const time = document.createElement('div');
      time.className = 'caption';
      time.textContent = `Next: ${formatTime(schedule.nextRunAt)}`;

      const row = document.createElement('div');
      row.className = 'row wrap';

      const toggleBtn = document.createElement('button');
      toggleBtn.textContent = schedule.enabled ? 'Disable' : 'Enable';
      toggleBtn.addEventListener('click', async () => {
        const result = await send({
          type: PANEL_TOGGLE_SCHEDULE,
          scheduleId: schedule.id,
          enabled: !schedule.enabled,
        });

        if (!result || result.ok !== true) {
          setStatus(`Toggle schedule failed: ${result?.error || 'unknown error'}`, 'error');
          return;
        }

        setStatus('Schedule updated.', 'ok');
        await refresh();
      });

      const deleteBtn = document.createElement('button');
      deleteBtn.className = 'danger';
      deleteBtn.textContent = 'Delete';
      deleteBtn.addEventListener('click', async () => {
        const result = await send({
          type: PANEL_DELETE_SCHEDULE,
          scheduleId: schedule.id,
        });

        if (!result || result.ok !== true) {
          setStatus(`Delete schedule failed: ${result?.error || 'unknown error'}`, 'error');
          return;
        }

        setStatus('Schedule deleted.', 'ok');
        await refresh();
      });

      row.appendChild(toggleBtn);
      row.appendChild(deleteBtn);

      item.appendChild(title);
      item.appendChild(desc);
      item.appendChild(time);
      item.appendChild(row);
      scheduleListEl.appendChild(item);
    }
  }
}

function renderDeveloper(state) {
  const activity = state.activity;
  const activeTabId = state.control?.activeTab?.id;
  const domState = viewState.lastDomState || state.latestDomState || null;

  developerEl.innerHTML = `
    <h3>Developer Signals</h3>
    <div class="row wrap">
      <button id="dev-refresh-dom">Refresh DOM</button>
      <button id="dev-clear-activity">Clear Events</button>
      <button id="dev-toggle-bridge">${state.options.pageBridgeEnabled ? 'Disable' : 'Enable'} Page Bridge</button>
      <span class="caption">Console: ${activity.console.length} · Network: ${activity.network.length}</span>
      <span class="caption">Bridge: ${state.options.pageBridgeEnabled ? 'ON' : 'OFF (high-risk default)'}</span>
    </div>
    <div class="row">
      <input id="dev-selector" placeholder="DOM selector for capture (optional)" class="mono" />
      <label class="caption" style="display:flex;align-items:center;gap:4px;white-space:nowrap;">
        <input type="checkbox" id="dev-interactive-only" style="width:auto" />
        interactive-only
      </label>
    </div>
    <div class="grid-2">
      <div>
        <div class="section-title"><h4>DOM State</h4></div>
        <pre id="dev-dom-json"></pre>
      </div>
      <div>
        <div class="section-title"><h4>Recent Commands</h4></div>
        <div id="dev-command-list" class="list"></div>
      </div>
    </div>
    <hr />
    <div class="grid-2">
      <div>
        <div class="section-title"><h4>Console Events</h4></div>
        <div id="dev-console-list" class="list"></div>
      </div>
      <div>
        <div class="section-title"><h4>Network Events</h4></div>
        <div id="dev-network-list" class="list"></div>
      </div>
    </div>
  `;

  const domPre = developerEl.querySelector('#dev-dom-json');
  domPre.textContent = domState
    ? JSON.stringify(domState, null, 2)
    : 'No DOM state captured yet. Use Snapshot or DOM State in Browser Control.';

  const commandList = developerEl.querySelector('#dev-command-list');
  if (!activity.commandHistory || activity.commandHistory.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'empty';
    empty.textContent = 'No commands yet.';
    commandList.appendChild(empty);
  } else {
    for (const entry of activity.commandHistory.slice(0, 8)) {
      const item = document.createElement('div');
      item.className = 'item';
      item.innerHTML = `<div class="item-title">${entry.ok ? 'OK' : 'FAIL'} · ${escapeInline(entry.action)}</div><div class="caption">${formatTime(entry.timestamp)}${entry.error ? ` · ${escapeInline(entry.error)}` : ''}</div>`;
      commandList.appendChild(item);
    }
  }

  const consoleList = developerEl.querySelector('#dev-console-list');
  if (!activity.console || activity.console.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'empty';
    empty.textContent = 'No console events.';
    consoleList.appendChild(empty);
  } else {
    for (const event of activity.console.slice(0, 12)) {
      const payload = event.payload || {};
      const level = payload.level || 'log';
      const text = payload.message || (Array.isArray(payload.args) ? payload.args.join(' ') : JSON.stringify(payload));
      const item = document.createElement('div');
      item.className = 'item';
      item.innerHTML = `<div class="item-title">${escapeInline(level.toUpperCase())}</div><div class="item-url">${escapeInline(text)}</div><div class="caption">${formatTime(event.timestamp)}</div>`;
      consoleList.appendChild(item);
    }
  }

  const networkList = developerEl.querySelector('#dev-network-list');
  if (!activity.network || activity.network.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'empty';
    empty.textContent = 'No network events.';
    networkList.appendChild(empty);
  } else {
    for (const event of activity.network.slice(0, 12)) {
      const payload = event.payload || {};
      const item = document.createElement('div');
      item.className = 'item';
      const line1 = `${payload.transport || 'net'} ${payload.method || ''} ${payload.status || ''}`.trim();
      const line2 = payload.url || payload.error || '(unknown)';
      const line3 = payload.durationMs ? `${payload.durationMs} ms` : '';
      item.innerHTML = `<div class="item-title">${escapeInline(line1)}</div><div class="item-url">${escapeInline(line2)}</div><div class="caption">${escapeInline(line3)} · ${formatTime(event.timestamp)}</div>`;
      networkList.appendChild(item);
    }
  }

  developerEl.querySelector('#dev-refresh-dom').addEventListener('click', async () => {
    const selector = developerEl.querySelector('#dev-selector').value.trim();
    const interactiveOnly = developerEl.querySelector('#dev-interactive-only').checked;

    const response = await runAction(
      'dom-state',
      {
        selector: selector || undefined,
        interactiveOnly,
        maxNodes: 120,
      },
      activeTabId
    );

    if (response?.state) {
      viewState.lastDomState = response.state;
      renderDeveloper(state);
    }
  });

  developerEl.querySelector('#dev-clear-activity').addEventListener('click', async () => {
    await send({ type: PANEL_CLEAR_ACTIVITY });
    setStatus('Activity events cleared.', 'ok');
    await refresh();
  });

  developerEl.querySelector('#dev-toggle-bridge').addEventListener('click', async () => {
    const result = await send({
      type: PANEL_SET_OPTIONS,
      options: {
        ...state.options,
        pageBridgeEnabled: !state.options.pageBridgeEnabled,
      },
    });
    if (!result || result.ok !== true) {
      setStatus(`Toggle Page Bridge failed: ${result?.error || 'unknown error'}`, 'error');
      return;
    }
    setStatus(
      `Page Bridge ${!state.options.pageBridgeEnabled ? 'enabled' : 'disabled'} (reload page to apply).`,
      'ok'
    );
    await refresh();
  });
}

async function refresh() {
  const response = await send({ type: PANEL_GET_STATE });
  if (!response || response.ok !== true || !response.state) {
    setStatus(response?.error || 'Failed to load extension state.', 'error');
    return;
  }

  const normalizedState = normalizePanelState(response.state);
  viewState.panelState = normalizedState;
  if (normalizedState.latestDomState) {
    viewState.lastDomState = normalizedState.latestDomState;
  }
  renderControl(normalizedState);
  renderSummary(normalizedState);
  renderAutomation(normalizedState);
  renderDeveloper(normalizedState);
  renderSessions(normalizedState);
  renderDownloads(normalizedState);
}

refreshBtn.addEventListener('click', async () => {
  await refresh();
  setStatus('Panel refreshed.', 'ok');
});

cleanupBtn.addEventListener('click', async () => {
  const response = await send({ type: PANEL_CLEAN_EMPTY_GROUPS });
  if (!response || response.ok !== true) {
    setStatus(`Clean failed: ${response?.error || 'unknown error'}`, 'error');
    return;
  }
  setStatus('Empty groups cleaned.', 'ok');
  await refresh();
});

refresh().catch((error) => {
  setStatus(error instanceof Error ? error.message : String(error), 'error');
});

setInterval(() => {
  refresh().catch(() => {});
}, 5000);
