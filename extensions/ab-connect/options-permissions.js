// Options page: Permissions & safety (#781). Pending approvals, the settings
// the CLI's site gate reads, the approved and blocked sites, what the
// organization pinned, and the permission history. Changes go through the
// worker (`ab-site-permission`), which refuses a setting the managed policy
// locks.

import { i18nReady, t } from './i18n.js';
import { mountSiteApprovals } from './site-approvals-view.js';
import { CATEGORIES, KINDS, SITE_PERMISSIONS_KEY, effectiveSettings, lockedSettings, managedPolicy, normalizeStore } from './site-permissions.js';

const KIND_KEYS = { download: 'options_perm_kind_download', payment_input: 'options_perm_kind_payment_input', password_input: 'options_perm_kind_password_input', purchase: 'options_perm_kind_purchase' };
const CATEGORY_KEYS = { navigation: 'options_perm_cat_navigation', click: 'options_perm_cat_click', typing: 'options_perm_cat_typing', upload: 'options_perm_cat_upload', download: 'options_perm_cat_download' };
const DECISION_KEYS = { once: 'options_perm_decision_once', always: 'options_perm_decision_always', decline: 'options_perm_decision_decline', withdrawn: 'options_perm_decision_withdrawn' };

/** One history line, in words. */
export function historyLine(entry, tr = t) {
  const when = new Date(Number(entry.at) || 0).toLocaleString();
  const decision = String(entry.decision || '');
  let what;
  if (DECISION_KEYS[decision]) what = tr(DECISION_KEYS[decision]);
  else if (decision.startsWith('added:')) what = tr(decision === 'added:allow' ? 'options_perm_history_added_allow' : 'options_perm_history_added_block');
  else if (decision.startsWith('removed:')) what = tr(decision === 'removed:allow' ? 'options_perm_history_removed_allow' : 'options_perm_history_removed_block');
  else what = decision;
  const detail = entry.description || entry.action || '';
  return { when, host: entry.host || '', what, detail };
}

export function startPermissions({ document: doc, chrome }) {
  const el = id => doc.getElementById(id);
  const send = async msg => {
    const reply = await chrome.runtime.sendMessage({ type: 'ab-site-permission', ...msg });
    if (!reply?.success) throw new Error(reply?.error || t('panel_site_decision_failed'));
    return reply;
  };
  const status = text => { el('permEditStatus').textContent = text || ''; };

  function checks(container, values, keys, selected, locked, onChange) {
    container.replaceChildren();
    for (const value of values) {
      const label = doc.createElement('label');
      const box = doc.createElement('input');
      box.type = 'checkbox'; box.checked = selected.includes(value); box.disabled = locked;
      box.addEventListener('change', () => onChange(value, box.checked));
      label.append(box, doc.createTextNode(t(keys[value])));
      container.append(label);
    }
  }

  function siteList(container, hosts, list, kinds = {}) {
    container.replaceChildren();
    if (!hosts.length) {
      const li = doc.createElement('li'); li.className = 'hint'; li.textContent = t('options_perm_none'); container.append(li); return;
    }
    for (const host of hosts) {
      const li = doc.createElement('li');
      const name = doc.createElement('span'); name.className = 'grow mono'; name.textContent = host;
      if (kinds[host]?.length) {
        const also = doc.createElement('span'); also.className = 'hint';
        also.textContent = ` · ${t('options_perm_also_kinds', kinds[host].map(k => t(KIND_KEYS[k])).join(', '))}`;
        name.append(also);
      }
      const remove = doc.createElement('button'); remove.type = 'button'; remove.className = 'btn ghost'; remove.textContent = t('options_perm_remove');
      remove.addEventListener('click', () => send({ op: 'sites', list, host, remove: true }).catch(e => status(e.message)));
      li.append(name, remove);
      container.append(li);
    }
  }

  async function render() {
    const store = normalizeStore((await chrome.storage.local.get(SITE_PERMISSIONS_KEY))?.[SITE_PERMISSIONS_KEY]);
    let managed = {};
    try { managed = managedPolicy(await chrome.storage.managed.get(null)); } catch {}
    const settings = effectiveSettings(store, managed), locked = lockedSettings(managed);
    el('permManaged').hidden = Object.keys(managed).length === 0;

    const sensitive = el('permSensitive');
    sensitive.checked = settings.sensitiveSites; sensitive.disabled = locked.sensitiveSites;
    el('permSensitiveLock').hidden = !locked.sensitiveSites;
    el('permAskLock').hidden = !locked.ask;
    el('permFirstLock').hidden = !locked.firstVisit;
    const toggle = (key, list) => (value, on) => {
      const next = on ? [...new Set([...list, value])] : list.filter(v => v !== value);
      send({ op: 'settings', patch: { [key]: next } }).catch(e => status(e.message));
    };
    checks(el('permAsk'), KINDS, KIND_KEYS, settings.ask, locked.ask, toggle('ask', settings.ask));
    checks(el('permFirst'), CATEGORIES, CATEGORY_KEYS, settings.firstVisit, locked.firstVisit, toggle('firstVisit', settings.firstVisit));

    siteList(el('permAllowList'), store.allow, 'allow', store.allowKinds);
    siteList(el('permBlockList'), store.block, 'block');

    const managedRows = [
      ['options_perm_managed_allowed', managed.allowedDomains],
      ['options_perm_managed_blocked', managed.blockedDomains],
      ['options_perm_managed_confirm', managed.confirmDomains],
      ['options_perm_managed_agents', managed.disabledAgents],
      ['options_perm_managed_actions', managed.actionPolicy && [JSON.stringify(managed.actionPolicy)]],
    ].filter(([, v]) => Array.isArray(v) && v.length);
    el('permManagedRow').hidden = managedRows.length === 0;
    el('permManagedList').replaceChildren(...managedRows.map(([key, values]) => {
      const li = doc.createElement('li');
      const label = doc.createElement('span'); label.textContent = t(key);
      const value = doc.createElement('span'); value.className = 'grow mono'; value.textContent = values.join(', ');
      li.append(label, value); return li;
    }));

    const history = el('permHistory');
    const entries = [...store.history].reverse();
    history.replaceChildren(...(entries.length ? entries : [null]).map(entry => {
      const li = doc.createElement('li');
      if (!entry) { li.className = 'hint'; li.textContent = t('options_perm_history_empty'); return li; }
      const line = historyLine(entry);
      const text = doc.createElement('span'); text.className = 'grow';
      text.textContent = `${line.what} · ${line.host}${line.detail ? ` · ${line.detail}` : ''}`;
      const when = doc.createElement('span'); when.className = 'hint'; when.textContent = line.when;
      li.append(text, when); return li;
    }));
  }

  el('permSensitive').addEventListener('change', event => {
    send({ op: 'settings', patch: { sensitiveSites: event.target.checked } }).catch(e => { status(e.message); void render(); });
  });
  for (const list of ['allow', 'block']) {
    const input = el(list === 'allow' ? 'permAllowInput' : 'permBlockInput');
    const add = () => send({ op: 'sites', list, host: input.value }).then(() => { input.value = ''; status(''); }, e => status(e.message));
    el(list === 'allow' ? 'permAllowAdd' : 'permBlockAdd').addEventListener('click', add);
    input.addEventListener('keydown', event => { if (event.key === 'Enter') add(); });
  }
  el('permHistoryClear').addEventListener('click', () => send({ op: 'clearHistory' }).catch(e => status(e.message)));
  chrome.storage.onChanged.addListener((changes, area) => {
    if ((area === 'local' && SITE_PERMISSIONS_KEY in changes) || area === 'managed') void render();
  });
  mountSiteApprovals({ doc, container: el('permPending'), storage: chrome.storage, runtime: chrome.runtime, t });
  return render();
}

if (typeof document !== 'undefined' && globalThis.chrome?.storage?.local && globalThis.chrome?.storage?.session) {
  void i18nReady.then(() => startPermissions({ document, chrome: globalThis.chrome })).catch(error => console.warn('Permissions:', String(error?.message || error)));
}
