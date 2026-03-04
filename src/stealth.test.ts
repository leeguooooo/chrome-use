import { afterEach, describe, expect, it } from 'vitest';
import { BrowserManager } from './browser.js';

async function readWebdriverSignals(browser: BrowserManager): Promise<{
  value: boolean | undefined;
  inNavigator: boolean;
  ownNavigator: boolean;
  ownPrototype: boolean;
}> {
  const page = browser.getPage();
  await page.goto('about:blank');
  return page.evaluate(() => {
    const prototype = Object.getPrototypeOf(navigator);
    return {
      value: navigator.webdriver,
      inNavigator: 'webdriver' in navigator,
      ownNavigator: Object.prototype.hasOwnProperty.call(navigator, 'webdriver'),
      ownPrototype: Object.prototype.hasOwnProperty.call(prototype, 'webdriver'),
    };
  });
}

describe('Stealth mode', () => {
  let browser: BrowserManager;

  afterEach(async () => {
    if (browser?.isLaunched()) {
      await browser.close();
    }
  });

  it('removes navigator.webdriver when stealth is enabled', async () => {
    browser = new BrowserManager();
    await browser.launch({ headless: true, stealth: true });

    const signals = await readWebdriverSignals(browser);
    expect(signals.value).toBeUndefined();
    expect(signals.inNavigator).toBe(false);
    expect(signals.ownNavigator).toBe(false);
    expect(signals.ownPrototype).toBe(false);
  });

  it('applies stealth patches to contexts created by newWindow', async () => {
    browser = new BrowserManager();
    await browser.launch({ headless: true, stealth: true });
    await browser.newWindow();

    const signals = await readWebdriverSignals(browser);
    expect(signals.value).toBeUndefined();
    expect(signals.inNavigator).toBe(false);
    expect(signals.ownNavigator).toBe(false);
    expect(signals.ownPrototype).toBe(false);
  });

  it('aligns navigator language with AGENT_BROWSER_LOCALE', async () => {
    const previousLocale = process.env.AGENT_BROWSER_LOCALE;
    process.env.AGENT_BROWSER_LOCALE = 'fr-FR';

    try {
      browser = new BrowserManager();
      await browser.launch({ headless: true, stealth: true });

      const languageSignals = await browser.getPage().evaluate(() => ({
        language: navigator.language,
        languages: navigator.languages,
      }));

      expect(languageSignals.language).toBe('fr-FR');
      expect(languageSignals.languages).toEqual(['fr-FR', 'fr']);
    } finally {
      if (previousLocale === undefined) {
        delete process.env.AGENT_BROWSER_LOCALE;
      } else {
        process.env.AGENT_BROWSER_LOCALE = previousLocale;
      }
    }
  });

  it('keeps navigator.vendor aligned with Chrome', async () => {
    browser = new BrowserManager();
    await browser.launch({ headless: true, stealth: true });

    const vendorSignals = await browser.getPage().evaluate(() => ({
      userAgent: navigator.userAgent,
      vendor: navigator.vendor,
    }));

    if (vendorSignals.userAgent.includes('Chrome/')) {
      expect(vendorSignals.vendor).toBe('Google Inc.');
    }
  });

  it('keeps worker and page userAgent free of HeadlessChrome tokens', async () => {
    browser = new BrowserManager();
    await browser.launch({ headless: true, stealth: true });

    const userAgentSignals = await browser.getPage().evaluate(async () => {
      const pageUA = navigator.userAgent;
      const workerUA = await new Promise<string>((resolve) => {
        const source = 'postMessage(navigator.userAgent);';
        const blob = new Blob([source], { type: 'application/javascript' });
        const worker = new Worker(URL.createObjectURL(blob));
        worker.onmessage = (event) => resolve(String(event.data));
      });
      return { pageUA, workerUA };
    });

    expect(userAgentSignals.pageUA).not.toContain('HeadlessChrome');
    expect(userAgentSignals.workerUA).not.toContain('HeadlessChrome');
  });

  it('neutralizes the css webdriver heuristic probe', async () => {
    browser = new BrowserManager();
    await browser.launch({ headless: true, stealth: true });

    const signals = await browser.getPage().evaluate(() => ({
      probe: CSS.supports('border-end-end-radius: initial'),
      baseline: CSS.supports('display: block'),
      webdriver: navigator.webdriver,
      inNavigator: 'webdriver' in navigator,
    }));

    expect(signals.probe).toBe(false);
    expect(signals.baseline).toBe(true);
    expect(signals.webdriver).toBeUndefined();
    expect(signals.inNavigator).toBe(false);
  });

  it('neutralizes creepjs prefers-color-scheme light probe', async () => {
    browser = new BrowserManager();
    await browser.launch({ headless: true, stealth: true });

    const signals = await browser.getPage().evaluate(() => {
      const node = document.createElement('div');
      node.setAttribute('style', 'background-color: ActiveText');
      document.body.appendChild(node);
      const activeTextColor = getComputedStyle(node).backgroundColor;
      node.remove();

      return {
        activeTextColor,
        prefersLight: matchMedia('(prefers-color-scheme: light)').matches,
        prefersDark: matchMedia('(prefers-color-scheme: dark)').matches,
        lightListenerCalls: (() => {
          try {
            const mql = matchMedia('(prefers-color-scheme: light)');
            const handler = () => {};
            mql.addEventListener('change', handler);
            mql.removeEventListener('change', handler);
            return 'ok';
          } catch (error) {
            return String(error);
          }
        })(),
      };
    });

    expect(signals.activeTextColor).not.toBe('rgb(255, 0, 0)');
    expect(signals.prefersLight).toBe(false);
    expect(typeof signals.prefersDark).toBe('boolean');
    expect(signals.lightListenerCalls).toBe('ok');
  });

  it('exposes realistic mimeTypes/pdf/share signals', async () => {
    browser = new BrowserManager();
    await browser.launch({ headless: true, stealth: true });

    const signals = await browser.getPage().evaluate(() => ({
      mimeTypesLength: navigator.mimeTypes ? navigator.mimeTypes.length : 0,
      pdfViewerEnabled: navigator.pdfViewerEnabled,
      hasShare: typeof navigator.share === 'function',
      hasCanShare: typeof navigator.canShare === 'function',
      hasConnectionDownlinkMax:
        !!navigator.connection && typeof navigator.connection.downlinkMax === 'number',
      hasConnectionDownlinkMaxOnProto:
        !!navigator.connection && 'downlinkMax' in Object.getPrototypeOf(navigator.connection),
    }));

    expect(signals.mimeTypesLength).toBeGreaterThan(0);
    expect(signals.pdfViewerEnabled).toBe(true);
    expect(signals.hasShare).toBe(true);
    expect(signals.hasCanShare).toBe(true);
    expect(signals.hasConnectionDownlinkMax).toBe(true);
    expect(signals.hasConnectionDownlinkMaxOnProto).toBe(true);
  });

  it('exposes legacy chrome.app/csi/loadTimes APIs', async () => {
    browser = new BrowserManager();
    await browser.launch({ headless: true, stealth: true });

    const signals = await browser.getPage().evaluate(() => {
      const chromeObj = (window as any).chrome;
      const csi = chromeObj && typeof chromeObj.csi === 'function' ? chromeObj.csi() : null;
      const loadTimes =
        chromeObj && typeof chromeObj.loadTimes === 'function' ? chromeObj.loadTimes() : null;
      return {
        hasChrome: !!chromeObj,
        hasApp: !!(chromeObj && chromeObj.app),
        appInstalled: chromeObj?.app?.isInstalled,
        appRunningState: chromeObj?.app?.runningState?.(),
        hasCsi: typeof chromeObj?.csi === 'function',
        hasLoadTimes: typeof chromeObj?.loadTimes === 'function',
        csiHasOnloadT: csi && typeof csi.onloadT === 'number',
        csiHasPageT: csi && typeof csi.pageT === 'number',
        loadTimesHasRequestTime: loadTimes && typeof loadTimes.requestTime === 'number',
        loadTimesHasConnectionInfo: loadTimes && typeof loadTimes.connectionInfo === 'string',
      };
    });

    expect(signals.hasChrome).toBe(true);
    expect(signals.hasApp).toBe(true);
    expect(signals.appInstalled).toBe(false);
    expect(signals.appRunningState).toBe('cannot_run');
    expect(signals.hasCsi).toBe(true);
    expect(signals.hasLoadTimes).toBe(true);
    expect(signals.csiHasOnloadT).toBe(true);
    expect(signals.csiHasPageT).toBe(true);
    expect(signals.loadTimesHasRequestTime).toBe(true);
    expect(signals.loadTimesHasConnectionInfo).toBe(true);
  });

  it('spoofs high-signal media codec probes', async () => {
    browser = new BrowserManager();
    await browser.launch({ headless: true, stealth: true });

    const codecs = await browser.getPage().evaluate(() => {
      const video = document.createElement('video');
      const audio = document.createElement('audio');
      return {
        mp4Avc: video.canPlayType('video/mp4; codecs="avc1.42E01E"'),
        xM4a: audio.canPlayType('audio/x-m4a;'),
        aac: audio.canPlayType('audio/aac'),
      };
    });

    expect(codecs.mp4Avc).toBe('probably');
    expect(codecs.xM4a).toBe('maybe');
    expect(codecs.aac).toBe('probably');
  });

  it('patches srcdoc iframe.contentWindow probes', async () => {
    browser = new BrowserManager();
    await browser.launch({ headless: true, stealth: true });

    const iframeSignals = await browser.getPage().evaluate(() => {
      const iframe = document.createElement('iframe');
      iframe.srcdoc = '<!doctype html><html><body>ok</body></html>';
      const win = iframe.contentWindow;
      return {
        hasContentWindow: !!win,
        selfEqualsWindow: win ? win.self === win : false,
        selfEqualsTop: win ? win.self === window.top : null,
        frameElementMatches: win ? win.frameElement === iframe : false,
        zeroSlotType: typeof (win as any)?.[0],
      };
    });

    expect(iframeSignals.hasContentWindow).toBe(true);
    expect(iframeSignals.selfEqualsWindow).toBe(true);
    expect(iframeSignals.selfEqualsTop).toBe(false);
    expect(iframeSignals.frameElementMatches).toBe(true);
    expect(iframeSignals.zeroSlotType).toBe('undefined');
  });

  it('sanitizes Playwright sourceURL markers in error stacks', async () => {
    browser = new BrowserManager();
    await browser.launch({ headless: true, stealth: true });

    const stacks = await browser.getPage().evaluate(() => {
      const explicitEvalStack = eval(
        `(() => { try { throw new Error('explicit'); } catch (error) { return String(error.stack || ''); } })()\n//# sourceURL=__playwright_evaluation_script__`
      );
      let directStack = '';
      try {
        throw new Error('direct');
      } catch (error) {
        directStack = String((error as Error).stack || '');
      }
      return { explicitEvalStack, directStack };
    });

    expect(stacks.explicitEvalStack).not.toContain('__playwright_evaluation_script__');
    expect(stacks.explicitEvalStack).not.toContain('__puppeteer_evaluation_script__');
    expect(stacks.explicitEvalStack).not.toContain('sourceURL=');
    expect(stacks.directStack).not.toContain('__playwright_evaluation_script__');
    expect(stacks.directStack).not.toContain('__puppeteer_evaluation_script__');
  });

  it('sanitizes sourceURL markers in direct CDP Runtime.evaluate payloads', async () => {
    browser = new BrowserManager();
    await browser.launch({ headless: true, stealth: true });

    const cdp = await browser.getCDPSession();
    const response = await cdp.send('Runtime.evaluate', {
      expression:
        "(() => { throw new Error('cdp'); })()\\n//# sourceURL=__playwright_evaluation_script__",
      returnByValue: true,
    });
    const raw = JSON.stringify(response);
    expect(raw).not.toContain('__playwright_evaluation_script__');
    expect(raw).not.toContain('__puppeteer_evaluation_script__');
    expect(raw).not.toContain('sourceURL=');
  });

  it('doctor reports CDP sourceURL probe as pass in launched chromium sessions', async () => {
    browser = new BrowserManager();
    await browser.launch({ headless: true, stealth: true });

    const report = await browser.runDoctor();
    const check = report.checks.find((entry) => entry.name === 'cdp:sourceurl-sanitized');

    expect(check).toBeDefined();
    expect(check?.status).toBe('pass');
  });

  it('doctor marks plugin handshake context as skip outside CDP mode', async () => {
    browser = new BrowserManager();
    await browser.launch({ headless: true, stealth: true });

    const report = await browser.runDoctor();
    const check = report.checks.find((entry) => entry.name === 'plugin:handshake-context');

    expect(check).toBeDefined();
    expect(check?.status).toBe('skip');
    expect(check?.message).toContain('only applies to CDP');
  });

  it('exposes contacts manager and content index APIs', async () => {
    browser = new BrowserManager();
    await browser.launch({ headless: true, stealth: true });

    const signals = await browser.getPage().evaluate(() => ({
      hasContacts: 'contacts' in navigator,
      contactsManagerCtor: typeof (window as any).ContactsManager === 'function',
      hasContentIndexCtor: typeof (window as any).ContentIndex === 'function',
      hasServiceWorkerRegistration: typeof ServiceWorkerRegistration !== 'undefined',
      hasContentIndexOnSWR:
        typeof ServiceWorkerRegistration !== 'undefined' &&
        ('contentIndex' in ServiceWorkerRegistration.prototype ||
          'index' in ServiceWorkerRegistration.prototype),
      notificationPermission: typeof Notification !== 'undefined' ? Notification.permission : null,
      screenMatchesViewport:
        screen.width === window.innerWidth && screen.height === window.innerHeight,
    }));

    expect(signals.hasContacts).toBe(true);
    expect(signals.contactsManagerCtor).toBe(true);
    expect(signals.hasContentIndexCtor).toBe(true);
    if (signals.hasServiceWorkerRegistration) {
      expect(signals.hasContentIndexOnSWR).toBe(true);
    }
    expect(signals.notificationPermission).toBe('default');
    expect(signals.screenMatchesViewport).toBe(false);
  });

  it('exposes downlinkMax inside dedicated workers', async () => {
    browser = new BrowserManager();
    await browser.launch({ headless: true, stealth: true });

    const workerSignals = await browser.getPage().evaluate(async () => {
      return new Promise<{
        hasConnection: boolean;
        hasDownlinkMax: boolean;
        hasDownlinkMaxOnProto: boolean;
        downlinkMax: unknown;
      }>((resolve) => {
        const source =
          "postMessage({hasConnection: !!navigator.connection, hasDownlinkMax: navigator.connection ? ('downlinkMax' in navigator.connection) : false, hasDownlinkMaxOnProto: navigator.connection ? ('downlinkMax' in Object.getPrototypeOf(navigator.connection)) : false, downlinkMax: navigator.connection && navigator.connection.downlinkMax});";
        const blob = new Blob([source], { type: 'application/javascript' });
        const worker = new Worker(URL.createObjectURL(blob));
        worker.onmessage = (event) => resolve(event.data);
      });
    });

    expect(workerSignals.hasConnection).toBe(true);
    expect(workerSignals.hasDownlinkMax).toBe(true);
    expect(workerSignals.hasDownlinkMaxOnProto).toBe(true);
    expect(typeof workerSignals.downlinkMax).toBe('number');
  });

  it('skips worker wrapping for cross-origin blob URLs', async () => {
    browser = new BrowserManager();
    await browser.launch({ headless: true, stealth: true });

    const signals = await browser.getPage().evaluate(() => {
      const nativeCreateObjectURL = URL.createObjectURL;
      const nativeRevokeObjectURL = URL.revokeObjectURL;
      let createCalls = 0;
      let revokeCalls = 0;

      (URL as any).createObjectURL = (...args: unknown[]) => {
        createCalls += 1;
        return nativeCreateObjectURL.apply(URL, args as [Blob | MediaSource]);
      };
      (URL as any).revokeObjectURL = (...args: unknown[]) => {
        revokeCalls += 1;
        return nativeRevokeObjectURL.apply(URL, args as [string]);
      };

      try {
        new Worker('blob:https://challenges.cloudflare.com/11111111-1111-1111-1111-111111111111');
      } catch {}

      (URL as any).createObjectURL = nativeCreateObjectURL;
      (URL as any).revokeObjectURL = nativeRevokeObjectURL;
      return { createCalls, revokeCalls };
    });

    expect(signals.createCalls).toBe(0);
    expect(signals.revokeCalls).toBe(0);
  });
});
