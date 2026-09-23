# Changelog

## 1.5.137

<!-- release:start -->
### Bug Fixes

- **`jev run` now completes the 16-question form it could not finish.** Six runs out of six submit it with all twelve fields and all four checkboxes as asked — including leaving one unchecked because the goal said so — and report `done`, in 18.4–23.0s (median 19.8s), checked against the form itself rather than against jev's own status. The cause was what the decision model could not see: its request carried only the last ten actions, and by the checkboxes the run had taken thirteen, so "full name", "company" and "role" left that list at the same moment they scrolled out of the viewport. The goal still asked for them and nothing the model was shown said they were done, so BLOCKED climbed from 0.07 to 0.53 over the last three decisions. The request now carries the whole run; an entry is a few dozen bytes and runs are capped.
- **The text helper is retried once.** A single malformed answer ended the whole run — seen as prose instead of JSON cut off at the token limit, and as an upstream 502. The call only generates text and nothing is typed until it succeeds, so a retry repeats no side effect. One retry, not a loop.
- A field a run already filled with the same text is now marked rather than removed: it stays in the element list under its own name and with its value — the evidence it is done — while offering nothing to type. Removing it (v1.5.136) left it represented only by its `Open <label>` click.

### Corrections

- **v1.5.136 was wrong about why the form stopped.** It said the remaining stop at the checkboxes was "the model's judgement on complete input, and nothing here addresses it". The input was not complete. Controlled runs isolated it: a page with only the checkboxes and a goal about only them completed every time; the same page with the full 16-item goal went straight to BLOCKED (0.77); a pre-filled form with no history stalled too — each time on goal items the model had no evidence for.

### Contributors

- @leeguooooo
<!-- release:end -->

## 1.5.136

### Bug Fixes

- **`jev run` no longer offers a field it already filled with the same text.** Retyping a field that still holds the exact text the run typed into it is a no-op, and offering it is not free: the TYPE_TEXT target question has no "none of these" answer, so once the operation question picks TYPE_TEXT some field has to be chosen. On a 16-question form every text field left in view after scrolling was already filled, and that is what the run kept choosing — at 0.48 for TYPE_TEXT against 0.43 for CLICK, while a radio and four checkboxes sat visibly unset beside it. With those candidates withheld, five runs in a row click both radio groups instead, spending the same 13 actions on work that counts. Only an exact match with the run's own typed text is dropped, so a field the page reset, or one holding something the run did not write, is still offered — as is the `Open <label>` click that re-triggers an autocomplete. **The form still does not complete**: with only checkboxes and Submit left, Jev answers BLOCKED (0.53) over CLICK (0.40) from a state that correctly shows every box unchecked and clickable. That is the model's judgement on complete input, and nothing here addresses it.

### Improvements

- **`JEV_TRACE` now records the request body too**, alongside the candidates, the choice and Jev's answer probabilities. The candidate list alone does not show what the model was told about the page, and both defects found in this area were questions about the input rather than the choice. It records the goal, the page text and field values, so treat the file as sensitive.

### Contributors

- @leeguooooo

## 1.5.135

### Bug Fixes

- **`press Meta+a` now selects all on macOS.** It never did: Chrome resolves Cmd+A there through the OS text system, which a synthetic CDP key event does not reach, so the key was delivered, nothing was selected, and the next `keyboard inserttext` appended to what the field held. A field reading `hello` became `helloX` instead of `X`. That broke the documented `click <input>` then `press Meta+a` pattern for every agent on macOS. The platform select-all chord (Cmd+A on macOS, Ctrl+A elsewhere, with no other modifier) now goes through the same editor command `fill` already used. Ctrl+A on macOS keeps its own meaning. Copy, paste, cut and undo were not reproduced and are unchanged.
- **`jev run` no longer reads a checkbox's or radio's value attribute as its state.** An unchecked terms box was presented to the model as "current value `on`" and an unselected radio as its own option name, so the model skipped both as already set. They now carry an empty value beside the real `checked` state. Together with the select-all fix, a re-filled field is replaced instead of appended to, and a skipped radio is now selected. The 16-question form that exposed both still does not complete: with only radios, checkboxes and Submit left, Jev weighs TYPE_TEXT (~0.52) over CLICK (~0.30) and re-types a finished field. That is recorded as open, not fixed.

### Improvements

- **`JEV_TRACE=<file>`** writes one JSON line per `jev run` decision: the candidates the model was shown, its choice, and Jev's answer probabilities. Off unless set. It records field labels and current values, including anything typed, so treat the file as sensitive. The run report also splits `act_ms` into `act_read_ms`, `cmd_click_ms`, `cmd_press_ms` and `cmd_insert_ms`.

### Contributors

- @leeguooooo

## 1.5.134

### Improvements

- **The installed skill now hands off to the guide the binary carries.** `skills/chrome-use/SKILL.md` — the entry point an installed agent actually reads — was a separate 244-line manual that never loaded `core`, so v1.5.132's smaller `core/SKILL.md` did not reach it. It is now a 41-line entry that loads `chrome-use skills get core`, and upgrading the binary updates the guide an agent follows. On what that buys: in a small cross-harness comparison on synthetic local tasks, DeepSeek V4.1 Flash needed 7 outer tool calls with the new guide against 10 with the old one and about 35% lower host-reported cost on the paired basic task; Claude Code did not show that pattern, and native MCP did not consistently reduce model turns against the CLI. That is a few observations under uneven host load, not a general speedup or success-rate claim. The harness and full report are in `bench/skill-eval/` and `bench/reports/`.

### Bug Fixes

- **A `wait` that hit its deadline is no longer reported as a dead connection.** `Wait timed out after …` used to be rewritten as "the session's browser connection is unresponsive … Reconnect with `connect`, or close the session". In one recorded run an agent waited for `Saved` while the page already read `Delivery saved`, spent its 25-second budget on the case mismatch, was told to throw the session away, and the very next `get text` worked. The hint now says the condition was not observed and that the timeout alone does not establish a connection failure — it does not claim the opposite either, since the poller retries failed probes until its deadline. It also states that `--text` is case-sensitive and that an already-visible receipt does not need a second wait. Genuine CDP/relay timeouts keep their existing diagnosis, and `wait` matching itself is unchanged.

### Contributors

- @leeguooooo

## 1.5.133

### Corrections

- **v1.5.132 overstated who benefits from the smaller skill.** It said "the agent skill's entry point is 73.9% smaller". The measured reduction is real, but it applies to `core/SKILL.md` — the file `chrome-use skills get core` serves. The entry point an installed agent actually reads is `skills/chrome-use/SKILL.md`, a separate 244-line manual with no `skills get core` indirection, which this change did not touch. So nothing about what an installed agent loads follows from that number, and the note now names the file instead of the entry point. Found by codex-01a0c18c auditing the real loading path rather than the file I had measured.

### Bug Fixes

- **`jev run` named one of its diagnostic fields wrongly, and the name was misleading enough to misread runs through.** The last entry in the `stale_fields` report was labelled `doc`, implying a document identity. It is the form-control state (`value`, `checked`, `selectedIndex`, `disabled`, `readOnly` per input); `timeOrigin` is what changes when a document is replaced. Read with the correct names, `timeOrigin` and `url` appear in no discarded decision at all — every one of them was the same document at the same URL, with the page's text, actionable set or form state still moving. The field is now called `formState`.

### Contributors

- @leeguooooo

## 1.5.132

### Improvements

- **`core/SKILL.md` is 73.9% smaller.** It went from 9443 to 2460 tokens (o200k_base, measured — not inferred from line count), 204 lines. The default behaviour rules moved to the front, and plain `click` / `fill` / `select` / `pick` are now self-contained, so the common case loads no reference at all. Detail moved into `core/references/` — `reading`, `connection` and `site-adapters` are new — where frames, closed shadow roots, canvas, screenshot parameters, auth handoff and idle recovery all still live. This is a static routing budget: the token counts are real, the effect on task completion or latency was not measured and is not claimed.

- **`jev run` reports where its time went** — `jev_ms`, `act_ms`, `observe_ms`, `fresh_ms`, plus decision, stale and eval counts. One measured run on a public site split 64% model round trips, 29% real page load, 6% the CLI's own commands, against separately measured costs of ~15ms per daemon-to-renderer eval and ~34ms per click. That is one task on one site from one location; the shape is what generalises, not the figures.

- **`jev run --terminal-shadow`** (opt-in) asks, in the same request, whether the chosen action ends the goal, and records that claim against what the closing decision then decided, along with which parts of the page moved when a decision was discarded. It is a measurement tool, not an optimisation: it does not change the completion control flow, and across six runs the model never once predicted a terminal action, so the shortcut it was built to evaluate would have saved nothing. A cheap local check is not a completion test either — a checkout bounced to `/login` changes the page exactly as a success would.

### Bug Fixes

- **Three defects in the skill's quickstart**: it taught `npm i -g chrome-use`, which is the distribution path this project deliberately does not support (it ships GitHub Release binaries and an `install.sh` one-liner); it offered `close --all`, which reaches every session rather than the reader's; and an unclosed code fence swallowed the "Diagnosing install issues" section and everything after it.

### Contributors

- @leeguooooo

## 1.5.131

### Bug Fixes

- **A large `Input.insertText` is no longer declared failed while it is working** (#315, #309). A timed-out insert cannot be cancelled — losing a `Promise.race` cancels nothing and CDP has no primitive to recall a dispatched command — so the page keeps working on it for minutes and the next command on that tab collides with a renderer we already gave up on. That damage was largely our own timeout: the extension's budget is 8s + 2ms/byte, "~4x the measured worst case", but the 120s cap started binding at 56KB and the *effective* rate collapsed above it — 0.8ms/byte at 150KB, under the ~1.0ms/byte worst case measured on chatgpt.com. A healthy renderer was failed at exactly the sizes the feature exists for, and #309 measured the same ceiling from outside ("around 100KB, not the ~265KB the budgets imply"). The extension cap moves to 300s and the daemon's to 360s, keeping "client outlasts daemon outlasts extension" at every size; the invariant test now spans both ceilings. The cap still exists for a pathological payload — it just no longer cuts into the per-byte allowance an ordinary large insert depends on.

- **The extension's timeout text no longer advises the chunking that corrupts text.** It still said "Insert less at once", which #301 proved scrambles text at every boundary because a call returns on dispatch, not on commit. The CLI-side hint was fixed for this; this copy was missed. It now says the insert was not cancelled and the page may still be working.

- ab-connect **0.5.27**.

### Contributors

- @leeguooooo

## 1.5.130

### Bug Fixes

- **Windows: a first command on a fresh `--session` name could hang forever with no output** (#327). `resolve_port` fell back to a port derived from a hash of the session name into 49152-65534 — the Windows *ephemeral* range, the ports the OS hands to every other program's outbound sockets — so on a busy machine an unrelated process was routinely already listening there. Connecting then succeeded against a stranger: `daemon_ready` reported a healthy daemon so none was started, the command was written, and nothing ever answered. The recovery that clears stale state and starts a fresh daemon keys on "os error 2", which a Unix socket reports and a TCP connect never does, so it could not fire on Windows. Now the `.port` file a daemon writes is the only source for connecting (no file means "start one", not "connect to whoever is there"), the derived port moves to 21000-31999 so the daemon's preferred bind stops colliding by construction, and the Windows connect is bounded and reaches the existing recovery. Cross-checked against `x86_64-pc-windows-gnu`; not reproduced on Windows, so this is a demonstrated defect matching the report, not a confirmed diagnosis of the reporter's machine.

- **A child-reaping test no longer flakes** under a loaded machine: it slept a fixed 200 ms and then asserted the child had exited, and now polls to a deadline.

### Contributors

- @leeguooooo

## 1.5.129

### Bug Fixes

- **`open --prefer-spa` waits for the page to be ready, not just for the URL to change.** The clicked link is not always a client-side route: on a same-origin plain link the browser does a real navigation and `location` changes at commit, well before the document is parsed, so the command could return a page that an ordinary `open --wait-until load` would still have been waiting for. `readyState` is now polled with the URL. An SPA route never sits at `loading`, so it costs that path nothing.

- **`cu.find` is now defined.** The script guide has always listed it among the `cu.*` helpers, but the prelude never defined it, so calling it threw "cu.find is not a function". It mirrors the CLI: a string is the natural-language search, and `{ selector }` lists matching elements.

### Contributors

- @leeguooooo

## 1.5.128

### Bug Fixes

- **A `script` context could deadlock the session.** A context runs one program at a time, so a `cu.*` call that re-entered `script --in <the same name>` queued a job behind the very program waiting for it: the thread could not pick it up, nothing closed the new run's bridge, and the daemon waited forever — the session simply looked unresponsive. The re-entrant call is now refused with an error that says why. Introduced with the feature in 1.5.127 and caught before it was used in anger.

- **`AGENT_BROWSER_DEBUG=1` produces a daemon log on Windows too** (#327). The debug-log redirection sat inside a `#[cfg(unix)]` block, so on Windows the variable did nothing at all — no `<session>.log`, no daemon stderr anywhere. #327 is a Windows-only hang whose reporter offered to run with a verbose/debug variable, and there wasn't one for them. Windows now writes the same file; `eprintln!` goes through CRT fd 2, so the fd is re-pointed rather than only the Win32 stderr handle. Cross-checked against `x86_64-pc-windows-gnu`; not verified at runtime on Windows.

- **`session stop <name> --force` no longer implies the name is clear** (#309). It reported "dropped its record", which reads as "this name works again" — and it does not. `--force` drops the CLI's record; the tabs stay open, and on the extension relay a session's tabs live in a tab group named after the session, so a fresh daemon under the same name meets them again. If one of those tabs has a busy renderer the name behaves exactly as before. The note now says so and names the two moves that work: close the tab, or use a different `--session` name.

### Contributors

- @leeguooooo

## 1.5.127

### New Features

- **`script --keep <name>` / `--in <name>`: JS contexts that persist across calls** (#289). `script` built a fresh engine every call, so nothing survived between them. What that costs is round trips, not bytes: a single call could always do several steps, but an agent usually has to see one step's result before choosing the next, which meant re-deriving every handle each time. `--keep` runs in a named context and creates it on demand, `--in` requires one that already exists, `--drop <name>` releases one and `--contexts` lists them. Contexts are released with the session's daemon. A named context evaluates at top level so declarations survive, which has two consequences that now carry a hint instead of a bare SyntaxError: top-level `return` is invalid (end with the expression), and re-declaring a `const` the context still holds throws, as in a Node REPL.

- **`open <url> --prefer-spa`: route in-page instead of cold-booting the app** (#311). When the page is already on the target's origin and the app has its own link to the target, click that link and let the router handle it. A full navigation makes the SPA boot from scratch — measured on chatgpt.com in the issue, `open <origin>` cost 45 backend-api requests where the app's own controls cost 1 to 9, and that site throttles on requests, not messages. On react.dev (`/learn` to `/reference/react`) an in-page route costs 19 requests against 47 for the navigation. It falls back to an ordinary navigation for a cross-origin target, when no link matches, or when the router does not land, so it never leaves you somewhere other than the requested URL. Opt-in, because a client-side route can keep stale state a reload would have cleared.

### Bug Fixes

- **`cu.fill` never worked.** The `script` JS helper sent `text` where the `fill` action wants `value`, so every `cu.fill(...)` failed with "Missing 'value' parameter". Found by driving a real login form while verifying the above.

### Contributors

- @leeguooooo

## 1.5.126

### Bug Fixes

- **Tab switch no longer pays for a second target discovery** (#338 by @AmeerAliAnwar): `tab switch` ran `resync_targets` unconditionally and then `tab_switch` ran discovery again through `reattach_active_session`. A known target id or `t<N>` ref now resolves locally, and discovery runs only as the fallback for an unknown tab reference.

- **The refusal for an unadopted tab now names a command that works.** It gained a recovery hint reading ``use `tab adopt t1` or `--adopt` ``, and neither exists: `tab adopt` matches a spec against targetIds and URL substrings only, so `t1` fails with "no open tab matching `t1`", and there is no `--adopt` flag in the CLI. The hint now names the tab's targetId, so the suggested command is copy-pasteable.

- **The extension version is read from the profile actually being driven** (#319, remaining callers): the per-profile version file landed for `status` and `doctor`, but `outdated_extension_note`, the `tab select` / `tab adopt` liveness warnings, `tab inspect`'s error and the bug-report environment block still read the generic `relay-ext-version`, which whichever worker said hello last overwrites. With two profiles connected each could describe the other one — including telling an up-to-date user to go update. The fallback for extensions too old to report a `profileId` is unchanged.

- **Duplicated tabs verify their settle under coarse timer ticks** (#338 by @AmeerAliAnwar): with 15.6ms platform tick quantization the transaction deadline could expire while `completeBefore` waited on a stalled promise, so `observeWithin` was called with `timeoutMs = 0` and skipped verification even though foreground activation had finished. A small observation budget lets it confirm a state that is already reached.

### Contributors

- @leeguooooo
- @AmeerAliAnwar

## 1.5.125

### New Features

- **`chrome-use jev run --goal <text> [--url <url>]`**: a browser agent in which TypeSafe's Jev picks each step's operation and target from an indexed element table, and a small OpenAI-compatible model (default `inception/mercury-2.5` on OpenRouter) writes text only when a field needs typing. The policy is adapted from browser-use/jev-ultrafast (MIT); the browser layer is this CLI's own daemon socket, so no process is spawned per step. Keys come from `TYPESAFE_API_KEY` / `TEXT_MODEL_API_KEY` or `~/.config/typesafe/key` / `~/.config/openrouter/key`. On the Google Flights task (Zurich to London) from Tokyo it finishes in 11 to 13s; most of that is Jev round trips, about 460 ms each from Japan versus about 150 ms from Los Angeles. An explicit "no value for this field" from the text model ends the run as `blocked`, and `--json` reports `success` from the run status.

### Bug Fixes

- **`--cdp` could adopt a hung tab and fail every command for 30s.** On connect, every existing page tab was adopted and the first made active without checking it. When that tab's renderer was hung, the first command waited out a 30s `Page.enable` and reported the tab as gone. Each tab is now released from any debugger wait and probed with a 2s evaluation; the first that answers is driven, otherwise a fresh tab is opened and the hung ones are left alone. On a 1 GB Linux host with a hung Google Flights tab, `open` went from 3/6 (each failure 30s) to 6/6.

- **Multi-tab concurrency isolation** (#334 by @AmeerAliAnwar, integrated in #335): per-tab state so concurrent sessions driving different tabs do not clobber each other, a `tab switch` alias, `chrome_use_tabs` back in the extended MCP profile so `core` is the original 12 tools, and screenshots bring a background tab to front so headful Chrome keeps producing frames for the capture.

### Contributors

- @leeguooooo
- @AmeerAliAnwar

## 1.5.124

### Performance

- **Every command was ~150ms slower than it needed to be.** Before dispatching anything, the CLI slept 150ms and probed the daemon socket a second time, on every call, even when the daemon was healthy. That sleep was about 95% of a warm command: `eval` goes from 167ms to 11ms median. On a Jev-driven Google Flights search (about 60 browser calls) the whole task went from 20.0s to 14.1s median over five alternating runs, all verified. The sleep guarded against connecting to a daemon that was shutting down; `close` now unlinks the socket before its shutdown delay, so a successful connect already means the daemon is serving.

### Bug Fixes

- **`eval` of an `async` function that declares a variable returned `{}`.** `(async () => { const x = {a: 1}; return x; })()` came back empty. Scripts that declare a top-level `let`/`const` run in Chrome's replMode so they can be redeclared across calls, and replMode does not await promises. The "might return a promise" check did not look for `async`. It now matches `async` and `Promise` as whole words, so identifiers like `asyncData` still get replMode.
- `main` did not compile after the multi-tab change (two borrow errors), and three unit tests had not been updated for it; `chrome_use_tabs` is now counted as a core MCP tool in the tests, as it already was in the server.

### New Features

- Concurrent multi-tab workflows: `--new-tab` on navigation and explicit tab targeting (#330).
- Trusted coordinate clicks on the extension relay; background tabs no longer wait on a paint before screenshots; agent screenshots as base64.

### Contributors

- @leeguooooo

## 1.5.123

### Bug Fixes

- **v1.5.122's fix for #319 did not work; this is the real one.** `status` prints "driving A" above "profile: B" with two different ids. v1.5.122 moved the extension version into a per-profile file but still looked it up with the wrong id, so the same mistake simply entered somewhere else and the symptom survived — one `status` run after upgrading showed it unchanged. The actual defect is larger than the original diagnosis: "which profile is driving" had **two unrelated implementations**. The one that decides which browser is actually bound picks by window focus; the ones that report it to you — the `profile:` line, `drivingProfileId`, `browsers`' default column, `doctor`, and the version resolver — all used the other one, "whichever worker connected last". With two profiles connected those disagree. They are now a single resolver, preferring the focus-based answer that the endpoint selection actually uses and falling back to the generic sidecar only where the focus answer is deliberately absent (fewer than two profiles, an extension too old to report focus, or a tie) — which are exactly the cases where the generic one is right.
- **Honest limitation: this is not verified.** The symptom only appears with two Chrome profiles connected, and that environment was not available. What is established is that the two notions are unified in code and the suite passes — *the same evidence v1.5.122 offered before turning out to be ineffective*. #319 is therefore left open rather than closed. If you have two profiles, run `chrome-use status` and check whether the `driving` and `profile:` lines name the same id.

### Contributors

- @leeguooooo

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
