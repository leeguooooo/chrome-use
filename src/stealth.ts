/**
 * Stealth mode patches to prevent browser automation detection.
 *
 * These scripts run via addInitScript (before any page JS) and patch the
 * fingerprinting surfaces that anti-bot systems use to identify Playwright /
 * Puppeteer / headless Chrome.
 */

import type { BrowserContext } from 'playwright-core';

/**
 * Chromium args that reduce automation fingerprint.
 * Intended to be merged into the user-supplied args array at launch time.
 */
export const STEALTH_CHROMIUM_ARGS: string[] = ['--disable-blink-features=AutomationControlled'];

/**
 * Apply all stealth patches to a BrowserContext.
 * Must be called BEFORE any page is created / navigated.
 */
export async function applyStealthScripts(context: BrowserContext): Promise<void> {
  await context.addInitScript({ content: buildStealthScript() });
}

function buildStealthScript(): string {
  // Each patch is an IIFE so variable scoping is clean
  return [
    patchNavigatorWebdriver(),
    patchChromeRuntime(),
    patchNavigatorPlugins(),
    patchNavigatorPermissions(),
    patchWebGLVendor(),
    patchCdcProperties(),
    patchIframeContentWindow(),
    patchNavigatorHardwareConcurrency(),
    patchMediaDevices(),
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
    window.chrome.runtime = {
      connect: function(){},
      sendMessage: function(){},
    };
  }
})();`;
}

/**
 * Inject a realistic navigator.plugins array.
 * Headless Chrome reports an empty PluginArray; real Chrome always has a few.
 */
function patchNavigatorPlugins(): string {
  return `(function(){
  const makePlugin = (name, description, filename, mimeType) => {
    const mime = { type: mimeType, suffixes: '', description, enabledPlugin: null };
    const plugin = { name, description, filename, length: 1, 0: mime };
    mime.enabledPlugin = plugin;
    return plugin;
  };
  const plugins = [
    makePlugin('Chrome PDF Plugin', 'Portable Document Format', 'internal-pdf-viewer', 'application/x-google-chrome-pdf'),
    makePlugin('Chrome PDF Viewer', '', 'mhjfbmdgcfjbbpaeojofohoefgiehjai', 'application/pdf'),
    makePlugin('Native Client', '', 'internal-nacl-plugin', 'application/x-nacl'),
  ];
  const pluginArray = Object.create(PluginArray.prototype);
  plugins.forEach((p, i) => {
    Object.setPrototypeOf(p, Plugin.prototype);
    pluginArray[i] = p;
  });
  Object.defineProperty(pluginArray, 'length', { get: () => plugins.length });
  pluginArray.item = (i) => plugins[i] || null;
  pluginArray.namedItem = (name) => plugins.find(p => p.name === name) || null;
  pluginArray.refresh = () => {};
  pluginArray[Symbol.iterator] = function*() { for (const p of plugins) yield p; };
  Object.defineProperty(navigator, 'plugins', {
    get: () => pluginArray,
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
  if (!navigator.permissions) return;
  const origQuery = navigator.permissions.query.bind(navigator.permissions);
  navigator.permissions.query = (params) => {
    if (params.name === 'notifications') {
      return Promise.resolve({ state: Notification.permission, onchange: null });
    }
    return origQuery(params);
  };
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
function patchIframeContentWindow(): string {
  return `(function(){
  const orig = Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype, 'contentWindow');
  if (orig && orig.get) {
    Object.defineProperty(HTMLIFrameElement.prototype, 'contentWindow', {
      get: function() {
        const w = orig.get.call(this);
        if (w === null) {
          return window;
        }
        return w;
      },
      configurable: true,
    });
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
