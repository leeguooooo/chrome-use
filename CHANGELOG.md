# Changelog

## 1.5.123

<!-- release:start -->
### Bug Fixes

- **v1.5.122's fix for #319 did not work; this is the real one.** `status` prints "driving A" above "profile: B" with two different ids. v1.5.122 moved the extension version into a per-profile file but still looked it up with the wrong id, so the same mistake simply entered somewhere else and the symptom survived — one `status` run after upgrading showed it unchanged. The actual defect is larger than the original diagnosis: "which profile is driving" had **two unrelated implementations**. The one that decides which browser is actually bound picks by window focus; the ones that report it to you — the `profile:` line, `drivingProfileId`, `browsers`' default column, `doctor`, and the version resolver — all used the other one, "whichever worker connected last". With two profiles connected those disagree. They are now a single resolver, preferring the focus-based answer that the endpoint selection actually uses and falling back to the generic sidecar only where the focus answer is deliberately absent (fewer than two profiles, an extension too old to report focus, or a tie) — which are exactly the cases where the generic one is right.
- **Honest limitation: this is not verified.** The symptom only appears with two Chrome profiles connected, and that environment was not available. What is established is that the two notions are unified in code and the suite passes — *the same evidence v1.5.122 offered before turning out to be ineffective*. #319 is therefore left open rather than closed. If you have two profiles, run `chrome-use status` and check whether the `driving` and `profile:` lines name the same id.

### Contributors

- @leeguooooo
<!-- release:end -->

## 1.5.122

### Bug Fixes

- **With two Chrome profiles connected, `status` reported the other profile's extension version (#319).** It printed `driving 27ade1bc-…` and `extension: live 0.5.21, expected 0.5.26` together, where the `0.5.21` belonged to a different profile — so the reader is told to update an extension that is already current. The cause was a missing dimension rather than a race: the version sidecar is a single file written by whichever worker sent `hello` last, while "which profile is driving" is decided separately by focus timestamp. #60 had already given the *endpoint* per-profile treatment for exactly this reason ("regardless of who last clobbered the generic file"); the version never followed. It now has its own per-profile sidecar, written next to the endpoint on `hello` and removed with it on disconnect, and the places that print a version next to a profile read the driving profile's copy. An extension too old to report a profile id still writes only the generic file, so those keep working rather than degrading to "unknown". CLI-side only — no extension update needed.

### Contributors

- @leeguooooo

## 1.5.121

### Features

- **`keep` now says why a tab was left behind, and can be undone (#290).** `keep` exempts a tab from the shutdown sweep by *dropping ownership*, which erased the only record we had — afterwards the tab was indistinguishable from one we never touched, so `tab list` showed it as `foreign` and could not answer "why is this still open?". It now takes a reason: `keep --as deliverable` for a tab that IS the result (an edited document, a checkout the user must finish), `keep --as handoff` for one a later turn resumes from (waiting on a login, an approval, a code). Bare `keep` still means `deliverable`, so existing callers are unchanged. `tab list` marks them `[kept: deliverable]` / `[kept: handoff]`. `keep --release` takes a tab back so it closes with the session again — and only works on a tab this session created and then kept, because the recorded keep is the proof: without it the command would claim the right to close a tab it never opened. The tab does not rejoin the session's tab group; ungrouping is one-way today, and the message says so rather than implying otherwise. `keep` also gained its own `--help`, which it never had.

### Bug Fixes

- **The per-KB cost quoted in the insert-timeout hint is now the measured one.** It said `~0.45-0.53s/KB`, from an early small sample. Measured on chatgpt.com: 60 KB took 37s (0.62s/KB) and 90 KB took 88s (0.98s/KB). The point is not that the constant was low — it is that the per-KB cost **rises with size**, so quoting a flat range invites extrapolating from it, which is exactly how a "roughly 226 KB" ceiling got published and then corrected. The hint now gives the measured range and says the cost rises.

### Contributors

- @leeguooooo

## 1.5.120

### Bug Fixes

- **The insert-timeout hint no longer recommends the one thing that corrupts text.** It told the caller to "split the text and send it as separate `keyboard inserttext` calls" — the exact pattern #301 was filed for: a call returns when Chrome dispatched the insert, not when the editor committed it, so the next piece races the uncommitted tail and scrambles the text while preserving the total length, which is why a character-count check passes and the damage ships. The repo already said this in `commands.rs`; the error message said the opposite. If a split is mentioned at all, the hint now says to compare the field's **content** between pieces, never just its length.
- **The hint now says the insert was not cancelled.** Losing the timeout race cancels nothing: the command was already dispatched before the race began, and CDP has no primitive to recall it. A renderer was measured still working roughly 14 minutes after the caller got the error, with memory still climbing — so a command sent right after the failure lands on a tab that is still busy. The hint now says to let the tab go quiet and re-read the field first, because some or all of the text may have landed (#315).
- The per-character cost quoted in the hint is now `~0.45-0.53s/KB`, the range actually measured, rather than the single early figure.

### Contributors

- @leeguooooo

## 1.5.119

### Bug Fixes

- **A large insert no longer dies at ~55s looking like a dead session.** Three layers budget the same CDP command — the extension (8s + 2ms/byte, capped at 120s), the daemon (30s + 4ms/byte, capped at 180s), and the client's socket read. v1.5.117/118 scaled the first two by payload but left the read budget flat at 45s, so the outermost layer cut first and the inner, more specific "the payload needed more time" error never reached the caller. That flat 45s plus the 8s `DAEMON_SHUTDOWN_GRACE` is exactly the ~55s wall reported from live use (measured 53.6s / 54.0s / 55.6s). The read budget now scales by the same rule, and a test pins it against the budget the CDP client actually enforces, so the two cannot drift. Commands without a payload keep the flat 45s, so a genuinely hung session still fails just as fast. The real ceiling is now the extension's 120s cap. Measured on chatgpt.com it falls between **90 KB and 120 KB** (60 KB passed in 37s, 90 KB in 88s, 120 KB hit the cap). Note the per-KB cost *rises* with size — 0.62s/KB at 60 KB, 0.98s/KB at 90 KB — so extrapolating from one small sample overestimates the ceiling, which is exactly what an earlier draft of this entry did.
- **The "session unresponsive" message no longer guesses at a cause.** It used to assert the page was still finishing a long command. That fit one reproduction and not another (60 seconds versus over an hour), and the mechanism was never observed. It now states only what was observed — the name has been seen to free up on its own, so it is not permanently taken — and gives a move that works immediately: use a different `--session` name, or `adopt` the tab into a fresh session.

### Contributors

- @leeguooooo

## 1.5.118

### Bug Fixes

- **The daemon half of the payload-scaled insert budget actually ships.** v1.5.117's release artifact was published 36 seconds *before* the fix merged, so it carried only the extension side. The result was exactly the failure the fix predicted: a 150 KB `keyboard inserttext --file` was cut off at 30.170s by the daemon's hard-coded budget while the extension would have allowed 308s. The daemon budget now scales with the payload too (30s + 4ms/byte, capped at 180s), and a cross-side test pins the invariant that the daemon always outlasts the extension — otherwise the daemon cuts first and the relay's more specific error never reaches the caller.
- **A payload-sized timeout no longer claims the connection is dead.** `CDP command timed out: Input.insertText` used to add "the session's browser connection is unresponsive (likely a stale relay/service-worker mid-session). Reconnect with `connect`" — sending the caller after a stale worker that is not there. The connection is fine; `Input.insertText` costs time per character (~0.45s/KB in a rich editor) and the payload needed more than the budget. It now says this is a size limit, says explicitly not to reconnect, and suggests inserting less at once. Every other command's timeout keeps the connection diagnosis, where it is the likely cause.

### Contributors

- @leeguooooo

## 1.5.117

### Bug Fixes

- **`keyboard inserttext` gains `--file` / `--stdin` — and chunking a large insert no longer corrupts it (#301).** Back-to-back `keyboard inserttext` calls scrambled text at each chunk boundary while preserving total length, so a char-count check passed and the scrambled text shipped. The cause: `Input.insertText` returns when Chrome dispatched the insert, not when the editor (e.g. ProseMirror) committed it, so the next chunk raced the uncommitted tail. Reading the whole payload from a file or stdin sends it in one `Input.insertText`, removing the boundary entirely — the fix a caller actually wants, since it removes the need to chunk. Reported by the chatgpt-use session from live ChatGPT-composer use.
- **`site --help` prints site's own help (#299).** It was the only subcommand that fell through to the 534-line top-level help, which an agent reads as "that command does not exist" — then falls back to the expensive snapshot+click path adapters exist to replace.
- **`site list` no longer lists loader internals as adapters (#302).** `_`-prefixed files (family helpers like `_helper`, injected automatically) and `*.test.js` are filtered, so the list holds only runnable `name/command` adapters.
- **A purely numeric `click` argument errors instead of silently becoming a selector.** `click 1155` (e.g. a coordinate that lost its pair to a stray token) used to be sent to the DOM as a selector and reported the misleading "selector matched nothing"; it now says the argument looks like a coordinate and points at `click x y` / `click --coords x,y`. Prompted by a chatgpt-use report; the two-number form itself parses correctly through the full pipeline.
- **`press` help documents the popover trap.** `press <key> --selector <sel>` focuses through the selector, which dismisses an open popover/menu; to press a key against a control inside one, click it by coordinate first, then use a bare `press`.

### Contributors

- @leeguooooo

## 1.5.116

### New Features

- **One extension door instead of one extension release per feature (ab-connect 0.5.25).** `ABExt.call` forwards one allow-listed `chrome.*` call: `tabs`, `tabGroups`, `windows`, `downloads`, `webNavigation`, read methods freely, mutating methods only on tabs this relay created (adopted tabs stay read-only; `windows.create` / `windows.remove` are refused outright; `debugger`, `identity`, `storage`, `runtime`, `management` are never reachable this way). Of the eleven hand-written `ABExt.*` handlers shipped between 0.5.13 and 0.5.24, seven are wrappers over calls this door admits; behaviour fixes inside the worker still need releases, which 0.5.23's self-update delivers without user action. `ABExt.state` returns everything the extension holds, owns and is configured to in one round trip. The manifest's permissions are unchanged, so the update installs without re-approval.
- **`extension call <namespace.method> [json-args]` and `extension state`.** Debugging-level access to the door above; on an extension without the `call` capability both say "requires ab-connect 0.5.25 or newer" instead of Chrome's "'ABExt.call' wasn't found".

### Contributors

- @leeguooooo

## 1.5.115

### New Features

- **The core loop is one round trip per step.** The skill now teaches `click @e3 --observe` and `snapshot -i --diff` instead of a fresh snapshot after every action. A new `core/behaviour` reference covers the rules around that loop: read the `why:` line before retrying a quiet action, never drop to coordinates because a ref click was silent, `reload` instead of re-`open`ing the page you are on, one direct navigation for a lookup rather than a grid of guessed URLs, the page's own signal ends verification, and what `keep` is for. `trust-boundaries` gains the three tiers of side effects: hand back to the user, confirm at the step, or task-level pre-approval is enough. Learned from reading the browser-use plugin bundled with Codex; written against chrome-use's own commands.
- **`console --level <l>[,<l>]` and `--filter <text>`.** Both narrow the buffer before `--limit` tails it, so `--level error --limit 5` is the last five errors and not the errors among the last five lines. `warn` also matches Chrome's `warning`.
- **A click that replaced the page returns the new tree.** `--observe` on a navigating click used to emit the whole old tree as removals plus the whole new tree as additions (93 KB for one Hacker News link, three times the page). When at least 80% of both trees changed and both are page-sized, the observation now carries the new tree under `observed snapshot:` with one line saying how many lines of the old page are gone. In-page changes and small dialogs still get a delta.
- **Skill description leads with the browser.** Codex trims every skill description to about fifteen characters when many skills are installed; ours began "Default tool f", which says nothing, and Codex routed a logged-in-Chrome task to its own browser plugin. It now begins "Browser automation in the user's real, logged-in Chrome". The stub also says to load `core` once, reuse the session across turns, and keep daemon/relay/ref vocabulary out of replies.

### Bug Fixes

- **`snapshot --diff` with nothing changed no longer prints a bare newline.** The "no change since the last snapshot" note went to stderr only, so a caller reading stdout saw an empty page. The note is now the stdout output, in parentheses.
- **Embedded skill content is re-extracted when it changes, not only when the version does.** The per-version cache under the user's cache directory kept serving whatever the first binary of that version had extracted, so a rebuild at the same version with a new reference answered "No reference 'behaviour' in skill 'core'". The cache marker now carries a fingerprint of the embedded trees as well.
- **`console` says why it is empty.** Capture is off by default for stealth, and the text output was simply blank; the hint about `AGENT_BROWSER_CAPTURE_CONSOLE=1` reached only `--json`. It now prints on stderr whenever the list is empty.
- **Docs gaps closed:** `--remember`, `extract --schema`, the ⚠ mark in `tab list` (`relayAttached: false`), and the `why:` line under `--observe`'s `no change`.

### Contributors

- @leeguooooo

## 1.5.114

### New Features

- **Signed and notarized macOS binaries.** Releases were ad-hoc signed, which `spctl` rejects; a copy downloaded through a browser therefore carried a quarantine flag Gatekeeper would refuse. The macOS archives are now signed with a Developer ID and notarized — `spctl` reports `accepted / source=Notarized Developer ID` (#283).

  The `rc=137` this was first attributed to has a different cause, measured after the fact: **overwriting a binary that a process is currently executing** kills anything started from it, whatever its signature. `install.sh` already replaces the binary with `mv` (a rename, which leaves the running process on the old inode) and is unaffected; `cp` over the same path is what fails.
- **Repeated control names carry the text that separates them.** Three identical `button "进入"` in a card grid now read as `context="飞行棋"` / `"H5 Games"` / `"Win In Future"`. Only when a name is repeated, and only when the added text actually distinguishes them — context that leaves two lines identical is dropped rather than making both longer (#284).
- **Why nothing changed.** When an observed action produces an empty delta, the reply now says which of the three situations it is: the target is disabled, has no box, is covered by something, is off-screen, or is present and fine — in which case the action legitimately changed nothing and an empty delta must not be read as failure. Probed only when the delta was empty (#277).
- **`report` states its own boundary** — what it includes, what it excludes (full URLs, page content, form input, cookies, tokens), and that screenshots are a separate decision no text redaction covers. It now reads the extension version and relay state itself instead of asking you to run `doctor` and paste it (#278).
- **`tabs` reports the tab the relay is actually attached to**, not only the one the session pinned, and says so when the two disagree. Needs ab-connect 0.5.24 (#279).

### Bug Fixes

- **`type <text>` with no target** is now a usage error naming both forms, instead of treating the text as a selector and reporting "selector matched nothing in the page DOM" (#282).
- **`fill` on a React combobox that resets on focus.** `fill` blurs and restores focus so a following `press Enter` reaches the field; a control whose `onFocus` clears its own query lost the value in between. The value is re-applied without touching focus and re-verified — the verification itself is unchanged, so nothing is reported as filled that the field does not hold (#282).
- **Update advice under policy install.** A force-installed extension has no update control on its own row, so "chrome://extensions → Update" pointed at a button that is not there — reported as "chrome-use took away my ability to upgrade". Managed installs are now told to use the toolbar Update button or restart Chrome (#284).

### Contributors

- @leeguooooo

## 1.5.113

### New Features

- **Extension self-update (ab-connect 0.5.23):** the extension now asks Chrome to check the Web Store instead of waiting for its own multi-hour schedule, and applies a downloaded update the moment no tab is attached. It needs no new permission. A pending update that never finds a quiet moment is still applied by Chrome when the worker stops (#272).
- **Outdated-extension notice where it matters:** when a relay command fails with a blocked debugger access or a stale target, the error now says whether the installed ab-connect is behind the version the Web Store serves. Only on the failure path, and only when a newer build is genuinely installable — being behind the bundled version while on the newest published one is normal and is not reported (#271).
- **Observations name their target:** `--observe` output carries `target` with the session, tab id, target id and url, so a caller can tell whether a rebinding, a cross-process navigation or a tab switch moved it to a different page before it reuses refs across the boundary. Ids only — no account address (#270).
- **`frame <index>`:** `frames` prints `[0] top …` and the frame-boundary error tells you to switch with `frame <id>`, but a bare index used to come back as a raw `SyntaxError: '0' is not a valid selector`. The documented recovery path now works when followed literally, and out-of-range says what the range is (#269).

### Bug Fixes

- **`do` no longer reports an unconfirmable action as a plain success.** Only `expand`/`collapse` can be judged from the accessibility tree afterwards; `showMenu`, `toggle`, `increment` and `decrement` look the same whether the control responded or ignored the click. Those are now marked `·` with `confirmed: null`, not `✓` (#266).
- **A failed rebinding keeps the session on the browser it already had.** An explicit `--browser` / `connect <port|url>` / `--cdp` used to close the old connection before establishing the new one, so a typo left the session bound to nothing, showing `about:blank`, while later commands still returned success (#268).
- **Blocked debugger access names the page it was about.** The error now reports which tab the session is pinned to and its url, and reads the url: an extension or internal page is the cause; an ordinary page means the pin did not move, so the failure is elsewhere (#267).
- **Socket path limit explains where the 103 bytes went** — the directory, the room left for a session name, and the name's length — instead of blaming a session name the user never chose. `doctor`'s own generated name is now short enough not to consume half the budget (#265).

### Improvements

- `flags` unit tests no longer assert against values the real environment supplies, so `cargo test` passes on machines that export `AGENT_BROWSER_EXECUTABLE_PATH` (#265).

### Contributors

- @leeguooooo

## 1.5.112

### New Features

- **Rule write-back:** `open <url> --browser <profile> --remember` asks ChooseBrowser to route that site to that profile from now on. It only proposes: ChooseBrowser confirms in its own dialog and nothing is saved otherwise. Requests that could not be valid are refused before navigating, with the reason. Needs ChooseBrowser 0.2.1 or later; on an older build chrome-use says so instead of pretending the request went through (#257).
- **Doctor self-check for ChooseBrowser:** `chrome-use doctor` reports which rules file is read and how many rules it holds, an unreadable version or file, a second copy that exists but is not being read, and whether the installed app accepts rule requests (#253, #257).
- **`session stop --force`:** when a session's tabs can no longer be reached (the browser endpoint changed after an upgrade or restart), drop the ownership record instead of refusing. The tabs stay open (#263).

### Bug Fixes

- **Upload:** one upload now fires exactly one `change`. The post-upload confirmation step dispatched a second one, which left React/Livewire uploaders with a phantom 0% entry and a disabled submit button (#261).
- **`read <url>` on client-rendered pages:** a JavaScript app shell no longer comes back as an empty page. The command refuses with the reason and points to `open` + `text`; a shell with only a little text returns it with a warning on stderr (#262).
- **Session stop after an upgrade:** the failure message no longer asks you to reconnect to a browser that no longer exists; it says the daemon is stopped, how many tabs remain open, and how to pick them up or drop the record (#263).
- **Protected-content errors:** `Cannot access a chrome-extension:// URL of different extension` now notes that another chrome-use session daemon may hold the tab, with the commands to find and stop it (#263).

### Contributors

- @leeguooooo

## 1.5.111

### New Features

- **Local action context:** Interactive snapshots retain bounded nearby text, including product prices, alongside action controls. Semantic items, heading groups, and unambiguous linked product cards are supported. Repeated context on product links is omitted (#246).
- **Profile rules:** Opening a site can use existing ChooseBrowser rules to select a connected Chrome profile. Explicit `--browser` takes precedence; `--no-choosebrowser` disables rule lookup. An unavailable rule target falls back to normal profile selection, so verify account identity when needed (#247).

### Bug Fixes

- **Status receipts:** Interactive snapshots and action observations retain status text, including cart confirmations. Clickable status elements keep their action references (#246).
- **Drag routing:** Launched browsers choose their drag path from their own connection, even when another Chrome profile has an extension relay running (#246).

### Improvements

- Context output prioritizes short fields, marks truncation, and preserves references through budgeted continuation. Iframe expansion accepts references with additional metadata (#246).
- Release version and changelog validation run before the platform build matrix (#249).

### Contributors

- @leeguooooo

## 1.5.109

### Bug Fixes

- **Action outcomes:** Relay recovery no longer automatically repeats an action whose result is unknown. Errors distinguish uncertain execution from a confirmed rejection (#242).
- **Observation quality:** Failed before/after captures no longer fabricate an empty page or removed elements. Responses preserve the action result and report complete, partial, or unavailable observations (#242).
- **Relay recovery:** Child-frame errors preserve healthy parent connections, recovered tab aliases target the replacement tab, and protected extension content produces an explicit access error (#242).
- **Connection status:** Bundled ab-connect 0.5.22 confirms the native host connection before showing Connected and ignores stale connection callbacks. Chrome Web Store distribution is separate (#242).
- **Session locks:** Lifecycle locks are explicitly released so a forked child retaining the descriptor does not delay subsequent commands (#242).

### Improvements

- Observation request summaries are bounded, retain omission counts, and appear in text output even without a DOM change. Full records remain available through `network requests --json` (#242).
- Added `CHROME_USE_RELAY_DIR` for isolated relay discovery. Relative paths are rejected instead of silently using a different registry (#242).
- Navigation detects confirmed debugger access denial while retaining normal page-load waits (#242).

### Contributors

- @leeguooooo

## 1.5.101

### Bug Fixes

- **File upload confirmation:** React dropzones that consume and clear or replace their file input now return success with a verification warning instead of a false rejection (#208).
- **Controlled selects:** Native selects use platform setters and dispatch both input and change events so controlled forms commit the selection (#209).
- **Idle session preservation:** Idle daemon recycling preserves external Chrome tabs, URLs and in-page state. Explicit close and session stop still clean up created tabs (#210).
- **Relay navigation:** ab-connect 0.5.20 uses browser-level navigation first and reports rapid same-URL reload loops with recovery guidance. App Store Connect still needs a signed-in site regression check (#211).

### Improvements

- Added browser-backed upload and select regression tests and extension navigation/reload-loop tests.
- Updated command help, agent guidance, and English/Chinese documentation.

### Contributors

- @leeguooooo

Earlier releases are documented in [the English changelog](docs/en/changelog.html) and [the Chinese changelog](docs/changelog.html).
