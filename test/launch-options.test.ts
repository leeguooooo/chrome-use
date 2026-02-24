import { describe, it, expect, afterEach } from 'vitest';
import { BrowserManager } from '../src/browser.js';

describe('Launch Options', () => {
  let browser: BrowserManager;

  afterEach(async () => {
    if (browser?.isLaunched()) {
      await browser.close();
    }
  });

  describe('browser args', () => {
    it('should keep webdriver undefined with custom args under stealth defaults', async () => {
      browser = new BrowserManager();
      await browser.launch({
        headless: true,
        args: ['--disable-blink-features=AutomationControlled'],
      });

      const page = browser.getPage();
      await page.goto('about:blank');

      // Under stealth defaults, webdriver is hidden (undefined)
      const webdriver = await page.evaluate(() => navigator.webdriver);
      expect(webdriver).toBeUndefined();
    });

    it('should launch with multiple args', async () => {
      browser = new BrowserManager();
      await browser.launch({
        headless: true,
        args: [
          '--disable-blink-features=AutomationControlled',
          '--disable-dev-shm-usage',
        ],
      });

      expect(browser.isLaunched()).toBe(true);
    });

    it('should launch without args and keep webdriver hidden by default', async () => {
      browser = new BrowserManager();
      await browser.launch({
        headless: true,
      });

      const page = browser.getPage();
      await page.goto('about:blank');

      // Stealth default behavior - webdriver is hidden
      const webdriver = await page.evaluate(() => navigator.webdriver);
      expect(webdriver).toBeUndefined();
    });
  });

  describe('custom user-agent', () => {
    it('should launch with custom user-agent', async () => {
      const customUA = 'CustomTestBot/1.0';
      browser = new BrowserManager();
      await browser.launch({
        headless: true,
        userAgent: customUA,
      });

      const page = browser.getPage();
      await page.goto('about:blank');

      const ua = await page.evaluate(() => navigator.userAgent);
      expect(ua).toBe(customUA);
    });

    it('should use default user-agent when not specified', async () => {
      browser = new BrowserManager();
      await browser.launch({
        headless: true,
      });

      const page = browser.getPage();
      await page.goto('about:blank');

      const ua = await page.evaluate(() => navigator.userAgent);
      // Default UA should contain Chrome/Chromium
      expect(ua).toContain('Chrome');
    });
  });

  describe('proxy configuration', () => {
    it('should accept proxy configuration', async () => {
      browser = new BrowserManager();
      // Note: This test just verifies the proxy option is accepted without error
      // Actual proxy testing requires a running proxy server
      await browser.launch({
        headless: true,
        proxy: {
          server: 'http://localhost:8080',
        },
      });

      expect(browser.isLaunched()).toBe(true);
    });

    it('should accept proxy with bypass list', async () => {
      browser = new BrowserManager();
      await browser.launch({
        headless: true,
        proxy: {
          server: 'http://localhost:8080',
          bypass: 'localhost,*.internal.com',
        },
      });

      expect(browser.isLaunched()).toBe(true);
    });

    it('should fail connection when proxy is unreachable (proves proxy is being used)', async () => {
      browser = new BrowserManager();
      await browser.launch({
        headless: true,
        proxy: {
          server: 'http://127.0.0.1:59999', // Non-existent proxy
        },
      });

      const page = browser.getPage();
      // Navigation should fail because proxy is unreachable
      // This proves the proxy setting is actually being used
      await expect(page.goto('https://example.com', { timeout: 5000 })).rejects.toThrow();
    });
  });

  describe('combined options', () => {
    it('should launch with args, user-agent, and proxy combined', async () => {
      const customUA = 'CombinedTestBot/2.0';
      browser = new BrowserManager();
      await browser.launch({
        headless: true,
        args: ['--disable-blink-features=AutomationControlled'],
        userAgent: customUA,
        proxy: {
          server: 'http://localhost:8080',
          bypass: 'localhost',
        },
      });

      const page = browser.getPage();
      await page.goto('about:blank');

      // Verify user-agent
      const ua = await page.evaluate(() => navigator.userAgent);
      expect(ua).toBe(customUA);

      // Verify webdriver is hidden
      const webdriver = await page.evaluate(() => navigator.webdriver);
      expect(webdriver).toBeUndefined();
    });
  });
});
