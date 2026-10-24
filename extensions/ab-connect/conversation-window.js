import { t } from './i18n.js';

// Browser destinations are recorded from actual panel bindings and relay use,
// never inferred from a domain or a tab-group title shared by several sessions.
const LIVE = 'ab_conversation_windows';
const HISTORY = 'ab_conversation_pages';
const webUrl = url => { try { return ['https:', 'http:'].includes(new URL(url).protocol); } catch { return false; } };
export function createConversationWindows({ tabs, windows, storage, boundTab, beforeActivate = () => {} }) {
  const records = new Map();
  let loaded, writes = Promise.resolve();
  const pending = new Map();
  function load() {
    return loaded ??= Promise.all([storage.local.get(HISTORY), storage.session.get(LIVE)]).then(([history, live]) => {
      for (const [id, record] of Object.entries(history[HISTORY] || {})) if (webUrl(record?.url)) records.set(id, { url: record.url });
      for (const [id, record] of Object.entries(live[LIVE] || {})) if (Number.isInteger(record?.tabId)) records.set(id, record);
    });
  }
  function save() {
    // Serialize snapshots so an older asynchronous write cannot replace a new one.
    const entries = [...records].slice(-1000);
    const live = Object.fromEntries(entries), history = Object.fromEntries(entries.filter(([, r]) => webUrl(r.url)).map(([id, r]) => [id, { url: r.url }]));
    const write = writes.catch(() => {}).then(() => Promise.all([storage.session.set({ [LIVE]: live }), storage.local.set({ [HISTORY]: history })]));
    writes = write;
    return write;
  }
  async function remember(id, tab) {
    if (!id || !Number.isInteger(tab?.id)) return;
    await load();
    const previous = records.get(id);
    const url = tab.pendingUrl || tab.url || '';
    const next = { tabId: tab.id, url: webUrl(url) ? url : previous?.url || '' };
    if (previous?.tabId === next.tabId && previous?.url === next.url) return;
    records.delete(id); records.set(id, next);
    await save();
  }
  async function observe(session, tabId) {
    if (!session?.startsWith('panel-') || !Number.isInteger(tabId)) return;
    await remember(session.slice(6), await tabs.get(tabId));
  }
  async function updated(tab) {
    await load();
    for (const [id, record] of records) if (record.tabId === tab.id) await remember(id, tab);
  }
  async function show(id) {
    await load();
    const record = records.get(id);
    const candidate = record?.tabId ?? await boundTab(id);
    let tab;
    if (Number.isInteger(candidate)) {
      try { tab = await tabs.get(candidate); }
      catch (error) {
        // Only a confirmed missing tab authorizes reopening. Other API errors
        // leave the outcome uncertain and must not create a duplicate.
        if (!/no tab with id|invalid tab id/i.test(String(error?.message || error))) throw error;
      }
    }
    if (!tab) {
      if (!webUrl(record?.url)) throw new Error(t('panel_window_no_page'));
      tab = await tabs.create({ url: record.url, active: false });
      // Record before focusing: a focus failure must not create another tab on retry.
      await remember(id, tab);
    }
    beforeActivate(tab.id);
    await tabs.update(tab.id, { active: true });
    await windows.update(tab.windowId, { focused: true });
    const [actual, win] = await Promise.all([tabs.get(tab.id), windows.get(tab.windowId)]);
    if (!actual.active || actual.windowId !== tab.windowId || !win.focused) throw new Error(t('panel_window_focus_failed'));
    await remember(id, actual);
    return { tabId: actual.id, windowId: actual.windowId };
  }
  async function binding(id, tab) {
    await load();
    if (!records.has(id)) await remember(id, tab);
  }
  async function replaced(removedId, tab) {
    await load();
    for (const [id, record] of [...records]) if (record.tabId === removedId) await remember(id, tab);
  }
  async function forget(id) { await load(); records.delete(id); await save(); }
  return { remember, binding, replaced, observe, updated, forget, show(id) {
    if (typeof id !== 'string' || !id || id.length > 200) return Promise.reject(new Error('A conversation is required'));
    if (pending.has(id)) return pending.get(id);
    const task = show(id).finally(() => pending.delete(id)); pending.set(id, task); return task;
  } };
}
