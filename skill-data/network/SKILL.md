---
name: network
description: Mock responses, rewrite outgoing requests, block URLs, and record HAR with `chrome-use network route` (CDP Fetch, no proxy). Use when a task needs to stub an API response, override request headers/body, rewrite a URL to staging, block analytics, or capture/inspect network traffic.
allowed-tools: Bash(chrome-use:*), Bash(chrome-use:*), Bash(abs:*), Bash(npx chrome-use:*), Bash(npx chrome-use:*)
---

# chrome-use — Network Interception

Intercept, mock, rewrite, block, and record network traffic on the page you're driving — all via the CDP Fetch domain (no proxy, no extra extension permission).

## Mock responses & rewrite requests

`network route` intercepts matching requests via the CDP Fetch domain (no proxy,
no extension permission). Three modes, verbs/fields mirror Playwright:

```bash
# Mock the RESPONSE (fulfill — the request never leaves the browser)
chrome-use network route "**/api/users" --body '{"users":[]}' --status 200 --content-type application/json
chrome-use network route "**/api/me" --body '{"vip":true}' --header X-From=mock

# Rewrite the outgoing REQUEST (continue with overrides — real response returns)
chrome-use network route "**/api/save" --method POST --set-header Authorization="Bearer test"
chrome-use network route "**/api/save" --set-body '{"x":1}'          # replace the request body
chrome-use network route "**/v1/*" --rewrite-url https://staging.example.com/v1/thing

# Edit the REAL response (request goes out, response comes back, then tweak it)
chrome-use network route "**/api/me" --edit-status 503                # override status
chrome-use network route "**/api/me" --edit-header X-Env=test         # add/override a response header
chrome-use network route "**/api/me" --replace 'prod=>staging'        # substring-replace in the real body (repeatable)

# Block entirely
chrome-use network route "**/analytics" --abort

# Scope any rule to resource types, and inspect / record traffic
chrome-use network route "**/*.json" --abort --resource-type xhr,fetch
chrome-use network requests --clear        # start capturing fresh
chrome-use network requests                # inspect what fired
chrome-use network har start               # record all traffic
# ... perform actions ...
chrome-use network har stop /tmp/trace.har
chrome-use network unroute                 # drop all routes
```

Three modes, distinguished by which flags you pass:
- **Mock** (`--body` / `--status` / `--header` / `--content-type`): fulfill a fake
  response, short-circuit the request (never hits the network).
- **Rewrite request** (`--method` / `--set-body` / `--set-header` / `--rewrite-url`):
  change the outgoing request, then let the real response come back.
- **Edit real response** (`--edit-status` / `--edit-header` / `--replace 'from=>to'`):
  let the request go out, then tweak the real response before the page sees it.
  Great for forcing an error state or patching a field without a mock server.

If both a mock and a rewrite are given on one rule, the mock wins. `--header` /
`--set-header` / `--edit-header` / `--replace` are repeatable; request/response
headers merge over the originals. Note: editing the **top-level document**
navigation's status can confuse page load — scope response edits to API calls
with `--resource-type xhr,fetch` when needed.
