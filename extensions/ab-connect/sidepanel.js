import { PanelTimeline, phaseLabels, assessmentLabel, outcomeLabel, stepVerdict, verdictLabels, stepActions, actingPages } from './panel-state.js';
import { heifConverter, isHeif, prepareImage } from './image-attachment.js';
import { renderMarkdown } from './markdown.js';
import { t, messageMap, applyI18n } from './i18n.js';

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
  if (seconds < 60) return t('panel_time_just_now');
  if (seconds < 3600) return t('panel_time_minutes_ago', Math.floor(seconds / 60));
  if (seconds < 86400) return t('panel_time_hours_ago', Math.floor(seconds / 3600));
  return t('panel_time_days_ago', Math.floor(seconds / 86400));
}
const agentNames = { codex: 'Codex', claude: 'Claude Code', opencode: 'OpenCode', pi: 'Pi' };
const chatAgents = new Set(Object.keys(agentNames));
const fieldLabels = messageMap({ model: 'panel_field_model', provider: 'panel_field_provider', effort: 'panel_field_effort', serviceTier: 'panel_field_service_tier', smallModel: 'panel_field_small_model', opusModel: 'panel_field_opus_model', sonnetModel: 'panel_field_sonnet_model', haikuModel: 'panel_field_haiku_model' });

// What `control` means is this conversation's agent process only; nothing
// here says the user has the browser to themselves.
export const controlLabels = messageMap({ agent: 'panel_control_agent', stopRequested: 'panel_control_stop_requested', stopped: 'panel_control_stopped', resuming: 'panel_control_resuming', unknown: 'panel_control_unknown' });
// Step verdicts arrive as English values from panel-state.js; they key the
// chip's class and are translated only for display.
const verdictClass = { Confirmed: 'confirmed', Failed: 'failed', 'Not checked': 'unchecked', 'No browser record': 'missing', 'Page not recorded': 'missing' };
const computedLabels = messageMap({ checked: 'panel_check_checked', checkFailed: 'panel_check_failed', stale: 'panel_check_stale', claimed: 'panel_check_claimed', unmet: 'panel_check_unmet', unknown: 'panel_check_unknown' });
const stallText = { repeatedIdenticalResult: n => n ? t('panel_stall_repeated_result_times', n) : t('panel_stall_repeated_result'), repeatedFailure: n => n ? t('panel_stall_repeated_failure_times', n) : t('panel_stall_repeated_failure'), noActivity: () => t('panel_stall_no_activity'), nativeNoProgress: () => t('panel_stall_no_progress') };
const unavailableText = messageMap({ cancelled: 'panel_unavailable_cancelled', interrupted: 'panel_unavailable_interrupted', timeout: 'panel_unavailable_timeout', failed: 'panel_unavailable_failed', tooLargeToRetain: 'panel_unavailable_too_large', quotaExceeded: 'panel_unavailable_quota', pathMismatch: 'panel_unavailable_path_mismatch', notRegularFile: 'panel_unavailable_not_regular', notOwned: 'panel_unavailable_not_owned', linked: 'panel_unavailable_linked', missing: 'panel_unavailable_missing', changed: 'panel_unavailable_changed', encoderMissing: 'panel_unavailable_encoder_missing', encoderFailed: 'panel_unavailable_encoder_failed' });
const ARTIFACT_CHUNK = 384 * 1024;
function hostOf(url) { try { return new URL(url).host; } catch { return ''; } }
function pageName(target) { const host = hostOf(target?.url); let path = ''; try { path = new URL(target.url).pathname; } catch {} return target?.title ? `${target.title}${host ? ` · ${host}` : ''}` : `${host}${path === '/' ? '' : path}` || t('panel_page_fallback'); }
function sizeText(bytes) { return !Number.isFinite(bytes) ? '' : bytes < 1024 ? `${bytes} B` : bytes < 1048576 ? `${(bytes / 1024).toFixed(1)} KB` : `${(bytes / 1048576).toFixed(1)} MB`; }
function base64Bytes(data) { const text = atob(data); const bytes = new Uint8Array(text.length); for (let i = 0; i < text.length; i++) bytes[i] = text.charCodeAt(i); return bytes; }

export function createPanel({ document, runtime, storage, tabs, windows, schedule = setTimeout, cancel = clearTimeout, encodeImage = prepareImage, objectUrls = { create: blob => URL.createObjectURL(blob), revoke: url => URL.revokeObjectURL(url) }, subtle = globalThis.crypto?.subtle }) {
  const ids = ['agentsSettingsTab', 'skillsSettingsTab', 'schedulesSettingsTab', 'localAgents', 'scheduleSettings', 'reloadSchedules', 'scheduleEntries', 'scheduleId', 'scheduleName', 'scheduleAgent', 'scheduleModel', 'scheduleEffort', 'scheduleTier', 'scheduleAccess', 'scheduleAutomatic', 'scheduleKind', 'scheduleTiming', 'scheduleSkill', 'schedulePrompt', 'scheduleEnabled', 'saveSchedule', 'newSchedule', 'skillPicker', 'skillLibrary', 'reloadSkills', 'skillEntries', 'skillName', 'skillContent', 'saveSkill', 'skillImportKind', 'skillImportSource', 'importSkill', 'attachmentDrafts', 'imageFiles', 'attachImages', 'turnAccess', 'turnEffortLabel', 'turnOptions', 'turnEffort', 'turnTier', 'turnTierLabel', 'saveTurnOptions', 'pendingRecovery', 'retryPending', 'clearPending', 'connection', 'notice', 'refresh', 'deleteChat', 'deleteConfirm', 'confirmDelete', 'cancelDelete', 'settings', 'chatTab', 'agentsTab', 'chatView', 'agentsView', 'conversation', 'conversationButton', 'conversationTitle', 'conversationMenu', 'scopeSite', 'scopeAll', 'conversationItems', 'newChat', 'pageChip', 'pageTitle', 'pageDetach', 'newOptions', 'access', 'accessLabel', 'modelChip', 'agent', 'model', 'customModel', 'modelStatus', 'inspectAgent', 'executable', 'status', 'phase', 'control', 'effectiveModel', 'transcript', 'empty', 'latest', 'requests', 'composer', 'message', 'queue', 'queuedMessages', 'resume', 'stop', 'sendDirect', 'send', 'recordTask', 'recordTaskLabel', 'discover', 'agentList', 'configuration', 'configTitle', 'configPath', 'configWarnings', 'configForm', 'configFields', 'configPreview', 'configDiff', 'applyConfig', 'discardConfig', 'restoreConfig', 'configResult'];
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
  // Turn view: one card per user message, used only when the host reports
  // turn evidence. Cards are keyed by the projected turn object.
  const turnCards = new Map(), liveUrls = new Set(), dismissedStalls = new Set();
  let viewMode = null, forkView = null;
  const turnView = () => Boolean(panelSupport?.turnEvidence);

  // Keep only correlation IDs in extension storage. Chat text lives in the
  // native journal; closing the view must not turn an uncertain send into a retry.
  async function savePending() {
    if (!storage?.local?.set) throw new Error(t('panel_error_pending_storage'));
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
    if (!runtime?.sendMessage) throw new Error(t('panel_error_open_from_extension'));
    const reply = await runtime.sendMessage({ type: 'ab-agent-request', operation, params });
    if (!reply?.success) throw new Error(reply?.error || t('panel_error_host_unconfirmed'));
    return reply.result;
  }
  const convertHeif = heifConverter(api);
  function renderConnection(state) {
    connected = state?.connected === true;
    panelSupport = state?.agentPanel?.schemaVersion === 1 ? state.agentPanel : null;
    nodes.connection.classList.toggle('disconnected', !connected);
    nodes.connection.classList.toggle('ok', connected && panelSupport?.mode === 'full');
    nodes.connection.textContent = connected ? t('panel_connection_connected') : state?.connectionState === 'connecting' ? t('panel_connection_waiting') : state?.connectionError || t('panel_connection_unavailable');
    if (connected && !panelSupport) nodes.connection.textContent = t('panel_connection_update_cli');
    if (connected && panelSupport?.mode === 'readOnly') nodes.connection.textContent = t('panel_connection_read_only');
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
    if (!runtime?.sendMessage) { renderConnection(null); throw new Error(t('panel_error_open_from_extension')); }
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
      const first = element('option', t('panel_cli_default')); first.value = '';
      const efforts = optionAgent === 'codex' ? ['minimal','low','medium','high','xhigh'] : optionAgent === 'claude' ? ['low','medium','high','max'] : [];
      nodes.turnEffort.replaceChildren(first, ...efforts.map(value => { const node = element('option', value); node.value = value; return node; }));
      const access = element('option',t('panel_access_follow_switch')); access.value = '';
      const modes = optionAgent === 'opencode' ? ['runCommands'] : optionAgent === 'codex' ? ['readFiles','writeFiles','runCommands'] : ['browserOnly','readFiles','writeFiles','runCommands'];
      const labels = { browserOnly:t('panel_tools_browser_only'), readFiles: optionAgent === 'codex' ? t('panel_tools_read_files_codex') : t('panel_tools_read_files'), writeFiles:t('panel_tools_write_files'), runCommands:t('panel_tools_run_commands') };
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
    nodes.accessLabel.textContent = conversation?.options?.toolAccess ? (full ? t('panel_access_allow_tools') : t('panel_access_review_tools')) : full ? t('panel_access_full') : cliPermissions ? t('panel_access_cli_permissions') : browserOnly ? t('panel_tools_browser_only') : t('panel_access_ask_first');
    nodes.access.setAttribute('aria-pressed', String(full));
    nodes.access.title = full ? (browserOnly ? t('panel_access_title_pi_full') : t('panel_access_title_full')) : cliPermissions ? t('panel_access_title_opencode') : browserOnly ? t('panel_access_title_pi_browser') : t('panel_access_title_ask');
    nodes.deleteChat.hidden = !selected || !writable || !panelSupport?.conversationDelete;
    nodes.deleteChat.disabled = sending || !connected || busyPhases.has(conversation?.phase);
    if (nodes.deleteChat.hidden) nodes.deleteConfirm.hidden = true;
    nodes.confirmDelete.disabled = nodes.deleteChat.disabled;
    nodes.phase.textContent = panelSupport?.mode === 'readOnly' ? t('panel_phase_read_only') : conversation ? phaseLabels[conversation.phase] || t('panel_phase_unknown') : t('panel_phase_ready');
    nodes.effectiveModel.textContent = conversation?.model || '';
    const control = turnView() && conversation?.phase !== 'idle' ? controlLabels[conversation?.control] : null;
    nodes.control.hidden = !control; nodes.control.textContent = control || '';
    nodes.recordTaskLabel.hidden = !panelSupport?.recording || panelSupport?.mode !== 'full';
    nodes.recordTask.disabled = !connected || sending;
    const busy = busyPhases.has(conversation?.phase);
    renderWorking();
    nodes.status.classList.toggle('idle', !conversation || (conversation.phase === 'idle' && !conversation.model));
    nodes.status.classList.toggle('busy', busy);
    nodes.queue.textContent = pending ? t('panel_queue_unconfirmed') : conversation?.queued ? (conversation.queuePaused ? t('panel_queue_count_paused', conversation.queued) : t('panel_queue_count', conversation.queued)) : '';
    nodes.resume.hidden = !conversation?.queuePaused || !conversation?.queued;
    nodes.resume.disabled = !connected || !writable || conversation?.phase === 'recoveryRequired';
    nodes.stop.disabled = !connected || !writable || !busy || conversation?.phase === 'stopping';
    nodes.stop.hidden = !busy;
    nodes.attachImages.hidden = !panelSupport?.imageAttachments; nodes.attachImages.disabled = !connected || sending || convertingImages;
    nodes.send.disabled = convertingImages || !connected || !writable || sending || Boolean(pending) || conversation?.phase === 'recoveryRequired';
    nodes.sendDirect.hidden = !busyPhases.has(conversation?.phase) && !conversation?.queuePaused;
    nodes.sendDirect.disabled = nodes.send.disabled || Boolean(conversation?.directMessageId);
    nodes.sendDirect.textContent = busyPhases.has(conversation?.phase) ? t('panel_send_stop_and_send') : t('panel_send_now');
    nodes.send.title = busy ? t('panel_send_queue_title') : t('panel_send');
    nodes.send.setAttribute('aria-label', busy ? t('panel_send_queue_label') : t('panel_send_label'));
  }
  function conversationChoices() {
    nodes.conversation.replaceChildren(element('option', t('panel_new_conversation')));
    nodes.conversation.firstElementChild.value = '';
    for (const chat of conversations) { const option = element('option', chat.title || t('panel_conversation_untitled')); option.value = chat.id; nodes.conversation.append(option); }
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
    nodes.conversationTitle.textContent = chat?.title || t('panel_new_conversation');
    const domain = currentDomain(), siteScope = scope === 'site' && Boolean(domain);
    nodes.scopeSite.textContent = domain ? t('panel_scope_site_domain', domain) : t('panel_scope_site');
    nodes.scopeSite.disabled = !domain;
    nodes.scopeSite.setAttribute('aria-selected', String(siteScope)); nodes.scopeAll.setAttribute('aria-selected', String(!siteScope));
    const shown = siteScope ? conversations.filter(c => c.site && mainDomain(c.site) === domain) : conversations;
    const newItem = element('button', undefined, 'conversation-item new'); newItem.type = 'button';
    newItem.append(element('span', t('panel_new_conversation_item'), 'title'));
    newItem.addEventListener('click', () => { closeConversationMenu(); nodes.conversation.value = ''; select(''); loadModels(); });
    const items = [newItem];
    for (const c of shown) {
      const item = element('button', undefined, `conversation-item${c.id === selected ? ' selected' : ''}`); item.type = 'button';
      item.setAttribute('role', 'option'); item.setAttribute('aria-selected', String(c.id === selected));
      item.append(element('span', c.title || t('panel_conversation_untitled'), 'title'), element('span', [agentNames[c.agentId] || c.agentId, !siteScope && c.site, relativeTime(c.updated)].filter(Boolean).join(' · '), 'meta'));
      item.addEventListener('click', () => { closeConversationMenu(); select(c.id); });
      items.push(item);
    }
    if (!shown.length) items.push(element('p', siteScope ? t('panel_no_conversations_on', domain) : t('panel_no_conversations'), 'conversation-empty'));
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
    toolTail = null; toolGroups.clear(); clearTurnView();
    nodes.transcript.replaceChildren(nodes.empty); nodes.empty.hidden = false; controls(); renderConversationMenu();
    storage?.local?.set({ ab_panel_conversation: id }).catch(() => {});
    reportPresence();
    void poll().catch(error);
  }
  // Tell the background which conversation this panel shows, so a turn the
  // user is watching does not notify and its badge count clears.
  let presencePort = null, windowId = null;
  function reportPresence() {
    if (!presencePort || windowId === null) return;
    const visible = document.visibilityState !== 'hidden' && !nodes.chatView.hidden;
    try { presencePort.postMessage({ type: 'presence', windowId, conversationId: selected, visible }); } catch {}
  }
  function connectPresence() {
    if (closed || !runtime?.connect) return;
    try { presencePort = runtime.connect({ name: 'ab-panel-presence' }); } catch { return; }
    presencePort.onMessage.addListener(message => { if (message?.type === 'select' && typeof message.conversationId === 'string') select(message.conversationId); });
    // The service worker restarting closes the port; reconnect to the new one.
    presencePort.onDisconnect.addListener(() => { presencePort = null; schedule(connectPresence, 1000); });
    reportPresence();
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
    const mode = turnView() ? 'turns' : 'rows';
    if (viewMode !== mode) {
      if (viewMode) { rendered.clear(); toolTail = null; toolGroups.clear(); clearTurnView(); nodes.transcript.replaceChildren(nodes.empty); }
      viewMode = mode;
    }
    if (mode === 'turns') { renderTurns(); return; }
    const follow = nodes.transcript.scrollHeight - nodes.transcript.scrollTop - nodes.transcript.clientHeight < 80;
    nodes.empty.hidden = timeline.rows.length > 0;
    // A tool still open when the response ended is not running any more.
    const live = busyPhases.has(conversation?.phase);
    for (const row of timeline.rows) {
      let view = rendered.get(row.key);
      if (!view) {
        if (row.type === 'tool') {
          view = toolView(row); const { container } = view;
          if (!toolTail) toolTail = toolGroup();
          toolTail.tools.push(view); toolTail.list.append(container); toolGroups.add(toolTail);
          rendered.set(row.key, view);
        } else view = messageView(row, row.type !== 'user' || !turnView());
        if (row.type !== 'tool') { toolTail = null; rendered.set(row.key, view); nodes.transcript.append(view.container); }
      }
      updateRowView(view, row, live);
    }
    for (const group of toolGroups) renderToolGroup(group);
    renderWorking();
    if (follow) nodes.transcript.scrollTop = nodes.transcript.scrollHeight;
    nodes.latest.hidden = follow || timeline.rows.length === 0;
  }
  function messageView(row, showPage = true) {
    const container = element('article', undefined, `message-row ${row.type}`);
    const speaker = element('div', row.type === 'user' ? t('panel_speaker_you') : row.type === 'assistant' ? t('panel_speaker_assistant') : t('panel_speaker_status'), 'speaker');
    const content = element('div', undefined, 'content'); container.append(content, speaker); const view = { container, content, speaker };
    if (row.attachments?.length) {
      const images = element('div', undefined, 'attachment-images'); container.append(images);
      const chatId = selected;
      for (const attachment of row.attachments) {
        const image = element('img'); image.alt = t('panel_image_attached'); images.append(image);
        void api('conversationAttachmentRead', { conversationId: chatId, attachment }).then(result => {
          image.src = `data:${result.attachment.mediaType};base64,${result.data}`;
        }).catch(() => { image.alt = t('panel_image_unavailable'); });
      }
    }
    if (showPage && row.page?.title) container.append(element('div', row.page.title, 'page-ref'));
    if (row.detail) { const raw = element('details'); raw.append(element('summary', t('panel_details')), element('pre', row.detail)); container.append(raw); }
    if (row.type === 'note' && row.failed && conversation?.agentId) {
      const agentId = conversation.agentId;
      const prompt = [...timeline.rows].reverse().find(r => r.type === 'user' && !r.queueStatus)?.text;
      // Only an explicitly chosen model can be dropped; a model from the
      // CLI's own configuration has to be changed there.
      if (conversation.requestedModel && prompt) container.append(button(t('panel_retry_default_model'), () => retryWithDefaultModel(agentId, prompt), 'retry'));
      else if (conversation.model) container.append(button(t('panel_change_default_model'), async () => { showAgents(); await loadConfig(agentId, agentNames[agentId] || agentId); }, 'retry'));
    }
    return view;
  }
  function toolView(row) {
    const container = element('details', undefined, 'tool-row'); const title = element('summary');
    const name = element('span', undefined, 'tool-name'); const hint = element('span', undefined, 'tool-hint'); const state = element('span', undefined, 'tool-state'); title.append(name, hint, state);
    const content = element('pre'); container.append(title, content); return { container, title, name, hint, state, content, row };
  }
  function updateRowView(view, row, live) {
    if (view.text !== row.text) {
      view.text = row.text;
      // Agent replies are Markdown; user text and tool output stay literal.
      if (row.type === 'assistant') { renderMarkdown(document, view.content, row.text.trim()); view.container.hidden = !row.text.trim(); }
      else view.content.textContent = row.text;
    }
    if (row.type === 'user') view.speaker.textContent = row.queueStatus || '';
    if (row.type === 'tool') {
      view.name.textContent = row.name; view.hint.textContent = row.hint || ''; view.row = row;
      view.state.textContent = row.failed ? t('panel_tool_failed') : ''; view.running = live && !row.failed && !row.completed;
      view.state.title = row.failed ? t('panel_tool_failed') : view.running ? t('panel_tool_running') : '';
      view.container.classList.toggle('failed', row.failed); view.container.classList.toggle('running', view.running);
    }
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
    group.summary.textContent = [running && t('panel_working'), stepCount(count), failed && t('panel_steps_failed', failed)].filter(Boolean).join(' · ');
    if (count === 1) group.container.open = true;
    else if (!group.touched && running !== group.wasRunning) group.container.open = running;
    group.wasRunning = running;
  }
  function stepCount(count) { return count === 1 ? t('panel_steps_one', count) : t('panel_steps_many', count); }
  // ---- Turn view -----------------------------------------------------------
  // Every section is rebuilt only when its own data changed (`section`); steps,
  // texts and result cards are keyed so open details and loaded files survive.
  function clearTurnView() {
    for (const url of liveUrls) objectUrls.revoke(url);
    liveUrls.clear(); turnCards.clear(); forkView = null;
  }
  function section(card, name, data, build) {
    const signature = JSON.stringify(data);
    if (card.signatures[name] === signature) return;
    card.signatures[name] = signature; card[name].replaceChildren(...build());
    card[name].hidden = card[name].children.length === 0;
  }
  function turnCard(turn) {
    const container = element('article', undefined, 'turn-card');
    const card = { container, turn, signatures: {}, texts: new Map(), steps: new Map(), results: new Map(), touched: false, wasRunning: null, review: null };
    card.user = turn.user ? messageView(turn.user, false) : null;
    if (card.user) container.append(card.user.container);
    for (const name of ['sentWith', 'acting']) { card[name] = element('div', undefined, `turn-${name === 'sentWith' ? 'sent' : 'acting'} hint`); container.append(card[name]); }
    card.group = element('details', undefined, 'tool-group'); card.groupSummary = element('summary'); card.groupList = element('div', undefined, 'tool-list');
    card.group.append(card.groupSummary, card.groupList); card.group.addEventListener('click', () => { card.touched = true; });
    container.append(card.group);
    for (const name of ['unlinked', 'answer', 'resultList', 'assessment', 'outcome', 'stall', 'recovery']) { card[name] = element('div', undefined, `turn-${name}`); container.append(card[name]); }
    card.answerTexts = element('div'); card.answerNote = element('p', '', 'hint'); card.answer.append(card.answerTexts, card.answerNote);
    return card;
  }
  function renderTurns() {
    const follow = nodes.transcript.scrollHeight - nodes.transcript.scrollTop - nodes.transcript.clientHeight < 80;
    nodes.empty.hidden = timeline.rows.length > 0 || Boolean(timeline.recovery.context);
    const live = busyPhases.has(conversation?.phase);
    renderForkContext();
    for (const id of timeline.turnOrder) {
      const turn = timeline.turns.get(id);
      let card = turnCards.get(turn);
      if (!card) { card = turnCard(turn); turnCards.set(turn, card); nodes.transcript.append(card.container); }
      renderTurn(card, turn, live && turn.messageId === conversation?.activeMessageId);
    }
    renderWorking();
    if (follow) nodes.transcript.scrollTop = nodes.transcript.scrollHeight;
    nodes.latest.hidden = follow || timeline.rows.length === 0;
  }
  function renderForkContext() {
    const context = timeline.recovery.context;
    if (!context || forkView) return;
    forkView = element('section', undefined, 'turn-banner');
    forkView.append(element('p', t('panel_fork_context')));
    if (typeof context.fromConversationId === 'string') forkView.append(button(t('panel_open_earlier_conversation'), async () => { await list(); select(context.fromConversationId); }));
    nodes.transcript.insertBefore(forkView, nodes.transcript.children[0] || null);
  }
  function renderTurn(card, turn, live) {
    if (card.user) updateRowView(card.user, turn.user, live);
    const pageSent = turn.attachedPage;
    section(card, 'sentWith', pageSent, () => pageSent ? [element('span', t('panel_sent_with', pageName(pageSent)))] : []);
    renderActing(card, turn);
    renderSteps(card, turn, live);
    renderUnlinked(card, turn);
    renderAnswer(card, turn, live);
    renderResults(card, turn);
    renderAssessment(card, turn);
    renderOutcome(card, turn);
    renderStall(card, turn);
    renderRecovery(card, turn);
  }
  function renderActing(card, turn) {
    const pages = actingPages(turn);
    const browserSteps = turn.steps.some(key => timeline.items.get(key)?.browser);
    section(card, 'acting', { pages, recorded: turn.recorded, browserSteps }, () => {
      const lines = [];
      const list = targets => `${targets.slice(0, 3).map(target => target.attached ? t('panel_page_you_sent') : pageName(target)).join(', ')}${targets.length > 3 ? ` +${targets.length - 3}` : ''}`;
      if (!turn.recorded) { if (browserSteps) lines.push(element('div', t('panel_acted_not_recorded'))); return lines; }
      if (pages.actedOn.length) lines.push(element('div', t('panel_acted_on', list(pages.actedOn))));
      if (pages.pointedAt.length) lines.push(element('div', t('panel_pointed_at', list(pages.pointedAt))));
      for (const redirect of pages.redirects.slice(0, 3)) lines.push(element('div', t('panel_redirect', redirect.asked, redirect.landed)));
      return lines;
    });
  }
  function stepNote(turn, row) {
    const unresolved = turn.batchInterrupted?.unresolved?.find(entry => entry.toolCallId && entry.toolCallId === row.toolCallId);
    if (unresolved) return unresolved.executed === 'unknown' ? t('panel_step_no_record') : t('panel_step_unfinished');
    if (stepActions(timeline, row).some(action => action.late)) return t('panel_step_reported_late');
    return '';
  }
  function renderSteps(card, turn, live) {
    for (const key of turn.steps) {
      const row = timeline.items.get(key); if (!row) continue;
      let view = card.steps.get(key);
      if (!view) { view = toolView(row); view.chip = element('span', undefined, 'verdict-chip'); view.note = element('span', undefined, 'step-note'); view.title.append(view.chip, view.note); card.steps.set(key, view); card.groupList.append(view.container); }
      updateRowView(view, row, live);
      const verdict = stepVerdict(timeline, turn, row);
      view.verdict = verdict;
      view.chip.textContent = verdict ? verdictLabels[verdict] || verdict : ''; view.chip.hidden = !verdict; view.chip.className = `verdict-chip ${verdictClass[verdict] || ''}`;
      if (verdict === 'Failed') view.state.textContent = '';
      view.note.textContent = stepNote(turn, row); view.note.hidden = !view.note.textContent;
    }
    const views = [...card.steps.values()], count = views.length;
    card.group.hidden = count === 0;
    // A folded group names its failed steps, so folding never hides a failure.
    const failed = views.filter(view => view.row.failed || view.verdict === 'Failed').map(view => view.row.name);
    const running = views.some(view => view.running);
    card.groupSummary.textContent = [running && t('panel_working'), stepCount(count), failed.length && t('panel_steps_failed_named', failed.length, [...new Set(failed)].join(', ')), turn.compactions.length && t('panel_memory_summarized')].filter(Boolean).join(' · ');
    card.group.classList.toggle('single', count === 1); card.group.classList.toggle('running', running);
    if (count === 1) card.group.open = true;
    else if (!card.touched && running !== card.wasRunning) card.group.open = running;
    card.wasRunning = running;
  }
  function renderUnlinked(card, turn) {
    const unlinked = [...turn.actions.values()].filter(action => !action.linkedStep).map(action => ({ tool: action.tool, verdict: action.observed?.verdict || null, late: action.late, unreported: action.unreported && !action.target }));
    const stray = (turn.batchInterrupted?.unresolved || []).filter(entry => !entry.toolCallId || !turn.stepByCall.has(entry.toolCallId)).map(entry => ({ name: entry.name, executed: entry.executed }));
    const compactions = turn.compactions.length;
    section(card, 'unlinked', { unlinked, stray, compactions }, () => {
      const lines = [];
      if (compactions) lines.push(element('p', t('panel_memory_summarized_note'), 'hint'));
      if (unlinked.length) {
        const box = element('details', undefined, 'unlinked-actions'); box.append(element('summary', unlinked.length === 1 ? t('panel_unlinked_one', unlinked.length) : t('panel_unlinked_many', unlinked.length)));
        for (const action of unlinked) box.append(element('div', [(action.tool || t('panel_browser_tool')).replace(/^chrome_use_/, ''), action.unreported ? t('panel_action_unreported') : verdictLabels[action.verdict === 'confirmed' ? 'Confirmed' : action.verdict === 'failed' ? 'Failed' : 'Not checked'], action.late && t('panel_action_reported_late')].filter(Boolean).join(' · ')));
        lines.push(box);
      }
      for (const entry of stray) lines.push(element('div', entry.executed === 'unknown' ? t('panel_stray_no_record', entry.name || t('panel_a_tool')) : t('panel_stray_unfinished', entry.name || t('panel_a_tool')), 'hint'));
      return lines;
    });
  }
  function renderAnswer(card, turn, live) {
    const keys = [...turn.texts].sort((a, b) => (turn.roles.get(a) === 'commentary' ? 0 : 1) - (turn.roles.get(b) === 'commentary' ? 0 : 1));
    keys.forEach((key, index) => {
      const row = timeline.items.get(key); if (!row) return;
      let view = card.texts.get(key);
      if (!view) { view = messageView(row); card.texts.set(key, view); }
      const role = turn.roles.get(key);
      view.speaker.textContent = role === 'commentary' ? t('panel_role_progress') : role === 'final' ? t('panel_role_answer') : t('panel_role_response');
      view.container.className = `message-row assistant${role === 'commentary' ? ' commentary' : ''}`;
      if (card.answerTexts.children[index] !== view.container) card.answerTexts.insertBefore(view.container, card.answerTexts.children[index] || null);
      updateRowView(view, row, live);
    });
    card.answerNote.textContent = turn.emptyAnswer ? t('panel_empty_answer') : '';
    card.answerNote.hidden = !turn.emptyAnswer;
    card.answer.hidden = !keys.length && !turn.emptyAnswer;
  }
  function renderResults(card, turn) {
    card.resultList.hidden = turn.artifacts.size === 0;
    for (const artifact of turn.artifacts.values()) {
      let view = card.results.get(artifact.artifactId);
      if (!view) { view = { container: element('section', undefined, 'result-card'), lines: element('div'), media: element('div', undefined, 'result-media'), status: element('p', '', 'hint'), loading: false, signature: null };
        view.container.append(view.lines, view.media, view.status); card.results.set(artifact.artifactId, view); card.resultList.append(view.container); }
      const signature = JSON.stringify(artifact);
      if (view.signature === signature) continue;
      view.signature = signature;
      view.lines.replaceChildren(...artifactLines(artifact));
      view.container.className = `result-card ${artifact.state}`;
      if (artifact.state === 'ready' && panelSupport?.artifacts) {
        if (artifact.kind === 'screenshot' && !view.url && !view.loading) void showArtifact(view, artifact, true);
        else if (artifact.kind !== 'screenshot' && !view.save) { view.save = button(t('panel_artifact_save_copy'), () => showArtifact(view, artifact, false)); view.media.append(view.save); }
      }
    }
  }
  // A result card reports what happened to the file, never that it meets the request.
  function artifactLines(artifact) {
    const kind = { screenshot: t('panel_artifact_screenshot'), download: t('panel_artifact_download'), recording: t('panel_artifact_recording') }[artifact.kind] || t('panel_artifact_file');
    const lines = [element('div', `${kind}${artifact.name ? ` · ${artifact.name}` : ''}`, 'result-title')];
    if (artifact.state === 'unavailable') {
      lines.push(element('div', t('panel_artifact_unavailable', unavailableText[artifact.reason] || t('panel_artifact_not_kept'))));
      if (artifact.detail) lines.push(element('div', artifact.detail, 'hint'));
      if (artifact.reason === 'tooLargeToRetain' && Number.isFinite(artifact.bytes)) lines.push(element('div', t('panel_artifact_size', sizeText(artifact.bytes)), 'hint'));
    } else if (artifact.state !== 'ready') lines.push(element('div', { capturing: artifact.kind === 'recording' ? t('panel_artifact_recording_now') : t('panel_artifact_capturing'), finalizing: t('panel_artifact_finalizing') }[artifact.state] || t('panel_artifact_waiting')));
    else {
      lines.push(element('div', t('panel_artifact_captured')));
      if (artifact.declaredExtension) lines.push(element('div', artifact.typeMatches === true ? t('panel_artifact_type_yes', artifact.declaredExtension) : artifact.typeMatches === false ? t('panel_artifact_type_no', artifact.declaredExtension, artifact.mime || t('panel_artifact_unknown_type')) : t('panel_artifact_type_unchecked', artifact.declaredExtension)));
      lines.push(element('div', artifact.bytes > 0 ? t('panel_artifact_not_empty') : t('panel_artifact_empty')));
      lines.push(element('div', [artifact.mime, sizeText(artifact.bytes)].filter(Boolean).join(' · '), 'hint'));
    }
    if (artifact.delivery) lines.push(element('div', artifact.delivery.state === 'delivered' ? t('panel_artifact_delivered') : artifact.delivery.detail ? t('panel_artifact_not_delivered_detail', artifact.delivery.detail) : t('panel_artifact_not_delivered')));
    return lines;
  }
  // Files are read in chunks, sequentially, at most two at a time (the bridge
  // caps pending requests). Bytes are shown only when their SHA-256 matches.
  let artifactsRunning = 0; const artifactWaiters = [];
  async function artifactSlot(task) {
    if (artifactsRunning >= 2) await new Promise(resolve => artifactWaiters.push(resolve));
    artifactsRunning++;
    try { return await task(); } finally { artifactsRunning--; artifactWaiters.shift()?.(); }
  }
  async function readArtifact(conversationId, artifact) {
    const chunks = []; let offset = 0, total = null, digest = null;
    do {
      const result = await api('conversationArtifactRead', { conversationId, artifactId: artifact.artifactId, offset, length: ARTIFACT_CHUNK });
      if ((digest !== null && result.digest !== digest) || (total !== null && result.total !== total)) throw new Error('changed');
      digest = result.digest; total = result.total;
      const bytes = base64Bytes(result.data);
      if (!bytes.length && offset < total) throw new Error('changed');
      chunks.push(bytes); offset += bytes.length;
    } while (offset < total);
    if (offset !== total || (artifact.digest && digest !== artifact.digest)) throw new Error('changed');
    const blob = new Blob(chunks, { type: artifact.mime || 'application/octet-stream' });
    const hash = [...new Uint8Array(await subtle.digest('SHA-256', await blob.arrayBuffer()))].map(byte => byte.toString(16).padStart(2, '0')).join('');
    if (`sha256:${hash}` !== digest) throw new Error('changed');
    return blob;
  }
  async function showArtifact(view, artifact, inline) {
    if (view.loading) return;
    const id = selected, epoch = selectionEpoch;
    view.loading = true; view.status.textContent = t('panel_artifact_reading');
    try {
      const blob = await artifactSlot(() => readArtifact(id, artifact));
      if (id !== selected || epoch !== selectionEpoch) return;
      if (view.url) { objectUrls.revoke(view.url); liveUrls.delete(view.url); }
      view.url = objectUrls.create(blob); liveUrls.add(view.url);
      if (inline) { const image = element('img'); image.alt = artifact.name || t('panel_artifact_screenshot'); image.src = view.url; view.media.replaceChildren(image); }
      const save = element('a', t('panel_artifact_save'), 'quiet'); save.href = view.url; save.download = artifact.name || 'artifact';
      if (inline) view.media.append(save); else save.click?.();
      view.status.textContent = '';
    } catch (failure) {
      if (id !== selected || epoch !== selectionEpoch) return;
      view.status.textContent = failure?.message === 'changed' ? t('panel_artifact_changed') : /artifact_unavailable/.test(failure?.message || '') ? t('panel_artifact_gone') : t('panel_artifact_read_failed', failure?.message || t('panel_no_answer'));
      view.status.className = 'hint changed';
    } finally { view.loading = false; }
  }
  function renderAssessment(card, turn) {
    const { final, provisional } = turn.assessment;
    const label = assessmentLabel(turn);
    section(card, 'assessment', { label, final, provisional: Boolean(provisional) }, () => {
      const box = element('details', undefined, 'assessment');
      const summary = element('summary'); summary.append(element('span', label, `badge ${final?.status || (!final && turn.recorded && turn.state !== 'ended' ? 'pending' : 'none')}`)); box.append(summary);
      if (!final) return [box];
      if (final.blocker?.text) box.append(element('p', t('panel_assessment_blocked', final.blocker.text)));
      if (final.reason === 'notAssessed') box.append(element('p', t('panel_assessment_not_assessed'), 'hint'));
      for (const requirement of final.requirements || []) {
        const line = element('div', undefined, `requirement ${requirement.computed}`);
        line.append(element('span', requirement.text || t('panel_requirement')), element('span', computedLabels[requirement.computed] || t('panel_check_unknown'), 'requirement-state'));
        const detail = checkDetail(requirement);
        if (detail) line.append(element('div', detail, 'hint'));
        else if (requirement.because) line.append(element('div', requirement.because, 'hint'));
        box.append(line);
      }
      for (const reason of final.downgradedBecause || []) box.append(element('div', t('panel_assessment_lowered', reason), 'hint'));
      if (final.requirements?.length) box.append(element('p', t('panel_assessment_disclaimer'), 'hint'));
      return [box];
    });
  }
  // "page showed 'Order placed' on shop.example/orders" — only for checked
  // requirements, and only from the cited evidence's own recorded target.
  function checkDetail(requirement) {
    const check = requirement.check;
    if (requirement.computed !== 'checked' || !check) return check?.predicate ? t('panel_check_predicate', check.predicate) : '';
    const action = timeline.actions.get(check.evidence), artifact = timeline.artifacts.get(check.evidence);
    if (artifact) return t('panel_check_file', artifact.name || '', check.predicate).replace('  ', ' ');
    const target = action?.observed?.check?.target;
    const where = target?.url ? `${hostOf(target.url)}${(() => { try { return new URL(target.url).pathname; } catch { return ''; } })()}` : '';
    const when = action?.observed?.at ? new Date(action.observed.at).toLocaleTimeString() : '';
    const verb = { expect: t('panel_check_verb_expect'), readBack: t('panel_check_verb_read_back'), urlMatch: t('panel_check_verb_url') }[check.kind] || t('panel_check_verb_other');
    if (where && when) return t('panel_check_evidence_where_when', verb, check.predicate, where, when);
    if (where) return t('panel_check_evidence_where', verb, check.predicate, where);
    if (when) return t('panel_check_evidence_when', verb, check.predicate, when);
    return t('panel_check_evidence', verb, check.predicate);
  }
  function renderOutcome(card, turn) {
    const label = outcomeLabel(turn);
    section(card, 'outcome', { label, error: turn.error, notes: turn.notes }, () => {
      const lines = [];
      if (label) lines.push(element('div', label, `outcome ${turn.outcome || turn.legacyTerminal || ''}`));
      for (const key of turn.notes) { const row = timeline.rows.find(candidate => candidate.key === key); if (row) { const view = messageView(row); updateRowView(view, row, false); lines.push(view.container); } }
      return lines;
    });
  }
  function renderStall(card, turn) {
    const stall = turn.state !== 'ended' ? turn.stalls.findLast(entry => entry.open && !dismissedStalls.has(`${turn.messageId}:${entry.sequence}`)) : null;
    section(card, 'stall', stall, () => {
      if (!stall) return [];
      const banner = element('div', undefined, 'stall-banner');
      banner.append(element('p', t('panel_stall_banner', (stallText[stall.kind] || stallText.noActivity)(stall.count))));
      const actions = element('div', undefined, 'actions');
      actions.append(button(t('panel_stop'), () => controlConversation('conversationStop'), 'quiet'), button(t('panel_keep_going'), async () => { dismissedStalls.add(`${turn.messageId}:${stall.sequence}`); renderTurns(); }));
      banner.append(actions); return [banner];
    });
  }
  function renderRecovery(card, turn) {
    const unknown = turn.outcome === 'unknown' || (conversation?.phase === 'recoveryRequired' && conversation?.activeMessageId === turn.messageId);
    const show = panelSupport?.recovery && (unknown || turn.forkedTo);
    const review = card.review;
    section(card, 'recovery', { show: Boolean(show), forkedTo: turn.forkedTo, review, phase: conversation?.phase }, () => {
      if (!show) return [];
      const block = element('section', undefined, 'recovery-block');
      if (turn.forkedTo) { block.append(element('p', t('panel_recovery_forked')), button(t('panel_open_new_conversation'), async () => { await list(); select(turn.forkedTo); })); return [block]; }
      block.append(element('p', t('panel_recovery_unknown')));
      const actions = element('div', undefined, 'actions');
      actions.append(button(t('panel_recovery_review'), () => recoveryReview(card)));
      if (review?.error) block.append(element('p', review.error, 'hint'));
      if (review?.process) {
        const state = review.process.state;
        block.append(element('p', state === 'alive' ? t('panel_recovery_alive') : state === 'exited' ? t('panel_recovery_exited') : t('panel_recovery_state_unknown')));
        if (review.process.reason) block.append(element('p', review.process.reason, 'hint'));
        const executed = (review.actions || []).filter(action => action.possiblyExecuted);
        if (executed.length) { const list = element('ul'); for (const action of executed.slice(0, 20)) { const tool = String(action.tool || t('panel_browser_tool')).replace(/^chrome_use_/, ''); list.append(element('li', action.target?.url ? t('panel_recovery_action_on', tool, hostOf(action.target.url)) : t('panel_recovery_action', tool))); } block.append(list); }
        if (review.terminated) block.append(element('p', review.terminated.confirmed ? t('panel_recovery_ended') : t('panel_recovery_end_unconfirmed'), 'hint'));
        if (state === 'alive') actions.append(button(t('panel_recovery_end'), () => recoveryTerminate(card), 'danger'));
        else if (state === 'exited' && conversation?.phase === 'recoveryRequired') actions.append(button(t('panel_recovery_continue'), () => recoveryFork(), 'primary'));
        else if (state === 'unknown') block.append(element('p', t('panel_recovery_no_options'), 'hint'));
      }
      block.append(actions); return [block];
    });
  }
  async function recoveryReview(card) {
    const id = selected, epoch = selectionEpoch;
    try {
      const review = await api('conversationRecoveryReview', { conversationId: id });
      if (id === selected && epoch === selectionEpoch) { card.review = { ...review, terminated: card.review?.terminated || null }; renderTurns(); }
    } catch (failure) { if (id === selected && epoch === selectionEpoch) { card.review = { error: t('panel_recovery_review_failed', failure?.message || t('panel_no_answer')) }; renderTurns(); } }
  }
  async function recoveryTerminate(card) {
    const id = selected, epoch = selectionEpoch;
    const result = await api('conversationRecoveryTerminate', { conversationId: id });
    if (id !== selected || epoch !== selectionEpoch) return;
    card.review = { ...card.review, terminated: { confirmed: result.confirmed } };
    await recoveryReview(card); await poll();
  }
  async function recoveryFork() {
    const id = selected;
    const result = await api('conversationRecoveryFork', { conversationId: id });
    await list(); if (selected === id) select(result.conversation.id);
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
  // `allowCustom(question)` decides whether a free-text answer is offered.
  // `read()` returns the typed values; `answer()` the decision shape the
  // service validates: {option}, {options} (multi-select) or {custom}. Options
  // are sent by index: the shown labels may be redacted, the original's are
  // what the agent gets.
  function questionFields(questions, container, allowCustom = () => true) {
    return questions.map((question, index) => {
      const group = element('div', undefined, 'question'); group.append(element('label', question.question || question.header || t('panel_question')));
      const choices = [];
      for (const [optionIndex, option] of (question.options || []).entries()) {
        const label = element('label', undefined, 'choice'); const input = element('input'); input.dataset.index = String(optionIndex);
        input.type = question.multiSelect ? 'checkbox' : 'radio'; input.name = `question-${selectionEpoch}-${index}`; input.value = option.label;
        const text = element('span', option.label); if (option.description) text.append(element('small', option.description)); label.append(input, text); group.append(label); choices.push(input);
      }
      const free = element('input'); free.placeholder = t('panel_your_answer'); free.setAttribute('aria-label', t('panel_answer_label', question.question || question.header || t('panel_question')));
      const custom = allowCustom(question); if (custom) group.append(free); container.append(group);
      const read = () => { const text = custom ? free.value.trim() : ''; return text ? [text] : choices.filter(c => c.checked).map(c => c.value); };
      return { question, read, answer() {
        const text = custom ? free.value.trim() : ''; const picked = choices.filter(c => c.checked).map(c => Number(c.dataset.index));
        return text ? { custom: text } : question.multiSelect ? { options: picked } : { option: picked[0] };
      } };
    });
  }
  // A host that binds replies to request revisions gets only a decision; the
  // service builds the agent's reply from the request it kept. The revision
  // is the one this card was built for, never a newer request's.
  function boundReplies(request) { return panelSupport?.requestRevision === 1 && Number.isInteger(request?.requestRevision) && Number.isInteger(request?.generation); }
  async function respond(key, result, decision) {
    const request = timeline.requests.get(key); if (!request || request.sent) return;
    const id = selected, epoch = selectionEpoch;
    const view = requestNodes.get(key);
    if (view.revision !== request.requestRevision) throw new Error(t('panel_request_changed'));
    for (const node of view.container.querySelectorAll('button')) node.disabled = true;
    const params = view.bound
      ? { conversationId: id, requestId: request.requestId, generation: view.generation, requestRevision: view.revision, decision }
      : { conversationId: id, requestId: request.requestId, result };
    try {
      await api('conversationRespond', params);
      if (selected === id && epoch === selectionEpoch) { request.sent = true; renderRequests(); }
    } catch (failure) { if (selected === id && epoch === selectionEpoch) { request.unconfirmed = true; view.status.textContent = t('panel_respond_unconfirmed'); } throw failure; }
  }
  // Buttons that let a requested action run. They stay disabled once Stop was
  // requested; the service refuses them then as well (decline still works).
  function allow(node) { node.dataset.decision = 'allow'; return node; }
  function requestView(key, request) {
    const container = element('section', undefined, 'request'); const status = element('p', '', 'hint');
    const params = request.params || {}; const method = request.method; const actions = element('div', undefined, 'actions');
    const bound = boundReplies(request);
    container.append(element('h3', method === 'item/tool/requestUserInput' || params.tool_name === 'AskUserQuestion' ? t('panel_request_needs_input') : t('panel_request_review')));
    if (params.truncated) { container.append(element('p', t('panel_request_truncated'))); }
    else if (method === 'item/tool/requestUserInput' || params.tool_name === 'AskUserQuestion') {
      const codex = method === 'item/tool/requestUserInput';
      // Codex marks where free text is allowed; Claude always offers "Other".
      const allowCustom = question => !bound || !codex || question.isOther === true || !(question.options || []).length;
      const fields = questionFields(codex ? params.questions || [] : params.input?.questions || [], container, allowCustom);
      actions.append(allow(button(t('panel_send_answers'), async () => {
        const answers = fields.map(f => ({ question: f.question, values: f.read(), answer: f.answer() }));
        if (answers.some(a => a.values.length === 0)) throw new Error(t('panel_answer_each'));
        const result = codex
          ? { answers: Object.fromEntries(answers.map(a => [a.question.id, { answers: a.values }])) }
          : { behavior: 'allow', updatedInput: { ...params.input, answers: Object.fromEntries(answers.map(a => [a.question.question, a.values.join(', ')])) } };
        // Questions by position: their shown text may be redacted.
        const decision = { answersByPosition: answers.map(a => a.answer) };
        await respond(key, result, decision);
      }, 'primary')));
    } else if (['item/commandExecution/requestApproval', 'item/fileChange/requestApproval'].includes(method)) {
      container.append(element('p', params.reason || t('panel_request_decision')));
      if (params.command) container.append(element('pre', params.command));
      if (params.cwd) container.append(element('p', t('panel_working_folder', params.cwd), 'hint'));
      actions.append(allow(button(t('panel_allow_once'), () => respond(key, { decision: 'accept' }, 'accept'), 'primary')), button(t('panel_decline'), () => respond(key, { decision: 'decline' }, 'decline')));
    } else if (method === 'mcpServer/elicitation/request') {
      container.append(element('p', params.message || t('panel_request_mcp')));
      const properties = params.requestedSchema?.properties;
      const approval = params.serverName === 'chrome_use' && params.mode === 'form' && params._meta?.codex_approval_kind === 'mcp_tool_call' && params.requestedSchema?.type === 'object' && properties && typeof properties === 'object' && Object.keys(properties).length === 0 && (params.requestedSchema.required === undefined || (Array.isArray(params.requestedSchema.required) && params.requestedSchema.required.length === 0)) && params._meta.tool_params && typeof params._meta.tool_params === 'object' && !Array.isArray(params._meta.tool_params);
      if (approval) {
        container.dataset.approvalKind = 'mcpTool'; container.dataset.serverName = params.serverName;
        if (params._meta.tool_description) container.append(element('p', params._meta.tool_description, 'hint'));
        container.append(element('pre', JSON.stringify(params._meta.tool_params, null, 2), 'tool-arguments'));
        actions.append(allow(button(t('panel_allow_once'), () => respond(key, {action:'accept',content:{}}, 'accept'), 'primary')));
      } else container.append(element('p', t('panel_request_form_unsafe')));
      actions.append(button(t('panel_decline'), () => respond(key, {action:'decline',content:null}, 'decline')), button(t('panel_cancel_request'), () => respond(key, {action:'cancel',content:null}, 'cancel')));
    } else if (method === 'session/request_permission') {
      container.append(element('p', params.toolCall?.title || t('panel_request_opencode')));
      container.append(element('pre', JSON.stringify(params.toolCall?.rawInput || {}, null, 2)));
      for (const option of params.options || []) { const choice = button(option.name || option.optionId, () => respond(key, { outcome: { outcome: 'selected', optionId: option.optionId } }, { optionId: option.optionId }), option.kind === 'allow_once' ? 'primary' : 'quiet'); actions.append(String(option.kind || '').startsWith('allow') ? allow(choice) : choice); }
      actions.append(button(t('panel_cancel_request'), () => respond(key, { outcome: { outcome: 'cancelled' } }, 'cancel')));
    } else if (method === 'can_use_tool') {
      container.append(element('p', params.tool_name || t('panel_request_tool'))); container.append(element('pre', JSON.stringify(params.input || {}, null, 2)));
      actions.append(allow(button(t('panel_allow_once'), () => respond(key, { behavior: 'allow', updatedInput: params.input || {} }, 'allow'), 'primary')), button(t('panel_decline'), () => respond(key, { behavior: 'deny', message: 'User declined' }, 'deny')));
    } else if (method === 'item/permissions/requestApproval') {
      container.append(element('p', params.reason || t('panel_request_permissions'))); container.append(element('pre', JSON.stringify(params.permissions || {}, null, 2)));
      actions.append(allow(button(t('panel_allow_turn'), () => respond(key, { permissions: params.permissions || {}, scope: 'turn' }, 'accept'), 'primary')));
    } else { container.append(element('p', t('panel_request_unsupported'))); }
    const question = method === 'item/tool/requestUserInput' || params.tool_name === 'AskUserQuestion';
    if (!question && !params.truncated && panelSupport?.fullAccess && actions.querySelectorAll('button').length) actions.append(allow(button(t('panel_allow_all'), () => setAccess(true))));
    container.append(actions, status); return { container, status, bound, revision: request.requestRevision, generation: request.generation };
  }
  function renderRequests() {
    // A request re-sent under the same id is a new revision: its old card goes.
    for (const [key, view] of requestNodes) if (!timeline.requests.has(key) || timeline.requests.get(key).requestRevision !== view.revision) { view.container.remove(); requestNodes.delete(key); }
    for (const [key, request] of timeline.requests) {
      if (!requestNodes.has(key)) { const view = requestView(key, request); requestNodes.set(key, view); nodes.requests.append(view.container); }
      const view = requestNodes.get(key); view.status.textContent = request.sent ? t('panel_request_sent') : request.unconfirmed ? t('panel_request_unconfirmed') : '';
      if (request.sent || request.unconfirmed) for (const node of view.container.querySelectorAll('button')) node.disabled = true;
      else if (conversation?.control === 'stopRequested') {
        for (const node of view.container.querySelectorAll('button')) if (node.dataset.decision === 'allow') node.disabled = true;
        view.status.textContent = t('panel_request_stop_requested');
      }
    }
  }
  function renderQueue(messages) {
    nodes.queuedMessages.hidden = messages.length === 0;
    const live = new Set(messages.map(message => message.id));
    for (const [id, view] of queuedNodes) if (!live.has(id)) { view.container.remove(); queuedNodes.delete(id); }
    for (const message of messages) {
      if (queuedNodes.has(message.id)) continue;
      const container = element('details', undefined, 'queued-message');
      container.append(element('summary', t(message.direct ? 'panel_queued_direct' : 'panel_queued', message.text.split('\n')[0].slice(0, 100))));
      let expectedText = message.truncated ? null : message.text;
      const input = element('textarea'); input.readOnly = message.truncated === true; input.value = message.text; input.maxLength = 32768; input.setAttribute('aria-label', t('panel_queued_edit_label'));
      const actions = element('div', undefined, 'actions'); const status = element('p', '', 'hint');
      const id = selected, epoch = selectionEpoch;
      const mutate = async (operation, extra = {}) => {
        if (selected !== id || selectionEpoch !== epoch) return;
        if (expectedText === null) throw new Error(t('panel_queued_load_first'));
        for (const node of actions.querySelectorAll('button')) node.disabled = true;
        try {
          await api(operation, { conversationId: id, messageId: message.id, expectedText, ...extra });
          if (selected === id && selectionEpoch === epoch) { status.textContent = t('panel_queued_saved'); await poll(); }
        } catch (failure) { if (selected === id && selectionEpoch === epoch) status.textContent = t('panel_queued_unconfirmed'); throw failure; }
      };
      if (message.truncated) actions.append(button(t('panel_queued_load_full'), async () => {
        const result = await api('conversationQueueMessageRead', { conversationId: id, messageId: message.id });
        if (selected !== id || selectionEpoch !== epoch) return;
        expectedText = result.queuedMessage.text; input.value = expectedText; input.readOnly = false; status.textContent = t('panel_queued_loaded');
      }));
      actions.append(button(t('panel_queued_save'), async () => {
        if (!input.value.trim()) throw new Error(t('panel_queued_empty'));
        if (input.value === expectedText) return;
        await mutate('conversationQueueEdit', { replacementId: crypto.randomUUID(), text: input.value });
      }), button(t('panel_queued_remove'), () => mutate('conversationQueueRemove')));
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
    if (retry && typeof pending.text !== 'string') throw new Error(t('panel_pending_text_missing'));
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
    if (panelSupport?.mode !== 'full') throw new Error(t('panel_error_cli_required'));
    if (!connected || sending || convertingImages || pendingMessages.has(selected) || conversation?.phase === 'recoveryRequired' || !nodes.message.value.trim()) return;
    if (direct && conversation?.directMessageId) throw new Error(t('panel_error_direct_waiting'));
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
      // Recording consent is per message; the host refuses to record without it.
      if (panelSupport?.recording && nodes.recordTask.checked) params.record = true;
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
      pageDetached = false; renderPage(); nodes.recordTask.checked = false;
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
    nodes.pageChip.title = page ? t('panel_page_chip_title', page.url) : '';
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
      if (deleted.cliHistory.failed.length) problems.push(t('panel_delete_not_removed', deleted.cliHistory.failed.join(', ')));
      if (deleted.cliHistory.error) problems.push(t('panel_delete_session_files', deleted.cliHistory.error));
      if (!deleted.browserSession.closed) problems.push(t('panel_delete_tabs_unconfirmed'));
      if (problems.length) error(t('panel_delete_done_with_problems', problems.join(' ')));
      await list();
    } finally { nodes.deleteConfirm.hidden = true; controls(); }
  }
  function view(agents) { nodes.chatView.hidden = agents; nodes.agentsView.hidden = !agents; nodes.chatTab.classList.toggle('selected', !agents); nodes.agentsTab.classList.toggle('selected', agents); nodes.chatTab.setAttribute('aria-pressed', String(!agents)); nodes.agentsTab.setAttribute('aria-pressed', String(agents)); reportPresence(); }
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
    const keep = nodes.model.value; const first = element('option', t('panel_default_model')); first.value = '';
    nodes.model.replaceChildren(first);
    for (const model of models) { const option = element('option', model.label || model.id); option.value = model.id; nodes.model.append(option); }
    nodes.model.value = models.some(model => model.id === keep) ? keep : '';
  }
  function clearModels() { modelEpoch++; modelOptions(); nodes.modelStatus.textContent = t('panel_model_not_checked'); nodes.inspectAgent.disabled = !connected || panelSupport?.mode !== 'full' || !panelSupport?.modelInspection; }
  function showInspection(inspection) {
    modelOptions(inspection.models);
    const account = { reportedSignedIn: t('panel_cli_signed_in'), reportedSignedOut: t('panel_cli_signed_out'), reportedMetadata: t('panel_cli_account_metadata'), unverified: t('panel_cli_account_unverified') }[inspection.account.status];
    const count = inspection.models.length, version = inspection.versionReported;
    nodes.modelStatus.textContent = version
      ? (inspection.complete ? t('panel_model_status_version', version, count, account) : t('panel_model_status_version_partial', version, count, account))
      : (inspection.complete ? t('panel_model_status', count, account) : t('panel_model_status_partial', count, account));
  }
  async function inspectModels(force = true) {
    if (panelSupport?.mode !== 'full' || !panelSupport?.modelInspection) throw new Error(t('panel_model_inspection_unavailable'));
    const epoch = ++modelEpoch, agentId = nodes.agent.value, executable = nodes.executable.value.trim();
    nodes.inspectAgent.disabled = true; nodes.modelStatus.textContent = t('panel_model_reading');
    try {
      const inspection = await readInspection(agentId, executable, force);
      if (closed || epoch !== modelEpoch || agentId !== nodes.agent.value || executable !== nodes.executable.value.trim() || inspection.agentId !== agentId) return;
      showInspection(inspection);
    } catch (failure) { if (epoch === modelEpoch) nodes.modelStatus.textContent = t('panel_model_metadata_failed', failure?.message || t('panel_cli_no_answer')); throw failure; }
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
  const accountLabels = messageMap({ reportedSignedIn: 'panel_account_signed_in', reportedSignedOut: 'panel_account_signed_out', reportedMetadata: 'panel_account_reported', unverified: 'panel_account_login_unknown' });
  // One line per agent that answers "can I use this?" from the CLI's own report.
  // The report is not proof of model access; that is settled by the first turn.
  async function agentStatus(agent, line, epoch, row) {
    const mark = state => { if (row) row.className = `agent-row ${state}${configuration?.agentId === agent.id && !nodes.configuration.hidden ? ' selected' : ''}`; };
    if (!agent.executable) { mark('missing'); line.textContent = agent.configFiles?.length ? t('panel_agent_config_only') : t('panel_agent_not_installed'); return; }
    mark('installed');
    if (!(panelSupport?.chatAgents || ['codex', 'claude']).includes(agent.id)) { line.textContent = t('panel_agent_chat_unsupported'); return; }
    if (panelSupport?.mode !== 'full' || !panelSupport?.modelInspection) { line.textContent = t('panel_agent_installed'); return; }
    line.textContent = t('panel_agent_checking');
    try {
      const inspection = await readInspection(agent.id);
      if (epoch !== discoverEpoch) return;
      const account = inspection.account || {};
      const parts = [inspection.versionReported ? `v${String(inspection.versionReported).replace(/^v/, '')}` : t('panel_agent_installed'), `${accountLabels[account.status] || t('panel_account_login_unknown')}${account.status === 'reportedSignedIn' && typeof account.kind === 'string' ? ` (${account.kind})` : ''}`];
      if (inspection.models?.length) parts.push(t('panel_models_count', inspection.models.length));
      line.textContent = parts.join(' · ');
      line.classList.toggle('signed-out', account.status === 'reportedSignedOut');
      mark(account.status === 'reportedSignedOut' ? 'signed-out' : account.status === 'reportedSignedIn' ? 'ready' : 'installed');
      line.title = t('panel_agent_status_title');
    } catch (failure) { if (epoch === discoverEpoch) { line.textContent = failure?.message ? t('panel_agent_status_failed_reason', failure.message) : t('panel_agent_status_failed'); } }
  }
  async function discover() {
    const epoch = ++discoverEpoch;
    const result = await api('discover'); if (epoch !== discoverEpoch) return; nodes.agentList.replaceChildren(); agentRows.clear();
    for (const agent of result.agents) {
      const row = element('div', undefined, 'agent-row'), info = element('div', undefined, 'agent-info'), line = element('div', undefined, 'agent-evidence');
      info.append(element('div', agent.name, 'agent-name'), line);
      row.append(info, button(t('panel_agent_configure'), () => loadConfig(agent.id, agent.name))); nodes.agentList.append(row); agentRows.set(agent.id, row);
      if (nodes.agentsView.hidden) line.textContent = agent.executable ? t('panel_agent_installed') : t('panel_agent_not_installed'); else void agentStatus(agent, line, epoch, row);
    }
  }
  async function loadConfig(agentId, name) {
    const epoch = ++configEpoch; const result = await api('configRead', { agentId }); if (epoch !== configEpoch) return;
    configuration = result.configuration; configuration.name = name; preview = null; applied = null;
    nodes.configuration.hidden = false; nodes.configTitle.textContent = t('panel_config_title', name); nodes.configPath.textContent = configuration.path;
    nodes.configWarnings.textContent = (configuration.warnings || []).join('\n'); nodes.configFields.replaceChildren(); nodes.configPreview.hidden = true; nodes.restoreConfig.hidden = true; nodes.configResult.textContent = '';
    for (const button of nodes.configForm.querySelectorAll('button')) button.disabled = panelSupport?.mode !== 'full';
    for (const [id, row] of agentRows) row.classList.toggle('selected', id === agentId);
    const modelInputs = [];
    for (const field of configuration.fields) {
      const label = element('label', fieldLabels[field.id] || field.id); const input = element('input'); input.disabled = panelSupport?.mode !== 'full'; input.value = field.value || ''; input.placeholder = t('panel_cli_default'); input.dataset.field = field.id;
      input.addEventListener('input', () => { input.dataset.changed = 'true'; preview = null; nodes.configPreview.hidden = true; });
      const choices = field.id === 'effort' ? choiceList(input, `config-${field.id}-choices`, effortChoices[agentId]) : null;
      if (modelFields.has(field.id)) modelInputs.push(input);
      label.append(input); if (choices) label.append(choices); if (field.validType === false) label.append(element('span', t('panel_config_invalid_type'), 'hint')); nodes.configFields.append(label);
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
  function diffValue(value) { return value?.redacted ? t('panel_config_hidden_value') : value?.state === 'absent' ? t('panel_cli_default') : typeof value?.value === 'string' ? value.value : JSON.stringify(value?.value); }
  async function previewConfig() {
    if (panelSupport?.mode !== 'full') throw new Error(t('panel_config_unavailable'));
    if (!configuration) return; const agentId = configuration.agentId, epoch = configEpoch; const changes = {};
    for (const input of nodes.configFields.querySelectorAll('input')) if (input.dataset.changed) changes[input.dataset.field] = input.value.trim() || null;
    if (!Object.keys(changes).length) throw new Error(t('panel_config_no_changes'));
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
    applied = result.configuration; preview = null; nodes.configPreview.hidden = true; nodes.restoreConfig.hidden = false; nodes.configResult.textContent = t('panel_config_saved');
  }
  async function restoreConfig() {
    if (!applied) return; const plan = applied, epoch = configEpoch; nodes.restoreConfig.disabled = true;
    try { const result = await api('configRestore', { agentId: plan.agentId, planId: plan.planId }); if (epoch !== configEpoch) return;
      nodes.configResult.textContent = result.configuration.phase === 'restoreConflicts' ? t('panel_config_restore_conflicts', result.configuration.conflicts.map(p => p.join('.')).join(', ')) : t('panel_config_restored');
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
      row.append(element('strong',task.name),element('small',[task.enabled?t('panel_schedule_enabled'):t('panel_schedule_disabled'),agentNames[task.agentId],task.lastResult || t('panel_schedule_not_run'),t('panel_schedule_next',new Date(task.nextRun).toLocaleString())].join(' · ')));
      row.append(button(t('panel_edit'),async()=>editSchedule(task)),button(t('panel_schedule_run_now'),async()=>{await api('scheduleRun',{id:task.id});await loadSchedules();await list();}),button(task.enabled?t('panel_disable'):t('panel_enable'),async()=>{await api('scheduleSave',{id:task.id,enabled:!task.enabled});await loadSchedules();}));
      if(task.lastChat) row.append(button(t('panel_schedule_open_run'),async()=>{await list();select(task.lastChat);view(false);}));
      if(task.lastResult==='unconfirmed') row.append(button(t('panel_schedule_acknowledge'),async()=>{await api('scheduleSave',{id:task.id,acknowledgeUnconfirmed:true});await loadSchedules();}));
      row.append(button(t('panel_schedule_delete'),async()=>{await api('scheduleDelete',{id:task.id});await loadSchedules();}));
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
    const skill = nodes.scheduleSkill.value, none = element('option',t('panel_skill_none')); none.value='';
    nodes.scheduleSkill.replaceChildren(none,...skillCatalog.filter(s=>s.enabled).map(s=>{const node=element('option',`${s.name} · ${s.source}`);node.value=s.id;return node;}));
    nodes.scheduleSkill.value=skillCatalog.some(s=>s.id===skill)?skill:'';
  }
  function renderSkills() {
    nodes.skillEntries.replaceChildren(...skillCatalog.map(skill => {
      const row = element('div', undefined, 'skill-entry'); row.append(element('strong', skill.name), element('small', [skill.source, !skill.enabled && t('panel_skill_disabled'), skill.description].filter(part => part !== false).join(' · ')));
      row.append(button(t('panel_skill_use'), async () => { skillChoice = skill; nodes.message.value = `/${skill.name} `; view(false); renderSkillPicker(); }));
      if (skill.editable) row.append(button(t('panel_edit'), async () => {
        const result = await api('skillRead', { agentId: skillAgent(), id: skill.id });
        nodes.skillName.value = skill.name; nodes.skillContent.value = result.skill.instructions;
      }), button(skill.enabled ? t('panel_disable') : t('panel_enable'), async () => {
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
    if (matches.length !== 1) throw new Error(matches.length ? t('panel_skill_ambiguous') : t('panel_skill_unknown'));
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
      item.append(preview, button(t('panel_image_remove'), async () => { images.splice(index, 1); renderImageDrafts(); })); return item;
    }));
  }
  async function addImages(files) {
    if (!panelSupport?.imageAttachments || sending || convertingImages) return;
    const id = selected, list = imageDrafts.get(id) || [];
    if (list.length + files.length > 5) throw new Error(t('panel_image_limit'));
    convertingImages = true; controls();
    try {
      const added = []; for (const file of files) added.push(await encodeImage(file, { convertHeif: panelSupport?.heicConvert ? convertHeif : null }));
      imageDrafts.set(id, [...list, ...added]); if (id === selected) renderImageDrafts();
    } finally { convertingImages = false; nodes.imageFiles.value = ''; controls(); }
  }
  nodes.attachImages.addEventListener('click', () => nodes.imageFiles.click());
  nodes.imageFiles.addEventListener('change', () => void addImages([...nodes.imageFiles.files]).catch(error));
  nodes.message.addEventListener('paste', event => {
    const files = [...(event.clipboardData?.files || [])].filter(file => file.type.startsWith('image/') || isHeif(file));
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
      document.addEventListener?.('visibilitychange', reportPresence);
      void Promise.resolve(windows?.getCurrent?.()).then(current => { if (Number.isInteger(current?.id)) { windowId = current.id; connectPresence(); } }).catch(() => {});
      await refresh();
    } catch (failure) { error(failure); }
  }, refresh, select, send, timeline: () => timeline, close() { closed = true; try { presencePort?.disconnect(); } catch {} clearTurnView(); modelEpoch++; discoverEpoch++; skillsEpoch++; schedulesEpoch++; cancel(timer); selectionEpoch++; configEpoch++; connectionEpoch++; } };
}

if (typeof document !== 'undefined') {
  applyI18n(document);
  const panel = createPanel({ document, runtime: globalThis.chrome?.runtime, storage: globalThis.chrome?.storage, tabs: globalThis.chrome?.tabs, windows: globalThis.chrome?.windows });
  void panel.start();
  globalThis.addEventListener('pagehide', () => panel.close());
}
