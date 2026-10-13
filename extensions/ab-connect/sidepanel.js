import { PanelTimeline, phaseLabels } from './panel-state.js';

const busyPhases = new Set(['starting', 'running', 'waitingInput', 'stopping']);
const agentNames = { codex: 'Codex', claude: 'Claude Code' };
const fieldLabels = { model: 'Model', provider: 'Provider', effort: 'Reasoning effort', serviceTier: 'Service tier', smallModel: 'Small model', opusModel: 'Opus model', sonnetModel: 'Sonnet model', haikuModel: 'Haiku model' };

export function createPanel({ document, runtime, storage, schedule = setTimeout, cancel = clearTimeout }) {
  const ids = ['pendingRecovery', 'retryPending', 'clearPending', 'connection', 'notice', 'refresh', 'deleteChat', 'deleteConfirm', 'confirmDelete', 'cancelDelete', 'settings', 'chatTab', 'agentsTab', 'chatView', 'agentsView', 'conversation', 'newChat', 'newOptions', 'access', 'accessLabel', 'modelChip', 'agent', 'model', 'modelList', 'modelStatus', 'inspectAgent', 'executable', 'status', 'phase', 'effectiveModel', 'transcript', 'empty', 'latest', 'requests', 'composer', 'message', 'queue', 'queuedMessages', 'resume', 'stop', 'sendDirect', 'send', 'discover', 'agentList', 'configuration', 'configTitle', 'configPath', 'configWarnings', 'configForm', 'configFields', 'configPreview', 'configDiff', 'applyConfig', 'discardConfig', 'restoreConfig', 'configResult'];
  const nodes = Object.fromEntries(ids.map(id => [id, document.getElementById(id)]));
  let selected = '', selectionEpoch = 0, connectionEpoch = 0, connected = false, closed = false, panelSupport = null;
  let timeline = new PanelTimeline(), conversations = [], conversation = null, timer = null, polling = false;
  const pendingMessages = new Map(), drafts = new Map();
  let modelEpoch = 0, newFullAccess = true;
  let sending = false, configuration = null, configEpoch = 0, preview = null, applied = null;
  const rendered = new Map(), requestNodes = new Map(), queuedNodes = new Map();

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
    nodes.conversation.disabled = sending || !writable; nodes.newChat.disabled = sending || !writable;
    nodes.message.disabled = !writable; nodes.inspectAgent.disabled = !connected || !writable || !panelSupport?.modelInspection;
    nodes.agent.disabled = !writable; nodes.model.disabled = !writable; nodes.executable.disabled = !writable;
    nodes.newOptions.hidden = Boolean(selected); nodes.modelChip.hidden = Boolean(selected);
    const full = selected ? conversation?.fullAccess === true : newFullAccess;
    nodes.access.hidden = !writable || !panelSupport?.fullAccess;
    nodes.access.disabled = !connected || sending || (Boolean(selected) && !conversation);
    nodes.accessLabel.textContent = full ? 'Full access' : 'Ask first';
    nodes.access.setAttribute('aria-pressed', String(full));
    nodes.access.title = full ? 'Runs browser tools, commands and file changes without asking. Click to ask first.' : 'Asks before each tool, command or file change. Click for full access.';
    nodes.deleteChat.hidden = !selected || !writable || !panelSupport?.conversationDelete;
    nodes.deleteChat.disabled = sending || !connected || busyPhases.has(conversation?.phase);
    if (nodes.deleteChat.hidden) nodes.deleteConfirm.hidden = true;
    nodes.confirmDelete.disabled = nodes.deleteChat.disabled;
    nodes.phase.textContent = panelSupport?.mode === 'readOnly' ? 'Read-only discovery' : conversation ? phaseLabels[conversation.phase] || 'Unknown response state' : 'Ready';
    nodes.effectiveModel.textContent = conversation?.model || '';
    const busy = busyPhases.has(conversation?.phase);
    nodes.status.classList.toggle('idle', !conversation || (conversation.phase === 'idle' && !conversation.model));
    nodes.status.classList.toggle('busy', busy);
    nodes.queue.textContent = pending ? 'Send unconfirmed; checking stored message' : conversation?.queued ? `${conversation.queued} queued${conversation.queuePaused ? ', paused' : ''}` : '';
    nodes.resume.hidden = !conversation?.queuePaused || !conversation?.queued;
    nodes.resume.disabled = !connected || !writable || conversation?.phase === 'recoveryRequired';
    nodes.stop.disabled = !connected || !writable || !busy || conversation?.phase === 'stopping';
    nodes.stop.hidden = !busy;
    nodes.send.disabled = !connected || !writable || sending || Boolean(pending) || conversation?.phase === 'recoveryRequired';
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
  }
  function select(id, duringCreate = false) {
    if (sending && !duringCreate) { nodes.conversation.value = selected; return; }
    drafts.set(selected, nodes.message.value);
    nodes.message.value = drafts.get(id) || '';
    nodes.conversation.value = id;
    nodes.deleteConfirm.hidden = true;
    selected = id; selectionEpoch++; conversation = conversations.find(c => c.id === id) || null;
    timeline = new PanelTimeline(); queuedNodes.clear(); nodes.queuedMessages.replaceChildren(); nodes.queuedMessages.hidden = true; rendered.clear(); requestNodes.clear(); nodes.requests.replaceChildren();
    nodes.transcript.replaceChildren(nodes.empty); nodes.empty.hidden = false; controls();
    storage?.local?.set({ ab_panel_conversation: id }).catch(() => {});
    void poll().catch(error);
  }
  async function list() {
    const result = await api('conversationList'); conversations = result.conversations; conversationChoices();
  }
  function renderTimeline() {
    const follow = nodes.transcript.scrollHeight - nodes.transcript.scrollTop - nodes.transcript.clientHeight < 80;
    nodes.empty.hidden = timeline.rows.length > 0;
    for (const row of timeline.rows) {
      let view = rendered.get(row.key);
      if (!view) {
        if (row.type === 'tool') {
          const container = element('details', undefined, 'tool-row'); const title = element('summary');
          const name = element('span'); const state = element('span', undefined, 'tool-state'); title.append(name, state);
          const content = element('pre'); container.append(title, content); view = { container, name, state, content };
        } else {
          const container = element('article', undefined, `message-row ${row.type}`);
          const speaker = element('div', row.type === 'user' ? 'You' : row.type === 'assistant' ? 'Assistant' : 'Response status', 'speaker');
          const content = element('div', undefined, 'content'); container.append(content, speaker); view = { container, content, speaker };
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
        rendered.set(row.key, view); nodes.transcript.append(view.container);
      }
      if (view.content.textContent !== row.text) view.content.textContent = row.text;
      if (row.type === 'user') view.speaker.textContent = row.queueStatus || '';
      if (row.type === 'tool') { view.name.textContent = row.name; view.state.textContent = row.failed ? 'Error' : row.completed ? '' : 'Running…'; view.container.classList.toggle('failed', row.failed); }
    }
    if (follow) nodes.transcript.scrollTop = nodes.transcript.scrollHeight;
    nodes.latest.hidden = follow || timeline.rows.length === 0;
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
          if (id === selected && epoch === selectionEpoch) { if (nodes.message.value === pending.text) nodes.message.value = ''; controls(); }
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
      try { await poll(); } catch (failure) { error(failure); }
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
        await api(pending.direct ? 'conversationSendDirect' : 'conversationSend', {conversationId:id,messageId:pending.id,text:pending.text});
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
    if (!connected || sending || pendingMessages.has(selected) || conversation?.phase === 'recoveryRequired' || !nodes.message.value.trim()) return;
    if (direct && conversation?.directMessageId) throw new Error('A direct message is already waiting for stop confirmation.');
    sending = true; clearError(); controls(); const text = nodes.message.value;
    try {
      if (!selected) {
        const options = { agentId: nodes.agent.value };
        if (nodes.model.value.trim()) options.model = nodes.model.value.trim();
        if (nodes.executable.value.trim()) options.executable = nodes.executable.value.trim();
        if (panelSupport?.fullAccess) options.fullAccess = newFullAccess;
        const result = await api('conversationCreate', options); conversations.unshift(result.conversation); conversationChoices(); select(result.conversation.id, true); nodes.message.value = text;
      }
      const id = selected, pending = { id: crypto.randomUUID(), text, direct }; pendingMessages.set(id, pending); controls();
      try { await savePending(); } catch (failure) { pendingMessages.delete(id); throw failure; }
      const result = await api(direct ? 'conversationSendDirect' : 'conversationSend', { conversationId: id, messageId: pending.id, text });
      if (selected === id) { conversation = result.conversation; if (nodes.message.value === text) nodes.message.value = ''; }
      pendingMessages.delete(id); drafts.delete(id); await savePending(); await list(); await poll();
    } finally { sending = false; controls(); }
  }
  // A conversation keeps the model it was created with, so a model the account
  // cannot use is fixed by starting a new conversation on the CLI default.
  async function retryWithDefaultModel(agentId, prompt) {
    if (sending) return;
    select(''); nodes.agent.value = agentId; nodes.model.value = ''; clearModels(); nodes.message.value = prompt;
    await send();
  }
  function showAgents() { view(true); }
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
      if (!deleted.browserSession.closed) problems.push(`Browser tabs were not closed: ${deleted.browserSession.error || 'unconfirmed'}`);
      if (problems.length) error(`Conversation deleted. ${problems.join(' ')}`);
      await list();
    } finally { nodes.deleteConfirm.hidden = true; controls(); }
  }
  function view(agents) { nodes.chatView.hidden = agents; nodes.agentsView.hidden = !agents; nodes.chatTab.classList.toggle('selected', !agents); nodes.agentsTab.classList.toggle('selected', agents); nodes.chatTab.setAttribute('aria-pressed', String(!agents)); nodes.agentsTab.setAttribute('aria-pressed', String(agents)); }
  function clearModels() { modelEpoch++; nodes.modelList.replaceChildren(); nodes.modelStatus.textContent = 'CLI model access has not been checked.'; nodes.inspectAgent.disabled = !connected || panelSupport?.mode !== 'full' || !panelSupport?.modelInspection; }
  async function inspectModels() {
    if (panelSupport?.mode !== 'full' || !panelSupport?.modelInspection) throw new Error('CLI model inspection is unavailable for this host.');
    const epoch = ++modelEpoch, agentId = nodes.agent.value, executable = nodes.executable.value.trim();
    nodes.inspectAgent.disabled = true; nodes.modelList.replaceChildren(); nodes.modelStatus.textContent = 'Reading CLI model metadata…';
    try {
      const result = await api('agentInspect', { agentId, ...(executable ? { executable } : {}) });
      if (closed || epoch !== modelEpoch || agentId !== nodes.agent.value || executable !== nodes.executable.value.trim()) return;
      const inspection = result.inspection;
      for (const model of inspection.models) { const option = element('option', `${model.label}${model.efforts.length ? ` · effort: ${model.efforts.join(', ')}` : ''}`); option.value = model.id; nodes.modelList.append(option); }
      const account = { reportedSignedIn: 'CLI reports signed in', reportedSignedOut: 'CLI reports signed out', reportedMetadata: 'CLI reports account metadata', unverified: 'Account state unverified' }[inspection.account.status];
      nodes.modelStatus.textContent = `${inspection.versionReported ? `CLI ${inspection.versionReported}. ` : 'CLI version unverified. '}${inspection.models.length} models${inspection.complete ? '' : ' (partial catalog)'}. ${account}. Model execution access is unverified.`;
    } catch (failure) { if (epoch === modelEpoch) nodes.modelStatus.textContent = 'Model metadata unavailable. No fallback list was substituted; you can still enter a model ID.'; throw failure; }
    finally { if (epoch === modelEpoch) nodes.inspectAgent.disabled = false; }
  }
  let discoverEpoch = 0;
  const accountLabels = { reportedSignedIn: 'Signed in', reportedSignedOut: 'Signed out', reportedMetadata: 'Account reported', unverified: 'Login unknown' };
  // One line per agent that answers "can I use this?" from the CLI's own report.
  // The report is not proof of model access; that is settled by the first turn.
  async function agentStatus(agent, line, epoch) {
    if (!agent.executable) { line.textContent = agent.configFiles?.length ? 'Configuration found; program not installed' : 'Not installed'; return; }
    if (panelSupport?.mode !== 'full' || !panelSupport?.modelInspection) { line.textContent = 'Installed'; return; }
    line.textContent = 'Installed · checking…';
    try {
      const { inspection } = await api('agentInspect', { agentId: agent.id });
      if (epoch !== discoverEpoch) return;
      const account = inspection.account || {};
      const parts = [inspection.versionReported ? `v${String(inspection.versionReported).replace(/^v/, '')}` : 'Installed', `${accountLabels[account.status] || 'Login unknown'}${account.status === 'reportedSignedIn' && typeof account.kind === 'string' ? ` (${account.kind})` : ''}`];
      if (inspection.models?.length) parts.push(`${inspection.models.length} models`);
      line.textContent = parts.join(' · ');
      line.classList.toggle('signed-out', account.status === 'reportedSignedOut');
      line.title = 'Reported by the CLI. Model access is confirmed when a conversation runs.';
    } catch { if (epoch === discoverEpoch) line.textContent = 'Installed · status unavailable'; }
  }
  async function discover() {
    const epoch = ++discoverEpoch;
    const result = await api('discover'); if (epoch !== discoverEpoch) return; nodes.agentList.replaceChildren();
    for (const agent of result.agents) {
      const row = element('div', undefined, 'agent-row'), info = element('div', undefined, 'agent-info'), line = element('div', undefined, 'agent-evidence');
      info.append(element('div', agent.name, 'agent-name'), line);
      row.append(info, button('Configure', () => loadConfig(agent.id, agent.name))); nodes.agentList.append(row);
      if (nodes.agentsView.hidden) line.textContent = agent.executable ? 'Installed' : 'Not installed'; else void agentStatus(agent, line, epoch);
    }
  }
  async function loadConfig(agentId, name) {
    const epoch = ++configEpoch; const result = await api('configRead', { agentId }); if (epoch !== configEpoch) return;
    configuration = result.configuration; configuration.name = name; preview = null; applied = null;
    nodes.configuration.hidden = false; nodes.configTitle.textContent = `${name} configuration`; nodes.configPath.textContent = configuration.path;
    nodes.configWarnings.textContent = (configuration.warnings || []).join('\n'); nodes.configFields.replaceChildren(); nodes.configPreview.hidden = true; nodes.restoreConfig.hidden = true; nodes.configResult.textContent = '';
    for (const button of nodes.configForm.querySelectorAll('button')) button.disabled = panelSupport?.mode !== 'full';
    for (const field of configuration.fields) {
      const label = element('label', fieldLabels[field.id] || field.id); const input = element('input'); input.disabled = panelSupport?.mode !== 'full'; input.value = field.value || ''; input.placeholder = 'CLI default'; input.dataset.field = field.id;
      input.addEventListener('input', () => { input.dataset.changed = 'true'; preview = null; nodes.configPreview.hidden = true; });
      label.append(input); if (field.validType === false) label.append(element('span', 'Stored value has an unsupported type; enter a replacement to repair it.', 'hint')); nodes.configFields.append(label);
    }
  }
  function diffValue(value) { return value?.redacted ? 'Hidden structured or sensitive value' : value?.state === 'absent' ? 'CLI default' : typeof value?.value === 'string' ? value.value : JSON.stringify(value?.value); }
  async function previewConfig() {
    if (panelSupport?.mode !== 'full') throw new Error('Configuration changes are unavailable for this host.');
    if (!configuration) return; const agentId = configuration.agentId, epoch = configEpoch; const changes = {};
    for (const input of nodes.configFields.querySelectorAll('input')) if (input.dataset.changed) changes[input.dataset.field] = input.value.trim() || null;
    if (!Object.keys(changes).length) throw new Error('Change a field before previewing.');
    const result = await api('configPlan', { agentId, changes }); if (epoch !== configEpoch) return; preview = result.configuration; nodes.configDiff.replaceChildren();
    for (const file of preview.files) for (const change of file.changes) { const row = element('div', undefined, 'config-change'); row.append(element('div', change.path.join('.'), 'field'), element('div', `Before: ${diffValue(change.before)}`, 'before'), element('div', `After: ${diffValue(change.after)}`, 'after')); nodes.configDiff.append(row); }
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
    await poll(); loop();
  }
  nodes.composer.addEventListener('submit', event => { event.preventDefault(); void send().catch(error); });
  nodes.message.addEventListener('keydown', event => { if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) { event.preventDefault(); void send(event.ctrlKey || event.metaKey).catch(error); } });
  nodes.sendDirect.addEventListener('click', () => void send(true).catch(error));
  nodes.conversation.addEventListener('change', () => select(nodes.conversation.value));
  nodes.newChat.addEventListener('click', () => { nodes.conversation.value = ''; select(''); });
  nodes.refresh.addEventListener('click', () => void refresh().catch(error));
  nodes.access.addEventListener('click', () => void setAccess(!(selected ? conversation?.fullAccess === true : newFullAccess)).catch(error));
  nodes.deleteChat.addEventListener('click', () => { nodes.deleteConfirm.hidden = false; });
  nodes.cancelDelete.addEventListener('click', () => { nodes.deleteConfirm.hidden = true; });
  nodes.confirmDelete.addEventListener('click', () => void deleteConversation().catch(error));
  nodes.inspectAgent.addEventListener('click', () => void inspectModels().catch(error));
  nodes.agent.addEventListener('change', () => { clearModels(); nodes.model.value = ''; });
  nodes.executable.addEventListener('input', clearModels);
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
  controls();
  return { async start() {
    try {
      const saved = await storage?.local?.get(['ab_panel_conversation', 'ab_panel_pending', 'ab_panel_full_access']);
      selected = saved?.ab_panel_conversation || '';
      if (saved?.ab_panel_full_access === false) newFullAccess = false;
      for (const [id, messageId] of Object.entries(saved?.ab_panel_pending || {}).slice(0, 64)) {
        if (typeof messageId === 'string' && messageId.length <= 80) pendingMessages.set(id, { id: messageId });
      }
      await refresh();
    } catch (failure) { error(failure); }
  }, refresh, select, send, timeline: () => timeline, close() { closed = true; modelEpoch++; discoverEpoch++; cancel(timer); selectionEpoch++; configEpoch++; connectionEpoch++; } };
}

if (typeof document !== 'undefined') {
  const panel = createPanel({ document, runtime: globalThis.chrome?.runtime, storage: globalThis.chrome?.storage });
  void panel.start();
  globalThis.addEventListener('pagehide', () => panel.close());
}
