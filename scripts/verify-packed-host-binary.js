#!/usr/bin/env node

/**
 * Build an npm tarball locally and verify that the bundled host-platform native
 * binary reports the same version as package.json.
 *
 * This catches publish drifts where package.json is bumped but the embedded
 * binary still points to an older fork version.
 */

import { existsSync, mkdtempSync, readFileSync, rmSync, unlinkSync } from 'fs';
import { tmpdir } from 'os';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';
import { execFileSync } from 'child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const rootDir = join(__dirname, '..');

const pkg = JSON.parse(readFileSync(join(rootDir, 'package.json'), 'utf8'));
const expectedVersion = String(pkg.version || '').trim();

if (!expectedVersion) {
  console.error('Error: package.json version is empty');
  process.exit(1);
}

const ext = process.platform === 'win32' ? '.exe' : '';
const hostBinaryName = `agent-browser-${process.platform}-${process.arch}${ext}`;

let tempDir = '';
let packedTarball = '';

try {
  const packRaw = execFileSync('npm', ['pack', '--json'], {
    cwd: rootDir,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  const parsed = JSON.parse(packRaw);
  const filename = parsed?.[0]?.filename;
  if (!filename || typeof filename !== 'string') {
    throw new Error(`unexpected npm pack output: ${packRaw}`);
  }

  packedTarball = join(rootDir, filename);
  if (!existsSync(packedTarball)) {
    throw new Error(`tarball not found: ${packedTarball}`);
  }

  tempDir = mkdtempSync(join(tmpdir(), 'ab-pack-verify-'));
  execFileSync('tar', ['-xzf', packedTarball, '-C', tempDir], {
    stdio: ['ignore', 'pipe', 'pipe'],
  });

  const packedRoot = join(tempDir, 'package');
  const packedPkg = JSON.parse(readFileSync(join(packedRoot, 'package.json'), 'utf8'));
  if (String(packedPkg.version || '').trim() !== expectedVersion) {
    throw new Error(
      `packed package.json version mismatch: expected ${expectedVersion}, got ${packedPkg.version}`
    );
  }

  const packedBinary = join(packedRoot, 'bin', hostBinaryName);
  if (!existsSync(packedBinary)) {
    throw new Error(`host binary missing in tarball: bin/${hostBinaryName}`);
  }

  const binaryVersion = execFileSync(packedBinary, ['--version'], {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  }).trim();

  if (!binaryVersion.includes(expectedVersion)) {
    throw new Error(
      `tarball host binary version mismatch: expected ${expectedVersion}, got "${binaryVersion}"`
    );
  }

  console.log(`✓ Packed tarball host binary matches package.json (${expectedVersion})`);
} catch (error) {
  const message = error instanceof Error ? error.message : String(error);
  console.error(`Error: ${message}`);
  process.exitCode = 1;
} finally {
  if (tempDir) rmSync(tempDir, { recursive: true, force: true });
  if (packedTarball && existsSync(packedTarball)) unlinkSync(packedTarball);
}
