(() => {
  const REQUEST_TYPE = 'AB_TAB_GROUP_REQUEST';
  const RESPONSE_TYPE = 'AB_TAB_GROUP_RESPONSE';

  const CONTENT_EVENT_TYPE = 'AB_CONTENT_EVENT';
  const CONTENT_EXECUTE_ACTION = 'AB_CONTENT_EXECUTE_ACTION';
  const CONTENT_GET_DOM_STATE = 'AB_CONTENT_GET_DOM_STATE';
  const CONTENT_PING = 'AB_CONTENT_PING';
  const PAGE_BRIDGE_EVENT = 'AB_PAGE_BRIDGE_EVENT';
  const STORAGE_OPTIONS_KEY = 'abExtensionOptionsV1';

  const mutationState = {
    total: 0,
    recent: [],
    observerReady: false,
  };

  function pushMutationSummary(entry) {
    mutationState.total += 1;
    mutationState.recent.push({
      ...entry,
      timestamp: Date.now(),
    });
    if (mutationState.recent.length > 40) {
      mutationState.recent.splice(0, mutationState.recent.length - 40);
    }
  }

  function serializeValue(value, depth = 0) {
    if (value === null || typeof value === 'undefined') return value;
    if (typeof value === 'string') return value.slice(0, 300);
    if (typeof value === 'number' || typeof value === 'boolean') return value;
    if (value instanceof Error) return `${value.name}: ${value.message}`;
    if (depth > 2) return '[depth-limit]';

    if (Array.isArray(value)) {
      return value.slice(0, 10).map((item) => serializeValue(item, depth + 1));
    }

    if (typeof value === 'object') {
      const out = {};
      for (const [key, entry] of Object.entries(value).slice(0, 15)) {
        out[key] = serializeValue(entry, depth + 1);
      }
      return out;
    }

    return String(value).slice(0, 300);
  }

  function sendRuntimeEvent(kind, payload) {
    try {
      chrome.runtime.sendMessage({
        type: CONTENT_EVENT_TYPE,
        kind,
        payload: serializeValue(payload),
        url: window.location.href,
        title: document.title,
        timestamp: Date.now(),
      });
    } catch {
      // Ignore runtime channel errors.
    }
  }

  function getPageBridgeEnabled() {
    return new Promise((resolve) => {
      try {
        chrome.storage.local.get([STORAGE_OPTIONS_KEY], (result) => {
          if (chrome.runtime.lastError) {
            resolve(false);
            return;
          }
          const rawOptions = result?.[STORAGE_OPTIONS_KEY];
          resolve(Boolean(rawOptions && typeof rawOptions === 'object' && rawOptions.pageBridgeEnabled === true));
        });
      } catch {
        resolve(false);
      }
    });
  }

  async function installPageBridge() {
    // Receives events emitted by the injected page-world hook script.
    const bridgeListener = (event) => {
      if (event.source !== window) return;
      const data = event.data;
      if (!data || data.type !== PAGE_BRIDGE_EVENT) return;
      sendRuntimeEvent(data.kind || 'page-event', data.payload || {});
    };

    window.addEventListener('message', bridgeListener);

    const parent = document.documentElement || document.head || document.body;
    if (!parent) return;

    if (!(await getPageBridgeEnabled())) {
      sendRuntimeEvent('lifecycle', {
        event: 'bridge-disabled-default',
      });
      return;
    }

    // Use external extension script instead of inline text to reduce CSP conflicts.
    const script = document.createElement('script');
    script.src = chrome.runtime.getURL('page-bridge.js');
    script.async = false;
    script.dataset.abBridgeEvent = PAGE_BRIDGE_EVENT;
    script.onload = () => script.remove();
    script.onerror = () => {
      sendRuntimeEvent('lifecycle', {
        event: 'bridge-load-failed',
        host: window.location.hostname,
      });
      script.remove();
    };
    parent.appendChild(script);
  }

  function ensureMutationObserver() {
    if (mutationState.observerReady) return;
    if (!document.documentElement) return;

    const observer = new MutationObserver((records) => {
      const summary = {
        records: records.length,
        addedNodes: 0,
        removedNodes: 0,
      };

      for (const record of records.slice(0, 40)) {
        summary.addedNodes += record.addedNodes?.length || 0;
        summary.removedNodes += record.removedNodes?.length || 0;
      }

      pushMutationSummary(summary);
    });

    observer.observe(document.documentElement, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: ['class', 'style', 'hidden', 'disabled', 'aria-hidden'],
    });

    mutationState.observerReady = true;
  }

  function toSimpleNode(element) {
    if (!element || typeof element !== 'object') return null;
    const node = {
      tag: element.tagName?.toLowerCase() || 'unknown',
      id: element.id || undefined,
      className: typeof element.className === 'string' ? element.className.slice(0, 120) : '',
      role: element.getAttribute?.('role') || undefined,
      name:
        element.getAttribute?.('aria-label') ||
        element.getAttribute?.('name') ||
        element.getAttribute?.('placeholder') ||
        '',
      text: (element.textContent || '').trim().replace(/\s+/g, ' ').slice(0, 160),
      disabled: element.disabled === true,
      hidden: element.hidden === true,
    };

    return node;
  }

  function collectInteractiveElements(root, limit = 80) {
    const selector = [
      'a[href]',
      'button',
      'input',
      'select',
      'textarea',
      'summary',
      '[role="button"]',
      '[role="link"]',
      '[tabindex]'
    ].join(',');

    const out = [];
    const nodes = root.querySelectorAll(selector);
    for (const element of nodes) {
      if (out.length >= limit) break;
      out.push(toSimpleNode(element));
    }
    return out.filter(Boolean);
  }

  function collectDomState(options = {}) {
    const selector = typeof options.selector === 'string' ? options.selector.trim() : '';
    const root = selector ? document.querySelector(selector) : document.body || document.documentElement;

    if (!root) {
      return {
        ok: false,
        error: selector ? `selector-not-found: ${selector}` : 'root-not-found',
      };
    }

    const textPreview = (root.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 1000);
    const interactiveOnly = options.interactiveOnly === true;
    const interactiveElements = collectInteractiveElements(root, options.maxNodes || 80);

    const dom = {
      href: window.location.href,
      title: document.title,
      readyState: document.readyState,
      selector: selector || null,
      rootTag: root.tagName?.toLowerCase() || 'unknown',
      textPreview,
      interactiveCount: interactiveElements.length,
      interactiveElements,
      mutation: {
        total: mutationState.total,
        recent: mutationState.recent.slice(-10),
      },
      capturedAt: Date.now(),
    };

    if (interactiveOnly) {
      dom.textPreview = '';
    }

    return {
      ok: true,
      state: dom,
    };
  }

  function queryElement(selector) {
    if (typeof selector !== 'string' || selector.trim().length === 0) {
      throw new Error('selector is required');
    }
    const element = document.querySelector(selector);
    if (!element) {
      throw new Error(`Element not found: ${selector}`);
    }
    return element;
  }

  function focusElement(element) {
    if (typeof element.focus === 'function') {
      element.focus({ preventScroll: false });
    }
  }

  function dispatchInputEvents(element) {
    element.dispatchEvent(new Event('input', { bubbles: true }));
    element.dispatchEvent(new Event('change', { bubbles: true }));
  }

  async function executeAction(command, args = {}) {
    switch (command) {
      case 'click': {
        const element = queryElement(args.selector);
        focusElement(element);
        element.click();
        return { ok: true, action: command, selector: args.selector };
      }
      case 'fill': {
        const element = queryElement(args.selector);
        if (!('value' in element)) {
          throw new Error(`Element is not fillable: ${args.selector}`);
        }
        focusElement(element);
        element.value = typeof args.value === 'string' ? args.value : String(args.value || '');
        dispatchInputEvents(element);
        return { ok: true, action: command, selector: args.selector, valueLength: element.value.length };
      }
      case 'press': {
        const key = typeof args.key === 'string' && args.key.trim().length > 0 ? args.key.trim() : 'Enter';
        let target;
        if (typeof args.selector === 'string' && args.selector.trim().length > 0) {
          target = queryElement(args.selector);
          focusElement(target);
        } else {
          target = document.activeElement || document.body;
        }

        const down = new KeyboardEvent('keydown', { key, bubbles: true });
        const up = new KeyboardEvent('keyup', { key, bubbles: true });
        target.dispatchEvent(down);
        target.dispatchEvent(up);
        return { ok: true, action: command, key };
      }
      case 'eval': {
        if (typeof args.expression !== 'string' || args.expression.trim().length === 0) {
          throw new Error('expression is required');
        }
        const fn = new Function(`return (${args.expression});`);
        const result = fn();
        return { ok: true, action: command, result: serializeValue(result) };
      }
      case 'snapshot': {
        return {
          ok: true,
          action: command,
          ...collectDomState({
            selector: args.selector,
            interactiveOnly: args.interactiveOnly === true,
            maxNodes: args.maxNodes,
          }),
        };
      }
      default:
        throw new Error(`Unknown content action: ${command}`);
    }
  }

  ensureMutationObserver();
  installPageBridge();

  window.addEventListener('message', (event) => {
    if (event.source !== window) {
      return;
    }

    const data = event.data;
    if (!data || data.type !== REQUEST_TYPE) {
      return;
    }

    const request = {
      type: REQUEST_TYPE,
      nonce: data.nonce,
      session: data.session,
      groupTitle: data.groupTitle,
      pluginId: data.pluginId,
      allowedDomains: Array.isArray(data.allowedDomains) ? data.allowedDomains : undefined,
    };

    try {
      chrome.runtime.sendMessage(request, (response) => {
        const lastError = chrome.runtime.lastError;
        if (lastError) {
          window.postMessage(
            {
              type: RESPONSE_TYPE,
              nonce: request.nonce,
              ok: false,
              error: lastError.message,
            },
            '*'
          );
          return;
        }

        const payload = response && typeof response === 'object' ? response : { ok: false };

        window.postMessage(
          {
            type: RESPONSE_TYPE,
            nonce: request.nonce,
            ok: payload.ok === true,
            extensionId:
              typeof payload.extensionId === 'string' && payload.extensionId.length > 0
                ? payload.extensionId
                : chrome.runtime.id,
            groupId: typeof payload.groupId === 'number' ? payload.groupId : undefined,
            windowId: typeof payload.windowId === 'number' ? payload.windowId : undefined,
            color: typeof payload.color === 'string' ? payload.color : undefined,
            collapsed: payload.collapsed === true,
            policy:
              payload.policy && typeof payload.policy === 'object'
                ? {
                    enforced: payload.policy.enforced === true,
                    blocked: payload.policy.blocked === true,
                    reason:
                      typeof payload.policy.reason === 'string' ? payload.policy.reason : undefined,
                  }
                : undefined,
            riskHints: Array.isArray(payload.riskHints) ? payload.riskHints : undefined,
            error: typeof payload.error === 'string' ? payload.error : undefined,
          },
          '*'
        );
      });
    } catch (error) {
      const errorMessage = error instanceof Error ? error.message : String(error);
      window.postMessage(
        {
          type: RESPONSE_TYPE,
          nonce: request.nonce,
          ok: false,
          error: errorMessage,
        },
        '*'
      );
    }
  });

  chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
    if (!message || typeof message !== 'object') return;

    if (message.type === CONTENT_PING) {
      sendResponse({
        ok: true,
        href: window.location.href,
        title: document.title,
        readyState: document.readyState,
      });
      return;
    }

    if (message.type === CONTENT_GET_DOM_STATE) {
      sendResponse(collectDomState(message.options || {}));
      return;
    }

    if (message.type === CONTENT_EXECUTE_ACTION) {
      executeAction(message.command, message.args || {})
        .then((result) => sendResponse(result))
        .catch((error) => {
          sendResponse({
            ok: false,
            action: message.command,
            error: error instanceof Error ? error.message : String(error),
          });
        });
      return true;
    }
  });
})();
