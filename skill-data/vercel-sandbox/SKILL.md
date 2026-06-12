---
name: vercel-sandbox
description: Run chrome-use + Chrome inside Vercel Sandbox microVMs for browser automation from any Vercel-deployed app. Use when the user needs browser automation in a Vercel app (Next.js, SvelteKit, Nuxt, Remix, Astro, etc.), wants to run headless Chrome without binary size limits, needs persistent browser sessions across commands, or wants ephemeral isolated browser environments. Triggers include "Vercel Sandbox browser", "microVM Chrome", "chrome-use in sandbox", "browser automation on Vercel", or any task requiring Chrome in a Vercel Sandbox.
---

# Browser Automation with Vercel Sandbox

Run chrome-use + headless Chrome inside ephemeral Vercel Sandbox microVMs. A Linux VM spins up on demand, executes browser commands, and shuts down. Works with any Vercel-deployed framework (Next.js, SvelteKit, Nuxt, Remix, Astro, etc.).

## Dependencies

```bash
pnpm add @vercel/sandbox
```

The sandbox VM needs system dependencies for Chromium plus chrome-use itself. Use sandbox snapshots (below) to pre-install everything for sub-second startup.

## Core Pattern

```ts
import { Sandbox } from "@vercel/sandbox";

// System libraries required by Chromium on the sandbox VM (Amazon Linux / dnf)
const CHROMIUM_SYSTEM_DEPS = [
  "nss", "nspr", "libxkbcommon", "atk", "at-spi2-atk", "at-spi2-core",
  "libXcomposite", "libXdamage", "libXrandr", "libXfixes", "libXcursor",
  "libXi", "libXtst", "libXScrnSaver", "libXext", "mesa-libgbm", "libdrm",
  "mesa-libGL", "mesa-libEGL", "cups-libs", "alsa-lib", "pango", "cairo",
  "gtk3", "dbus-libs",
];

function getSandboxCredentials() {
  if (
    process.env.VERCEL_TOKEN &&
    process.env.VERCEL_TEAM_ID &&
    process.env.VERCEL_PROJECT_ID
  ) {
    return {
      token: process.env.VERCEL_TOKEN,
      teamId: process.env.VERCEL_TEAM_ID,
      projectId: process.env.VERCEL_PROJECT_ID,
    };
  }
  return {};
}

async function withBrowser<T>(
  fn: (sandbox: InstanceType<typeof Sandbox>) => Promise<T>,
): Promise<T> {
  const snapshotId = process.env.AGENT_BROWSER_SNAPSHOT_ID;
  const credentials = getSandboxCredentials();

  const sandbox = snapshotId
    ? await Sandbox.create({
        ...credentials,
        source: { type: "snapshot", snapshotId },
        timeout: 120_000,
      })
    : await Sandbox.create({ ...credentials, runtime: "node24", timeout: 120_000 });

  if (!snapshotId) {
    await sandbox.runCommand("sh", [
      "-c",
      `sudo dnf clean all 2>&1 && sudo dnf install -y --skip-broken ${CHROMIUM_SYSTEM_DEPS.join(" ")} 2>&1 && sudo ldconfig 2>&1`,
    ]);
    await sandbox.runCommand("npm", ["install", "-g", "chrome-use"]);
    await sandbox.runCommand("npx", ["chrome-use", "install"]);
  }

  try {
    return await fn(sandbox);
  } finally {
    await sandbox.stop();
  }
}
```

## Screenshot

The `screenshot --json` command saves to a file and returns the path. Read the file back as base64:

```ts
export async function screenshotUrl(url: string) {
  return withBrowser(async (sandbox) => {
    await sandbox.runCommand("chrome-use", ["open", url]);

    const titleResult = await sandbox.runCommand("chrome-use", [
      "get", "title", "--json",
    ]);
    const title = JSON.parse(await titleResult.stdout())?.data?.title || url;

    const ssResult = await sandbox.runCommand("chrome-use", [
      "screenshot", "--json",
    ]);
    const ssPath = JSON.parse(await ssResult.stdout())?.data?.path;
    const b64Result = await sandbox.runCommand("base64", ["-w", "0", ssPath]);
    const screenshot = (await b64Result.stdout()).trim();

    await sandbox.runCommand("chrome-use", ["close"]);

    return { title, screenshot };
  });
}
```

## Accessibility Snapshot

```ts
export async function snapshotUrl(url: string) {
  return withBrowser(async (sandbox) => {
    await sandbox.runCommand("chrome-use", ["open", url]);

    const titleResult = await sandbox.runCommand("chrome-use", [
      "get", "title", "--json",
    ]);
    const title = JSON.parse(await titleResult.stdout())?.data?.title || url;

    const snapResult = await sandbox.runCommand("chrome-use", [
      "snapshot", "-i", "-c",
    ]);
    const snapshot = await snapResult.stdout();

    await sandbox.runCommand("chrome-use", ["close"]);

    return { title, snapshot };
  });
}
```

## Multi-Step Workflows

The sandbox persists between commands, so you can run full automation sequences:

```ts
export async function fillAndSubmitForm(url: string, data: Record<string, string>) {
  return withBrowser(async (sandbox) => {
    await sandbox.runCommand("chrome-use", ["open", url]);

    const snapResult = await sandbox.runCommand("chrome-use", [
      "snapshot", "-i",
    ]);
    const snapshot = await snapResult.stdout();
    // Parse snapshot to find element refs...

    for (const [ref, value] of Object.entries(data)) {
      await sandbox.runCommand("chrome-use", ["fill", ref, value]);
    }

    await sandbox.runCommand("chrome-use", ["click", "@e5"]);
    await sandbox.runCommand("chrome-use", ["wait", "--load", "networkidle"]);

    const ssResult = await sandbox.runCommand("chrome-use", [
      "screenshot", "--json",
    ]);
    const ssPath = JSON.parse(await ssResult.stdout())?.data?.path;
    const b64Result = await sandbox.runCommand("base64", ["-w", "0", ssPath]);
    const screenshot = (await b64Result.stdout()).trim();

    await sandbox.runCommand("chrome-use", ["close"]);

    return { screenshot };
  });
}
```

## Sandbox Snapshots (Fast Startup)

A **sandbox snapshot** is a saved VM image of a Vercel Sandbox with system dependencies + chrome-use + Chromium already installed. Think of it like a Docker image -- instead of installing dependencies from scratch every time, the sandbox boots from the pre-built image.

This is unrelated to chrome-use's *accessibility snapshot* feature (`chrome-use snapshot`), which dumps a page's accessibility tree. A sandbox snapshot is a Vercel infrastructure concept for fast VM startup.

Without a sandbox snapshot, each run installs system deps + chrome-use + Chromium (~30s). With one, startup is sub-second.

### Creating a sandbox snapshot

The snapshot must include system dependencies (via `dnf`), chrome-use, and Chromium:

```ts
import { Sandbox } from "@vercel/sandbox";

const CHROMIUM_SYSTEM_DEPS = [
  "nss", "nspr", "libxkbcommon", "atk", "at-spi2-atk", "at-spi2-core",
  "libXcomposite", "libXdamage", "libXrandr", "libXfixes", "libXcursor",
  "libXi", "libXtst", "libXScrnSaver", "libXext", "mesa-libgbm", "libdrm",
  "mesa-libGL", "mesa-libEGL", "cups-libs", "alsa-lib", "pango", "cairo",
  "gtk3", "dbus-libs",
];

async function createSnapshot(): Promise<string> {
  const sandbox = await Sandbox.create({
    runtime: "node24",
    timeout: 300_000,
  });

  await sandbox.runCommand("sh", [
    "-c",
    `sudo dnf clean all 2>&1 && sudo dnf install -y --skip-broken ${CHROMIUM_SYSTEM_DEPS.join(" ")} 2>&1 && sudo ldconfig 2>&1`,
  ]);
  await sandbox.runCommand("npm", ["install", "-g", "chrome-use"]);
  await sandbox.runCommand("npx", ["chrome-use", "install"]);

  const snapshot = await sandbox.snapshot();
  return snapshot.snapshotId;
}
```

Run this once, then set the environment variable:

```bash
AGENT_BROWSER_SNAPSHOT_ID=snap_xxxxxxxxxxxx
```

A helper script is available in the demo app:

```bash
npx tsx examples/environments/scripts/create-snapshot.ts
```

Recommended for any production deployment using the Sandbox pattern.

## Authentication

On Vercel deployments, the Sandbox SDK authenticates automatically via OIDC. For local development or explicit control, set:

```bash
VERCEL_TOKEN=<personal-access-token>
VERCEL_TEAM_ID=<team-id>
VERCEL_PROJECT_ID=<project-id>
```

These are spread into `Sandbox.create()` calls. When absent, the SDK falls back to `VERCEL_OIDC_TOKEN` (automatic on Vercel).

## Scheduled Workflows (Cron)

Combine with Vercel Cron Jobs for recurring browser tasks:

```ts
// app/api/cron/route.ts  (or equivalent in your framework)
export async function GET() {
  const result = await withBrowser(async (sandbox) => {
    await sandbox.runCommand("chrome-use", ["open", "https://example.com/pricing"]);
    const snap = await sandbox.runCommand("chrome-use", ["snapshot", "-i", "-c"]);
    await sandbox.runCommand("chrome-use", ["close"]);
    return await snap.stdout();
  });

  // Process results, send alerts, store data...
  return Response.json({ ok: true, snapshot: result });
}
```

```json
// vercel.json
{ "crons": [{ "path": "/api/cron", "schedule": "0 9 * * *" }] }
```

## Environment Variables

| Variable | Required | Description |
|---|---|---|
| `AGENT_BROWSER_SNAPSHOT_ID` | No (but recommended) | Pre-built sandbox snapshot ID for sub-second startup (see above) |
| `VERCEL_TOKEN` | No | Vercel personal access token (for local dev; OIDC is automatic on Vercel) |
| `VERCEL_TEAM_ID` | No | Vercel team ID (for local dev) |
| `VERCEL_PROJECT_ID` | No | Vercel project ID (for local dev) |

## Framework Examples

The pattern works identically across frameworks. The only difference is where you put the server-side code:

| Framework | Server code location |
|---|---|
| Next.js | Server actions, API routes, route handlers |
| SvelteKit | `+page.server.ts`, `+server.ts` |
| Nuxt | `server/api/`, `server/routes/` |
| Remix | `loader`, `action` functions |
| Astro | `.astro` frontmatter, API routes |

## Example

See `examples/environments/` in the chrome-use repo for a working app with the Vercel Sandbox pattern, including a sandbox snapshot creation script, streaming progress UI, and rate limiting.
