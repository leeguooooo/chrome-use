/**
 * Stealth mode patches to prevent browser automation detection.
 *
 * These scripts run via addInitScript (before any page JS) and patch the
 * fingerprinting surfaces that anti-bot systems use to identify Playwright /
 * Puppeteer / headless Chrome.
 */

import type { BrowserContext } from 'playwright-core';

export interface StealthScriptOptions {
  locale?: string;
}

/**
 * Chromium args that reduce automation fingerprint.
 * Intended to be merged into the user-supplied args array at launch time.
 */
export const STEALTH_CHROMIUM_ARGS: string[] = ['--disable-blink-features=AutomationControlled'];

/**
 * Apply all stealth patches to a BrowserContext.
 * Must be called BEFORE any page is created / navigated.
 */
export async function applyStealthScripts(
  context: BrowserContext,
  options: StealthScriptOptions = {}
): Promise<void> {
  await context.addInitScript({ content: buildStealthScript(options) });
}

function normalizeLocale(locale?: string): string | undefined {
  if (!locale) return undefined;
  const trimmed = locale.trim();
  if (!trimmed) return undefined;
  const cleaned = trimmed.split(',')[0]?.split(';')[0]?.replace(/_/g, '-');
  if (!cleaned) return undefined;
  try {
    return new Intl.Locale(cleaned).toString();
  } catch {
    return undefined;
  }
}

function deriveLanguages(locale?: string): string[] {
  const normalized = normalizeLocale(locale) ?? 'en-US';
  const base = normalized.split('-')[0];
  if (!base || base === normalized) return [normalized];
  return [normalized, base];
}

function buildStealthScript(options: StealthScriptOptions): string {
  const locale = normalizeLocale(options.locale) ?? 'en-US';
  const languages = deriveLanguages(locale);
  const configScript = `const __abStealth = ${JSON.stringify({ locale, languages })};`;

  // Each patch is an IIFE so variable scoping is clean
  return [
    configScript,
    patchNavigatorWebdriver(),
    patchChromeRuntime(),
    patchNavigatorLanguages(),
    patchNavigatorPluginsAndMimeTypes(),
    patchNavigatorPermissions(),
    patchWebGLVendor(),
    patchCdcProperties(),
    patchWindowDimensions(),
    patchScreenAvailability(),
    patchNavigatorHardwareConcurrency(),
    patchNavigatorConnection(),
    patchNavigatorShare(),
    patchNavigatorContacts(),
    patchPdfViewerEnabled(),
    patchMediaDevices(),
    patchUserAgentData(),
    patchUserAgent(),
    patchPerformanceMemory(),
  ].join('\n');
}

// ---------------------------------------------------------------------------
// Individual patches
// ---------------------------------------------------------------------------

/**
 * Remove navigator.webdriver entirely.
 * Modern detection checks both value and property presence (`'webdriver' in navigator`).
 */
function patchNavigatorWebdriver(): string {
  return `(function(){
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
})();`;
}

/**
 * Ensure window.chrome and window.chrome.runtime exist.
 * Headless Chrome (and Playwright) omit chrome.runtime which is a dead giveaway.
 */
function patchChromeRuntime(): string {
  return `(function(){
  if (!window.chrome) { window.chrome = {}; }
  if (!window.chrome.runtime) {
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
    Object.defineProperty(window.chrome, 'runtime', {
      value: runtime,
      configurable: true,
    });
  }
})();`;
}

/**
 * Keep navigator.language + navigator.languages aligned with launch locale.
 */
function patchNavigatorLanguages(): string {
  return `(function(){
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
})();`;
}

/**
 * Inject realistic navigator.plugins and navigator.mimeTypes arrays.
 * Headless Chrome reports an empty PluginArray; real Chrome always has a few.
 */
function patchNavigatorPluginsAndMimeTypes(): string {
  return `(function(){
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
})();`;
}

/**
 * navigator.permissions.query({name:'notifications'}) should resolve to
 * 'denied' in a normal browser, but Playwright throws or returns 'prompt'.
 */
function patchNavigatorPermissions(): string {
  return `(function(){
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
})();`;
}

/**
 * WebGL vendor/renderer: headless Chrome uses SwiftShader which is distinctive.
 * Patch getParameter to return Intel GPU strings when SwiftShader is detected.
 */
function patchWebGLVendor(): string {
  return `(function(){
  const getCtx = HTMLCanvasElement.prototype.getContext;
  HTMLCanvasElement.prototype.getContext = function(type, attrs) {
    const ctx = getCtx.call(this, type, attrs);
    if (ctx && (type === 'webgl' || type === 'webgl2' || type === 'experimental-webgl')) {
      const origGetParameter = ctx.getParameter.bind(ctx);
      ctx.getParameter = function(param) {
        const ext = ctx.getExtension('WEBGL_debug_renderer_info');
        if (ext) {
          if (param === ext.UNMASKED_VENDOR_WEBGL) {
            const real = origGetParameter(param);
            return (real && real.includes('SwiftShader')) ? 'Intel Inc.' : real;
          }
          if (param === ext.UNMASKED_RENDERER_WEBGL) {
            const real = origGetParameter(param);
            return (real && real.includes('SwiftShader')) ? 'Intel Iris OpenGL Engine' : real;
          }
        }
        return origGetParameter(param);
      };
    }
    return ctx;
  };
})();`;
}

/**
 * Remove Playwright's injected cdc_ (Chrome DevTools) properties on document.
 * Some older detection scripts look for these on the document element.
 */
function patchCdcProperties(): string {
  return `(function(){
  const clean = (target) => {
    for (const key of Object.keys(target)) {
      if (/^cdc_|^\\$cdc_/.test(key)) {
        delete target[key];
      }
    }
  };
  clean(document);
  if (document.documentElement) clean(document.documentElement);
})();`;
}

/**
 * contentWindow on cross-origin iframes: Playwright sometimes returns null
 * where real browsers return a (restricted) Window object.
 */
function patchWindowDimensions(): string {
  return `(function(){
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
})();`;
}

/**
 * Make Screen avail* values look like a desktop with taskbar/menu bar reserved space.
 */
function patchScreenAvailability(): string {
  return `(function(){
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
})();`;
}

/**
 * navigator.hardwareConcurrency: headless often reports 2 (CI);
 * real desktops typically have >= 4 cores.
 */
function patchNavigatorHardwareConcurrency(): string {
  return `(function(){
  if (navigator.hardwareConcurrency < 4) {
    Object.defineProperty(navigator, 'hardwareConcurrency', {
      get: () => 4,
      configurable: true,
    });
  }
})();`;
}

/**
 * Add missing connection.downlinkMax in Chromium headless environments.
 */
function patchNavigatorConnection(): string {
  return `(function(){
  if (!navigator.connection) return;
  const conn = navigator.connection;
  if (typeof conn.downlinkMax === 'number') return;
  try {
    Object.defineProperty(conn, 'downlinkMax', {
      get: () => 10,
      configurable: true,
    });
  } catch {}
})();`;
}

/**
 * Add share/canShare APIs expected on modern Chromium desktop.
 */
function patchNavigatorShare(): string {
  return `(function(){
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
})();`;
}

/**
 * Add Contacts Manager stub to avoid "missing contacts manager" signals.
 */
function patchNavigatorContacts(): string {
  return `(function(){
  if (navigator.contacts) return;
  try {
    Object.defineProperty(navigator, 'contacts', {
      value: {
        select: async () => [],
        getProperties: () => ['name', 'email', 'tel', 'address', 'icon'],
      },
      configurable: true,
    });
  } catch {}
})();`;
}

/**
 * Chromium exposes navigator.pdfViewerEnabled=true in normal browsing mode.
 */
function patchPdfViewerEnabled(): string {
  return `(function(){
  if (navigator.pdfViewerEnabled === true) return;
  try {
    Object.defineProperty(navigator, 'pdfViewerEnabled', {
      get: () => true,
      configurable: true,
    });
  } catch {}
})();`;
}

/**
 * navigator.mediaDevices.enumerateDevices should return at least some devices
 * instead of an empty array (headless default).
 */
function patchMediaDevices(): string {
  return `(function(){
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
})();`;
}

/**
 * Replace "HeadlessChrome" with "Chrome" in navigator.userAgent so
 * UA-based detection is bypassed at the JavaScript level.
 */
function patchUserAgent(): string {
  return `(function(){
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
})();`;
}

/**
 * Ensure userAgentData does not expose "HeadlessChrome" brand tokens.
 */
function patchUserAgentData(): string {
  return `(function(){
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
})();`;
}

/**
 * Provide a fake performance.memory (Chrome-only, non-standard).
 * Headless Chrome omits this; some detectors check for its presence.
 */
function patchPerformanceMemory(): string {
  return `(function(){
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
})();`;
}
