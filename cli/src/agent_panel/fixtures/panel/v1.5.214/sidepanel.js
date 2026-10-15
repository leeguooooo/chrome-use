import { PanelTimeline, phaseLabels } from './panel-state.js';
import { prepareImage } from './image-attachment.js';
import { renderMarkdown } from './markdown.js';

const busyPhases = new Set(['starting', 'running', 'waitingInput', 'stopping']);
// Main domain of a host: the last two labels, or three under common
// second-level public suffixes (example.co.uk, example.com.cn).
const secondLevel = new Set(['co.uk','org.uk','ac.uk','gov.uk','com.cn','net.cn','org.cn','gov.cn','edu.cn','com.hk','com.tw','co.jp','ne.jp','or.jp','ac.jp','co.kr','com.au','net.au','org.au','com.br','com.sg','co.in','co.nz','com.mx']);
export function mainDomain(host) {
  const clean = String(host || '').toLowerCase().replace(/^www\./, '').replace(/\.$/, '');
  if (!clean || /^[\d.]+$/.test(clean) || clean.includes(':') || !clean.includes('.')) return clean;
  const labels = clean.split('.');
  return labels.slice(secondLevel.has(labels.slice(-2).join('.')) ? -3 : -2).join('.');
}
function relativeTime(at) {
  const seconds = Math.max(0, (Date.now() - Number(at || 0)) / 1000);
  if (!at || !Number.isFinite(seconds)) return '';
  if (seconds < 60) return 'just now';
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return `${Math.floor(seconds / 86400)}d ago`;
}
const agentNames = { codex: 'Codex', claude: 'Claude Code', opencode: 'OpenCode', pi: 'Pi' };
const chatAgents = new Set(Object.keys(agentNames));
const fieldLabels = { model: 'Model', provider: 'Provider', effort: 'Reasoning effort', serviceTier: 'Service tier', smallModel: 'Small model', opusModel: 'Opus model', sonnetModel: 'Sonnet model', haikuModel: 'Haiku model' };

export function createPanel({ document, runtime, storage, tabs, schedule = setTimeout, cancel = clearTimeout, encodeImage = prepareImage }) {
  const ids = ['agentsSettingsTab', 'skillsSettingsTab', 'schedulesSettingsTab', 'localAgents', 'scheduleSettings', 'reloadSchedules', 'scheduleEntries', 'scheduleId', 'scheduleName', 'scheduleAgent', 'scheduleModel', 'scheduleEffort', 'scheduleTier', 'scheduleAccess', 'scheduleAutomatic', 'scheduleKind', 'scheduleTiming', 'scheduleSkill', 'schedulePrompt', 'scheduleEnabled', 'saveSchedule', 'newSchedule', 'skillPicker', 'skillLibrary', 'reloadSkills', 'skillEntries', 'skillName', 'skillContent', 'saveSkill', 'skillImportKind', 'skillImportSource', 'importSkill', 'attachmentDrafts', 'imageFiles', 'attachImages', 'turnAccess', 'turnEffortLabel', 'turnOptions', 'turnEffort', 'turnTier', 'turnTierLabel', 'saveTurnOptions', 'pendingRecovery', 'retryPending', 'clearPending', 'connection', 'notice', 'refresh', 'deleteChat', 'deleteConfirm', 'confirmDelete', 'cancelDelete', 'settings', 'chatTab', 'agentsTab', 'chatView', 'agentsView', 'conversation', 'conversationButton', 'conversationTitle', 'conversationMenu', 'scopeSite', 'scopeAll', 'conversationItems', 'newChat', 'pageChip', 'pageTitle', 'pageDetach', 'newOptions', 'access', 'accessLabel', 'modelChip', 'agent', 'model', 'customModel', 'modelStatus', 'inspectAgent', 'executable', 'status', 'phase', 'effectiveModel', 'transcript', 'empty', 'latest', 'requests', 'composer', 'message', 'queue', 'queuedMessages', 'resume', 'stop', 'sendDirect', 'send', 'discover', 'agentList', 'configuration', 'configTitle', 'configPath', 'configWarnings', 'configForm', 'configFields', 'configPreview', 'configDiff', 'applyConfig', 'discardConfig', 'restoreConfig', 'configResult'];
  const nodes = Object.fromEntries(ids.map(id => [id, document.getElementById(id)]));
  let selected = '', selectionEpoch = 0, connectionEpoch = 0, connected = false, closed = false, panelSupport = null;
  let timeline = new PanelTimeline(), conversations = [], conversation = null, timer = null, polling = false;
  const pendingMessages = new Map(), drafts = new Map(), imageDrafts = new Map();
  let convertingImages = false, skillCatalog = [], skillsEpoch = 0, skillChoice = null, visibleSkills = [], skillPickerIndex = 0;
  let schedules = [], schedulesEpoch = 0, settingsSection = 'agents';
  let optionsOwner = null;
  // The agent the user picked, sent with, or last chatted with. Only that
  // agent's CLI is started on its own; others wait for an explicit check.
  let preferredAgent = '';
  let modelEpoch = 0, newFullAccess = true, page = null, pageDetached = false, scope = 'site';
  let sending = false, configuration = null, configEpoch = 0, preview = null, applied = null;
  const rendered = new Map(), requestNodes = new Map(), queuedNodes = new Map();
  // Nodes added after the id list; looked up apart so the list stays stable.
  const working = document.getElementById('working'), suggestions = document.getElementById('suggestions');
  // Consecutive tool calls share one collapsible group; `toolTail` is the
  // group the next tool joins while nothing else has been rendered after it.
  let toolTail = null; const toolGroups = new Set();

  // Keep only correlation IDs in extension storage. Chat text lives in the
  // native journal; closing the view must not turn an uncertain send into a retry.
  async function savePending() {
    if (!storage?.local?.set) throw new Error('Message identity storage is unavailable; the message was not sent.');
    await storage.local.set({ ab_panel_pending: Object.fromEntries([...pendingMessages].map(([id, message]) => [id, message.id])) });
  }

  function element(tag, text, className) {
    const node = document.createElement(tag);
    if (text !== undefined) node.textContent = text;
    if (className) node.className = className;
    return node;
  }
  function error(error) { nodes.notice.textContent = String(error?.message || error); nodes.notice.hidden = false; }
  function clearError() { nodes.notice.hidden = true; nodes.notice.textContent = ''; }
  async function api(operation, params = {}) {
    if (!runtime?.sendMessage) throw new Error('Open this page from the chrome-use extension.');
    const reply = await runtime.sendMessage({ type: 'ab-agent-request', operation, params });
    if (!reply?.success) throw new Error(reply?.error || 'The local host did not confirm this operation.');
    return reply.result;
  }
  function renderConnection(state) {
    connected = state?.connected === true;
    panelSupport = state?.agentPanel?.schemaVersion === 1 ? state.agentPanel : null;
    nodes.connection.classList.toggle('disconnected', !connected);
    nodes.connection.classList.toggle('ok', connected && panelSupport?.mode === 'full');
    nodes.connection.textContent = connected ? 'Local bridge connected' : state?.connectionState === 'connecting' ? 'Waiting for local bridge…' : state?.connectionError || 'Local bridge unavailable. Run chrome-use extension install, then refresh.';
    if (connected && !panelSupport) nodes.connection.textContent = 'Bridge connected. Update the chrome-use CLI, then refresh to enable the agent panel.';
    if (connected && panelSupport?.mode === 'readOnly') nodes.connection.textContent = 'Connected. This Windows panel is read-only; browser CLI automation remains available.';
    const previousAgent = nodes.agent.value || preferredAgent;
    const supported = panelSupport?.chatAgents || ['codex', 'claude'];
    nodes.agent.replaceChildren(...supported.filter(id => chatAgents.has(id)).map(id => { const option = element('option', agentNames[id]); option.value = id; return option; }));
    nodes.agent.value = supported.includes(previousAgent) ? previousAgent : supported[0] || '';
    if (nodes.agent.value !== previousAgent) clearModels();
    const scheduleAgent=nodes.scheduleAgent.value;
    nodes.scheduleAgent.replaceChildren(...supported.filter(id => chatAgents.has(id)).map(id => {const node=element('option',agentNames[id]);node.value=id;return node;}));
    nodes.scheduleAgent.value=supported.includes(scheduleAgent)?scheduleAgent:supported[0] || '';
    controls();
  }
  async function connectionStatus() {
    const epoch = ++connectionEpoch;
    if (!runtime?.sendMessage) { renderConnection(null); throw new Error('Open this page from the chrome-use extension.'); }
    const state = await runtime.sendMessage({ type: 'ab-status' });
    if (epoch === connectionEpoch) renderConnection(state);
  }
  function controls() {
    const pending = pendingMessages.get(selected);
    nodes.pendingRecovery.hidden = !pending?.unconfirmed;
    nodes.retryPending.disabled = sending || !connected || typeof pending?.text !== 'string';
    nodes.clearPending.disabled = sending;
    const writable = panelSupport?.mode === 'full';
    nodes.conversation.disabled = sending || !writable; nodes.conversationButton.disabled = sending || !writable; nodes.newChat.disabled = sending || !writable;
    nodes.message.disabled = !writable; nodes.inspectAgent.disabled = !connected || !writable || !panelSupport?.modelInspection;
    nodes.agent.disabled = !writable; nodes.model.disabled = !writable; nodes.customModel.disabled = !writable; nodes.executable.disabled = !writable;
    nodes.skillsSettingsTab.hidden = !panelSupport?.skillLibrary;
    nodes.schedulesSettingsTab.hidden = !panelSupport?.schedules;
    nodes.localAgents.hidden = settingsSection !== 'agents';
    nodes.skillLibrary.hidden = !panelSupport?.skillLibrary || settingsSection !== 'skills';
    nodes.scheduleSettings.hidden = !panelSupport?.schedules || settingsSection !== 'schedules';
    nodes.agentsView.dataset.section = settingsSection;
    nodes.newOptions.hidden = Boolean(selected); nodes.modelChip.hidden = Boolean(selected);
    const optionAgent = conversation?.agentId || nodes.agent.value;
    const optionOwner = `${selected}:${optionAgent}`;
    if (optionsOwner !== optionOwner) {
      optionsOwner = optionOwner;
      const first = element('option', 'CLI default'); first.value = '';
      const efforts = optionAgent === 'codex' ? ['minimal','low','medium','high','xhigh'] : optionAgent === 'claude' ? ['low','medium','high','max'] : [];
      nodes.turnEffort.replaceChildren(first, ...efforts.map(value => { const node = element('option', value); node.value = value; return node; }));
      const access = element('option','Follow permission switch'); access.value = '';
      const modes = optionAgent === 'opencode' ? ['runCommands'] : optionAgent === 'codex' ? ['readFiles','writeFiles','runCommands'] : ['browserOnly','readFiles','writeFiles','runCommands'];
      const labels = { browserOnly:'Browser only', readFiles: optionAgent === 'codex' ? 'Browser and file reads (Codex shell remains)' : 'Browser and file reads', writeFiles:'Browser, file reads and writes', runCommands:'Browser, files and commands' };
      nodes.turnAccess.replaceChildren(access,...modes.map(value => { const node=element('option',labels[value]); node.value=value; return node; }));
      nodes.turnAccess.value = conversation?.options?.toolAccess || '';
      nodes.turnEffort.value = conversation?.options?.effort || '';
      nodes.turnTier.value = conversation?.options?.serviceTier || '';
    }
    nodes.turnOptions.hidden = !panelSupport?.conversationOptions;
    nodes.turnEffortLabel.hidden = !['codex','claude'].includes(optionAgent);
    nodes.turnTierLabel.hidden = optionAgent !== 'codex';
    const optionsLocked = !connected || sending || busyPhases.has(conversation?.phase) || conversation?.phase === 'recoveryRequired';
    nodes.turnAccess.disabled = optionsLocked; nodes.turnEffort.disabled = optionsLocked; nodes.turnTier.disabled = optionsLocked;
    nodes.saveTurnOptions.hidden = !selected; nodes.saveTurnOptions.disabled = optionsLocked;
    const full = selected ? conversation?.fullAccess === true : newFullAccess;
    nodes.access.hidden = !writable || !panelSupport?.fullAccess;
    nodes.access.disabled = !connected || sending || (Boolean(selected) && !conversation);
    const accessAgent = conversation?.agentId || nodes.agent.value;
    const cliPermissions = accessAgent === 'opencode', browserOnly = accessAgent === 'pi';
    if (browserOnly && conversation?.options?.toolAccess) nodes.access.hidden = true;
    nodes.accessLabel.textContent = conversation?.options?.toolAccess ? (full ? 'Allow tools automatically' : 'Review tool approvals') : full ? 'Full access' : cliPermissions ? 'CLI permissions' : browserOnly ? 'Browser only' : 'Ask first';
    nodes.access.setAttribute('aria-pressed', String(full));
    nodes.access.title = full ? (browserOnly ? 'Pi runs browser tools, commands and file changes. Click for browser tools only.' : 'Automatically allows permission requests in this conversation. Click to turn this off.') : cliPermissions ? 'OpenCode uses its own permission rules and may allow tools without asking. Requests it does send appear here. Click to automatically allow those requests.' : browserOnly ? 'Pi never asks before a tool runs, so it gets only the browser tools: no shell or file changes. Click for full access.' : 'Asks before each tool, command or file change. Click for full access.';
    nodes.deleteChat.hidden = !selected || !writable || !panelSupport?.conversationDelete;
    nodes.deleteChat.disabled = sending || !connected || busyPhases.has(conversation?.phase);
    if (nodes.deleteChat.hidden) nodes.deleteConfirm.hidden = true;
    nodes.confirmDelete.disabled = nodes.deleteChat.disabled;
    nodes.phase.textContent = panelSupport?.mode === 'readOnly' ? 'Read-only discovery' : conversation ? phaseLabels[conversation.phase] || 'Unknown response state' : 'Ready';
    nodes.effectiveModel.textContent = conversation?.model || '';
    const busy = busyPhases.has(conversation?.phase);
    renderWorking();
    nodes.status.classList.toggle('idle', !conversation || (conversation.phase === 'idle' && !conversation.model));
    nodes.status.classList.toggle('busy', busy);
    nodes.queue.textContent = pending ? 'Send unconfirmed; checking stored message' : conversation?.queued ? `${conversation.queued} queued${conversation.queuePaused ? ', paused' : ''}` : '';
    nodes.resume.hidden = !conversation?.queuePaused || !conversation?.queued;
    nodes.resume.disabled = !connected || !writable || conversation?.phase === 'recoveryRequired';
    nodes.stop.disabled = !connected || !writable || !busy || conversation?.phase === 'stopping';
    nodes.stop.hidden = !busy;
    nodes.attachImages.hidden = !panelSupport?.imageAttachments; nodes.attachImages.disabled = !connected || sending || convertingImages;
    nodes.send.disabled = convertingImages || !connected || !writable || sending || Boolean(pending) || conversation?.phase === 'recoveryRequired';
    nodes.sendDirect.hidden = !busyPhases.has(conversation?.phase) && !conversation?.queuePaused;
    nodes.sendDirect.disabled = nodes.send.disabled || Boolean(conversation?.directMessageId);
    nodes.sendDirect.textContent = busyPhases.has(conversation?.phase) ? 'Stop & send' : 'Send now';
    nodes.send.title = busy ? 'Queue — sends after the current response' : 'Send';
    nodes.send.setAttribute('aria-label', busy ? 'Queue message' : 'Send message');
  }
  function conversationChoices() {
    nodes.conversation.replaceChildren(element('option', 'New conversation'));
    nodes.conversation.firstElementChild.value = '';
    for (const chat of conversations) { const option = element('option', chat.title || 'Conversation'); option.value = chat.id; nodes.conversation.append(option); }
    nodes.conversation.value = selected;
    renderConversationMenu();
  }
  // The list can show every conversation or only those started on the main
  // domain of the tab beside the panel (x.com covers mobile.x.com).
  function currentDomain() {
    try { return page ? mainDomain(new URL(page.url).hostname) : null; } catch { return null; }
  }
  function renderConversationMenu() {
    const chat = conversations.find(c => c.id === selected);
    nodes.conversationTitle.textContent = chat?.title || 'New conversation';
    const domain = currentDomain(), siteScope = scope === 'site' && Boolean(domain);
    nodes.scopeSite.textContent = domain ? `This site · ${domain}` : 'This site';
    nodes.scopeSite.disabled = !domain;
    nodes.scopeSite.setAttribute('aria-selected', String(siteScope)); nodes.scopeAll.setAttribute('aria-selected', String(!siteScope));
    const shown = siteScope ? conversations.filter(c => c.site && mainDomain(c.site) === domain) : conversations;
    const newItem = element('button', undefined, 'conversation-item new'); newItem.type = 'button';
    newItem.append(element('span', '＋ New conversation', 'title'));
    newItem.addEventListener('click', () => { closeConversationMenu(); nodes.conversation.value = ''; select(''); loadModels(); });
    const items = [newItem];
    for (const c of shown) {
      const item = element('button', undefined, `conversation-item${c.id === selected ? ' selected' : ''}`); item.type = 'button';
      item.setAttribute('role', 'option'); item.setAttribute('aria-selected', String(c.id === selected));
      item.append(element('span', c.title || 'Conversation', 'title'), element('span', [agentNames[c.agentId] || c.agentId, !siteScope && c.site, relativeTime(c.updated)].filter(Boolean).join(' · '), 'meta'));
      item.addEventListener('click', () => { closeConversationMenu(); select(c.id); });
      items.push(item);
    }
    if (!shown.length) items.push(element('p', siteScope ? `No conversations on ${domain} yet.` : 'No conversations yet.', 'conversation-empty'));
    nodes.conversationItems.replaceChildren(...items);
  }
  function closeConversationMenu() { nodes.conversationMenu.hidden = true; nodes.conversationButton.setAttribute('aria-expanded', 'false'); }
  function setScope(next) { scope = next; storage?.local?.set({ ab_panel_conversation_scope: next }).catch(() => {}); renderConversationMenu(); }
  function select(id, duringCreate = false) {
    if (sending && !duringCreate) { nodes.conversation.value = selected; return; }
    drafts.set(selected, nodes.message.value);
    nodes.message.value = drafts.get(id) || '';
    nodes.conversation.value = id;
    nodes.deleteConfirm.hidden = true;
    selected = id; skillChoice = null; renderImageDrafts(); selectionEpoch++; conversation = conversations.find(c => c.id === id) || null;
    void loadSkills().catch(error);
    timeline = new PanelTimeline(); queuedNodes.clear(); nodes.queuedMessages.replaceChildren(); nodes.queuedMessages.hidden = true; rendered.clear(); requestNodes.clear(); nodes.requests.replaceChildren();
    toolTail = null; toolGroups.clear();
    nodes.transcript.replaceChildren(nodes.empty); nodes.empty.hidden = false; controls(); renderConversationMenu();
    storage?.local?.set({ ab_panel_conversation: id }).catch(() => {});
    void poll().catch(error);
  }
  async function list() {
    const result = await api('conversationList'); conversations = result.conversations; conversationChoices();
    if (preferredAgent) return;
    const latest = conversations.filter(chat => [...nodes.agent.querySelectorAll('option')].some(option => option.value === chat.agentId)).sort((a, b) => (b.updated || 0) - (a.updated || 0))[0];
    if (latest && !selected && nodes.agent.value !== latest.agentId) { nodes.agent.value = latest.agentId; clearModels(); }
    if (latest) preferredAgent = latest.agentId;
  }
  function rememberAgent(agentId) { preferredAgent = agentId; storage?.local?.set({ ab_panel_agent: agentId }).catch(() => {}); }
  function renderTimeline() {
    const follow = nodes.transcript.scrollHeight - nodes.transcript.scrollTop - nodes.transcript.clientHeight < 80;
    nodes.empty.hidden = timeline.rows.length > 0;
    // A tool still open when the response ended is not running any more.
    const live = busyPhases.has(conversation?.phase);
    for (const row of timeline.rows) {
      let view = rendered.get(row.key);
      if (!view) {
        if (row.type === 'tool') {
          const container = element('details', undefined, 'tool-row'); const title = element('summary');
          const name = element('span', undefined, 'tool-name'); const hint = element('span', undefined, 'tool-hint'); const state = element('span', undefined, 'tool-state'); title.append(name, hint, state);
          const content = element('pre'); container.append(title, content); view = { container, name, hint, state, content, row };
          if (!toolTail) toolTail = toolGroup();
          toolTail.tools.push(view); toolTail.list.append(container); toolGroups.add(toolTail);
          rendered.set(row.key, view);
        } else {
          const container = element('article', undefined, `message-row ${row.type}`);
          const speaker = element('div', row.type === 'user' ? 'You' : row.type === 'assistant' ? 'Assistant' : 'Response status', 'speaker');
          const content = element('div', undefined, 'content'); container.append(content, speaker); view = { container, content, speaker };
          if (row.attachments?.length) {
            const images = element('div', undefined, 'attachment-images'); container.append(images);
            const chatId = selected;
            for (const attachment of row.attachments) {
              const image = element('img'); image.alt = 'Attached image'; images.append(image);
              void api('conversationAttachmentRead', { conversationId: chatId, attachment }).then(result => {
                image.src = `data:${result.attachment.mediaType};base64,${result.data}`;
              }).catch(() => { image.alt = 'Saved image unavailable'; });
            }
          }
          if (row.page?.title) container.append(element('div', row.page.title, 'page-ref'));
          if (row.detail) { const raw = element('details'); raw.append(element('summary', 'Details'), element('pre', row.detail)); container.append(raw); }
          if (row.type === 'note' && row.failed && conversation?.agentId) {
            const agentId = conversation.agentId;
            const prompt = [...timeline.rows].reverse().find(r => r.type === 'user' && !r.queueStatus)?.text;
            // Only an explicitly chosen model can be dropped; a model from the
            // CLI's own configuration has to be changed there.
            if (conversation.requestedModel && prompt) container.append(button('Retry with default model', () => retryWithDefaultModel(agentId, prompt), 'retry'));
            else if (conversation.model) container.append(button('Change default model', async () => { showAgents(); await loadConfig(agentId, agentNames[agentId] || agentId); }, 'retry'));
          }
        }
        if (row.type !== 'tool') { toolTail = null; rendered.set(row.key, view); nodes.transcript.append(view.container); }
      }
      if (view.text !== row.text) {
        view.text = row.text;
        // Agent replies are Markdown; user text and tool output stay literal.
        if (row.type === 'assistant') { renderMarkdown(document, view.content, row.text.trim()); view.container.hidden = !row.text.trim(); }
        else view.content.textContent = row.text;
      }
      if (row.type === 'user') view.speaker.textContent = row.queueStatus || '';
      if (row.type === 'tool') {
        view.name.textContent = row.name; view.hint.textContent = row.hint || ''; view.row = row;
        view.state.textContent = row.failed ? 'Failed' : ''; view.running = live && !row.failed && !row.completed;
        view.state.title = row.failed ? 'Failed' : view.running ? 'Running' : '';
        view.container.classList.toggle('failed', row.failed); view.container.classList.toggle('running', view.running);
      }
    }
    for (const group of toolGroups) renderToolGroup(group);
    renderWorking();
    if (follow) nodes.transcript.scrollTop = nodes.transcript.scrollHeight;
    nodes.latest.hidden = follow || timeline.rows.length === 0;
  }
  function toolGroup() {
    const container = element('details', undefined, 'tool-group'), summary = element('summary'), list = element('div', undefined, 'tool-list');
    const group = { container, summary, list, tools: [], touched: false, wasRunning: null };
    // Once the user opens a group or a step in it, it stays the way they left it.
    container.addEventListener('click', () => { group.touched = true; });
    container.append(summary, list); nodes.transcript.append(container); return group;
  }
  // A running group stays open; a finished group of several steps folds into
  // one summary line so the reply below it stays in view.
  function renderToolGroup(group) {
    const count = group.tools.length, failed = group.tools.filter(t => t.row.failed).length;
    const running = group.tools.some(t => t.running);
    group.container.classList.toggle('single', count === 1); group.container.classList.toggle('running', running);
    group.summary.textContent = `${running ? 'Working · ' : ''}${count} step${count === 1 ? '' : 's'}${failed ? ` · ${failed} failed` : ''}`;
    if (count === 1) group.container.open = true;
    else if (!group.touched && running !== group.wasRunning) group.container.open = running;
    group.wasRunning = running;
  }
  // Three dots under the transcript while the agent works and nothing else
  // (a running tool) already shows that it is busy.
  function renderWorking() {
    if (!working) return;
    const last = timeline.rows[timeline.rows.length - 1];
    const show = ['starting', 'running'].includes(conversation?.phase) && !(last?.type === 'tool' && !last.completed && !last.failed);
    working.hidden = !show;
    if (show && nodes.transcript.lastElementChild !== working) nodes.transcript.append(working);
  }
  function button(text, action, className = 'quiet') {
    const node = element('button', text, className); node.type = 'button'; node.addEventListener('click', () => void action().catch(error)); return node;
  }
  function questionFields(questions, container) {
    return questions.map((question, index) => {
      const group = element('div', undefined, 'question'); group.append(element('label', question.question || question.header || 'Question'));
      const choices = [];
      for (const option of question.options || []) {
        const label = element('label', undefined, 'choice'); const input = element('input');
        input.type = question.multiSelect ? 'checkbox' : 'radio'; input.name = `question-${selectionEpoch}-${index}`; input.value = option.label;
        const text = element('span', option.label); if (option.description) text.append(element('small', option.description)); label.append(input, text); group.append(label); choices.push(input);
      }
      const free = element('input'); free.placeholder = 'Your answer'; free.setAttribute('aria-label', `Answer: ${question.question || question.header || 'Question'}`); group.append(free); container.append(group);
      return { question, read() { const text = free.value.trim(); return text ? [text] : choices.filter(c => c.checked).map(c => c.value); } };
    });
  }
  async function respond(key, result) {
    const request = timeline.requests.get(key); if (!request || request.sent) return;
    const id = selected, epoch = selectionEpoch;
    const view = requestNodes.get(key); for (const node of view.container.querySelectorAll('button')) node.disabled = true;
    try {
      await api('conversationRespond', { conversationId: id, requestId: request.requestId, result });
      if (selected === id && epoch === selectionEpoch) { request.sent = true; renderRequests(); }
    } catch (failure) { if (selected === id && epoch === selectionEpoch) { request.unconfirmed = true; view.status.textContent = 'Response unconfirmed. Read the current state before retrying.'; } throw failure; }
  }
  function requestView(key, request) {
    const container = element('section', undefined, 'request'); const status = element('p', '', 'hint');
    const params = request.params || {}; const method = request.method; const actions = element('div', undefined, 'actions');
    container.append(element('h3', method === 'item/tool/requestUserInput' || params.tool_name === 'AskUserQuestion' ? 'The agent needs your input' : 'Review the agent request'));
    if (params.truncated) { container.append(element('p', 'The request preview is incomplete. Stop this response before approving anything.')); }
    else if (method === 'item/tool/requestUserInput' || params.tool_name === 'AskUserQuestion') {
      const fields = questionFields(method === 'item/tool/requestUserInput' ? params.questions || [] : params.input?.questions || [], container);
      actions.append(button('Send answers', async () => {
        const answers = fields.map(f => ({ question: f.question, values: f.read() }));
        if (answers.some(a => a.values.length === 0)) throw new Error('Answer each question before sending.');
        const result = method === 'item/tool/requestUserInput'
          ? { answers: Object.fromEntries(answers.map(a => [a.question.id, { answers: a.values }])) }
          : { behavior: 'allow', updatedInput: { ...params.input, answers: Object.fromEntries(answers.map(a => [a.question.question, a.values.join(', ')])) } };
        await respond(key, result);
      }, 'primary'));
    } else if (['item/commandExecution/requestApproval', 'item/fileChange/requestApproval'].includes(method)) {
      container.append(element('p', params.reason || 'This action requires a decision.'));
      if (params.command) container.append(element('pre', params.command));
      if (params.cwd) container.append(element('p', `Working folder: ${params.cwd}`, 'hint'));
      actions.append(button('Allow once', () => respond(key, { decision: 'accept' }), 'primary'), button('Decline', () => respond(key, { decision: 'decline' })));
    } else if (method === 'mcpServer/elicitation/request') {
      container.append(element('p', params.message || 'The MCP server needs a decision.'));
      const properties = params.requestedSchema?.properties;
      const approval = params.serverName === 'chrome_use' && params.mode === 'form' && params._meta?.codex_approval_kind === 'mcp_tool_call' && params.requestedSchema?.type === 'object' && properties && typeof properties === 'object' && Object.keys(properties).length === 0 && (params.requestedSchema.required === undefined || (Array.isArray(params.requestedSchema.required) && params.requestedSchema.required.length === 0)) && params._meta.tool_params && typeof params._meta.tool_params === 'object' && !Array.isArray(params._meta.tool_params);
      if (approval) {
        container.dataset.approvalKind = 'mcpTool'; container.dataset.serverName = params.serverName;
        if (params._meta.tool_description) container.append(element('p', params._meta.tool_description, 'hint'));
        container.append(element('pre', JSON.stringify(params._meta.tool_params, null, 2), 'tool-arguments'));
        actions.append(button('Allow once', () => respond(key, {action:'accept',content:{}}), 'primary'));
      } else container.append(element('p', 'This form cannot be safely completed here. Decline or cancel this request.'));
      actions.append(button('Decline', () => respond(key, {action:'decline',content:null})), button('Cancel request', () => respond(key, {action:'cancel',content:null})));
    } else if (method === 'session/request_permission') {
      container.append(element('p', params.toolCall?.title || 'OpenCode tool request'));
      container.append(element('pre', JSON.stringify(params.toolCall?.rawInput || {}, null, 2)));
      for (const option of params.options || []) actions.append(button(option.name || option.optionId, () => respond(key, { outcome: { outcome: 'selected', optionId: option.optionId } }), option.kind === 'allow_once' ? 'primary' : 'quiet'));
      actions.append(button('Cancel request', () => respond(key, { outcome: { outcome: 'cancelled' } })));
    } else if (method === 'can_use_tool') {
      container.append(element('p', params.tool_name || 'Tool request')); container.append(element('pre', JSON.stringify(params.input || {}, null, 2)));
      actions.append(button('Allow once', () => respond(key, { behavior: 'allow', updatedInput: params.input || {} }), 'primary'), button('Decline', () => respond(key, { behavior: 'deny', message: 'User declined' })));
    } else if (method === 'item/permissions/requestApproval') {
      container.append(element('p', params.reason || 'Review the requested access for this turn.')); container.append(element('pre', JSON.stringify(params.permissions || {}, null, 2)));
      actions.append(button('Allow for this turn', () => respond(key, { permissions: params.permissions || {}, scope: 'turn' }), 'primary'));
    } else { container.append(element('p', 'This request needs a response format this view cannot present. Stop the response and inspect the CLI request.')); }
    const question = method === 'item/tool/requestUserInput' || params.tool_name === 'AskUserQuestion';
    if (!question && !params.truncated && panelSupport?.fullAccess && actions.querySelectorAll('button').length) actions.append(button('Allow all in this conversation', () => setAccess(true)));
    container.append(actions, status); return { container, status };
  }
  function renderRequests() {
    for (const [key, view] of requestNodes) if (!timeline.requests.has(key)) { view.container.remove(); requestNodes.delete(key); }
    for (const [key, request] of timeline.requests) {
      if (!requestNodes.has(key)) { const view = requestView(key, request); requestNodes.set(key, view); nodes.requests.append(view.container); }
      const view = requestNodes.get(key); view.status.textContent = request.sent ? 'Choice sent; waiting for the agent to continue.' : request.unconfirmed ? 'Response unconfirmed. Waiting for the current agent state.' : '';
      if (request.sent || request.unconfirmed) for (const node of view.container.querySelectorAll('button')) node.disabled = true;
    }
  }
  function renderQueue(messages) {
    nodes.queuedMessages.hidden = messages.length === 0;
    const live = new Set(messages.map(message => message.id));
    for (const [id, view] of queuedNodes) if (!live.has(id)) { view.container.remove(); queuedNodes.delete(id); }
    for (const message of messages) {
      if (queuedNodes.has(message.id)) continue;
      const container = element('details', undefined, 'queued-message');
      container.append(element('summary', `${message.direct ? 'Send after stop' : 'Queued'}: ${message.text.split('\n')[0].slice(0, 100)}`));
      let expectedText = message.truncated ? null : message.text;
      const input = element('textarea'); input.readOnly = message.truncated === true; input.value = message.text; input.maxLength = 32768; input.setAttribute('aria-label', 'Edit queued message');
      const actions = element('div', undefined, 'actions'); const status = element('p', '', 'hint');
      const id = selected, epoch = selectionEpoch;
      const mutate = async (operation, extra = {}) => {
        if (selected !== id || selectionEpoch !== epoch) return;
        if (expectedText === null) throw new Error('Load the full queued message before changing it.');
        for (const node of actions.querySelectorAll('button')) node.disabled = true;
        try {
          await api(operation, { conversationId: id, messageId: message.id, expectedText, ...extra });
          if (selected === id && selectionEpoch === epoch) { status.textContent = 'Change saved; reading the queue.'; await poll(); }
        } catch (failure) { if (selected === id && selectionEpoch === epoch) status.textContent = 'Change unconfirmed. Refresh the queue before editing again.'; throw failure; }
      };
      if (message.truncated) actions.append(button('Load full text', async () => {
        const result = await api('conversationQueueMessageRead', { conversationId: id, messageId: message.id });
        if (selected !== id || selectionEpoch !== epoch) return;
        expectedText = result.queuedMessage.text; input.value = expectedText; input.readOnly = false; status.textContent = 'Full queued message loaded.';
      }));
      actions.append(button('Save edit', async () => {
        if (!input.value.trim()) throw new Error('Enter a message, or remove it from the queue.');
        if (input.value === expectedText) return;
        await mutate('conversationQueueEdit', { replacementId: crypto.randomUUID(), text: input.value });
      }), button('Remove', () => mutate('conversationQueueRemove')));
      container.append(input, actions, status); queuedNodes.set(message.id, { container }); nodes.queuedMessages.append(container);
    }
    for (const [index, message] of messages.entries()) {
      const container = queuedNodes.get(message.id).container;
      if (nodes.queuedMessages.children[index] !== container) nodes.queuedMessages.insertBefore(container, nodes.queuedMessages.children[index] || null);
    }
  }
  async function poll() {
    if (closed || polling || !connected || panelSupport?.mode !== 'full' || !selected) return;
    polling = true; const id = selected, epoch = selectionEpoch;
    try {
      let more = true;
      while (more && id === selected && epoch === selectionEpoch) {
        const result = await api('conversationRead', { conversationId: id, after: timeline.cursor });
        if (id !== selected || epoch !== selectionEpoch) break;
        timeline.apply(result.journal.events, result.journal.cursor); conversation = result.conversation; more = result.journal.more === true;
        renderTimeline(); renderRequests(); renderQueue(result.queuedMessages || []); controls();
      }
      const pending = pendingMessages.get(id);
      if (pending && id === selected && epoch === selectionEpoch) {
        const status = await api('conversationMessageStatus', { conversationId: id, messageId: pending.id });
        if (pendingMessages.get(id) === pending && status.message.known) {
          pendingMessages.delete(id);
          await savePending();
          if (id === selected && epoch === selectionEpoch) { if (nodes.message.value === pending.text) { nodes.message.value = ''; imageDrafts.delete(id); renderImageDrafts(); } controls(); }
          if (drafts.get(id) === pending.text) drafts.delete(id);
        } else if (pendingMessages.get(id) === pending) {
          pending.unconfirmed = true;
          if (id === selected && epoch === selectionEpoch) controls();
        }
      }
    } finally { polling = false; }
  }
  function loop() {
    if (closed) return;
    cancel(timer); timer = schedule(async () => {
      try { await poll(); if (!nodes.agentsView.hidden && settingsSection === 'schedules') await loadSchedules(); } catch (failure) { error(failure); }
      loop();
    }, busyPhases.has(conversation?.phase) || pendingMessages.has(selected) ? 600 : 1800);
  }
  async function recoverPending(retry) {
    const id = selected, pending = pendingMessages.get(id);
    if (!pending?.unconfirmed || sending) return;
    if (retry && typeof pending.text !== 'string') throw new Error('The original text is unavailable; clear the warning or inspect the conversation history.');
    sending = true; controls();
    try {
      if (retry) {
        await api(pending.direct ? 'conversationSendDirect' : 'conversationSend', {conversationId:id,messageId:pending.id,text:pending.text, ...(pending.params || {})});
        if (selected === id && nodes.message.value === pending.text) nodes.message.value = '';
        if (drafts.get(id) === pending.text) drafts.delete(id);
      }
      if (pendingMessages.get(id) === pending) { pendingMessages.delete(id); await savePending(); }
      await poll();
    } finally { sending = false; controls(); }
  }
  nodes.retryPending.addEventListener('click', () => void recoverPending(true).catch(error));
  nodes.clearPending.addEventListener('click', () => void recoverPending(false).catch(error));
  async function send(direct = false) {
    if (panelSupport?.mode !== 'full') throw new Error('Local agent conversations require a supported current CLI.');
    if (!connected || sending || convertingImages || pendingMessages.has(selected) || conversation?.phase === 'recoveryRequired' || !nodes.message.value.trim()) return;
    if (direct && conversation?.directMessageId) throw new Error('A direct message is already waiting for stop confirmation.');
    const text = nodes.message.value, chosen = chosenSkill(text);
    const images = [...(imageDrafts.get(selected) || [])];
    sending = true; clearError(); controls();
    try {
      if (!selected) {
        const options = { agentId: nodes.agent.value };
        const model = nodes.customModel.value.trim() || nodes.model.value;
        if (model) options.model = model;
        if (panelSupport?.conversationOptions) options.options = readTurnOptions();
        if (nodes.executable.value.trim()) options.executable = nodes.executable.value.trim();
        if (panelSupport?.fullAccess) options.fullAccess = newFullAccess;
        const result = await api('conversationCreate', options); rememberAgent(options.agentId); conversations.unshift(result.conversation); conversationChoices(); select(result.conversation.id, true); nodes.message.value = text;
      }
      const id = selected, pending = { id: crypto.randomUUID(), text, direct };
      imageDrafts.set(id, images); if (images.length) imageDrafts.delete(''); renderImageDrafts();
      const params = { conversationId: id, messageId: pending.id, text };
      const attached = attachedPage(); if (attached) params.page = attached;
      if (images.length) {
        params.attachments = [];
        for (const image of images) {
          const uploaded = await api('conversationAttachmentPut', { conversationId: id, data: image.data });
          params.attachments.push(uploaded.attachment);
        }
      }
      if (chosen) {
        const snapshot = await api('conversationSkillSelect', { conversationId: id, skillId: chosen.id });
        params.skillId = snapshot.skill.id;
      }
      pending.params = { ...params }; delete pending.params.conversationId; delete pending.params.messageId; delete pending.params.text;
      pendingMessages.set(id, pending); controls();
      try { await savePending(); } catch (failure) { pendingMessages.delete(id); throw failure; }
      const result = await api(direct ? 'conversationSendDirect' : 'conversationSend', params);
      pageDetached = false; renderPage();
      if (selected === id) { conversation = result.conversation; if (nodes.message.value === text) nodes.message.value = ''; }
      imageDrafts.delete(id); if (images.length) imageDrafts.delete(''); renderImageDrafts();
      pendingMessages.delete(id); drafts.delete(id); await savePending(); await list(); await poll();
    } finally { sending = false; controls(); }
  }
  // A conversation keeps the model it was created with, so a model the account
  // cannot use is fixed by starting a new conversation on the CLI default.
  async function retryWithDefaultModel(agentId, prompt) {
    if (sending) return;
    select(''); nodes.agent.value = agentId; nodes.model.value = ''; nodes.customModel.value = ''; clearModels(); nodes.message.value = prompt;
    await send();
  }
  function showAgents() { settingsTab('agents'); view(true); }
  // The tab beside the panel is what "this page" means. It is attached to the
  // next message unless the user removes it; only web pages are offered.
  function attachedPage() {
    if (!page || pageDetached || !panelSupport?.pageContext) return null;
    return { tabId: page.tabId, url: page.url.slice(0, 8192), title: page.title.slice(0, 1024) };
  }
  function renderPage() {
    nodes.pageChip.hidden = !page || pageDetached || !panelSupport?.pageContext || panelSupport?.mode !== 'full';
    nodes.pageTitle.textContent = page?.title || '';
    nodes.pageChip.title = page ? `Sent with your message: ${page.url}` : '';
  }
  async function refreshPage() {
    if (!tabs?.query) return;
    const [tab] = await tabs.query({ active: true, currentWindow: true });
    const next = tab && typeof tab.id === 'number' && /^https?:/.test(tab.url || '') ? { tabId: tab.id, url: tab.url, title: tab.title || tab.url } : null;
    if (next?.tabId !== page?.tabId) pageDetached = false;
    page = next; renderPage(); renderConversationMenu();
  }
  // New conversations use the remembered choice; an existing one switches in
  // place, and the service answers requests that are already waiting.
  async function setAccess(full) {
    if (!selected) { newFullAccess = full; storage?.local?.set({ ab_panel_full_access: full }).catch(() => {}); controls(); return; }
    const id = selected, epoch = selectionEpoch;
    const result = await api('conversationSetAccess', { conversationId: id, fullAccess: full });
    if (selected === id && epoch === selectionEpoch) { conversation = result.conversation; controls(); await poll(); }
  }
  // Deletion is permanent: the native service stops the agent first, then
  // removes panel history, the CLI's session files and the browser session.
  async function deleteConversation() {
    const id = selected; if (!id || sending || !panelSupport?.conversationDelete) return;
    nodes.confirmDelete.disabled = true;
    try {
      const { deleted } = await api('conversationDelete', { conversationId: id });
      pendingMessages.delete(id); drafts.delete(id); await savePending();
      conversations = conversations.filter(chat => chat.id !== id);
      if (selected === id) { drafts.delete(''); select(''); nodes.message.value = ''; }
      conversationChoices();
      const problems = [];
      if (deleted.cliHistory.failed.length) problems.push(`Could not remove: ${deleted.cliHistory.failed.join(', ')}`);
      if (deleted.cliHistory.error) problems.push(`Agent session files were not removed: ${deleted.cliHistory.error}`);
      if (!deleted.browserSession.closed) problems.push('chrome-use could not confirm that all of its browser tabs closed; a leftover tab may remain in the AI window.');
      if (problems.length) error(`Conversation deleted. ${problems.join(' ')}`);
      await list();
    } finally { nodes.deleteConfirm.hidden = true; controls(); }
  }
  function view(agents) { nodes.chatView.hidden = agents; nodes.agentsView.hidden = !agents; nodes.chatTab.classList.toggle('selected', !agents); nodes.agentsTab.classList.toggle('selected', agents); nodes.chatTab.setAttribute('aria-pressed', String(!agents)); nodes.agentsTab.setAttribute('aria-pressed', String(agents)); }
  // The model menu is filled from the CLI's own catalog, read once per agent
  // and program; "Default model" leaves the choice to the CLI configuration.
  const modelCache = new Map();
  // The native host runs at most two CLI probes at once; Agents & models asks
  // for every chat agent, so probes wait for a free slot instead of failing.
  let inspectionsRunning = 0; const inspectionWaiters = [];
  async function inspectionSlot(task) {
    if (inspectionsRunning >= 2) await new Promise(resolve => inspectionWaiters.push(resolve));
    inspectionsRunning++;
    try { return await task(); } finally { inspectionsRunning--; inspectionWaiters.shift()?.(); }
  }
  // One CLI probe per agent and program serves both the menu and Agents & models.
  function readInspection(agentId, executable = '', force = false) {
    const key = `${agentId}\n${executable}`;
    if (force || !modelCache.has(key)) {
      const pending = inspectionSlot(() => api('agentInspect', { agentId, ...(executable ? { executable } : {}) })).then(result => result.inspection);
      pending.catch(() => { if (modelCache.get(key) === pending) modelCache.delete(key); });
      modelCache.set(key, pending);
    }
    return modelCache.get(key);
  }
  function modelOptions(models = []) {
    const keep = nodes.model.value; const first = element('option', 'Default model'); first.value = '';
    nodes.model.replaceChildren(first);
    for (const model of models) { const option = element('option', model.label || model.id); option.value = model.id; nodes.model.append(option); }
    nodes.model.value = models.some(model => model.id === keep) ? keep : '';
  }
  function clearModels() { modelEpoch++; modelOptions(); nodes.modelStatus.textContent = 'CLI model access has not been checked.'; nodes.inspectAgent.disabled = !connected || panelSupport?.mode !== 'full' || !panelSupport?.modelInspection; }
  function showInspection(inspection) {
    modelOptions(inspection.models);
    const account = { reportedSignedIn: 'CLI reports signed in', reportedSignedOut: 'CLI reports signed out', reportedMetadata: 'CLI reports account metadata', unverified: 'Account state unverified' }[inspection.account.status];
    nodes.modelStatus.textContent = `${inspection.versionReported ? `CLI ${inspection.versionReported}. ` : 'CLI version unverified. '}${inspection.models.length} models${inspection.complete ? '' : ' (partial catalog)'}. ${account}. Model execution access is unverified.`;
  }
  async function inspectModels(force = true) {
    if (panelSupport?.mode !== 'full' || !panelSupport?.modelInspection) throw new Error('CLI model inspection is unavailable for this host.');
    const epoch = ++modelEpoch, agentId = nodes.agent.value, executable = nodes.executable.value.trim();
    nodes.inspectAgent.disabled = true; nodes.modelStatus.textContent = 'Reading CLI model metadata…';
    try {
      const inspection = await readInspection(agentId, executable, force);
      if (closed || epoch !== modelEpoch || agentId !== nodes.agent.value || executable !== nodes.executable.value.trim() || inspection.agentId !== agentId) return;
      showInspection(inspection);
    } catch (failure) { if (epoch === modelEpoch) nodes.modelStatus.textContent = `Model metadata unavailable: ${failure?.message || 'the CLI did not answer'}. No fallback list was substituted; you can still enter a custom model ID.`; throw failure; }
    finally { if (epoch === modelEpoch) nodes.inspectAgent.disabled = false; }
  }
  // Read the menu automatically for a new conversation; failures stay in the
  // status line instead of interrupting the panel.
  function loadModels() {
    if (selected || !nodes.agent.value || nodes.agent.value !== preferredAgent || !connected || panelSupport?.mode !== 'full' || !panelSupport?.modelInspection) return;
    void inspectModels(false).catch(() => {});
  }
  let discoverEpoch = 0; const agentRows = new Map();
  // Suggested values for configuration fields; the inputs stay free text.
  const effortChoices = { codex: ['minimal', 'low', 'medium', 'high', 'xhigh'], claude: ['low', 'medium', 'high', 'max'] };
  const modelFields = new Set(['model', 'smallModel', 'opusModel', 'sonnetModel', 'haikuModel']);
  function choiceList(input, id, values) {
    if (!values?.length) return;
    const list = element('datalist'); list.id = id;
    list.append(...values.map(value => { const option = element('option'); option.value = value; return option; }));
    input.setAttribute('list', id); return list;
  }
  const accountLabels = { reportedSignedIn: 'Signed in', reportedSignedOut: 'Signed out', reportedMetadata: 'Account reported', unverified: 'Login unknown' };
  // One line per agent that answers "can I use this?" from the CLI's own report.
  // The report is not proof of model access; that is settled by the first turn.
  async function agentStatus(agent, line, epoch, row) {
    const mark = state => { if (row) row.className = `agent-row ${state}${configuration?.agentId === agent.id && !nodes.configuration.hidden ? ' selected' : ''}`; };
    if (!agent.executable) { mark('missing'); line.textContent = agent.configFiles?.length ? 'Configuration found; program not installed' : 'Not installed'; return; }
    mark('installed');
    if (!(panelSupport?.chatAgents || ['codex', 'claude']).includes(agent.id)) { line.textContent = 'Installed · chat not supported in the panel yet'; return; }
    if (panelSupport?.mode !== 'full' || !panelSupport?.modelInspection) { line.textContent = 'Installed'; return; }
    line.textContent = 'Installed · checking…';
    try {
      const inspection = await readInspection(agent.id);
      if (epoch !== discoverEpoch) return;
      const account = inspection.account || {};
      const parts = [inspection.versionReported ? `v${String(inspection.versionReported).replace(/^v/, '')}` : 'Installed', `${accountLabels[account.status] || 'Login unknown'}${account.status === 'reportedSignedIn' && typeof account.kind === 'string' ? ` (${account.kind})` : ''}`];
      if (inspection.models?.length) parts.push(`${inspection.models.length} models`);
      line.textContent = parts.join(' · ');
      line.classList.toggle('signed-out', account.status === 'reportedSignedOut');
      mark(account.status === 'reportedSignedOut' ? 'signed-out' : account.status === 'reportedSignedIn' ? 'ready' : 'installed');
      line.title = 'Reported by the CLI. Model access is confirmed when a conversation runs.';
    } catch (failure) { if (epoch === discoverEpoch) { line.textContent = `Installed · status unavailable${failure?.message ? `: ${failure.message}` : ''}`; } }
  }
  async function discover() {
    const epoch = ++discoverEpoch;
    const result = await api('discover'); if (epoch !== discoverEpoch) return; nodes.agentList.replaceChildren(); agentRows.clear();
    for (const agent of result.agents) {
      const row = element('div', undefined, 'agent-row'), info = element('div', undefined, 'agent-info'), line = element('div', undefined, 'agent-evidence');
      info.append(element('div', agent.name, 'agent-name'), line);
      row.append(info, button('Configure', () => loadConfig(agent.id, agent.name))); nodes.agentList.append(row); agentRows.set(agent.id, row);
      if (nodes.agentsView.hidden) line.textContent = agent.executable ? 'Installed' : 'Not installed'; else void agentStatus(agent, line, epoch, row);
    }
  }
  async function loadConfig(agentId, name) {
    const epoch = ++configEpoch; const result = await api('configRead', { agentId }); if (epoch !== configEpoch) return;
    configuration = result.configuration; configuration.name = name; preview = null; applied = null;
    nodes.configuration.hidden = false; nodes.configTitle.textContent = `${name} configuration`; nodes.configPath.textContent = configuration.path;
    nodes.configWarnings.textContent = (configuration.warnings || []).join('\n'); nodes.configFields.replaceChildren(); nodes.configPreview.hidden = true; nodes.restoreConfig.hidden = true; nodes.configResult.textContent = '';
    for (const button of nodes.configForm.querySelectorAll('button')) button.disabled = panelSupport?.mode !== 'full';
    for (const [id, row] of agentRows) row.classList.toggle('selected', id === agentId);
    const modelInputs = [];
    for (const field of configuration.fields) {
      const label = element('label', fieldLabels[field.id] || field.id); const input = element('input'); input.disabled = panelSupport?.mode !== 'full'; input.value = field.value || ''; input.placeholder = 'CLI default'; input.dataset.field = field.id;
      input.addEventListener('input', () => { input.dataset.changed = 'true'; preview = null; nodes.configPreview.hidden = true; });
      const choices = field.id === 'effort' ? choiceList(input, `config-${field.id}-choices`, effortChoices[agentId]) : null;
      if (modelFields.has(field.id)) modelInputs.push(input);
      label.append(input); if (choices) label.append(choices); if (field.validType === false) label.append(element('span', 'Stored value has an unsupported type; enter a replacement to repair it.', 'hint')); nodes.configFields.append(label);
    }
    nodes.configuration.scrollIntoView?.({ block: 'start', behavior: 'smooth' });
    // Model names come from the probe the agent list already started; the
    // form does not wait for it and no new probe is run.
    const cached = modelCache.get(`${agentId}\n`);
    if (cached && modelInputs.length) void cached.then(inspection => {
      if (epoch !== configEpoch) return;
      for (const input of modelInputs) { const list = choiceList(input, `config-${input.dataset.field}-choices`, (inspection.models || []).map(m => m.id)); if (list) input.after?.(list); }
    }, () => {});
  }
  function diffValue(value) { return value?.redacted ? 'Hidden structured or sensitive value' : value?.state === 'absent' ? 'CLI default' : typeof value?.value === 'string' ? value.value : JSON.stringify(value?.value); }
  async function previewConfig() {
    if (panelSupport?.mode !== 'full') throw new Error('Configuration changes are unavailable for this host.');
    if (!configuration) return; const agentId = configuration.agentId, epoch = configEpoch; const changes = {};
    for (const input of nodes.configFields.querySelectorAll('input')) if (input.dataset.changed) changes[input.dataset.field] = input.value.trim() || null;
    if (!Object.keys(changes).length) throw new Error('Change a field before previewing.');
    const result = await api('configPlan', { agentId, changes }); if (epoch !== configEpoch) return; preview = result.configuration; nodes.configDiff.replaceChildren();
    for (const file of preview.files) for (const change of file.changes) {
      const row = element('div', undefined, 'config-change'), values = element('div', undefined, 'values');
      values.append(element('span', diffValue(change.before), 'before'), element('span', '→', 'arrow'), element('span', diffValue(change.after), 'after'));
      row.append(element('div', fieldLabels[change.path.join('.')] || change.path.join('.'), 'field'), values); nodes.configDiff.append(row);
    }
    nodes.configPreview.hidden = false; nodes.applyConfig.disabled = !preview.files.some(f => f.changes.length); nodes.configResult.textContent = '';
  }
  async function applyConfig() {
    if (!preview) return; const plan = preview, epoch = configEpoch; nodes.applyConfig.disabled = true;
    const result = await api('configApply', { agentId: plan.agentId, planId: plan.planId }); if (epoch !== configEpoch) return;
    applied = result.configuration; preview = null; nodes.configPreview.hidden = true; nodes.restoreConfig.hidden = false; nodes.configResult.textContent = 'Configuration saved and read back. Existing conversations keep their current settings.';
  }
  async function restoreConfig() {
    if (!applied) return; const plan = applied, epoch = configEpoch; nodes.restoreConfig.disabled = true;
    try { const result = await api('configRestore', { agentId: plan.agentId, planId: plan.planId }); if (epoch !== configEpoch) return;
      nodes.configResult.textContent = result.configuration.phase === 'restoreConflicts' ? `Restored unchanged fields. Preserved later edits to: ${result.configuration.conflicts.map(p => p.join('.')).join(', ')}` : 'The original configuration was restored and read back.';
      nodes.restoreConfig.hidden = result.configuration.phase === 'restored';
    } finally { nodes.restoreConfig.disabled = false; }
  }
  async function refresh() {
    clearError(); await connectionStatus(); if (!connected) return;
    queuedNodes.clear(); nodes.queuedMessages.replaceChildren();
    if (!panelSupport) return;
    if (panelSupport.mode === 'readOnly') { await discover(); return; }
    const results = await Promise.allSettled([list(), discover()]); for (const result of results) if (result.status === 'rejected') error(result.reason);
    if (results[0].status === 'fulfilled' && selected) {
      if (!conversations.some(chat => chat.id === selected)) select('');
      else if (!conversation || conversation.id !== selected) select(selected);
    }
    await poll(); loop(); loadModels(); void loadSkills().catch(error); void loadSchedules().catch(error); void refreshPage().catch(() => {});
  }
  nodes.composer.addEventListener('submit', event => { event.preventDefault(); void send().catch(error); });
  nodes.message.addEventListener('keydown', event => { if (skillPickerKey(event)) return; if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) { event.preventDefault(); void send(event.ctrlKey || event.metaKey).catch(error); } });
  function settingsTab(section) {
    settingsSection = section;
    for (const name of ['agents','skills','schedules']) nodes[`${name}SettingsTab`].setAttribute('aria-selected',String(name===section));
    controls();
    if (section==='skills') void loadSkills().catch(error);
    if (section==='schedules') void loadSchedules().catch(error);
  }
  nodes.agentsSettingsTab.addEventListener('click',()=>settingsTab('agents'));
  nodes.skillsSettingsTab.addEventListener('click',()=>settingsTab('skills'));
  nodes.schedulesSettingsTab.addEventListener('click',()=>settingsTab('schedules'));
  async function loadSchedules() {
    if (!connected || !panelSupport?.schedules) return;
    const epoch=++schedulesEpoch;
    const result=await api('scheduleList');
    if (epoch!==schedulesEpoch) return;
    schedules=result.schedules;
    nodes.scheduleEntries.replaceChildren(...schedules.map(task => {
      const row=element('div',undefined,'skill-entry');
      row.append(element('strong',task.name),element('small',`${task.enabled?'Enabled':'Disabled'} · ${agentNames[task.agentId]} · ${task.lastResult || 'Not run yet'} · Next: ${new Date(task.nextRun).toLocaleString()}`));
      row.append(button('Edit',async()=>editSchedule(task)),button('Run now',async()=>{await api('scheduleRun',{id:task.id});await loadSchedules();await list();}),button(task.enabled?'Disable':'Enable',async()=>{await api('scheduleSave',{id:task.id,enabled:!task.enabled});await loadSchedules();}));
      if(task.lastChat) row.append(button('Open run',async()=>{await list();select(task.lastChat);view(false);}));
      if(task.lastResult==='unconfirmed') row.append(button('Acknowledge unknown outcome',async()=>{await api('scheduleSave',{id:task.id,acknowledgeUnconfirmed:true});await loadSchedules();}));
      row.append(button('Delete schedule',async()=>{await api('scheduleDelete',{id:task.id});await loadSchedules();}));
      return row;
    }));
  }
  function editSchedule(task) {
    nodes.scheduleId.value=task?.id || '';nodes.scheduleName.value=task?.name || '';nodes.schedulePrompt.value=task?.prompt || '';
    nodes.scheduleAgent.value=task?.agentId || skillAgent();nodes.scheduleModel.value=task?.model || '';nodes.scheduleEffort.value=task?.options?.effort || '';nodes.scheduleTier.value=task?.options?.serviceTier || '';
    nodes.scheduleAccess.value=task?.options?.toolAccess || (nodes.scheduleAgent.value==='opencode'?'runCommands':nodes.scheduleAgent.value==='codex'?'readFiles':'browserOnly');
    nodes.scheduleAutomatic.checked=nodes.scheduleAgent.value==='pi' || task?.fullAccess===true;nodes.scheduleAutomatic.disabled=nodes.scheduleAgent.value==='pi';nodes.scheduleEnabled.checked=task?.enabled!==false;
    nodes.scheduleKind.value=task?.rule?.kind || 'interval';nodes.scheduleTiming.value=task?.rule?.minutes || task?.rule?.time || task?.rule?.expression || '60';nodes.scheduleSkill.value=task?.skillId || '';
  }
  nodes.reloadSchedules.addEventListener('click',()=>void loadSchedules().catch(error));
  nodes.newSchedule.addEventListener('click',()=>editSchedule(null));
  nodes.scheduleAgent.addEventListener('change',()=>{nodes.scheduleEffort.value='';nodes.scheduleTier.value='';nodes.scheduleModel.value='';nodes.scheduleAccess.value=nodes.scheduleAgent.value==='opencode'?'runCommands':nodes.scheduleAgent.value==='codex'?'readFiles':'browserOnly';nodes.scheduleAutomatic.disabled=nodes.scheduleAgent.value==='pi';nodes.scheduleAutomatic.checked=nodes.scheduleAgent.value==='pi';});
  nodes.saveSchedule.addEventListener('click',()=>void (async()=>{
    nodes.saveSchedule.disabled=true;
    try {
      const kind=nodes.scheduleKind.value, value=nodes.scheduleTiming.value.trim();
      const rule={kind,...(kind==='interval'?{minutes:Number(value)}:kind==='cron'?{expression:value}:{time:value})};
      await api('scheduleSave',{...(nodes.scheduleId.value?{id:nodes.scheduleId.value}:{}),name:nodes.scheduleName.value,prompt:nodes.schedulePrompt.value,agentId:nodes.scheduleAgent.value,model:nodes.scheduleModel.value.trim() || null,rule,enabled:nodes.scheduleEnabled.checked,fullAccess:nodes.scheduleAutomatic.checked,skillId:nodes.scheduleSkill.value || null,options:{toolAccess:nodes.scheduleAccess.value,effort:nodes.scheduleEffort.value.trim() || null,serviceTier:nodes.scheduleTier.value || null}});
      editSchedule(null);await loadSchedules();
    } finally {nodes.saveSchedule.disabled=false;}
  })().catch(error));
  function skillAgent() { return conversation?.agentId || nodes.agent.value; }
  async function loadSkills() {
    if (!connected || !panelSupport?.skillLibrary) return;
    const epoch = ++skillsEpoch, agentId = skillAgent();
    const result = await api('skillList', { agentId });
    if (epoch !== skillsEpoch || agentId !== skillAgent()) return;
    skillCatalog = result.skills;
    renderSkills(); renderSkillPicker();
    const skill = nodes.scheduleSkill.value, none = element('option','No skill'); none.value='';
    nodes.scheduleSkill.replaceChildren(none,...skillCatalog.filter(s=>s.enabled).map(s=>{const node=element('option',`${s.name} · ${s.source}`);node.value=s.id;return node;}));
    nodes.scheduleSkill.value=skillCatalog.some(s=>s.id===skill)?skill:'';
  }
  function renderSkills() {
    nodes.skillEntries.replaceChildren(...skillCatalog.map(skill => {
      const row = element('div', undefined, 'skill-entry'); row.append(element('strong', skill.name), element('small', `${skill.source}${skill.enabled ? '' : ' · disabled'} · ${skill.description}`));
      row.append(button('Use', async () => { skillChoice = skill; nodes.message.value = `/${skill.name} `; view(false); renderSkillPicker(); }));
      if (skill.editable) row.append(button('Edit', async () => {
        const result = await api('skillRead', { agentId: skillAgent(), id: skill.id });
        nodes.skillName.value = skill.name; nodes.skillContent.value = result.skill.instructions;
      }), button(skill.enabled ? 'Disable' : 'Enable', async () => {
        await api('skillSetEnabled', { agentId: skillAgent(), id: skill.id, enabled: !skill.enabled }); await loadSkills();
      }));
      return row;
    }));
  }
  function chosenSkill(text) {
    const match = /^\/([^\s]+)(?:\s|$)/.exec(text);
    if (!match || !panelSupport?.skillLibrary) return null;
    if (skillChoice?.name === match[1] && skillChoice.enabled) return skillChoice;
    const matches = skillCatalog.filter(skill => skill.enabled && skill.name === match[1]);
    if (matches.length !== 1) throw new Error(matches.length ? 'Choose which skill source to use from the picker.' : 'Unknown or disabled skill. Refresh skills or choose an entry from the picker.');
    return matches[0];
  }
  function renderSkillPicker() {
    const match = /^\/([^\s]*)$/.exec(nodes.message.value);
    const skills = match ? skillCatalog.filter(skill => skill.enabled && skill.name.toLowerCase().includes(match[1].toLowerCase())).slice(0, 12) : [];
    visibleSkills = skills; skillPickerIndex = Math.min(skillPickerIndex,Math.max(0,skills.length-1));
    nodes.skillPicker.hidden = !skills.length;
    nodes.skillPicker.replaceChildren(...skills.map(skill => button(`${skill.name} · ${skill.source} · ${skill.description}`, async () => {
      skillChoice = skill; nodes.message.value = `/${skill.name} `; nodes.skillPicker.hidden = true; nodes.message.focus?.();
    })));
  }
  function skillPickerKey(event) {
    if (event.isComposing || nodes.skillPicker.hidden) return false;
    if (event.key === 'Escape') { nodes.skillPicker.hidden = true; event.preventDefault(); return true; }
    if (['ArrowDown','ArrowUp'].includes(event.key)) {
      event.preventDefault(); skillPickerIndex = (skillPickerIndex + (event.key==='ArrowDown'?1:-1) + visibleSkills.length) % visibleSkills.length;
      for (const [index,node] of [...nodes.skillPicker.children].entries()) { node.classList.toggle('selected',index===skillPickerIndex); node.setAttribute('aria-selected',String(index===skillPickerIndex)); if(index===skillPickerIndex)node.scrollIntoView?.({block:'nearest'}); }
      return true;
    }
    if (['Tab','Enter'].includes(event.key) && !event.ctrlKey && !event.metaKey) {
      const skill = visibleSkills[skillPickerIndex]; if(!skill)return false;
      event.preventDefault(); skillChoice=skill; nodes.message.value=`/${skill.name} `;nodes.skillPicker.hidden=true;return true;
    }
    return false;
  }
  nodes.message.addEventListener('input', () => {skillPickerIndex=0;renderSkillPicker();});
  nodes.reloadSkills.addEventListener('click', () => void loadSkills().catch(error));
  nodes.saveSkill.addEventListener('click', () => void (async () => {
    await api('skillSave', { agentId: skillAgent(), name: nodes.skillName.value.trim(), content: nodes.skillContent.value }); await loadSkills();
  })().catch(error));
  nodes.importSkill.addEventListener('click', () => void (async () => {
    nodes.importSkill.disabled = true;
    try { await api('skillImport', { agentId: skillAgent(), kind: nodes.skillImportKind.value, source: nodes.skillImportSource.value.trim() }); await loadSkills(); }
    finally { nodes.importSkill.disabled = false; }
  })().catch(error));
  function renderImageDrafts() {
    const images = imageDrafts.get(selected) || [];
    nodes.attachmentDrafts.hidden = !images.length;
    nodes.attachmentDrafts.replaceChildren(...images.map((image, index) => {
      const item = element('div'); const preview = element('img'); preview.alt = image.name; preview.src = `data:${image.mediaType};base64,${image.data}`;
      item.append(preview, button('Remove image', async () => { images.splice(index, 1); renderImageDrafts(); })); return item;
    }));
  }
  async function addImages(files) {
    if (!panelSupport?.imageAttachments || sending || convertingImages) return;
    const id = selected, list = imageDrafts.get(id) || [];
    if (list.length + files.length > 5) throw new Error('Attach up to five images per message.');
    convertingImages = true; controls();
    try {
      const added = []; for (const file of files) added.push(await encodeImage(file));
      imageDrafts.set(id, [...list, ...added]); if (id === selected) renderImageDrafts();
    } finally { convertingImages = false; nodes.imageFiles.value = ''; controls(); }
  }
  nodes.attachImages.addEventListener('click', () => nodes.imageFiles.click());
  nodes.imageFiles.addEventListener('change', () => void addImages([...nodes.imageFiles.files]).catch(error));
  nodes.message.addEventListener('paste', event => {
    const files = [...(event.clipboardData?.files || [])].filter(file => file.type.startsWith('image/'));
    if (files.length && panelSupport?.imageAttachments) { event.preventDefault(); void addImages(files).catch(error); }
  });
  nodes.message.addEventListener('dragover', event => { if (panelSupport?.imageAttachments) event.preventDefault(); });
  nodes.message.addEventListener('drop', event => { if (panelSupport?.imageAttachments) { event.preventDefault(); void addImages([...(event.dataTransfer?.files || [])]).catch(error); } });
  nodes.sendDirect.addEventListener('click' , () => void send(true).catch(error));
  nodes.conversation.addEventListener('change', () => select(nodes.conversation.value));
  nodes.conversationButton.addEventListener('click', () => {
    if (sending) return;
    const open = nodes.conversationMenu.hidden; renderConversationMenu();
    nodes.conversationMenu.hidden = !open; nodes.conversationButton.setAttribute('aria-expanded', String(open));
  });
  nodes.scopeSite.addEventListener('click', () => setScope('site'));
  nodes.scopeAll.addEventListener('click', () => setScope('all'));
  if (document.addEventListener) {
    document.addEventListener('keydown', event => { if (event.key === 'Escape') closeConversationMenu(); });
    document.addEventListener('click', event => { if (!nodes.conversationMenu.hidden && !nodes.conversationMenu.contains(event.target) && !nodes.conversationButton.contains(event.target)) closeConversationMenu(); });
  }
  // Example tasks only fill the composer; the user still decides to send.
  suggestions?.addEventListener('click', event => {
    const prompt = event.target?.closest?.('[data-prompt]')?.dataset.prompt;
    if (!prompt || nodes.message.disabled) return;
    nodes.message.value = prompt; nodes.message.focus?.();
  });
  nodes.newChat.addEventListener('click', () => { nodes.conversation.value = ''; select(''); loadModels(); });
  nodes.refresh.addEventListener('click', () => void refresh().catch(error));
  nodes.pageDetach.addEventListener('click', () => { pageDetached = true; renderPage(); });
  if (tabs?.onActivated) tabs.onActivated.addListener(() => void refreshPage().catch(() => {}));
  if (tabs?.onUpdated) tabs.onUpdated.addListener((id, change) => { if (change.title || change.url || change.status === 'complete') void refreshPage().catch(() => {}); });
  function readTurnOptions() {
    return { toolAccess: nodes.turnAccess.value || null, effort: ['codex','claude'].includes(conversation?.agentId || nodes.agent.value) ? nodes.turnEffort.value || null : null, serviceTier: (conversation?.agentId || nodes.agent.value) === 'codex' ? nodes.turnTier.value || null : null };
  }
  nodes.saveTurnOptions.addEventListener('click', () => void (async () => {
    const id = selected, epoch = selectionEpoch;
    if (!id || nodes.saveTurnOptions.disabled) return;
    nodes.saveTurnOptions.disabled = true;
    try {
      const result = await api('conversationSetOptions', { conversationId: id, options: readTurnOptions() });
      if (selected === id && selectionEpoch === epoch) { conversation = result.conversation; optionsOwner = null; controls(); }
    } finally { controls(); }
  })().catch(error));
  nodes.access.addEventListener('click' , () => void setAccess(!(selected ? conversation?.fullAccess === true : newFullAccess)).catch(error));
  nodes.deleteChat.addEventListener('click', () => { nodes.deleteConfirm.hidden = false; });
  nodes.cancelDelete.addEventListener('click', () => { nodes.deleteConfirm.hidden = true; });
  nodes.confirmDelete.addEventListener('click', () => void deleteConversation().catch(error));
  nodes.inspectAgent.addEventListener('click', () => void inspectModels(true).catch(error));
  nodes.agent.addEventListener('change', () => { rememberAgent(nodes.agent.value); clearModels(); loadModels(); skillChoice = null; void loadSkills().catch(error); controls(); });
  nodes.executable.addEventListener('change', () => { clearModels(); loadModels(); });
  nodes.discover.addEventListener('click', () => void discover().catch(error));
  nodes.settings.addEventListener('click', () => runtime?.openOptionsPage?.());
  nodes.chatTab.addEventListener('click', () => view(false)); nodes.agentsTab.addEventListener('click', () => { view(true); if (connected && panelSupport) void discover().catch(error); });
  async function controlConversation(operation) {
    const id = selected, epoch = selectionEpoch; if (!id || !connected) return;
    const result = await api(operation, { conversationId: id });
    if (id === selected && epoch === selectionEpoch) { conversation = result.conversation; controls(); }
  }
  nodes.stop.addEventListener('click', () => void controlConversation('conversationStop').catch(error));
  nodes.resume.addEventListener('click', () => void controlConversation('conversationResumeQueue').catch(error));
  nodes.latest.addEventListener('click', () => { nodes.transcript.scrollTop = nodes.transcript.scrollHeight; nodes.latest.hidden = true; });
  nodes.configForm.addEventListener('submit', event => { event.preventDefault(); void previewConfig().catch(error); });
  nodes.applyConfig.addEventListener('click', () => void applyConfig().catch(error));
  nodes.restoreConfig.addEventListener('click', () => void restoreConfig().catch(error));
  nodes.discardConfig.addEventListener('click', () => { preview = null; nodes.configPreview.hidden = true; });
  if (runtime?.onMessage) runtime.onMessage.addListener(message => {
    if (message?.type === 'ab-host-state') {
      const wasUsable = connected && Boolean(panelSupport); connectionEpoch++; renderConnection(message);
      if (connected && panelSupport && !wasUsable) void refresh().catch(error);
      else if (connected) void poll().catch(error);
    }
  });
  controls(); closeConversationMenu();
  return { async start() {
    try {
      const saved = await storage?.local?.get(['ab_panel_conversation', 'ab_panel_pending', 'ab_panel_full_access', 'ab_panel_conversation_scope', 'ab_panel_agent']);
      if (chatAgents.has(saved?.ab_panel_agent)) preferredAgent = saved.ab_panel_agent;
      if (saved?.ab_panel_conversation_scope === 'all') scope = 'all';
      selected = saved?.ab_panel_conversation || '';
      if (saved?.ab_panel_full_access === false) newFullAccess = false;
      for (const [id, messageId] of Object.entries(saved?.ab_panel_pending || {}).slice(0, 64)) {
        if (typeof messageId === 'string' && messageId.length <= 80) pendingMessages.set(id, { id: messageId });
      }
      await refresh();
    } catch (failure) { error(failure); }
  }, refresh, select, send, timeline: () => timeline, close() { closed = true; modelEpoch++; discoverEpoch++; skillsEpoch++; schedulesEpoch++; cancel(timer); selectionEpoch++; configEpoch++; connectionEpoch++; } };
}

if (typeof document !== 'undefined') {
  const panel = createPanel({ document, runtime: globalThis.chrome?.runtime, storage: globalThis.chrome?.storage, tabs: globalThis.chrome?.tabs });
  void panel.start();
  globalThis.addEventListener('pagehide', () => panel.close());
}
