#!/usr/bin/env tsx

/**
 * Deterministic Turnstile check using Cloudflare official testing sitekey.
 *
 * Usage:
 *   pnpm run check:turnstile-testkey
 *   pnpm run check:turnstile-testkey -- --headed
 */

import http from 'node:http';
import { BrowserManager } from '../src/browser.js';

const args = process.argv.slice(2);
const headed = args.includes('--headed');
const waitMsRaw = args.includes('--wait-ms')
  ? args[args.indexOf('--wait-ms') + 1]
  : undefined;
const waitMs = Number.isFinite(Number(waitMsRaw)) ? Number(waitMsRaw) : 9000;

const html = `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>agent-browser turnstile testkey</title>
  </head>
  <body>
    <h1>Turnstile Testkey Probe</h1>
    <div class="cf-turnstile" data-sitekey="1x00000000000000000000AA" data-callback="onTurnstileToken"></div>
    <script>
      window.__turnstileToken = '';
      function onTurnstileToken(token) {
        window.__turnstileToken = token || '';
      }
    </script>
    <script src="https://challenges.cloudflare.com/turnstile/v0/api.js" async defer></script>
  </body>
</html>`;

function createServer(): Promise<http.Server> {
  const server = http.createServer((_, res) => {
    res.writeHead(200, {
      'Content-Type': 'text/html; charset=utf-8',
      'Cache-Control': 'no-store',
    });
    res.end(html);
  });

  return new Promise((resolve, reject) => {
    server.on('error', reject);
    server.listen(0, '127.0.0.1', () => resolve(server));
  });
}

function closeServer(server: http.Server): Promise<void> {
  return new Promise((resolve) => server.close(() => resolve()));
}

async function main(): Promise<void> {
  const server = await createServer();
  const address = server.address();
  if (!address || typeof address === 'string') {
    await closeServer(server);
    throw new Error('Unable to start local HTTP server for Turnstile probe');
  }

  const localUrl = `http://127.0.0.1:${address.port}/`;
  const browser = new BrowserManager();

  try {
    await browser.launch({
      id: 'turnstile-testkey',
      action: 'launch',
      browser: 'chromium',
      stealth: true,
      headless: !headed,
    });

    const page = browser.getPage();
    await page.goto(localUrl, { waitUntil: 'domcontentloaded', timeout: 45_000 });
    await page.waitForTimeout(waitMs);

    const result = await page.evaluate(() => {
      const hidden = document.querySelector('input[name="cf-turnstile-response"]') as
        | HTMLInputElement
        | null;
      const hiddenValue = hidden?.value || '';
      const callbackValue =
        typeof (window as any).__turnstileToken === 'string'
          ? (window as any).__turnstileToken
          : '';
      const token = hiddenValue || callbackValue || '';

      return {
        url: location.href,
        title: document.title || '',
        tokenLength: token.length,
        tokenSample: token.slice(0, 40),
        isDummyToken: token.includes('DUMMY'),
        hiddenFieldLength: hiddenValue.length,
        callbackLength: callbackValue.length,
        widgetCount: document.querySelectorAll('.cf-turnstile').length,
      };
    });

    const report = {
      timestamp: new Date().toISOString(),
      headed,
      waitMs,
      ok: result.isDummyToken,
      result,
    };

    console.log(JSON.stringify(report, null, 2));
    process.exit(result.isDummyToken ? 0 : 1);
  } finally {
    await browser.close().catch(() => {});
    await closeServer(server);
  }
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
});
