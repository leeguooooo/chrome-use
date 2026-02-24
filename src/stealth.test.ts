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
});
