#!/usr/bin/env node

/**
 * Copies the compiled Rust binary to bin/ with platform-specific naming.
 * On macOS, re-apply an ad-hoc signature after copying so the binary remains
 * executable from the packaged bin/ path.
 */

import { copyFileSync, existsSync, mkdirSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';
import { platform, arch } from 'os';
import { spawnSync } from 'child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const projectRoot = join(__dirname, '..');

const binDir = join(projectRoot, 'bin');

function defaultPaths() {
  const sourceExt = platform() === 'win32' ? '.exe' : '';
  const sourcePath = join(projectRoot, `cli/target/release/agent-browser${sourceExt}`);

  const platformKey = `${platform()}-${arch()}`;
  const ext = platform() === 'win32' ? '.exe' : '';
  const targetName = `agent-browser-${platformKey}${ext}`;
  const targetPath = join(binDir, targetName);

  return { sourcePath, targetPath };
}

function resolvePaths() {
  const [sourceArg, targetArg] = process.argv.slice(2);
  if (!sourceArg && !targetArg) {
    return defaultPaths();
  }
  if (!sourceArg || !targetArg) {
    console.error('Usage: node scripts/copy-native.js [source-binary target-binary]');
    process.exit(1);
  }
  return {
    sourcePath: join(projectRoot, sourceArg),
    targetPath: join(projectRoot, targetArg),
  };
}

function adHocSignIfNeeded(targetPath) {
  if (platform() !== 'darwin') {
    return;
  }

  const result = spawnSync('codesign', ['--force', '--sign', '-', targetPath], {
    stdio: 'pipe',
    encoding: 'utf8',
  });

  if (result.status !== 0) {
    const message = result.stderr?.trim() || result.stdout?.trim() || 'unknown codesign error';
    console.error(`Error: Failed to codesign ${targetPath}: ${message}`);
    process.exit(result.status ?? 1);
  }
}

const { sourcePath, targetPath } = resolvePaths();

if (!existsSync(sourcePath)) {
  console.error(`Error: Native binary not found at ${sourcePath}`);
  console.error('Run "cargo build --release --manifest-path cli/Cargo.toml" first');
  process.exit(1);
}

if (!existsSync(binDir)) {
  mkdirSync(binDir, { recursive: true });
}

copyFileSync(sourcePath, targetPath);
adHocSignIfNeeded(targetPath);
console.log(`✓ Copied native binary to ${targetPath}`);
