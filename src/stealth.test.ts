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
    }));

    expect(signals.mimeTypesLength).toBeGreaterThan(0);
    expect(signals.pdfViewerEnabled).toBe(true);
    expect(signals.hasShare).toBe(true);
    expect(signals.hasCanShare).toBe(true);
    expect(signals.hasConnectionDownlinkMax).toBe(true);
  });
});
