#!/usr/bin/env node

/**
 * End-to-end check for bot.sannysoft.com WebDriver (New) result.
 *
 * Usage:
 *   node scripts/check-sannysoft-webdriver.js
 *   node scripts/check-sannysoft-webdriver.js --compare-stealth
 *   node scripts/check-sannysoft-webdriver.js --binary ./cli/target/release/agent-browser
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
const sessionPrefix = getArgValue('--session-prefix', 'botcheck-e2e');
const compareStealth = args.includes('--compare-stealth');
const targetUrl = getArgValue('--url', 'https://bot.sannysoft.com');

const extractionScript = `(() => {
  const normalize = (s) => (s || '').replace(/\\s+/g, ' ').trim();
  const rows = Array.from(document.querySelectorAll('tr'));
  const exact = rows.find((tr) => normalize(tr.cells?.[0]?.textContent).toLowerCase() === 'webdriver (new)');
  const fallback = exact || rows.find((tr) => normalize(tr.cells?.[0]?.textContent).toLowerCase().includes('webdriver'));
  return {
    found: !!fallback,
    label: fallback ? normalize(fallback.cells?.[0]?.textContent) : null,
    valueText: fallback ? normalize(fallback.cells?.[1]?.textContent) : null,
    statusText: fallback ? normalize(fallback.textContent) : null,
    navigatorWebdriver: navigator.webdriver,
    webdriverInNavigator: ('webdriver' in navigator),
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

  // Best-effort cleanup in case previous run left state behind.
  runCommand([...withSessionArgs(session, stealth), 'close'], { allowFailure: true });

  try {
    runCommand([...withSessionArgs(session, stealth), 'open', targetUrl]);
    runCommand([...withSessionArgs(session, stealth), 'wait', '--load', 'networkidle']);
    runCommand([...withSessionArgs(session, stealth), 'wait', '5000']);

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
