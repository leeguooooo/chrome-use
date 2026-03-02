#!/usr/bin/env node

/**
 * Verifies that the bundled native binary version matches package.json version.
 * This prevents publishing npm tarballs where package version and native binary
 * version drift (e.g. package is fork.8 but binary still reports fork.7).
 */

import { existsSync, readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';
import { arch, platform } from 'os';
import { execFileSync } from 'child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const projectRoot = join(__dirname, '..');

const pkg = JSON.parse(readFileSync(join(projectRoot, 'package.json'), 'utf8'));
const expectedVersion = pkg.version;

const ext = platform() === 'win32' ? '.exe' : '';
const platformBinary = join(projectRoot, 'bin', `agent-browser-${platform()}-${arch()}${ext}`);

if (!existsSync(platformBinary)) {
  console.error(`Error: native binary not found for current platform: ${platformBinary}`);
  console.error('Run `pnpm run build:native` before publishing.');
  process.exit(1);
}

let versionOutput = '';
try {
  versionOutput = execFileSync(platformBinary, ['--version'], {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  }).trim();
} catch (error) {
  const message = error instanceof Error ? error.message : String(error);
  console.error(`Error: failed to execute native binary --version: ${message}`);
  process.exit(1);
}

if (!versionOutput.includes(expectedVersion)) {
  console.error(`Version mismatch: package.json=${expectedVersion}, native='${versionOutput}'.`);
  console.error('Run `pnpm run build:native` and retry publishing.');
  process.exit(1);
}

console.log(`✓ Native binary version matches package.json (${expectedVersion})`);
