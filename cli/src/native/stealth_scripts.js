const __abStealth = { locale: "en-US", languages: ["en-US", "en"], allowWebGLContextFallback: false };
(function(){
  const removeWebdriver = (target) => {
    if (!target) return;
    try { delete target.webdriver; } catch {}
  };
  removeWebdriver(navigator);
  removeWebdriver(Object.getPrototypeOf(navigator));
  removeWebdriver(Navigator.prototype);
  if (typeof WorkerNavigator !== 'undefined') {
    removeWebdriver(WorkerNavigator.prototype);
  }
})();
(function(){
  if (typeof CSS === 'undefined' || typeof CSS.supports !== 'function') return;
  const nativeSupports = CSS.supports.bind(CSS);
  const normalize = (value) => String(value).replace(/\s+/g, ' ').trim().toLowerCase();
  const target = 'border-end-end-radius: initial';
  const patchedSupports = function(...args) {
    if (args.length === 1 && normalize(args[0]) === target) {
      return false;
    }
    if (args.length >= 2 && normalize(args[0] + ': ' + args[1]) === target) {
      return false;
    }
    return nativeSupports(...args);
  };
  try {
    Object.defineProperty(patchedSupports, 'name', { value: 'supports', configurable: true });
    Object.defineProperty(patchedSupports, 'toString', {
      value: () => nativeSupports.toString(),
      configurable: true,
    });
  } catch {}
  try {
    Object.defineProperty(CSS, 'supports', {
      value: patchedSupports,
      configurable: true,
      writable: true,
    });
  } catch {
    try { CSS.supports = patchedSupports; } catch {}
  }
})();
(function(){
  if (typeof Document === 'undefined') return;
  const AUTO_SELECTOR = '[disable-devtool-auto]';
  const NEVER_MATCH_SELECTOR = 'script[__ab_disable_devtool_never_match__="1"]';
  const normalize = (value) => {
    if (typeof value !== 'string') return '';
    return value.replace(/\s+/g, '').toLowerCase();
  };
  const shouldHideSelector = (selector) => normalize(selector) === AUTO_SELECTOR;

  const patchQueryMethod = (proto, method) => {
    if (!proto) return;
    const native = proto[method];
    if (typeof native !== 'function') return;

    const wrapped = function(selector, ...args) {
      if (shouldHideSelector(selector)) {
        return native.call(this, NEVER_MATCH_SELECTOR, ...args);
      }
      return native.call(this, selector, ...args);
    };
    try {
      Object.defineProperty(wrapped, 'name', {
        value: native.name,
        configurable: true,
      });
      Object.defineProperty(wrapped, 'toString', {
        value: () => native.toString(),
        configurable: true,
      });
    } catch {}

    try {
      Object.defineProperty(proto, method, {
        value: wrapped,
        configurable: true,
        writable: true,
      });
    } catch {}
  };

  patchQueryMethod(Document.prototype, 'querySelector');
  patchQueryMethod(Document.prototype, 'querySelectorAll');
  if (typeof Element !== 'undefined') {
    patchQueryMethod(Element.prototype, 'querySelector');
    patchQueryMethod(Element.prototype, 'querySelectorAll');
  }
})();
(function(){
  const chromeObject = ('chrome' in window && window.chrome) ? window.chrome : {};
  if (!('chrome' in window)) {
    try {
      Object.defineProperty(Window.prototype, 'chrome', {
        get: () => chromeObject,
        configurable: true,
      });
    } catch {
      try { Object.defineProperty(window, 'chrome', { value: chromeObject, configurable: true }); } catch {}
    }
  }
  if (!chromeObject.runtime) {
    const makeEvent = () => ({
      addListener: () => {},
      removeListener: () => {},
      hasListener: () => false,
      hasListeners: () => false,
      dispatch: () => {},
    });
    const makePort = () => ({
      name: '',
      sender: undefined,
      disconnect: () => {},
      onDisconnect: makeEvent(),
      onMessage: makeEvent(),
      postMessage: () => {},
    });
    const runtime = {
      id: undefined,
      connect: () => makePort(),
      sendMessage: () => undefined,
      onConnect: makeEvent(),
      onMessage: makeEvent(),
    };
    Object.defineProperty(chromeObject, 'runtime', {
      value: runtime,
      configurable: true,
    });
  }
})();
(function(){
  const chromeObject = ('chrome' in window && window.chrome) ? window.chrome : null;
  if (!chromeObject) return;
  const nativeNow = Date.now;
  const nativeToString = Function.prototype.toString;
  const timing = window.performance && window.performance.timing ? window.performance.timing : null;
  const getNavigationEntry = () => {
    try {
      return performance.getEntriesByType('navigation')[0] || { nextHopProtocol: 'h2', type: 'other' };
    } catch {
      return { nextHopProtocol: 'h2', type: 'other' };
    }
  };
  const defineValue = (target, key, value) => {
    try {
      Object.defineProperty(target, key, {
        value,
        configurable: true,
        writable: true,
      });
      return true;
    } catch {
      return false;
    }
  };
  const patchFunctionShape = (fn, name) => {
    try {
      Object.defineProperty(fn, 'name', { value: name, configurable: true });
      Object.defineProperty(fn, 'toString', {
        value: () => nativeToString.call(nativeNow).replace('now', name),
        configurable: true,
      });
    } catch {}
  };

  if (!('app' in chromeObject)) {
    const invokeError = (name) => new TypeError('Error in invocation of app.' + name + '()');
    const app = {
      isInstalled: false,
      InstallState: {
        DISABLED: 'disabled',
        INSTALLED: 'installed',
        NOT_INSTALLED: 'not_installed',
      },
      RunningState: {
        CANNOT_RUN: 'cannot_run',
        READY_TO_RUN: 'ready_to_run',
        RUNNING: 'running',
      },
      getDetails: function getDetails() {
        if (arguments.length) throw invokeError('getDetails');
        return null;
      },
      getIsInstalled: function getIsInstalled() {
        if (arguments.length) throw invokeError('getIsInstalled');
        return false;
      },
      runningState: function runningState() {
        if (arguments.length) throw invokeError('runningState');
        return 'cannot_run';
      },
    };
    defineValue(chromeObject, 'app', app);
  }

  if (!('csi' in chromeObject) && timing) {
    const csi = function csi() {
      return {
        onloadT: timing.domContentLoadedEventEnd,
        startE: timing.navigationStart,
        pageT: Date.now() - timing.navigationStart,
        tran: 15,
      };
    };
    patchFunctionShape(csi, 'csi');
    defineValue(chromeObject, 'csi', csi);
  }

  if (!('loadTimes' in chromeObject) && timing) {
    const toFixed = (num, fixed) => {
      const matcher = new RegExp('^-?\\d+(?:.\\d{0,' + (fixed || -1) + '})?');
      const match = String(num).match(matcher);
      return match ? match[0] : String(num);
    };
    const loadTimes = function loadTimes() {
      const navigationEntry = getNavigationEntry();
      const nextHopProtocol = navigationEntry.nextHopProtocol || 'h2';
      let firstPaint = timing.loadEventEnd / 1000;
      try {
        const paintEntries = performance.getEntriesByType('paint');
        if (paintEntries && paintEntries[0] && typeof paintEntries[0].startTime === 'number') {
          firstPaint = (paintEntries[0].startTime + performance.timeOrigin) / 1000;
        }
      } catch {}
      return {
        connectionInfo: nextHopProtocol,
        npnNegotiatedProtocol: ['h2', 'hq'].includes(nextHopProtocol) ? nextHopProtocol : 'unknown',
        navigationType: navigationEntry.type || 'other',
        wasAlternateProtocolAvailable: false,
        wasFetchedViaSpdy: ['h2', 'hq'].includes(nextHopProtocol),
        wasNpnNegotiated: ['h2', 'hq'].includes(nextHopProtocol),
        firstPaintAfterLoadTime: 0,
        requestTime: timing.navigationStart / 1000,
        startLoadTime: timing.navigationStart / 1000,
        commitLoadTime: timing.responseStart / 1000,
        finishDocumentLoadTime: timing.domContentLoadedEventEnd / 1000,
        finishLoadTime: timing.loadEventEnd / 1000,
        firstPaintTime: toFixed(firstPaint, 3),
      };
    };
    patchFunctionShape(loadTimes, 'loadTimes');
    defineValue(chromeObject, 'loadTimes', loadTimes);
  }
})();
(function(){
  if (typeof document === 'undefined' || typeof document.createElement !== 'function') return;
  const nativeCreateElement = document.createElement.bind(document);
  const nativeSrcdocDescriptor =
    typeof HTMLIFrameElement !== 'undefined'
      ? Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype, 'srcdoc')
      : null;
  const srcdocGetter = nativeSrcdocDescriptor && nativeSrcdocDescriptor.get;
  const srcdocSetter = nativeSrcdocDescriptor && nativeSrcdocDescriptor.set;
  const iframeProxyMap = new WeakMap();
  const patchedIframes = new WeakSet();

  const ensureContentWindowProxy = (iframe) => {
    if (!iframe || iframeProxyMap.has(iframe)) return;
    try {
      if (iframe.contentWindow) return;
    } catch {}
    const proxy = new Proxy(window, {
      get(target, key) {
        if (key === 'self') return proxy;
        if (key === 'frameElement') return iframe;
        if (key === '0') return undefined;
        return Reflect.get(target, key, target);
      },
    });
    iframeProxyMap.set(iframe, proxy);
    try {
      Object.defineProperty(iframe, 'contentWindow', {
        get: () => proxy,
        set: () => undefined,
        enumerable: true,
        configurable: false,
      });
    } catch {}
  };

  const patchIframeSrcdoc = (iframe) => {
    if (!iframe || patchedIframes.has(iframe)) return;
    patchedIframes.add(iframe);
    try {
      Object.defineProperty(iframe, 'srcdoc', {
        configurable: true,
        get() {
          if (typeof srcdocGetter === 'function') {
            return srcdocGetter.call(this);
          }
          return '';
        },
        set(value) {
          ensureContentWindowProxy(this);
          if (typeof srcdocSetter === 'function') {
            srcdocSetter.call(this, value);
          } else {
            this.setAttribute('srcdoc', String(value ?? ''));
          }
        },
      });
    } catch {}
  };

  const patchedCreateElement = function(...args) {
    const element = nativeCreateElement(...args);
    try {
      const name = args && args.length > 0 ? String(args[0]).toLowerCase() : '';
      if (name === 'iframe') {
        patchIframeSrcdoc(element);
      }
    } catch {}
    return element;
  };
  try {
    Object.defineProperty(patchedCreateElement, 'name', {
      value: 'createElement',
      configurable: true,
    });
    Object.defineProperty(patchedCreateElement, 'toString', {
      value: () => nativeCreateElement.toString(),
      configurable: true,
    });
  } catch {}
  try {
    Object.defineProperty(document, 'createElement', {
      value: patchedCreateElement,
      configurable: true,
      writable: true,
    });
  } catch {
    try { document.createElement = patchedCreateElement; } catch {}
  }
})();
(function(){
  const config = (typeof __abStealth === 'object' && __abStealth) ? __abStealth : null;
  if (!config || !Array.isArray(config.languages) || config.languages.length === 0) return;
  const locale = typeof config.locale === 'string' ? config.locale : config.languages[0];
  try {
    Object.defineProperty(navigator, 'language', {
      get: () => locale,
      configurable: true,
    });
  } catch {}
  try {
    Object.defineProperty(navigator, 'languages', {
      get: () => config.languages.slice(),
      configurable: true,
    });
  } catch {}
})();
(function(){
  const ua = String(navigator.userAgent || '');
  if (!/Chrome\//.test(ua) || /Firefox\//.test(ua)) return;
  const target = 'Google Inc.';
  const proto = Object.getPrototypeOf(navigator);
  try {
    if (navigator.vendor === target) return;
  } catch {}
  const defineVendor = (targetObj) => {
    if (!targetObj) return false;
    try {
      Object.defineProperty(targetObj, 'vendor', {
        get: () => target,
        configurable: true,
      });
      return true;
    } catch {
      return false;
    }
  };
  if (defineVendor(proto)) {
    try { delete (navigator).vendor; } catch {}
    return;
  }
  defineVendor(navigator);
})();
(function(){
  const makeMimeType = (type, suffixes, description) => {
    const mime = Object.create(MimeType.prototype);
    Object.defineProperties(mime, {
      type: { value: type, enumerable: true },
      suffixes: { value: suffixes, enumerable: true },
      description: { value: description, enumerable: true },
      enabledPlugin: { value: null, writable: true, enumerable: true },
    });
    return mime;
  };

  const makePlugin = (name, description, filename, mimes) => {
    const plugin = Object.create(Plugin.prototype);
    Object.defineProperties(plugin, {
      name: { value: name, enumerable: true },
      description: { value: description, enumerable: true },
      filename: { value: filename, enumerable: true },
      length: { value: mimes.length, enumerable: true },
    });
    mimes.forEach((mime, i) => {
      Object.defineProperty(plugin, i, {
        value: mime,
        enumerable: true,
      });
      Object.defineProperty(plugin, mime.type, {
        value: mime,
        enumerable: false,
      });
      try { mime.enabledPlugin = plugin; } catch {}
    });
    return plugin;
  };

  const pdfMime = makeMimeType('application/pdf', 'pdf', 'Portable Document Format');
  const chromePdfMime = makeMimeType(
    'application/x-google-chrome-pdf',
    'pdf',
    'Portable Document Format'
  );
  const naclMime = makeMimeType('application/x-nacl', '', 'Native Client Executable');
  const pnaclMime = makeMimeType('application/x-pnacl', '', 'Portable Native Client Executable');

  const plugins = [
    makePlugin('Chrome PDF Plugin', 'Portable Document Format', 'internal-pdf-viewer', [chromePdfMime]),
    makePlugin('Chrome PDF Viewer', '', 'mhjfbmdgcfjbbpaeojofohoefgiehjai', [pdfMime]),
    makePlugin('Native Client', '', 'internal-nacl-plugin', [naclMime, pnaclMime]),
  ];
  const pluginArray = Object.create(PluginArray.prototype);
  plugins.forEach((p, i) => {
    pluginArray[i] = p;
    pluginArray[p.name] = p;
  });
  Object.defineProperty(pluginArray, 'length', { get: () => plugins.length });
  pluginArray.item = (i) => plugins[i] || null;
  pluginArray.namedItem = (name) => plugins.find(p => p.name === name) || null;
  pluginArray.refresh = () => {};
  pluginArray[Symbol.iterator] = function*() { for (const p of plugins) yield p; };

  const mimeTypes = [chromePdfMime, pdfMime, naclMime, pnaclMime];
  const mimeTypeArray = Object.create(MimeTypeArray.prototype);
  mimeTypes.forEach((m, i) => {
    mimeTypeArray[i] = m;
    mimeTypeArray[m.type] = m;
  });
  Object.defineProperty(mimeTypeArray, 'length', { get: () => mimeTypes.length });
  mimeTypeArray.item = (i) => mimeTypes[i] || null;
  mimeTypeArray.namedItem = (name) => mimeTypes.find(m => m.type === name) || null;
  mimeTypeArray[Symbol.iterator] = function*() { for (const m of mimeTypes) yield m; };

  Object.defineProperty(navigator, 'plugins', {
    get: () => pluginArray,
    configurable: true,
  });
  Object.defineProperty(navigator, 'mimeTypes', {
    get: () => mimeTypeArray,
    configurable: true,
  });
})();
(function(){
  if (!navigator.permissions || !navigator.permissions.query) return;
  const origQuery = navigator.permissions.query.bind(navigator.permissions);
  const makePermissionStatus = (state) => {
    if (typeof PermissionStatus !== 'undefined') {
      const status = Object.create(PermissionStatus.prototype);
      Object.defineProperty(status, 'state', {
        value: state,
        writable: false,
        enumerable: true,
      });
      Object.defineProperty(status, 'onchange', {
        value: null,
        writable: true,
        enumerable: true,
      });
      return status;
    }
    return { state, onchange: null };
  };
  const patchedQuery = new Proxy(origQuery, {
    apply(target, thisArg, argList) {
      const params = argList && argList[0];
      if (params && params.name === 'notifications') {
        const state = (typeof Notification !== 'undefined' && Notification.permission) || 'default';
        return Promise.resolve(makePermissionStatus(state));
      }
      return Reflect.apply(target, navigator.permissions, argList);
    }
  });
  try {
    Object.defineProperty(navigator.permissions, 'query', {
      value: patchedQuery,
      configurable: true,
      writable: true,
    });
  } catch {}
})();
(function(){
  const getCtx = HTMLCanvasElement.prototype.getContext;
  const WEBGL_VENDOR = 'Intel Inc.';
  const WEBGL_RENDERER = 'Intel Iris OpenGL Engine';
  const DEBUG_RENDERER_INFO = {
    UNMASKED_VENDOR_WEBGL: 0x9245,
    UNMASKED_RENDERER_WEBGL: 0x9246,
  };

  const createFallbackWebGLContext = (canvas, requestedType) => {
    const isWebGL2 = requestedType === 'webgl2';
    const ctx = {
      __abFallbackWebGLContext: true,
      canvas,
      drawingBufferWidth: canvas.width || 300,
      drawingBufferHeight: canvas.height || 150,
      VENDOR: 0x1F00,
      RENDERER: 0x1F01,
      VERSION: 0x1F02,
      SHADING_LANGUAGE_VERSION: 0x8B8C,
      getExtension(name) {
        if (name === 'WEBGL_debug_renderer_info') return DEBUG_RENDERER_INFO;
        return null;
      },
      getSupportedExtensions() {
        return ['WEBGL_debug_renderer_info'];
      },
      getContextAttributes() {
        return {
          alpha: true,
          antialias: true,
          depth: true,
          desynchronized: false,
          failIfMajorPerformanceCaveat: false,
          powerPreference: 'default',
          premultipliedAlpha: true,
          preserveDrawingBuffer: false,
          stencil: false,
        };
      },
      getParameter(param) {
        if (param === DEBUG_RENDERER_INFO.UNMASKED_VENDOR_WEBGL || param === this.VENDOR) {
          return WEBGL_VENDOR;
        }
        if (param === DEBUG_RENDERER_INFO.UNMASKED_RENDERER_WEBGL || param === this.RENDERER) {
          return WEBGL_RENDERER;
        }
        if (param === this.VERSION) {
          return isWebGL2
            ? 'WebGL 2.0 (OpenGL ES 3.0 Chromium)'
            : 'WebGL 1.0 (OpenGL ES 2.0 Chromium)';
        }
        if (param === this.SHADING_LANGUAGE_VERSION) {
          return isWebGL2
            ? 'WebGL GLSL ES 3.00 (OpenGL ES GLSL ES 3.0 Chromium)'
            : 'WebGL GLSL ES 1.0 (OpenGL ES GLSL ES 1.0 Chromium)';
        }
        return 0;
      },
      getError() { return 0; },
      clear() {},
      clearColor() {},
      createBuffer() { return {}; },
      bindBuffer() {},
      bufferData() {},
      createProgram() { return {}; },
      createShader() { return {}; },
      shaderSource() {},
      compileShader() {},
      attachShader() {},
      linkProgram() {},
      useProgram() {},
      viewport() {},
      drawArrays() {},
      readPixels() {},
      finish() {},
      flush() {},
    };
    try {
      const proto =
        requestedType === 'webgl2' && typeof WebGL2RenderingContext !== 'undefined'
          ? WebGL2RenderingContext.prototype
          : typeof WebGLRenderingContext !== 'undefined'
            ? WebGLRenderingContext.prototype
            : null;
      if (proto) Object.setPrototypeOf(ctx, proto);
    } catch {}
    return ctx;
  };

  HTMLCanvasElement.prototype.getContext = function(type, attrs) {
    const ctx = getCtx.call(this, type, attrs);
    if (
      (type === 'webgl' || type === 'webgl2' || type === 'experimental-webgl') &&
      !ctx &&
      __abStealth &&
      __abStealth.allowWebGLContextFallback === true
    ) {
      return createFallbackWebGLContext(this, type);
    }
    if (ctx && (type === 'webgl' || type === 'webgl2' || type === 'experimental-webgl')) {
      const origGetParameter = ctx.getParameter.bind(ctx);
      ctx.getParameter = function(param) {
        const ext = ctx.getExtension('WEBGL_debug_renderer_info');
        if (ext) {
          if (param === ext.UNMASKED_VENDOR_WEBGL) {
            const real = origGetParameter(param);
            return (real && real.includes('SwiftShader')) ? WEBGL_VENDOR : real;
          }
          if (param === ext.UNMASKED_RENDERER_WEBGL) {
            const real = origGetParameter(param);
            return (real && real.includes('SwiftShader')) ? WEBGL_RENDERER : real;
          }
        }
        if (param === ctx.VENDOR) return WEBGL_VENDOR;
        if (param === ctx.RENDERER) return WEBGL_RENDERER;
        return origGetParameter(param);
      };
    }
    return ctx;
  };
})();
(function(){
  const clean = (target) => {
    for (const key of Object.keys(target)) {
      if (/^cdc_|^\$cdc_/.test(key)) {
        delete target[key];
      }
    }
  };
  clean(document);
  if (document.documentElement) clean(document.documentElement);
})();
(function(){
  if (typeof Error === 'undefined') return;
  const sanitizeStack = (value) => {
    if (typeof value !== 'string') return value;
    let stack = value;
    stack = stack.replace(/\/\/# sourceURL=.*$/gm, '');
    stack = stack.replace(/__playwright_evaluation_script__/g, '<anonymous>');
    stack = stack.replace(/__puppeteer_evaluation_script__/g, '<anonymous>');
    stack = stack.replace(/__pw_evaluation_script__/g, '<anonymous>');
    return stack;
  };

  const nativePrepare = Error.prepareStackTrace;
  Error.prepareStackTrace = function(error, structuredStackTrace) {
    let stackString;
    if (typeof nativePrepare === 'function') {
      stackString = nativePrepare.call(this, error, structuredStackTrace);
    } else {
      const name = error && error.name ? String(error.name) : 'Error';
      const message = error && error.message ? String(error.message) : '';
      const header = message ? name + ': ' + message : name;
      const frames = Array.isArray(structuredStackTrace)
        ? structuredStackTrace.map((frame) => '    at ' + String(frame))
        : [];
      stackString = [header].concat(frames).join('\n');
    }
    return sanitizeStack(String(stackString));
  };

  if (typeof Error.captureStackTrace === 'function') {
    const nativeCapture = Error.captureStackTrace;
    Error.captureStackTrace = function(targetObject, constructorOpt) {
      nativeCapture.call(this, targetObject, constructorOpt);
      try {
        const stack = targetObject && targetObject.stack;
        if (typeof stack === 'string') {
          Object.defineProperty(targetObject, 'stack', {
            value: sanitizeStack(stack),
            configurable: true,
            writable: true,
          });
        }
      } catch {}
    };
  }
})();
(function(){
  const widthDelta = 12;
  const heightDelta = 74;
  const patchWidth =
    !Number.isFinite(window.outerWidth) ||
    window.outerWidth === 0 ||
    Math.abs(window.outerWidth - window.innerWidth) <= 1;
  const patchHeight =
    !Number.isFinite(window.outerHeight) ||
    window.outerHeight === 0 ||
    Math.abs(window.outerHeight - window.innerHeight) <= 1;
  if (patchWidth) {
    try {
      Object.defineProperty(window, 'outerWidth', {
        get: () => Math.max(window.innerWidth + widthDelta, window.innerWidth),
        configurable: true,
      });
    } catch {}
  }
  if (patchHeight) {
    try {
      Object.defineProperty(window, 'outerHeight', {
        get: () => Math.max(window.innerHeight + heightDelta, window.innerHeight),
        configurable: true,
      });
    } catch {}
  }
  const patchScreenPosition =
    (!Number.isFinite(window.screenX) || !Number.isFinite(window.screenY)) ||
    (window.screenX === 0 && window.screenY === 0 && (patchWidth || patchHeight));
  if (patchScreenPosition) {
    try {
      Object.defineProperty(window, 'screenX', {
        get: () => 16,
        configurable: true,
      });
      Object.defineProperty(window, 'screenY', {
        get: () => 72,
        configurable: true,
      });
      Object.defineProperty(window, 'screenLeft', {
        get: () => 16,
        configurable: true,
      });
      Object.defineProperty(window, 'screenTop', {
        get: () => 72,
        configurable: true,
      });
    } catch {}
  }
})();
(function(){
  const patchNumber = (target, key, value) => {
    try {
      Object.defineProperty(target, key, {
        get: () => value,
        configurable: true,
      });
    } catch {}
  };
  const width = Number(screen.width);
  const height = Number(screen.height);
  const innerWidth = Number(window.innerWidth);
  const innerHeight = Number(window.innerHeight);
  if (
    Number.isFinite(width) &&
    Number.isFinite(height) &&
    Number.isFinite(innerWidth) &&
    Number.isFinite(innerHeight) &&
    width === innerWidth &&
    height === innerHeight
  ) {
    patchNumber(screen, 'width', Math.max(innerWidth + 86, 1366));
    patchNumber(screen, 'height', Math.max(innerHeight + 48, 768));
  }
})();
(function(){
  const patchNumber = (target, key, value) => {
    try {
      Object.defineProperty(target, key, {
        get: () => value,
        configurable: true,
      });
    } catch {}
  };
  const availWidth = Number(screen.availWidth);
  const availHeight = Number(screen.availHeight);
  const width = Number(screen.width);
  const height = Number(screen.height);
  if (Number.isFinite(width) && Number.isFinite(availWidth) && availWidth >= width) {
    patchNumber(screen, 'availWidth', Math.max(width - 8, 0));
  }
  if (Number.isFinite(height) && Number.isFinite(availHeight) && availHeight >= height) {
    patchNumber(screen, 'availHeight', Math.max(height - 40, 0));
  }
  if (Number.isFinite(screen.availLeft) && screen.availLeft === 0) {
    patchNumber(screen, 'availLeft', 0);
  }
  if (Number.isFinite(screen.availTop) && screen.availTop === 0) {
    patchNumber(screen, 'availTop', 24);
  }
})();
(function(){
  if (navigator.hardwareConcurrency < 4) {
    Object.defineProperty(navigator, 'hardwareConcurrency', {
      get: () => 4,
      configurable: true,
    });
  }
})();
(function(){
  if (typeof Notification === 'undefined') return;
  const current = Notification.permission;
  if (current === 'granted') return;
  try {
    Object.defineProperty(Notification, 'permission', {
      get: () => 'default',
      configurable: true,
    });
  } catch {}
})();
(function(){
  if (typeof Element === 'undefined' || !Element.prototype) return;
  const nativeSetAttribute = Element.prototype.setAttribute;
  if (typeof nativeSetAttribute !== 'function') return;
  const normalize = (value) => String(value).replace(/\s+/g, ' ').trim().toLowerCase();
  const probeStyle = 'background-color: activetext';
  const replacement = 'background-color: rgb(0, 0, 0)';
  const patchedSetAttribute = function(name, value) {
    if (String(name).toLowerCase() === 'style' && normalize(value) === probeStyle) {
      return nativeSetAttribute.call(this, name, replacement);
    }
    return nativeSetAttribute.call(this, name, value);
  };
  try {
    Object.defineProperty(patchedSetAttribute, 'name', {
      value: 'setAttribute',
      configurable: true,
    });
    Object.defineProperty(patchedSetAttribute, 'toString', {
      value: () => nativeSetAttribute.toString(),
      configurable: true,
    });
  } catch {}
  try {
    Object.defineProperty(Element.prototype, 'setAttribute', {
      value: patchedSetAttribute,
      configurable: true,
      writable: true,
    });
  } catch {
    try { Element.prototype.setAttribute = patchedSetAttribute; } catch {}
  }
})();
(function(){
  if (!navigator.connection) return;
  const conn = navigator.connection;
  if (typeof conn.downlinkMax === 'number') return;
  const defineDownlinkMax = (target) => {
    if (!target) return false;
    try {
      Object.defineProperty(target, 'downlinkMax', {
        get: () => 10,
        configurable: true,
      });
      return true;
    } catch {
      return false;
    }
  };
  try {
    const proto = Object.getPrototypeOf(conn);
    if (defineDownlinkMax(proto)) {
      try { delete conn.downlinkMax; } catch {}
      return;
    }
  } catch {}
  defineDownlinkMax(conn);
})();
(function(){
  if (typeof Worker !== 'function') return;
  const isCloudflareChallengeRuntime = (() => {
    try {
      const host = String(location.hostname || '').toLowerCase();
      const path = String(location.pathname || '');
      if (host === 'challenges.cloudflare.com') return true;
      return /\/cdn-cgi\/challenge-platform\//.test(path);
    } catch {
      return false;
    }
  })();
  // Cloudflare challenge workers are sensitive to constructor wrapping.
  // Keep native Worker behavior in this runtime to avoid importScripts(blob) failures.
  if (isCloudflareChallengeRuntime) return;
  const NativeWorker = Worker;
  const workerPrelude = `
(() => {
  try {
    if (!navigator || !navigator.connection) return;
    const conn = navigator.connection;
    if (typeof conn.downlinkMax === 'number') return;
    const defineDownlinkMax = (target) => {
      if (!target) return false;
      try {
        Object.defineProperty(target, 'downlinkMax', {
          get: () => 10,
          configurable: true,
        });
        return true;
      } catch {
        return false;
      }
    };
    try {
      const proto = Object.getPrototypeOf(conn);
      if (defineDownlinkMax(proto)) {
        try { delete conn.downlinkMax; } catch {}
        return;
      }
    } catch {}
    defineDownlinkMax(conn);
  } catch {}
})();
`;
  const buildPatchedScript = (url, options) => {
    const scriptUrl = String(url);
    const isModule = options && options.type === 'module';
    const loader = isModule
      ? `import ${JSON.stringify(scriptUrl)};`
      : `importScripts(${JSON.stringify(scriptUrl)});`;
    return `${workerPrelude}\n${loader}`;
  };
  const resolveWorkerUrl = (value) => {
    try {
      return new URL(String(value), location.href);
    } catch {
      return null;
    }
  };
  const shouldPatchWorker = (value) => {
    const resolved = resolveWorkerUrl(value);
    if (!resolved) return false;
    if (resolved.protocol === 'blob:') return resolved.origin === location.origin;
    if (resolved.protocol === 'http:' || resolved.protocol === 'https:') {
      return resolved.origin === location.origin;
    }
    if (resolved.protocol === 'file:') return location.protocol === 'file:';
    return false;
  };
  const WrappedWorker = function(scriptURL, options) {
    if (!shouldPatchWorker(scriptURL)) {
      return new NativeWorker(scriptURL, options);
    }
    try {
      const source = buildPatchedScript(scriptURL, options);
      const blob = new Blob([source], { type: 'application/javascript' });
      const patchedUrl = URL.createObjectURL(blob);
      const worker = new NativeWorker(patchedUrl, options);
      try {
        setTimeout(() => URL.revokeObjectURL(patchedUrl), 0);
      } catch {}
      return worker;
    } catch {
      return new NativeWorker(scriptURL, options);
    }
  };
  WrappedWorker.prototype = NativeWorker.prototype;
  try {
    Object.setPrototypeOf(WrappedWorker, NativeWorker);
  } catch {}
  try {
    Object.defineProperty(WrappedWorker, 'name', { value: 'Worker', configurable: true });
  } catch {}
  try {
    Object.defineProperty(WrappedWorker, 'toString', {
      value: () => NativeWorker.toString(),
      configurable: true,
    });
  } catch {}
  try {
    Object.defineProperty(window, 'Worker', {
      value: WrappedWorker,
      configurable: true,
      writable: true,
    });
  } catch {}
})();
(function(){
  if (typeof navigator.share !== 'function') {
    try {
      Object.defineProperty(navigator, 'share', {
        value: async () => undefined,
        configurable: true,
      });
    } catch {}
  }
  if (typeof navigator.canShare !== 'function') {
    try {
      Object.defineProperty(navigator, 'canShare', {
        value: () => true,
        configurable: true,
      });
    } catch {}
  }
})();
(function(){
  const ContactsCtor = typeof ContactsManager === 'function'
    ? ContactsManager
    : function ContactsManager() {};
  try {
    Object.defineProperty(window, 'ContactsManager', {
      value: ContactsCtor,
      configurable: true,
    });
  } catch {}
  const manager = Object.create(ContactsCtor.prototype || Object.prototype);
  if (typeof manager.select !== 'function') {
    manager.select = async () => [];
  }
  if (typeof manager.getProperties !== 'function') {
    manager.getProperties = () => ['name', 'email', 'tel', 'address', 'icon'];
  }
  const defineContacts = (target) => {
    if (!target) return false;
    try {
      Object.defineProperty(target, 'contacts', {
        get: () => manager,
        configurable: true,
      });
      return true;
    } catch {
      return false;
    }
  };
  if (defineContacts(navigator)) return;
  try {
    defineContacts(Object.getPrototypeOf(navigator));
  } catch {}
})();
(function(){
  const ContentIndexCtor = typeof ContentIndex === 'function'
    ? ContentIndex
    : function ContentIndex() {};
  try {
    Object.defineProperty(window, 'ContentIndex', {
      value: ContentIndexCtor,
      configurable: true,
    });
  } catch {}
  const index = Object.create(ContentIndexCtor.prototype || Object.prototype);
  if (typeof index.add !== 'function') {
    index.add = async () => undefined;
  }
  if (typeof index.delete !== 'function') {
    index.delete = async () => undefined;
  }
  if (typeof index.getAll !== 'function') {
    index.getAll = async () => [];
  }
  if (typeof ServiceWorkerRegistration === 'undefined') return;
  const defineIndex = (key) => {
    try {
      Object.defineProperty(ServiceWorkerRegistration.prototype, key, {
        get: () => index,
        configurable: true,
      });
      return true;
    } catch {
      return false;
    }
  };
  if (!('contentIndex' in ServiceWorkerRegistration.prototype)) {
    defineIndex('contentIndex');
  }
  if (!('index' in ServiceWorkerRegistration.prototype)) {
    defineIndex('index');
  }
})();
(function(){
  if (typeof window.matchMedia !== 'function') return;
  const nativeMatchMedia = window.matchMedia.bind(window);
  const normalize = (query) => String(query).replace(/\s+/g, ' ').trim().toLowerCase();
  const prefersLight = '(prefers-color-scheme: light)';
  const patchMediaQueryList = (mql) => {
    if (!mql || typeof mql !== 'object') return mql;
    return new Proxy(mql, {
      get(target, prop) {
        if (prop === 'matches') return false;
        const value = Reflect.get(target, prop, target);
        if (typeof value === 'function') {
          return value.bind(target);
        }
        return value;
      },
    });
  };
  const patchedMatchMedia = function(query) {
    const mql = nativeMatchMedia(query);
    if (normalize(query) === prefersLight) {
      return patchMediaQueryList(mql);
    }
    return mql;
  };
  try {
    Object.defineProperty(patchedMatchMedia, 'name', { value: 'matchMedia', configurable: true });
    Object.defineProperty(patchedMatchMedia, 'toString', {
      value: () => nativeMatchMedia.toString(),
      configurable: true,
    });
  } catch {}
  try {
    Object.defineProperty(window, 'matchMedia', {
      value: patchedMatchMedia,
      configurable: true,
      writable: true,
    });
  } catch {
    try { window.matchMedia = patchedMatchMedia; } catch {}
  }
})();
(function(){
  if (navigator.pdfViewerEnabled === true) return;
  try {
    Object.defineProperty(navigator, 'pdfViewerEnabled', {
      get: () => true,
      configurable: true,
    });
  } catch {}
})();
(function(){
  if (typeof HTMLMediaElement === 'undefined' || !HTMLMediaElement.prototype) return;
  const nativeCanPlayType = HTMLMediaElement.prototype.canPlayType;
  if (typeof nativeCanPlayType !== 'function') return;
  const parseInput = (value) => {
    const input = String(value || '').trim();
    const [mimePart, codecPart] = input.split(';');
    const mime = String(mimePart || '').trim().toLowerCase();
    const codecs = [];
    if (codecPart && codecPart.includes('codecs=')) {
      const normalized = codecPart
        .replace(/^[^=]*=/, '')
        .replace(/^\s*["']?/, '')
        .replace(/["']?\s*$/, '');
      normalized
        .split(',')
        .map((codec) => codec.trim().toLowerCase())
        .filter(Boolean)
        .forEach((codec) => codecs.push(codec));
    }
    return { mime, codecs };
  };
  const patchedCanPlayType = function(type) {
    const { mime, codecs } = parseInput(type);
    if (mime === 'video/mp4' && codecs.includes('avc1.42e01e')) {
      return 'probably';
    }
    if (mime === 'audio/x-m4a' && codecs.length === 0) {
      return 'maybe';
    }
    if (mime === 'audio/aac' && codecs.length === 0) {
      return 'probably';
    }
    return nativeCanPlayType.call(this, type);
  };
  try {
    Object.defineProperty(patchedCanPlayType, 'name', {
      value: 'canPlayType',
      configurable: true,
    });
    Object.defineProperty(patchedCanPlayType, 'toString', {
      value: () => nativeCanPlayType.toString(),
      configurable: true,
    });
  } catch {}
  try {
    Object.defineProperty(HTMLMediaElement.prototype, 'canPlayType', {
      value: patchedCanPlayType,
      configurable: true,
      writable: true,
    });
  } catch {
    try { HTMLMediaElement.prototype.canPlayType = patchedCanPlayType; } catch {}
  }
})();
(function(){
  if (!navigator.mediaDevices) return;
  const orig = navigator.mediaDevices.enumerateDevices;
  if (!orig) return;
  navigator.mediaDevices.enumerateDevices = async function() {
    const devices = await orig.call(navigator.mediaDevices);
    if (devices.length === 0) {
      return [
        { deviceId: 'default', kind: 'audioinput', label: '', groupId: 'default' },
        { deviceId: 'default', kind: 'videoinput', label: '', groupId: 'default' },
        { deviceId: 'default', kind: 'audiooutput', label: '', groupId: 'default' },
      ];
    }
    return devices;
  };
})();
(function(){
  const uaData = navigator.userAgentData;
  if (!uaData) return;
  const sanitizeBrand = (brand) => {
    if (typeof brand !== 'string') return brand;
    return brand.replace(/HeadlessChrome/gi, 'Google Chrome');
  };
  const patchBrandList = (value) => {
    if (!Array.isArray(value)) return value;
    return value.map((entry) => ({
      ...entry,
      brand: sanitizeBrand(entry.brand),
    }));
  };
  const patched = Object.create(Object.getPrototypeOf(uaData));
  Object.defineProperties(patched, {
    brands: {
      get: () => patchBrandList(uaData.brands),
      enumerable: true,
    },
    mobile: {
      get: () => uaData.mobile,
      enumerable: true,
    },
    platform: {
      get: () => uaData.platform,
      enumerable: true,
    },
  });
  patched.toJSON = () => ({
    brands: patchBrandList(uaData.brands),
    mobile: uaData.mobile,
    platform: uaData.platform,
  });
  patched.getHighEntropyValues = async (hints) => {
    const values = await uaData.getHighEntropyValues(hints);
    if (values && typeof values === 'object') {
      if ('brands' in values) values.brands = patchBrandList(values.brands);
      if ('fullVersionList' in values) {
        values.fullVersionList = patchBrandList(values.fullVersionList);
      }
    }
    return values;
  };
  try {
    Object.defineProperty(navigator, 'userAgentData', {
      get: () => patched,
      configurable: true,
    });
  } catch {}
})();
(function(){
  const ua = navigator.userAgent;
  if (ua.includes('HeadlessChrome')) {
    const patched = ua.replace(/HeadlessChrome/g, 'Chrome');
    Object.defineProperty(navigator, 'userAgent', {
      get: () => patched,
      configurable: true,
    });
    Object.defineProperty(navigator, 'appVersion', {
      get: () => patched.replace('Mozilla/', ''),
      configurable: true,
    });
  }
})();
(function(){
  if (!performance.memory) {
    Object.defineProperty(performance, 'memory', {
      get: () => ({
        jsHeapSizeLimit: 2172649472,
        totalJSHeapSize: 35839739,
        usedJSHeapSize: 22592767,
      }),
      configurable: true,
    });
  }
})();
(function(){
  if (document.documentElement) {
    const style = getComputedStyle(document.documentElement);
    const bg = style.backgroundColor;
    if (!bg || bg === 'rgba(0, 0, 0, 0)' || bg === 'transparent') {
      document.documentElement.style.backgroundColor = 'rgb(255, 255, 255)';
    }
  }
})();
