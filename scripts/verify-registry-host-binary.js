#!/usr/bin/env node

/**
 * Download the published npm tarball and verify that the bundled host-platform
 * native binary reports the expected package version.
 */

import { createWriteStream, existsSync, mkdtempSync, readFileSync, rmSync, unlinkSync } from 'fs';
import { tmpdir } from 'os';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';
import { execFileSync } from 'child_process';
import { get } from 'https';

const __dirname = dirname(fileURLToPath(import.meta.url));
const rootDir = join(__dirname, '..');

const pkg = JSON.parse(readFileSync(join(rootDir, 'package.json'), 'utf8'));
const packageName = process.env.PACKAGE_NAME || pkg.name || 'agent-browser-stealth';
const expectedVersion = process.env.EXPECTED_VERSION || pkg.version;

if (!expectedVersion) {
  console.error('Error: expected version is empty');
  process.exit(1);
}

const ext = process.platform === 'win32' ? '.exe' : '';
const hostBinaryName = `agent-browser-${process.platform}-${process.arch}${ext}`;

function downloadFile(url, dest) {
  return new Promise((resolve, reject) => {
    const file = createWriteStream(dest);

    const request = (currentUrl) => {
      get(currentUrl, (response) => {
        if (response.statusCode === 301 || response.statusCode === 302) {
          request(response.headers.location);
          return;
        }

        if (response.statusCode !== 200) {
          reject(new Error(`Failed to download tarball: HTTP ${response.statusCode}`));
          return;
        }

        response.pipe(file);
        file.on('finish', () => {
          file.close();
          resolve();
        });
      }).on('error', (err) => {
        reject(err);
      });
    };

    request(url);
  });
}

let tempDir = '';
let tarballPath = '';

try {
  const tarballUrl = execFileSync(
    'npm',
    ['view', `${packageName}@${expectedVersion}`, 'dist.tarball'],
    {
      cwd: rootDir,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
    }
  ).trim();

  if (!tarballUrl) {
    throw new Error(`could not resolve dist.tarball for ${packageName}@${expectedVersion}`);
  }

  tempDir = mkdtempSync(join(tmpdir(), 'ab-registry-verify-'));
  tarballPath = join(tempDir, 'package.tgz');
  await downloadFile(tarballUrl, tarballPath);

  execFileSync('tar', ['-xzf', tarballPath, '-C', tempDir], {
    stdio: ['ignore', 'pipe', 'pipe'],
  });

  const packedRoot = join(tempDir, 'package');
  const packedPkg = JSON.parse(readFileSync(join(packedRoot, 'package.json'), 'utf8'));
  if (String(packedPkg.version || '').trim() !== expectedVersion) {
    throw new Error(
      `registry package.json version mismatch: expected ${expectedVersion}, got ${packedPkg.version}`
    );
  }

  const packedBinary = join(packedRoot, 'bin', hostBinaryName);
  if (!existsSync(packedBinary)) {
    throw new Error(`host binary missing in registry tarball: bin/${hostBinaryName}`);
  }

  const binaryVersion = execFileSync(packedBinary, ['--version'], {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  }).trim();

  if (!binaryVersion.includes(expectedVersion)) {
    throw new Error(
      `registry host binary version mismatch: expected ${expectedVersion}, got "${binaryVersion}"`
    );
  }

  console.log(
    `✓ Registry tarball host binary matches package.json (${packageName}@${expectedVersion})`
  );
} catch (error) {
  const message = error instanceof Error ? error.message : String(error);
  console.error(`Error: ${message}`);
  process.exitCode = 1;
} finally {
  if (tarballPath && existsSync(tarballPath)) unlinkSync(tarballPath);
  if (tempDir) rmSync(tempDir, { recursive: true, force: true });
}
