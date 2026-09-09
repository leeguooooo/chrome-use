# chrome-use

**English** · [简体中文](README.zh.md)

<p align="center">
  <a href="https://github.com/leeguooooo/chrome-use/releases"><img alt="Release" src="https://img.shields.io/github/v/release/leeguooooo/chrome-use?sort=semver&color=2f81f7"></a>
  <a href="https://github.com/leeguooooo/chrome-use/stargazers"><img alt="GitHub stars" src="https://img.shields.io/github/stars/leeguooooo/chrome-use?color=f0b429"></a>
  <a href="https://bot-detector.rebrowser.net/"><img alt="CreepJS 0% bot" src="https://img.shields.io/badge/CreepJS-0%25%20bot-2ea043"></a>
  <img alt="Platforms" src="https://img.shields.io/badge/macOS%20·%20Linux%20·%20Windows-informational">
  <a href="LICENSE"><img alt="License: Apache-2.0" src="https://img.shields.io/github/license/leeguooooo/chrome-use?color=8957e5"></a>
</p>

<p align="center"><i>⭐ If it saves you a re-login, a star helps other devs find it.</i></p>

![chrome-use](assets/hero.png)

<p align="center">
  <img src="assets/demo.gif" alt="chrome-use demo: open Hacker News in your real Chrome and pull the top stories as structured JSON in one command" width="820">
  <br>
  <sub>Point it at a page in your <b>real</b> Chrome → get structured data in one command. <a href="assets/demo.tape">(regenerate: <code>vhs assets/demo.tape</code>)</a></sub>
</p>

**chrome-use** drives your real, logged-in Chrome from any AI agent. It shares your existing login sessions and is undetectable by anti-bot systems because it *is* your real browser. Part of the `*-use` family ([iphone-use](https://github.com/leeguooooo/iphone-use) drives your real iPhone; [bitwarden-use](https://github.com/leeguooooo/bitwarden-use) pulls passwords/2FA/passkeys from your Bitwarden vault so an agent can log in with credentials; chrome-use drives your real Chrome).

<sub>Originally based on [vercel-labs/agent-browser](https://github.com/vercel-labs/agent-browser) (Apache-2.0); now a standalone project. The stealth/extension-relay architecture, anti-detection, humanize, multi-agent isolation, and CLI have diverged substantially.</sub>

> 📚 **Documentation:** **[chrome-use.leeguoo.com](https://chrome-use.leeguoo.com)**: full guides, workflows & command reference (中文 · English).
>
> 📖 **Deep dive:** [Letting an agent click into cross-origin iframes: how chrome-use solves the hardest part of browser control](https://blog.leeguoo.com/en/posts/chrome-use-cross-origin-iframe/)
> · [Driving your already-logged-in real Chrome (CreepJS scores it 0% bot), 中文](https://blog.leeguoo.com/zh/posts/chrome-use-drive-your-real-chrome/)

## Give your AI agent the browser you already live in

**No fresh Chrome. No re-login. No "are you a robot?" walls.**

chrome-use points **any** agent (Claude Code, Cursor, Codex, your own scripts) at the **Chrome you're already signed into everything on**. It clicks in *your* window, so you watch it work and grab the wheel the moment it hits a 2FA prompt or captcha. And because it's literally your real browser (over a one-click extension, native messaging, no debug port), sites read it as 100% human: **[CreepJS scores it 0% bot](#anti-detection).**

**Typical browser automation** (Playwright, Puppeteer, or a fresh `--launch`) opens a brand-new browser with an empty profile. You have to log in again, and websites can tell it's automated. **chrome-use** connects to your existing Chrome. Your cookies, sessions, and browser fingerprint are all real, because it IS your real browser. Since **Chrome 136**, every raw `--remote-debugging-port` connection pops a blocking **"Allow remote debugging?"** consent dialog. Our extension uses native messaging instead: **install once, then zero per-use confirmation.**

| | Typical automation (Playwright · Puppeteer · browser-use) | web-access / raw CDP port | [Claude in Chrome](https://www.anthropic.com/claude/chrome) | **chrome-use** |
|---|:---:|:---:|:---:|:---:|
| Works with **any** agent / CLI (not one app) | ✅ | ✅ | ❌ Claude only | ✅ |
| Drives your **real, logged-in** Chrome | ❌ fresh empty profile | ✅ | ✅ | ✅ |
| Connect method / **"Allow remote debugging?" popup** | — (own browser) | `--remote-debugging-port` · **every connection** 🔴 | `chrome.debugger` · no | native messaging · **never** ✅ |
| Real-browser fingerprint (CreepJS ~0%)¹ | ❌ automation markers / headless | ✅ | ✅ | ✅ **verified 0%** |
| **No `Runtime.enable` CDP leak** (rebrowser)² | ❌ leaks | ❌ leaks | — | ✅ **off by default** |
| Many agents on **one** real Chrome, isolated tab groups³ | ❌ separate browsers | ⚠️ shared tabs, no isolation | ❌ single app | ✅ |
| Permissions footprint | full control | full CDP | 16 incl. `<all_urls>` | **7, no `<all_urls>`** |

<sub>¹ All three real-Chrome tools score ~0% on CreepJS (it's a real browser); we've measured ours. ² rebrowser's `runtimeEnableLeak`: verified clean on our relay path; Claude in Chrome not independently tested (—). ³ web-access can run parallel sub-agents on one browser, but without per-session isolation; each `--session` here gets its own colored, command-isolated tab group. See [Anti-detection](#anti-detection) for the measured numbers.</sub>

## How it works

![how it works](assets/how-it-works.png)

Your **chrome-use CLI** talks to a tiny **browser extension** over Chrome
**native messaging**: a local inter-process channel, *no network socket, no
token, no remote server*. The extension uses `chrome.debugger` to drive the tabs
you target in **your own, already-logged-in Chrome**, then hands results back to
the CLI. Everything stays on your machine.

![architecture](assets/architecture.png)

Each `--session` gets its **own colored Chrome tab group**, so multiple agents
can share one real browser concurrently without stepping on each other, or your
own tabs. When `--session` is omitted, chrome-use derives a stable per-agent
session from supported runner IDs, including Codex's `CODEX_THREAD_ID`.
Explicit `--session` / `AGENT_BROWSER_SESSION` always win. Session naming,
`session list` / `stop` / `prune`, ownership handoff, and daemon recovery are
covered in the [sessions guide](https://chrome-use.leeguoo.com/en/sessions.html).

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/leeguooooo/chrome-use/main/install.sh | sh
```

Downloads the prebuilt binary for your platform from the latest [GitHub Release](https://github.com/leeguooooo/chrome-use/releases) and installs `chrome-use` (+ the `abs` alias). No npm, no tokens.

<details>
<summary>Other ways to install</summary>

- **Pin a version:** `AGENT_BROWSER_VERSION=v0.27.0-fork.12 curl -fsSL https://raw.githubusercontent.com/leeguooooo/chrome-use/main/install.sh | sh`
- **Custom location:** `AGENT_BROWSER_BIN_DIR=$HOME/bin curl -fsSL … | sh`
- **Windows:** download `chrome-use-win32-x64.tar.gz` from the [Releases page](https://github.com/leeguooooo/chrome-use/releases) and put `chrome-use.exe` on your PATH.
- **npm (legacy):** `npm install -g chrome-use`. Still published, but GitHub Releases is the primary channel now.
</details>

### Install with Nix

Run it once, no install: `nix run github:leeguooooo/chrome-use -- --help`.
The flake also ships a home-manager module and a NixOS module (`programs.chrome-use.enable = true`); on NixOS the native-messaging host is registered per-user, so run `chrome-use extension connect` once after switching.
Dev shell: `nix develop` (rust toolchain + node 24 + pnpm + chromium + vhs).
Full snippets: [install guide](https://chrome-use.leeguoo.com/en/install.html).

### Install the AI agent skill

**Claude Code, plugin marketplace (recommended):** installs the skill globally (all projects), auto-updates, and lists the rest of the [`*-use` family](https://github.com/leeguooooo/plugins):

```
/plugin marketplace add leeguooooo/plugins
/plugin install chrome-use@leeguooooo-plugins
```

**Other agent runners (Cursor, Codex, custom):** pull the SKILL.md with [skills.sh](https://skills.sh). Add `-g` for a global install (visible to every project); drop it to install only into the current project:

```bash
npx skills add leeguooooo/chrome-use -g
```

> The `install.sh` one-liner above already runs this step for you (opt out with `AGENT_BROWSER_NO_SKILL=1`). Run it by hand only when you skipped the installer or use a non-default agent runner.

> **Codex users:** Codex ships its own browser plugin and picks it for browser tasks. Measured on a machine with many skills installed, Codex also trims every skill description to a few characters (or none), so the skill's description cannot win the routing, and naming `chrome-use` in the prompt was not enough either. What worked was one line in the project's `AGENTS.md`:
>
> ```text
> Use the `chrome-use` CLI from the shell for every browser task; start with `chrome-use skills get core`. Do not use the built-in Chrome plugin for browser work here.
> ```text

Either way the agent gets the right usage patterns and pre-approved bash permissions for `chrome-use` and `abs`; the skill self-heals a missing binary by re-running the `install.sh` one-liner above. Specialized guides (`electron`, `slack`, `agentcore`, …) are served by the binary itself via `chrome-use skills get <name>`, so instructions always match the installed version.

Upgrading the binary does **not** move a SKILL.md already copied into a runner; that copy lives outside the binary. Refresh it with `chrome-use skills update` (`refresh` and `install` are the same command; add `--project` to install into `./` instead of globally).

### Use from an MCP client (Claude Desktop, etc.)

For hosts that speak **MCP but can't run arbitrary shell commands** (Claude Desktop, ChatGPT connectors, n8n/Dify), run chrome-use as an MCP stdio server by wiring it into Claude Desktop's `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "chrome-use": { "command": "chrome-use", "args": ["mcp"] }
  }
}
```

## Setup: connect to your Chrome

Install the [**chrome-use** extension from the Chrome Web Store](https://chromewebstore.google.com/detail/chrome-use/knfcmbamhjmaonkfnjhldjedeobeafmk), then register the local bridge once:

```bash
chrome-use extension install      # register the native-messaging host (one-time)
chrome-use open https://x.com/home
chrome-use status                 # relay, profile, extension, and session health
```

`chrome-use open` then drives your real, logged-in Chrome over **native messaging**: no debug port, no token, and **no "Allow remote debugging?" dialog, ever**. The raw remote-debugging-port alternative (which pops a consent dialog) is described in the [real Chrome guide](https://chrome-use.leeguoo.com/en/real-chrome.html).

### Multi-profile Chrome: ChooseBrowser rules

If you keep several Chrome profiles (work, personal, a client's), you already know which account each site belongs to, and you tell us with `--browser` every time. [ChooseBrowser](https://choosebrowser.leeguoo.com) is a macOS link router that stores that mapping. When it is installed, `chrome-use open <url>` without an explicit `--browser` follows the rule you already wrote for that site, and says so:

```
$ chrome-use open https://github.com/my-org/repo
· using Chrome profile Profile 14 — a ChooseBrowser rule routes this site there
  (github.com|/my-org*). Override with --browser <id|email>, or skip with
  --no-choosebrowser.
```

Read-only, and invisible if you do not use it: no rules file means no behaviour change and no message. A rule naming a profile that is not running the extension falls back to the normal profile choice. That fallback does not verify the site account; check its identity or pin `--browser` when a specific account is required.

> **Disclosure:** ChooseBrowser is a paid macOS app (US$4.99, 7-day trial) by the same author as chrome-use. This is a companion-tool note, not an independent review. chrome-use needs none of it.

## Usage

The core loop: open, read, act, re-read only what changed.

```bash
chrome-use open https://example.com    # connect to your Chrome and navigate
chrome-use snapshot -i                 # the start of every interaction: interactive elements with @refs
chrome-use click @e3 --observe         # act, and watch for the page's reaction
chrome-use snapshot -i --diff          # only what changed since the last snapshot
```

The agent operates in your Chrome: you'll see tabs opening, pages loading, clicks happening in real time. You can take over at any point (e.g. solve a CAPTCHA), then let the agent continue.

| Command | Purpose |
|---|---|
| `chrome-use open <url>` | Connect to your Chrome and navigate |
| `chrome-use snapshot -i` | Read the page; the start of every interaction |
| `chrome-use click "Post"` · `click @e3` · `click 449 320` | Click by text, by snapshot ref, or on a raw viewport coordinate |
| `chrome-use fill "Title" "Hello World"` · `type @e3 "text"` | `fill` replaces a whole value and `type` appends |
| `chrome-use screenshot ./page.png` | Save a screenshot (an output for looking at, never the way an agent reads a page) |
| `chrome-use find "edit web service settings button"` | Ranked, non-acting candidates from a natural-language description |
| `chrome-use actions @e15` · `do @e15 expand` | What this element supports right now, and perform one of exactly those |
| `chrome-use tab list` · `tab select t2` · `tab adopt <url-substring\|targetId>` | List tabs; select a created or adopted tab; attach an already-open tab without navigating it |
| `chrome-use dialog status` · `dialog accept\|dismiss` | Handle a native `confirm()` / `prompt()` opened by a click |
| `chrome-use download @e2 ./video.mp4` | Download with the same cookies as the logged-in browser, without navigating the current tab |
| `chrome-use network route "*/api/me" --body '{"vip":true}'` | Mock a response, rewrite an outgoing request, or block one |
| `chrome-use site github/issues epiral/bb-browser --json` | Run a site adapter and get clean JSON from the site's own API |
| `chrome-use session list` · `session stop [name]` | Manage session workers |
| `chrome-use status` | Relay, profile, extension, and session health |

## Anti-detection

<img src="assets/shield.png" alt="stealth shield" width="320" align="right" />

When connected to your real Chrome, we inject **zero** JavaScript patches. Your browser's fingerprint is completely genuine. The guiding rule is **native CDP/Chrome overrides over JS lies**: a re-defined getter is itself detectable; a native override isn't.

- `navigator.webdriver = false` via `Emulation.setAutomationOverride` (native, undetectable by CreepJS-style lie tests).
- **`Runtime.enable` is left OFF by default.** A live `Runtime` domain is a detectable CDP signal (the patchright/rebrowser "runtime leak"), even when attached to your real Chrome. We only enable it when you opt into console/error capture. `click`, `fill`, `eval`, etc. work without it.

**Test results (connected to real Chrome):**

| Test site | Result |
|---|---|
| [CreepJS](https://abrahamjuliot.github.io/creepjs/) | **0% stealth · 0% headless** (no override traces at all) |
| [bot.incolumitas.com](https://bot.incolumitas.com/) | all checks OK: `overflowTest`, `overrideTest`, `puppeteerExtraStealthUsed`, worker consistency |
| [bot.sannysoft.com](https://bot.sannysoft.com) | all green |
| [BrowserScan](https://www.browserscan.net/bot-detection) | Webdriver · User-Agent · CDP all clean |
| [Cloudflare Turnstile](https://nowsecure.nl) | passed |

`0% stealth` on CreepJS is the key number: because the connect path patches **nothing**, there is no override for a lie-detector to catch. (Dashboards that read `navigator.languages` order or IP geolocation may show a soft "navigator"/"location" flag. That tracks *your real Chrome's* language list and network, not an automation tell.)

When using `--launch` mode (standalone browser), a full suite of stealth patches is applied instead, and it passes the suite above, with one caveat: CreepJS reports **~20% stealth** because the srcdoc-iframe `contentWindow` patch trips its `hasIframeProxy` probe (the proxy that hides automation is itself a tell). Everything else is clean (`0% headless`, sannysoft/browserscan green, Cloudflare passed). Set **`AGENT_BROWSER_DISABLE_IFRAME_PROXY=1`** to drop that patch for a clean **0% stealth** (trades the niche srcdoc-iframe masking). The **extension-connect path** (your real Chrome) injects zero JS and is unaffected; it's the genuine 0% path.

### Verify it yourself

Don't take our word for it. Point your connected Chrome at the toughest public detectors and compare:

- **[CreepJS](https://abrahamjuliot.github.io/creepjs/)**: the most thorough fingerprint / lie detector
- **[bot.incolumitas.com](https://bot.incolumitas.com/)**: behavioral + fingerprint scoring with a public methodology
- **[BrowserScan](https://www.browserscan.net/bot-detection)**: Webdriver / User-Agent / CDP / Navigator
- **[bot.sannysoft.com](https://bot.sannysoft.com)**: the classic automation-marker checklist
- **[pixelscan.net](https://pixelscan.net/)** · **[iphey.com](https://iphey.com/)**: consistency & identity

We deliberately **don't ship our own bot detector**. The strongest, most honest benchmark is the market's best detectors run against your real browser.

## More in the docs

- [Site adapters](https://chrome-use.leeguoo.com/en/site-adapters.html): turn a website into a structured-data CLI (`chrome-use site`)
- [Automated testing](https://chrome-use.leeguoo.com/en/testing.html): re-runnable YAML suites with `chrome-use test`
- [Accessibility audits](https://chrome-use.leeguoo.com/en/commands.html): axe-core via `chrome-use a11y`
- [Reading a page for fewer bytes](https://chrome-use.leeguoo.com/en/reading.html): `snapshot -i --diff`, `--max-bytes`, `--from`
- [Waiting before a read](https://chrome-use.leeguoo.com/en/waiting.html): settle detection, `--settle-ms`, `--with-screenshot`
- [Editing inside a field, and pasting with a MIME type](https://chrome-use.leeguoo.com/en/interacting.html): `select-text`, `paste --format html`
- [Actions beyond a click](https://chrome-use.leeguoo.com/en/interacting.html): `actions`, `do expand|showMenu|increment`
- [Finding elements and stable refs](https://chrome-use.leeguoo.com/en/finding.html): `find`, XPath, shadow-DOM refs
- [Downloads](https://chrome-use.leeguoo.com/en/interacting.html): `download`, `download-url`, `downloads`
- [Local HTTP API](https://chrome-use.leeguoo.com/en/http-api.html): the versioned `/api/v1` surface on each session's stream port
- [Network interception](https://chrome-use.leeguoo.com/en/network.html): `network route` to mock, rewrite, or block
- [Human-like input (humanize)](https://chrome-use.leeguoo.com/en/stealth.html): `--humanize off|fast|human`, adaptive anti-bot escalation
- [Silent operation](https://chrome-use.leeguoo.com/en/real-chrome.html): background tabs, never steals your foreground tab
- [Tuning knobs](https://chrome-use.leeguoo.com/en/commands.html): `AGENT_BROWSER_*` environment variables
- [Standalone mode (`--launch`)](https://chrome-use.leeguoo.com/en/real-chrome.html): a fresh isolated browser, `--profile auto` to keep your login
- [Tabs, dialogs, and sessions](https://chrome-use.leeguoo.com/en/commands.html): `tab duplicate|select|adopt|inspect`, `dialog`, `session handoff`
- [MCP server](https://chrome-use.leeguoo.com/en/mcp.html): `chrome-use mcp`, `--tools all`
- [Troubleshooting](https://chrome-use.leeguoo.com/en/troubleshooting.html)

<!-- use-family -->
## The `*-use` family

Small, composable CLIs that give an AI agent hands on one real thing. Same shape
everywhere: `curl … install.sh | sh` to install, `npx skills add leeguooooo/<name>`
to teach your agent, JSON on stdout.

| Repo | Gives your agent |
|---|---|
| [mail-use](https://github.com/leeguooooo/mail-use) | Email: read, search, send, triage across Gmail / QQ / 163 / any IMAP |
| [iphone-use](https://github.com/leeguooooo/iphone-use) | A real iPhone: tap, type, screenshot, pull on-device data |
| [wechat-use](https://github.com/leeguooooo/wechat-use) | WeChat on macOS: send messages, query contacts and history |
| [discord-use](https://github.com/leeguooooo/discord-use) | Discord: messages, channels, forums, webhooks (REST-only, Rust) |
| [cookie-use](https://github.com/leeguooooo/cookie-use) | Many logged-in accounts per site: capture, switch, apply sessions |
| [profile-use](https://github.com/leeguooooo/profile-use) | Your personal profile, safely: fill signup / KYC / checkout forms |
| [bitwarden-use](https://github.com/leeguooooo/bitwarden-use) | Bitwarden / Vaultwarden: headless passkey (FIDO2) login |
| [chatgpt-use](https://github.com/leeguooooo/chatgpt-use) | Your ChatGPT subscription as a coding-agent backend, no API key |
| [computer-use](https://github.com/leeguooooo/computer-use) | The macOS desktop itself |
| [pixcake-use](https://github.com/leeguooooo/pixcake-use) | Read-only PixCake probing: snapshot / diff / SQLite inspection |

## Contributing

`AGENTS.md` carries the conventions for this codebase: where docs live, how to
build and test, and two hard-won rules about never shipping a silent success and
about measuring performance honestly. Open work is tracked in
[issues](https://github.com/leeguooooo/chrome-use/issues).

Thanks to everyone who has contributed to chrome-use!

<a href="https://github.com/leeguooooo/chrome-use/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=leeguooooo/chrome-use" alt="Contributors" />
</a>

## License

Apache-2.0

---

> Built by **leeguooooo**. Field notes on AI agents, reverse engineering & Cloudflare Workers at **[blog.leeguoo.com](https://blog.leeguoo.com)** · follow on **[X @leeguooooo](https://x.com/leeguooooo)**
