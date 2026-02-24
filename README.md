# agent-browser-stealth

Stealth-focused fork of `agent-browser` for anti-bot evasion in production automation.

This fork keeps core browser automation capabilities in sync with upstream `agent-browser`, and focuses its own changes on stealth and anti-detection behavior.

## Positioning

- Core commands and workflows: aligned with upstream `agent-browser`
- Fork value: stronger anti-bot defaults and operational policies
- Default mindset: no extra stealth toggle, stealth is always on

## Installation

### Global (recommended)

```bash
npm install -g agent-browser-stealth
agent-browser install
```

### Quick try with npx

```bash
npx agent-browser-stealth install
npx agent-browser-stealth open example.com
```

### From source

```bash
git clone https://github.com/leeguooooo/agent-browser
cd agent-browser
pnpm install
pnpm build
pnpm build:native
pnpm link --global
agent-browser install
```

## Quick Start

```bash
agent-browser open https://example.com
agent-browser snapshot -i
agent-browser click @e2
agent-browser fill @e3 "test@example.com"
agent-browser screenshot page.png
```

## Anti-Bot Measures

Stealth is always enabled. Legacy `launch.stealth` is accepted only for compatibility and ignored.

### 1) Fingerprint hardening

- Hides automation indicators such as `navigator.webdriver`
- Adds Chromium launch args to reduce automation fingerprints
- Rewrites headless UA markers (`HeadlessChrome`)
- Patches high-signal surfaces such as:
  - `navigator.plugins` / `navigator.mimeTypes`
  - `window.chrome.runtime`
  - WebGL vendor/renderer exposure
  - permissions/language/media/device related probes
- Applies both context init scripts and CDP-level UA overrides
- Preserves explicit custom UA from `--user-agent` or `launch({ userAgent })`

### 2) Behavioral humanization

- Randomized typing cadence when `--delay` is used
- Random wait ranges (`wait 2000-5000`)
- Bezier-curve mouse movement before click actions
- Randomized navigation pacing

### 3) Region signal alignment

- Auto-aligns locale/timezone/Accept-Language by target TLD
- Reduces locale-timezone mismatch risk on region-sensitive sites

### 4) Verification-aware retry

- Detects common captcha/verification interstitial patterns
- Retries navigation with randomized backoff when triggered

## Typing `--delay` Correctly

Use `--delay` as an option:

```bash
agent-browser type @e2 "iphone" --delay 120
agent-browser keyboard type "iphone" --delay 120
```

If literal text includes `--delay`, stop option parsing with `--`:

```bash
agent-browser type @e2 -- "--delay 120"
agent-browser keyboard type -- "--delay 120"
```

## Validation Snapshot

Manual checks were run against common public detection pages in headed mode, including:

- [bot.sannysoft.com](https://bot.sannysoft.com/)
- [CreepJS](https://abrahamjuliot.github.io/creepjs/)
- [areyouheadless](https://arh.antoinevastel.com/bots/areyouheadless)
- [detect-headless](https://infosimples.github.io/detect-headless)

Reproduce CreepJS check:

```bash
node scripts/check-creepjs-headless.js --binary ./cli/target/release/agent-browser
```

## Command Coverage And Docs

Core command set is intentionally kept compatible with upstream `agent-browser`.

- Full command reference: [upstream agent-browser docs](https://github.com/vercel-labs/agent-browser)
- Local help: `agent-browser --help`

## Fork Policies

This fork enforces a few operational policies:

- `--profile` / `AGENT_BROWSER_PROFILE` are forbidden
- `--channel` / `AGENT_BROWSER_CHANNEL` are forbidden
- Default mode expects an existing browser via CDP on `localhost:9333`

## Maintainer Notes (Fork Release)

- Keep `upstream-main` for clean upstream sync
- Merge upstream into short-lived sync branches, then PR into `main`
- Recommended release format: `<upstream>-fork.<fork>` (example: `0.14.0-fork.3`)
- Use npm Trusted Publishing (OIDC)

## OpenClaw Skill Sync

This repo includes a dedicated OpenClaw skill at:

- `skills/agent-browser-stealth/SKILL.md`

Local git `pre-push` hook auto-syncs skills before every push:

- `.husky/pre-push` -> `pnpm run clawhub:sync`

Manual sync command (same logic as hook):

```bash
pnpm run clawhub:sync
```

This uses your existing local ClawHub login session (no GitHub secret required).

Temporarily skip auto-sync for one push:

```bash
SKIP_CLAWHUB_SYNC=1 git push
```

## License

Apache-2.0
