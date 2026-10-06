# Connecting to Chrome

## Driving the user's real, already-open Chrome (extension)

Use the **extension connect** flow to drive the user's *live*, logged-in Chrome
window (their real session) rather than a fresh browser. One-time setup:
`chrome-use extension install` + install the **chrome-use** Web Store extension.
After that, plain `chrome-use open <url>` auto-connects through the relay (so
Chrome's "Allow remote debugging?" popup never fires); `chrome-use extension
connect` (alias `reconnect`) is the explicit form, and the CLI self-heals
transient relay drops — usually just retry the command.

The extension popup shows Connected only after a reply from the native host.
While confirmation is pending it shows Connecting; missing-host errors are shown
verbatim. With an older host that does not answer the initial ping, the first
real CLI command can confirm the connection. This status confirms the host link,
not that every page or frame is drivable.

A restricted or detached child-frame command returns its error without detaching
the parent tab. Top-level recovery retries against the recovered tab ID, rather
than reusing an obsolete target. Re-read the page before retrying a failed frame action.
If a top-level action is interrupted after dispatch, the relay does not replay it
unless the command is safe to repeat or Chrome explicitly rejected it before dispatch.
`action_outcome_unknown` means the action may already have executed; JSON reports
`retryable: false`. Observe the current page before choosing another action.

- **`--launch`** opens an isolated, empty test profile (no cookies/login/extensions,
  relay off) — use when a clean browser is fine. On macOS this path disables
  Chrome's code-sign clone so interrupted automation sessions do not leak disk.
- **`--profile auto`** (or `AGENT_BROWSER_PROFILE=auto`) reuses the user's real
  Chrome profile — real cookies, login, extensions.
- Many profiles? `chrome-use browsers` lists connected ones; `--browser <id|email>`
  pins this session to one (sticky per session).

**Per-agent isolation is automatic:** each `--session <name>` gets its own colored
Chrome tab group + dedicated daemon and drives only tabs it created or explicitly
adopted, so
concurrent agents share one real Chrome without cross-talk and never touch
unadopted user tabs. With no `--session` / `AGENT_BROWSER_SESSION`, the name is
derived as `cu-<repo>-<tag>`, where `<tag>` hashes the first of these that is
set: an **agent** id (`AGENT_BROWSER_SESSION_ID`, `OPENCODE_PID`,
`CODEX_THREAD_ID`, `CMUX_SURFACE_ID`, `CMUX_CLAUDE_PID`, `CLAUDE_PID`), then a
conventionally-named one, then a **terminal** id (`TERM_SESSION_ID`,
`ITERM_SESSION_ID`, `TMUX_PANE`, `WT_SESSION`, …) — agent ids outrank terminal
ids, because two agents in one terminal tab share the terminal's. With none of
them set it falls back to the shared `default`. A command run from another
directory reuses the live daemon carrying the same tag, so tabs and refs survive
`cd` (explicit `--session` still wins). `chrome-use doctor` prints the name and
the variable it was keyed on. `adopt
<url|targetId>` drives a pre-existing tab on demand; OAuth/SSO popups and
cross-process redirects are followed automatically.

**Anti-detection ranking: real logged-in Chrome (extension connect) > headed launched
browser > headless (forbidden).** **Silent by default** — new tabs open un-focused and
the agent never force-fronts a tab (emulated focus keeps the page rendering).
`--humanize` adds human-like input; `chrome-use cf-status` checks/reuses Cloudflare
clearance.

Full detail: `chrome-use skills get real-chrome`

## Status, browser choice, and other people's tabs

`chrome-use status` probes the extension for up to 10 seconds without a session
daemon. Stale relay files do not count as online. A healthy relay does not prove
that an individual page renderer responds; verify the intended page separately.

Plain `open` connects through the extension relay after one-time extension
setup; `extension connect` reconnects explicitly. `--launch` uses an isolated
empty test profile that does not carry the user's login. Headed is the default.

Task session isolation is automatic when an agent/terminal identity is
available; otherwise it falls back to shared `default`. Use `--session <name>`
for explicit isolation and reuse that name. `--browser <id|email>` pins a
profile. `browsers` lists connected profiles; `tab list` lists session tabs.
Adopt an existing user tab only when needed for the request. Do not close,
navigate, or reconfigure unrelated tabs or sessions.

An empty tab list or one stale tab is not proof the browser disconnected.
Read the error before restarting anything. For setup/version failures, use
`doctor --offline --quick`; load `core/troubleshooting` for diagnosis.


Chrome refuses debugger access to a web tab while another extension's frame is
in it, most often a password manager's inline autofill menu (Bitwarden,
1Password, ...) that opens next to a focused login or card field. The error
names the password managers installed in this Chrome. The menu closes when the
tab is hidden and shown again. For a tab the session created that is in front,
chrome-use does that itself (a blank tab for a moment) and repeats the command if
repeating it is harmless (a read, `fill`, `select`); a click or key press is not
repeated and the error says so. A background tab is never brought to the front
on its own: the error gives the exact `extension call tabs.update` command that
does it (`tab select --activate` cannot, it needs the debugger first).
Reattaching does not help. To avoid it, fill those fields with `fill` rather than
`type --key-events`, turn the extension's inline menu off for the site, or use a
`--launch` profile.

Relay navigation makes up to three bounded access checks while waiting for a
lifecycle event. A confirmed debugger access denial ends the wait early; a
successful check or a transient failure does not substitute for page readiness.
Fast pages can finish before any check is sent.

For isolated development, set `CHROME_USE_RELAY_DIR` to the same absolute
directory in the native-host launcher and the CLI. This scopes relay discovery
without changing HOME; combine it with unique session names and an explicit
`--browser` ID. Relative paths are rejected before discovery. Omit it for ordinary shared-profile discovery.
