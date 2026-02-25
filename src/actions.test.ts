import { describe, it, expect } from 'vitest';
import { detectRiskSignals, toAIFriendlyError } from './actions.js';

describe('toAIFriendlyError', () => {
  describe('element blocked by overlay', () => {
    it('should detect intercepts pointer events even when Timeout is in message', () => {
      // This is the exact error from Playwright when a cookie banner blocks an element
      // Bug: Previously this was incorrectly reported as "not found or not visible"
      const error = new Error(
        'TimeoutError: locator.click: Timeout 10000ms exceeded.\n' +
          'Call log:\n' +
          "  - waiting for getByRole('link', { name: 'Anmelden', exact: true }).first()\n" +
          '    - locator resolved to <a href="https://example.com/login">Anmelden</a>\n' +
          '  - attempting click action\n' +
          '    2 x waiting for element to be visible, enabled and stable\n' +
          '      - element is visible, enabled and stable\n' +
          '      - scrolling into view if needed\n' +
          '      - done scrolling\n' +
          '      - <body class="font-sans antialiased">...</body> intercepts pointer events\n' +
          '    - retrying click action'
      );

      const result = toAIFriendlyError(error, '@e4');

      // Must NOT say "not found" - the element WAS found
      expect(result.message).not.toContain('not found');
      // Must indicate the element is blocked
      expect(result.message).toContain('blocked by another element');
      expect(result.message).toContain('modal or overlay');
    });

    it('should suggest dismissing cookie banners', () => {
      const error = new Error('<div class="cookie-overlay"> intercepts pointer events');
      const result = toAIFriendlyError(error, '@e1');

      expect(result.message).toContain('cookie banners');
    });
  });
});

describe('detectRiskSignals', () => {
  it('should detect verification patterns from URL and title', () => {
    const signals = detectRiskSignals(
      'https://example.com/verify/captcha?scene=anti_bot',
      'Just a moment...'
    );
    expect(signals.length).toBeGreaterThan(0);
    expect(signals.some((s) => s.source === 'url' && s.code === 'captcha_interstitial')).toBe(true);
    expect(
      signals.some((s) => s.source === 'title' && s.code === 'verification_interstitial')
    ).toBe(true);
  });

  it('should return empty array for normal pages', () => {
    const signals = detectRiskSignals('https://example.com/dashboard', 'Dashboard');
    expect(signals).toEqual([]);
  });
});
