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

Enabling **Stealth mode** or disabling **Agent cursor** in extension settings
queues removal of existing cursor hosts (including ripples) and `window.__abCursor`
after any in-flight draw. Cleanup uses existing debugger connections and checks
that the host and global are gone. Detached or unreachable tabs remain pending,
with a warning in settings and tab IDs in the relay's `ABExt.state.cursorCleanupPending`.
Reload affected pages before relying on stealth. Cleanup retries on normal
reattachment; changing the setting does not force a debugger attachment.

A restricted or detached child-frame command returns its error without detaching
the parent tab. Top-level recovery retries against the recovered tab ID, rather
than reusing an obsolete target. Re-read the page before retrying a failed frame action.
If a top-level action is interrupted after dispatch, the relay does not replay it
unless the command is safe to repeat or Chrome explicitly rejected it before dispatch.
`action_outcome_unknown` means the action may already have executed; JSON reports
`retryable: false`. Observe the current page before choosing another action.
`kept_ref_unverified` (JSON `retryable: false`, whatever cause it quotes) means a
ref kept after a failed post-action capture could not be confirmed live: nothing
was acted on and nothing is retried or recovered. Run `snapshot -i`.

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
ordinary actions do not force-front a tab. Emulated focus does not ensure frames from minimized Windows windows. `screenshot --allow-window-restore` is an explicit Windows relay exception: it restores the bound window to normal and leaves it restored, with possible focus changes; use it only within the user’s authorization.
`--humanize` adds human-like input; `chrome-use cf-status` checks/reuses Cloudflare
clearance.

Full detail: `chrome-use skills get real-chrome`

## Status, browser choice, and other people's tabs

`chrome-use status` reads extension state without a session daemon or debugger command. Stale relay files do not count as online. A healthy relay does not prove
that an individual page renderer responds; verify the intended page separately.

`status`, `extension status`, and doctor's relay check read `ABExt.state` over one diagnostic connection with a total 10-second budget. An error reply falls back to `ABExt.inspectTab` on that same connection. They send no debugger commands. The debugger summary passively counts answered commands (including non-timeout errors) and timeouts with “in the last 10 min” for a full reporting window, otherwise “since the extension worker started <age> ago” (whole seconds below 120 seconds, such as “45s”; whole minutes otherwise, such as “3 min”). No samples means “not exercised”; an older extension without health data reports that explicitly. Counts use bounded one-second buckets and reset when the service worker restarts. If commands keep failing, reload the chrome-use extension at `chrome://extensions` or restart Chrome.

New native hosts acknowledge diagnostic connections and skip `attachAll`. A still-running older host performs its usual `attachAll` once; the diagnostic notice recommends reloading the extension after upgrading to start the new host. Possible duplicate extensions are identified from sidecar identities and concurrent TCP liveness checks with a one-second limit and no WebSocket handshake. Matching account/browser with distinct extension ids does not prove a shared Chrome profile: disable one only if both are installed in the same profile.

JSON health contains only `transportResponsive`, `hostDiagnostic`, `debugger`, and `extensionHealth` (`windowMs`, `workerAgeMs`, `answered`, `timedOut`, `lastTimeout`, or null). `connectedProfiles` and `warnings` remain available. `extension status --json` retains `relayUp` as endpoint presence; `status --json` uses an extension transport reply. Update an older extension when a newer version is published; if the live extension is newer than this CLI expects, run `chrome-use upgrade`. Only `installType: development` identifies a development install. On macOS/Linux, doctor shows one-minute system load and CPU count; load above twice the CPU count warns and adds a hint to relay/CDP timeout errors.

Plain `open` connects through the extension relay after one-time extension
setup; `extension connect` reconnects explicitly. `--launch` uses an isolated
empty test profile that does not carry the user's login. Headed is the default.

Task session isolation is automatic when an agent/terminal identity is
available; otherwise it falls back to shared `default`. Use `--session <name>`
for explicit isolation and reuse that name. `--browser <name>` pins a
profile. `browsers` lists Chrome profiles; `tab list` lists session tabs.
Adopt an existing user tab only when needed for the request. Do not close,
navigate, or reconfigure unrelated tabs or sessions.

## Opt-in native focus recovery

`--allow-focus-recovery` authorizes one native Escape recovery for this invocation only. On Windows extension relay with extension 0.5.103 or later and a recovery-capable native host, a protected Bitwarden menu blocking debugger access may trigger a brief focus change to the bound agent window. Native foreground verification waits at most 500 ms after that single focus request; it never requests focus again. It requires an active agent-owned tab in a non-minimized window containing only agent tabs, verifies the same page and native menu, and attempts to restore the original foreground afterward.

It refuses an ambiguous target, unsupported platform, or changed user input. The default remains disabled; no environment variable, config, or reused daemon grants permission. The original operation runs only after a read-only probe confirms access; an operation with an unknown result is never replayed.

The invoking Windows CLI can request foreground eligibility for the verified Chrome parent after matching the CLI, daemon and native host to the same binary build. This grants no authority to the daemon and performs no activation or input. Windows may deny the request. `nativeFocus.foregroundAuthorization` distinguishes `Admitted`, `AlreadyForegroundProcess`, `OsDenied` and legacy `Unavailable`; admission alone proves neither focus nor key delivery. Update and restart mismatched native hosts before a new attempt; never retry an unknown-effect operation blindly.

JSON `nativeFocus` and warnings report input admission and restoration separately. OS input admission does not prove delivery, and restoration may remain unconfirmed. macOS currently provides only a read-only permission probe, without native focus recovery.

```bash
chrome-use --allow-focus-recovery click @e1
```

## Choosing a profile

Each Chrome profile is its own login: one may hold the work GitHub account,
another the personal one. Pick the profile before the task, not after a login wall.

1. `chrome-use browsers` lists every profile: display name, directory,
   account, connected, default (used without `--browser`), and the one this
   session uses.
2. `chrome-use browsers --who <domain>` shows which profiles look signed in
   to a site. It reads cookie names from disk, never values, and opens no tab.
   "signed in" is a known login cookie; "session cookies present" only suggests a login.
   On macOS, an unavailable PATH sqlite3 executable can fall back to
   `/usr/bin/sqlite3` (missing or unsupported-format startup errors only).
   Execution permission denials and started-query failures remain explicit;
   locked/corrupt stores are not retried through a different executable.
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
repeated and the error says so. For a background tab in a window with the
user's own tabs (where agent tabs open by default), it activates a blank tab in
its agent window instead: Bitwarden closes the menu of a tab that is not in
front whenever another tab is activated, so the user's window, its front tab
and the focus stay as they were. The agent window is kept minimized for the
session (one blank tab, closed with the session): the first time it is seen for
about a second while it minimizes, later times nothing shows. An agent window
that already holds other agent tabs (`--window dedicated`) is used as it is and
not minimized. A locked vault, or another password manager, may
not do that; the error then says so. A background tab is never brought to the
front on its own: the error gives the exact `extension call tabs.update`
command that does it (`tab select --activate` cannot, it needs the debugger
first).
Reattaching does not help. To avoid it, fill those fields with `fill` rather than
`type --key-events`, turn the extension's inline menu off for the site, or use a
`--launch` profile.

Relay navigation makes up to three bounded access checks while waiting for a
lifecycle event. A confirmed debugger access denial ends the wait early; a
successful check or a transient failure does not substitute for page readiness.
Fast pages can finish before any check is sent.

For isolated development, set `CHROME_USE_RELAY_DIR` to the same absolute
directory when installing the native host and running the CLI. On macOS/Linux,
`extension install` pins the installation's `HOME` and relay override in its
launcher, so Chrome cannot redirect that host into the ordinary registry by
starting it under another HOME. An install with no relay override generates a
launcher that clears any inherited override. Re-run the install with the intended
environment to refresh an older launcher. Private rigs should set
`CHROME_USE_NO_BROWSER_SCAN=1` and pass `--no-open --no-profile`; this registers
only the explicitly supplied test profile and does not open a browser. Combine
this with unique session names and an explicit `--browser` ID. Relative relay
paths are rejected before discovery. Omit the override for ordinary discovery.

Native agent-panel configuration journals are also scoped by this directory,
under `agent-panel/profiles/<profile-id>/configuration`. Without an override they
live under `~/.chrome-use/agent-panel`. This isolates journals, not the installed
agents' own settings. Fixture validation must also point each tested agent's
config-root variable at synthetic settings. Discovery does not read credentials.
Config planning reports a field diff without applying it; apply verifies source
identity and bytes, writes atomically and reads back. Restore preserves later
user edits and reports conflicts. A saved config does not prove a running session
changed model. If apply/restore loses its reply, inspect current values before
retrying; the native request is not automatically replayed.

Native conversation services also live under the profile's agent-panel data
directory. A service holds an OS ownership lock and uses a capability kept in a
private endpoint file; neither the endpoint nor its capability belongs in page
scripts. The native host confirms service health and the exact executable hash
before reusing it. An active service from another build is not silently replaced.

Codex uses app-server, Claude Code uses stream-json, OpenCode uses ACP, and Pi
uses JSONL RPC. The host advertises the available chat agents.
Their processes stay open between turns. Browser tools use the same chrome-use
binary with explicit `AGENT_BROWSER_BROWSER` and `AGENT_BROWSER_SESSION`, plus
the relay registry override when present. No separate browser transport is
introduced for displaying progress. Reconnect retrieves journal events by cursor
without sending an old message again. A repeated message ID is idempotent, and
reusing it with different text fails. Failure/interruption pauses queued messages
until explicitly resumed. In the side panel, a new message after a confirmed end starts immediately in the same conversation and model via conversationSend, even while older queued messages stay paused. The host checks the live phase under its lock: if a newer turn is already busy, an ordinary send queues without interrupting it. Capacity errors do not trigger automatic replay or a model switch. A stop remains pending until backend terminal output
or owned-process exit is confirmed. An unconfirmed process or recovered active
turn requires inspection; it is not marked successful or automatically replayed.

Synthetic protocol fixtures verify lifecycle behavior without using an account
or Chrome. They do not establish real CLI login, model access, browser acceptance,
Windows process-tree behavior or unchanged detector results.

The toolbar side panel requires Chrome's native sidePanel API. The worker reads
back open-on-action behavior before removing the connection popup; failure keeps
that popup available. Settings in the panel opens the existing options page.
Conversation text and tool data render as text, not HTML. Reconnect reads the
journal cursor; an uncertain send queries conversationMessageStatus for the
original message ID and does not interpret absence as rejection or resend it.
Selection changes discard old read/control replies, and drafts remain scoped to
each conversation while the panel stays open. Tool rows say Returned or Error
returned, not that the requested browser effect was verified. Queue resume is
explicit. Agents & models offers allow-listed field previews and native verified
apply/restore; installation evidence is separate from authentication/model access.


conversationRead returns bounded queued-message previews. A truncated preview
is not editable text: conversationQueueMessageRead reads one full message while
confirming it is still queued. conversationQueueEdit and conversationQueueRemove
compare expectedText with the stored original under the service lock. Edit
assigns a fresh immutable replacement ID; both original and removed IDs remain
in the acceptance map so delayed sends cannot restore old queue entries.
A changed/dequeued message is refused. Queue events keep transcript audit notes;
they do not claim the message ran. Editing keeps the composer draft intact.
Lost mutation replies are unconfirmed and never automatically retried.

conversationSendDirect accepts a durable message ID and records pending direct
intent before requesting interruption. Its ACK confirms acceptance, not stopping
or execution. The old turn must complete/interrupt or the owned process must
exit before begin_next can select that message ahead of the ordinary queue.
Ordinary messages stay paused after the direct turn. A failed terminal result
requires explicit resume; processUnconfirmed or recovered busy state refuses
resume entirely. Repeated IDs are deduplicated and a second pending direct ID
is refused. Queue edit/remove can replace or cancel the pending intent before
execution. The native service can restart the CLI after confirmed exit, retaining
its backend session identity; startup failure reports recoveryRequired rather
than dropping the selected message or pretending it ran.

agentInspect runs the selected chat-agent executable in private scratch
directories, separately from installation-only discover. It checks version
output and reads Codex model/list/account/read, Claude initialization metadata,
OpenCode ACP configOptions, or Pi get_available_models. OpenCode creates a
transient metadata session and deletes it before returning success. No prompt
or browser task is sent. Codex cursor pages are bounded; remaining pages mark
the list partial. Model identifiers, labels and supported reasoning efforts
are allow-listed. Account state is reported metadata, not verified login;
OpenCode and Pi return unverified. Email, organization and credentials are not
transported; modelAccessVerified stays false. Two concurrent probes, bounded
output/deadlines and owned-process exit checks apply. The panel discards stale
selection results and does not substitute a static catalog after failure.
Inspections use owned Unix process groups or Windows jobs; all descendants
must exit before completion is reported.

The native hello reply advertises agentPanel schemaVersion/mode/modelInspection
capabilities. Port liveness alone does not enable the panel; an older host without
a compatible advertisement requires upgrade. Capabilities reset on reconnection
and stale ports cannot change them. Opening while connecting automatically loads
agent/history metadata after host confirmation. macOS, Linux and Windows advertise full mode, with the same conversation,
configuration transaction, skills, images, options and schedule operations.
Legacy Windows hosts may still advertise readOnly and require a CLI upgrade.
Windows launches agent jobs without a console window, confirms their process
trees are empty on stop/delete/options changes, and protects private state with
a user/SYSTEM ACL. Atomic configuration replacements preserve existing ACLs
and use real Windows file identities. Native executables and installed .cmd/.bat
entry points are supported; no administrator access is required.

Browser-tool approvals show the requested arguments and allow only a single call. Claude permission requests remain pending until a matching tool result arrives; writing a reply alone does not confirm resolution.
Codex chrome_use MCP approvals accept only the known empty-form call schema;
unknown forms can be declined or cancelled. Claude uses stdio permission prompts.

An unconfirmed message has explicit Retry same message and Clear send warning
controls. Retry keeps the exact ID/text and direct-send intent; reopen preserves
only the ID, so retry is disabled when the text is unavailable. Clearing the
warning does not cancel a possibly delivered task. No automatic resend occurs.
Idle older services are replaced only after endpoint removal and OS-lock release.
Read/stop operations remain available against the old service; busy services are
never automatically killed. Custom metadata executables must be absolute paths.
Journal loading validates the last sequence in one pass; replay uses bounded
byte-cursor caches, invalidated when an external file change is observed.

### Side panel controls

On macOS, Linux or Windows, select Codex, Claude Code, OpenCode or Pi from the
host-advertised agent menu. Use the installed CLI's own login
and provider configuration. The model menu reads that CLI's catalog; Default
model uses its configured model. A Custom model ID in the ＋ menu overrides the
menu choice; Reload models from CLI refreshes metadata, not execution access.

The page tag above the composer attaches the user's current webpage to the next
message. Its tab id, URL and title tell the agent which tab “this page” means.
Click × to omit it. A chat still has a separate browser session; an attached
user tab is a specific target, not a transfer of all user tabs into that session.

Full access is per conversation and starts enabled for new chats unless a
previous choice was saved. Codex/Claude use Ask first when disabled and run
without permission prompts when enabled; supported approval requests are
answered automatically, while questions still require user input. OpenCode's
CLI permissions preserve its own rules, which may already allow some tools;
full access answers incoming permission requests with allow_once. Pi never
asks before tools: Browser only exposes chrome_use MCP tools without shell or
file tools; full access enables the CLI's full tool set.

Stop a running reply before confirming deletion from the header. Deletion
removes the panel journal/workspace and the conversation's browser session.
Codex rollout files are matched by session id in sessions/ and archived_sessions/;
Claude files/directories are matched in projects/; their configured roots are
honored. OpenCode deletion uses the advertised ACP session/delete API. Pi's
session files live inside the conversation workspace and are removed with it.
Codex's SQLite cache is not edited. Cleanup errors and unconfirmed browser-tab
closure are reported rather than treated as complete deletion.

Panel history stays in the profile-scoped local journal. The chosen CLI may
send tasks and page observations to its configured model provider; that
provider's privacy policy applies. The extension does not contact a model
server directly or manage the CLI's authentication credentials.

## Side panel skills, images and scheduled prompts

The panel also supports:

- **Skills:** type `/` to choose a bundled, user or profile-library skill. **Agents & models → Skill library** edits library entries, enables/disables them, and imports an absolute local folder, ZIP/.skill archive or HTTPS Git repository. User and CLI skill folders are read-only here. A selected skill and its assets are snapshotted before delivery, so queued messages and explicit retries retain the selected revision. Panel-bound agents can use `chrome_use_skills` to list/read skills, read selected relative text assets and save library instructions when asked.
- **Images:** paste, drop or choose up to five PNG/JPEG/WebP files with **Attach images**. Images are reduced to a 2000-pixel long edge and at most 512 KiB each before native transport. They stay with the conversation, including queued-message edits and retries, and are removed with it. ACP agents must advertise image input; unsupported input fails instead of dropping the images. The model service receives the attachments.
- **Conversation options:** choose allowed tools and, for Codex/Claude, reasoning effort; Codex also supports a service-tier override. Save while idle. Overrides take effect on the next response and do not edit CLI defaults. Codex can restrict writes but cannot remove its file-reading shell. OpenCode ACP does not enforce built-in tool restrictions; its own permission rules still apply. The approval switch controls automatic answers within the selected tool set; Pi has no per-call approvals.
- **Scheduled prompts:** **Agents & models → Scheduled prompts** saves intervals, daily/weekday local times or five-field cron, with agent, model, options, skill and explicit tool scope. **Run now** does not move the next scheduled time. Runs use independent background browser sessions and remain in history; completion notifications link to their conversations. The host-local service must be running; closing the panel does not stop it. On restart or wake, missed periods coalesce into one run. An active or unconfirmed run is never overlapped or replayed; inspect the history before acknowledging an unknown outcome. A chat cannot schedule more tools or automatic approval than it has, and scheduled conversations cannot mutate or trigger schedules or rewrite library skills for future runs. Native acceptance and agent completion remain separate from verifying the requested page effect.

Library imports never execute imported code. Names use lowercase letters,
digits, hyphens or underscores. The entry file is limited to 96 KiB; an asset
is limited to 4 MiB, and a skill to 16 MiB. Symbolic links in imported assets
are refused. Git imports accept HTTPS repository URLs without embedded
credentials; archives must use contained paths and regular files. Only the
profile's library is writable. Selected skill assets can be read through the
panel-bound skill MCP tool even when shell/file tools are unavailable.

Codex `readFiles` uses `read-only`, `writeFiles` uses `workspace-write`, and
`runCommands` uses `danger-full-access`. Claude/Pi receive explicit tool lists
for restricted modes; their file tools do not constitute an OS sandbox.
OpenCode supports the unrestricted built-in tool set with its CLI permission
rules. Unsupported restrictions and options return errors.

A schedule's rules use `{kind:"interval",minutes:60}`, `{kind:"daily",time:"09:00"}`,
`{kind:"weekdays",time:"09:00"}` or `{kind:"cron",expression:"0 9 * * 1-5"}`.
Cron accepts lists, ranges, steps and English month/weekday abbreviations.
When both day fields are restricted, either matching day runs. A nonexistent
local time during daylight-saving transition is skipped.

Cross-adapter scheduling also preserves sandbox and approval boundaries.
A Codex workspace-write conversation cannot schedule Claude/Pi unrestricted
file writes. Pi's tools always run without per-call approval; a conversation
that requires approvals cannot schedule automatic Pi work, regardless of its
saved fullAccess flag.

## Remote Linux browser acceptance

Chrome may reduce `navigator.languages` to its first preferred language for privacy. With `AGENT_BROWSER_LOCALE=ja-JP`, the stealth benchmark accepts matching page/worker lists of `ja-JP` or `ja-JP,ja`; wrong locales, disagreement and missing results still fail. It does not disable Chrome's language reduction. Native GPU availability and site challenge outcomes must be measured on the actual host; other detector failures still make the benchmark fail.

Run the CLI and browser on the SSH host. Prefer headed Chrome on a real or virtual display (for example Xvfb); the default headed launch preserves native CPU, GPU, screen, UA Client Hints and iframe functions across pages and workers. Keep the CDP port on loopback if attaching over an SSH tunnel. Configure the display and browser executable on that host explicitly.

`AGENT_BROWSER_ALLOW_HEADLESS=1` permits a compatibility fallback, not an undetectable browser. Headless page UA patches cannot conceal Worker or Service Worker fingerprints. `stealth status` includes `headless`, `scope` and `warnings`, and marks a known headless launch as needing attention even when the basic page probes pass. Recheck public detectors on the actual host; cloud-provider IPs, graphics support, fonts and site policies still affect results. Do not infer Cloudflare or real-account acceptance from one detector.

Native CDP IME input can produce an untrusted `compositionend`. Keep that evidence when reporting a detector pass; do not rewrite event trust or dispatch synthetic events to conceal it.
