// Per-site permissions (#781): the approved-sites list, the permission
// history, the pending approval requests and the managed (enterprise) policy.
//
// The daemon decides. It asks `ABExt.sitePolicy` for the layers kept here
// (the person's decisions and settings, and what an administrator pinned in
// `chrome.storage.managed`), merges them with the policy file and the
// built-in sensitive-site list (cli/src/native/site_policy.rs), and, when an
// action needs a person, files a request with `ABExt.siteApproval`. The
// person answers in the side panel, the options page or the notification;
// the daemon reads the answer back. Nothing here touches a page, so the gate
// leaves no trace a site can see (stealth first).
//
// Kept free of `chrome.*` so every rule is unit-testable; the worker passes
// the real APIs into `createSitePermissions`.

export const SITE_PERMISSIONS_KEY = 'ab_site_permissions';
export const SITE_REQUESTS_KEY = 'ab_site_requests';
export const CATEGORIES = Object.freeze(['navigation', 'click', 'typing', 'upload', 'download']);
export const KINDS = Object.freeze(['payment_input', 'purchase', 'password_input', 'download']);
// Payment fields and purchases ask by default. Password / one-time-code
// fields and downloads are opt-in: agents sign in (`auth login`,
// `fill --from-env`) and run download automations unattended.
export const DEFAULT_ASK = Object.freeze(['payment_input', 'purchase']);
export const DEFAULT_SETTINGS = Object.freeze({ sensitiveSites: true, ask: [...DEFAULT_ASK], firstVisit: [] });
export const HISTORY_LIMIT = 200;
export const DECISIONS = Object.freeze(['once', 'always', 'decline']);
/** An unanswered request is dropped after this long; an answered one sooner. */
export const REQUEST_TTL_MS = 10 * 60 * 1000;
export const DECIDED_TTL_MS = 2 * 60 * 1000;
const MAX_PENDING = 20;
const MAX_LIST = 500;
const MANAGED_LISTS = ['allowedDomains', 'blockedDomains', 'confirmDomains'];

/** A host or pattern as people write it, reduced to `example.com`. */
export function normalizeHost(value) {
  let p = String(value ?? '').trim().toLowerCase();
  if (p === '*') return '*';
  p = p.split('://').pop();
  p = p.split(/[/?#]/)[0].split(':')[0];
  p = p.replace(/^(\*\.)+/, '').replace(/^\.+/, '').replace(/\.+$/, '');
  return /^[a-z0-9.-]+$/.test(p) || /^[^\s/]+$/.test(p) ? p : '';
}

/** A domain covers itself and its subdomains (same rule as the CLI). */
export function hostMatches(pattern, host) {
  const p = normalizeHost(pattern);
  if (p === '*') return true;
  return !!p && (host === p || host.endsWith(`.${p}`));
}

const hostList = list => [...new Set((Array.isArray(list) ? list : []).filter(v => typeof v === 'string').map(normalizeHost).filter(Boolean))].slice(0, MAX_LIST);
const pick = (list, allowed) => (Array.isArray(list) ? [...new Set(list.filter(v => allowed.includes(v)))] : undefined);

export function emptyStore() {
  return { allow: [], block: [], allowKinds: {}, settings: {}, history: [] };
}

/**
 * `{ host: [kind, ...] }`: the always-ask kinds (a payment field, a
 * purchase ...) a person allowed for good on one site by choosing "Always
 * allow on this site" on that kind's request (#846). Only for hosts that are
 * still on the approved list.
 */
function kindMap(raw, allow) {
  const out = {};
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return out;
  for (const [key, kinds] of Object.entries(raw)) {
    const host = normalizeHost(key);
    if (!host || host === '*' || !allow.includes(host)) continue;
    const list = pick(kinds, KINDS);
    if (list?.length) out[host] = list;
  }
  return out;
}

/** What is in storage, made safe to use whatever an older build left there. */
export function normalizeStore(raw) {
  const s = raw && typeof raw === 'object' ? raw : {};
  const settings = s.settings && typeof s.settings === 'object' ? s.settings : {};
  const allow = hostList(s.allow);
  const out = { allow, block: hostList(s.block), allowKinds: kindMap(s.allowKinds, allow), settings: {}, history: Array.isArray(s.history) ? s.history.slice(-HISTORY_LIMIT) : [] };
  if (typeof settings.sensitiveSites === 'boolean') out.settings.sensitiveSites = settings.sensitiveSites;
  const ask = pick(settings.ask, KINDS); if (ask) out.settings.ask = ask;
  const firstVisit = pick(settings.firstVisit, CATEGORIES); if (firstVisit) out.settings.firstVisit = firstVisit;
  return out;
}

/**
 * The managed policy (chrome.storage.managed, schema in managed-schema.json),
 * keeping only well-formed values. Unknown keys are dropped.
 */
export function managedPolicy(raw) {
  const m = raw && typeof raw === 'object' ? raw : {};
  const out = {};
  for (const key of MANAGED_LISTS) if (Array.isArray(m[key])) out[key] = hostList(m[key]);
  if (typeof m.sensitiveSites === 'boolean') out.sensitiveSites = m.sensitiveSites;
  const ask = pick(m.ask, KINDS); if (ask) out.ask = ask;
  const firstVisit = pick(m.firstVisit, CATEGORIES); if (firstVisit) out.firstVisit = firstVisit;
  if (m.actionPolicy && typeof m.actionPolicy === 'object' && !Array.isArray(m.actionPolicy)) {
    const ap = {};
    if (m.actionPolicy.default === 'allow' || m.actionPolicy.default === 'deny') ap.default = m.actionPolicy.default;
    for (const key of ['allow', 'deny', 'confirm']) if (Array.isArray(m.actionPolicy[key])) ap[key] = m.actionPolicy[key].filter(a => typeof a === 'string' && a).slice(0, MAX_LIST);
    out.actionPolicy = ap;
  }
  if (Array.isArray(m.disabledAgents)) out.disabledAgents = m.disabledAgents.filter(a => typeof a === 'string' && a).slice(0, 32);
  return out;
}

/** Which settings an administrator pinned (the UI shows them locked). */
export function lockedSettings(managed) {
  const m = managed || {};
  return { sensitiveSites: 'sensitiveSites' in m, ask: 'ask' in m, firstVisit: 'firstVisit' in m };
}

/** The settings in force: managed, else the person's, else the defaults. */
export function effectiveSettings(store, managed) {
  const m = managed || {}, s = normalizeStore(store).settings;
  return {
    sensitiveSites: m.sensitiveSites ?? s.sensitiveSites ?? DEFAULT_SETTINGS.sensitiveSites,
    ask: m.ask ?? s.ask ?? [...DEFAULT_SETTINGS.ask],
    firstVisit: m.firstVisit ?? s.firstVisit ?? [...DEFAULT_SETTINGS.firstVisit],
  };
}

/** The `ABExt.sitePolicy` reply: raw layers; the daemon merges them. */
export function sitePolicyReply(store, managed) {
  const s = normalizeStore(store);
  return { version: 1, user: { allow: s.allow, block: s.block, allowKinds: s.allowKinds, ...s.settings }, managed: managed || {} };
}

/** Drop expired requests. */
export function pruneRequests(requests, now) {
  return (Array.isArray(requests) ? requests : []).filter(r => r && now - Number(r.decidedAt || r.at || 0) < (r.decision ? DECIDED_TTL_MS : REQUEST_TTL_MS));
}

const text = (v, max) => (typeof v === 'string' ? v.slice(0, max) : '');

/** File a request from the daemon. Throws on a malformed one. */
export function addRequest(requests, params, now) {
  const id = text(params?.id, 64), host = normalizeHost(params?.host);
  if (!/^[A-Za-z0-9_-]{1,64}$/.test(id)) throw new Error('siteApproval: bad id');
  const request = {
    id, host,
    category: text(params?.category, 32),
    reason: text(params?.reason, 32),
    description: text(params?.description, 300),
    action: text(params?.action, 40),
    session: text(params?.session, 80),
    at: now,
  };
  // One pending request per session: a new gated action replaces the last,
  // as the daemon's own pending slot does.
  const rest = pruneRequests(requests, now).filter(r => r.id !== id && !(r.session === request.session && !r.decision));
  return [...rest, request].slice(-MAX_PENDING);
}

export function requestStatus(requests, id) {
  const r = (requests || []).find(x => x.id === id);
  return r ? { known: true, decision: r.decision || null } : { known: false, decision: null };
}

function historyEntry(request, decision, now, by) {
  return { at: now, host: request.host, category: request.category, reason: request.reason, description: request.description, action: request.action, decision, by };
}

/**
 * Record a person's choice. `once` / `always` / `decline` answer the
 * request; `always` also adds the site to the approved list; `cancel`
 * (the agent withdrew it with `deny`) only closes it.
 * Returns `{ store, requests, entry }`; throws when the request is unknown or
 * was already answered.
 */
export function applyDecision(store, requests, id, decision, now, by = 'panel') {
  if (![...DECISIONS, 'cancel'].includes(decision)) throw new Error(`siteApproval: unknown decision ${decision}`);
  const list = pruneRequests(requests, now);
  const request = list.find(r => r.id === id);
  if (!request) throw new Error('This request expired or was already withdrawn');
  if (request.decision) throw new Error('This request was already answered');
  const s = normalizeStore(store);
  const recorded = decision === 'cancel' ? 'withdrawn' : decision;
  if (decision === 'always' && request.host) {
    s.allow = hostList([...s.allow, request.host]);
    s.block = s.block.filter(h => h !== request.host);
    // An always-ask kind asks even on an approved site, so "always" on its
    // request has to remember the kind too, or the next fill asks again.
    if (KINDS.includes(request.reason)) {
      s.allowKinds = { ...s.allowKinds, [request.host]: [...new Set([...(s.allowKinds[request.host] || []), request.reason])] };
    }
  }
  const entry = historyEntry(request, recorded, now, by);
  s.history = [...s.history, entry].slice(-HISTORY_LIMIT);
  const next = decision === 'cancel'
    ? list.filter(r => r.id !== id)
    : list.map(r => (r.id === id ? { ...r, decision, decidedAt: now } : r));
  return { store: s, requests: next, entry };
}

/** Add or remove a site on the approved (`allow`) or blocked (`block`) list. */
export function editSites(store, { list, host, remove = false }, now) {
  if (list !== 'allow' && list !== 'block') throw new Error('Unknown list');
  const h = normalizeHost(host);
  if (!h || h === '*') throw new Error('Enter a site such as example.com');
  const s = normalizeStore(store);
  if (remove) {
    s[list] = s[list].filter(x => x !== h);
    if (list === 'allow') { const { [h]: _gone, ...rest } = s.allowKinds; s.allowKinds = rest; }
  } else {
    s[list] = hostList([...s[list], h]);
    const other = list === 'allow' ? 'block' : 'allow';
    s[other] = s[other].filter(x => x !== h);
    if (list === 'block') { const { [h]: _gone, ...rest } = s.allowKinds; s.allowKinds = rest; }
  }
  s.history = [...s.history, { at: now, host: h, decision: remove ? `removed:${list}` : `added:${list}`, by: 'options' }].slice(-HISTORY_LIMIT);
  return s;
}

/** Change settings; a setting the administrator pinned is refused. */
export function updateSettings(store, patch, managed) {
  const s = normalizeStore(store), locked = lockedSettings(managed);
  for (const key of Object.keys(patch || {})) {
    if (!(key in DEFAULT_SETTINGS)) throw new Error(`Unknown setting ${key}`);
    if (locked[key]) throw new Error('This setting is managed by your organization');
  }
  return normalizeStore({ ...s, settings: { ...s.settings, ...patch } });
}

/**
 * The worker's side: storage, notifications and the relay methods.
 * `api` carries `storage` (local, session, managed), `notifications`
 * and `t` (i18n), so tests can pass fakes.
 */
export function createSitePermissions(api) {
  let managed = {};
  let chain = Promise.resolve();
  // One read-modify-write at a time: a decision and a new request racing
  // must not lose either.
  const serial = fn => { const run = chain.then(fn, fn); chain = run.catch(() => {}); return run; };
  const now = () => (api.now ? api.now() : Date.now());
  const loadStore = async () => (await api.storage.local.get(SITE_PERMISSIONS_KEY))?.[SITE_PERMISSIONS_KEY];
  const loadRequests = async () => pruneRequests((await api.storage.session.get(SITE_REQUESTS_KEY))?.[SITE_REQUESTS_KEY], now());
  // The toolbar badge belongs to finished turns and attached tabs; a request
  // shows as a notification and in the side panel instead.
  const saveRequests = requests => api.storage.session.set({ [SITE_REQUESTS_KEY]: requests });
  async function refreshManaged() {
    try { managed = managedPolicy(await api.storage.managed.get(null)); } catch { managed = {}; }
    return managed;
  }
  const ready = refreshManaged();

  return {
    ready,
    refreshManaged,
    managed: () => managed,
    async policy() {
      await ready;
      return sitePolicyReply(await loadStore(), managed);
    },
    /** `ABExt.siteApproval`: request / status / decide (terminal prompt) / cancel. */
    approval(params) {
      const op = params?.op;
      if (op === 'status') return loadRequests().then(requests => requestStatus(requests, params.id));
      if (op === 'request') return serial(async () => {
        const requests = addRequest(await loadRequests(), params, now());
        await saveRequests(requests);
        const request = requests[requests.length - 1];
        try {
          api.notifications?.create?.(`ab-site-${request.id}`, {
            type: 'basic', iconUrl: 'icons/icon128.png', priority: 2, requireInteraction: true,
            title: api.t('panel_site_approval_notification_title', request.host),
            message: request.description || request.host,
          });
        } catch {}
        return { filed: true, id: request.id };
      });
      if (op === 'decide' || op === 'cancel') return serial(async () => {
        const decision = op === 'cancel' ? 'cancel' : params.decision === 'always' ? 'always' : params.decision;
        const out = applyDecision(await loadStore(), await loadRequests(), params.id, decision, now(), 'terminal');
        await api.storage.local.set({ [SITE_PERMISSIONS_KEY]: out.store });
        await saveRequests(out.requests);
        try { api.notifications?.clear?.(`ab-site-${params.id}`); } catch {}
        return { recorded: true };
      });
      return Promise.reject(new Error(`siteApproval: unknown op ${op}`));
    },
    /** Messages from the side panel / options page (`ab-site-permission`). */
    onMessage(msg) {
      return serial(async () => {
        const store = await loadStore();
        if (msg.op === 'decide') {
          // Only the agent withdraws a request (`chrome-use deny`).
          if (!DECISIONS.includes(msg.decision)) throw new Error('Choose Allow once, Always allow on this site, or Decline');
          const out = applyDecision(store, await loadRequests(), msg.id, msg.decision, now(), 'panel');
          await api.storage.local.set({ [SITE_PERMISSIONS_KEY]: out.store });
          await saveRequests(out.requests);
          try { api.notifications?.clear?.(`ab-site-${msg.id}`); } catch {}
          return { recorded: true };
        }
        if (msg.op === 'sites') { await api.storage.local.set({ [SITE_PERMISSIONS_KEY]: editSites(store, msg, now()) }); return { saved: true }; }
        if (msg.op === 'settings') { await api.storage.local.set({ [SITE_PERMISSIONS_KEY]: updateSettings(store, msg.patch, managed) }); return { saved: true }; }
        if (msg.op === 'clearHistory') { await api.storage.local.set({ [SITE_PERMISSIONS_KEY]: { ...normalizeStore(store), history: [] } }); return { saved: true }; }
        throw new Error(`Unknown site permission operation ${msg.op}`);
      });
    },
  };
}
