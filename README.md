# agent-browser-stealth

Stealth fork of [agent-browser](https://github.com/vercel-labs/agent-browser) — connects to your real Chrome, shares your login sessions, and is undetectable by anti-bot systems.

For basic usage, commands, and API reference, see the [upstream documentation](https://github.com/vercel-labs/agent-browser).

## Why this fork?

**agent-browser** launches a fresh browser with an empty profile. You need to log in again, and websites can detect it's automated.

**agent-browser-stealth** connects to your existing Chrome. Your cookies, sessions, and browser fingerprint are all real — because it IS your real browser.

| | agent-browser | agent-browser-stealth |
|---|---|---|
| Browser | Launches new Chrome | Connects to your Chrome |
| Login state | Empty, need to re-login | Your existing sessions |
| Fingerprint | Automation markers present | Your real fingerprint |
| User collaboration | Separate window | Same window, take over anytime |
| CAPTCHA | Agent stuck | You solve it, agent continues |

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/leeguooooo/agent-browser-stealth/main/install.sh | sh
```

Downloads the prebuilt binary for your platform from the latest [GitHub Release](https://github.com/leeguooooo/agent-browser-stealth/releases) and installs `agent-browser` (+ the `abs` alias). No npm, no tokens.

<details>
<summary>Other ways to install</summary>

- **Pin a version:** `AGENT_BROWSER_VERSION=v0.27.0-fork.12 curl -fsSL https://raw.githubusercontent.com/leeguooooo/agent-browser-stealth/main/install.sh | sh`
- **Custom location:** `AGENT_BROWSER_BIN_DIR=$HOME/bin curl -fsSL … | sh`
- **Windows:** download `agent-browser-win32-x64.tar.gz` from the [Releases page](https://github.com/leeguooooo/agent-browser-stealth/releases) and put `agent-browser.exe` on your PATH.
- **npm (legacy):** `npm install -g agent-browser-stealth` — still published, but GitHub Releases is the primary channel now.
</details>

### Install the AI agent skills

The repo ships SKILL.md files for Claude Code, Cursor, etc. Pull them into the current project with [skills.sh](https://skills.sh):

```bash
npx skills add leeguooooo/agent-browser-stealth
```

This drops `skills/agent-browser` (and the specialized `skill-data/{core,electron,slack,dogfood,agentcore,vercel-sandbox}`) into your project so your AI agent gets the right usage patterns and pre-approved bash permissions for `agent-browser`, `agent-browser-stealth`, and `abs`.

## Command names

`agent-browser`, `agent-browser-stealth`, and `abs` are **the same binary** —
`abs` is just a short alias. There is no separate "stealth executable"; stealth
is a runtime behavior (see [Anti-detection](#anti-detection) below), applied
automatically based on whether you attach to your real Chrome or `--launch` a
fresh one.

## Setup: connect to your Chrome

Attaching uses the Chrome DevTools Protocol, which Chrome only exposes when it is
**launched with a remote-debugging port**. This is a startup flag, not a setting
— the `chrome://inspect` toggle alone is **not** enough (it only enables target
discovery, not the CDP attach).

**Recommended — fully quit Chrome, then relaunch with the port:**

```bash
# macOS
open -a "Google Chrome" --args --remote-debugging-port=9222
# Linux
google-chrome --remote-debugging-port=9222
# Windows: add --remote-debugging-port=9222 to your Chrome shortcut's target
```

Then run `agent-browser open <url>` — it auto-discovers the port and attaches.
On first attach, **Chrome 136+ shows an "Allow remote debugging?" dialog — click
Allow once** (it persists for that Chrome session).

**No setup / don't want to touch your real Chrome?** Use
`agent-browser --launch open <url>` to spawn a fresh isolated stealth browser
(full anti-detection patches applied; see below). This always works without any
port setup and is what CI uses automatically.

## Usage

```bash
# Connect to your Chrome and navigate
agent-browser open https://example.com

# Everything works through your logged-in browser
agent-browser click "Post"
agent-browser fill "Title" "Hello World"
agent-browser screenshot ./page.png
```

The agent operates in your Chrome — you'll see tabs opening, pages loading, clicks happening in real time. You can take over at any point (e.g. solve a CAPTCHA), then let the agent continue.

### Standalone mode

If you need a separate browser (CI, testing, etc.):

```bash
agent-browser --launch open https://example.com
```

In CI environments, standalone mode is used automatically.

## Anti-detection

When connected to your real Chrome, we inject **zero** JavaScript patches. Your browser's fingerprint is completely genuine.

The only thing we do is call `Emulation.setAutomationOverride` via CDP to set `navigator.webdriver = false` at the native Chrome level — undetectable by lie-detection systems like CreepJS.

**Test results (connected to real Chrome):**

| Test site | Result |
|---|---|
| [CreepJS](https://abrahamjuliot.github.io/creepjs/) | 0% stealth, 0% headless |
| [bot.sannysoft.com](https://bot.sannysoft.com) | All green |
| [Cloudflare Turnstile](https://nowsecure.nl) | Passed |

When using `--launch` mode (standalone browser), a full suite of 32 stealth patches is applied for headless Chrome.

## Differences from upstream

Based on [agent-browser v0.27.0](https://github.com/vercel-labs/agent-browser). Changes:

- **Auto-connect is default** — `agent-browser open <url>` connects to your Chrome instead of launching a new one
- **CDP-native stealth** — `Emulation.setAutomationOverride` instead of JS patches
- **Dual stealth mode** — zero patches for real Chrome, full patches for `--launch` mode
- **`--launch` / `--new` flag** — explicitly start a standalone browser
- **CI auto-detection** — standalone mode when `CI` env var is set

All upstream features (commands, snapshots, screenshots, recordings, tabs, sessions, etc.) work the same. See the [upstream repo](https://github.com/vercel-labs/agent-browser) for full documentation.

## License

Apache-2.0 (same as upstream)
