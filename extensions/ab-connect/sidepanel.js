import { missingHostHint } from './native-host-hint.js';
import { PanelTimeline, phaseLabels, assessmentLabel, outcomeLabel, stepVerdict, verdictLabels, stepActions, actingPages, notStarted } from './panel-state.js';
import { heifConverter, isHeif, prepareImage } from './image-attachment.js';
import { renderMarkdown } from './markdown.js';
import { oldBuildBlocking } from './panel-errors.js';
import { t, messageMap, applyI18n, i18nReady } from './i18n.js';
import { renderReplies, validReplies } from './inspect-replies.js';
import { createResumePicker, parseResumeCommand, importedView, RESUME_TEXT } from './resume-picker.js';
import { createProjectChoice, folderLabel } from './project-picker.js';
import { grantRefusal, originOf } from './tab-grant.js';
import { panelTabKey, panelTabFrom } from './panel-tabs.js';
import { versionNotices } from './version-notice.js';
import { renderVersionNotices } from './version-banner.js';
import { inspectChipsKey, inspectPickKey, chipTitle, composeMessage, clipboardText, removeChip, withinLabel, splitElementContext, sourceShort } from './inspect-chips.js';

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
// The `/word` being typed at the caret: at the start of the text or after
// whitespace, so "fill it in with /profile-use" completes too. `start` is
// the slash, `end` the caret.
export function slashQuery(text, caret = String(text || '').length) {
  const before = String(text || '').slice(0, caret);
  const match = /(^|\s)\/([^\s/]*)$/.exec(before);
  return match ? { query: match[2], start: before.length - match[2].length - 1, end: before.length } : null;
}
// Every `/name` in a message, where it starts and what it names.
// Punctuation right after a name ("/profile-use," or "/profile-use。") is not part of it.
function slashTokens(text) { return [...String(text || '').matchAll(/(^|\s)\/([^\s/]+)(?=\s|$)/g)].map(m => ({ name: m[2].replace(/[.,;:!?)\]，。；：！？）]+$/, ''), start: m.index + m[1].length })).filter(token => token.name); }

// The host names a conversation "New conversation" until its first message;
// that default is shown in the panel's language, a real title as it is.
const HOST_DEFAULT_TITLE = 'New conversation';
export function titleOf(chat, fallbackKey) {
  return chat?.title && chat.title !== HOST_DEFAULT_TITLE ? chat.title : t(fallbackKey);
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

export function createPanel({ document, runtime, storage, tabs, windows, permissions, scripting, mode = 'panel', initialConversation = '', panelTab = null, setLocation = () => {}, schedule = setTimeout, cancel = clearTimeout, encodeImage = prepareImage, objectUrls = { create: blob => URL.createObjectURL(blob), revoke: url => URL.revokeObjectURL(url) }, subtle = globalThis.crypto?.subtle }) {
  const ids = ['agentsSettingsTab', 'skillsSettingsTab', 'schedulesSettingsTab', 'localAgents', 'scheduleSettings', 'reloadSchedules', 'scheduleEntries', 'scheduleId', 'scheduleName', 'scheduleAgent', 'scheduleModel', 'scheduleEffort', 'scheduleTier', 'scheduleAccess', 'scheduleAutomatic', 'scheduleKind', 'scheduleTiming', 'scheduleSkill', 'schedulePrompt', 'scheduleEnabled', 'saveSchedule', 'newSchedule', 'skillPicker', 'skillLibrary', 'reloadSkills', 'skillEntries', 'skillName', 'skillContent', 'saveSkill', 'skillImportKind', 'skillImportSource', 'importSkill', 'attachmentDrafts', 'inspectChips', 'chipActions', 'copyElements', 'copyInput', 'pickElement', 'pickHint', 'pickCancel', 'chipCard', 'chipLive', 'imageFiles', 'attachImages', 'turnAccess', 'turnEffortLabel', 'turnOptions', 'turnEffort', 'turnTier', 'turnTierLabel', 'saveTurnOptions', 'pendingRecovery', 'retryPending', 'clearPending', 'connection', 'hostHint', 'notice', 'refresh', 'deleteChat', 'deleteConfirm', 'confirmDelete', 'cancelDelete', 'settings', 'chatTab', 'agentsTab', 'chatView', 'agentsView', 'conversation', 'conversationButton', 'conversationTitle', 'conversationMenu', 'scopeSite', 'scopeAll', 'conversationItems', 'newChat', 'pageChip', 'pageTitle', 'pageDetach', 'newOptions', 'access', 'accessLabel', 'modelChip', 'agent', 'model', 'customModel', 'modelStatus', 'inspectAgent', 'executable', 'status', 'phase', 'control', 'effectiveModel', 'transcript', 'empty', 'latest', 'requests', 'composer', 'message', 'queue', 'queuedMessages', 'resume', 'stop', 'sendDirect', 'send', 'recordTask', 'recordTaskLabel', 'discover', 'agentList', 'configuration', 'configTitle', 'configPath', 'configWarnings', 'configForm', 'configFields', 'configPreview', 'configDiff', 'applyConfig', 'discardConfig', 'restoreConfig', 'configResult', 'closeConfig', 'settingsTabs', 'skillEditor', 'scheduleEditor', 'scheduleEditorTitle', 'cancelSchedule'];
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
  let pendingBinding = null, sending = false, configuration = null, configEpoch = 0, preview = null, applied = null;
  const rendered = new Map(), requestNodes = new Map(), queuedNodes = new Map();
  // Ids in the host's queue as of the last read; those messages show in the
  // queue area only, not as sent turns.
  let queuedIds = new Set();
  // Nodes added after the id list; looked up apart so the list stays stable.
  const working = document.getElementById('working'), suggestions = document.getElementById('suggestions');
  // "Open in a tab": the same panel as a full-page tab. There is no page
  // beside it, so "this page" is a web tab of its window: the last one the
  // user was on, or the one they pick.
  const tabMode = mode === 'tab';
  // One panel per tab (0.5.63): this panel belongs to `panelTab`, shows that
  // tab's conversation and means that tab by "this page".
  const tabBound = !tabMode && Number.isInteger(panelTab);
  const openInTab = document.getElementById('openInTab'), pagePicker = document.getElementById('pagePicker'), pageLabel = document.getElementById('pageLabel');
  let ownTabId = null, pickedTabId = null;
  // Inspect & Ask → chat: elements picked on this tab wait here as chips
  // (written by the background under this tab's key) until the next send.
  const chipKey = tabBound ? inspectChipsKey(panelTab) : null;
  let chipRecord = { items: [], question: '' };
  // The Pick button's picker on this tab is running (set by the background
  // under its own key; shown at once on click, before it confirms).
  let picking = false, shownChipIds = null;
  // "Let the agent use this tab": grants live in the background (per tab,
  // per conversation); every view of a conversation shows the same ones.
  const grantBar = document.getElementById('grantBar'), grantText = document.getElementById('grantText'), grantNote = document.getElementById('grantNote'), grantRevoke = document.getElementById('grantRevoke'), grantTab = document.getElementById('grantTab');
  let grants = [], grantBusy = false;
  // "Agent works in" (0.5.63): in a tab's panel the agent works in that tab
  // unless the conversation (or the default in Options) says its own window.
  // The tab is lent automatically, with the same badge, bar and take-back as
  // a manual lend; a page that cannot be lent, a take-back or a refused lend
  // leaves the agent in its own window, and the panel says so.
  const workRow = document.getElementById('workRow'), workInSelect = document.getElementById('workIn'), workNote = document.getElementById('workNote');
  let workInDefault = 'tab', workInMap = {}, newWorkIn = null, autoNote = '', panelTabUrl = '', pageRead = false;
  // Lends tried, by conversation, tab and origin, and the lend in flight for
  // each, so a send waits for the lend a poll started.
  const autoTried = new Map();
  function workInFor(id) { return id ? workInMap[id] || workInDefault : newWorkIn || workInDefault; }
  // The empty state says where the agent will work: this tab (the default
  // since 0.5.63) or, when set so or outside a tab's panel, its own window.
  function renderEmptyDetail() {
    const detail = document.getElementById('emptyDetail');
    if (detail) detail.textContent = t(tabBound && workInFor('') === 'tab' ? 'sidepanel_empty_detail' : 'panel_empty_detail_window');
  }
  async function setWorkIn(id, value) {
    if (!id) { newWorkIn = value; renderEmptyDetail(); return; }
    workInMap[id] = value;
    // Merged with what other panels saved meanwhile.
    const stored = (await Promise.resolve(storage?.local?.get?.('ab_panel_work_in')).catch(() => null))?.ab_panel_work_in || {};
    workInMap = { ...stored, [id]: value };
    await storage?.local?.set?.({ ab_panel_work_in: workInMap });
  }
  const grantRefusedText = messageMap({ notWeb: 'panel_grant_refused_not_web', webStore: 'panel_grant_refused_web_store', otherConversation: 'panel_grant_refused_other', agentTab: 'panel_grant_refused_agent_tab', closed: 'panel_grant_refused_closed' });
  // Consecutive tool calls share one collapsible group; `toolTail` is the
  // group the next tool joins while nothing else has been rendered after it.
  let toolTail = null; const toolGroups = new Set();
  // Turn view: one card per user message, used only when the host reports
  // turn evidence. Cards are keyed by the projected turn object.
  const turnCards = new Map(), liveUrls = new Set(), dismissedStalls = new Set();
  let viewMode = null, forkView = null;
  const turnView = () => Boolean(panelSupport?.turnEvidence);
  // `/resume`: continue a CLI session started outside the panel.
  const resumePicker = createResumePicker({
    document, api, onError: error, relativeTime, schedule, cancel,
    agents: () => (panelSupport?.chatAgents || ['codex', 'claude']).filter(id => chatAgents.has(id)).map(id => ({ id, name: agentNames[id] })),
    preferredAgent: () => conversation?.agentId || nodes.agent.value || preferredAgent,
    // The page's project (its repository's checkout or the site's) goes first.
    preferredProject: () => projectChoice?.preferred || '',
    executable: agentId => agentId === nodes.agent.value ? nodes.executable.value.trim() : '',
    onResumed: async (resumed, existing, refused) => {
      await list(); rememberAgent(conversations.find(c => c.id === resumed.id)?.agentId || preferredAgent); select(resumed.id);
      // The session's folder could not be the project (a system or home folder).
      if (refused) error(t('panel_resume_project_refused', refused));
    },
  });
  document.getElementById('resumeHost')?.append(resumePicker.container);
  // The project folder a new conversation's agent works in.
  const projectChip = document.getElementById('projectChip'), projectPath = document.getElementById('projectPath');
  const projectChoice = createProjectChoice({
    document, api, onError: error, select: document.getElementById('project'), host: document.getElementById('projectHost'),
    supported: () => Boolean(panelSupport?.projectDirectory), canRemember: () => Boolean(panelSupport?.inspect),
    matchesRepositories: () => (panelSupport?.projectDirectory || 0) >= 2,
    // What the page calls its project: the tab title, then its first heading
    // when the extension may read the site (never through the debugger).
    pageName: async target => {
      const title = target?.title || '';
      if (!Number.isInteger(target?.tabId) || !scripting?.executeScript) return [title];
      const read = scripting.executeScript({ target: { tabId: target.tabId, frameIds: [0] }, func: () => (document.querySelector('h1')?.innerText || '').trim().slice(0, 200) })
        .then(([result]) => [title, result?.result || ''], () => [title]);
      return Promise.race([read, new Promise(resolve => setTimeout(() => resolve([title]), 800))]);
    },
  });
  function refreshProjects() { if (!selected && connected) void projectChoice.refresh(page).catch(() => {}); }
  let importedNode = null, importedCount = -1;
  async function openResume(query = '') {
    if (!panelSupport?.sessionResume) throw new Error(RESUME_TEXT.updateCli);
    closeConversationMenu(); nodes.skillPicker.hidden = true; nodes.message.value = '';
    await resumePicker.open(query);
  }
  // The imported block sits above everything else and is rebuilt only when
  // more of it arrived.
  function renderImported() {
    const imported = timeline.imported;
    const count = imported ? imported.messages.length + (imported.end ? 1 : 0) : 0;
    if (count === importedCount) return;
    importedNode?.remove?.(); importedCount = count;
    importedNode = importedView(document, imported, { agentName: id => agentNames[id] || id || '', renderMarkdown });
    if (importedNode) nodes.transcript.insertBefore(importedNode, nodes.transcript.children[0] || null);
  }

  // Keep only correlation IDs in extension storage. Chat text lives in the
  // native journal; closing the view must not turn an uncertain send into a retry.
  // Only `id`'s entry changes: the side panel and a panel tab share the map.
  async function savePending(id) {
    if (!storage?.local?.set) throw new Error(t('panel_error_pending_storage'));
    const stored = { ...((await storage.local.get?.('ab_panel_pending'))?.ab_panel_pending || {}) };
    if (pendingMessages.has(id)) stored[id] = pendingMessages.get(id).id; else delete stored[id];
    await storage.local.set({ ab_panel_pending: stored });
  }

  function element(tag, text, className) {
    const node = document.createElement(tag);
    if (text !== undefined) node.textContent = text;
    if (className) node.className = className;
    return node;
  }
  function error(error) {
    const text = String(error?.message || error);
    const blocking = oldBuildBlocking(text);
    if (!blocking) { nodes.notice.textContent = text; nodes.notice.hidden = false; return; }
    // An older build keeps its service while one of its conversations works
    // or waits for the user; name them and offer to open or stop each.
    const title = id => titleOf(conversations.find(c => c.id === id), 'panel_conversation_untitled');
    nodes.notice.replaceChildren(element('div', t('panel_old_build_busy')));
    for (const id of blocking) {
      const row = element('div', `“${title(id)}” `);
      row.append(button(t('panel_old_build_open'), async () => { clearError(); await openBlocking(id); }), button(t('panel_old_build_stop'), async () => {
        try { await api('conversationStop', { conversationId: id }); clearError(); } catch (failure) { error(failure); }
      }));
      nodes.notice.append(row);
    }
    // Or take over from a terminal: `upgrade -f` stops the old build's
    // conversations and its service without asking.
    const command = 'chrome-use upgrade -f', copy = button(t('panel_copy_command'), async () => {
      await navigator.clipboard.writeText(command); copy.textContent = t('panel_copied');
    });
    const force = element('div', undefined, 'force-takeover');
    force.append(element('div', t('panel_old_build_force_hint')), element('code', command), copy);
    nodes.notice.append(force);
    nodes.notice.hidden = false;
  }
  // Open a conversation the error named, wherever it was started: the list
  // switches to all sites when this site's list does not show it.
  async function openBlocking(id) {
    if (!conversations.some(c => c.id === id)) await list().catch(() => {});
    const domain = currentDomain(), chat = conversations.find(c => c.id === id);
    if (scope === 'site' && domain && !(chat?.site && mainDomain(chat.site) === domain)) setScope('all');
    select(id);
  }
  function clearError() { nodes.notice.hidden = true; nodes.notice.textContent = ''; }
  async function api(operation, params = {}) {
    if (!runtime?.sendMessage) throw new Error(t('panel_error_open_from_extension'));
    const reply = await runtime.sendMessage({ type: 'ab-agent-request', operation, params });
    if (!reply?.success) throw new Error(reply?.error || t('panel_error_host_unconfirmed'));
    return reply.result;
  }
  const convertHeif = heifConverter(api);
  // Version banners: what needs doing about the CLI or this extension. The
  // "updated to X" note stays in the popup; the panel keeps to actionable ones.
  function renderVersions(state) {
    const notices = versionNotices({ cli: state?.cli, legacyCli: state?.legacyCli === true, platform: globalThis.navigator?.userAgentData?.platform || globalThis.navigator?.platform || '', cliUpgrade: state?.cliUpgrade, update: state?.update, extensionVersion: state?.extensionVersion }).filter(notice => notice.id !== 'ext-updated');
    renderVersionNotices(document.getElementById('versionNotices'), notices, { document, t, onAction: async action => {
      if (!runtime?.sendMessage) return;
      const reply = await runtime.sendMessage({ type: action === 'upgradeCli' ? 'ab-cli-upgrade' : 'ab-apply-update' });
      if (!reply?.success) error(new Error(reply?.error || t('popup_update_failed')));
    } });
  }
  function renderConnection(state) {
    renderVersions(state);
    connected = state?.connected === true;
    panelSupport = state?.agentPanel?.schemaVersion === 1 ? state.agentPanel : null;
    nodes.connection.classList.toggle('disconnected', !connected);
    nodes.connection.classList.toggle('ok', connected && panelSupport?.mode === 'full');
    nodes.connection.textContent = connected ? t('panel_connection_connected') : state?.connectionState === 'connecting' ? t('panel_connection_waiting') : state?.connectionError || t('panel_connection_unavailable');
    const hostHint = !connected && state?.connectionState !== 'connecting' ? missingHostHint(state?.connectionError) : null;
    nodes.hostHint.hidden = !hostHint;
    nodes.hostHint.replaceChildren();
    if (hostHint) {
      const copy = button(t('panel_copy_command'), async () => {
        try {
          await navigator.clipboard.writeText(hostHint.command);
          copy.textContent = t('panel_copied');
        } catch {
          copy.textContent = t('panel_copy_failed');
        }
      });
      nodes.hostHint.append(element('p', t(hostHint.messageKey)), element('code', hostHint.command), copy);
    }
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
    // One tab is no choice; the bar only appears when there is somewhere to go.
    nodes.settingsTabs.hidden = !panelSupport?.skillLibrary && !panelSupport?.schedules;
    nodes.localAgents.hidden = settingsSection !== 'agents';
    nodes.skillLibrary.hidden = !panelSupport?.skillLibrary || settingsSection !== 'skills';
    nodes.scheduleSettings.hidden = !panelSupport?.schedules || settingsSection !== 'schedules';
    nodes.agentsView.dataset.section = settingsSection;
    nodes.newOptions.hidden = Boolean(selected); nodes.modelChip.hidden = Boolean(selected);
    if (projectChip) projectChip.hidden = Boolean(selected) || !panelSupport?.projectDirectory;
    if (projectPath) {
      // Fixed at creation: shown, never edited.
      projectPath.hidden = !conversation?.project;
      projectPath.textContent = conversation?.project ? t('panel_project_header', folderLabel(conversation.project).split(' · ')[0]) : '';
      projectPath.title = conversation?.project || '';
    }
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
    nodes.status.classList.toggle('idle', !conversation || (conversation.phase === 'idle' && !conversation.model && !conversation.project));
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
    if (openInTab) openInTab.hidden = tabMode || !runtime?.getURL || !tabs?.create;
    renderGrant();
  }
  function applyPendingBinding() {
    if (pendingBinding === null) return;
    const next = pendingBinding; pendingBinding = null;
    if (next !== selected) select(next);
  }
  function grantFor(id) { return id ? grants.find(grant => grant.conversationId === id) || null : null; }
  // The bar says which tab the agent may use and what page it shows now; the
  // button offers the page this view means by "this page".
  function renderGrant() {
    if (!grantBar) return;
    const grant = grantFor(selected), supported = Boolean(panelSupport?.tabGrant) && panelSupport?.mode === 'full';
    grantBar.hidden = !grant;
    if (grant) {
      grantText.textContent = t('panel_grant_active', grant.title || grant.url, originOf(grant.url) || grant.url);
      grantText.title = grant.url;
      const refusal = grantRefusal(grant.url);
      grantNote.hidden = !refusal; grantNote.textContent = refusal ? t('panel_grant_page_unusable') : '';
      grantRevoke.disabled = grantBusy;
    }
    grantTab.hidden = !supported || !selected || !page || pageDetached || grant?.tabId === page.tabId;
    if (workRow) {
      workRow.hidden = !tabBound || !supported;
      workInSelect.value = workInFor(selected);
      workInSelect.disabled = grantBusy;
      workNote.hidden = !autoNote; workNote.textContent = autoNote;
    }
    grantTab.textContent = grant ? t('panel_grant_switch') : t('panel_grant_use');
    grantTab.title = page ? t('panel_grant_use_title', page.url) : '';
    grantTab.disabled = grantBusy || !connected || !conversation;
  }
  async function grantMessage(message) {
    if (!runtime?.sendMessage) throw new Error(t('panel_error_open_from_extension'));
    const reply = await runtime.sendMessage({ type: 'ab-tab-grant', ...message });
    if (!reply?.success) throw new Error(grantRefusedText[reply?.reason] || reply?.error || t('panel_error_host_unconfirmed'));
    return reply;
  }
  async function loadGrants() {
    if (!runtime?.sendMessage) return;
    const reply = await runtime.sendMessage({ type: 'ab-tab-grant', action: 'list' });
    if (reply?.success && Array.isArray(reply.grants)) { grants = reply.grants; renderGrant(); }
  }
  // The conversation and tab are fixed by the caller: an awaited step before
  // this (an automatic lend revoking the tab's previous holder) may have
  // changed the selection, and the tab must never go to that other one.
  async function grantPage(id = selected, tabId = page?.tabId) {
    if (!id || !Number.isInteger(tabId) || grantBusy) return;
    if (tabBound && tabId === panelTab) { await setWorkIn(id, 'tab'); if (id === selected) autoNote = ''; }
    grantBusy = true; renderGrant();
    try {
      const reply = await grantMessage({ action: 'grant', tabId, conversationId: id, conversationTitle: titleOf(conversations.find(c => c.id === id), 'panel_conversation_untitled') });
      grants = [...grants.filter(g => g.conversationId !== id && g.tabId !== tabId), reply.grant];
      // The prompt names the tab either way; the session not taking it now
      // only means a command without a tab handle still runs elsewhere.
      if (reply.service?.adopted === false && id === selected) error(t('panel_grant_not_adopted', reply.service.error || t('panel_no_answer')));
    } finally { grantBusy = false; renderGrant(); }
  }
  async function revokeGrant(id = selected) {
    if (!grantFor(id)) return;
    grantBusy = true; renderGrant();
    try {
      await grantMessage({ action: 'revoke', conversationId: id });
      grants = grants.filter(g => g.conversationId !== id);
    } finally { grantBusy = false; renderGrant(); }
  }
  // "Take back the tab" in a tab's panel also moves the conversation to the
  // agent's own window, so the tab is not lent again behind the user's back.
  async function takeBack() {
    const id = selected;
    await revokeGrant(id);
    // Remembered in every view: taking the tab back in a panel tab must stop
    // the side panel from lending it again on its own.
    if (id) { await setWorkIn(id, 'window'); if (id === selected && tabBound) autoNote = t('panel_work_in_window_taken_back'); renderGrant(); }
  }
  // Lend this panel's tab to its conversation when it works in this tab.
  async function maybeAutoLend() {
    if (!tabBound || !panelSupport?.tabGrant || panelSupport?.mode !== 'full' || !connected || !selected || !conversation || grantBusy) return;
    const id = selected;
    if (workInFor(id) !== 'tab') { autoNote = ''; renderGrant(); return; }
    if (grantFor(id)?.tabId === panelTab) { autoNote = ''; renderGrant(); return; }
    // Until the tab was read there is nothing to decide.
    if (!pageRead) return;
    const refusal = panelTabUrl ? grantRefusal(panelTabUrl) : 'notWeb';
    if (refusal || !page) { autoNote = refusal === 'webStore' ? t('panel_work_in_window_web_store') : t('panel_work_in_window_page'); renderGrant(); return; }
    const key = `${id}:${panelTab}:${originOf(page.url)}`;
    if (autoTried.has(key)) return autoTried.get(key);
    const lending = autoLend(id);
    autoTried.set(key, lending);
    return lending;
  }
  async function autoLend(id) {
    try {
      // This tab may still be lent to the conversation it showed before.
      const holder = grants.find(g => g.tabId === panelTab && g.conversationId !== id);
      if (holder) { await grantMessage({ action: 'revoke', tabId: panelTab }); grants = grants.filter(g => g !== holder); }
      // Another conversation was selected meanwhile: it lends nothing by itself.
      if (id !== selected || workInFor(id) !== 'tab') return;
      await grantPage(id, panelTab);
      if (id === selected) autoNote = '';
    } catch (failure) {
      if (id === selected) autoNote = t('panel_work_in_window_failed', failure?.message || t('panel_no_answer'));
    } finally { renderGrant(); }
  }
  workInSelect?.addEventListener('change', () => void (async () => {
    const id = selected, value = workInSelect.value === 'window' ? 'window' : 'tab';
    await setWorkIn(id, value);
    if (value === 'window') { autoNote = ''; if (grantFor(id)?.tabId === panelTab) await revokeGrant(id); }
    else { for (const key of [...autoTried.keys()]) if (key.startsWith(`${id}:`)) autoTried.delete(key); await maybeAutoLend(); }
    renderGrant();
  })().catch(error));
  function conversationChoices() {
    nodes.conversation.replaceChildren(element('option', t('panel_new_conversation')));
    nodes.conversation.firstElementChild.value = '';
    for (const chat of conversations) { const option = element('option', titleOf(chat, 'panel_conversation_untitled')); option.value = chat.id; nodes.conversation.append(option); }
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
    nodes.conversationTitle.textContent = titleOf(chat, 'panel_new_conversation');
    const domain = currentDomain(), siteScope = scope === 'site' && Boolean(domain);
    nodes.scopeSite.textContent = domain ? t('panel_scope_site_domain', domain) : t('panel_scope_site');
    nodes.scopeSite.disabled = !domain;
    nodes.scopeSite.setAttribute('aria-selected', String(siteScope)); nodes.scopeAll.setAttribute('aria-selected', String(!siteScope));
    const shown = siteScope ? conversations.filter(c => c.site && mainDomain(c.site) === domain) : conversations;
    const newItem = element('button', undefined, 'conversation-item new'); newItem.type = 'button';
    newItem.append(element('span', t('panel_new_conversation_item'), 'title'));
    newItem.addEventListener('click', () => { closeConversationMenu(); nodes.conversation.value = ''; select(''); loadModels(); });
    const items = [newItem];
    if (panelSupport?.sessionResume) {
      const resumeItem = element('button', undefined, 'conversation-item new'); resumeItem.type = 'button';
      resumeItem.append(element('span', RESUME_TEXT.menuItem, 'title'));
      resumeItem.addEventListener('click', () => void openResume().catch(error));
      items.push(resumeItem);
    }
    for (const c of shown) {
      const item = element('div', undefined, `conversation-item${c.id === selected ? ' selected' : ''}`);
      const choose = element('button', undefined, 'conversation-choose'); choose.type = 'button';
      item.setAttribute('role', 'group');
      item.setAttribute('aria-label', titleOf(c, 'panel_conversation_untitled'));
      choose.setAttribute('aria-pressed', String(c.id === selected));
      choose.append(element('span', titleOf(c, 'panel_conversation_untitled'), 'title'), element('span', [agentNames[c.agentId] || c.agentId, !siteScope && c.site, relativeTime(c.updated)].filter(Boolean).join(' · '), 'meta'));
      choose.addEventListener('click', () => { closeConversationMenu(); select(c.id); });
      const jump = element('button', undefined, 'conversation-jump'); jump.type = 'button';
      jump.title = t('panel_show_window');
      jump.setAttribute('aria-label', `${t('panel_show_window')}: ${titleOf(c, 'panel_conversation_untitled')}`);
      jump.innerHTML = '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M14 4h6v6M20 4l-9 9"/><path d="M10 4H5a1 1 0 0 0-1 1v14a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1v-5"/></svg>';
      jump.addEventListener('click', () => void (async () => {
        if (jump.disabled) return;
        jump.disabled = true;
        try {
          const reply = await runtime.sendMessage({ type: 'ab-conversation-window', conversationId: c.id });
          if (!reply?.success) throw new Error(reply?.error || t('panel_window_unconfirmed'));
          closeConversationMenu();
        } catch (failure) { closeConversationMenu(); error(failure); }
        finally { jump.disabled = false; }
      })());
      item.append(choose, jump);
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
    timeline = new PanelTimeline(); queuedIds = new Set(); queuedNodes.clear(); nodes.queuedMessages.replaceChildren(); nodes.queuedMessages.hidden = true; rendered.clear(); requestNodes.clear(); nodes.requests.replaceChildren();
    toolTail = null; toolGroups.clear(); clearTurnView(); importedNode = null; importedCount = -1;
    nodes.transcript.replaceChildren(nodes.empty); nodes.empty.hidden = false; controls(); renderConversationMenu();
    // A panel tab keeps its conversation in its own address, so opening one
    // never changes what the side panel shows next.
    if (tabMode) setLocation(id); else if (tabBound) void bindTab(id).catch(() => {}); else storage?.local?.set({ ab_panel_conversation: id }).catch(() => {});
    if (!id) refreshProjects(); else projectChoice.hideQuestion();
    reportPresence();
    void poll().catch(error);
  }
  // This tab now shows `id`; a conversation is shown by one tab at a time, so
  // another tab that showed it lets it go (its panel starts a new one).
  async function bindTab(id) {
    const key = panelTabKey(panelTab), session = storage?.session;
    if (!session) return;
    if (!id) { await session.remove(key); return; }
    await session.set({ [key]: id });
    const all = await session.get(null);
    const others = Object.entries(all || {}).filter(([other, value]) => other !== key && other.startsWith('ab_panel_tab_') && value === id).map(([other]) => other);
    if (others.length) await session.remove(others);
  }
  // Tell the background which conversation this panel shows, so a turn the
  // user is watching does not notify and its badge count clears.
  let presencePort = null, windowId = null;
  function reportPresence() {
    if (!presencePort || windowId === null) return;
    const visible = document.visibilityState !== 'hidden' && !nodes.chatView.hidden;
    const repliesShown = visible && replyCount > 0 && replyBox.open === true;
    try { presencePort.postMessage({ type: 'presence', windowId, ...(tabBound ? { tabId: panelTab } : {}), conversationId: selected, visible, ...(repliesShown ? { repliesShown } : {}) }); } catch {}
  }
  // Inspect & Ask replies: agents answer batches sent from the page picker,
  // and hosts that report `inspectReplies` keep them for this list.
  const replyBox = document.getElementById('inspectReplies'), replyList = document.getElementById('inspectReplyList');
  let replyCount = 0, replyTimer = null;
  async function loadReplies() {
    if (!panelSupport?.inspectReplies) { replyBox.hidden = true; replyCount = 0; return; }
    replyCount = renderReplies(document, replyList, validReplies(await api('inspectReplies')), renderMarkdown, { showBatch: true });
    replyBox.hidden = !replyCount;
    reportPresence();
  }
  function replyLoop() {
    cancel(replyTimer);
    replyTimer = schedule(() => { if (!closed) void loadReplies().catch(() => {}).finally(replyLoop); }, 20000);
  }
  function showReplies() {
    replyBox.open = true;
    void loadReplies().then(() => replyBox.scrollIntoView?.({ block: 'nearest' })).catch(error);
  }
  replyBox.addEventListener('toggle', reportPresence);
  function connectPresence() {
    if (closed || !runtime?.connect) return;
    try { presencePort = runtime.connect({ name: 'ab-panel-presence' }); } catch { return; }
    presencePort.onMessage.addListener(message => {
      if (message?.type === 'select' && typeof message.conversationId === 'string') select(message.conversationId);
      if (message?.type === 'showReplies') showReplies();
    });
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
      if (viewMode) { rendered.clear(); toolTail = null; toolGroups.clear(); clearTurnView(); importedNode = null; importedCount = -1; nodes.transcript.replaceChildren(nodes.empty); }
      viewMode = mode;
    }
    renderImported();
    if (mode === 'turns') { renderTurns(); return; }
    const follow = nodes.transcript.scrollHeight - nodes.transcript.scrollTop - nodes.transcript.clientHeight < 80;
    nodes.empty.hidden = timeline.rows.length > 0 || Boolean(timeline.imported);
    // A tool still open when the response ended is not running any more.
    const live = busyPhases.has(conversation?.phase);
    for (const row of timeline.rows) {
      const waiting = row.type === 'user' && !row.queueStatus && queuedIds.has(row.messageId);
      let view = rendered.get(row.key);
      if (!view && waiting) continue;
      if (!view) {
        if (row.type === 'tool') {
          view = toolView(row); const { container } = view;
          if (!toolTail) toolTail = toolGroup();
          toolTail.tools.push(view); toolTail.list.append(container); toolGroups.add(toolTail);
          rendered.set(row.key, view);
        } else view = messageView(row, row.type !== 'user' || !turnView());
        if (row.type !== 'tool') { toolTail = null; rendered.set(row.key, view); nodes.transcript.append(view.container); }
      }
      view.container.hidden = waiting;
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
      const prompt = [...timeline.rows].reverse().find(r => r.type === 'user' && !r.queueStatus && !queuedIds.has(r.messageId))?.text;
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
      else {
        // Picked elements ride in the message; the transcript folds them.
        const picked = row.type === 'user' ? splitElementContext(row.text) : null;
        view.content.textContent = picked ? picked.words : row.text;
        if (picked) { const fold = element('details', undefined, 'inspect-context'); fold.append(element('summary', t('panel_inspect_context_summary', picked.count)), element('pre', picked.block)); view.content.append(fold); }
      }
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
    nodes.empty.hidden = timeline.rows.length > 0 || Boolean(timeline.recovery.context) || Boolean(timeline.imported);
    const live = busyPhases.has(conversation?.phase);
    renderForkContext();
    for (const id of timeline.turnOrder) {
      const turn = timeline.turns.get(id);
      let card = turnCards.get(turn);
      const waiting = notStarted(turn, queuedIds);
      if (card) card.container.hidden = waiting;
      if (waiting) continue;
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
        // A title set by a message sent from another view (a panel tab, the
        // side panel) shows here too.
        const listed = conversations.find(chat => chat.id === id);
        if (listed && typeof conversation?.title === 'string' && listed.title !== conversation.title) { listed.title = conversation.title; conversationChoices(); }
        const queued = result.queuedMessages || []; queuedIds = new Set(queued.map(message => message.id));
        renderTimeline(); renderRequests(); renderQueue(queued); controls();
      }
      if (id === selected && epoch === selectionEpoch) void maybeAutoLend();
      const pending = pendingMessages.get(id);
      if (pending && id === selected && epoch === selectionEpoch) {
        const status = await api('conversationMessageStatus', { conversationId: id, messageId: pending.id });
        if (pendingMessages.get(id) === pending && status.message.known) {
          pendingMessages.delete(id);
          await savePending(id);
          if (id === selected && epoch === selectionEpoch) { if (nodes.message.value === (pending.typed ?? pending.text)) { nodes.message.value = ''; imageDrafts.delete(id); renderImageDrafts(); } controls(); }
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
      if (pendingMessages.get(id) === pending) { pendingMessages.delete(id); await savePending(id); }
      await poll();
    } finally { sending = false; controls(); applyPendingBinding(); }
  }
  nodes.retryPending.addEventListener('click', () => void recoverPending(true).catch(error));
  nodes.clearPending.addEventListener('click', () => void recoverPending(false).catch(error));
  async function send(direct = false) {
    if (panelSupport?.mode !== 'full') throw new Error(t('panel_error_cli_required'));
    const chips = [...chipRecord.items];
    if (!connected || sending || convertingImages || pendingMessages.has(selected) || conversation?.phase === 'recoveryRequired' || (!nodes.message.value.trim() && !chips.length)) return;
    // A built-in command, looked up before skills.
    const command = parseResumeCommand(nodes.message.value);
    if (command) { await openResume(command.query); return; }
    if (direct && conversation?.directMessageId) throw new Error(t('panel_error_direct_waiting'));
    const typed = nodes.message.value, chosen = chosenSkill(typed);
    const images = [...(imageDrafts.get(selected) || [])];
    sending = true; clearError(); controls();
    try {
      // Picked elements ride in the message: their context after the user's
      // words, their screenshots after the user's own images.
      const shots = [], imageNumbers = [];
      if (panelSupport?.imageAttachments) {
        for (const [index, chip] of chips.entries()) {
          const match = chip.image && /^data:(image\/(?:png|jpeg));base64,(.+)$/.exec(chip.image);
          if (!match || images.length + shots.length >= 5) continue;
          try { shots.push(await encodeImage(new Blob([base64Bytes(match[2])], { type: match[1] }))); imageNumbers[index] = images.length + shots.length; } catch {}
        }
      }
      const text = composeMessage(typed, chips, { tabId: panelTab, imageNumbers });
      if (!selected) {
        const options = { agentId: nodes.agent.value };
        const model = nodes.customModel.value.trim() || nodes.model.value;
        if (model) options.model = model;
        if (panelSupport?.conversationOptions) options.options = readTurnOptions();
        if (nodes.executable.value.trim()) options.executable = nodes.executable.value.trim();
        if (panelSupport?.fullAccess) options.fullAccess = newFullAccess;
        const project = panelSupport?.projectDirectory ? projectChoice.value() : '';
        if (project) options.project = project;
        const result = await api('conversationCreate', options); rememberAgent(options.agentId);
        conversations.unshift(result.conversation); conversationChoices(); select(result.conversation.id, true); nodes.message.value = typed;
        // Where it works was chosen before it existed; its first turn already
        // runs in this tab when it works here.
        if (tabBound) { await setWorkIn(result.conversation.id, workInFor('')); newWorkIn = null; await maybeAutoLend(); }
        // After selecting it: selecting another conversation hides the question.
        if (project) { void projectChoice.created(result.conversation.project || project, page).catch(error); projectChoice.reset(); }
      }
      const id = selected, pending = { id: crypto.randomUUID(), text, typed, direct };
      imageDrafts.set(id, images); if (images.length) imageDrafts.delete(''); renderImageDrafts();
      const params = { conversationId: id, messageId: pending.id, text };
      const attached = attachedPage(); if (attached) params.page = attached;
      // The service hears the grant with every message, so one that
      // restarted since the grant still names the tab.
      if (panelSupport?.tabGrant) { const grant = grantFor(id); params.tabGrant = grant ? { tabId: grant.tabId, url: grant.url.slice(0, 8192), title: (grant.title || grant.url).slice(0, 1024) } : null; }
      // Recording consent is per message; the host refuses to record without it.
      if (panelSupport?.recording && nodes.recordTask.checked) params.record = true;
      if (images.length || shots.length) {
        params.attachments = [];
        for (const image of [...images, ...shots]) {
          const uploaded = await api('conversationAttachmentPut', { conversationId: id, data: image.data });
          params.attachments.push(uploaded.attachment);
        }
      }
      if (chosen) {
        const snapshot = await api('conversationSkillSelect', { conversationId: id, skillId: chosen.id });
        params.skillId = snapshot.skill.id;
      }
      pending.params = { ...params }; delete pending.params.conversationId; delete pending.params.messageId; delete pending.params.text; delete pending.params.typed;
      pendingMessages.set(id, pending); controls();
      try { await savePending(id); } catch (failure) { pendingMessages.delete(id); throw failure; }
      const result = await api(direct ? 'conversationSendDirect' : 'conversationSend', params);
      pageDetached = false; renderPage(); nodes.recordTask.checked = false;
      if (selected === id) { conversation = result.conversation; if (nodes.message.value === typed) nodes.message.value = ''; }
      imageDrafts.delete(id); if (images.length) imageDrafts.delete(''); renderImageDrafts();
      if (chips.length) await dropChips(chips.map(chip => chip.id));
      pendingMessages.delete(id); drafts.delete(id); await savePending(id); await list(); await poll();
    } finally { sending = false; controls(); applyPendingBinding(); }
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
    // A panel tab says where "this page" comes from and lets the user change it.
    nodes.pageTitle.hidden = tabMode;
    if (pagePicker) pagePicker.hidden = !tabMode;
    if (pageLabel) pageLabel.hidden = !tabMode;
    renderGrant();
  }
  async function refreshPage() {
    if (!(tabBound ? tabs?.get : tabs?.query)) return;
    const tab = tabMode ? await windowPage() : tabBound ? await Promise.resolve(tabs.get?.(panelTab)).catch(() => null) : (await tabs.query({ active: true, currentWindow: true }))[0];
    const next = tab && typeof tab.id === 'number' && /^https?:/.test(tab.url || '') ? { tabId: tab.id, url: tab.url, title: tab.title || tab.url } : null;
    if (tabBound) { panelTabUrl = tab?.url || tab?.pendingUrl || ''; pageRead = true; renderPick(); }
    if (next?.tabId !== page?.tabId) pageDetached = false;
    const moved = next?.url !== page?.url;
    page = next; renderPage(); renderConversationMenu();
    if (moved) refreshProjects();
    if (moved || tabBound) void maybeAutoLend();
  }
  // In a panel tab: the web tab of this window the user picked, else the one
  // they were on last. The picker lists them all.
  async function windowPage() {
    const list = Number.isInteger(windowId) ? await tabs.query({ windowId }) : [];
    const web = list.filter(tab => tab.id !== ownTabId && /^https?:/.test(tab.url || ''));
    let chosen = web.find(tab => tab.id === pickedTabId);
    if (!chosen) { pickedTabId = null; chosen = [...web].sort((a, b) => (b.lastAccessed || 0) - (a.lastAccessed || 0))[0] || null; }
    if (pagePicker) {
      pagePicker.replaceChildren(...web.map(tab => { const option = element('option', `${tab.title || tab.url} · ${hostOf(tab.url)}`); option.value = String(tab.id); return option; }));
      pagePicker.value = chosen ? String(chosen.id) : '';
      pagePicker.title = chosen ? t(pickedTabId === null ? 'panel_page_tab_last_active' : 'panel_page_tab_picked', chosen.url) : '';
    }
    return chosen;
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
      void revokeGrant(id).catch(() => {});
      pendingMessages.delete(id); drafts.delete(id); await savePending(id);
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
    const mark = state => {
      if (!row) return;
      const open = configuration?.agentId === agent.id && !nodes.configuration.hidden;
      row.className = `agent-row ${state}${open ? ' selected' : ''}`;
      // Agents you can chat with move up, ahead of the ones still being checked.
      const rows = Array.from(nodes.agentList.children);
      const first = state === 'ready' && rows.includes(row) ? rows.find(other => other !== row && other !== nodes.configuration && !/\bready\b/.test(other.className)) : null;
      if (first && rows.indexOf(first) < rows.indexOf(row)) { nodes.agentList.insertBefore(row, first); if (open) placeConfiguration(agent.id); }
    };
    if (!agent.executable) { mark(agent.configFiles?.length ? 'missing config-only' : 'missing'); line.textContent = agent.configFiles?.length ? t('panel_agent_config_only') : t('panel_agent_not_installed'); return; }
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
    // Agents that are not installed collapse into chips under the list.
    const missingBox = document.getElementById('agentMissing'), missingList = document.getElementById('agentMissingList');
    missingList?.replaceChildren();
    for (const agent of result.agents) {
      const row = element('div', undefined, 'agent-row'), info = element('div', undefined, 'agent-info'), line = element('div', undefined, 'agent-evidence');
      info.append(element('div', agent.name, 'agent-name'), line);
      const open = button(t('panel_agent_configure'), () => loadConfig(agent.id, agent.name), 'agent-open'); open.title = `${t('panel_agent_configure')} · ${agent.name}`;
      row.append(info, open); (!agent.executable && missingList ? missingList : nodes.agentList).append(row); agentRows.set(agent.id, row);
      if (nodes.agentsView.hidden) line.textContent = agent.executable ? t('panel_agent_installed') : t('panel_agent_not_installed'); else void agentStatus(agent, line, epoch, row);
    }
    if (missingBox) { const count = missingList?.children?.length || 0; missingBox.hidden = !count; const label = document.getElementById('agentMissingCount'); if (label) label.textContent = String(count); }
    // The open configuration sits under its agent's row; rebuilding the list detached it.
    if (configuration && !nodes.configuration.hidden) placeConfiguration(configuration.agentId);
  }
  // Under its row in the list; a chip's configuration goes below the chips.
  function placeConfiguration(agentId) {
    const row = agentRows.get(agentId), chips = document.getElementById('agentMissing');
    (row && chips && row.parentNode === document.getElementById('agentMissingList') ? chips : row)?.after?.(nodes.configuration);
  }
  function closeConfig() {
    configEpoch++; preview = null; nodes.configuration.hidden = true;
    for (const row of agentRows.values()) row.classList.toggle('selected', false);
  }
  async function loadConfig(agentId, name) {
    const epoch = ++configEpoch; const result = await api('configRead', { agentId }); if (epoch !== configEpoch) return;
    configuration = result.configuration; configuration.name = name; preview = null; applied = null;
    nodes.configuration.hidden = false; nodes.configTitle.textContent = t('panel_config_title', name); nodes.configPath.textContent = configuration.path;
    nodes.configWarnings.textContent = (configuration.warnings || []).join('\n'); nodes.configFields.replaceChildren(); nodes.configPreview.hidden = true; nodes.restoreConfig.hidden = true; nodes.configResult.textContent = '';
    for (const button of nodes.configForm.querySelectorAll('button')) button.disabled = panelSupport?.mode !== 'full';
    for (const [id, row] of agentRows) row.classList.toggle('selected', id === agentId);
    placeConfiguration(agentId);
    const modelInputs = [];
    for (const field of configuration.fields) {
      const label = element('label', undefined, 'field'); label.append(element('span', fieldLabels[field.id] || field.id)); const input = element('input'); input.disabled = panelSupport?.mode !== 'full'; input.value = field.value || ''; input.placeholder = t('panel_cli_default'); input.dataset.field = field.id;
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
    void loadReplies().catch(() => {}); replyLoop(); void loadGrants().catch(() => {});
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

    if (!schedules.length) { nodes.scheduleEntries.replaceChildren(element('p', t('panel_schedules_empty'), 'list-empty')); return; }
    nodes.scheduleEntries.replaceChildren(...schedules.map(task => {
      const row=element('div',undefined,`list-entry schedule-entry${task.enabled?'':' off'}`);
      const head=element('div',undefined,'entry-head');
      head.append(element('strong',task.name),element('span',task.enabled?t('panel_schedule_enabled'):t('panel_schedule_disabled'),`badge ${task.enabled?'on':''}`));
      const result=task.lastResult || t('panel_schedule_not_run'), tone=task.lastResult==='unconfirmed'?'warn':/^failed/.test(task.lastResult || '')?'bad':'';
      row.append(head,element('div',[ruleText(task.rule),agentNames[task.agentId] || task.agentId].filter(Boolean).join(' · '),'entry-meta'),
        element('div',[t('panel_schedule_last',result),task.enabled?t('panel_schedule_next',new Date(task.nextRun).toLocaleString([], {month:'short',day:'numeric',hour:'2-digit',minute:'2-digit'})):''].filter(Boolean).join(' · '),`entry-meta ${tone}`));
      const actions=element('div',undefined,'entry-actions');
      if(task.lastResult==='unconfirmed') actions.append(button(t('panel_schedule_acknowledge'),async()=>{await api('scheduleSave',{id:task.id,acknowledgeUnconfirmed:true});await loadSchedules();},'pill warn'));
      actions.append(button(t('panel_schedule_run_now'),async()=>{await api('scheduleRun',{id:task.id});await loadSchedules();await list();},'pill'),button(t('panel_edit'),async()=>editSchedule(task),'pill'),button(task.enabled?t('panel_disable'):t('panel_enable'),async()=>{await api('scheduleSave',{id:task.id,enabled:!task.enabled});await loadSchedules();},'pill'));
      if(task.lastChat) actions.append(button(t('panel_schedule_open_run'),async()=>{await list();select(task.lastChat);view(false);},'pill'));
      actions.append(button(t('panel_schedule_delete'),async()=>{await api('scheduleDelete',{id:task.id});await loadSchedules();},'pill danger-text'));
      row.append(actions);
      return row;
    }));
  }
  function ruleText(rule) {
    if (rule?.kind==='interval') return t('panel_schedule_every',rule.minutes);
    if (rule?.kind==='daily') return t('panel_schedule_daily_at',rule.time);
    if (rule?.kind==='weekdays') return t('panel_schedule_weekdays_at',rule.time);
    return rule?.expression ? `cron ${rule.expression}` : '';
  }
  function editSchedule(task) {
    nodes.scheduleEditor.hidden=false;nodes.scheduleEditorTitle.textContent=task?t('panel_schedule_editing',task.name):t('sidepanel_new_schedule');
    nodes.scheduleId.value=task?.id || '';nodes.scheduleName.value=task?.name || '';nodes.schedulePrompt.value=task?.prompt || '';
    nodes.scheduleAgent.value=task?.agentId || skillAgent();nodes.scheduleModel.value=task?.model || '';nodes.scheduleEffort.value=task?.options?.effort || '';nodes.scheduleTier.value=task?.options?.serviceTier || '';
    nodes.scheduleAccess.value=task?.options?.toolAccess || (nodes.scheduleAgent.value==='opencode'?'runCommands':nodes.scheduleAgent.value==='codex'?'readFiles':'browserOnly');
    nodes.scheduleAutomatic.checked=nodes.scheduleAgent.value==='pi' || task?.fullAccess===true;nodes.scheduleAutomatic.disabled=nodes.scheduleAgent.value==='pi';nodes.scheduleEnabled.checked=task?.enabled!==false;
    nodes.scheduleKind.value=task?.rule?.kind || 'interval';nodes.scheduleTiming.value=task?.rule?.minutes || task?.rule?.time || task?.rule?.expression || '60';nodes.scheduleSkill.value=task?.skillId || '';
    nodes.scheduleEditor.scrollIntoView?.({block:'nearest',behavior:'smooth'});
  }
  function closeScheduleEditor() { editSchedule(null); nodes.scheduleEditor.hidden=true; }
  nodes.reloadSchedules.addEventListener('click',()=>void loadSchedules().catch(error));
  nodes.newSchedule.addEventListener('click',()=>{editSchedule(null);nodes.scheduleName.focus?.();});
  nodes.cancelSchedule.addEventListener('click',closeScheduleEditor);
  nodes.scheduleAgent.addEventListener('change',()=>{nodes.scheduleEffort.value='';nodes.scheduleTier.value='';nodes.scheduleModel.value='';nodes.scheduleAccess.value=nodes.scheduleAgent.value==='opencode'?'runCommands':nodes.scheduleAgent.value==='codex'?'readFiles':'browserOnly';nodes.scheduleAutomatic.disabled=nodes.scheduleAgent.value==='pi';nodes.scheduleAutomatic.checked=nodes.scheduleAgent.value==='pi';});
  nodes.saveSchedule.addEventListener('click',()=>void (async()=>{
    nodes.saveSchedule.disabled=true;
    try {
      const kind=nodes.scheduleKind.value, value=nodes.scheduleTiming.value.trim();
      const rule={kind,...(kind==='interval'?{minutes:Number(value)}:kind==='cron'?{expression:value}:{time:value})};
      await api('scheduleSave',{...(nodes.scheduleId.value?{id:nodes.scheduleId.value}:{}),name:nodes.scheduleName.value,prompt:nodes.schedulePrompt.value,agentId:nodes.scheduleAgent.value,model:nodes.scheduleModel.value.trim() || null,rule,enabled:nodes.scheduleEnabled.checked,fullAccess:nodes.scheduleAutomatic.checked,skillId:nodes.scheduleSkill.value || null,options:{toolAccess:nodes.scheduleAccess.value,effort:nodes.scheduleEffort.value.trim() || null,serviceTier:nodes.scheduleTier.value || null}});
      closeScheduleEditor();await loadSchedules();
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
    if (!skillCatalog.length) { nodes.skillEntries.replaceChildren(element('p', t('panel_skills_empty'), 'list-empty')); return; }
    nodes.skillEntries.replaceChildren(...skillCatalog.map(skill => {
      const row = element('div', undefined, `list-entry skill-entry${skill.enabled ? '' : ' off'}`), head = element('div', undefined, 'entry-head');
      head.append(element('strong', skill.name), element('span', skill.source, 'badge'));
      if (!skill.enabled) head.append(element('span', t('panel_skill_disabled'), 'badge'));
      row.append(head);
      if (skill.description) { const text = element('div', skill.description, 'entry-description'); text.title = skill.description; row.append(text); }
      const actions = element('div', undefined, 'entry-actions');
      actions.append(button(t('panel_skill_use'), async () => { skillChoice = skill; nodes.message.value = `/${skill.name} `; view(false); renderSkillPicker(); }, 'pill'));
      if (skill.editable) actions.append(button(t('panel_edit'), async () => {
        const result = await api('skillRead', { agentId: skillAgent(), id: skill.id });
        nodes.skillName.value = skill.name; nodes.skillContent.value = result.skill.instructions;
        nodes.skillEditor.open = true; nodes.skillEditor.scrollIntoView?.({ block: 'nearest', behavior: 'smooth' });
      }, 'pill'), button(skill.enabled ? t('panel_disable') : t('panel_enable'), async () => {
        await api('skillSetEnabled', { agentId: skillAgent(), id: skill.id, enabled: !skill.enabled }); await loadSkills();
      }, 'pill'));
      row.append(actions);
      return row;
    }));
  }
  function chosenSkill(text) {
    if (!panelSupport?.skillLibrary) return null;
    const tokens = slashTokens(text);
    if (skillChoice?.enabled && tokens.some(token => token.name === skillChoice.name)) return skillChoice;
    const named = name => skillCatalog.filter(skill => skill.enabled && skill.name === name);
    const first = tokens[0]?.start === 0 ? tokens[0] : null;
    if (first) {
      const matches = named(first.name);
      if (matches.length !== 1) throw new Error(matches.length ? t('panel_skill_ambiguous') : t('panel_skill_unknown'));
      return matches[0];
    }
    for (const token of tokens) { const matches = named(token.name); if (matches.length === 1) return matches[0]; }
    return null;
  }
  function caretQuery() { return slashQuery(nodes.message.value, Number.isInteger(nodes.message.selectionStart) ? nodes.message.selectionStart : undefined); }
  // Put `/name ` in place of the `/word` at the caret, keeping the text around it.
  function insertSkill(skill) {
    const value = nodes.message.value, query = caretQuery() || { start: 0, end: value.length };
    const after = value.slice(query.end).replace(/^[^\s]*/, '');
    const inserted = `/${skill.name} `;
    nodes.message.value = value.slice(0, query.start) + inserted + after.replace(/^\s/, '');
    const caret = query.start + inserted.length;
    try { nodes.message.setSelectionRange?.(caret, caret); } catch {}
    skillChoice = skill; nodes.skillPicker.hidden = true; nodes.message.focus?.();
  }
  function renderSkillPicker() {
    const match = caretQuery();
    // Built-in commands come first and open their own view when chosen; they
    // are commands only at the start of the message.
    const builtins = match && match.start === 0 && nodes.message.value.slice(match.end).trim() === '' && panelSupport?.sessionResume && RESUME_TEXT.command.startsWith(match.query.toLowerCase()) ? [{ name: RESUME_TEXT.command, source: 'chrome-use', description: RESUME_TEXT.commandHint, builtin: true }] : [];
    const skills = match ? [...builtins, ...skillCatalog.filter(skill => skill.enabled && skill.name.toLowerCase().includes(match.query.toLowerCase()))].slice(0, 12) : [];
    visibleSkills = skills; skillPickerIndex = Math.min(skillPickerIndex,Math.max(0,skills.length-1));
    nodes.skillPicker.hidden = !skills.length;
    nodes.skillPicker.replaceChildren(...skills.map(skill => button(`${skill.name} · ${skill.source} · ${skill.description}`, async () => {
      if (skill.builtin) { await openResume(); return; }
      insertSkill(skill);
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
      event.preventDefault();
      if (skill.builtin) { void openResume().catch(error); return true; }
      insertSkill(skill);return true;
    }
    return false;
  }
  nodes.message.addEventListener('input', () => {skillPickerIndex=0;renderSkillPicker();});
  // Moving the caret onto or off a `/word` shows or hides the choices.
  nodes.message.addEventListener('click', () => renderSkillPicker());
  nodes.message.addEventListener('keyup', event => { if (['ArrowLeft','ArrowRight','Home','End'].includes(event.key)) renderSkillPicker(); });
  nodes.reloadSkills.addEventListener('click', () => void loadSkills().catch(error));
  nodes.saveSkill.addEventListener('click', () => void (async () => {
    await api('skillSave', { agentId: skillAgent(), name: nodes.skillName.value.trim(), content: nodes.skillContent.value }); await loadSkills();
  })().catch(error));
  nodes.importSkill.addEventListener('click', () => void (async () => {
    nodes.importSkill.disabled = true;
    try { await api('skillImport', { agentId: skillAgent(), kind: nodes.skillImportKind.value, source: nodes.skillImportSource.value.trim() }); await loadSkills(); }
    finally { nodes.importSkill.disabled = false; }
  })().catch(error));
  // Chips, and a question typed in the picker: the question is put into the
  // composer once and taken out of storage, so a reload does not add it again.
  function applyChips(record) {
    chipRecord = { items: Array.isArray(record?.items) ? record.items : [], question: '' };
    const question = typeof record?.question === 'string' ? record.question.trim() : '';
    if (question) {
      const current = nodes.message.value.trimEnd();
      nodes.message.value = current ? `${current}\n${question}` : question;
      // Read again before writing: a chip added meanwhile must stay.
      void writeChips(stored => ({ ...stored, question: stored.question === record.question ? '' : stored.question })).catch(() => {});
    }
    renderChips();
  }
  async function writeChips(change) {
    if (!chipKey) return;
    const stored = (await Promise.resolve(storage?.session?.get?.(chipKey)).catch(() => null))?.[chipKey] || { items: [], question: '' };
    const next = change(stored);
    if (next.items.length || next.question) await storage?.session?.set?.({ [chipKey]: next }); else await storage?.session?.remove?.(chipKey);
    chipRecord = { items: next.items, question: '' }; renderChips();
  }
  // Only the chips that were sent go: one added while sending stays.
  function dropChips(ids) { return writeChips(record => ({ ...record, items: (record.items || []).filter(chip => !ids.includes(chip.id)) })).catch(() => {}); }
  // Element chips sit with the page chip, one family: icon, short name, the
  // source file when one was found, a dot while a screenshot or source is
  // still coming. Hover or focus shows the details; Delete or × removes.
  // Chips are kept by id and updated in place, so a chip's landing
  // highlight keeps playing while its screenshot and source fill in.
  const chipNodes = new Map();
  function renderChips() {
    const items = chipRecord.items;
    const fresh = shownChipIds ? items.filter(chip => !shownChipIds.has(chip.id)) : [];
    shownChipIds = new Set(items.map(chip => chip.id));
    nodes.inspectChips.hidden = !items.length;
    nodes.chipActions.hidden = !items.length;
    for (const [id, node] of chipNodes) if (!shownChipIds.has(id)) { node.remove(); chipNodes.delete(id); }
    items.forEach((chip, index) => {
      let item = chipNodes.get(chip.id);
      if (!item) { item = chipNode(chip.id); chipNodes.set(chip.id, item); if (fresh.includes(chip)) { item.dataset.landed = '1'; schedule(() => { delete item.dataset.landed; item.classList.remove?.('landed'); }, 1300); } }
      fillChip(item, chip);
      if (nodes.inspectChips.children[index] !== item) nodes.inspectChips.insertBefore(item, nodes.inspectChips.children[index] || null);
    });
    if (fresh.length) {
      announce(t('panel_inspect_added_live', fresh.map(chipTitle).join(', ')));
      // A pick from the Pick button is done: the next step is typing.
      if (!picking) nodes.message.focus?.();
    }
    if (cardChip && !items.some(chip => chip.id === cardChip)) hideCard();
    else if (cardChip) showCard(items.find(chip => chip.id === cardChip));
  }
  function chipNode(id) {
    const item = element('span');
    item.setAttribute('role', 'listitem'); item.tabIndex = 0;
    const current = () => chipRecord.items.find(chip => chip.id === id);
    const drop = () => {
      const name = chipTitle(current() || { label: '' }), at = chipRecord.items.findIndex(chip => chip.id === id);
      return writeChips(record => removeChip(record, id)).then(() => { announce(t('panel_inspect_removed_live', name)); hideCard(); (nodes.inspectChips.children[at] || nodes.inspectChips.children[at - 1] || nodes.message).focus?.(); });
    };
    item.drop = drop;
    item.addEventListener('mouseenter', () => showCard(current())); item.addEventListener('mouseleave', hideCard);
    item.addEventListener('focus', () => showCard(current())); item.addEventListener('blur', hideCard);
    item.addEventListener('keydown', event => {
      if (event.key === 'Delete' || event.key === 'Backspace') { event.preventDefault(); void drop().catch(error); }
      else if (event.key === 'Escape') hideCard();
    });
    return item;
  }
  // Element chips sit with the page chip, one family: icon, short name, the
  // source file when one was found, a dot while a screenshot or source is
  // still coming. Hover or focus shows the details; Delete or × removes.
  function fillChip(item, chip) {
    const name = chipTitle(chip);
    item.className = 'page-chip inspect-chip' + (chip.stale ? ' stale' : '') + (item.dataset.landed ? ' landed' : '');
    item.setAttribute('aria-label', t(chip.stale ? 'panel_inspect_chip_aria_stale' : 'panel_inspect_chip_aria', name));
    const icon = element('span', undefined, 'inspect-chip-icon'); icon.setAttribute('aria-hidden', 'true');
    const parts = [icon, element('span', name, 'inspect-chip-label')];
    const source = sourceShort(chip.source);
    if (source) parts.push(element('span', source, 'inspect-chip-src'));
    if (chip.pending?.image || chip.pending?.source) { const dot = element('span', undefined, 'inspect-chip-pending'); dot.title = t('panel_inspect_chip_pending'); parts.push(dot); }
    if (chip.stale) parts.push(element('span', t('panel_inspect_chip_stale'), 'inspect-chip-tag stale'));
    const remove = button('×', () => item.drop(), 'inspect-chip-remove');
    remove.tabIndex = -1; remove.title = t('panel_inspect_chip_remove'); remove.setAttribute('aria-label', t('panel_inspect_chip_remove_named', name));
    parts.push(remove);
    item.replaceChildren(...parts);
  }
  function announce(text) { nodes.chipLive.textContent = text; }
  // Details card above the composer: screenshot (or its placeholder), the
  // selector, where the element sits, its source and text.
  let cardChip = null;
  function showCard(chip) {
    if (!chip) return;
    cardChip = chip.id;
    const card = nodes.chipCard, rows = [];
    const head = element('div', chipTitle(chip), 'chip-card-title');
    const shot = chip.image ? element('img', undefined, 'chip-card-shot') : element('div', t(chip.pending?.image ? 'panel_inspect_card_shot_pending' : 'panel_inspect_card_no_shot'), 'chip-card-shot empty' + (chip.pending?.image ? ' loading' : ''));
    if (chip.image) { shot.src = chip.image; shot.alt = t('panel_inspect_card_shot_alt', chipTitle(chip)); }
    const row = (label, value, mono = false) => { if (!value) return; const line = element('div', undefined, 'chip-card-row'); line.append(element('span', label, 'chip-card-key'), element('span', value, mono ? 'chip-card-value mono' : 'chip-card-value')); rows.push(line); };
    row(t('panel_inspect_card_selector'), chip.selector, true);
    const where = withinLabel(chip);
    if (where) row(t('panel_inspect_card_inside'), `${t(where === 'frame' ? 'panel_inspect_chip_in_frame' : where === 'foreign' ? 'panel_inspect_chip_foreign_frame' : 'panel_inspect_chip_in_shadow')} · ${where === 'frame' ? chip.frames.map(f => f.selector).join(' › ') : where === 'foreign' ? chip.foreignFrame.url || chip.selector : chip.shadowHosts.join(' › ')}`);
    row(t('panel_inspect_card_source'), chip.source ? `${chip.source.path}${chip.source.line ? `:${chip.source.line}` : ''}` : chip.pending?.source ? t('panel_inspect_card_source_pending') : '', true);
    row(t('panel_inspect_card_text'), chip.text);
    row(t('panel_inspect_card_page'), chip.pageUrl, true);
    card.replaceChildren(head, shot, ...rows);
    if (chip.stale) card.append(element('div', t('panel_inspect_chip_stale_title'), 'chip-card-stale'));
    card.hidden = false;
  }
  function hideCard() { cardChip = null; nodes.chipCard.hidden = true; }
  // The Pick button: shown in a tab's panel, usable on web pages only.
  function renderPick() {
    const button = nodes.pickElement;
    button.hidden = !tabBound;
    const pickable = /^https?:/.test(panelTabUrl);
    button.disabled = !picking && pageRead && !pickable;
    button.setAttribute('aria-pressed', String(picking));
    button.classList.toggle('active', picking);
    const label = picking ? t('panel_pick_cancel_title') : button.disabled ? t('panel_pick_unavailable') : t('panel_pick_title');
    button.title = label; button.setAttribute('aria-label', label);
    nodes.pickHint.hidden = !picking;
  }
  async function togglePick(event) {
    if (!tabBound) return;
    if (picking) { picking = false; renderPick(); await runtime.sendMessage({ type: 'ab-inspect-pick-cancel', tabId: panelTab }); return; }
    const keep = !!event?.shiftKey;
    picking = true; renderPick(); hideCard();
    try {
      let reply = await runtime.sendMessage({ type: 'ab-inspect-pick', tabId: panelTab, keep });
      // A site the extension may not script yet: Chrome asks the user once.
      if (reply?.success && reply.needsPermission) {
        const granted = await Promise.resolve(permissions?.request?.({ origins: [reply.needsPermission] })).catch(() => false);
        if (!granted) throw new Error(t('panel_pick_permission_denied'));
        reply = await runtime.sendMessage({ type: 'ab-inspect-pick', tabId: panelTab, keep });
      }
      if (!reply?.success || !reply.started) throw new Error(reply?.error || t('panel_pick_failed'));
    } catch (failure) { picking = false; renderPick(); throw failure; }
  }
  function cancelPick() { if (picking) void togglePick().catch(error); }
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
  nodes.pickElement.addEventListener('click', event => void togglePick(event).catch(error));
  // Copy is for handing the context to another agent or tool (#680): the
  // picked elements alone, or the whole message as the user would send it.
  async function copyContext(target, withWords, labelKey) {
    const images = withWords ? (imageDrafts.get(selected) || []).length : 0;
    const text = clipboardText(withWords ? nodes.message.value : '', chipRecord.items, { tabId: panelTab, images });
    try { await navigator.clipboard.writeText(text); target.textContent = t('panel_copied'); }
    catch { target.textContent = t('panel_copy_failed'); }
    schedule(() => { target.textContent = t(labelKey); }, 1500);
  }
  nodes.copyElements.addEventListener('click', () => void copyContext(nodes.copyElements, false, 'sidepanel_copy_elements').catch(error));
  nodes.copyInput.addEventListener('click', () => void copyContext(nodes.copyInput, true, 'sidepanel_copy_input').catch(error));
  nodes.pickCancel.addEventListener('click', cancelPick);
  // Esc cancels picking from the panel too (focus stays here while the
  // pointer works on the page).
  document.addEventListener?.('keydown', event => { if (event.key === 'Escape' && picking) { event.preventDefault?.(); cancelPick(); nodes.pickElement.focus?.(); } });
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
  pagePicker?.addEventListener('change', () => { pickedTabId = Number(pagePicker.value); pageDetached = false; void refreshPage().catch(() => {}); });
  grantTab?.addEventListener('click', () => void grantPage().catch(error));
  grantRevoke?.addEventListener('click', () => void takeBack().catch(error));
  openInTab?.addEventListener('click', () => void tabs.create({ url: runtime.getURL(`sidepanel.html?mode=tab${selected ? `&conversation=${encodeURIComponent(selected)}` : ''}`) }).catch(error));
  if (tabs?.onRemoved) tabs.onRemoved.addListener(() => { if (tabMode) void refreshPage().catch(() => {}); });
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
  const deleteHint = nodes.deleteConfirm.querySelector?.('.hint');
  nodes.deleteChat.addEventListener('click', () => {
    // A continued CLI session is never deleted with the conversation.
    if (deleteHint) deleteHint.textContent = conversation?.resumed ? RESUME_TEXT.deleteKeeps : t('sidepanel_delete_confirm_detail');
    nodes.deleteConfirm.hidden = false;
  });
  nodes.cancelDelete.addEventListener('click', () => { nodes.deleteConfirm.hidden = true; });
  nodes.confirmDelete.addEventListener('click', () => void deleteConversation().catch(error));
  nodes.inspectAgent.addEventListener('click', () => void inspectModels(true).catch(error));
  nodes.agent.addEventListener('change', () => { rememberAgent(nodes.agent.value); projectChoice.setAgent(nodes.agent.value); clearModels(); loadModels(); skillChoice = null; void loadSkills().catch(error); controls(); });
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
  nodes.closeConfig.addEventListener('click', closeConfig);
  if (runtime?.onMessage) runtime.onMessage.addListener(message => {
    if (message?.type === 'ab-tab-grants' && Array.isArray(message.grants)) { grants = message.grants; renderGrant(); }
    if (message?.type === 'ab-host-state') {
      const wasUsable = connected && Boolean(panelSupport); connectionEpoch++; renderConnection(message);
      if (connected && panelSupport && !wasUsable) void refresh().catch(error);
      else if (connected) void poll().catch(error);
    }
  });
  // Another tab took this tab's conversation, or a notification bound one here.
  storage?.onChanged?.addListener?.((changes, area) => {
    // Another view changed where a conversation works (a take-back in a panel tab).
    if (changes.ab_panel_work_in && area !== 'session') { workInMap = changes.ab_panel_work_in.newValue || {}; renderGrant(); }
    if (!tabBound || area !== 'session') return;
    if (chipKey && changes[chipKey]) applyChips(changes[chipKey].newValue);
    if (tabBound && changes[inspectPickKey(panelTab)]) { picking = !!changes[inspectPickKey(panelTab)].newValue; renderPick(); }
    const change = changes[panelTabKey(panelTab)];
    if (!change || (change.newValue || '') === selected) return;
    // Applied once the send ends, so two panels never keep one conversation.
    if (sending) { pendingBinding = change.newValue || ''; return; }
    select(change.newValue || '');
  });
  controls(); closeConversationMenu();
  return { async start() {
    try {
      const saved = await storage?.local?.get(['ab_panel_conversation', 'ab_panel_pending', 'ab_panel_full_access', 'ab_panel_conversation_scope', 'ab_panel_agent', 'ab_panel_show_replies']);
      // Opened from a reply notification.
      if (Date.now() - Number(saved?.ab_panel_show_replies || 0) < 30000) replyBox.open = true;
      if (chatAgents.has(saved?.ab_panel_agent)) preferredAgent = saved.ab_panel_agent;
      if (saved?.ab_panel_conversation_scope === 'all') scope = 'all';
      workInDefault = (await Promise.resolve(storage?.sync?.get?.({ ab_work_in: 'tab' })).catch(() => null))?.ab_work_in === 'window' ? 'window' : 'tab';
      renderEmptyDetail();
      workInMap = (await Promise.resolve(storage?.local?.get?.('ab_panel_work_in')).catch(() => null))?.ab_panel_work_in || {};
      if (tabBound) {
        const key = panelTabKey(panelTab); selected = (await Promise.resolve(storage?.session?.get?.(key)).catch(() => null))?.[key] || '';
        const waiting = (await Promise.resolve(storage?.session?.get?.(chipKey)).catch(() => null))?.[chipKey];
        if (waiting) applyChips(waiting);
        // Chips already waiting when the panel opens are not "new".
        shownChipIds = new Set(chipRecord.items.map(chip => chip.id));
        picking = !!(await Promise.resolve(storage?.session?.get?.(inspectPickKey(panelTab))).catch(() => null))?.[inspectPickKey(panelTab)];
        renderPick();
      }
      else selected = tabMode ? initialConversation : saved?.ab_panel_conversation || '';
      if (saved?.ab_panel_full_access === false) newFullAccess = false;
      for (const [id, messageId] of Object.entries(saved?.ab_panel_pending || {}).slice(0, 64)) {
        if (typeof messageId === 'string' && messageId.length <= 80) pendingMessages.set(id, { id: messageId });
      }
      document.addEventListener?.('visibilitychange', reportPresence);
      if (tabMode) ownTabId = (await Promise.resolve(tabs?.getCurrent?.()).catch(() => null))?.id ?? null;
      void Promise.resolve(windows?.getCurrent?.()).then(current => { if (Number.isInteger(current?.id)) { windowId = current.id; connectPresence(); if (tabMode) void refreshPage().catch(() => {}); } }).catch(() => {});
      await refresh();
      projectChoice.setAgent(nodes.agent.value); refreshProjects();
    } catch (failure) { error(failure); }
  }, refresh, select, send, togglePick, timeline: () => timeline, close() { closed = true; try { presencePort?.disconnect(); } catch {} cancel(replyTimer); clearTurnView(); modelEpoch++; discoverEpoch++; skillsEpoch++; schedulesEpoch++; cancel(timer); selectionEpoch++; configEpoch++; connectionEpoch++; } };
}

// The English fallback loads first, so the first render has no raw keys.
if (typeof document !== 'undefined') void i18nReady.then(() => {
  applyI18n(document);
  const query = new URLSearchParams(globalThis.location?.search || '');
  const mode = query.get('mode') === 'tab' ? 'tab' : 'panel';
  document.documentElement.dataset.mode = mode;
  const setLocation = id => { try { globalThis.history.replaceState(null, '', `?mode=tab${id ? `&conversation=${encodeURIComponent(id)}` : ''}`); } catch {} };
  const panel = createPanel({ document, runtime: globalThis.chrome?.runtime, storage: globalThis.chrome?.storage, tabs: globalThis.chrome?.tabs, windows: globalThis.chrome?.windows, permissions: globalThis.chrome?.permissions, scripting: globalThis.chrome?.scripting, mode, initialConversation: query.get('conversation') || '', panelTab: panelTabFrom(globalThis.location?.search), setLocation });
  void panel.start();
  globalThis.addEventListener('pagehide', () => panel.close());
});
