#!/usr/bin/env node

/**
 * Verifies that all bundled platform binaries are present and embed the
 * package.json version string. This catches stale binary bundles where the
 * package version is bumped but one or more binaries were not rebuilt.
 */

import { existsSync, readFileSync, statSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const rootDir = join(__dirname, "..");

const pkg = JSON.parse(readFileSync(join(rootDir, "package.json"), "utf8"));
const expectedVersion = String(pkg.version || "").trim();

if (!expectedVersion) {
  console.error("Error: package.json version is empty");
  process.exit(1);
}

const expectedBinaries = [
  "agent-browser-linux-x64",
  "agent-browser-linux-arm64",
  "agent-browser-win32-x64.exe",
  "agent-browser-darwin-x64",
  "agent-browser-darwin-arm64",
];

const minSizeBytes = 100_000;
const versionBytes = Buffer.from(expectedVersion, "utf8");

let errors = 0;

for (const name of expectedBinaries) {
  const binaryPath = join(rootDir, "bin", name);
  if (!existsSync(binaryPath)) {
    console.error(`ERROR: missing binary: bin/${name}`);
    errors += 1;
    continue;
  }

  const size = statSync(binaryPath).size;
  if (size < minSizeBytes) {
    console.error(
      `ERROR: binary too small: bin/${name} (${size} bytes, expected >= ${minSizeBytes})`
    );
    errors += 1;
    continue;
  }

  const bytes = readFileSync(binaryPath);
  if (!bytes.includes(versionBytes)) {
    console.error(
      `ERROR: stale binary version: bin/${name} does not contain "${expectedVersion}"`
    );
    errors += 1;
    continue;
  }

  console.log(`OK: bin/${name} matches version ${expectedVersion}`);
}

if (errors > 0) {
  console.error(`\nFound ${errors} binary validation issue(s).`);
  process.exit(1);
}

console.log(`\nAll bundled binaries match package.json version ${expectedVersion}.`);
