// Project a native journal into visible rows. All content remains text data;
// duplicate cursors and completed-text chunks never duplicate a response.
export class PanelTimeline {
  constructor() {
    this.cursor = 0; this.rows = []; this.items = new Map(); this.requests = new Map();
    // Turn projection (one card per user message). Evidence is attached only by
    // the messageId the host journaled with it, never by time or adjacency.
    this.turns = new Map(); this.turnOrder = []; this.lastUser = null;
    this.actions = new Map(); this.stepClaims = new Map(); this.artifacts = new Map();
    this.unbound = []; this.control = null; this.recovery = { terminated: null, forkedTo: null, context: null };
    this.requestKeys = new Map();
  }

  // A turn exists from its user event; journals without turnStarted start it
  // at the first event carrying its messageId. Events with no messageId (the
  // oldest journals) join the preceding user message as legacy grouping.
  turn(messageId, at) {
    const id = messageId ?? this.lastUser ?? '';
    let turn = this.turns.get(id);
    if (!turn) {
      turn = { messageId: id, user: null, attachedPage: null, state: 'legacy', recorded: false, at, generation: null, direct: false, continuation: null,
        outcome: null, error: null, emptyAnswer: false, finalItemIds: [], legacyTerminal: null, steps: [], texts: [], notes: [], roles: new Map(),
        stepByCall: new Map(), actions: new Map(), artifacts: new Map(), assessment: { provisional: null, final: null }, stalls: [],
        batchInterrupted: null, compactions: [], summary: null, forkedTo: null, legacy: { grouping: false } };
      this.turns.set(id, turn); this.turnOrder.push(id);
    }
    if (messageId == null && id !== '') turn.legacy.grouping = true;
    return turn;
  }
  action(data) {
    let action = this.actions.get(data.actionId);
    if (!action) {
      action = { actionId: data.actionId, messageId: data.messageId ?? null, tool: data.tool ?? null, toolClass: data.toolClass ?? null, admitted: null, target: null, observed: null, linkedStep: this.stepClaims.get(data.actionId) ?? null, late: false, afterStop: false, unreported: false };
      this.actions.set(data.actionId, action);
      if (data.messageId != null) this.turn(data.messageId).actions.set(data.actionId, action);
    }
    if (data.tool) action.tool = data.tool;
    if (data.toolClass) action.toolClass = data.toolClass;
    return action;
  }
  artifact(artifactId, data = {}) {
    let artifact = this.artifacts.get(artifactId);
    if (!artifact) { artifact = { artifactId, messageId: null, actionId: null, kind: null, name: null, state: 'pending', delivery: null }; this.artifacts.set(artifactId, artifact); }
    if (data.actionId && !artifact.actionId) artifact.actionId = data.actionId;
    if (data.kind) artifact.kind = data.kind;
    if (data.name) artifact.name = data.name;
    if (data.messageId != null && artifact.messageId == null) { artifact.messageId = data.messageId; this.turn(data.messageId).artifacts.set(artifactId, artifact); }
    return artifact;
  }
  claim(row, actionIds) {
    for (const actionId of Array.isArray(actionIds) ? actionIds : []) {
      if (typeof actionId !== 'string') continue;
      const owner = this.stepClaims.get(actionId);
      if (owner && owner !== row.key) { if (!row.duplicateClaims.includes(actionId)) row.duplicateClaims.push(actionId); continue; }
      this.stepClaims.set(actionId, row.key);
      if (!row.actionIds.includes(actionId)) row.actionIds.push(actionId);
      const action = this.actions.get(actionId); if (action) action.linkedStep = row.key;
    }
  }
  turnEvent(event, data) {
    const kind = event.kind;
    if (kind === 'turnStarted') {
      const turn = this.turn(data.messageId, event.at);
      Object.assign(turn, { state: turn.state === 'ended' ? 'ended' : 'running', recorded: true, generation: data.generation ?? null, direct: data.direct === true, continuation: data.continuation ?? null });
    } else if (kind === 'turnEnded') {
      const turn = this.turn(data.messageId, event.at);
      Object.assign(turn, { state: 'ended', recorded: true, outcome: data.outcome ?? null, error: data.error ?? null, emptyAnswer: data.emptyAnswer === true, finalItemIds: Array.isArray(data.finalItemIds) ? data.finalItemIds : [] });
      for (const itemId of turn.finalItemIds) turn.roles.set(`text:${turn.messageId}:${itemId}`, 'final');
      for (const actionId of Array.isArray(data.openLeases) ? data.openLeases : []) this.action({ actionId, messageId: data.messageId }).unreported = true;
    } else if (kind === 'textRole') this.turn(data.messageId, event.at).roles.set(`text:${data.messageId || 'legacy'}:${data.itemId}`, data.role);
    else if (kind === 'actionAdmitted') {
      const action = this.action(data);
      action.admitted = { sequence: event.sequence, outputs: Array.isArray(data.outputs) ? data.outputs : [] };
      for (const output of action.admitted.outputs) if (output?.artifactId) this.artifact(output.artifactId, { ...output, messageId: data.messageId, actionId: data.actionId });
    } else if (kind === 'browserActionTarget') {
      const action = this.action(data);
      action.target = { requested: data.requested ?? null, observed: data.observed ?? null, outcome: data.outcome ?? null, errorCode: data.errorCode ?? null, durationMs: data.durationMs ?? null, sequence: event.sequence };
      action.late = data.afterTurnEnd === true; action.afterStop = data.afterStop === true;
    } else if (kind === 'actionObserved') {
      this.action(data).observed = { verdict: data.verdict, basis: data.basis ?? null, detail: data.detail ?? null, possiblyExecuted: data.possiblyExecuted === true, check: data.check ?? null, sequence: event.sequence };
    } else if (kind === 'unboundActionReport') this.unbound.push({ sequence: event.sequence, ...data });
    else if (kind === 'artifactCreated') Object.assign(this.artifact(data.artifactId, data), { state: data.state || 'pending' });
    else if (kind === 'artifactState') this.artifact(data.artifactId).state = data.state;
    else if (kind === 'artifactReady') Object.assign(this.artifact(data.artifactId), { state: 'ready', mime: data.mime ?? null, declaredExtension: data.declaredExtension ?? null, typeMatches: data.typeMatches ?? null, bytes: data.bytes ?? null, digest: data.digest ?? null });
    else if (kind === 'artifactUnavailable') Object.assign(this.artifact(data.artifactId), { state: 'unavailable', reason: data.reason ?? null, detail: data.detail ?? null, bytes: data.bytes ?? null, digest: data.digest ?? null, detectedBy: data.detectedBy ?? null });
    else if (kind === 'fileDelivered') this.artifact(data.artifactId, data).delivery = { state: data.state, requestedDestination: data.requestedDestination ?? null, detail: data.detail ?? null };
    else if (kind === 'taskAssessment') { const turn = this.turn(data.messageId, event.at); turn.assessment[data.phase === 'final' ? 'final' : 'provisional'] = data; }
    else if (kind === 'controlChanged') this.control = data.to ?? null;
    else if (kind === 'progressStalled') this.turn(data.messageId, event.at).stalls.push({ sequence: event.sequence, kind: data.kind, count: data.count ?? null, quietMs: data.quietMs ?? null, open: true });
    else if (kind === 'progressResumed') { const stall = this.turn(data.messageId, event.at).stalls.findLast(stall => stall.open); if (stall) stall.open = false; }
    else if (kind === 'batchInterrupted') this.turn(data.messageId, event.at).batchInterrupted = data;
    else if (kind === 'turnSummary') this.turn(data.messageId, event.at).summary = data;
    else if (kind === 'contextCompacted') this.turn(data.messageId, event.at).compactions.push({ sequence: event.sequence, backend: data.backend ?? null, trigger: data.trigger ?? null });
    else if (kind === 'recoveryTerminated') this.recovery.terminated = data;
    else if (kind === 'recoveryForked') { this.recovery.forkedTo = data.forkedTo; this.turn(data.messageId, event.at).forkedTo = data.forkedTo; }
    else if (kind === 'recoveryContext') this.recovery.context = data;
  }

  apply(events, cursor) {
    for (const event of events) {
      if (event.sequence <= this.cursor) continue;
      if (event.sequence !== this.cursor + 1) throw new Error('Conversation history has a gap. Refresh before relying on this view.');
      const data = event.data || {};
      if (event.kind === 'user') {
        const row = { key: `user:${data.messageId}`, type: 'user', messageId: data.messageId, text: data.text || '', page: data.page || null, attachments: data.attachments || [], at: event.at };
        this.rows.push(row);
        const turn = this.turn(data.messageId, event.at); turn.user = row; turn.attachedPage = row.page; turn.at = event.at;
        if (turn.state === 'legacy') turn.state = 'queued';
        this.lastUser = data.messageId;
      } else if (event.kind === 'queueEdited' || event.kind === 'queueRemoved') {
        const row = this.rows.find(row => row.type === 'user' && row.messageId === data.messageId);
        if (row) { row.queueStatus = event.kind === 'queueRemoved' ? 'Removed from queue' : 'Edited before sending';
          if (event.kind === 'queueEdited') { row.text = data.text; row.messageId = data.replacementId; }
        }
        // The replacement is the message that runs; replay gives one turn for it.
        const turn = this.turns.get(data.messageId);
        if (event.kind === 'queueEdited' && turn && data.replacementId && !this.turns.has(data.replacementId)) {
          this.turns.delete(data.messageId); turn.messageId = data.replacementId; this.turns.set(data.replacementId, turn);
          this.turnOrder[this.turnOrder.indexOf(data.messageId)] = data.replacementId;
          if (this.lastUser === data.messageId) this.lastUser = data.replacementId;
        }
      }
      else if (event.kind === 'textDelta' || event.kind === 'textFinal') {
        const key = `text:${data.messageId || 'legacy'}:${data.itemId}`;
        let row = this.items.get(key);
        if (!row) { row = { key, type: 'assistant', text: '', at: event.at, finalPart: -1 }; this.items.set(key, row); this.rows.push(row); this.turn(data.messageId, event.at).texts.push(key); }
        if (typeof data.role === 'string') this.turn(data.messageId, event.at).roles.set(key, data.role);
        if (event.kind === 'textFinal') {
          if (data.part === 0) { row.text = ''; row.finalPart = -1; }
          if (data.part !== row.finalPart + 1) throw new Error('Completed response chunks arrived out of order.');
          row.finalPart = data.part;
        }
        row.text += data.text || '';
      } else if (event.kind === 'tool' && !shownTool(data.item || {})) {
        // Reasoning and untyped items are not tool calls; keep them out of view.
      } else if (event.kind === 'tool') {
        const item = data.item || {};
        const key = `tool:${data.messageId || 'legacy'}:${item.id || item.tool_use_id || event.sequence}`;
        let row = this.items.get(key);
        if (!row) { row = { key, type: 'tool', text: '', at: event.at, browser: false, toolCallId: null, actionIds: [], duplicateClaims: [] }; this.items.set(key, row); this.rows.push(row); this.turn(data.messageId, event.at).steps.push(key); }
        row.browser = row.browser || browserStep(item);
        if (typeof data.toolCallId === 'string' && !row.toolCallId) { row.toolCallId = data.toolCallId; this.turn(data.messageId, event.at).stepByCall.set(data.toolCallId, key); }
        this.claim(row, data.actionIds);
        row.name = toolName(item);
        row.hint = toolHint(item);
        row.failed = item.is_error === true || item.status === 'failed' || Boolean(item.error);
        row.completed = data.completed === true;
        row.text = describeTool(item);
      } else if (event.kind === 'request') {
        // A request id can come back with a new revision (a re-sent request or a
        // new process reusing JSON-RPC ids); the older card is retired.
        const id = JSON.stringify(data.requestId), key = Number.isInteger(data.requestRevision) ? `${id}#${data.requestRevision}` : id;
        const previous = this.requestKeys.get(id); if (previous !== undefined && previous !== key) this.requests.delete(previous);
        this.requestKeys.set(id, key); this.requests.set(key, { ...data, sent: false });
      } else if (event.kind === 'replySent') { const request = this.requests.get(this.requestKey(data.requestId)); if (request) request.sent = true; }
      else if (event.kind === 'requestResolved') { const id = JSON.stringify(data.requestId); this.requests.delete(this.requestKey(data.requestId)); this.requestKeys.delete(id); }
      else if (event.kind === 'terminal') {
        this.requests.clear(); this.requestKeys.clear();
        this.turn(data.messageId, event.at).legacyTerminal = data.status || null;
        if (data.status !== 'completed') this.note({ key: `note:${event.sequence}`, type: 'note', failed: data.status === 'failed', ...readableError(data.error || (data.status === 'interrupted' ? 'Response interrupted. Queued messages are paused.' : 'Response failed. Queued messages are paused.')), at: event.at }, data);
      } else if (event.kind === 'exited' || event.kind === 'processUnconfirmed') {
        this.requests.clear(); this.requestKeys.clear();
        if (event.kind === 'processUnconfirmed' && data.messageId != null) this.turn(data.messageId, event.at).legacyTerminal = 'unknown';
        if (data.error) this.note({ key: `note:${event.sequence}`, type: 'note', ...readableError(data.error), at: event.at }, data);
      } else this.turnEvent(event, data);
      this.cursor = event.sequence;
    }
    if (cursor !== this.cursor) throw new Error('Conversation cursor did not match the received events.');
  }
  requestKey(requestId) { const id = JSON.stringify(requestId); return this.requestKeys.get(id) ?? id; }
  note(row, data) { this.rows.push(row); this.turn(data.messageId, row.at).notes.push(row.key); }
}

// Wording is fixed here so every view says the same thing. The badge never
// says "Verified": the host checks the agent's chosen predicates, nothing more.
export const assessmentLabels = { checksPassed: "Agent's browser checks passed", someChecksPassed: 'Some checks passed', notChecked: 'Not checked', blocked: 'Blocked' };
export function assessmentLabel(turn) {
  const final = turn.assessment.final;
  if (final) return assessmentLabels[final.status] || 'Not checked';
  if (turn.recorded && turn.state !== 'ended') return 'Assessment pending';
  return 'Checks not recorded';
}
export const outcomeLabels = { completed: 'Completed', failed: 'Failed', interrupted: 'Interrupted', unknown: 'Outcome unknown' };
export function outcomeLabel(turn) {
  if (turn.outcome) return outcomeLabels[turn.outcome] || 'Outcome unknown';
  if (!turn.recorded && turn.legacyTerminal) return outcomeLabels[turn.legacyTerminal] || 'Outcome unknown';
  return '';
}
// Linked browser actions of one step, in claim order.
export function stepActions(timeline, row) {
  return row.actionIds.filter(id => timeline.stepClaims.get(id) === row.key).map(id => timeline.actions.get(id)).filter(Boolean);
}
// Chip for a browser step. Only the host's recorded verdicts produce
// Confirmed/Failed; reads carry no verdict; an old journal never shows one.
export function stepVerdict(timeline, turn, row) {
  if (!row.browser) return null;
  if (!turn.recorded) return 'Page not recorded';
  const actions = stepActions(timeline, row);
  if (!actions.length) return 'No browser record';
  if (actions.some(action => action.observed?.verdict === 'failed')) return 'Failed';
  const acting = actions.filter(action => action.toolClass !== 'observe');
  if (!acting.length) return null;
  return acting.every(action => action.observed?.verdict === 'confirmed') ? 'Confirmed' : 'Not checked';
}
function sameTarget(a, b) { return (a.targetId && a.targetId === b.targetId) || (!a.targetId && !b.targetId && a.url === b.url); }
// "Acted on" lists only pages the action's own reply named; cached daemon
// state is where the tools were pointed, not proof of an effect. Pages the
// user merely switched to never appear: nothing but evidence writes these.
export function actingPages(turn) {
  const actedOn = [], pointedAt = [], redirects = [];
  for (const action of turn.actions.values()) {
    const after = action.target?.observed?.after;
    if (!after || typeof after !== 'object') continue;
    if (after.provenance === 'actionReply') {
      if (!actedOn.some(target => sameTarget(target, after))) actedOn.push({ ...after, attached: after.chromeTabId != null && after.chromeTabId === turn.attachedPage?.tabId });
      const asked = action.target.requested?.url;
      if (typeof asked === 'string' && typeof after.url === 'string' && normalUrl(asked) !== normalUrl(after.url)) redirects.push({ asked, landed: after.url });
    } else if (!pointedAt.some(target => sameTarget(target, after))) pointedAt.push(after);
  }
  return { actedOn, pointedAt: pointedAt.filter(target => !actedOn.some(acted => sameTarget(acted, target))), redirects };
}
function normalUrl(url) { try { const parsed = new URL(url); parsed.hash = ''; return parsed.href; } catch { return url; } }

// Agent CLIs often report failures as raw API JSON. Show its message as the
// note and keep the original text available as a collapsed detail.
export function readableError(text) {
  const raw = String(text);
  try {
    const parsed = JSON.parse(raw.slice(raw.indexOf('{')));
    const message = parsed?.error?.message || parsed?.message || (typeof parsed?.error === 'string' ? parsed.error : '');
    if (typeof message === 'string' && message.trim()) return { text: message.trim(), detail: raw };
  } catch {}
  return { text: raw };
}

const hiddenItems = new Set(['reasoning', 'userMessage', 'agentMessage', 'plan', 'contextCompaction']);
function shownTool(item) {
  if (item.truncated) return true;
  if (hiddenItems.has(item.type)) return false;
  return Boolean(item.tool || item.name || item.type);
}
// chrome_use tools arrive under several spellings (Codex `chrome_use_open`,
// Pi `mcp__chrome_use__chrome_use_open`, Claude `mcp__chrome_use__...`).
function browserTool(name) {
  const match = /^(?:mcp__chrome_use__)?chrome_use[_.]([a-z_]+)$/.exec(name || '');
  return match ? match[1].replace(/_/g, ' ') : null;
}
// OpenCode runs MCP tools inside a codemode `execute` wrapper; it is a
// browser step when its inner calls name a chrome_use tool.
function browserStep(item) {
  if (browserTool(item.tool || item.name)) return true;
  const calls = item.rawOutput?.metadata?.toolCalls;
  return Array.isArray(calls) && calls.some(call => /^chrome_use[._]/.test(String(call?.tool || '')));
}
function toolHint(item) {
  const args = item.arguments ?? item.input ?? item.rawInput ?? {};
  if (item.type === 'commandExecution' && item.command) return String(item.command).replace(/^\/bin\/\w+ -lc /, '').slice(0, 80);
  if (!args || typeof args !== 'object') return '';
  for (const key of ['url', 'action', 'query', 'ref', 'selector', 'text', 'tabId', 'path', 'command', 'code']) {
    if (typeof args[key] === 'string' && args[key].trim()) return args[key].trim().split('\n')[0].slice(0, 80);
  }
  return '';
}
function toolName(item) {
  const browser = browserTool(item.tool || item.name);
  if (browser) return `Browser · ${browser}`;
  if (item.type === 'commandExecution') return 'Ran a command';
  if (item.type === 'fileChange') return 'Changed files';
  if (item.type === 'webSearch') return 'Searched the web';
  if (item.type === 'tool_result') return 'Tool result';
  return item.tool || item.name || item.type || 'Tool';
}
function resultText(result) {
  if (typeof result === 'string') return result;
  const blocks = Array.isArray(result?.content) ? result.content : Array.isArray(result) ? result : null;
  if (blocks) return blocks.map(block => typeof block?.text === 'string' ? block.text : JSON.stringify(block)).join('\n');
  return result == null ? '' : JSON.stringify(result, null, 2);
}
function describeTool(item) {
  if (item.truncated) return `${item.preview || ''}\n[The tool preview was shortened]`;
  let text;
  if (item.type === 'commandExecution') text = [item.command && `$ ${item.command}`, item.aggregatedOutput].filter(Boolean).join('\n\n');
  else if (item.type === 'fileChange') text = (item.changes || []).map(change => `${change.kind?.type || change.kind || 'change'} ${change.path || ''}`).join('\n');
  else if (item.type === 'mcpToolCall') text = [JSON.stringify(item.arguments ?? {}, null, 2), item.result != null ? resultText(item.result) : item.error ? resultText(item.error) : ''].filter(Boolean).join('\n\n');
  else if (item.type === 'tool_result') text = resultText(item.content);
  else {
    const detail = item.arguments ?? item.input ?? item.content ?? item.result ?? item.output ?? '';
    text = typeof detail === 'string' ? detail : JSON.stringify(detail, null, 2);
  }
  return text.length > 6000 ? `${text.slice(0, 6000)}\n[The tool preview was shortened]` : text;
}

export const phaseLabels = {
  idle: 'Ready', starting: 'Starting agent', running: 'Responding', waitingInput: 'Waiting for your input',
  stopping: 'Waiting for stop confirmation', failed: 'Response failed', interrupted: 'Response interrupted',
  recoveryRequired: 'Previous outcome unconfirmed',
};
