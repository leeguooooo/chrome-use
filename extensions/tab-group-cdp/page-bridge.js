(() => {
  if (window.__AB_STEALTH_BRIDGE_INSTALLED__) return;
  window.__AB_STEALTH_BRIDGE_INSTALLED__ = true;

  const currentScript = document.currentScript;
  const TYPE = currentScript?.dataset?.abBridgeEvent || 'AB_PAGE_BRIDGE_EVENT';

  const post = (kind, payload) => {
    try {
      window.postMessage({ type: TYPE, kind, payload, timestamp: Date.now() }, '*');
    } catch {
      // Ignore post failures.
    }
  };

  const serializeArg = (value, depth = 0) => {
    if (value === null || typeof value === 'undefined') return value;
    if (typeof value === 'string') return value.slice(0, 250);
    if (typeof value === 'number' || typeof value === 'boolean') return value;
    if (value instanceof Error) return `${value.name}: ${value.message}`;
    if (depth > 2) return '[depth-limit]';
    if (Array.isArray(value)) return value.slice(0, 10).map((item) => serializeArg(item, depth + 1));
    if (typeof value === 'object') {
      const out = {};
      const entries = Object.entries(value).slice(0, 12);
      for (const [k, v] of entries) {
        out[k] = serializeArg(v, depth + 1);
      }
      return out;
    }
    return String(value).slice(0, 250);
  };

  const patchConsoleMethod = (name) => {
    const original = console[name];
    if (typeof original !== 'function') return;
    console[name] = function patchedConsole(...args) {
      post('console', {
        level: name,
        args: args.map((arg) => serializeArg(arg)),
      });
      return original.apply(this, args);
    };
  };

  patchConsoleMethod('error');
  patchConsoleMethod('warn');

  window.addEventListener('error', (event) => {
    post('console', {
      level: 'error',
      message: event.message,
      source: event.filename,
      line: event.lineno,
      column: event.colno,
    });
  });

  window.addEventListener('unhandledrejection', (event) => {
    post('console', {
      level: 'error',
      message: 'Unhandled rejection',
      reason: serializeArg(event.reason),
    });
  });

  if (typeof window.fetch === 'function') {
    const originalFetch = window.fetch.bind(window);
    window.fetch = async (...args) => {
      const startedAt = Date.now();
      const requestInfo = args[0];
      const requestInit = args[1] || {};
      const method = requestInit.method || 'GET';
      const url = typeof requestInfo === 'string' ? requestInfo : requestInfo?.url || '';

      try {
        const response = await originalFetch(...args);
        post('network', {
          transport: 'fetch',
          method,
          url,
          status: response.status,
          ok: response.ok,
          durationMs: Date.now() - startedAt,
        });
        return response;
      } catch (error) {
        post('network', {
          transport: 'fetch',
          method,
          url,
          error: serializeArg(error),
          durationMs: Date.now() - startedAt,
        });
        throw error;
      }
    };
  }

  if (typeof window.XMLHttpRequest === 'function') {
    const originalOpen = XMLHttpRequest.prototype.open;
    const originalSend = XMLHttpRequest.prototype.send;

    XMLHttpRequest.prototype.open = function patchedOpen(method, url, ...rest) {
      this.__abRequestMeta = {
        method: typeof method === 'string' ? method : 'GET',
        url: typeof url === 'string' ? url : String(url || ''),
        startedAt: Date.now(),
      };
      return originalOpen.call(this, method, url, ...rest);
    };

    XMLHttpRequest.prototype.send = function patchedSend(...args) {
      this.addEventListener('loadend', () => {
        const meta = this.__abRequestMeta || {};
        post('network', {
          transport: 'xhr',
          method: meta.method || 'GET',
          url: meta.url || '',
          status: this.status,
          ok: this.status >= 200 && this.status < 400,
          durationMs: Date.now() - (meta.startedAt || Date.now()),
        });
      });
      return originalSend.apply(this, args);
    };
  }

  post('lifecycle', {
    event: 'bridge-installed',
    href: location.href,
  });
})();
