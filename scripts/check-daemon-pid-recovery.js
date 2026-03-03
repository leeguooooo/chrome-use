#!/usr/bin/env node

/**
 * Regression check for daemon liveness when <session>.pid is missing.
 *
 * What it verifies:
 * 1) A daemon session is reachable.
 * 2) Deleting <session>.pid does not break the next command.
 * 3) The session socket is not recreated (inode unchanged on Unix),
 *    meaning we reused the live daemon instead of tearing it down.
 *
 * Usage:
 *   node scripts/check-daemon-pid-recovery.js
 *   node scripts/check-daemon-pid-recovery.js --session default
 *   node scripts/check-daemon-pid-recovery.js --binary ./bin/agent-browser-darwin-arm64
 */

import fs from 'node:fs';
import os from 'node:os';
import path, { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const rootDir = join(__dirname, '..');

const args = process.argv.slice(2);
const getArgValue = (name, fallback) => {
  const index = args.indexOf(name);
  if (index === -1 || index + 1 >= args.length) return fallback;
  return args[index + 1];
};

function resolveSocketDir() {
  if (process.env.AGENT_BROWSER_SOCKET_DIR && process.env.AGENT_BROWSER_SOCKET_DIR.length > 0) {
    return process.env.AGENT_BROWSER_SOCKET_DIR;
  }
  if (process.env.XDG_RUNTIME_DIR && process.env.XDG_RUNTIME_DIR.length > 0) {
    return path.join(process.env.XDG_RUNTIME_DIR, 'agent-browser');
  }
  return path.join(os.homedir(), '.agent-browser');
}

function resolveDefaultBinary() {
  const osKey = os.platform() === 'win32' ? 'win32' : os.platform() === 'darwin' ? 'darwin' : 'linux';
  const archKey = os.arch() === 'arm64' ? 'arm64' : 'x64';
  const ext = os.platform() === 'win32' ? '.exe' : '';

  const candidates = [
    join(rootDir, 'bin', `agent-browser-${osKey}-${archKey}${ext}`),
    join(rootDir, 'cli', 'target', 'release', `agent-browser${ext}`),
    join(rootDir, 'bin', `agent-browser-local${ext}`),
  ];

  for (const candidate of candidates) {
    if (fs.existsSync(candidate)) return candidate;
  }

  return candidates[0];
}

function runCommand(binary, commandArgs, allowFailure = false) {
  const result = spawnSync(binary, commandArgs, {
    encoding: 'utf8',
    env: process.env,
  });
  if (result.status !== 0 && !allowFailure) {
    const stderr = (result.stderr || '').trim();
    const stdout = (result.stdout || '').trim();
    throw new Error(
      `Command failed: ${binary} ${commandArgs.join(' ')}\n${stderr || stdout || `exit code ${result.status}`}`
    );
  }
  return result;
}

function main() {
  const session = getArgValue('--session', 'default');
  const binary = getArgValue('--binary', resolveDefaultBinary());
  const socketDir = resolveSocketDir();
  const isWindows = os.platform() === 'win32';

  const pidPath = join(socketDir, `${session}.pid`);
  const socketPath = isWindows ? null : join(socketDir, `${session}.sock`);
  const portPath = isWindows ? join(socketDir, `${session}.port`) : null;

  if (!fs.existsSync(binary)) {
    throw new Error(`Binary not found: ${binary}`);
  }

  // Ensure daemon/session is live before we simulate pid loss.
  runCommand(binary, ['--session', session, 'get', 'url']);

  let socketInodeBefore = null;
  if (!isWindows) {
    if (!socketPath || !fs.existsSync(socketPath)) {
      throw new Error(`Socket file not found: ${socketPath}`);
    }
    socketInodeBefore = fs.statSync(socketPath).ino;
  }

  const pidExistedBefore = fs.existsSync(pidPath);
  if (pidExistedBefore) {
    fs.unlinkSync(pidPath);
  }

  // This is the critical step: should still work even though pid file is gone.
  const second = runCommand(binary, ['--session', session, 'get', 'title']);
  const secondOutput = (second.stdout || '').trim();

  let socketInodeAfter = null;
  let socketUnchanged = true;
  if (!isWindows) {
    if (!socketPath || !fs.existsSync(socketPath)) {
      throw new Error(`Socket file missing after pid removal: ${socketPath}`);
    }
    socketInodeAfter = fs.statSync(socketPath).ino;
    socketUnchanged = socketInodeBefore === socketInodeAfter;
  } else if (portPath && !fs.existsSync(portPath)) {
    throw new Error(`Port file missing after pid removal: ${portPath}`);
  }

  const pidExistsAfter = fs.existsSync(pidPath);
  const passed = socketUnchanged;

  const report = {
    passed,
    session,
    binary,
    socketDir,
    pidPath,
    pidExistedBefore,
    pidExistsAfter,
    socketPath,
    socketInodeBefore,
    socketInodeAfter,
    socketUnchanged,
    secondCommandOutput: secondOutput,
    timestamp: new Date().toISOString(),
  };

  console.log(JSON.stringify(report, null, 2));
  if (!passed) {
    process.exit(1);
  }
}

main();
