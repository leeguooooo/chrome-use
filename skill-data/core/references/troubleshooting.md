# Troubleshooting

## Relay and debugger health

`status`, `extension status`, and doctor's relay check read `ABExt.state` over one diagnostic connection with a total 10-second budget. An error reply falls back to `ABExt.inspectTab` on that same connection. They send no debugger commands. The debugger summary passively counts answered commands (including non-timeout errors) and timeouts with “in the last 10 min” for a full reporting window, otherwise “since the extension worker started <age> ago” (whole seconds below 120 seconds, such as “45s”; whole minutes otherwise, such as “3 min”). No samples means “not exercised”; an older extension without health data reports that explicitly. Counts use bounded one-second buckets and reset when the service worker restarts. If commands keep failing, reload the chrome-use extension at `chrome://extensions` or restart Chrome.

New native hosts acknowledge diagnostic connections and skip `attachAll`. A still-running older host performs its usual `attachAll` once; the diagnostic notice recommends reloading the extension after upgrading to start the new host. Possible duplicate extensions are identified from sidecar identities and concurrent TCP liveness checks with a one-second limit and no WebSocket handshake. Matching account/browser with distinct extension ids does not prove a shared Chrome profile: disable one only if both are installed in the same profile.

JSON health contains only `transportResponsive`, `hostDiagnostic`, `debugger`, and `extensionHealth` (`windowMs`, `workerAgeMs`, `answered`, `timedOut`, `lastTimeout`, or null). `connectedProfiles` and `warnings` remain available. `extension status --json` retains `relayUp` as endpoint presence; `status --json` uses an extension transport reply. Update an older extension when a newer version is published; if the live extension is newer than this CLI expects, run `chrome-use upgrade`. Only `installType: development` identifies a development install. On macOS/Linux, doctor shows one-minute system load and CPU count; load above twice the CPU count warns and adds a hint to relay/CDP timeout errors.

**"Ref not found" / "Element not found: @eN"**
Page changed since the snapshot. Run `chrome-use snapshot -i` again,
then use the new refs.

**"Unknown ref @eN"**
The error names the session that answered and says whether it holds any refs.
"no snapshot has run in this session" means the command reached a different
session than the one you snapshotted (another directory or terminal): run
`chrome-use session list` and pin with `--session <name>`. If it lists a ref
range instead, the page changed; re-snapshot. Sessions are now reused across
`cd` for the same agent tag, so this mostly shows up with a different terminal.

**Element exists in the DOM but not in the snapshot**
It's probably off-screen or not yet rendered. Try:

```bash
chrome-use scroll down 1000
chrome-use snapshot -i
# or
chrome-use wait --text "..."
chrome-use snapshot -i
```

**Click does nothing / overlay swallows the click**
Some modals and cookie banners block other clicks. Snapshot, find the
dismiss/close button, click it, then re-snapshot.

**`stale sessionId … re-open your target URL` (extension-relay mode)**
Your tab may have been closed, navigated across processes, or its debugger detached
(e.g. it landed on a `chrome://` or Chrome Web Store page, which Chrome
forbids debugging). The session no longer has a live tab — re-run
`chrome-use open <your URL>` to re-attach, then retry. This loud error
replaces the old silent behaviour where the command ran on some *other*
tab and returned wrong data.

To recover, you need the tab's **exact** URL (query params and all — a long
SSO/redirect link breaks if truncated). `tab list` shortens long URLs with
`…`; use **`tab list --full`** to print them untruncated, then re-`open` the
right one. For multi-redirect SSO flows, re-open the **stable entry URL**
(not the mid-redirect one) and `wait` a few seconds for the SPA to settle
before snapshotting.

If the tab is still present but the page is white or frozen, do not reopen it
and destroy the diagnostic state. Select it only when `tab list` marks it
`created` or `adopted`; otherwise inspect its browser metadata directly with
`tab inspect <targetId>`, and use `tab adopt <url-substring|targetId>` before
selecting or driving it. A relay timeout means the renderer did not answer; it
does not mean the tab disappeared.

For an explicit `open`/`navigate`, ab-connect 0.5.18 and newer recover a
renderer-scoped `Page.navigate` timeout through Chrome's browser-level tab API.
The command returns a warning but keeps the same session and tab. Other methods
such as `eval` still fail while the page main thread is blocked. On reconnect,
the extension also validates every attached Chrome tab before re-announcing it,
so dead bootstrap `about:blank` records are dropped instead of becoming active.

`tab select` and `tab adopt` report one of three outcomes, and the third is
not a success:

- `✓ … verified: confirmed` — the liveness probe answered from the new tab.
  The switch happened; drive it.
- a warning naming a stale/closed target — the switch failed outright.
- `⚠ … verified: unconfirmed` — the switch was **requested** but never
  confirmed. The tab printed under it is what was *asked for*, not what
  answered. Do not treat the title/URL as a read of the live page.

An unconfirmed switch is not fixed by blindly repeating it. Re-read with
`snapshot -i` first; a background tab usually answers. Only when the user has
agreed to have their browser brought forward, try
`tab select <targetId> --activate` once (for a foreign tab,
`tab adopt <targetId> --activate`). Activation raises the user's Chrome window
and changes the tab in front of it; it happens before renderer initialization or
the liveness probe and leaves the tab in the foreground. Keep the same session and connection endpoint.
If new-tab initialization fails, use the retained target ID reported in the
error instead of repeating `tab new`. If reads still fail, preserve the target
and inspect it; do not automatically replay clicks or reload the page.
A method timeout alone does not prove that the relay or browser connection is
broken. Check `status` and `tab list` to distinguish connection health from a
page that did not answer. `tab inspect <ref>` reads browser-level target metadata over
the *browser* connection — it can succeed while driving that tab still fails,
so a successful inspect is not evidence the tab is drivable. An outdated or
unknown ab-connect version can also make the probe channel unavailable;
`tab inspect` requires ab-connect 0.5.16 or newer, so update or reload it from
`chrome://extensions` and retry.

**`tab list` marks the active tab with ⚠ instead of →**
The session still points at that tab, but the extension relay reports it
is not attached to it. Every command would run against nothing or against
whatever tab the relay does hold. Do not keep driving: `open <url>` the page
you need (a fresh attach), or `tab adopt <targetId>` for a tab that is still
open, and check that the next `tab list` shows → again. In `--json` the same
fact is `relayAttached: false` on the tab; when the key is absent the relay
could not be asked, which is not the same as detached.

**Reads landing on the wrong page**
`eval`, `screenshot`, and `network requests` print the page they ran
against to stderr: `eval @ <url>`, `screenshot @ <url>`, `network @ <url>`.
If that URL isn't the page you expected (the active tab drifted), re-`open`
your target URL — don't trust the result. Treat the stamp as a built-in
sanity check on every read.

`screenshot` also checks the pixels it just wrote. If the image is a single
flat colour while the page reports real layout and text, the capture is
reported with `⚠` and a warning instead of a plain `✓` — the shot is still
saved, but don't feed it to a vision model without confirming the target
first (`chrome-use eval "location.href"`, then re-pin with `tab` / `adopt`).
A page that genuinely is one colour never triggers this, and `--selector`,
`--clip` and `--annotate` captures are exempt.

**Fill / type doesn't work**
Some custom input components intercept key events. Try:

```bash
chrome-use focus @e1
chrome-use keyboard inserttext "text"    # bypasses key events
# or
chrome-use keyboard type "text"          # raw keystrokes, no selector
```

**Page needs JS you can't get right in one shot**
Use `eval --stdin` with a heredoc instead of inline:

```bash
cat <<'EOF' | chrome-use eval --stdin
// Complex script with quotes, backticks, whatever
document.querySelectorAll('[data-id]').length
EOF
```

**Cross-origin iframe not accessible**
Cross-origin iframes that block accessibility tree access are silently
skipped. Use `frame "#iframe"` to switch into them explicitly if the
parent opts in, otherwise the iframe's contents aren't available via
snapshot — fall back to `eval` in the iframe's origin or use the
`--headers` flag to satisfy CORS.

**Authentication expires mid-workflow**
Use `--session-name <name>` or `state save`/`state load` so your session
survives browser restarts. See [references/session-management.md](session-management.md)
and [references/authentication.md](authentication.md).

## Diagnosing install issues

If a command fails unexpectedly (`Unknown command`, `Failed to connect`,
stale daemons, version mismatches after `upgrade`, missing Chrome, etc.)
run `doctor` before anything else:

```bash
chrome-use doctor                     # full diagnosis (env, Chrome, daemons, config, providers, network, relay check or headless launch test)
chrome-use doctor --offline --quick   # fast, local-only
chrome-use doctor --fix               # also run destructive repairs (reinstall Chrome, purge old state, ...)
chrome-use doctor --json              # structured output for programmatic consumption
chrome-use stealth status             # stealth self-check: mode + live probes
chrome-use stealth status --json      #   (webdriver/chrome/plugins/UA) + applied
                                         #   overrides. Gate a sensitive flow on this
                                         #   instead of driving an external detector.
```

`doctor` auto-cleans stale socket/pid/version sidecar files on every run.
Destructive actions require `--fix`. Exit code is `0` if all checks pass
(warnings OK), `1` if any fail.

`status` requires a live extension reply within 10 seconds. It hides cached profile
and version data when the probe fails. It does not create a session or tab, and
browser-level health is distinct from renderer responsiveness.

The probe uses a target-free `ABExt.inspectTab` validation reply, including on
extensions predating the generic call API. Extensions without inspectTab cannot
confirm health. A timeout means no confirmation within the deadline, not proof
of a permanent disconnect. Opening the relay connection retains its existing
owned-target reannouncement behavior.

## Friction log and bug reports

Unexpected failures are logged locally by `chrome-use friction` (disable with
`AGENT_BROWSER_NO_FRICTION_LOG=1`). Nothing leaves the machine until a report
is filed with the user's OK.

**When to offer a report.** chrome-use got in your way: a command failed and
you worked around it (often with `eval`), an error sent you the wrong way, or
a feature you needed was missing. Finish the task first, then offer it in one
line. chrome-use nudges at those moments: a response carries
`reportSuggestion` (plus one stderr line) when the same failure repeats in a
session or an `eval` follows a failed command, and `close` mentions `report`
after three or more failures. Each nudge appears once per failure per session;
`AGENT_BROWSER_NO_REPORT_HINTS=1` turns them off.

**The flow.**

```bash
# 1. Draft. Sends nothing; prints the markdown, matching open issues, and
#    where it would go. Use the session you were driving.
chrome-use report --session <name> --note "export the orders table as CSV"
# 2. Show the draft to the user and ask. Only with their yes:
chrome-use report --session <name> --note "…" --submit --yes
```

- The draft comes from this session's failures of the last six hours (else
  the most recent ones); `--last N`, `--this-session` and `--any-session`
  change that. `--title` overrides the generated title. A `--note` alone is
  enough to report a missing feature.
- Redacted before printing: URL query strings and fragments, cookies, tokens
  and auth headers, typed values, emails, home-directory paths (shown as `~`),
  and anything shaped like a secret. Host names stay. Read the draft anyway;
  screenshots are never attached and nothing redacts pixels.
- Duplicates: the draft carries a signature (`cu-sig-…`, the command plus the
  error's stable words). If an open issue already has it, `--submit` posts
  "+1, also seen on <version, platform, mode>" there instead of a new issue;
  `--new-issue` files a separate one. The search uses `gh`, else the public GitHub
  API, else `site github/issues` in the user's Chrome.
- Filing uses `gh` when it is logged in, else the github.com new-issue form
  (or comment box, for a +1) in the user's logged-in Chrome, in a tab of its
  own that is closed afterwards, else it prints a prefilled new-issue URL for
  the user to open (the body is cut to fit). Not signed in to github.com in
  Chrome: the reply says so and falls back to the URL.
- Consent: `--submit` refuses without `--yes`, unless the user pre-approved
  with `AGENT_BROWSER_REPORT_AUTO=1` or `"report": {"auto": true}` in
  `~/.chrome-use/config.json`. Never pass `--yes` on your own judgement.
  `--submit --yes --dry-run` checks everything and sends nothing.
