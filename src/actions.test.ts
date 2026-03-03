import { describe, it, expect, vi } from 'vitest';
import { detectRiskSignals, executeCommand, toAIFriendlyError } from './actions.js';

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

  it('should detect cloudflare security verification text', () => {
    const signals = detectRiskSignals(
      'https://dash.cloudflare.com/zone/abc/ssl-tls/acm',
      'dash.cloudflare.com',
      'Performing security verification Verifying... This website uses a security service to protect against malicious bots.'
    );
    expect(signals.some((s) => s.code === 'verification_interstitial')).toBe(true);
    expect(signals.some((s) => s.code === 'bot_challenge')).toBe(true);
  });
});

describe('tab grouping fallback', () => {
  it('should keep navigate successful when tab grouping trigger throws', async () => {
    const page = {
      waitForTimeout: vi.fn().mockResolvedValue(undefined),
      goto: vi.fn().mockResolvedValue(undefined),
      url: vi.fn().mockReturnValue('https://example.com/'),
      title: vi.fn().mockResolvedValue('Example Domain'),
    };

    const browser = {
      getPage: vi.fn().mockReturnValue(page),
      setTargetUrl: vi.fn().mockResolvedValue(undefined),
      triggerTabGroupingForActivePage: vi.fn().mockImplementation(() => {
        throw new Error('plugin-unavailable');
      }),
    };

    const response = await executeCommand(
      { id: 'n1', action: 'navigate', url: 'https://example.com', riskMode: 'off' },
      browser as any
    );

    expect(response.success).toBe(true);
    expect(page.goto).toHaveBeenCalledTimes(1);
  });

  it('should keep tab_new successful when tab grouping trigger throws after navigation', async () => {
    const page = {
      goto: vi.fn().mockResolvedValue(undefined),
    };

    const browser = {
      newTab: vi.fn().mockResolvedValue({ index: 1, total: 2 }),
      getPage: vi.fn().mockReturnValue(page),
      triggerTabGroupingForActivePage: vi.fn().mockImplementation(() => {
        throw new Error('plugin-unavailable');
      }),
    };

    const response = await executeCommand(
      { id: 't1', action: 'tab_new', url: 'https://example.com' },
      browser as any
    );

    expect(response.success).toBe(true);
    expect(browser.newTab).toHaveBeenCalledTimes(1);
    expect(page.goto).toHaveBeenCalledTimes(1);
  });
});

describe('risk interstitial recovery', () => {
  it('should wait for cloudflare-style challenge to clear before retrying navigation', async () => {
    const challengeClearMs = 10_000;
    let challengeElapsed = 0;
    let currentUrl = 'https://dash.cloudflare.com/challenge';
    let currentTitle = 'Just a moment...';

    const syncChallengeState = () => {
      if (challengeElapsed >= challengeClearMs) {
        currentUrl = 'https://dash.cloudflare.com/zone/abc/ssl-tls/acm';
        currentTitle = 'Cloudflare Dashboard';
      } else {
        currentUrl = 'https://dash.cloudflare.com/challenge';
        currentTitle = 'Just a moment...';
      }
    };

    const page = {
      waitForTimeout: vi.fn().mockImplementation(async (ms: number) => {
        challengeElapsed += Number(ms) || 0;
        syncChallengeState();
      }),
      goto: vi.fn().mockImplementation(async () => {
        // Refreshing during verification resets challenge progress.
        if (challengeElapsed < challengeClearMs) {
          challengeElapsed = 0;
        }
        syncChallengeState();
      }),
      url: vi.fn().mockImplementation(() => currentUrl),
      title: vi.fn().mockImplementation(async () => currentTitle),
      evaluate: vi.fn().mockImplementation(async () => {
        if (currentTitle === 'Just a moment...') {
          return 'Performing security verification Verifying... This website uses a security service to protect against malicious bots.';
        }
        return 'Dashboard content';
      }),
    };

    const browser = {
      getPage: vi.fn().mockReturnValue(page),
      setTargetUrl: vi.fn().mockResolvedValue(undefined),
      triggerTabGroupingForActivePage: vi.fn(),
    };

    const response = await executeCommand(
      {
        id: 'cf1',
        action: 'navigate',
        url: 'https://dash.cloudflare.com/zone/abc/ssl-tls/acm',
        riskMode: 'warn',
      },
      browser as any
    );

    expect(response.success).toBe(true);
    if (response.success) {
      expect(response.data.title).toBe('Cloudflare Dashboard');
      expect(response.data.warning).toContain('cleared after wait');
      expect(response.data.riskSignals?.length ?? 0).toBeGreaterThan(0);
    }
    expect(page.goto).toHaveBeenCalledTimes(1);
  });
});
