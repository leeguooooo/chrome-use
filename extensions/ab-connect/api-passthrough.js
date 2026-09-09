// `ABExt.call`: one generic, allow-listed door onto `chrome.*` so a new CLI
// feature that only needs an existing browser API no longer needs a new
// extension release (and a store review, and every user to update).
//
// Seven of the eleven hand-written `ABExt.*` handlers that shipped between
// 0.5.13 and 0.5.24 (renameGroup, ungroupTab, duplicateTab, attachedTargets,
// adoptByUrl, listDownloads, clearDownloads) are thin wrappers over calls
// this module admits. Behaviour fixes inside the worker still need releases;
// this removes the other class.
//
// Two rules, both enforced here rather than trusted to the daemon:
//
//   1. Allow-list by namespace AND method. Anything not listed is refused
//      with the list, so a typo never reaches Chrome as a mystery error.
//      `debugger`, `identity`, `storage`, `nativeMessaging`, `runtime`,
//      `management` are not on the list and never will be through this door.
//   2. Mutations are ownership-gated. A tab the agent did not create (or
//      adopt) cannot be updated, moved, grouped, reloaded or closed through
//      here — the v1.5.95 lesson (a tab the user was reading got closed).
//      Read-only methods carry no such gate.
//
// Kept free of `chrome.*` so every rule is unit-testable; the worker passes
// the real API and its ownership set in.

export const POLICY_VERSION = 'call-v1';

/** namespace -> { read: [...], mutate: [...] }. Not listed == refused. */
export const ALLOWED = Object.freeze({
  tabs: {
    read: ['query', 'get', 'getCurrent', 'getZoom', 'detectLanguage'],
    mutate: [
      'update',
      'move',
      'remove',
      'reload',
      'discard',
      'group',
      'ungroup',
      'setZoom',
      'duplicate',
      'goBack',
      'goForward',
    ],
  },
  tabGroups: {
    read: ['query', 'get'],
    mutate: ['update', 'move'],
  },
  windows: {
    // create / remove are refused on purpose: a window is the user's, and
    // closing one takes every tab in it with it.
    read: ['get', 'getAll', 'getCurrent', 'getLastFocused'],
    mutate: ['update'],
  },
  downloads: {
    read: ['search'],
    mutate: ['cancel', 'pause', 'resume', 'erase'],
  },
  webNavigation: {
    read: ['getFrame', 'getAllFrames'],
    mutate: [],
  },
});

const MAX_ARGS = 4;
const MAX_ARG_BYTES = 64 * 1024;

/** The advertised shape, for `hello` capabilities and error messages. */
export function policySummary() {
  const out = {};
  for (const [ns, { read, mutate }] of Object.entries(ALLOWED)) {
    out[ns] = { read: [...read], mutate: [...mutate] };
  }
  return { version: POLICY_VERSION, namespaces: out };
}

/**
 * Sync validation of a request. Returns `{ ok: true, namespace, method,
 * args, mutates }` or `{ ok: false, error }`. Never touches `chrome`.
 */
export function validateCall(request) {
  const namespace = typeof request?.namespace === 'string' ? request.namespace : '';
  const method = typeof request?.method === 'string' ? request.method : '';
  const args = request?.args === undefined ? [] : request.args;
  if (!namespace || !method) {
    return { ok: false, error: 'call: `namespace` and `method` are required' };
  }
  const policy = ALLOWED[namespace];
  if (!policy) {
    return {
      ok: false,
      error: `call: namespace '${namespace}' is not allowed. Allowed: ${Object.keys(ALLOWED).join(', ')}`,
    };
  }
  const mutates = policy.mutate.includes(method);
  if (!mutates && !policy.read.includes(method)) {
    const all = [...policy.read, ...policy.mutate];
    return {
      ok: false,
      error: `call: ${namespace}.${method} is not allowed. Allowed on ${namespace}: ${all.join(', ')}`,
    };
  }
  if (!Array.isArray(args)) {
    return { ok: false, error: 'call: `args` must be an array (positional arguments)' };
  }
  if (args.length > MAX_ARGS) {
    return { ok: false, error: `call: at most ${MAX_ARGS} arguments` };
  }
  let bytes = 0;
  for (const a of args) {
    if (typeof a === 'function') {
      return { ok: false, error: 'call: arguments must be JSON values' };
    }
    try {
      bytes += JSON.stringify(a === undefined ? null : a).length;
    } catch {
      return { ok: false, error: 'call: arguments must be JSON values' };
    }
  }
  if (bytes > MAX_ARG_BYTES) {
    return { ok: false, error: `call: arguments exceed ${MAX_ARG_BYTES} bytes` };
  }
  return { ok: true, namespace, method, args, mutates };
}

/**
 * Which tabs a mutating call would touch. Async because a group or a window
 * has to be expanded into its tabs. `lookups` supplies `tabsInGroup(groupId)`
 * and `tabsInWindow(windowId)`, each resolving to an array of tab ids.
 *
 * Returns `{ tabIds }` when the target set could be determined, or
 * `{ error }` when the call's shape gives no way to know what it touches
 * (in which case the call is refused: unknown scope is not permission).
 */
export async function mutationTargets(call, lookups) {
  const { namespace, method, args } = call;
  const first = args[0];
  const ids = (v) => (Array.isArray(v) ? v : [v]).filter((x) => Number.isInteger(x));

  if (namespace === 'tabs') {
    if (method === 'group') {
      const tabIds = ids(first?.tabIds);
      if (tabIds.length === 0) return { error: 'tabs.group: `tabIds` is required' };
      return { tabIds };
    }
    const tabIds = ids(first);
    if (tabIds.length === 0) {
      return { error: `tabs.${method}: a tab id (or array of ids) is required as the first argument` };
    }
    return { tabIds };
  }
  if (namespace === 'tabGroups') {
    if (!Number.isInteger(first)) return { error: `tabGroups.${method}: a group id is required` };
    const tabIds = await lookups.tabsInGroup(first);
    if (!Array.isArray(tabIds) || tabIds.length === 0) {
      return { error: `tabGroups.${method}: group ${first} has no tabs the relay can see` };
    }
    return { tabIds };
  }
  if (namespace === 'windows') {
    if (!Number.isInteger(first)) return { error: 'windows.update: a window id is required' };
    const tabIds = await lookups.tabsInWindow(first);
    if (!Array.isArray(tabIds) || tabIds.length === 0) {
      return { error: `windows.update: window ${first} has no tabs the relay can see` };
    }
    return { tabIds };
  }
  if (namespace === 'downloads') {
    // Downloads are not tabs. `erase` by an open-ended query could wipe the
    // user's history, so it needs an explicit id; cancel/pause/resume take one.
    if (method === 'erase') {
      if (!Number.isInteger(first?.id)) return { error: 'downloads.erase: `{ id }` is required' };
      return { tabIds: [] };
    }
    if (!Number.isInteger(first)) return { error: `downloads.${method}: a download id is required` };
    return { tabIds: [] };
  }
  return { error: `${namespace}.${method}: no ownership rule; refused` };
}

/**
 * Validate, gate, and run one call. `env` supplies:
 *   - api:       the `chrome` object (or a stub in tests)
 *   - isOwned:   (tabId) => boolean
 *   - tabsInGroup / tabsInWindow: see `mutationTargets`
 *
 * Resolves to `{ result }` or throws an Error whose message names the rule
 * that refused it. A Chrome API rejection is passed through with the
 * `namespace.method` prefix so the daemon can tell policy from Chrome.
 */
export async function executeCall(request, env) {
  const v = validateCall(request);
  if (!v.ok) throw new Error(v.error);
  if (v.mutates) {
    const scope = await mutationTargets(v, env);
    if (scope.error) throw new Error(`call: ${scope.error}`);
    const foreign = scope.tabIds.filter((id) => !env.isOwned(id));
    if (foreign.length > 0) {
      throw new Error(
        `call: ${v.namespace}.${v.method} refused — tab${foreign.length > 1 ? 's' : ''} ${foreign.join(', ')} ` +
          `${foreign.length > 1 ? 'are' : 'is'} not owned by this relay (agent-created or adopted tabs only)`
      );
    }
  }
  const ns = env.api?.[v.namespace];
  const fn = ns?.[v.method];
  if (typeof fn !== 'function') {
    throw new Error(`call: chrome.${v.namespace}.${v.method} is unavailable in this Chrome`);
  }
  try {
    const result = await fn.apply(ns, v.args);
    return { result: result === undefined ? null : result };
  } catch (e) {
    const msg = String((e && e.message) || e);
    throw new Error(`${v.namespace}.${v.method}: ${msg}`);
  }
}
