// Notify when a side-panel turn ends or waits for the user, unless the user is
// already looking at that conversation. The cursor stores conversation ids and
// turn tokens only, never titles, prompts or answers.
import { t } from './i18n.js';

const KEY = 'ab_turn_notifications';
const ENDED = new Set(['idle', 'failed', 'interrupted']);
const BUSY = new Set(['starting', 'running', 'stopping']);
const agentNames = { codex: 'Codex', claude: 'Claude Code', opencode: 'OpenCode', pi: 'Pi' };

/** What a conversation summary asks the user to notice, keyed so it fires once. */
export function turnEvent(c) {
  // Scheduled runs are announced by ScheduleNotifications.
  if (!c?.id || c.scheduleId) return null;
  if (c.phase === 'waitingInput' && c.waitingRequest != null && !c.fullAccess) {
    return { token: `w:${c.generation}:${c.waitingRequest}`, kind: 'waiting', at: c.updated || 0 };
  }
  if (c.phase === 'recoveryRequired') {
    return { token: `r:${c.activeMessageId || c.lastTurn?.messageId || c.updated}`, kind: 'recovery', at: c.updated || 0 };
  }
  if (ENDED.has(c.phase) && c.lastTurn?.messageId) {
    return { token: `t:${c.lastTurn.messageId}`, kind: c.lastTurn.outcome, at: c.lastTurn.ended || 0 };
  }
  return null;
}

function firstLine(text, max) {
  const line = String(text || '').split('\n').map(s => s.trim()).find(Boolean) || '';
  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}

/** Plain-words outcome plus at most the first line of the redacted answer. */
export function turnMessage(c, kind) {
  let text;
  if (kind === 'completed') text = c.lastTurn?.preview ? t('notify_turn_done_preview', c.lastTurn.preview) : t('notify_turn_done');
  else if (kind === 'failed') text = c.lastError ? t('notify_turn_failed_error', firstLine(c.lastError, 80)) : t('notify_turn_failed');
  else if (kind === 'interrupted') text = t('notify_turn_stopped');
  else if (kind === 'waiting') text = t('notify_turn_waiting_approval');
  else text = t('notify_turn_outcome_unconfirmed');
  return firstLine(text, 120);
}

export class TurnNotifications {
  constructor({ storage, notifications, badge, now = Date.now }) {
    this.storage = storage; this.notifications = notifications; this.badge = badge; this.now = now;
    this.startedAt = now(); this.state = null; this.running = false;
  }
  async load() {
    if (!this.state) {
      const saved = (await this.storage.get(KEY))[KEY] || {};
      this.state = { seen: saved.seen || {}, unseen: saved.unseen || {} };
    }
    return this.state;
  }
  async save() {
    const { seen } = this.state;
    // Keep cursors of conversations from other Chrome profiles' hosts.
    if (Object.keys(seen).length > 256) this.state.seen = Object.fromEntries(Object.entries(seen).slice(-256));
    await this.storage.set({ [KEY]: this.state });
    const count = Object.keys(this.state.unseen).length;
    this.badge?.(count ? (count > 9 ? '9+' : String(count)) : '');
  }
  /**
   * @param read resolves to the service's `conversationList` result
   * @param options.enabled the user's "Notify when a task finishes" setting
   * @param options.showing resolves whether a focused panel shows that conversation
   */
  async poll(read, { enabled = true, showing = async () => false } = {}) {
    if (this.running) return;
    this.running = true;
    try {
      const { seen, unseen } = await this.load();
      const { conversations } = await read();
      const live = new Set();
      for (const c of conversations || []) {
        live.add(c.id);
        // A new turn means the user has moved on from the last one. Its cursor
        // lets the turn notify even if it ends while the worker is stopped.
        if (BUSY.has(c.phase)) {
          delete unseen[c.id];
          if (c.activeMessageId) seen[c.id] = `b:${c.activeMessageId}`;
        }
        const event = turnEvent(c);
        if (!event || seen[c.id] === event.token) continue;
        const fresh = event.at >= this.startedAt || c.id in seen;
        // Acknowledging a recovery leaves the last ended turn in place; that
        // is not a new finish.
        const acknowledged = String(seen[c.id]).startsWith('r:') && event.token.startsWith('t:');
        seen[c.id] = event.token;
        if (!fresh || !enabled || acknowledged) continue;
        if (await showing(c.id)) { delete unseen[c.id]; continue; }
        unseen[c.id] = event.token;
        const title = firstLine(c.title || c.site || agentNames[c.agentId] || 'chrome-use', 80);
        await this.notifications.create(`panel-turn:${c.id}`, { type: 'basic', iconUrl: 'icons/icon128.png', title, message: turnMessage(c, event.kind) });
      }
      for (const id of Object.keys(unseen)) if (!live.has(id)) delete unseen[id];
      if (!enabled) this.state.unseen = {};
      await this.save();
    } finally { this.running = false; }
  }
  /** The panel shows `id`: its badge entry and notification are done. */
  async seen(id) {
    const { unseen } = await this.load();
    if (!(id in unseen)) return;
    delete unseen[id];
    await this.save();
    await this.notifications.clear?.(`panel-turn:${id}`);
  }
}

/**
 * Route a click on a panel-turn or schedule-run notification to its
 * conversation: store the selection for a panel that opens, switch any panel
 * already open in that window, and open the side panel. `sidePanel.open` needs
 * the click's user gesture, so it is called before anything is awaited.
 * `pending` receives the selection for a panel that has yet to report.
 */
export function openFromNotification(id, { windowId, chrome, panels, pending = () => {} }) {
  const match = /^(schedule-run|panel-turn):([a-f0-9-]{36})$/.exec(id);
  if (!match) return false;
  const conversationId = match[2];
  void Promise.resolve(chrome.storage.local.set({ ab_panel_conversation: conversationId })).catch(() => {});
  const open = windowId => {
    pending({ windowId, conversationId, at: Date.now() });
    void Promise.resolve(chrome.sidePanel.open({ windowId })).catch(error => console.warn('Open the side panel to read the finished task:', String(error?.message || error)));
    void Promise.resolve(chrome.windows.update(windowId, { focused: true })).catch(() => {});
    for (const [port, presence] of panels) {
      if (presence.windowId === windowId) try { port.postMessage({ type: 'select', conversationId }); } catch {}
    }
  };
  // A click that woke the worker has no window yet; looking it up may cost
  // the gesture, so the stored selection still applies when the panel opens.
  if (windowId !== null && windowId !== undefined) open(windowId);
  else void Promise.resolve(chrome.windows.getLastFocused()).then(window => { if (window?.id !== undefined) open(window.id); }).catch(() => {});
  void Promise.resolve(chrome.notifications.clear(id)).catch(() => {});
  return true;
}
