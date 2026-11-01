// A separate native-host protocol: panel permissions cannot authorize input.
export class NativeFocusBridge {
  constructor({ send, timeoutMs = 20000, schedule = (callback, delay) => globalThis.setTimeout(callback, delay), cancel = timer => globalThis.clearTimeout(timer), identity = crypto.randomUUID() }) {
    Object.assign(this, { send, timeoutMs, schedule, cancel, identity });
    this.sequence = 0;
    this.pending = new Map();
  }
  request(operation, params) {
    if (!['probe', 'prepare', 'inspect', 'escape', 'finish'].includes(operation)) return Promise.reject(new Error('Unsupported native focus operation'));
    if (this.pending.size >= 8) return Promise.reject(new Error('Native focus requests are busy'));
    const requestId = `${this.identity}:${++this.sequence}`;
    return new Promise((resolve, reject) => {
      const timer = this.schedule(() => {
        this.pending.delete(requestId);
        reject(new Error('Native focus outcome is unconfirmed; the request was not replayed'));
      }, this.timeoutMs);
      this.pending.set(requestId, { resolve, reject, timer, operation, token: params?.token });
      try { this.send({ method: 'nativeFocus', requestId, operation, params }); }
      catch (error) {
        this.cancel(timer);
        this.pending.delete(requestId);
        reject(error);
      }
    });
  }
  revoke(token) {
    if (typeof token !== 'string' || !/^[0-9a-f]{32}$/i.test(token)) return false;
    // A returned postMessage call is only a revocation request, not proof that
    // an already-started OS dispatch was retracted.
    try { this.send({ method: 'nativeFocusRevoke', token }); return true; }
    catch { return false; }
  }
  receive(message) {
    if (message?.method !== 'nativeFocusReply') return false;
    const pending = this.pending.get(message.requestId);
    if (!pending) return true; // Late replies never enter CDP or cause a resend.
    this.pending.delete(message.requestId);
    this.cancel(pending.timer);
    if (typeof message.error === 'string') pending.reject(new Error(message.error));
    else if (validReply(pending, message.result)) pending.resolve(message.result);
    else pending.reject(new Error('Malformed native focus reply; no request was replayed'));
    return true;
  }
  disconnect() {
    for (const { reject, timer } of this.pending.values()) {
      this.cancel(timer);
      reject(new Error('Native host disconnected; focus outcome is unconfirmed and was not replayed'));
    }
    this.pending.clear();
  }
}

function validReply({ operation, token }, reply) {
  if (reply?.schemaVersion !== 1) return false;
  if (operation === 'probe') return reply.platform === 'macos' && reply.probeOnly === true && reply.nativeInputEnabled === false && reply.token === undefined && reply.leaseMs === undefined;
  if (operation === 'prepare') return typeof reply.token === 'string' && /^[0-9a-f]{32}$/i.test(reply.token) && Number.isInteger(reply.leaseMs) && reply.leaseMs > 0 && reply.leaseMs <= 120000;
  if (operation === 'inspect') return reply.token === token && reply.verified === true;
  if (operation === 'finish') return typeof reply.expired === 'boolean' && typeof reply.restore?.attempted === 'boolean' && typeof reply.restore?.restored === 'boolean';
  return reply.token === token && [0, 1].includes(reply.input?.keyDownAdmitted) && [0, 1].includes(reply.input?.keyUpAdmitted) && reply.input?.delivery === 'unverified' && reply.input?.replayed === false;
}

// Each await is followed by a fresh synchronous ownership/binding check. The
// native host independently verifies the real foreground process/documents.
export async function prepareNativeFocusRecovery(params, deps) {
  if (params?.allowFocusRecovery !== true) throw new Error('Native focus recovery needs explicit invocation permission');
  const url = new URL(params.url);
  if (!['http:', 'https:'].includes(url.protocol)) throw new Error('Native focus recovery needs a web page');
  let cancelled = false;
  let begun = false;
  let finishing = null;
  const assertOwned = (id, cleanup = false) => {
    if (cancelled && !cleanup) throw new Error('Native focus recovery was cancelled');
    if (deps.boundTab() !== id || !deps.isOwned(id)) throw new Error('Native focus target is no longer an owned bound tab');
    deps.guard(id);
  };
  const tabId = deps.boundTab();
  if (!Number.isInteger(tabId)) throw new Error('Native focus recovery has no exact bound tab');
  const read = async (requireFocused, cleanup = false) => {
    assertOwned(tabId, cleanup);
    if (!cleanup && deps.validateBinding && !await deps.validateBinding()) throw new Error('Native focus browser target binding is not live');
    assertOwned(tabId, cleanup);
    const tab = await deps.getTab(tabId);
    assertOwned(tabId, cleanup);
    if (tab.id !== tabId || (!cleanup && tab.url !== params.url) || !tab.active || (!cleanup && tab.pendingUrl && tab.pendingUrl !== params.url)) throw new Error('Native focus page or active tab changed');
    const window = await deps.getWindow(tab.windowId);
    assertOwned(tabId, cleanup);
    if (window.id !== tab.windowId || window.type !== 'normal' || window.state === 'minimized' || (requireFocused && window.focused !== true)) throw new Error('Native focus window is unavailable');
    const tabs = await deps.getWindowTabs(tab.windowId);
    assertOwned(tabId, cleanup);
    if (!tabs.length || !tabs.some(item => item.id === tabId)) throw new Error('Native focus window membership changed');
    for (const item of tabs) {
      deps.guard(item.id);
      if (!deps.isOwned(item.id)) throw new Error('Native focus recovery refuses a window containing user tabs');
    }
    const current = await deps.getTab(tabId);
    assertOwned(tabId, cleanup);
    if ((!cleanup && current.url !== params.url) || !current.active || current.windowId !== tab.windowId || (!cleanup && current.pendingUrl && current.pendingUrl !== params.url)) throw new Error('Native focus binding changed during inspection');
    return tab.windowId;
  };
  const windowId = await read(false);
  const prepared = await deps.request('prepare', { allowFocusRecovery: true, url: params.url });
  let focusRequested = false;
  let input = null;
  return {
    token: prepared.token,
    cancel() { cancelled = true; },
    async begin() {
      if (begun || cancelled) return { started: false, error: 'Native recovery was already started or cancelled', focusRequested, input };
      begun = true;
      try {
        if (await read(false) !== windowId) throw new Error('Native focus window changed before activation');
        assertOwned(tabId);
        focusRequested = true;
        await deps.focusWindow(windowId);
        assertOwned(tabId);
        if (await read(true) !== windowId) throw new Error('Native focus window changed after activation');
        const inspected = await deps.request('inspect', { token: prepared.token });
        if (inspected.verified !== true) throw new Error('Native document inspection was not verified');
        // The native document read is asynchronous. Recheck Chrome's actual active
        // tab afterwards, immediately before the one armed native key request.
        if (await read(true) !== windowId) throw new Error('Native focus tab changed after inspection');
        const escaped = await deps.request('escape', { token: prepared.token });
        input = escaped.input;
        if (input.keyDownAdmitted !== 1 || input.keyUpAdmitted !== 1) throw new Error('Native Escape admission was incomplete; it was not repeated');
        // Renderer verification belongs to the daemon and names its pinned page.
        return { started: true, token: prepared.token, input };
      } catch (error) {
        return { started: false, error: String(error.message || error), focusRequested, input };
      }
    },
    finish() {
      cancelled = true;
      if (!finishing) {
        const revocationRequested = deps.revoke ? deps.revoke(prepared.token) : false;
        finishing = (async () => {
          let restoreAllowed = false;
          try { restoreAllowed = (await read(true, true)) === windowId; } catch { /* Never restore over a changed binding. */ }
          const receipt = await deps.request('finish', { token: prepared.token, restoreAllowed });
          return { ...receipt, focusRequested, revocationRequested };
        })();
      }
      return finishing;
    },
  };
}

// Convenience for isolated controller tests. The production relay returns the
// preparation token to the daemon first, so cancellation can close it before
// any foreground change or key dispatch.
export async function beginNativeFocusRecovery(params, deps) {
  const lease = await prepareNativeFocusRecovery(params, deps);
  const result = await lease.begin();
  if (!result.started) {
    try { result.restore = await lease.finish(); } catch { result.restore = null; }
  }
  return result;
}
