---
name: agent-browser-stealth
description: Stealth-first browser automation for OpenClaw using agent-browser-stealth. Use when tasks involve bot-protected websites, anti-fingerprint evasion, captcha-prone flows, login persistence, region-sensitive targets (e.g., Shopee/TikTok/e-commerce), or any request to automate web actions with lower detection risk.
homepage: https://github.com/leeguooooo/agent-browser
---

# agent-browser-stealth for OpenClaw

Use this skill when the task needs web automation and anti-bot stability.

## What this skill prioritizes

- Use `agent-browser` CLI from `agent-browser-stealth` package
- Prefer stealth-safe interaction patterns over brittle one-shot scripts
- Keep command flow deterministic: `open -> snapshot -> act -> re-snapshot`
- Minimize bot signals with humanized pacing and stable session reuse

## Install and baseline

```bash
npm install -g agent-browser-stealth
agent-browser install
agent-browser --version
```

If default CDP mode is used in your environment, ensure a browser is available at `localhost:9333`, or pass `--cdp` / `--auto-connect` explicitly.

## Standard execution workflow

```bash
agent-browser open <url>
agent-browser wait --load networkidle
agent-browser snapshot -i
# choose refs (@e1, @e2, ...)
agent-browser click @eN
agent-browser fill @eM "..."
agent-browser snapshot -i
```

Use refs (`@e1`) from snapshot output whenever possible.

## Anti-bot operating rules

1. Prefer headed mode for sensitive targets:

```bash
agent-browser --headed --session-name shop open https://example.com
```

2. Reuse session state to avoid repeated cold-start fingerprints:

```bash
agent-browser --session-name shop open https://example.com
```

3. Keep interactions human-like:

```bash
agent-browser type @e2 "query" --delay 120
agent-browser wait 1200-2600
```

4. For contenteditable editors, use keyboard mode:

```bash
agent-browser click "[contenteditable='true']"
agent-browser keyboard type "Hello world" --delay 90
```

5. If text must literally include `--delay`, stop arg parsing with `--`:

```bash
agent-browser type @e2 -- "--delay 120"
agent-browser keyboard type -- "--delay 120"
```

## Region-sensitive websites

For region-bound sites, open target domain directly and let locale/timezone alignment apply.

```bash
agent-browser open https://shopee.tw
```

Only override locale/timezone when explicitly required by the task.

## Recovery patterns

If blocked or unstable:

1. Retry with `--headed`.
2. Reuse `--session-name`.
3. Slow down action cadence (`wait`, `type --delay`).
4. Re-open page and regenerate refs with `snapshot -i`.

## Minimal recipes

Login flow:

```bash
agent-browser --session-name account open https://example.com/login
agent-browser snapshot -i
agent-browser fill @e1 "$USERNAME"
agent-browser fill @e2 "$PASSWORD"
agent-browser click @e3
agent-browser wait --url "**/dashboard"
```

Search and capture:

```bash
agent-browser open https://example.com
agent-browser snapshot -i
agent-browser type @e2 "iphone" --delay 120
agent-browser press Enter
agent-browser wait --load networkidle
agent-browser screenshot result.png
```

## Output expectations for OpenClaw

When using this skill, return:

- Exact commands executed
- Key page state changes (URL/title/important element text)
- Any anti-bot signal encountered and mitigation used
- Next safe action
