#!/usr/bin/env node

/**
 * End-to-end check for CreepJS headless/stealth indicators.
 *
 * Usage:
 *   node scripts/check-creepjs-headless.js
 *   node scripts/check-creepjs-headless.js --compare-stealth
 *   node scripts/check-creepjs-headless.js --binary ./cli/target/release/agent-browser
 */

import { spawnSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const rootDir = join(__dirname, '..');

const args = process.argv.slice(2);
const getArgValue = (name, fallback) => {
  const index = args.indexOf(name);
  if (index === -1 || index + 1 >= args.length) return fallback;
  return args[index + 1];
};

const binary = getArgValue('--binary', join(rootDir, 'cli', 'target', 'release', 'agent-browser'));
const sessionPrefix = getArgValue('--session-prefix', 'creepjs-e2e');
const compareStealth = args.includes('--compare-stealth');
const targetUrl = getArgValue('--url', 'https://abrahamjuliot.github.io/creepjs/');

const extractionScript = `(() => {
  const headless = globalThis.Fingerprint?.headless ?? null;
  const toNumber = (value) => (typeof value === 'number' ? value : null);
  return {
    found: !!headless,
    metrics: headless ? {
      chromium: !!headless.chromium,
      likeHeadless: toNumber(headless.likeHeadlessRating),
      headless: toNumber(headless.headlessRating),
      stealth: toNumber(headless.stealthRating),
      raw: headless,
    } : null,
    navigator: {
      userAgent: navigator.userAgent,
      userAgentData: navigator.userAgentData ? navigator.userAgentData.toJSON?.() ?? null : null,
      language: navigator.language,
      languages: navigator.languages,
      platform: navigator.platform,
      webdriver: navigator.webdriver,
      webdriverInNavigator: ('webdriver' in navigator),
    },
    window: {
      innerWidth: window.innerWidth,
      innerHeight: window.innerHeight,
      outerWidth: window.outerWidth,
      outerHeight: window.outerHeight,
      screenX: window.screenX,
      screenY: window.screenY,
    },
    intl: {
      locale: Intl.DateTimeFormat().resolvedOptions().locale,
      timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
    },
  };
})()`;

function runCommand(commandArgs, options = {}) {
  const result = spawnSync(binary, commandArgs, { encoding: 'utf8' });
  if (result.status !== 0 && !options.allowFailure) {
    const stderr = (result.stderr || '').trim();
    const stdout = (result.stdout || '').trim();
    throw new Error(
      `Command failed: ${binary} ${commandArgs.join(' ')}\n` +
        `${stderr || stdout || `exit code ${result.status}`}`
    );
  }
  return result;
}

function withSessionArgs(session, stealth) {
  const base = ['--session', session];
  if (stealth === false) {
    base.push('--stealth', 'false');
  }
  return base;
}

function runSingleCheck({ stealth, runId }) {
  const session = `${sessionPrefix}-${runId}-${stealth ? 'stealth-on' : 'stealth-off'}`;

  runCommand([...withSessionArgs(session, stealth), 'close'], { allowFailure: true });

  try {
    runCommand([...withSessionArgs(session, stealth), 'open', targetUrl]);
    runCommand([
      ...withSessionArgs(session, stealth),
      'wait',
      '--fn',
      '!!(window.Fingerprint && window.Fingerprint.headless)',
    ]);
    runCommand([...withSessionArgs(session, stealth), 'wait', '2000']);

    const evalResult = runCommand([
      ...withSessionArgs(session, stealth),
      'eval',
      '--json',
      extractionScript,
    ]);

    const payload = JSON.parse(evalResult.stdout);
    return {
      session,
      stealth,
      url: targetUrl,
      extracted: payload?.data?.result ?? null,
    };
  } finally {
    runCommand([...withSessionArgs(session, stealth), 'close'], { allowFailure: true });
  }
}

function main() {
  const runId = Date.now();
  const checks = compareStealth ? [true, false] : [true];
  const results = checks.map((stealth) => runSingleCheck({ stealth, runId }));

  const output = {
    binary,
    compareStealth,
    timestamp: new Date().toISOString(),
    results,
  };

  console.log(JSON.stringify(output, null, 2));
}

main();
