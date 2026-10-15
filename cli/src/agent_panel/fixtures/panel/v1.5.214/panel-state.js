// Project a native journal into visible rows. All content remains text data;
// duplicate cursors and completed-text chunks never duplicate a response.
export class PanelTimeline {
  constructor() { this.cursor = 0; this.rows = []; this.items = new Map(); this.requests = new Map(); }

  apply(events, cursor) {
    for (const event of events) {
      if (event.sequence <= this.cursor) continue;
      if (event.sequence !== this.cursor + 1) throw new Error('Conversation history has a gap. Refresh before relying on this view.');
      const data = event.data || {};
      if (event.kind === 'user') this.rows.push({ key: `user:${data.messageId}`, type: 'user', messageId: data.messageId, text: data.text || '', page: data.page || null, attachments: data.attachments || [], at: event.at });
      else if (event.kind === 'queueEdited' || event.kind === 'queueRemoved') {
        const row = this.rows.find(row => row.type === 'user' && row.messageId === data.messageId);
        if (row) { row.queueStatus = event.kind === 'queueRemoved' ? 'Removed from queue' : 'Edited before sending';
          if (event.kind === 'queueEdited') { row.text = data.text; row.messageId = data.replacementId; }
        }
      }
      else if (event.kind === 'textDelta' || event.kind === 'textFinal') {
        const key = `text:${data.messageId || 'legacy'}:${data.itemId}`;
        let row = this.items.get(key);
        if (!row) { row = { key, type: 'assistant', text: '', at: event.at, finalPart: -1 }; this.items.set(key, row); this.rows.push(row); }
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
        if (!row) { row = { key, type: 'tool', text: '', at: event.at }; this.items.set(key, row); this.rows.push(row); }
        row.name = toolName(item);
        row.hint = toolHint(item);
        row.failed = item.is_error === true || item.status === 'failed' || Boolean(item.error);
        row.completed = data.completed === true;
        row.text = describeTool(item);
      } else if (event.kind === 'request') this.requests.set(JSON.stringify(data.requestId), { ...data, sent: false });
      else if (event.kind === 'replySent') { const request = this.requests.get(JSON.stringify(data.requestId)); if (request) request.sent = true; }
      else if (event.kind === 'requestResolved') this.requests.delete(JSON.stringify(data.requestId));
      else if (event.kind === 'terminal') {
        this.requests.clear();
        if (data.status !== 'completed') this.rows.push({ key: `note:${event.sequence}`, type: 'note', failed: data.status === 'failed', ...readableError(data.error || (data.status === 'interrupted' ? 'Response interrupted. Queued messages are paused.' : 'Response failed. Queued messages are paused.')), at: event.at });
      } else if (event.kind === 'exited' || event.kind === 'processUnconfirmed') {
        this.requests.clear();
        if (data.error) this.rows.push({ key: `note:${event.sequence}`, type: 'note', ...readableError(data.error), at: event.at });
      }
      this.cursor = event.sequence;
    }
    if (cursor !== this.cursor) throw new Error('Conversation cursor did not match the received events.');
  }
}

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
