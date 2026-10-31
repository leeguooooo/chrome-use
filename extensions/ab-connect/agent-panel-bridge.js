// Correlate extension-page requests with the native host, separately from CDP.
// The bridge neither attaches tabs nor executes page JavaScript. Native-host
// replies must match the request's connection generation; no request is replayed.
// Slow operations outlast the native host's own budget for them (32 s and 47 s),
// so the host's answer arrives before this page gives up.
const operationTimeouts = { agentInspect: 35000, tabOperator: 46000, imageConvert: 30000, conversationDelete: 50000, skillImport: 45000, inspectDeliver: 35000, inspectStatus: 20000, inspectResolve: 20000, conversationArtifactRead: 12000, conversationRecoveryReview: 17000, conversationRecoveryTerminate: 17000, conversationRecoveryFork: 12000, conversationSessionList: 35000, conversationResume: 50000, conversationProjectList: 25000, conversationProjectChoose: 305000, conversationTabGrant: 30000 };
// Reads that change nothing; a timeout there never leaves an outcome to confirm.
const readOperations = ['conversationRead', 'conversationList', 'conversationArtifactRead', 'conversationRecoveryReview', 'conversationSessionList', 'conversationProjectList'];

// Inspect & Ask host operations. inspectDeliver is never replayed: a timeout
// leaves the batch unconfirmed and the caller asks inspectDeliveryStatus.
export const INSPECT_OPERATIONS = ['inspectStatus', 'inspectResolve', 'inspectClaimDecide', 'inspectBind', 'inspectUnbind', 'inspectBindings', 'inspectConsent', 'inspectDeliver', 'inspectDeliveryStatus', 'inspectLaunchCommand', 'inspectReplies', 'inspectProject', 'inspectProjectBind'];
// Of those, what extension pages (the side panel, the options page) may ask:
// managing bindings, consents and replies, and a site's project for a new
// conversation. Delivery, binding to a session and status belong to the
// authenticated input frame only.
export const PAGE_INSPECT_OPERATIONS = ['inspectBindings', 'inspectUnbind', 'inspectConsent', 'inspectClaimDecide', 'inspectReplies', 'inspectProject', 'inspectProjectBind'];
// The side panel's Record button (teach by demonstration): compiling a
// recording into a script and saving the reviewed script.
export const TEACH_OPERATIONS = ['teachCompile', 'teachSave'];
/** Whether an extension page's request may reach the host. */
export function pageOperationAllowed(operation) {
  return !INSPECT_OPERATIONS.includes(operation) || PAGE_INSPECT_OPERATIONS.includes(operation);
}

export class AgentPanelBridge {
  constructor({ send, schedule = (callback, delay) => globalThis.setTimeout(callback, delay), cancel = timer => globalThis.clearTimeout(timer), timeoutMs = 15000, maxPending = 16 }) {
    this.send = send;
    this.schedule = schedule;
    this.cancel = cancel;
    this.timeoutMs = timeoutMs;
    this.maxPending = maxPending;
    this.generation = 0;
    this.sequence = 0;
    this.identity = globalThis.crypto.randomUUID();
    this.pending = new Map();
  }

  request(operation, params = {}) {
    if (!['discover', 'tabOperator', 'agentInspect', 'configRead', 'configPlan', 'configApply', 'configRestore', 'serviceHealth', 'serviceStop', 'conversationList', 'conversationCreate', 'conversationSessionList', 'conversationResume', 'conversationProjectList', 'conversationProjectChoose', 'conversationRead', 'conversationSend', 'conversationSendDirect', 'conversationMessageStatus', 'conversationQueueEdit', 'conversationQueueMessageRead', 'conversationQueueRemove', 'conversationStop', 'conversationResumeQueue', 'conversationRespond', 'conversationDelete', 'conversationSetAccess', 'conversationTabGrant', 'conversationSetOptions', 'conversationAttachmentPut', 'conversationAttachmentRead', 'imageConvert', 'conversationArtifactRead', 'conversationRecoveryReview', 'conversationRecoveryTerminate', 'conversationRecoveryFork', 'skillList', 'skillRead', 'skillSave', 'skillSetEnabled', 'skillImport', 'conversationSkillSelect', 'scheduleList', 'scheduleSave', 'scheduleDelete', 'scheduleRun', ...TEACH_OPERATIONS, ...INSPECT_OPERATIONS].includes(operation)) return Promise.reject(new Error(`Unsupported agent panel operation: ${operation}`));
    if (this.pending.size >= this.maxPending) return Promise.reject(new Error('Too many pending agent panel requests'));
    const requestId = `${this.identity}:${this.generation}:${++this.sequence}`;
    return new Promise((resolve, reject) => {
      const timeout = this.schedule(() => {
        this.pending.delete(requestId);
        reject(new Error(operation.startsWith('conversation') && !readOperations.includes(operation)
          ? 'The conversation operation is unconfirmed. Read its current state before retrying; no message was replayed.'
          : operation === 'inspectDeliver'
          ? 'The delivery outcome is unconfirmed; it was not resent. Check its status.'
          : ['configApply', 'configRestore'].includes(operation)
          ? 'The configuration outcome could not be confirmed. Read its current values before retrying; the request was not replayed.'
          : 'The native host did not answer the agent panel request. Check the host version and connection; the request was not replayed.'));
      }, operationTimeouts[operation] ?? this.timeoutMs);
      this.pending.set(requestId, { resolve, reject, timeout, operation, agentId: params?.agentId, planId: params?.planId, conversationId: params?.conversationId, messageId: params?.messageId, replacementId: params?.replacementId, id: params?.id, skillId: params?.skillId, attachmentId: params?.attachment?.id, artifactId: params?.artifactId, offset: params?.offset, requestedOptions: params?.options, sessionId: params?.sessionId, project: params?.project });
      try { this.send({ method: 'agentPanel', requestId, operation, params }); }
      catch (error) {
        this.cancel(timeout);
        this.pending.delete(requestId);
        reject(error);
      }
    });
  }

  receive(message) {
    if (message?.method !== 'agentPanelReply') return false;
    const pending = this.pending.get(message.requestId);
    if (!pending) return true; // Late/duplicate panel reply must never enter CDP.
    this.pending.delete(message.requestId);
    this.cancel(pending.timeout);
    if (typeof message.error === 'string') pending.reject(new Error(message.error));
    else if (validResult(pending, message.result)) pending.resolve(message.result);
    else pending.reject(new Error('Malformed agent panel reply from the native host'));
    return true;
  }

  disconnect(reason) {
    this.generation++;
    for (const { reject, timeout, operation } of this.pending.values()) {
      this.cancel(timeout);
      reject(new Error(operation.startsWith('conversation') && !readOperations.includes(operation)
        ? `${reason || 'Native host disconnected'}. Conversation outcome is unconfirmed; reconnect and read events without resending the message.`
        : ['configApply', 'configRestore'].includes(operation)
        ? `${reason || 'Native host disconnected'}. Configuration outcome is unconfirmed; read current values before retrying.`
        : reason || 'Native host disconnected; the request was not replayed'));
    }
    this.pending.clear();
  }
}

// What a host with projectDirectory 2 says about the page (absent before).
function validProjectPage(page) {
  if (page === undefined || page === null) return true;
  const { suggestion, project } = page;
  return typeof page === 'object' && typeof page.multiProject === 'boolean' &&
    (project === null || (typeof project?.kind === 'string' && typeof project.key === 'string' && (project.pathScope === null || typeof project.pathScope === 'string'))) &&
    (suggestion === null || (typeof suggestion?.root === 'string' && ['repository', 'project', 'site'].includes(suggestion.reason))) &&
    Array.isArray(page.bindings) && page.bindings.every(b => typeof b?.pathScope === 'string' && typeof b.fingerprint === 'string');
}
function validResult({ operation, agentId, planId, conversationId, messageId, replacementId, id, skillId, attachmentId, artifactId, offset, requestedOptions, sessionId, project }, result) {
  if (result?.schemaVersion !== 1) return false;
  // Recovery and artifact replies name another conversation (a fork) or no
  // conversation at all, so they are checked before the generic branch.
  if (operation === 'conversationArtifactRead') return result.artifactId === artifactId && Number.isInteger(result.offset) && result.offset === offset && typeof result.data === 'string' && Number.isInteger(result.total) && result.total >= 0 && typeof result.digest === 'string' && /^sha256:[0-9a-f]{64}$/.test(result.digest);
  if (operation === 'conversationRecoveryReview') return ['exited', 'alive', 'unknown'].includes(result.process?.state);
  if (operation === 'conversationRecoveryTerminate') return typeof result.confirmed === 'boolean';
  if (operation === 'conversationProjectList') return Array.isArray(result.projects) && result.projects.length <= 30 && result.projects.every(project => typeof project.path === 'string' && Array.isArray(project.sources)) && validProjectPage(result.page);
  // The chosen folder, a cancelled dialog, or no picker on this computer.
  if (operation === 'conversationProjectChoose') return typeof result.path === 'string' ? result.cancelled === undefined : result.cancelled === true || result.unavailable === true;
  if (operation === 'tabOperator') return typeof result.operated === 'boolean';
  if (operation === 'conversationSessionList') return result.agentId === agentId && typeof result.supported === 'boolean' && Array.isArray(result.sessions) && result.sessions.length <= 100 && result.sessions.every(session => typeof session.sessionId === 'string' && session.agentId === agentId && typeof session.running === 'boolean' && Number.isFinite(session.lastActive) && (session.title === null || typeof session.title === 'string') && (session.cwd === null || typeof session.cwd === 'string') && (session.conversationId == null || typeof session.conversationId === 'string'));
  // A resume answers with the new (or already continued) conversation bound
  // to the session, or asks to confirm a session active elsewhere.
  if (operation === 'conversationResume') {
    if (result.confirmRequired !== undefined) return result.confirmRequired === 'running' && result.session?.sessionId === sessionId && result.conversation === undefined;
    const conversation = result.conversation;
    return typeof conversation?.id === 'string' && conversation.agentId === agentId && conversation.resumed?.sessionId === sessionId && ['idle', 'starting', 'running', 'waitingInput', 'stopping', 'failed', 'interrupted', 'recoveryRequired'].includes(conversation.phase);
  }
  if (operation === 'conversationRecoveryFork') return typeof result.conversation?.id === 'string' && result.conversation.id !== conversationId && result.forkedFrom === conversationId;
  if (operation === 'agentInspect') {
    const inspection = result.inspection;
    return inspection?.agentId === agentId && inspection.source === 'cliProtocol' && inspection.modelAccessVerified === false && inspection.processExitConfirmed === true && typeof inspection.complete === 'boolean' && Array.isArray(inspection.models) && inspection.models.length <= 256 && inspection.models.every(model => typeof model.id === 'string' && typeof model.label === 'string' && Array.isArray(model.efforts)) && ['reportedSignedIn','reportedSignedOut','reportedMetadata','unverified'].includes(inspection.account?.status);
  }
  if (['skillList','skillSave','skillSetEnabled','skillImport'].includes(operation)) return Array.isArray(result.skills);
  if (operation === 'skillRead') return result.skill?.id === id && typeof result.skill?.instructions === 'string';
  if (operation === 'scheduleList') return Array.isArray(result.schedules) && result.timeZone === 'hostLocal';
  if (['scheduleSave','scheduleRun'].includes(operation)) return typeof result.schedule?.id === 'string' && (!id || result.schedule.id === id) && (operation !== 'scheduleRun' || result.run?.dispatched === true && result.run.completed === false);
  if (operation === 'scheduleDelete') return result.deleted?.id === id;
  if (operation === 'discover') return Array.isArray(result.agents);
  if (operation === 'teachCompile') return Array.isArray(result.steps) && Array.isArray(result.program) && Array.isArray(result.params);
  if (operation === 'teachSave') return typeof result.path === 'string' && typeof result.run === 'string';
  if (operation === 'imageConvert') return result.mediaType === 'image/jpeg' && typeof result.data === 'string' && result.data.length > 0;
  if (operation === 'inspectResolve') return ['bound', 'needsConfirm', 'unbound'].includes(result.state) && Array.isArray(result.pendingClaims) && typeof result.ocs?.installed === 'boolean';
  if (operation === 'inspectClaimDecide') return ['accepted', 'rejected'].includes(result.decided);
  if (operation === 'inspectStatus') return typeof result.ocs?.installed === 'boolean' && Array.isArray(result.sessions) && Array.isArray(result.candidates);
  if (operation === 'inspectProject') return result.binding === null || (typeof result.binding?.root === 'string' && typeof result.binding.fingerprint === 'string' && typeof result.binding.exists === 'boolean');
  if (operation === 'inspectBind' || operation === 'inspectConsent' || operation === 'inspectProjectBind') return typeof result.binding?.origin === 'string' && typeof result.binding.root === 'string';
  if (operation === 'inspectUnbind') return result.unbound === true && typeof result.existed === 'boolean';
  if (operation === 'inspectBindings') return Array.isArray(result.bindings);
  if (operation === 'inspectDeliver') return ['sent', 'stored', 'unknown', 'failed', 'noBinding', 'noSession', 'sessionChanged', 'noOcs', 'upgradeRequired', 'targetChanged'].includes(result.state);
  if (operation === 'inspectDeliveryStatus') return result.delivery === null || typeof result.delivery?.state === 'string';
  if (operation === 'inspectReplies') return Array.isArray(result.replies);
  if (operation === 'inspectLaunchCommand') return typeof result.command === 'string' && typeof result.shell === 'string';
  if (operation === 'serviceHealth') return typeof result.profile === 'string' && typeof result.binaryHash === 'string' && Number.isInteger(result.pid);
  if (operation === 'serviceStop') return result.service?.stopRequested === true;
  if (operation === 'conversationList') return Array.isArray(result.conversations);
  if (operation === 'conversationDelete') {
    const deleted = result.deleted;
    return deleted?.id === conversationId && Array.isArray(deleted.cliHistory?.removed) && Array.isArray(deleted.cliHistory.failed) && typeof deleted.browserSession?.closed === 'boolean';
  }
  if (operation.startsWith('conversation')) {
    const conversation = result.conversation;
    if (!conversation || typeof conversation.id !== 'string' || (operation === 'conversationCreate' ? conversation.agentId !== agentId : conversation.id !== conversationId)) return false;
    // A host that dropped the requested project folder would run elsewhere.
    if (operation === 'conversationCreate' && project && typeof conversation.project !== 'string') return false;
    if (!['idle', 'starting', 'running', 'waitingInput', 'stopping', 'failed', 'interrupted', 'recoveryRequired'].includes(conversation.phase)) return false;
    // The service reports whether the conversation's browser session took
    // the tab; it never claims that for a tab it was not given.
    if (operation === 'conversationTabGrant') return typeof result.tabGrant?.granted === 'boolean' && (result.tabGrant.granted ? Number.isInteger(result.tabGrant.tabId) && typeof result.tabGrant.adopted === 'boolean' : result.tabGrant.adopted === undefined);
    if (operation === 'conversationSetOptions') return result.conversation.options && Object.entries(requestedOptions || {}).every(([key,value]) => result.conversation.options[key] === value);
    if (operation === 'conversationSkillSelect') return result.skill?.catalogId === skillId && typeof result.skill?.id === 'string' && typeof result.skill.name === 'string';
    if (operation.startsWith('conversationAttachment')) return typeof result.attachment?.id === 'string' && ['image/png','image/jpeg','image/webp'].includes(result.attachment.mediaType) && Number.isInteger(result.attachment.bytes) && (operation === 'conversationAttachmentPut' || result.attachment.id === attachmentId && typeof result.data === 'string');
    if (operation === 'conversationRead') return Array.isArray(result.journal?.events) && Number.isInteger(result.journal.cursor) && Array.isArray(result.queuedMessages) && result.queuedMessages.length === conversation.queued && result.queuedMessages.length <= 64 && result.queuedMessages.every(message => typeof message.id === 'string' && typeof message.text === 'string' && typeof message.truncated === 'boolean');
    if (['conversationSend', 'conversationSendDirect'].includes(operation)) return result.message?.id === messageId && typeof result.message.accepted === 'boolean' && result.message.replayed === false;
    if (operation === 'conversationMessageStatus') return result.message?.id === messageId && typeof result.message.known === 'boolean' && (result.message.known ? ['active', 'queued', 'previouslyAccepted'].includes(result.message.status) : result.message.status === 'unconfirmed');
    if (operation === 'conversationQueueMessageRead') return result.queuedMessage?.id === messageId && typeof result.queuedMessage.text === 'string';
    if (operation === 'conversationQueueEdit') return result.queueChange?.messageId === messageId && result.queueChange.replacementId === replacementId && result.queueChange.kind === 'edited';
    if (operation === 'conversationQueueRemove') return result.queueChange?.messageId === messageId && result.queueChange.kind === 'removed';
    if (operation === 'conversationRespond') return result.response?.dispatched === true && result.response.confirmed === false;
    return true;
  }
  const configuration = result.configuration;
  if (!configuration || typeof configuration.agentId !== 'string' || configuration.agentId !== agentId) return false;
  if (operation === 'configRead') return Array.isArray(configuration.fields) && typeof configuration.fingerprint === 'string';
  if (typeof configuration.planId !== 'string' || !Array.isArray(configuration.files)) return false;
  if (['configApply', 'configRestore'].includes(operation) && configuration.planId !== planId) return false;
  const phases = operation === 'configPlan' ? ['planned'] : operation === 'configApply' ? ['applied'] : ['restored', 'restoreConflicts'];
  return phases.includes(configuration.phase) && (configuration.phase !== 'restoreConflicts' || Array.isArray(configuration.conflicts));
}

// A content script, another extension, or a website cannot invoke local panel
// services. Check sender origin and the actual extension page URL, not a
// caller-supplied operation or claimed identity.
export function isAgentPanelSender(sender, runtime) {
  if (sender?.id !== runtime.id) return false;
  // The page itself, whatever its query (`?mode=tab&conversation=…`).
  if (typeof sender.url !== 'string') return false;
  const page = sender.url.split(/[?#]/)[0];
  // The side panel has no tab. Opened as a full-page tab ("Open in a tab")
  // it does, like the options page (options_ui.open_in_tab). Neither page is
  // web-accessible, so a web page cannot frame them; require the top frame
  // of that tab.
  if (page === runtime.getURL('sidepanel.html') || page === runtime.getURL('options.html')) return !sender.tab || sender.frameId === 0;
  return false;
}
