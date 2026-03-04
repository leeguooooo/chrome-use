#!/usr/bin/env node

/**
 * End-to-end stealth regression check across key anti-bot targets.
 *
 * Usage:
 *   node scripts/check-stealth-regression.js
 *   node scripts/check-stealth-regression.js --binary ./cli/target/release/agent-browser
 *   node scripts/check-stealth-regression.js --session-name stealth-regression
 */

import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync } from 'node:fs';
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

const sessionName = getArgValue('--session-name', 'stealth-regression');
const screenshotDir = getArgValue('--screenshot-dir', join('/tmp', 'agent-browser-stealth-regression'));
const binaryArg = getArgValue('--binary', '');

const candidates = [
  binaryArg,
  join(rootDir, 'cli', 'target', 'release', 'agent-browser'),
  join(rootDir, 'bin', 'agent-browser.js'),
  'agent-browser-stealth',
  'agent-browser',
].filter(Boolean);

function tryResolveBinary() {
  for (const candidate of candidates) {
    if (candidate.includes('/') && !existsSync(candidate)) continue;
    const probe = spawnSync(candidate, ['--version'], { encoding: 'utf8' });
    if (probe.status === 0) return candidate;
  }
  throw new Error(
    `Unable to find a runnable agent-browser binary. Tried: ${candidates.join(', ')}`
  );
}

const binary = tryResolveBinary();
const sessionArgs = ['--session', sessionName, '--session-name', sessionName];

function runBinary(commandArgs, { allowFailure = false } = {}) {
  const result = spawnSync(binary, commandArgs, {
    encoding: 'utf8',
    maxBuffer: 10 * 1024 * 1024,
  });
  if (result.status !== 0 && !allowFailure) {
    const stderr = (result.stderr || '').trim();
    const stdout = (result.stdout || '').trim();
    throw new Error(
      `Command failed: ${binary} ${commandArgs.join(' ')}\n` +
        `${stderr || stdout || `exit code ${result.status}`}`
    );
  }
  return result;
}

function runJson(actionArgs, options = {}) {
  const result = runBinary([...sessionArgs, '--json', ...actionArgs], options);
  const output = (result.stdout || '').trim();
  if (!output) return null;
  try {
    return JSON.parse(output);
  } catch {
    throw new Error(`Expected JSON output, got:\n${output}`);
  }
}

const genericRiskScript = `(() => {
  const lowerTitle = String(document.title || '').toLowerCase();
  const lowerBody = String(document.body?.innerText || '').toLowerCase();
  const hasCloudflare =
    lowerTitle.includes('just a moment') ||
    lowerTitle.includes('performing security verification') ||
    lowerBody.includes('performing security verification') ||
    lowerBody.includes('checking your browser') ||
    lowerBody.includes('cloudflare');
  const hasCaptcha =
    lowerBody.includes('captcha') ||
    lowerBody.includes('recaptcha') ||
    lowerBody.includes('hcaptcha') ||
    lowerBody.includes('turnstile');
  return {
    title: document.title || '',
    url: location.href,
    hasCloudflare,
    hasCaptcha,
    hasTurnstile:
      !!document.querySelector('.cf-turnstile, iframe[src*="challenges.cloudflare.com"], [name="cf-turnstile-response"]'),
    bodySample: lowerBody.slice(0, 600),
  };
})()`;

const sannysoftScript = `(() => {
  const normalize = (s) => String(s || '').replace(/\\s+/g, ' ').trim();
  const rows = Array.from(document.querySelectorAll('tr'));
  const failed = rows.filter((row) => /failed|fail/i.test(normalize(row.innerText)));
  return {
    failedCount: failed.length,
    failedRows: failed.map((row) => normalize(row.innerText)),
    navigatorWebdriver: navigator.webdriver,
    navigatorVendor: navigator.vendor,
  };
})()`;

function sanitizeFileSegment(input) {
  return input.replace(/[^a-zA-Z0-9._-]+/g, '-');
}

function main() {
  mkdirSync(screenshotDir, { recursive: true });
  const timestamp = new Date().toISOString();
  const targets = [
    'https://bot.sannysoft.com/',
    'https://chatgpt.com/',
    'https://super86.cc/login',
  ];

  runBinary([...sessionArgs, 'close'], { allowFailure: true });

  const report = {
    binary,
    sessionName,
    timestamp,
    screenshotDir,
    doctor: null,
    targets: [],
    ok: true,
  };

  try {
    const doctorResp = runJson(['doctor']);
    report.doctor = doctorResp?.data ?? null;

    for (const target of targets) {
      const entry = {
        target,
        open: null,
        risk: null,
        sannysoft: null,
        screenshot: null,
        ok: true,
        error: null,
      };

      try {
        const openResp = runJson(['open', target]);
        entry.open = openResp?.data ?? null;

        runJson(['wait', '3000'], { allowFailure: true });

        const riskResp = runJson(['eval', genericRiskScript]);
        entry.risk = riskResp?.data?.result ?? null;

        if (target.includes('bot.sannysoft.com')) {
          const sannysoftResp = runJson(['eval', sannysoftScript]);
          entry.sannysoft = sannysoftResp?.data?.result ?? null;
          if ((entry.sannysoft?.failedCount ?? 1) > 0) {
            entry.ok = false;
          }
        } else if (entry.risk?.hasCloudflare || entry.risk?.hasCaptcha) {
          entry.ok = false;
        }

        const host = sanitizeFileSegment(new URL(target).host);
        const shotPath = join(
          screenshotDir,
          `${host}-${Date.now().toString(36)}.png`
        );
        runJson(['screenshot', '--full', shotPath], { allowFailure: true });
        entry.screenshot = shotPath;
      } catch (error) {
        entry.ok = false;
        entry.error = error instanceof Error ? error.message : String(error);
      }

      if (!entry.ok) report.ok = false;
      report.targets.push(entry);
    }
  } finally {
    runBinary([...sessionArgs, 'close'], { allowFailure: true });
  }

  console.log(JSON.stringify(report, null, 2));
  process.exit(report.ok ? 0 : 1);
}

main();
