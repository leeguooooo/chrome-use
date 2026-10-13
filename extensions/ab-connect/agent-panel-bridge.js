// Correlate extension-page requests with the native host, separately from CDP.
// The bridge neither attaches tabs nor executes page JavaScript. Native-host
// replies must match the request's connection generation; no request is replayed.
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
    if (!['discover', 'agentInspect', 'configRead', 'configPlan', 'configApply', 'configRestore', 'serviceHealth', 'serviceStop', 'conversationList', 'conversationCreate', 'conversationRead', 'conversationSend', 'conversationSendDirect', 'conversationMessageStatus', 'conversationQueueEdit', 'conversationQueueMessageRead', 'conversationQueueRemove', 'conversationStop', 'conversationResumeQueue', 'conversationRespond', 'conversationDelete', 'conversationSetAccess'].includes(operation)) return Promise.reject(new Error(`Unsupported agent panel operation: ${operation}`));
    if (this.pending.size >= this.maxPending) return Promise.reject(new Error('Too many pending agent panel requests'));
    const requestId = `${this.identity}:${this.generation}:${++this.sequence}`;
    return new Promise((resolve, reject) => {
      const timeout = this.schedule(() => {
        this.pending.delete(requestId);
        reject(new Error(operation.startsWith('conversation') && !['conversationRead', 'conversationList'].includes(operation)
          ? 'The conversation operation is unconfirmed. Read its current state before retrying; no message was replayed.'
          : ['configApply', 'configRestore'].includes(operation)
          ? 'The configuration outcome could not be confirmed. Read its current values before retrying; the request was not replayed.'
          : 'The native host did not answer the agent panel request. Check the host version and connection; the request was not replayed.'));
      }, this.timeoutMs);
      this.pending.set(requestId, { resolve, reject, timeout, operation, agentId: params?.agentId, planId: params?.planId, conversationId: params?.conversationId, messageId: params?.messageId, replacementId: params?.replacementId });
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
      reject(new Error(operation.startsWith('conversation') && !['conversationRead', 'conversationList'].includes(operation)
        ? `${reason || 'Native host disconnected'}. Conversation outcome is unconfirmed; reconnect and read events without resending the message.`
        : ['configApply', 'configRestore'].includes(operation)
        ? `${reason || 'Native host disconnected'}. Configuration outcome is unconfirmed; read current values before retrying.`
        : reason || 'Native host disconnected; the request was not replayed'));
    }
    this.pending.clear();
  }
}

function validResult({ operation, agentId, planId, conversationId, messageId, replacementId }, result) {
  if (result?.schemaVersion !== 1) return false;
  if (operation === 'agentInspect') {
    const inspection = result.inspection;
    return inspection?.agentId === agentId && inspection.source === 'cliProtocol' && inspection.modelAccessVerified === false && inspection.processExitConfirmed === true && typeof inspection.complete === 'boolean' && Array.isArray(inspection.models) && inspection.models.length <= 256 && inspection.models.every(model => typeof model.id === 'string' && typeof model.label === 'string' && Array.isArray(model.efforts)) && ['reportedSignedIn','reportedSignedOut','reportedMetadata','unverified'].includes(inspection.account?.status);
  }
  if (operation === 'discover') return Array.isArray(result.agents);
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
    if (!['idle', 'starting', 'running', 'waitingInput', 'stopping', 'failed', 'interrupted', 'recoveryRequired'].includes(conversation.phase)) return false;
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
  if (sender?.id !== runtime.id || sender.tab) return false;
  return [runtime.getURL('sidepanel.html'), runtime.getURL('options.html')].includes(sender.url);
}
