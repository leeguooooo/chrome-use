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
- Many profiles? See [Choosing a profile](#choosing-a-profile) below.

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
for explicit isolation and reuse that name. `--browser <name>` pins a
profile. `browsers` lists Chrome profiles; `tab list` lists session tabs.
Adopt an existing user tab only when needed for the request. Do not close,
navigate, or reconfigure unrelated tabs or sessions.

## Choosing a profile

Each Chrome profile is its own login: one may hold the work GitHub account,
another the personal one. Pick the profile before the task, not after a login wall.

1. `chrome-use browsers` lists every profile: display name, directory,
   account, connected, default (used without `--browser`), and the one this
   session uses.
2. `chrome-use browsers --who <domain>` shows which profiles look signed in
   to a site. It reads cookie names from disk, never values, and opens no tab.
   "signed in" is a known login cookie; "session cookies present" only suggests a login.
3. Pin the session with `--browser <name>`. It accepts a display name
   (`Davian`), a unique name prefix (`d`), a directory (`"Profile 14"`), an
   email, or an id; `AGENT_BROWSER_PROFILE` takes the same values.
   A prefix that fits several profiles is an error listing them; use the directory.
   The session stays on that profile until closed.
   `open` and a session's first command print `profile: <name> (<dir>, <email>) — <why>`.
   `--json` returns the same as a `profile` field.
4. On a login wall, fix the login in that profile (`core/authentication`,
   `auth login --bwu`). Do not switch to another profile.

**Connecting another profile.** Only profiles running the extension are
connected; most people install it in one. `chrome-use connect --browser <name>`
connects one when it's needed. It opens the extension's Web Store page in that
profile, and the user clicks "Add to Chrome" once. If the extension is already
installed, it opens a window in that profile instead (the extension runs only
while the profile is open). If Chrome has it disabled, it opens its extensions
page. Then it waits for the relay. **It opens a window in the user's Chrome, so
ask the user before running it.** `--browser` naming an unconnected profile
fails with exactly this command.

**Defaults.** In `~/.chrome-use/config.json`:

```json
{"profiles": {"default": "Leo",
  "routes": [{"match": "dash.cloudflare.com", "profile": "Leo"},
             {"match": "github.com/acme/*", "profile": "Davian"}]}}
```

A session's first connect without `--browser` goes through these in order:

1. the first route matching the first `open` URL (host plus subdomains;
   the path is a prefix or `*` glob);
2. a ChooseBrowser rule;
3. `default`;
4. the most recently used profile.

The profile line names the rule that chose.

**A ChooseBrowser rule is binding.** chrome-use never opens a rule's site in
a different profile:

- If the rule's profile is not connected, the command fails, naming the rule
  and `chrome-use connect --browser <profile>` (opens a window in that
  profile, so ask the user first). It does not fall through to `default` or
  the focused profile.
- A running session never switches profiles. If it is bound to profile X and
  `open` / `goto` / `navigate` / `tab new <url>` targets a site whose rule
  names profile Y, the command fails. Use a new `--session` (it picks Y), or
  pass `--no-choosebrowser` to open it in X anyway.

`--browser` and config routes still win over a rule. A rule naming a profile
that no longer exists on this machine does not block anything: the command
runs with normal selection and prints one warning (`warning` in `--json`).
A rule whose key fits several profiles (one account signed in to two of
them) is refused, since picking one would be a guess.

The check covers every navigation, not just direct commands: each `batch`
step (a batch whose sites need different profiles is refused before any step
runs), MCP tool calls, and script steps. `chrome-use doctor`
lists each rule and whether its profile is connected.

**Every profile at once (opt-in).** Chrome's `ExtensionInstallForcelist`
policy installs the extension into every profile. `chrome-use extension
install` can write that policy. It needs a macOS configuration profile
approved by an admin, and it puts Chrome in "managed by your organization"
mode, which locks some settings (Secure DNS among them), so it is not the
default.

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
