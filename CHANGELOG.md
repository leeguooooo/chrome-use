# Changelog

## 1.5.113

<!-- release:start -->
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
<!-- release:end -->

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
