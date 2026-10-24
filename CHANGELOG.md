# Changelog

## 1.5.264

<!-- release:start -->
Works with extension ab-connect 0.5.84.

### New Features

- **Jump to a conversation's webpage.** Each conversation now has a separate window button beside its title. It focuses the last recorded agent tab and verifies the active tab and focused window. If the page was closed, it reopens its last HTTP(S) URL without sending a message or rerunning a task. The compact buttons support light and dark themes, keyboard focus and narrow panels.

### Bug Fixes

- **Clean up cursor artifacts when enabling stealth.** Enabling stealth or turning the cursor off removes existing cursor artifacts and reports unconfirmed cleanup (#712).
- **Name extensions that block debugger access.** Debugger errors identify the extensions whose frames prevent access (#695).
- **Report QR sign-in security verification.** A login waiting on a security-verification scan now explains that state (#694).
- **Register custom Chrome profiles.** Native-host installation also finds running Chrome instances with a custom user-data directory and supports explicit directories (#706).

### Improvements

- **Make ClawHub publishing opt-in for Git pushes** (#689).

### Contributors

- @leeguooooo
- @euvoor
- @Jerryx333
- @leinaldo
<!-- release:end -->

## 1.5.263

Works with extension ab-connect 0.5.83.

### Bug Fixes

- **Keep the mobile gateway alive while its network address is unavailable.** `mobile serve` now waits with exponential backoff from 1 to 60 seconds instead of exiting and triggering repeated service restarts. It keeps the exact requested address and reports readiness only after listening succeeds. Other bind errors still fail immediately. After a permanent network change, update the bind and phone endpoint or use a stable Tailscale address or named tunnel (#713).

### Contributors

- @leeguooooo

## 1.5.262

Works with extension ab-connect 0.5.83.

### Bug Fixes

- **Continue a conversation after a model capacity error.** Sending a new message after a failed or interrupted turn now starts it in the same conversation with the same model, instead of leaving it in the paused queue. Older queued messages stay paused until explicitly resumed; failed tasks are not automatically replayed (#711).

### Contributors

- @leeguooooo

## 1.5.261

Works with extension ab-connect 0.5.82.

### Bug Fixes

- **`args.ui` no longer reports a target on a scrolled page as covered.** The isolated-world hit test used CDP's `DOM.getNodeForLocation`, which does not take viewport coordinates: on a page scrolled down (Douyin's post page) it answered for a different spot, so clicks on the 自主声明 and visibility selects were refused and fell back to synthetic clicks. It now uses `elementFromPoint` in the isolated world, where the page's own copy is never called; it takes viewport coordinates and passes through `pointer-events: none`, as a real click does.

### Contributors

- @leeguooooo

## 1.5.260

Works with extension ab-connect 0.5.82.

### Bug Fixes

- **`args.ui` clicks through overlays with `pointer-events: none`.** The isolated-world hit test (1.5.257) counted an element the pointer passes through (a select's arrow, an icon laid over a button) as covering the target, so `args.ui.click` refused it and the adapter fell back to a synthetic click; seen twice on Douyin's post page. It now passes through them, as a real click and `elementFromPoint` do.
- **The `--launch` stealth script no longer replaces natives a page can check.** Its `Error.prepareStackTrace` / `Error.captureStackTrace` wrappers, which hid evaluated frames from stack traces, are gone (chrome-use's own code runs in an isolated world since 1.5.258, so its frames are not in the page's stacks), and the `querySelector` wrapper for sites using the disable-devtool library is off unless `AGENT_BROWSER_DISABLE_DEVTOOL_SHIM=1`. botcheck's patched-natives check passes in `--launch` mode, and `scripts/stealth-bench.sh --launch` passes everything it did before (#703).

### Improvements

- **`setOn` takes an element.** A site adapter can hand the file input itself to `args.<file>.setOn(el)`, also one that is not in the page, and chrome-use sets the file on that node; adapters no longer have to mark an input with an attribute or id to name it by selector.

### Contributors

- @leeguooooo

## 1.5.259

Works with extension ab-connect 0.5.82.

### Features

- **Site adapters can run where the page cannot see them: `"world": "isolated"`.** chrome-use's own reads moved to an isolated world in 1.5.258, but an adapter's own code still ran in the page's main world, so its `querySelector`, `fetch` and the like went through any wrapper the page installed. An adapter that declares `"world": "isolated"` in `@meta` now runs in the tab's isolated world: same DOM, cookies and storage, its own JS. What only page JS has (a React fiber or other element expando, an app store, the page's own signed `fetch`) comes through the new `args.page(fn, ...args)`: `fn` is sent by its source, run in the main world with the JSON `args`, and its JSON result comes back. Adapters without the key run as before. A `fetch` from the isolated world sends the page's cookies but skips the page's own fetch wrapper, so a site that signs its requests there needs `args.page` for them. The douyin, bilibili-creator, youtube-studio and twitter write adapters in chrome-use-sites move to it.

### Contributors

- @leeguooooo

## 1.5.258

Works with extension ab-connect 0.5.82.

### Improvements

- **Every command reads the page from an isolated world.** 1.5.257 did this for `args.ui` and `scrollintoview`; now `snapshot`, `click`, `type`, `fill`, `hover`, `find` / `getby*`, `get text` and the rest do too. chrome-use's own page reads (finding elements, measuring them, checking what covers them, reading values and text) used to run in the page's main world, where a page that wraps `querySelector`, `getBoundingClientRect`, `getComputedStyle`, `innerText` and the like sees an `<anonymous>` caller that is not one of its scripts. Each `Runtime.evaluate` and `DOM.resolveNode` chrome-use sends without naming a context now goes to the tab's isolated world (one per tab, recreated after a navigation; a node in a same-process frame falls back to the main world), so the page's own functions are never called. What has to see the page's JS still runs in the main world: your `eval`, `wait --fn`, `extract`, `addscript`, `pushstate`, site adapters, the React tools, the anti-bot vendor probe, `site analyze`, `stealth status` and the passkey guard, and `fill` / value reads on Monaco and CodeMirror editors (their model lives in page JS). `AGENT_BROWSER_MAIN_WORLD=1` puts everything back in the main world. As a side effect the popup guard no longer leaves `window.__chromeUsePopupGuard` in the page.

### Contributors

- @leeguooooo

## 1.5.257

Works with extension ab-connect 0.5.82.

### Improvements

- **`args.ui` and `scrollintoview` read the page without the page seeing it.** botcheck 0.2 wraps the page's `querySelector`, `getBoundingClientRect`, `elementFromPoint`, `getComputedStyle` and friends and looks at who calls them; rebrowser's `mainWorldExecution` test does the same for one function. chrome-use called them from the page's main world to find, measure and hit-test elements, and the caller showed up as `<anonymous>`, not one of the page's scripts. `args.ui` (the input site adapters such as `douyin-creator/video-publish` use) and `scrollintoview` now find elements in an isolated world, read an adapter's element handle off the run object by property, measure in the isolated world and hit-test with CDP's `DOM.getNodeForLocation`, so the page's own functions are never called. An adapter's own page reads are still the adapter's: this covers what chrome-use does on its behalf. Other commands (`click`, `type`, `fill`, `hover`, `snapshot`, `find`, `get text`) follow in later releases.
- **Scrolling an element into view no longer gives up after 12 wheel steps.** `args.ui.scroll` and `scrollintoview` keep wheeling until the element is in view, up to 60 steps, and stop early only when two steps in a row leave it where it was.

### Contributors

- @leeguooooo

## 1.5.256

Works with extension ab-connect 0.5.82.

### Improvements

- **`scroll` and `scrollintoview` use the wheel by default.** 1.5.255 moved `scroll --until` to the wheel; plain `scroll` (direction and amount, with or without `--selector`) and `scrollintoview` still moved the page with `scrollBy` / `scrollIntoView`, which a page sees as scrolling with no input at all (botcheck's scroll-source check, run in a real Chrome through the side panel). With humanize on (the default) they now wheel over the element or the page; `scrollintoview` wheels over the container the element scrolls in, re-measuring after each step, as `args.ui.scroll` does. A wheel that moves nothing falls back to the old way; `AGENT_BROWSER_HUMANIZE=off` keeps it.

### Contributors

- @leeguooooo

## 1.5.255

Works with extension ab-connect 0.5.82.

### Improvements

- **Stealth by default for the everyday commands.** botcheck (botcheck.leeguoo.com, our own detector for what automation leaves in a page and how its input looks) found that `click`, `type` and `scroll --until` gave themselves away, and they now behave like a person by default:
  - Input is humanized at the Fast level everywhere (a curved pointer path, landing jitter, varied key gaps), and at the Human level on sites that run their own bot checks the vendor detector cannot see (Douyin, TikTok, Xiaohongshu, Bilibili, X, Weibo, Kuaishou, Zhihu, Taobao, Tmall, JD, Pinduoduo, Instagram, Facebook, LinkedIn, YouTube).
  - A click by CSS selector no longer lands on the exact centre every time: the selector path now knows the element's size, as the `@ref` path did.
  - `type` on a web page types with key presses without `--key-events`, and Chinese or emoji go through an IME composition (key presses the page sees as `Process`, compositionstart / compositionupdate, then the commit), a few characters per commit. A bulk insert put text in with no keydown at all. Pages that are not http(s) (VS Code and Electron webviews) keep the bulk insert.
  - `scroll --until` scrolls with the wheel over what it scrolls, falling back to `scrollBy` for a step the wheel does not move.
  - Capitals and shifted symbols wait a human-sized moment after Shift (3–7 ms before).
  - `AGENT_BROWSER_HUMANIZE=off` restores the instant behaviour.

### Bug Fixes

- **A running site adapter leaves nothing on `window`.** botcheck found 1.5.253's `Symbol.for(<id>)` slot through `Object.getOwnPropertySymbols(window)`. The start script now returns the run object and the daemon reaches it by its CDP handle (`Runtime.callFunctionOn`); a navigation still ends the run and is reported as interrupted, and `--until-done` reruns it.
- **`args.ui.type` writes Chinese through an IME composition**, as `type` now does.

### Contributors

- @leeguooooo

## 1.5.254

Works with extension ab-connect 0.5.82.

### Features

- **Stealth mode switch, on by default.** The extension's options page has a Stealth mode switch (ab-connect 0.5.82). While it is on, chrome-use adds nothing of its own to the pages it drives; features that draw into pages work only with it off. Today that is the agent cursor: its switch is greyed out under stealth mode, and an agent cursor turned on before this stays dark until stealth mode is turned off. Inspect & Ask works either way, since you start it yourself. The extension's status reports `stealthMode`. Contributors, reviewers and releases now check every change for anything a page could see ("Stealth first" in AGENTS.md, and a new pull request template).

### Bug Fixes

- **The daemon's pointer follows `args.ui`.** After a site adapter's click, hover or scroll, a coordinate-less `mouse down` / `mouse up` pressed where the pointer was before the adapter ran (#698).
- **`args.ui` waits a varied time between scroll steps** while bringing an element into view, instead of a fixed 120 ms (#699).

### Contributors

- @leeguooooo

## 1.5.253

Works with extension ab-connect 0.5.81.

### Features

- **Site adapters can have chrome-use do their input: `args.ui`.** An adapter runs inside the page, so its own `el.click()`, `dispatchEvent` and `execCommand` are `isTrusted: false` events with no pointer movement and no key presses, which a site's bot checks can see. A write adapter can now call `args.ui.click(el)`, `hover`, `type(el, text, {clear})`, `typeHere(text)`, `press(key, {repeat})` (chords such as `Mod+a`) and `scroll(el | pixels)`, with an element or a CSS selector. The daemon wheels the element into view inside its scroll container, picks a point that is really on it, and clicks or types with real CDP input at a human pace: a curved pointer path, press dwell and varied keystroke gaps, whatever the anti-bot detector made of the site. ASCII goes as key presses, capitals and shifted symbols with Shift held; Chinese and emoji go in a few characters at a time after a pause, the way an IME commits them. A call fails with a reason when the element is gone, has no size or is covered, and names what covers it. A request the adapter does not wait for is still served before its result is read, so a last click before the page navigates still lands. The douyin, douyin-creator, bilibili-creator, youtube-studio and twitter write adapters in chrome-use-sites use it and keep their old events on older chrome-use.

### Bug Fixes

- **A running adapter no longer shows up as a named `window` property.** The run lived at `window.__cu_site_<id>` (`window.__cu_eval_<id>` for `eval --background`), which any page script listing `Object.getOwnPropertyNames(window)` could match on its fixed prefix. It now lives under `Symbol.for(<random id>)`, which `Object.keys`, `getOwnPropertyNames` and `for…in` do not list.
- **Typed capitals and symbols carry Shift.** `type --key-events` and `args.ui` sent `H`, `#` or `@` as key presses with `shiftKey` false, which no keyboard produces; Shift is now pressed around them, and `!@#$%^&*()` use their digit keys (`Digit1`…) instead of an empty code.
- **Humanized input no longer repeats from one daemon start to the next.** The seed for pointer paths, landing jitter and keystroke gaps started from a constant, so every daemon replayed the same sequence; it now starts from a random value.
- **`find … click` reports where the element really is.** The `at (x,y) WxH` in its reply read fractional coordinates as 0 (`at (0,205) 0x18` for an element at x 525, 66 wide); the click itself was not affected.
- **An adapter that sends input no longer waits up to a second per request.** The page is polled every 100 ms again after a request was served.

### Contributors

- @leeguooooo

## 1.5.252

Works with extension ab-connect 0.5.81.

### Bug Fixes

- **Agent tab groups no longer pile up in Chrome's saved tab groups.** Chrome keeps every tab group that is still open when its window closes or the browser quits as a saved group (signed in, on every device), and no extension can delete one. chrome-use left a project's `✦` group open after its last session ended, collapsed, waiting for the next session or the sweep, so quitting Chrome in between saved it. Now the tabs leave the group when the last session idles out, and `tabs cleanup` and the automatic sweep do the same for any agent group nobody works in; the tabs stay where they are, the project's next session still takes them over and puts them back in its group, and the sweep still closes them after 30 minutes. When Chrome restores the last session, ab-connect 0.5.81 takes the tabs of restored agent groups out of them, which deletes those saved groups too, without closing a tab. Groups saved before this cannot be removed by chrome-use. A group whose session is still running when Chrome quits is still saved. (#692)

### Contributors

- @leeguooooo

## 1.5.251

Works with extension ab-connect 0.5.80.

### Features

- **`site add` takes OpenCLI adapters and plugins.** It detects an OpenCLI-format source (an OpenCLI plugin repo such as `chrome-use site add darinouyang/opencli-plugin-shipxy`, a zip, or a local dir like `~/.opencli/clis`) and runs its commands through the OpenCLI runtime chrome-use already ships, over your chrome-use session; they show as `(opencli: <source>)` in `site list`. `.js` modules are supported, and `.ts`-only plugins on Node 22.13+ (compiled with Node's type stripping); YAML-only plugins are refused with a message, since OpenCLI itself stopped loading YAML in 1.8. OpenCLI adapters run as Node code with your user's permissions, not in the page like ours, so a local dir is added directly, while a remote source is pinned to the commit (a zip, or a repo fetched without the GitHub API: its SHA-256) you trusted: at a terminal `site add` lists its modules and asks, and scripts and agents must pass `--trust-node-code`. `site update` re-copies local dirs and never moves a pin; `site add` again to review a newer version. On a shared name our adapters still win, then added OpenCLI sources, then the OpenCLI package. `site sources` and `site remove` cover them too.

### Contributors

- @leeguooooo

## 1.5.250

Works with extension ab-connect 0.5.80.

### Bug Fixes

- **A session no longer takes over the tabs of another session that is still at work.** Sessions of one project share its tab group, and a new session took over the tabs of any session whose daemon was not answering, also while that session's agent was still waiting on a command (seen after a relay recovery stopped the daemon mid-command). A session now counts as ended only when no daemon runs for it, no `chrome-use` process is working with it, and it ran no command for two minutes. The same rule keeps a daemon that starts or resyncs from adopting such a session's tabs, and a tab another session opens meanwhile is no longer taken as the one a click opened. (#687)
- **A command that was sent but never confirmed fails as `action_outcome_unknown`.** When the daemon stays busy past the deadline, its reply is lost, or it disappears mid-command, the command exits non-zero with code `action_outcome_unknown`, is never retried, and the message says it may have run. (#687)
- **New `chrome-use session last`** reports a session's recent commands as `completed`, `failed`, `running` or `unknown` (never confirmed), newest first, without running anything or starting a daemon, so an agent can check an unconfirmed side-effecting command instead of repeating it. Only the action (and site adapter name) and how it ended are recorded, never arguments or output. (#687)
- **Relay recovery no longer takes the relay down for everyone.** When the relay was down for one profile, chrome-use stopped every native host on the machine, including those serving other profiles or a relay another session had just brought back, so the extension had to reconnect again ("background worker hasn't reconnected"). It now stops only hosts of this install that serve no published relay endpoint. (#687)

### Contributors

- @leeguooooo

## 1.5.249

Works with extension ab-connect 0.5.80.

### Bug Fixes

- **The side panel no longer asks you to reinstall a CLI that is already current (ab-connect 0.5.80).** After an upgrade, a `__nm-host` that Chrome started earlier keeps running the old binary, and one from before 1.5.239 sends no version, so the panel said "chrome-use CLI 太旧" and told you to reinstall even when the binary on disk was new. The extension now reconnects once, when no command or side panel request is running, before it shows that notice, and shows it only if the fresh host still sends no version. (#684)
- Two Rust tests that failed on Windows (a recording path that is not absolute there) and intermittently on macOS (a runner in session 1) are platform-independent. (#685)

### Contributors

- @Jerryx333
- @leeguooooo

## 1.5.248

Works with extension ab-connect 0.5.79.

### Changes

- **`hover` takes a viewport point, like `click`.** `hover 767 340` (also `767,340`, `--coords 767,340` and `x=767 y=340`, which `click` now reads too) moves the real pointer to that point, for a tooltip or badge with no stable selector. The reply names the element that received the trusted move (`receivedBy`, `verified`), and a point inside a cross-origin frame is moved to but reported unconfirmed. MCP `chrome_use_hover` takes `x` and `y`. A lone or odd number of numeric arguments, or a numeric "selector" the page rejects, now points at the coordinate form instead of the CSS selector hint. (#662)
- **A page that stops answering is told apart from a lost connection.** When a command times out on the extension relay, chrome-use asks the extension about the tab, which needs no renderer. If Chrome still answers for it, the reply says the tab is alive but its page is busy (a script blocking the main thread, or a page still loading on a held-open connection) and to wait and retry, instead of "restart the session and retry", which did not help. If nothing answers, it says the connection is gone and to open a fresh tab or session. A screenshot of a busy or closed tab now reports that, not "screenshot target drifted … attached to an unknown target". (#661)

### Contributors

- @leeguooooo

## 1.5.247


Works with extension ab-connect 0.5.79.

### Changes

- **Copy what you picked, for another agent or tool (ab-connect 0.5.79).** Under the picked elements in the side panel composer, Copy elements puts every picked element's context (page, selector, how to reach it inside frames and shadow roots, source, text, bounds, HTML) on the clipboard, and Copy message adds your words in front, as the agent would get them. Nothing is cut to the send limit, and a screenshot or attached image says plainly that it is not in the copied text. (#680)
- **Agents get the attached images' file paths in the message text.** Images you attach or that come with picked elements already reached Codex, Claude Code, OpenCode and Pi as image input, but the text never said where they were, so an agent handing the task on to another session (ocs, a terminal) passed only words. The message now ends with the numbered list of local files; "attached image N" in the element context is item N. (#680)
- **`snapshot --diff` in MCP.** `chrome_use_snapshot` takes `diff: true` for the same incremental read as the CLI; changing `filter` needs a new full baseline instead of reporting a different excerpt as page changes, and a valid delta lists only the refs it shows while the session keeps every live ref.
- **`read --help`** has its own page, and `read --filter` is documented as a literal, case-insensitive match (not a regex, unlike `snapshot --filter`).

### Bug Fixes

- **Closed agent tabs no longer leave saved tab groups behind (ab-connect 0.5.79).** Chrome keeps a group as a saved (synced) tab group when its last tab is closed while still in it. Agent tabs were taken out of their group before closing, except right after the extension's worker restarted (ownership not loaded yet), for agent pop-ups, and for tabs the cleanup closed through the raw `tabs.remove` passthrough, so groups such as `cu-chatdev-*` piled up. All three now leave the group first; tabs you own, lent or adopted keep theirs. (#674)
- **A click refused because something covers the element is reported as `element_occluded`, not retried.** The refusal quotes the covering element's text; a label like "Connection timeout" made it look like a retryable timeout. No click was dispatched, so the agent is told to deal with the blocker instead.
- A snapshot diff now recognizes a ref on an inserted root line (`+- button … [ref=e99]`).

### Contributors

- @leeguooooo

## 1.5.246

Works with extension ab-connect 0.5.78.

### Bug Fixes

- **`auth login --bwu` picks the right vault item among many.** bwu lists every login of the site's registrable domain (32 for apple.com), and the login stopped at "pick one with --item". Among several it now uses the item that signed in to the host last time (recorded only once the site was seen to take the login, by id only, in `~/.chrome-use/bwu-items.json`), else the one named exactly the page's host, else the one named exactly the host of the page's sign-in frame (App Store Connect signs in through an `idmsa.apple.com` iframe). It says which and why on stderr, even with `--json`. Otherwise the list shows items named for the page, its sign-in frame or its site first, and `--item` takes a name in any case. Login walls with auto-login choose the same way. (#672)
- **Sign-in frames that load late in their own process are found.** `auth login` (with or without `--bwu`) searched an out-of-process sign-in frame only if it existed when the command started, so a frame the page inserted afterwards was never filled ("no login field appeared"). The submit no longer fails when the sign-in frame goes away with the page after it. (#672)
- **A password manager's inline menu on a background agent tab in your own window is closed without touching your window.** Since agent tabs open in your window, Bitwarden's menu on one blocked every command and the recovery refused to switch your tabs, so the agent had to ask you to press Escape. chrome-use now activates a blank tab in its own agent window, which Bitwarden takes as a tab change and closes the menu; your window, the tab you are on and the focus stay as they were. A window opened for this holds only that blank tab and stays minimized for the session: the first time it is seen for about a second while it minimizes (measured on macOS), later times nothing shows. An agent window that already holds other agent tabs (`--window dedicated`) is used as it is and not minimized. A tab you are looking at in your own window is still left to you. `auth login` also takes the cursor out of each field it filled, so the menu usually closes before it gets in the way. (#672)

### Contributors

- @leeguooooo

## 1.5.245

Works with extension ab-connect 0.5.78.

### Changes

- **A new look for the extension (ab-connect 0.5.78).** The side panel, popup, settings page and Inspect & Ask frame share one warm theme: paper-coloured ground, a single clay accent, system serif for titles and agent replies, monospace for status and metadata, softer cards and a rounded composer. Light and dark both pass 4.5:1 text contrast. All four pages read their colours from one `theme.css`, so the Inspect & Ask frame and the "agent is using this tab" bar no longer carry their own blues and purples.
- **New icon:** a cream pointer on clay, drawn separately for the 16 px toolbar size so it stays readable.
- **Agents & models is grouped.** Agents you can chat with come first, installed ones below, and agents that are not installed collapse into chips under the list (a dashed chip means a configuration was found but the program is missing). A whole row opens its configuration; the per-row Configure buttons are gone.

### Contributors

- @leeguooooo

## 1.5.244

Works with extension ab-connect 0.5.77.

### Changes

- **Drive the agent chat from your phone: `chrome-use mobile`.** `chrome-use mobile serve` runs a gateway that the chrome-use Remote app (iOS) connects to over your own route: Tailscale, a Cloudflare named tunnel or the LAN; we run no relay for traffic. From the phone you can list conversations, start one, send text and up to five images, follow progress live (a reconnect resumes without missing or repeated events), answer approval requests, stop a turn, screenshot the conversation's own browser session (only if it is already running; a phone never opens a browser), list models (`agentInspect`) and run or switch scheduled tasks.
  - `mobile pair --url ws(s)://… [--name] [--profile] [--allow-full-access]` prints a QR code that works once within 10 minutes; `mobile devices`, `mobile revoke <id>` (drops the live connection within a second), `mobile access <id> full|limited`, `mobile push-test <id>`. `serve` binds `127.0.0.1:17345` by default; another interface needs `--bind`, every interface also `--allow-all-interfaces`; `--trust-cf-header` for cloudflared on the same computer; `--name` sets the computer name the app shows.
  - Every frame is end-to-end encrypted (HMAC handshake, AES-256-GCM per direction, strict sequence numbers), so a tunnel that terminates TLS sees no instructions, page text or screenshots. Pairing adds a secret generated on the phone, so the QR link left in the terminal no longer works once paired. Keys live in `~/.chrome-use/mobile/` (0700/0600).
  - The phone gets less than the desktop: no full access, conversations it creates go up to `writeFiles` (Pi: browser tools only), no project folder, executable, configuration, skill or schedule edits, and it cannot raise a conversation's access. Conversations with full access, command execution or Pi beyond the browser can be read, stopped and declined from the phone, but sending, approving and running them needs `mobile access <id> full` on the computer. The gateway never forwards `/api/v1/*`, the stream WebSocket or the panel service's capability; browser connections (any `Origin`) are refused, and failed handshakes back off per source.
- **Push notifications** when an agent needs your approval, a turn ends, or a scheduled run finishes or fails, at most one per conversation every 10 seconds; each conversation can be muted from the phone. They go through a stateless Cloudflare Worker (`https://chrome-use-push.leeguoo.com`, or `--push-url` / `CHROME_USE_PUSH_URL`) that only forwards ciphertext: the title and a short redacted preview are encrypted on the computer. Tokens the Worker refuses are dropped and the app is told to register again.
- **Faster conversation reads with two readers.** The panel service remembered one verified journal position per conversation, so the side panel and another reader polling the same conversation at different positions made every read rescan the whole journal. It now keeps a few positions per conversation.

### Contributors

- @leeguooooo

## 1.5.243

Works with extension ab-connect 0.5.77.

### Changes

- **Agent tabs are grouped by project, not by session.** A session's tabs go in its project's group, `✦ <project>`. The project is the side panel conversation's project folder, else the git repository around the working directory, else the working directory. So two terminals in one repository, and every new conversation there, share one group instead of each opening another. A project whose folder name another project already uses gets its parent folder added to the title, once; the title is kept from then on. `session name` renames the project's group.
- **Sessions hold leases on their tabs.** Each session drives only the tabs it opened. A tab another running session of the project opened is left to it, so two agents working at once never take each other's tabs, and `done` in one of them closes only its own. When a session's daemon has ended, its lease has too: the project's next session takes those tabs over and reuses them.
- **The same page is not opened twice.** `open <url>` switches to the session's tab already showing that page instead of opening another, in the background and without reloading it (`reusedTab: true`). Pages compare by url without the `#fragment` and tracking parameters (`utm_*`, `fbclid`, `gclid`, …). `open --new-tab <url>` opens another tab anyway. Different projects never share tabs, and the user's own tabs are never moved or grouped.
- **A group nobody works in collapses at once.** When the last session of a project idles out, its group is collapsed in your window; its tabs are closed after 30 idle minutes as before (`AGENT_BROWSER_GROUP_CLEANUP_MS`, `0` turns that off) unless the project's next session picks them up first.
- **No more groups holding a single blank tab.** A session opens a blank tab when it first connects; a session that only read or did nothing left it behind in its own group. A tab still on `about:blank` or the new tab page is now closed when its session idles out, and the cleanup closes such tabs of an ended session at once (no 30-minute wait), so the group goes with them. A blank tab is never taken for the user's, even in front. `tabs cleanup` clears them too.
- **Upgrading moves each session's tabs once.** A session whose tabs are in a group named after the session (1.5.236–1.5.242) moves them under its project's group the first time it runs on this version. Tabs of sessions that never run again are left to the cleanup.
- **No second Chrome from a sandboxed agent.** When the extension relay is down, chrome-use starts Chrome only when it can prove Chrome is not running. A connected extension counts as proof that it is, and a process list that cannot even see the system's own processes (as inside Codex's macOS sandbox) proves nothing. Before, such a sandbox saw no Chrome and `open -na` started a second one.
- **Tabs a session left for the user stay theirs.** Another session of the project never picks up a tab left with `keep`, `done --keep` or by the cleanup.

### Contributors

- @leeguooooo

## 1.5.242


Works with extension ab-connect 0.5.77.

### Changes

- **Agents end their tasks with `chrome-use done`, and decide themselves what stays open.** `done --keep <tab>` leaves a tab the user still needs (a page they asked to see, a result to act on, a login left for them) as a plain tab outside the ✦ group; every other tab the session opened is closed and read back, like `close`, and the session ends. With extension 0.5.77 the kept tab is also released by the extension, so no debugger stays attached to it. The skill and the core guide now tell agents to run it at the end of every task without asking the user. After the daemon's idle exit, `done --keep` reconnects to find the tab; `done` with nothing open, or a second time, does nothing and succeeds; a `--keep` naming a tab the session did not open (or a blank one) is not kept, and the reply says so. MCP: `chrome_use_tabs` with `action: "done"`, `keep: [...]`.
- **Tab groups no longer pile up.** A session's daemon exits after 10 idle minutes and leaves its tabs open, and every new conversation used to get a new group, so groups accumulated by the dozen. Now, once a session's daemon is gone and it has been idle for 30 minutes (`AGENT_BROWSER_GROUP_CLEANUP_MS`, `0` turns it off), the next new session closes its tabs in the background. A tab the user is looking at, or switched to and stayed on (extension 0.5.77), is left for them, ungrouped and recorded as kept. Tabs the user lent, adopted tabs and other people's tabs are never closed.
- **A new task continues the previous task's group.** A session started from the same terminal and working directory takes over the group and tabs of the previous session there once its daemon has exited, instead of opening another group. `status` names the session's tab group. `--session <name>` sessions are unaffected.
- **`chrome-use tabs cleanup`** closes the groups of every ended session now, including groups left by older builds (`⬉`, a trailing `🤖` or a bare session id in the title).
- **`keep` actually ungroups the tab.** The ungroup request was sent without the tab's session, so the extension could not tell which tab was meant and the kept tab stayed in the group.
- **No more "Apple cannot verify .bun-501-….dylib" from Inspect & Ask on macOS.** Opening Inspect & Ask ran `ocs` (a Bun-built program) inside Chrome's process tree, which inherits Chrome's file quarantine, so the library Bun extracts was blocked by Gatekeeper. These operations now run in the panel service, which launchd starts outside Chrome, so `ocs` no longer inherits the quarantine.
- **The agent cursor (when turned on) no longer slides in from the top-left corner** of a page, and its tip sits exactly on the point it clicks.

### Contributors

- @leeguooooo

## 1.5.241


Works with extension ab-connect 0.5.76.

### Changes

- **Sites that hold many projects go by project, not by domain.** App Store Connect apps (`/apps/<appId>`, TestFlight pages included), Vercel projects, Netlify sites, Cloudflare Workers and Pages, Firebase and Google Cloud projects (in the path or `?project=`) and Sentry projects are read from the URL by a rule table in the CLI (one row per URL shape, so another site is one more row). A recent folder is preselected, shown as "Matched by project", when a file in its root names that project: an App Store Connect app id on an `apple_id`/`app_id` line of fastlane's Fastfile, Appfile or Deliverfile, `.vercel/project.json` or `vercel.json`, `wrangler.toml`/`.json`, `.firebaserc`, `.sentryclirc` or `sentry.properties`. As with repositories, a project bound to that app or project still wins and a site-wide binding does not.
- **The remember question names the app or project.** It defaults to "This app" / "This project" and calls it by the page's own name (the tab title without the site's name, else the page heading when the extension may read the site): "Use this project for Hellow (appstoreconnect.apple.com/apps/1234567890) from now on?". Such sites are never bound whole by default. Projects named only in `?project=` (Google Cloud, some Sentry pages) are matched automatically but can be remembered by path only.
- **Repository remotes behind an ssh host alias match too.** A remote such as `git@github-work:owner/repo` is resolved through the `HostName` of that alias in `~/.ssh/config` (read only; `Include` is not followed).

### Contributors

- @leeguooooo

## 1.5.240

Works with extension ab-connect 0.5.75.

### Changes

- **A repository page starts in your checkout of that repository, not in whatever project the whole site was bound to.** On github.com/owner/repo (and GitLab with subgroups, Gitee, Bitbucket, Codeberg, self-hosted Gitea, Gogs and GitLab) the side panel now finds the recent folder whose git remote (origin, upstream or any other; https, ssh or `git@host:owner/repo`) is the page's repository and preselects it, shown as "Matched by repository". Before, remembering a project once on github.com made every repository there open in it. Remotes are read from `.git/config`, worktrees included, cached by modification time; git is not run. A project bound to the repository itself still wins; a broader binding (the whole site) no longer overrides the match. Nothing already stored is removed.
- **Remembering a project asks for the narrowest sensible scope, editable in place.** On a repository page the bar asks "Use this project for github.com/owner/repo from now on?" with "this path" and "whole site" as alternatives; other sites default to the site with a path option. A code host is never bound whole by default. The options page shows each binding's scope and can change it in one write.

### Contributors

- @leeguooooo

## 1.5.239

Works with extension ab-connect 0.5.74.

### New Features

- **The CLI keeps itself current.** Most installs stayed on the version they started with: background upgrades were opt-in and the only signal was a stderr line. When a newer release exists, any command, and the native host whenever Chrome starts it, now installs it in place in the background (it takes effect on the next run) and refreshes the skill copies afterwards. `CHROME_USE_AUTO_UPGRADE=0` turns it off and leaves the notice. A CLI installed where it cannot write, or a cargo build, is never replaced. On macOS the upgrade runs as a launchd job, because started from inside Chrome it would install a binary carrying Chrome's quarantine. How the last background upgrade ended is kept, and `doctor` says why a CLI is still behind and what to run.
- **The extension shows version state** (ab-connect 0.5.74). The popup and side panel say when the CLI is out of date, with an **Update CLI** button when it can replace itself and the command to run when it cannot; when a background upgrade failed, and why; and, for a CLI from before this release, that it must be reinstalled once. Once a new CLI is installed, the extension switches to it when no agent is working. Its own update gets a banner too: downloaded and waiting for the agent to go idle (with what it is waiting for), an **Update now** button when that is safe, and "updated to X" for a day afterwards.

### Contributors

- @leeguooooo

## 1.5.238

Works with extension ab-connect 0.5.73.

### Bug Fixes

- **A link the agent opens in a new tab no longer takes over the tab you are looking at.** Since 1.5.236 agent tabs share your window, and Chrome opens a page's `target=_blank` link or pop-up as the window's front tab, so an agent click switched you from your page to the agent's pop-up.
  - Cross-site `target=_blank` links are now opened by chrome-use in a background tab while agent tabs share your window, so Chrome neither switches your tab nor comes to the front (`AGENT_BROWSER_BACKGROUND_LINKS`, now `cross-site` by default in that mode; `off` turns it off). Cost, as before: a link that redirects back to the page's site arrives without its `SameSite=Strict` cookies.
  - ab-connect 0.5.73 brings back the tab that was in front for every other tab an agent tab opens (same-site links, `window.open`): the pop-up stays open behind it and the agent keeps working in it. It also adopts such a pop-up into the session again (and `--follow` follows it), which in your window failed because Chrome names your front tab as its opener. Until Chrome updates the extension, those still switch your tab and are not adopted.

### Changes

- **Agent tab groups are marked `✦`** instead of `⬉`: one plain character that takes the group's colour. A session running during the upgrade opens a new `✦` group next to its `⬉` one, once.

### Contributors

- @leeguooooo

## 1.5.237

Works with extension ab-connect 0.5.72 (no CLI change; this release ships the extension fix).

### Bug Fixes

- **The /resume confirmation no longer overlaps the panel.** Choosing a session that is still running in a terminal showed "continue here anyway / cancel" below the picker's box, on top of the status line and the lent-tab bar. The picker's list now shrinks and scrolls so the question stays inside it.

### Contributors

- @leeguooooo

## 1.5.236

### Changes

- **Agent tabs open in your own window again, in a tab group marked ⬉.** The separate agent window made it hard to watch the agent or take over: you had to go find a second window. AI tabs now open as background tabs in the window you are using, without taking focus, each session in its own tab group whose title starts with `⬉` (`⬉ 🔎 track a parcel`). `--window dedicated` or `AGENT_BROWSER_DEDICATED_WINDOW=1` brings back the shared background window. Groups made by an older build have no mark, so a session that is running during the upgrade opens a new `⬉` group next to its old one once. Closing a password-manager menu on a pinned tab now needs you in a shared window, as it did with `--window user`.

### Contributors

- @leeguooooo

## 1.5.235

### New Features

- **macOS: chrome-use installs as `ChromeUse.app`, so privacy settings list it once, by name and icon.** System Settings → Privacy & Security → Files and Folders keyed the bare `chrome-use` binary by its path and showed it with no name or icon, and every other copy of the binary (each build, each test run) became one more anonymous entry. On a Mac, install.sh now puts a signed, notarized and stapled `ChromeUse.app` (bundle id `com.leeguoo.chrome-use`) in `<prefix>/share/chrome-use/` and makes `<prefix>/bin/chrome-use` a symlink into it, so the panel service, the agents it runs and the native-messaging host all run as that one app. Its prompts say why access is asked for. `upgrade` keeps installing into the symlink's directory, archives without the bundle (older tags) still install the bare binary, and `AGENT_BROWSER_NO_APP_BUNDLE=1` opts out. Linux and Windows are unchanged.

### Contributors

- @leeguooooo

## 1.5.234

### Bug Fixes

- **Background form controls scroll instantly before pointer input.** CSS smooth scrolling no longer leaves off-screen click targets in place. Page/container scrolling reports an unchanged position, and `scrollintoview` checks that its target reached the viewport (#657).
- **Custom comboboxes open and select with trusted pointer input.** `pick` handles widgets that ignore synthetic mouse events, reports hidden form changes when the search input clears, and warns about unconfirmed choices. An empty observation no longer claims the action legitimately did nothing (#657).

### Contributors

- @leeguooooo

## 1.5.233

Works with extension ab-connect 0.5.71 (no CLI change; this release ships the extension fixes).

### Bug Fixes

- **Messages sent while the agent is working show only in the queue.** Each one used to appear twice: as a sent message in the conversation with "Checks not recorded" under it, as if it had already run, and again in the queue below. It now stays in the queue, where it can be edited or removed, and moves into the conversation when its turn starts.

### New Features

- **Thermo's tab organizer can add tabs to a group it made earlier, and new groups stay in the tabs' own window** instead of moving to the focused window.

### Contributors

- @leeguooooo

## 1.5.232

Works with extension ab-connect 0.5.70 (no CLI change; this release ships the extension fix).

### Bug Fixes

- **Picking a button no longer clicks it.** With Pick element, the pick ended on mouse release and the click that followed reached the page, so picking a submit or search button submitted it. The picker now swallows the whole click.

### Contributors

- @leeguooooo

## 1.5.231

Works with extension ab-connect 0.5.69 (no CLI change; this release ships the extension's element picking).

### New Features

- **Pick an element straight into the side-panel chat.** A "Pick element" button in the composer: click it, click something on the page, and the element lands in the message box as a chip at once, with its screenshot and, when the project's source is found, its file and line. Shift picks several; Esc cancels. Elements inside shadow DOM and same-origin iframes can be picked. The agent receives each element's selector and a way to locate it, so it can act on it in the tab.
- **Inspect & Ask goes to the side-panel chat by default.** Sending to a terminal session over ocs is the second choice, remembered per site.
- **One permission prompt instead of one per site.** Picking from the side panel asks Chrome for access to a site the first time; "Allow element picking on all sites" in Options asks once for all of them.

### Contributors

- @leeguooooo

## 1.5.230

Works with extension ab-connect 0.5.68 (no extension change).

### Bug Fixes

- **/resume pins the site's project when the session folder is a symlink or under /tmp.** Sessions are now listed with their folder resolved, as a site's remembered project is stored, so the two match. Resuming still uses the folder the session recorded.
- **Recovery no longer calls a process group that is just exiting "another user's".** On macOS a killed group whose members were still exiting, or had just been reaped, read as foreign, so recovery reported the outcome as unknown instead of gone.

### Contributors

- @leeguooooo

## 1.5.229

Works with extension ab-connect 0.5.68 (the copyable command needs it; the other fixes work with every extension).

### New Features

- **`chrome-use upgrade -f` takes over without asking.** It skips the guided extension setup, then stops every side panel conversation still running on an older build and that build's panel service, so the panel's next request uses the installed build. It does this even when the CLI is already up to date. The panel's "older build is still running" notice offers the command with a copy button.

### Bug Fixes

- **A conversation an older build is running keeps working in the panel.** While an older build's service was busy, the panel refused everything but listing, reading and stopping, including reading that conversation's screenshots, lending it a tab and answering its prompts, so the only way out was to stop the turn you were watching. Requests for a conversation the older build is busy with now go to that build.

### Contributors

- @leeguooooo

## 1.5.228

Works with extension ab-connect 0.5.67 (the new composer layout needs it; nothing changed in the CLI).

### Improvements

- **A tidier side panel composer.** The attached page and "Agent works in" sit on one row above the message as matching chips instead of two differently styled lines. Attach images and Conversation options are icon buttons, Full access, recording and Stop & send share one style, and recording is a dot toggle that turns red when on.

### Contributors

- @leeguooooo

## 1.5.227

Works with extension ab-connect 0.5.66 (the grouped picker needs it; the session times are fixed for every extension).

### New Features

- **/resume groups sessions by project.** The picker lists project folders, most recently active first, each with its latest sessions and "Show more"; sessions without a folder are under "Other". The project remembered for the current site comes first. Search matches titles, project names and paths and keeps the grouping.

### Bug Fixes

- **/resume no longer shows every Codex session as "just now" and "active elsewhere".** Codex Desktop rewrites session files when it imports Claude Code sessions, which made their file time look new. Sessions are now dated by their own records, and a session counts as active only when it wrote very recently or has an unfinished turn (or, for Claude Code, a live process). Codex subagent threads are no longer listed.

### Contributors

- @leeguooooo

## 1.5.226

Works with extension ab-connect 0.5.65 (no extension change).

### Bug Fixes

- **Stop always ends the agent's whole process group on macOS and Linux.** When an agent exited on Stop, its child processes were sometimes left running and the turn ended as "outcome unknown", and sometimes killed, depending on a few microseconds of timing. The service now ends the owned group every time, so Stop is deterministic and leaves no orphans (Windows already ended the whole job).

### Contributors

- @leeguooooo

## 1.5.225

Works with extension ab-connect 0.5.65 (no CLI change; this release ships the redesigned settings view and folder chip). Chrome Web Store publication follows Google review.

### New Features

- **A redesigned Agents & models view.** Agents, skills and schedules are card lists with status badges. Schedules show their cadence ("Weekdays at 09:00"), last result and next run, with Acknowledge highlighted when an outcome is unknown. An agent's configuration opens under its row instead of at the bottom of the page. The schedule and skill editors stay closed until New or Edit, model and tool options are folded away, and checkboxes are switches.
- **The project folder is a visible chip in the composer.** "No project folder" until you choose one, then the folder's name; "Choose a folder…" opens the picker. The keyboard hint moved to the send button so it no longer covers these controls.

### Contributors

- @leeguooooo

## 1.5.224

Works with extension ab-connect 0.5.64 (no CLI change; this release ships the extension's consent fixes).

### Bug Fixes

- **A tab is never lent to a conversation you did not lend it to.** When a tab's panel lent its tab automatically while you switched to another conversation, the tab could go to that other conversation and flip it to "this tab". The lend now goes only to the conversation it was started for.
- **A take-back holds in every view.** Taking a tab back from a conversation opened in a full-page tab now also stops the side panel from lending it again by itself.
- **Two panels never keep one conversation.** Moving a conversation to another tab while its old tab's panel was sending left both panels on it; the old panel now follows once the send ends.

### Contributors

- @leeguooooo

## 1.5.223

Works with extension ab-connect 0.5.63 (the per-tab panel and working in your tab need it). Chrome Web Store publication follows Google review.

### New Features

- **One side panel per tab.** Each tab has its own panel and conversation, and its own open or closed state. Switching tabs shows that tab's conversation, or no panel if you never opened one there; closing a tab keeps its conversation in history.
- **The agent works in your tab.** A side-panel conversation lends the agent the tab it sits beside on the first message (AI badge, grant bar, "Take back the tab"), instead of opening its own window. Chrome and Web Store pages, a failed lend or a take-back fall back to the agent's own window, and the panel says so. "Agent works in: this tab / its own window" is in Options and per conversation. When the conversation's browser session restarts, it takes the lent tab back instead of opening a blank tab.
- **Slash completion anywhere in the message,** including skills from enabled Claude Code plugins; ↑↓, Tab or Enter to choose, Esc to close.

### Bug Fixes

- **No raw text keys in the panel.** Text without a translation falls back to English.
- **An update blocked by an old conversation** now offers Open and Stop it for each conversation that keeps the older build running.

### Contributors

- @leeguooooo

## 1.5.222

Works with extension ab-connect 0.5.62 (no extension change).

### Bug Fixes

- **An update no longer gets stuck behind an old conversation without saying which.** When an older build's panel service still has a conversation running or waiting for your answer (for example a tool approval left open), the newer build cannot take over that profile and sending failed with "Another agent-panel build is running for this profile". The message now names those conversations, so you can open them to answer or stop them, then send again.

### Contributors

- @leeguooooo

## 1.5.221

Works with extension ab-connect 0.5.62 (the tab modes need it; older extensions keep working without them). Chrome Web Store publication follows Google review.

### New Features

- **Open a conversation in a tab.** "Open in a tab" shows the panel as a full-page tab with a centred column. It stays in sync with the side panel when both show the same conversation; each message is sent once. "This page" in a tab means the window's last web tab.
- **Lend the agent a tab.** In a conversation you can let the agent use one of your tabs. The tab is marked with an "AI" toolbar badge and the panel shows a grant bar; the agent works in that tab with your login, and cannot close it. "Take back" (or Stop, deleting the conversation, closing the tab) ends it immediately and later calls are refused. Chrome pages and Web Store pages cannot be lent; one tab per conversation. The agent's own window stays the default.

### Contributors

- @leeguooooo

## 1.5.220

Works with extension ab-connect 0.5.61 (the new side panel features need it; older extensions keep working without them). Chrome Web Store publication follows Google review.

### New Features

- **Continue a terminal session in the side panel (`/resume`).** Pick a Codex, Claude Code or OpenCode session started in the terminal and keep working on it in the panel. Its earlier messages are imported redacted and capped at the last 40; a session still running elsewhere asks for confirmation first. Deleting the panel conversation keeps the CLI's own session files.
- **Project folder per conversation.** A new conversation can run in a project folder chosen from recent folders or the system folder picker; the choice is fixed for that conversation. System folders are refused, Pi is not supported, and panel files, screenshots and downloads are never written into the project.
- **Sites remember their project.** After you pick a project on a site, the panel offers "Use this project for <site> from now on?"; later conversations on that site preselect it. The bindings are shared with Inspect & Ask and can be edited on the options page.
- **The extension speaks Chinese.** The side panel, options page and Inspect & Ask follow Chrome's language (English and Simplified Chinese).

### Bug Fixes

- **Inspect & Ask replies went to the wrong session.** Questions were sent as `chrome-use`, so a terminal session with that name received the agent's replies. Each inspect question now has its own sender identity, and replies show up in the inspect frame, in the side panel and as notifications.
- **The options page opened in a tab** could not talk to the panel service; it is now accepted when it is the top frame of its tab.

### Contributors

- @leeguooooo

## 1.5.219

Works with extension ab-connect 0.5.58 (no extension change).

### Improvements

- **Outdated extensions are called out per profile.** The `connect` profile list marks each profile whose extension is behind the newest build (the Web Store's when known), with how many days ago the profile was last opened, and says how to update: Chrome only updates a profile's extensions while that profile is open, so rarely used profiles keep old builds.

### Bug Fixes

- **No leftover launchd jobs.** The panel service unloads the launchd job that started it once it owns its profile, so each profile directory no longer leaves an idle `com.chrome-use.panel-service.*` job behind (test runs left hundreds).

### Contributors

- @leeguooooo

## 1.5.218

Patch release. Works with extension ab-connect 0.5.58 (no extension change).

### Bug Fixes

- **Recovery works on macOS again.** Since 1.5.208 the panel service started by launchd ran in session 1, so after an unconfirmed stop the side panel could never prove the old agent's process group and never offered End the old agent process or Continue in a new conversation. The service now starts itself in its own session (the launchd job exits normally; Chrome's quarantine is still not inherited).
- **Passkey guard restore** also finds a guard the page moved deeper in the credentials prototype chain, and reports a chain it cannot walk (proxy, accessor, too long) as a failure instead of clean.

### Contributors

- @leeguooooo

## 1.5.217

Patch release. Works with extension ab-connect 0.5.58 (no extension change).

### Bug Fixes

- **A browser check counts only when it is the same check.** A requirement is checked only when the agent's predicate equals the recorded one; operator words, quotes around the whole value and spacing between words may differ, the value itself must match byte for byte. An undescribed or different check is reported as claimed. Agents are told to copy the recorded predicate from the tool result.
- **Passkey guard during `auth login --bwu`.** Restoring never reports "clean" while a guard still blocks `create()`: proxy chains, builtins the page replaced, a partial install, an out-of-process frame without the Page domain and an accessor `create` are reported as failures. Checked on Chrome for Testing and branded Chrome 155.
- **Snapshot refs.** A ref named by an element's text is not handed on to new content in the same node.

### Contributors

- @leeguooooo

## 1.5.216

Patch release for the agent panel shipped in 1.5.215. Works with extension ab-connect 0.5.58 (no extension change).

### Bug Fixes

- **Browser checks must match what was recorded.** A requirement now counts as checked only when the cited check records the same selector, value or URL the agent describes (paraphrases such as `==`/`contains`, quotes and case are accepted). Citing a confirmed but unrelated check is reported as "claimed" instead of checked.
- **Redaction.** Protocol fields are kept unmasked only where the protocol puts them and only with identifier-shaped values, so a secret can no longer pass under a field named `name` or `status`; dispatch fields stay intact. A streamed answer with more than 256 secrets withholds the rest and still learns secrets split across chunks. A message whose text, page and attachments together would not fit one record is refused when sent.
- **Passkey guard during `auth login --bwu`.** The `create()` guard covers the page, its existing frames and attached cross-site frames, keeps no way back to the original reachable from page script, and is restored by identity with a per-frame report. Page script that already ran before the login started is out of scope; a registered passkey still stops the login.
- **Object groups** used by element lookups and the passkey guard are released even when a command is cancelled.

### Contributors

- @leeguooooo

## 1.5.215

Requires extension ab-connect 0.5.58 for the turn view, task notifications and HEIC attachments (older extensions keep the previous view). Chrome Web Store publication follows Google review.

### New Features

- **Side panel shows each request as one turn.** Every message gets a card with the page it was sent from, the pages the agent actually acted on ("asked for X, landed on Y" when they differ), its steps with a verdict per browser action, the answer, and an outcome line. Journals written by older versions show "not recorded" instead of guessing.
- **Browser evidence the agent cannot invent.** Each chrome_use browser call takes a lease from the panel service before it runs and reports what happened afterwards; the result is linked to the step by action id, never by timing. Calls admitted after Stop are refused.
- **Checks, not claims.** Before answering, the agent lists what you asked for and cites its browser checks (expect, read-back, URL match, download type). The badge says "Agent's browser checks passed", "Some checks passed", "Not checked" or "Blocked" — never "Verified".
- **Result files.** Screenshots, downloads and (with your per-message consent) recordings are kept per conversation, checked by SHA-256 and type, and shown as cards; the panel reads them by id, never by path. Saving to the path the agent asked for is reported separately.
- **Recovering an unknown outcome.** When an agent process cannot be confirmed stopped, the turn offers Review, End the old agent process (only when the process group is proven to be ours) and Continue in a new conversation, which carries the possibly-executed actions as context and sends nothing by itself.
- **Long tasks.** Repeated identical results, repeated failures and silence raise a stall banner (advisory only); turn summaries and context-compaction markers keep the history readable.
- **Agent Chat documentation.** A standalone chapter on the site covers the side panel end to end.
- **Task notifications.** A Chrome notification and a toolbar count when a panel task finishes, fails, stops or waits for your approval; clicking opens that conversation. Settings → Notifications to turn it off.
- **HEIC photos.** iPhone HEIC/HEIF images can be attached; the local CLI converts them to an upright JPEG without EXIF/GPS (macOS sips; Windows needs Microsoft's HEIF and HEVC extensions; Linux needs heif-convert or ImageMagick).

### Safety and privacy

- **Redaction before the journal.** Passwords, tokens, cookies, auth headers and secret-shaped strings are masked before anything is written to disk; values masked once are also scrubbed from later display text. Old journals are masked when read and never rewritten.
- **Approvals.** The panel answers with a decision bound to the request revision it showed; the service builds the agent's reply from the original input. During Stop only decline/cancel is accepted, and full access no longer auto-approves.
- **Nothing left on the page.** Element lookups, picks, sign-in fills and refocus no longer write marker attributes into the page DOM, and click watches, sign-in receipts and upload buffers no longer leave globals, `Symbol.for` keys or element properties a page could read; chrome-use holds them by CDP handle instead. Opt-in features (`react` diagnostics, `jev`, site adapters, `--allowed-domains`) are unchanged.
- **Redaction covers every entry point.** Tool results, approval requests, streamed and final answers and the attached page are masked on the whole structure before anything is shortened; a value a rule masks is also masked wherever the same event repeats it, as text or number. Oversized events become a short "too large to record" stub instead of failing.
- **Process isolation per turn.** A turn that ended badly, timed out a browser call or left a call open gets a fresh agent process that resumes the session; Codex 0.162.1 and Claude Code 2.1.300 keep their process after a clean turn.

### Bug Fixes

- **Inspect & Ask setup** asks code-inspector-plugin for `hotKeys: false`, so its own Option/Alt+Shift picker no longer reacts to Alt+Shift+C (test builds also `server: 'close'`).
- **Codex full access with browser-only or read-files tool access** refused every chrome_use call ("requires approval, but approval policy is never"); chrome_use tools are now approved for those threads without widening shell or file access.
- **Selectors that match several elements** are refused (`selector_ambiguous`, with match count, text and position) instead of acting on the first one.
- **Snapshot refs** on nodes whose accessible name the page re-renders now resolve when the element is the same, and still refuse a different element.
- **Rich-text fill/type verification** ignores zero-width spaces, NBSP and mention formatting instead of reporting a false failure.
- **Observed clicks report toasts and dialogs** that appear during the action; `--follow` says when it could not take the tab a click opened.
- **Request log** is capped at 1000 entries (64 KiB post data per request); `request_detail` on a dropped entry returns `request_evicted`.

### Contributors

- @leeguooooo

## 1.5.214

Requires extension ab-connect 0.5.57 for page-to-session matching (0.5.56 was never published; 0.5.57 includes it). Chrome Web Store publication follows Google review.

### New Features

- **Inspect & Ask finds the right project and session.** The first pick on a site (or a path of it) shows one card naming the session, its folder, the project and the evidence; **Hand over** makes it the default, **Only this tab** holds it for one tab. Only local evidence preselects: a committed `.chrome-use/inspect.json` naming the site's domains (single-label `*.` wildcards, per-path `pathScope`, shared `sourceRoots`), or the local dev server's folder, with exactly one running session there. Test environments rank candidates by CI source paths and by how the deployed revision relates to each worktree. (#628)
- **`chrome-use inspect link <url>`.** A session claims a site for itself (`unlink`, `list` too). When the site or an overlapping path belongs to another session, the claim waits for your Replace or Keep in the browser; until then requests keep going to the current target. (#628)

### Safety

- **What you see is where it goes.** Each send carries the page path and the target fingerprint the panel showed; a route change in a single-page app, an accepted claim, an unlink or a rebinding in between refuses the send instead of redirecting it. Sessions that ended or moved, a dev-server port now served from another folder, and different git worktrees never inherit a target. Copyable link commands quote the page URL for sh and PowerShell. Older extensions are refused on sites with path-specific targets rather than guessing. (#628)

### Bug Fixes

- **Side panel composer.** Stop and send stay together on the right and wrap as one group in narrow panels; the composer controls use one size. (#628)

### Contributors

- @leeguooooo

## 1.5.213

Requires extension ab-connect 0.5.56 for Inspect & Ask. Chrome Web Store publication follows Google review.

### New Features

- **Inspect & Ask: click an element, send the change to your agent.** Press Alt+Shift+C (or **Inspect element** in the toolbar popup), click one or more elements, write what should change and press Enter. chrome-use sends your words with each element's source `file:line`, a cropped screenshot and its styles through ocs to the Claude Code, Codex or Pi session already working on that project. Works on local dev servers and test-environment domains in your logged-in Chrome. Source locations come from code-inspector-plugin's `data-insp-path` marks, with React 18 and Vue dev metadata as fallbacks; absolute marks from CI builds are matched by path suffix inside the bound project only. Shift+click or drag to multi-select, ↑/↓ for parent/child, P to pause animations, a note and a pen per screenshot, **Copy** as Markdown and **Open in editor**. The first send on a site asks which project and session it belongs to; setting up source marks, or enabling them for test builds, is proposed with the exact changes and runs only after you agree. (#626)

### Safety

- **Page data never steers delivery.** The input box runs in the extension's own frame, authenticated with a one-time secret over a channel the page cannot read, so page scripts cannot read or change what you type, and a copied frame is refused. Each send re-checks that the panel is fully visible. Picking swallows the whole click, so selecting a button or link never acts on the page. Page text, HTML and source paths are labelled untrusted next to your instruction. (#626)
- **Delivery is never replayed.** Each batch has an id recorded before ocs runs; ocs's stored and wake results are shown separately, an unconfirmed outcome is queried rather than resent, and a tombstone of each id and its outcome is kept so a batch is never sent twice. Screenshots are attached only when the same tab, document and layout are confirmed before and after capture, and only to the selection they were taken for. Context files live in `~/.chrome-use/inspect` with owner-only permissions and are removed after 7 days. (#626)

### Contributors

- @leeguooooo

## 1.5.212

Requires extension ab-connect 0.5.55 or newer, including the selected-agent-only model probe.

### New Features

- **Full Windows agent panel.** Windows now supports Codex, Claude Code, OpenCode and Pi conversations, CLI model inspection, configuration preview/apply/restore, skills and imports, images, conversation permissions/options, scheduled prompts and panel-bound MCP tools. Native executable and batch entry points are supported without administrator access. (#622)

### Improvements

- **Owned Windows process trees and private state.** Agents enter a kill-on-close Windows Job before running; stop, delete and option changes confirm all owned descendants exit. Private state has a protected user/SYSTEM ACL, atomic writes preserve configuration ACLs, and file identities detect replacement. Windows Path and other environment names are matched without case sensitivity. (#622)
- **Windows runtime verification.** A dedicated Windows CI runs private-state/process-tree tests and the same native-host protocol acceptance used on macOS/Linux, with CRLF batch fixtures and all four adapters. (#622)

### Contributors

- @leeguooooo

## 1.5.211

Requires extension ab-connect 0.5.55. Chrome Web Store publication follows Google review.

### Bug Fixes

- **Opening the side panel no longer starts Codex for Claude Code users.** A new chat defaulted to Codex and read its model list, so the Codex CLI ran every time the panel opened, even for people who only use Claude Code. On a Mac with an outdated Codex build, macOS answered each open with "codex contains malware". The panel now remembers the agent you chose (or the one from your latest conversation) and reads models only for that agent. (#623)
- **A CLI blocked by macOS is named as blocked.** When a CLI is killed the moment it starts, as Gatekeeper and XProtect do, the panel now says the system stopped it and suggests updating or reinstalling, instead of "Model metadata unavailable". Agents & models shows the reason too. (#623)

### Contributors

- @leeguooooo

## 1.5.210

Requires extension ab-connect 0.5.54. Chrome Web Store publication follows Google review.

### New Features

- **Skills in the agent panel.** Type `/` to choose a skill, or manage a profile-shared library with folder, ZIP and HTTPS Git imports. Selected instructions and assets are saved with the message, so queues and retries retain the same revision. (#614)
- **Image attachments.** Paste, drop or select up to five PNG/JPEG/WebP images. The panel resizes them before transport and sends them through the selected agent's native image protocol. Images stay with queued edits, retries and conversation history. (#614)
- **Conversation options.** Set tool scope, Codex/Claude reasoning effort and Codex service tier without changing global CLI defaults. Backend processes exit before restarting with changed options. (#614)
- **Scheduled prompts.** Run interval, daily, weekday or five-field cron prompts in independent background sessions, with history and completion notifications. Missed periods coalesce; active or unconfirmed runs are not replayed. Scheduling preserves the creating conversation's tool and approval limits across adapters. (#614)

### Bug Fixes

- **CLI default effort is restored on resume.** Codex retains a rollout's old effort when the override is omitted. The panel now restores the configured effort or the model catalog's reported default explicitly. Missing run history disables its own schedule without stalling other tasks; wrapped skill archives import correctly and symbolic-link assets are refused. Skill snapshot instructions are verified, and restricted chats cannot grant Codex shell access or delete stronger schedules. (#614)

### Contributors

- @leeguooooo

## 1.5.209

Requires extension ab-connect 0.5.53. Chrome Web Store publication follows Google review.

### Improvements

- **The popup matches the side panel.** The toolbar popup, shown when Chrome has no side panel, drops the old cream-and-gold look for the side panel's colours, follows the system dark mode and has a **Settings** button. (#615)
- **New store listing images.** Four screenshots, a promo tile and a marquee are rendered from the real side panel and settings page by `scripts/store-screenshots.sh`, so they can be redone after any UI change. (#615)
- **A lighter repository.** README illustrations are WebP (4.4 MB down to 180 KB) and the README shows the side panel. The docs site's VIP badge shrinks from 1.38 MB to 6.6 KB, and unused images and old extension zips are gone. (#615)

### Contributors

- @leeguooooo

## 1.5.208

### Bug Fixes

- **macOS no longer blocks an agent's `.bun-…dylib`.** Agents started from the side panel inherited Chrome's file quarantine, so every file they wrote was quarantined. OpenCode, Claude Code and other Bun-built CLIs unpack native libraries to `$TMPDIR/.bun-<uid>-*.dylib` and load them, and macOS refused with "Apple could not verify '.bun-501-….dylib' is free of malware". The panel's agent service now starts through launchd, outside Chrome's process tree, so its agents write ordinary files. On start it also clears the quarantine from `.bun-*` libraries that earlier runs left behind. (#613)

### Contributors

- @leeguooooo

## 1.5.207

Requires extension ab-connect 0.5.52. Chrome Web Store publication follows Google review.

### Improvements

- **Tool steps fold into one line.** Consecutive tool calls in the side panel now form one group. It stays open while a step runs, then folds to `4 steps · 1 failed` so the reply below stays in view; a group you opened stays open. A running step shows a spinner instead of `Running…`, and a step left open when a response ended no longer looks busy. Three dots show the agent is still working when nothing else does. (#612)
- **A cleaner chat view.** The toolbar uses one icon set, queued messages read as pending bubbles, Markdown tables get rounded borders, and the empty state offers three example tasks that fill the message box without sending it. (#612)
- **Agents & models at a glance.** Each agent has a status dot (signed in, signed out, installed, not installed) instead of a red line, the agent being configured is highlighted, the model field suggests the model IDs the CLI reported and reasoning effort suggests its levels, and the preview shows each change as old → new. (#612)
- **Settings match the panel.** The extension settings page uses the side panel's colours and follows the system dark mode. (#612)

### Contributors

- @leeguooooo

## 1.5.206

### Bug Fixes

- **A refused first tab says nothing was opened.** When the extension declines to open a tab before creating anything (for example `agent window ownership is unknown`), `open` now fails with `first tab refused … Nothing was opened or attached.` (code `first_tab_refused`). It used to report `first tab outcome unknown` and tell you to look for a blank tab that was never opened. (#609)
- **`tab list --all` works after a refused `open`.** A refused first `open`, including `profile not open`, left a session daemon without a browser, and `tab list --all` in that session failed with `Browser not launched`. It now falls back to the same read-only listing a fresh session uses, still without attaching to or creating anything. (#610)

### Contributors

- @leeguooooo

## 1.5.205

Requires extension ab-connect 0.5.51. Chrome Web Store publication follows Google review.

### Bug Fixes

- **Your own tabs in the agent window no longer block new sessions.** If you opened or dragged your own tabs into the shared agent window, every later `open` failed with "the shared agent window contains user tabs" until you moved them out. That window now becomes yours: agent tabs already in it keep working, and new agent tabs go to one fresh agent window that later sessions share. Your tabs are still never moved or closed.
- **Recovery after a native host disconnect.** The extension retries the host after 1 second, backing off to at most 30 seconds, instead of waiting for Chrome's alarm. Command and event waits end when the old socket closes instead of hanging, and a lost reply reports `action_outcome_unknown` with `retryable: false`, so inspect the page before repeating an interrupted write. (#603)

### Contributors

- @leeguooooo

## 1.5.204

Requires extension ab-connect 0.5.49 for the conversation menu. Chrome Web Store publication follows Google review.

### New Features

- **Conversations for this site, or all of them.** The panel header opens a conversation menu with a "This site · x.com" / "All" switch. A conversation belongs to the main domain of the page its first message was sent from, so conversations started on mobile.x.com show on x.com. Each entry shows the agent, site and how long ago it was used, and the choice of view is remembered. Conversations from before this release have no site and appear under All.

### Contributors

- @leeguooooo

## 1.5.203

Requires extension ab-connect 0.5.47 for the new rendering. Chrome Web Store publication follows Google review.

### Improvements

- **Markdown in panel replies.** Agent replies render headings, bold and italic text, inline and fenced code, lists, quotes, tables and links instead of showing raw Markdown. Nothing in a reply is treated as HTML, and only http(s) links are clickable.
- **Readable tool rows.** Browser tool calls read "Browser · open" with the URL, action or command they acted on, instead of internal names such as `mcp__chrome_use__chrome_use_open`. Replies that contain only blank lines no longer leave an empty gap.

### Contributors

- @leeguooooo

## 1.5.202

### Bug Fixes

- **Deleting a panel conversation cleans up its browser session.** A session could keep a tab record that `close` can never verify, such as an old target id or a tab opened before Chrome restarted, so the delete left the session daemon running. Delete now tries `close`, then `close --forget-untracked`, then `session stop --force` for that conversation's own session. Only a plain close reports the tabs as closed; otherwise the panel says a tab may remain in the AI window.

### Contributors

- @leeguooooo

## 1.5.201

Requires extension ab-connect 0.5.46 for the longer panel waits. Chrome Web Store publication follows Google review.

### Bug Fixes

- **Slow panel operations.** The local panel service limited every request to 10 seconds and dropped the connection without an answer when one ran over. A cold OpenCode model check takes longer, so the OpenCode model menu could fail on first load, and a delete that waits for the agent to exit and then closes its browser tabs could run over too. Model checks now get 30 seconds and deletes 45 seconds, an overrun returns an error, and the layers above wait longer than the ones below.

### Contributors

- @leeguooooo

## 1.5.200

Requires extension ab-connect 0.5.45 for this fix. Chrome Web Store publication follows Google review.

### Bug Fixes

- **Every agent's status in Agents & models.** The local service runs at most two CLI checks at once, so with four chat agents two of them showed "status unavailable". The panel now queues the checks.

### Contributors

- @leeguooooo

## 1.5.199

### Bug Fixes

- **Pi model menu with OpenRouter aliases.** Model ids such as `openrouter/~anthropic/claude-opus-latest` are accepted, and a Pi model whose id still cannot be used is skipped instead of failing the whole menu.

### Contributors

- @leeguooooo

## 1.5.198

### Bug Fixes

- **Pi model menu with many models.** Pi lists every configured model; with more than 256 the model check failed. The menu now keeps the first 256 and marks the list as partial.
- **OpenCode refusal reason.** When OpenCode refuses a prompt, the panel shows OpenCode's reason, such as a retired default model, instead of only "OpenCode refused the prompt".

### Contributors

- @leeguooooo

## 1.5.197

Requires extension ab-connect 0.5.44 for the new agents in the panel menu. Chrome Web Store publication follows Google review.

### New Features

- **OpenCode in the panel.** Conversations run over OpenCode's Agent Client Protocol (`opencode acp`): streamed replies, tool rows, stop, queue, resume without replaying history, and its permission requests shown as cards. Full access allows those requests automatically. Without it the toggle reads "CLI permissions": OpenCode applies its own rules and may run tools without asking. Deleting a conversation calls OpenCode's own `session/delete`. The model menu comes from OpenCode's session options. Other MCP servers in the user's OpenCode configuration stay enabled (#590).
- **Pi in the panel.** Conversations run over Pi's RPC mode, with the conversation id as Pi's session id and its session file kept in the conversation folder, so resume is exact and deletion removes it. Pi never asks before a tool runs, so without full access it gets only the browser tools ("Browser only"); with full access it keeps all its tools. The user's other Pi MCP servers are turned off for panel conversations, and Pi extension dialogs are cancelled rather than left blocking a turn.
- **Host-reported agents.** The panel lists the agents the installed CLI can run; older CLIs keep Codex and Claude Code.

### Contributors

- @leeguooooo

## 1.5.196

Requires extension ab-connect 0.5.43 for the page chip. Chrome Web Store publication follows Google review.

### Improvements

- **"This page" means the tab you are looking at.** The panel sends the active web tab in its window with each message, shown as a removable chip above the input. The agent is told to read or act on that tab directly instead of listing every tab and guessing.
- **Less detour through the shell.** Panel agents are told the chrome_use tools are already connected, so they no longer read skill files, memories or documentation before acting.
- **Readable tool rows.** A command row shows the command and its output, and an MCP row its arguments and result. Model reasoning and untyped items are no longer listed as tool calls.

### Contributors

- @leeguooooo

## 1.5.195

Requires extension ab-connect 0.5.42 for the model menu. Chrome Web Store publication follows Google review.

### Improvements

- **Model menu.** The panel's model field is a menu filled automatically from the agent CLI's own catalog, with "Default model" (the CLI's configured model) first. A custom model ID in the + menu overrides it. One CLI probe per agent serves both the menu and Agents & models.
- **Clearer agent list.** Agents without a panel chat adapter (Pi, Gemini CLI, OpenCode and others) read "chat not supported in the panel yet" instead of "status unavailable".
- **Delete wording.** When chrome-use cannot confirm that a deleted conversation's browser tabs closed, the panel says a leftover tab may remain in the AI window (#583 tracks the underlying close issue).

### Contributors

- @leeguooooo

## 1.5.194

Requires extension ab-connect 0.5.41 for the access control. Chrome Web Store publication follows Google review.

### New Features

- **Full access mode.** A panel conversation can run without permission prompts. Codex starts with `approvalPolicy=never` and the full-access sandbox; Claude Code starts with `bypassPermissions` and its full tool set. Any approval request that still arrives is answered as "Allow once"; questions still go to the user. New conversations default to full access and remember the last choice. An existing conversation can switch in place, and the approval card's "Allow all in this conversation" releases requests that are already waiting. The next turn restarts the agent with matching flags.
- **Composer layout.** Agent, the access toggle and the model now sit in the input box's bottom row, following the Codex and Claude apps.

### Contributors

- @leeguooooo

## 1.5.193

Requires extension ab-connect 0.5.40 for the delete control. Chrome Web Store publication follows Google review.

### New Features

- **Delete a conversation.** The panel header has a delete button with a confirmation card. Deleting stops the conversation's agent and waits for it to exit, then removes the panel history, the agent CLI's own session history and the conversation's browser tabs. CLI history is matched by exact session id under `CODEX_HOME` / `CLAUDE_CONFIG_DIR`: Codex rollout files in `sessions/` and `archived_sessions/`, and Claude Code `projects/*/<session>.jsonl` with its folder. Anything that could not be removed is listed in the panel. Codex 0.162 also caches thread metadata and items in its own SQLite databases; those rows are left alone, and Codex no longer lists or opens the thread once its rollout is gone.

### Contributors

- @leeguooooo

## 1.5.192

### Bug Fixes

- **Panel agents run the same CLI as your terminal.** Chrome starts the native host with a minimal PATH, so a wrapper such as a version-manager shim could launch an older agent build than the terminal does. Codex 0.145.0 started instead of 0.162.1, and that older build rejected the configured model as "not supported". Agent conversations and the Agents & models check now run with the directories discovery searches, in the same order.
- **Model-failure action.** "Retry with default model" appears only when the conversation used an explicitly chosen model. When the failing model comes from the CLI's own configuration, the error offers "Change default model", which opens that agent's configuration.

### Contributors

- @leeguooooo

## 1.5.191

Requires extension ab-connect 0.5.39 for the panel changes. Chrome Web Store publication follows Google review; CLI browser automation remains compatible with 0.5.35.

### Improvements

- **Simpler agent side panel.** The header is one row: a conversation picker plus new, refresh and settings buttons. The connection line appears only when something is wrong. Agent and model are compact chips above a single composer, with the rest behind a menu. Stop appears only while a response runs. Your messages show as bubbles and tool calls as one-line rows that expand for details (#576).
- **Readable agent errors.** Raw API error JSON from the agent CLI is shown as its message, with the original text kept under Details. When a turn fails on an explicitly chosen model, a "Retry with default model" button starts a new conversation for the same agent on the CLI default and resends the last prompt (#576, #577).
- **Agent status at a glance.** Agents & models shows each installed agent's reported version, login state and model count on one line. The check runs only when that view opens (#576).

### Contributors

- @leeguooooo

## 1.5.190

Requires extension ab-connect 0.5.38 for the shared-window fixes. Chrome Web Store publication follows Google review; CLI browser automation remains compatible with 0.5.35.

### Bug Fixes

- **One shared AI window.** All AI sessions in a Chrome profile reuse one background window with separate tab groups, leaving the user's window separate. New windows use the first task tab directly, without an extra blank placeholder. Lost records recover verified agent windows, and fullscreen does not create a replacement. Mixed user/agent windows and uncertain ownership refuse new tabs instead of multiplying windows (#545).
- **Placeholder cleanup.** Older placeholders retain their identity when cleanup fails or cannot be confirmed. Incomplete setup names the tab already created so callers can inspect it before retrying (#545).
- **Read-only tab inventory.** Even a fresh `tab list --all` starts no daemon and creates no window or tab. Existing sessions keep their own browser; missing non-relay endpoints and daemon version mismatches are explicit refusals rather than guesses (#545).

### Contributors

- @leeguooooo

## 1.5.189

Requires extension ab-connect 0.5.37. Chrome Web Store publication follows Google review; CLI browser automation remains compatible with 0.5.35.

### New Features

- **Local agent side panel (macOS/Linux).** Open the extension panel to chat with your installed Codex or Claude Code using its existing configuration and login. Conversations persist locally, reconnect without resending messages, display streamed replies and tool returns, and support questions, approvals, stopping, queue edits/removal and direct sending after confirmed interruption. New chats use an independent browser session; binding the visible page is not included yet.
- **Agent discovery and configuration.** Discover installation/configuration evidence for 14 agent families, preview allow-listed changes, verify saved values and restore unchanged fields without overwriting later edits. Codex/Claude model suggestions come from their CLI metadata, with explicit partial/unavailable states. Finding a CLI, account or model does not prove execution access.

### Bug Fixes

- **Panel recovery.** Unconfirmed sends offer explicit same-ID retry or warning reset without automatic replay. Idle older agent services are replaced after verified shutdown while active work can still be read and stopped. Custom metadata programs require absolute paths. Long history replay seeks from validated byte cursors instead of rescanning each page.
- **Saved tab groups.** Agent-created tabs are ungrouped before closing so Chrome does not retain an empty saved group (#537).
- **Snapshot context.** Repeated controls receive context only when it helps distinguish them (#540).
- **Site inspection.** `site info <pack>` works and prints a runnable hint example (#539).
- **First-open and host compatibility.** A panel opened while connecting loads metadata after confirmation. Browser timer bindings retain their correct receiver. Older native hosts show an upgrade requirement rather than enabling unsupported panel operations. Windows panels remain read-only for agent discovery/configuration viewing until process ownership and private persistence support are verified; existing Windows browser CLI automation is unchanged.

### Improvements

- **Getting started.** Installation, aliases, banners and recording guidance are clearer (#538).

### Contributors

- @leeguooooo
- @mpeter

## 1.5.188

Requires extension ab-connect 0.5.35 (published on the Chrome Web Store; Chrome updates it automatically).

### Fixes

- **A tab or pop-up an agent click opens no longer leaves Chrome in front of the user's app (macOS).** Chrome activates itself when a page opens a tab (`target=_blank`, `window.open`, pop-up windows), before any extension or CDP event, so this can't be prevented; chrome-use now puts the previous app back. Before a `click` it notes the frontmost app; if Chrome comes to the front within 1.5 s with a new tab credited to that click (its opener or group is the session's), the previous app is brought back, front window only, without launching or opening anything and with no permission prompt. Measured with Finder in front: Chrome was frontmost for 41–72 ms instead of staying there. Nothing happens if Chrome was already in front, if another app came forward, or if Chrome came forward with no new tab (you clicked into it). The click reports `focusRestored`, `focusRestoredApp` and `focusRestoredAfterMs`, or `focusNote` with the reason. `AGENT_BROWSER_RESTORE_FOCUS=0` turns it off. Windows and Linux are unchanged. (#536, fixes #468)

### Contributors

- @leeguooooo

## 1.5.187

Requires extension ab-connect 0.5.35 (published on the Chrome Web Store; Chrome updates it automatically).

### Changes

- **OpenCLI is a last resort, and account actions use our own adapters.** Every site command that runs through OpenCLI now prints a stderr note that it is third-party code chrome-use does not maintain; for a write it adds that our own adapter is recommended and the agent should offer to port it into chrome-use-sites. `--json` keeps `source: "opencli"` and adds `maintainedBy: "opencli"`, `readOnly` and `notice`; `site info` shows `maintainedBy` too. Exit codes and behaviour are unchanged. The skill docs now treat OpenCLI as a last resort and say not to rely on it for posting, liking, following, deleting or publishing on real accounts. (#534)
- **`twitter/like`, `twitter/follow`, `bilibili/user-videos` and `bilibili/creator-stats` are ours.** They were ported into chrome-use-sites (chrome-use-sites#19, with attribution) and OpenCLI never runs them, even from a cache synced before the port. `like` and `follow` only act on the tweet or profile you named, never undo an existing like or follow, and confirm the result; run them with `--until-done`. The bilibili commands now return one object (`{mid, order, total, count, videos}` and `{bvid, title, source, values, metrics, missing}`) instead of OpenCLI's row list. (#535)

### Contributors

- @leeguooooo

## 1.5.186

Requires extension ab-connect 0.5.35 (published on the Chrome Web Store; Chrome updates it automatically).

### Features

- **`diag pages`: a read-only page report other tools can depend on.** `chrome-use diag pages [--json]` lists every real page (url, title, window, tab, active, discarded, owner) plus stale relay records, whether the extension is behind, the profile and the relay state, without attaching to anything. `--measure` adds heap, DOM nodes, event listeners and documents per page, and worker heaps; measuring a tab chrome-use doesn't hold needs `--force` (Chrome shows its debugging bar while measuring), and such tabs are attached, measured and released without being activated. `--watch SECONDS` samples twice and classifies leaks (`listeners_growing_nodes_flat`, `heap_growing`, `nodes_climbing`). The JSON is versioned (`schema: 1`, documented in the diag-pages reference); exit codes are 0 ok, 2 relay not connected, 3 another live session holds every page. Also as the MCP tool `chrome_use_diag_pages`. (#533, fixes #521)

### Contributors

- @leeguooooo

## 1.5.185

Works with extension ab-connect 0.5.34; 0.5.35 (submitted to the Chrome Web Store, Chrome updates it automatically once published) adds the update and large-reply fixes below.

### Bug Fixes

- **A failed `adopt` never leaves you on someone else's tab.** When `adopt <url>` finds no match, the session now has no current tab (remembered across daemon restarts) and commands that act on the current tab are refused with what to do next, instead of running in an unrelated tab you had open. `tab new`, `tab adopt`, `tab <id>` and `--force` still work, and user tabs an earlier adopt had grouped into the session are released (0.5.34+). (#527, fixes #507)
- **Large uploads over the relay work.** Files are streamed in bounded chunks and each reply is just a byte count, so 200 MB uploads complete (about 13 s on a test machine) where anything above about 45 MB used to time out, and a plain 31 MB `upload` takes about a second instead of over two minutes. Uploads to the same browser run one at a time; a waiting one says which session holds the queue. A page that navigates mid-upload fails the upload. (#528, #532, fixes #506)
- **Extension updates apply on their own (0.5.35).** A downloaded update used to wait forever while any driven tab stayed attached. Now the extension applies it once no command is running and the relay has been quiet for 60 s; sessions keep their tabs, ids and refs, and nothing is activated or closed. `status` and `doctor` show a pending update, what holds it back, the last update check and when the running version arrived. (#526, fixes #524)
- **Oversized replies fail at once (0.5.35).** A reply over Chrome's 64 MiB messaging limit returns `reply_too_large` (not retryable) instead of a 30 s timeout. (#526, fixes #530)

### Improvements

- **Our own site adapters always win over OpenCLI.** `douyin/delete`, `douyin/update` and `twitter/delete` are now maintained in chrome-use-sites (ported from OpenCLI, Apache-2.0, with attribution), run in the page with chrome-use's stealth handling, and never fall back to OpenCLI. `douyin/delete` pages through older works. (#529)

### Contributors

- @leeguooooo

## 1.5.184

Requires extension ab-connect 0.5.34 (published on the Chrome Web Store; Chrome updates it automatically).

### Features

- **See every tab you have open, and act on one when you ask.** `tab list --all` lists the tabs in all windows of the connected profile without attaching to or activating any of them. Each row says who owns it (this session, another session by name and whether it is live, or you), this session's tabs come first, and rows the session doesn't own say how to act on them. `--tab <handle> --force` runs a command on such a tab in place, in the background; `tab close <handle> --force` closes it and confirms it is gone. Without `--force` those tabs are refused, naming the owner. Ending a session never closes a tab it was forced onto; with 0.5.34 it releases it. (#520)

### Bug Fixes

- **`open` no longer reports a timeout for a page that loaded.** When the load event never arrives but the page is plainly usable (DOM ready, visible content, not an error page), `open` succeeds with a warning and `commit: "unverified"` when it can't prove the page came from this request, giving the real URL. A page that isn't usable fails with a non-retryable `navigation_commit_unknown` / `navigation_incomplete` that says what is still loading; it is never replayed. (#511, fixes #502)
- **`close` with nothing to close touches nothing.** A second `close`, or one for a session never opened, used to open and close an `about:blank` tab in your Chrome; it now does nothing and says so. (#518, fixes #517)
- **No phantom tabs over the relay.** Tabs that had closed could stay listed as live attached pages (and could be attached to); the extension now prunes them, and the CLI ignores any it can't confirm open, even with older extensions. (#522, fixes #519)
- **`douyin/delete` finds the right work.** 19-digit IDs keep every digit, and the work card is matched by title instead of by position, which could have clicked a different video. (#525, fixes #508)

### Contributors

- @leeguooooo

## 1.5.183

Requires extension ab-connect 0.5.33 (published on the Chrome Web Store; Chrome updates it automatically).

### Bug Fixes

- **`close` only reports closed tabs that are really gone.** After closing, it checks each tab by its exact Chrome tab id (over the relay through the extension's `tabPresence`, needs 0.5.33) and returns `tabsClosed`, `verifiedAbsent` and `verifiedBy`. A tab still open, or one it cannot confirm, makes it fail with `close incomplete`, listing the tab and keeping the session and its ownership record so a retry works; it never reports `closed: true` for a tab it didn't close. The whole close has one 15 s budget. `tab close` on the session's only tab, when the session created it, ends the session instead of refusing. Adopted and user tabs are never closed. (#496)
- **A profile with no window is reported as such.** `open` and `extension call` on a profile with no open window fail at once with `profile_not_open` (or `profile_window_unavailable` when the window list can't be read), create nothing and open no window, instead of reporting a stuck daemon after 45 s and leaving attached `about:blank` tabs behind. A first tab that can't be set up is closed again and verified; if that can't be confirmed the error says so and keeps the right to close it later. (#499, fixes #486)
- **`hover` no longer reports Done for a hover that didn't land.** It scrolls the element into view, finds a point where the element is on top, moves the real pointer there, and succeeds only when a trusted pointer event reaches the element. A covered element, or one inside a transformed frame, is refused before any input is sent. `hover` accepts `--observe` like `click`. (#509, fixes #500; #510, fixes #501)
- **Refs survive a failed post-action capture.** When only the observation after an action fails (`debugger_access_denied`), the previous refs are kept as unverified and used only after a live identity check, with a `kept_ref_unverified` error (not retryable) otherwise, instead of a misleading "Unknown ref". (#497)
- **`extract` supports the `"@attr"` shorthand** and refuses specs it cannot read instead of returning a silent null. (#512, fixes #504)
- **Settle names what kept the page busy.** Requests are dated by when the daemon received them, so requests left over from a previous page no longer count as the new page loading, and the warning names up to three pending requests. (#515, fixes #505)

### Improvements

- **Observations carry more of the page.** On small pages `--observe` returns the whole page (`observed.snapshot`) with a compact change list, plus `observed.text` with visible text that appeared or went away across frames (password and editable content excluded), so a receipt like "Page 2 of 3" or an iframe confirmation is visible without another snapshot. `--no-timing` / `AGENT_BROWSER_TIMING=0` drops the timing block. (#498)
- **`read --links`** appends the page's links as absolute URLs (100 by default, up to 1000 with `--max-links`), bounded in size and honest about what it left out. (#513, fixes #503)

### Contributors

- @leeguooooo

## 1.5.182

Requires extension ab-connect 0.5.32 (published on the Chrome Web Store; Chrome updates it automatically).

### Bug Fixes

- **A relay restart no longer breaks a `--browser` session or `close`.**
  - A session bound with `--browser <profile>` waits for that profile's new endpoint instead of failing on the old one, and keeps its tab ids and refs, under the same rules as other sessions. It only ever reconnects to its own profile. (#491, fixes #484)
  - `close` after the relay restarted while the session was idle reconnects first and really closes the session's tabs. If any tab is left open, it says `close incomplete`, keeps the session so the retry works, and never reports `closed: true`. (#492, fixes #485)
- **A busy session is no longer killed.** A command that takes more than 45 s (a slow click, a long `eval`) is kept alive with keepalives instead of being killed. A second command queued behind it no longer times out or takes the daemon down, and a queued command whose caller has gone away is never run late. (#488, #494)
- **Input over the relay gets 25 s instead of 8 s,** so a click on a page that blocks for a while completes once instead of leaving a half-click. (#489)
- **A tab recovers after a debugger timeout,** and concurrent sessions opening tabs no longer race; agent tabs stay in one background window, and a service-worker restart no longer opens a second agent window. (#478, #493)

### Improvements

- **`doctor` and `status` report relay health:** whether the debugger has been exercised, is healthy or timed out (reported, never repaired), duplicate copies of the extension, a version skew between the extension and the CLI, and machine load. They never launch a browser or touch a tab. (#490)

### Contributors

- @Sean529
- @leeguooooo

## 1.5.181

### Bug Fixes

- **`auth login --bwu` no longer reports a login the site never got.** On your own Chrome (extension relay), on a background tab, the Enter that submits the form could be accepted by Chrome and never reach the page: nothing was submitted, yet the command said `submitted: true` and auto-login said `ok: true` while the site stayed signed out. The Enter is now checked; one that did not arrive is replaced by a click on the form's sign-in button. And a login is reported only once the sign-in form is gone from the page (`signedIn: true` with the `evidence`); a form still there after about 12 s, or a page that cannot be read, fails with `the sign-in was not confirmed: …` and what the page shows. A tab that detaches under the Enter is judged by the page afterwards instead of failing with `action_outcome_unknown`, and `alreadySignedIn` needs a loaded page without a sign-in form. The login wall tries once more when the outcome is unknown or the page gave no reason. (#487, fixes #482)

### Contributors

- @leeguooooo

## 1.5.180

### Improvements

- **A login wall asks whether to sign in, instead of needing a config switch.** The first wall on a site (a redirect to its sign-in page, or a `site` adapter that finds it signed out) asks: sign in from the vault this time, always for that site without asking, or never. A person at a terminal gets a prompt. An agent gets `loginWall.ask` and the same text on stderr: a question to relay to the user as is and one command per answer (`auth login --bwu` on the sign-in page, `auth autologin always <host>`, `auth autologin never <host>`); the core skill tells it to ask and never to choose `always` itself. Decisions are stored per site in `~/.chrome-use/autologin.json`; `auth autologin status` shows them and `auth autologin off <host>` (or `--all`) forgets one. `"auth": {"autoLogin": "bwu"}` and `AGENT_BROWSER_AUTO_LOGIN=bwu` keep working as "always"; `AGENT_BROWSER_AUTO_LOGIN=ask` / `off` force asking or never. (#483, fixes #481)

### Bug Fixes

- **Auto-login tries once more when the outcome of the login submit is unknown.** A tab that detached during the Enter that submits the login (`action_outcome_unknown … Detached while handling command`) left the site signed out; auto-login now goes back to the sign-in page and runs `auth login --bwu` once more, which types nothing on a page that is already signed in. The cause of the detach is still open. (#483, refs #482)

### Contributors

- @leeguooooo

## 1.5.179

Requires extension ab-connect 0.5.31 (published on the Chrome Web Store; Chrome updates it automatically). Only the opt-in background links below depend on it.

### Bug Fixes

- **A site adapter whose site is signed out is a login wall.** An adapter can now say "not signed in" in one documented way, `loginRequired: true` (optionally with `loginUrl`, the sign-in page); `error: "login_required"` / `"not_logged_in"` mean the same. A run that fails after one of its `fetch` calls got HTTP 401 or was redirected to a sign-in page, or that left the tab on one, counts too. The command then fails with `login wall: <host> is not signed in …` on stderr and `loginWall` in `--json`, pointing at `auth login --bwu` instead of "log in in Chrome first", and a write that the site refused for want of a login is no longer reported as a failed write. With `"auth": {"autoLogin": "bwu"}` (or `AGENT_BROWSER_AUTO_LOGIN=bwu`) chrome-use opens the sign-in page, signs in from the vault and runs the command once more; a write is rerun only when the adapter reported the login itself. `auth login --bwu` without `--item` now uses the one vault item named exactly the site's host when the domain matches several. (#480, fixes #479)
- **A relay restart no longer breaks a session.**
  - **Tabs the session created stay its own**, so `close` and `tab close` work on them again. They are matched by the Chrome profile's identity, never by the relay's address, and tabs another session claims are never taken. (#469, fixes #461)
  - **A session is bound to its Chrome profile, not the relay host's address**, so `--browser <same profile>` follows the profile to its new endpoint, waiting up to 20 s if it is not connected yet and never falling back to another profile. (#476, fixes #472)
  - **Tab ids and refs stay on the same tabs.** t1, t2, ... keep pointing at the same tabs, and the first command no longer says the tabs are gone. If the tab being driven navigated or closed during the outage, its `@eN` refs are dropped instead of being matched on another page, and commands on the current tab are refused until a tab is chosen. A lost tab's label stays reserved. The tab record is written atomically, and a record that can't be read holds the session instead of renumbering. (#475, fixes #473)
- **A click that makes the page open a tab says when Chrome came to the front** (`openedTabWarning`). `AGENT_BROWSER_BACKGROUND_LINKS=cross-site` opens plain cross-site `target=_blank` links in a background tab instead, at the cost of one history entry and `SameSite=Strict` cookies on a link that redirects back to the page's site; it is off by default and needs ab-connect 0.5.31. (#474, #477, refs #468)

### Contributors

- @leeguooooo

## 1.5.178

Requires extension ab-connect 0.5.30 (published on the Chrome Web Store; Chrome updates it automatically).

### Bug Fixes

- **chrome-use no longer pops up browsers or grabs your tabs.**
  - The extension used the user's own window as its "agent window", and new agent tab groups were created in the last-focused window. That moved agent tabs next to the user's tabs, so the user's visible tab jumped when one closed. Agent tabs now stay in a background agent window that holds nothing else, and the extension never falls back to the user's window.
  - Unit tests and `doctor` launched visible browsers: unit tests no longer launch one, and with the relay up `doctor` checks the relay instead.
  - `screenshot --tab`, `tab duplicate`, `state save` and the password-manager menu recovery no longer touch the user's foreground or tabs.
  - `--activate`, `bringtofront` and `adopt` still work when asked for, and agents are told not to use them otherwise.
  - Known gap: a page that opens a pop-up in response to an agent click can still bring Chrome forward (#468). (#460)
- **A pop-up opened by the session's own tab is adopted over the relay.** It used to go unreported, unfollowed and left open after `close`. It is now attached by its exact Chrome tab id, reported as `openedTab`, followed with `--follow` (and settled on), and closed with the session. A tab that can't be confirmed as the session's is reported as `unadopted` and left alone; user tabs and other sessions' tabs are never taken. (#457, fixes #456)
- **A ChooseBrowser rule is binding.** A site the rule assigns to a profile is never opened in another profile without notice. If that profile isn't connected, the command says so and gives `chrome-use connect --browser <profile>`. A session bound to another profile refuses the site and suggests a new `--session` or `--no-choosebrowser`. This covers `open`, `goto`, `navigate`, `tab new`, `batch`, scripts and MCP. `doctor` lists each rule and whether its profile is connected. (#466)
- **`find` refuses ambiguous actions.** `find role button click --name Save` with two Save buttons used to click the first; it now refuses and lists the candidates. A unique scope (`--within`) narrows the search, and `first`/`nth` stay available for an explicit choice. (#459)
- npm postinstall no longer downloads from the wrong repository; it points to the installer (#467, fixes #423). Removed the unused "Approved sites" option card (#465, fixes #424). Fixed the real-chrome skill's frontmatter and added a YAML check in CI (#464, fixes #425). `AGENT_BROWSER_STATE_EXPIRE_DAYS` is documented as opt-in (#463, fixes #426), and the unused `AGENT_BROWSER_HOME` was removed from the docs (#462, fixes #427).

### Contributors

- @AmeerAliAnwar
- @leeguooooo

## 1.5.177

### Improvements

- **An observed click no longer waits half a second for nothing.** The page-change watcher used by `--observe` was attached after the click, so it missed the page's immediate re-render and always sat out the 500 ms first-reaction window. It is now attached just before the action. The settle rules are unchanged: 100 ms of DOM quiet, no running animation, no request in flight, a 1 s ceiling. On a three-page catalog task, each `click --observe` went from about 740 ms to about 220 ms, and the whole task from about 3.5 s to about 1.4 s (median of 6 alternating rounds against 1.5.176, same timing boundary; form and delayed-load tasks unchanged). With `--follow`, the opened tab is settled on its own, never reported quiet from the opener. (#455)

### Bug Fixes

- **`pick … --observe` returns an observation**, as the core skill says. It used to be ignored with a warning. (#455)
- **Flags inside a `batch` step apply to that step.** `--observe` in a step used to be dropped. An explicit `false` (`--if-present false`) overrides the batch-level flag, and a step's `--tab` never overrides a tab the command names itself. (#455)
- **`screenshot <path> --selector <sel>` works in any order.** Unknown options are refused instead of being read as a selector or path, and XPath selectors such as `//main` keep the selector-then-path order. (#455)
- **A `fill` whose element handle was lost mid-batch is not typed twice.** On "Could not find object with given id", chrome-use finds the field again and reads it before writing. A matching value reports success with a warning; a different value is filled once; a value it cannot read fails as unknown, and nothing is written. (#455)
- **A `click` that opens a tab reports `openedTab` again without `--follow`.** (#455)

### Contributors

- @leeguooooo

## 1.5.176

### Bug Fixes

- **`auth login --bwu` on an already signed-in page returns at once.** It used to wait 25 seconds for a login field and then fail with "no login field appeared", which agents read as a failed login. Now a page that isn't a sign-in URL and shows no login field after a short look returns `alreadySignedIn: true` in about 3 seconds, with nothing typed. Sign-in pages keep the full wait. (#453)
- **The first command after an upgrade says refs were lost, even when the old daemon had already idled out.** In that case the session's tab was kept, but the first `@ref` command said "no snapshot has run in this session" instead of explaining the upgrade. Daemons now leave a version record that outlives them, so the new one can tell it replaced an older version. (#454, refs #448)

### Contributors

- @leeguooooo

## 1.5.175

### Bug Fixes

- **An upgrade no longer throws away a running session's tab.** When the CLI found its daemon on an older version, it stopped the daemon in a way that closed every tab the session had opened. The next command ran on a blank page with "Unknown ref". Now the session's own tab is kept, refs carry over (and still go through the identity checks), and the first command says what happened. When upgrading from 1.5.174 or older, the old daemon can't hand over its refs: the tab is still kept, and the first `@ref` command says to run `snapshot -i`. (#450, fixes #448)
- **`auth login` and `auth login --bwu` work with sign-in forms inside same-site iframes**, such as Apple's sign-in on App Store Connect. They fill the frame's fields with trusted input, handle username-then-password forms, submit with the sign-in button (never a passkey or Google button), and report a 2FA code step without guessing. (#452, fixes #449)
- **A password manager's inline menu is cleared before every command that touches the page**, including `get text`, `eval`, `press` and frame commands, not just `click` and `fill`. If Chrome's whole window is hidden (for example full screen in another Space while you're in the terminal), the menu can't be closed without taking your focus. The error now says so: bring Chrome to the front once, or press Escape in that tab. (#452)

### Contributors

- @leeguooooo

## 1.5.174

### Bug Fixes

- Fixed **repeated unchanged action hints across CLI calls** being reset by the CLI's browser readiness check. A successful launch reuse on the same connection, target and session keeps the observation streak; rebinding, failed checks and loading storage state still clear it. (#447)

### Improvements

- **Real CLI regression coverage** checks launch handshakes, actual button activations, batch postconditions, failed and nested script advisories, and cleanup against an explicitly selected binary. (#447)
- **JSON output guides** distinguish ordinary command envelopes, batch arrays and bare script results so callers read the actual advisories and timing fields. (#447)

### Contributors

- @leeguooooo

## 1.5.173

### Improvements

- **A repeated action that changes nothing is flagged.** When an action reports success but repeated complete, settled observations show no change (no requests, resources or frame changes), `--observe` adds `observed.noProgress`. It is advice only: it doesn't change `success` and nothing is replayed. A popup, a dialog or an incomplete observation resets it. (#444)
- **Nested script failures are no longer hidden.** A nested program that returns `ok:false` now fails its caller instead of passing on transport success alone. Failed and nested runs keep up to 20 advisories. (#444)
- **`timing` separates total Chrome time from wall-clock time.** It reports summed request time (`cdpMs`), the union of request intervals (`cdpBusyMs`) and the rest (`nonCdpMs`). The two independent snapshot enrichment reads now run concurrently. (#444)
- **Static pages have search metadata and bilingual usage guides.** (#442, #445)

### Contributors

- @leeguooooo

## 1.5.172

### New Features

- **Chrome profiles by name, routed, and connected on demand.** (#443, fixes #437)
  - `chrome-use browsers` lists every Chrome profile with its display name, directory, account and whether it is connected, plus a `connect` command for each unconnected one.
  - `--browser` accepts a display name, directory, email, id, or a unique name prefix (`--browser dav`).
  - A session prints which profile it uses (and why) on first attach and on `open`.
  - `browsers --who github.com` shows which profiles look signed in to a site. It reads cookie names only, never values.
  - `"profiles": {"default": …, "routes": […]}` in `~/.chrome-use/config.json` picks a profile by URL.
  - `chrome-use connect --browser <name>` opens the extension's store page, a window, or its settings in that profile, as needed, and waits for it to connect. So a profile is set up the first time it is needed, not all twelve up front.
- **Login walls are detected.** When a page lands on its site's sign-in page, chrome-use says so once per host and points to `chrome-use auth login --bwu`. With `"auth": {"autoLogin": "bwu"}` it signs in from the vault and returns to the original page. (#438, fixes #434)

### Bug Fixes

- **A click on a background tab no longer takes 5 seconds after a `fill`.** On a profile with a password manager, filling a login field left the page hidden, and Chrome took about 5 s to deliver each mouse event to a hidden page. chrome-use now makes the page render again before a pointer event: about 0.2 s instead of 5.2 s. (#439)
- **`chrome-use report --submit` files through the github.com form in the user's Chrome** when `gh` is unavailable. The API-based site adapter could never authenticate. Long bodies are filled in, not truncated. (#440)
- **`report --new` is now `report --new-issue`.** `--new` is also the global `--launch` alias. (#441)

### Contributors

- @leeguooooo

## 1.5.171

### New Features

- **`chrome-use report` drafts a GitHub issue from what went wrong.** It builds the draft from the local failure log, with version, OS, extension and connection mode. URL queries, cookies, tokens, typed values, emails and home paths are redacted first. It searches open issues for the same failure signature; when one matches, it adds a "+1, also seen on …" comment instead of a duplicate. `--submit` files through `gh`, the user's logged-in Chrome, or a prefilled issue URL. It refuses unless the user agreed (`--yes`, or `report.auto` in `~/.chrome-use/config.json` / `AGENT_BROWSER_REPORT_AUTO=1`). When the same failure repeats or an `eval` follows a failed command, the reply suggests offering a report once (`reportSuggestion`); `AGENT_BROWSER_NO_REPORT_HINTS=1` turns that off. (#436)

### Bug Fixes

- **A password manager's inline menu no longer blocks the tab.** When Bitwarden's autofill menu made Chrome refuse access to the tab, the old workaround (#373) never actually hid the page, and background tabs had no recovery at all. chrome-use now closes the menu and continues: `snapshot`, `fill`, `click` and `press` work, a click runs once, and your foreground tab is left as it was. If it still cannot recover, the error says not to close tabs, stop the session or relaunch. (#435)
- **A daemon that drops the connection is cleared immediately** instead of being retried five times. (#421, thanks @AmeerAliAnwar)
- **`adopt` and `extension connect` use the selected profile's relay**, and a session bound to one profile refuses to switch to another. (#422, fixes #400 and #403, thanks @AmeerAliAnwar)
- **Refusals no longer offer the `--force` override.** An agent refused by `tab select --activate` reran it with `--force` the moment the error named it. The core skill now says a background tab still receives clicks, so wait for the result instead of activating it. (#433)

### Contributors

- @AmeerAliAnwar
- @leeguooooo

## 1.5.170

### Improvements

- **A `wait --text` timeout names the page's actual wording when only the case or spacing differs.** Waiting for "Grand Total" on a page that says "Grand total" used to time out after 25 seconds with only a general reminder that matching is case-sensitive, and agents then fell back to `eval`. Now the error says the page does show "Grand total" and gives the exact `wait --text` to use. (#432)
- **The core skill's `extract` example runs as written.** Agents copied the placeholder `extract --schema '{rows,fields}'` literally and got a JSON error. (#432)

### Contributors

- @leeguooooo

## 1.5.169

### Bug Fixes

- **`close --all` no longer closes other people's sessions by accident.** While another session is live, it closes nothing, lists the other sessions, and says to run `chrome-use close` for your own or add `--force` for all. In an 8-agent test, two agents ran `close --all` when stuck and wiped the other seven sessions mid-task. (#429)
- **A session closed from outside says so.** Its next command used to run silently in a new blank tab. Agents saw `about:blank`, assumed the form was lost, and submitted twice. Now the next command warns that the tabs were closed by `close --all` (or `session stop`/`prune`) from a named session, and says to check whether a submission already went through before redoing it. (#431)
- **`tab select --activate` won't hide another session's tab.** While another live session's tab is in front of the window, it is refused; add `--force` to override. The background-tab note now explains that a click there usually did reach the page: wait for the result and re-read, and don't resubmit. (#431)
- **`pick` handles autocomplete fields.** `pick @e6 --option "Kyoto"` types the text, waits for the suggestions, clicks the best match ("Kyoto" over "Kyoto Station") and checks the field took it. `pick <ref> "<text>"` works too, and `select` on a non-native combobox points to `pick`. Almost every agent in the test fell back to typing and clicking by hand. (#430)

### Improvements

- **The core skill again lists the commands to use instead of `eval`**, in 5 rows: text, click, counting, waiting, and setting values. (#428)

### Contributors

- @leeguooooo

## 1.5.168

### Improvements

- **A ref that no longer matches its snapshot is never acted on as a guess.** When a `@ref`'s element was replaced, chrome-use still re-finds it. If the replacement has the same role and name, the action runs and the reply reports it (`relocated` in `--json`, a `⚠ @eN relocated` line on stderr). If the best match has a different name ("Delete" → "Delete all"), nothing is clicked. The error offers it as a new ref, `try @e7 [button] "Delete all"`, along with up to two other close matches. A text field re-found by its `id`, `name` or `data-testid` is still filled after its label changes, and reported (#356). `Unknown ref` errors now say what the ref was and suggest current refs. Borrowed from callstack/agent-device. (#419)
- **`scroll down --until <selector|@ref|text=…>`** scrolls step by step until the target is in view, in one call. It stops at `--max-steps` (default 30), the timeout, or the end of the page, and fails with how far it got. `--until-text "…"` and `--selector <container>` work too. (#420)
- **A snapshot of a canvas page comes with a screenshot.** When the tree is near-empty because a canvas fills the page, the snapshot attaches a 1200px screenshot path (`data.screenshot`). That saves a round trip. `AGENT_BROWSER_SPARSE_SCREENSHOT=0` turns it off. (#420)
- **The core skill is a third of its size.** `skills get core` is 6 KB of rules and routing instead of 15.7 KB; the detail moved to the `core/<topic>` references. (#416)

### Bug Fixes

- **`--observe` before any snapshot no longer renumbers unchanged elements.** The delta reported unchanged links as removed and re-added under new refs, so a ref the agent held could point at a different control. (#418)
- **`--observe` no longer lists other extensions' requests**, such as `chrome-extension://…/locales.json`. (#418)
- **Local pages no longer suggest OpenCLI's desktop-app commands.** Every `localhost` page was offered 19 `antigravity/*` commands meant for a local Electron app. (#418)
- **Extension tests no longer fail on a busy machine.** The duplicate-tab deadline tests now run on an injected fake clock instead of real 5–20 ms budgets. These tests had made two release preflights fail. (#417)

### Contributors

- @leeguooooo

## 1.5.167

### Bug Fixes

- **`chrome-use status` no longer reports a dead relay as up.** When the native host exits abruptly, its relay address file stays on disk, and `status` used to read that file as a live connection, showing cached extension and profile details as current. It now asks the extension for a reply, waiting up to 10 seconds on a silent connection. If none arrives, it reports the relay as down and the cached details as unknown. (#411, thanks @Sean529)

### Contributors

- @Sean529

## 1.5.166

### Improvements

- **`click` refuses a target that something else covers.** It used to click the element through the DOM (`element.click()`, `isTrusted=false`) and only warn, so agents often believed they had hit the real control. Now it fails with `click refused: #b is covered by <div id="cookie"> "Cookie banner" at its click point…`. `click --allow-dom` clicks it through the DOM anyway. `check`, downloads and sign-in keep the old fallback, because a styled checkbox covering its own hidden input is normal there and they verify the result themselves. (#414)
- **Every reply says where the time went.** `--json` replies carry `timing: {ms, cdpMs, cdpCalls, slowest}`, the costliest Chrome calls, and the daemon logs one line per command to `~/.chrome-use/timing.jsonl` (no URLs or page content; rotated at 20 MB; `AGENT_BROWSER_TIMING_LOG=0` turns it off). (#414)
- **Screenshots default to 1200 px on the longest edge, down from 2000.** That is about 1.2k image tokens for a viewport instead of ~3.3k. Full-page shots are capped by width only, so a long page stays readable. `--full-res` keeps the captured size. (#414)

Borrowed from iphone-use.

### Contributors

- @leeguooooo

## 1.5.165

### New Features

- **`eval --background <expr>` runs a slow expression past the relay's 8-second limit.** It starts the expression in the page and polls for its value, the way `site` adapters already run. A value that happens to contain an `error` field still counts as data. (#413)

### Bug Fixes

- **OpenCLI commands that wait inside the page no longer time out.** `jd/search`, for example, waits in the page for results and was cut off after 8 seconds with "relay timeout". Its evaluations now use `eval --background`. (#413)
- **OpenCLI commands that need no browser run.** Commands like `pubmed/search` failed with `Cannot read properties of null (reading 'query')`, because they were handed a page they do not take. chrome-use now calls them the way OpenCLI does, using OpenCLI's own argument preparation and routing. (#413)

### Contributors

- @leeguooooo

## 1.5.164

### New Features

- **Rotating Yidun sliders.** `solve-slider` measures translation and rotation while dragging, matches the main puzzle silhouette, and checks the current question’s result. Unsupported or ambiguous shapes fail explicitly. (#412)

### Bug Fixes

- **Slider failure is a failed command.** Exhausted attempts now exit nonzero with `success:false`; a hidden widget’s old success cannot approve the current question. (#412)
- **Cross-origin response bodies come from the frame that made the request.** Network detail uses the recorded renderer session and reports `responseBodyError` when the body is unavailable. (#412)

### Improvements

- **Agents attempt ordinary CAPTCHAs during authorized tasks.** The bundled guide covers sliders and ordered image clicks, bounded retries, foreground coordinate measurement, and site-level acceptance. A provider success or resend label alone does not prove SMS delivery or login. (#412)
- **Development builds and tests can run over SSH.** Configure a build host for `build:native`, `build:190`, and `test:190`; the runner uploads Git-listed working-tree contents, serializes its Cargo cache, and verifies downloaded binaries against SHA-256 receipts. SSH failure never starts local compilation. (#412)

### Contributors

- @leeguooooo

## 1.5.163

### Improvements

- **A failing adapter no longer hides a working OpenCLI command of the same name.** When one of our adapters fails and OpenCLI has a read command with that name, chrome-use runs OpenCLI's instead. Example: `hackernews/top` from the community pack fetches an API the page's security policy blocks, so it always failed; it now returns data through OpenCLI. Your arguments carry over by name, with common aliases mapped (`count` → `limit`, `q` → `query`). stderr says what happened; `--json` adds `source: "opencli"` and `fallbackFrom`. Write commands never retry. (#409)

### Contributors

- @leeguooooo

## 1.5.162

### Bug Fixes

- **OpenCLI commands install on Windows.** `site update` looked for `npm`, but on Windows it is `npm.cmd`, so OpenCLI was silently skipped there. (#408)
- **An OpenCLI command can no longer run forever.** It stops after 300 seconds, or after its own `timeout` argument plus 60 seconds when that is longer (login flows wait for you), and says so. `AGENT_BROWSER_OPENCLI_TIMEOUT=<seconds>` changes the limit. (#408)

### Contributors

- @leeguooooo

## 1.5.161

### New Features

- **OpenCLI's commands run as `site` commands.** With Node.js 20+ on PATH, `site update` installs a pinned [OpenCLI](https://github.com/jackwener/OpenCLI) (1.8.8, about 180 sites and 1,300 commands) into `~/.chrome-use/opencli` with `npm install --ignore-scripts`; no token is needed. A `name/cmd` that neither of our packs has runs through OpenCLI's own runtime, but every browser step goes through chrome-use, so it uses your current session and logins: `chrome-use site hackernews/best --limit 5 --json`. They are marked `(opencli)` in `site list`, `site info` shows their args, and the site hint lists them after ours. Our adapters win on a shared name. `AGENT_BROWSER_SITES_NO_OPENCLI=1` turns this off. (#407)
- **`site analyze [url]` shows where a page's data comes from.** It lists the same-site API calls the page made, the state it embeds (`__NEXT_DATA__`, `__INITIAL_STATE__`, JSON script tags) and any anti-bot vendor. It then recommends reading the site's own API from the page, then embedded state, then the DOM, the order in which they break least, and lists next steps. Do the action that loads the data first, then analyze. (#407)
- **`site verify <name>/<cmd> [args]` catches a broken adapter.** `--write-fixture` records the shape of a good result in `~/.chrome-use/site-fixtures/` (types only, no values). Later runs fail with exit 1 when a field disappears, changes type, or a list comes back empty. Works for OpenCLI commands too. (#407)

### Contributors

- @leeguooooo

## 1.5.160

### Bug Fixes

- **Switching to a tab that is still loading announces its site adapters.** A tab opened in the background can still read `about:blank` from the page when you switch to it, so 1.5.159 missed that site's `siteAdapters` hint. The hint now uses the url the command reported, taken from Chrome's tab info, and checks again on the next command if the site is still unknown. (#406)

### Contributors

- @leeguooooo

## 1.5.159

### New Features

- **Site adapters are announced whenever you reach their site, not only on `open`/`snapshot`.** `tab new`, switching or closing tabs, `back`/`forward`/`reload`, a click or key press that navigates, `read`, and the first command on a tab the session did not open now attach the same `siteAdapters` hint when the page is on a different site than the last one announced. Staying on one site, you hear about it once. The text hint also names `chrome-use site info <pack>` for the arguments. (#405)
- **A site you drive a lot without an adapter gets a suggestion to write one.** After 30 actions on a site in one session, or on a third day of use, one response carries `siteAdapterSuggestion` (stderr: `site adapter suggestion: …`). It tells the agent to ask you before writing anything. It comes once per site per session and not again for two weeks; local hosts and IPs are skipped, and `AGENT_BROWSER_SITES_NO_SUGGEST=1` turns it off. Only hosts and dates are kept, in `~/.chrome-use/site-usage.json`. The agent guide explains how to write your own adapter in `~/.chrome-use/my-sites` and register it with `site add`. (#405)

### Improvements

- **The official adapter pack takes precedence over the community pack.** `site update` records which pack each adapter came from. When both packs ship the same `name/cmd` (today `twitter/search` and `twitter/thread`), the official one is used, and for each site the hint lists official and your own adapters before community ones. (#405)

### Contributors

- @leeguooooo

## 1.5.158

### Bug Fixes

- **Windows: a call no longer hangs after the session daemon starts.** The daemon was spawned with handle inheritance on, so it also held the stdout/stderr pipes of whoever ran chrome-use. The `chrome-use.exe` the caller started exited normally, but a caller reading its output to the end (Rust `Command::output()`, Python `subprocess.run`) kept waiting until the daemon exited, up to its 10-minute idle timeout. A new daemon starts after an idle exit, on `adopt`, or after a version change, so the relay seemed to work and then hang after idle. The standard handles are no longer inheritable when the daemon is spawned. (#399, likely cause of #392)

### Contributors

- @leeguooooo

## 1.5.157

### New Features

- **`auth login --bwu` signs in with vault passkeys.** In a `--launch` browser with bitwarden-use 0.9.0+, a temporary WebAuthn authenticator can answer a passkey or security-key second factor; `--passkey` signs in with the passkey alone, without reading passwords, TOTP or custom fields. Synced passkeys report counter 0, as with the Bitwarden extension; nonzero counters are refused until vault write-back is supported. Only sign-in controls are clicked, normal passkey registration calls are blocked during the attempt, and the authenticator and registration guard are removed afterwards. Retained native function references can bypass the page guard; unexpected credential creation aborts the command, as does unconfirmed cleanup. Site refusals include the visible error message. Passkeys are explicitly unsupported on the extension relay: `--passkey` fails immediately and ordinary login retains the password/TOTP flow. (#398)

### Bug Fixes

- **`auth login --bwu --no-submit` skips TOTP.** Sites can submit a code on its last digit, so fill-only mode reads neither TOTP nor passkeys. `--passkey --no-submit` is refused. (#398)

### Improvements

- **Vault login is discoverable in help and the agent guide.** Help, both READMEs, and the bilingual login docs describe account selection, passkey compatibility and authenticated-destination verification. (#398)

### Contributors

- @leeguooooo

## 1.5.156

### Bug Fixes

- **The cookie argument checks that 1.5.155's notes described actually ship now.** In 1.5.155 the parser half of #395's review fixes never applied: `cookies get --url` returned the current page's cookies instead of that URL's, and `cookies get`/`clear` still let unknown or repeated arguments through. The daemon already refused `--name` on a full clear, so the browser could not be wiped. Now `cookies clear --name` needs `--domain` or `--url`, `--domain` with `--url` or a flag given twice is an error, and `cookies get` takes `--url` (repeatable) and rejects anything else. (#396)

### Contributors

- @leeguooooo

## 1.5.155

### Bug Fixes

- **`cookies clear` no longer wipes the whole browser when asked for one site.** It ignored every argument and cleared all cookies: `cookies clear --domain platform.openai.com` on a real Chrome profile signed the user out of GitHub, claude.ai, x.com and every other site, while printing `✓ Cookies cleared`. Now `--domain <d>` (or `--url <u>`, using its host) deletes that domain's and its subdomains' cookies and never a parent domain's, `--name` narrows it to one cookie, and the result says how many were cleared and where. Clearing every cookie needs `--all --yes`; without `--yes` nothing is deleted and it says how many cookies on how many sites would go. Unknown arguments to `cookies clear`, `get` and `set` are errors instead of being skipped. Partitioned cookies are deleted too. (#395)
- **`auth login --bwu` no longer submits an empty 2FA form.** On a page that only asks for the code, the default steps pressed Enter before typing the code, which counted as a failed attempt (GitHub: "Two-factor authentication failed"). Codes are now typed once, without fill's rewrite, and a code the site rejects is reported as such, with its message. (#394)

### Contributors

- @leeguooooo

## 1.5.154

### Documentation

- **Touch ID is bitwarden-use's default, not a given.** bitwarden-use 0.8.0 added `require_touch_id false` for unattended runs. The authentication reference and the `auth login --bwu` notice said `bwu run` always asks; they now say it asks once by default, not for items in a reveal folder, and not at all when the user turned confirmation off. (#393)

### Contributors

- @leeguooooo

## 1.5.153

### Features

- **`auth login --bwu` logs in with your Bitwarden vault.** On a login page it asks bitwarden-use (0.7.0+) which vault logins match the site. It sees them masked, most recently used first, and uses the only one or the one named with `--item`; several are listed for you to choose from. The values go through `bwu run`: you confirm once with Touch ID, chrome-use runs itself again with them in that process's environment, and the result lists the steps filled, never the values. Default steps are username, password and Enter. It waits for a password field that only appears after the username was sent, and fills the TOTP on a code page, including one opened separately and sites that submit the code themselves. An item's `_autotype` field (rofi-rbw syntax, e.g. `username:enter:delay:password:enter`) sets the steps instead. `--no-submit` fills only. A secret command's `secrets` list is scrubbed from its response entry by entry. (#391)
- **`addinitscript <js>` / `addinitscript --file <path>`** adds a script that runs before the page's own in every page the session loads from now on, and prints its handle for `removeinitscript`. It had only been reachable through `--init-script` at launch. (#390)
- **`help` and `help <command>`** work as commands. (#390)

### Changed

- **Output is shaped by how agents actually call chrome-use.** Over 215 real agent sessions, 74% of calls piped output through `tail`/`head` and 24% discarded stderr, so an error printed only to stderr, or one whose last line was boilerplate, never reached the model. Now:
  - an error also goes to stdout when stderr goes to `/dev/null`, and its last line is the next step;
  - the `--launch` test-profile notice prints only on the call that launches, not on every command;
  - **plain-mode `eval` prints a string result as text**, byte for byte, instead of a JSON-quoted literal, so a `JSON.stringify(...)` result is parsed once. A string that would read as another type (empty, or a bare number/true/false/null) keeps its quotes; `--json` is unchanged. (#390)
- **"session unresponsive" names one successor session** (`foo` → `foo-2` → `foo-3`) and says to keep it. It used to say "use a different --session name", and one agent went through 98 names, each leaving a daemon behind. (#390)
- **Commands agents guess point to the real one**: `js` → `eval`, `requests` → `network requests`, `har` → `network har start`, `logs` → `console`, `links` → `snapshot -i -f link`. Edit distance had suggested `is` for `js`. `<command> --help` without a help page of its own prints the lines of the full help that mention it, not all 568; `tabs --help` shows the `tab` page. (#390)
- **core/SKILL.md** has a "before you write eval" table: in real sessions eval was 28% of all calls, mostly reading text, clicking by text and reading geometry, which `get text`, `click "text=…"` and `get box` already do. (#390)

### Bug Fixes

- **Tests no longer write to your friction log.** About three quarters of the 4000 lines in `~/.chrome-use/friction.jsonl` were test fixtures. Its categories now follow the error codes `--json` reports, plus `page_fetch_failed` and `policy`. Run `chrome-use friction --clear` once to drop the old test records. (#390)

### Contributors

- @leeguooooo

## 1.5.152

### Features

- **`fill <sel> --from-env <VAR>` fills a value from a password manager.** `bwu run --env PW='github.com#password' -- chrome-use fill @e3 --from-env PW` puts a Bitwarden secret into a field without it ever being an argument, output or part of the transcript. The value is treated as a secret wherever it lands: results, errors and the `--observe` snapshot show `<filled N chars>`, even on a field that does not look like a password, and the CLI drops the variable before starting a daemon so the daemon never holds it. The authentication guide now leads with this recipe.
- **`upload` catches the file chooser a button opens.** On a drop zone with no `<input type=file>` in the DOM (the button creates one and opens the chooser at once), `upload` intercepts the chooser and fills it, without a native dialog. A single-file chooser refuses several files, and a wrong ref to a link or submit button is never clicked. (#386)

### Bug Fixes

- **A password manager's inline menu no longer locks a login tab.** With Bitwarden's menu open on a focused field, Chrome blocked debugger commands and recovery could not clear it: the field kept focus and the menu reopened at once. Recovery now takes focus out of the field (only when it repeats the command itself) and waits for the overlay to settle; a fill blocked on its follow-up is confirmed by reading the field. (#373)
- **A click on a background tab says so.** When a click observes no change on a hidden page, the note says the tab is in the background and points to `tab select --activate`. Activating a tab warns when it hid a tab another live session created in the same window. (#385)
- **Google's sign-in rejection is recognized.** `accounts.google.com/…/signin/rejected` is reported as `blocked_by_signin_rejection` on `open` and in observations, instead of looking like an ordinary page. (#387)

### Contributors

- @leeguooooo

## 1.5.151

### Bug Fixes

- **A native `<select>` left on its placeholder reads as nothing selected.** #375's check only looked one level below the combobox, but Chrome nests a native select's options two levels down, so a select on a disabled "Select…" option still read `: Select…`. It now reads `(nothing selected; shows "Select…")`. (#383)

### Other

- **The extension publishes itself.** Merging a new `extensions/ab-connect.zip` to main uploads it to the Chrome Web Store and submits it for review, with a service account on API v2 (the old OAuth token had silently expired since 0.5.27). A failed publish opens a `cws-publish` issue. ab-connect 0.5.29 is now in review. (#384)
- **OpenAI plugin directory package** in `packaging/openai/` (`build.sh` writes the zip), and a `PRIVACY.md` listing exactly which requests chrome-use makes and what it keeps locally. It has no server and sends no telemetry. (#388)

### Contributors

- @leeguooooo

## 1.5.150

### Security

- **The dashboard refuses DNS-rebinding requests.** It only checked that `Origin` matched `Host`, which a page whose own hostname resolves to 127.0.0.1 satisfies, so while `chrome-use dashboard` ran such a page could watch and drive the browser through the stream proxy. Every request now needs a loopback `Host` (`localhost`, `*.localhost`, `127.x`, `::1`) or one listed in `AGENT_BROWSER_DASHBOARD_ALLOWED_HOSTS`. (#380)
- **Card numbers, CVCs, passwords and one-time codes are masked.** `snapshot` and `get value` print `<filled N chars>` for a field marked sensitive by `autocomplete`, `type=password`, its `name`/`id` or its label; `--reveal-values` prints the value. A fill mismatch on such a field no longer echoes either value. (#372)

### New Features

- **New tabs inherit the session's setup.** User agent, locale, timezone, geolocation, offline mode, extra headers, HTTP credentials, emulated media, init scripts and request interception now apply to `tab new`, `click --new-tab`, `open --new-tab`, popups and `tab duplicate` too. `tab new <url>` opens a blank tab, applies the setup, then navigates, so the first request already carries it. `addinitscript` now returns a handle `init-script-N`. (#382, upstream #1777)
- **`auth login --no-navigate`** fills the login form on the current page instead of opening the saved URL. It works only on the credential's own origin, and only in a tab this session opened or adopted. It stops if the page moves to another origin before it submits. (#382)
- **`--observe` lists the resources an action fetched**, and an action that changed nothing but loaded a known human-check script (OpenAI Sentinel, hCaptcha, Turnstile, reCAPTCHA, Arkose, DataDome, HUMAN, GeeTest) reports `blocked_by_human_check` with the vendor and says to hand off. (#377, #378)
- **A tab Chrome discarded is followed (ab-connect 0.5.29).** Memory Saver and `tabs.discard` give a tab a new id, and the relay reported the session's tab as gone. The extension now follows the replacement, reloads a discarded tab in the background (never activating it) and keeps the session on it. Needs the 0.5.29 extension. (#381)
- `AGENT_BROWSER_DASHBOARD_ALLOWED_HOSTS` allows extra dashboard hostnames, for a reverse proxy. (#380)
- `KERNEL_PROFILE_SAVE_CHANGES` for the Kernel provider. (#380, upstream #2004)

### Bug Fixes

- **`auth login` fills the field it means to and checks before submitting.** It picks the first visible, enabled, editable match and tags that exact element. Before pressing submit it checks the username still holds its value, the password is filled, and focus has not moved to another input; otherwise it stops with "auth login stopped before submitting". (#382)
- **A password manager's inline menu over a field** is named in the error. For a session tab in front, the daemon closes the menu and repeats the command when that is harmless (reads, `fill`, `select`, `check`; never clicks or keys). A background tab is never brought forward; the error gives the command to do it. (#373)
- `fill` accepts a single-line input where the page only added spaces or separators (`1234` → `12 / 34`), with a note. (#374)
- A select left on a placeholder reads `(nothing selected; shows "Select")`, invalid fields are marked `invalid`, and validation messages inside cross-origin frames are found. (#375)
- A failed snapshot keeps the previous refs; a "NO snapshot refs" error names the session and the live sessions. `diff snapshot` / `diff url` no longer leave refs half-updated on failure. (#376, #380)
- `select` matches labels with zero-width characters or NBSPs, prefers an exact value over another option's label, and fails when any requested value matches nothing. Snapshot text shows an NBSP as a space instead of gluing words together. (#380, upstream #1736)
- `wait --load` and `wait domcontentloaded` resolve at once when the page has already loaded. (#380, upstream #1554)
- A CDP reply that does not parse fails its command at once instead of after the 30s timeout. (#381, upstream #1739)
- A `--launch` Chrome no longer freezes when its stderr pipe fills; `/dev/shm` is measured before `--disable-dev-shm-usage` is added; root WebSocket URLs with a query connect; `a11y` reports an invalid selector cleanly; the Kernel profile is sent in the shape the API expects; Windows ARM64 falls back to the x64 binary. (#380, upstream #2003 #1890 #1735 #1604 #2004 #1725)

### Contributors

- @leeguooooo

## 1.5.149

### New Features

- **A site adapter is no longer bound by the ~8s budget of one command.** An adapter ran as a single awaited eval, which over the extension relay is cut off at about 8 seconds, so an upload or a publish had to return `status: "incomplete"` and be rerun by the caller. It now runs in the background of the page and is polled. `--timeout <300|90s|10m>` sets the total time (default: the adapter's `@meta.timeout` in seconds, else 120s). Progress the adapter reports with `args.progress(...)` is printed to stderr and returned as `progress` with `attempts` and `elapsedMs`; `args.budgetMs` tells the adapter how long the run has. Existing adapters run unchanged. (#366)
- **`site ... --until-done`** reruns the adapter while it returns `status: "incomplete"` or `"uploading"` (`@meta.retryStatuses` replaces the list), or when a page navigation ended the run, until it finishes or the timeout passes (default 600s). It honours a returned `retryAfterMs`. Without the flag, a run ended by a navigation returns `status: "interrupted"` and one that outlives the timeout returns `status: "timeout"`, each with a hint. (#366)
- **Site adapters take local files.** An arg declared `"type": "file"` takes a local path; in the adapter, `await args.<name>.setOn(selector?)` puts that file on the page's file input the way `chrome-use upload` does, over the relay too, so publishing a video is one command with no separate `upload` and no selector to know. The page names the selector, never the path. (#364)
- **Site adapter values from a file or stdin.** `--key @path` reads the value from a file, `--key @-` from stdin, and `--key-file path` is the same spelled out. `@name` that is not a file stays literal (`--user @jack`); a path-like `@post.md` that does not exist is an error, so a typo is never posted as text; `\@text` passes a literal leading `@`. (#365)
- The MCP `chrome_use_site` tool takes `untilDone` and `timeout`, and waits for the run's own budget instead of a flat 30 seconds. (#366)

### Documentation

- The site-adapters page, `site --help` and the core skill reference describe file args, `@file` values, `--timeout` and `--until-done`, and how to write an adapter that uses them. The skill reference listed `--timeout` as a reserved global flag; it is not. (#368)

### Contributors

- @leeguooooo

## 1.5.148

### Bug Fixes

- **`skills get core` works where the cache directory cannot be written.** A single-binary install unpacks its bundled guide to the cache directory on first use. In an agent sandbox that denies writes under the home directory that failed, and `skills get core` ended in "Skills directory not found". It now falls back to the temp directory; an extraction already there is replaced on every run, not reused. (#367)
- **A `skills/` directory that belongs to another tool no longer hides the bundled guide.** The lookup took the first directory above the binary that had a `skills/` in it, so with the binary in `~/.local/bin` a `~/.local/skills` or `~/skills` made `skills get core` answer "Skill not found: core". The directory must now contain `skills/chrome-use/SKILL.md`. (#367)
- The "Skills directory not found" error no longer tells a single-binary install to reinstall via npm. (#367)

### Contributors

- @leeguooooo

## 1.5.147

### Bug Fixes

- **`--launch` passes Cloudflare's managed challenge again.** With the stealth patches on, the challenge spun forever; with them off it passed in about 16 seconds. Three causes: the patches faked Android-only APIs (`connection.downlinkMax` in the page and, through a wrapped `Worker` constructor, in workers; `ContactsManager`; `ContentIndex`) that desktop Chrome does not have, so the fingerprint read as a Mac UA with Android APIs; they were evaluated into Cloudflare's own challenge iframe while its scripts ran; and `navigator.languages` was forced to `en-US` in the page while workers and the `Accept-Language` header kept the system languages. The fake APIs are gone, the payload now skips anti-bot challenge frames (Cloudflare, hCaptcha, reCAPTCHA, DataDome, Arkose) and keeps only the native overrides there, and languages are no longer patched in JS. The challenge now passes in about 8 seconds. CreepJS "like headless" rises from 0% to 19%; a real Mac Chrome scores 31% on the same probes. (#361)
- **`AGENT_BROWSER_LOCALE` sets the language everywhere.** It is passed to Chrome as `--accept-lang`, so the page, its workers and the `Accept-Language` header agree, for temporary and `--profile` launches alike. An `--accept-lang` in `--args` takes precedence. (#361)
- **`cf-status` no longer reports an embedded Turnstile widget as a challenge.** Its `[id^="cf-chl"]` probe matched the widget's hidden `cf-chl-widget-*_response` input, so an ordinary login page came back as "challenged, clearance stale, re-solve". (#361)

### Documentation

- nowsecure.nl now embeds a Turnstile test sitekey and no longer tests anything; the stealth page and READMEs benchmark against scrapingcourse.com's managed challenge instead. (#361)

### Contributors

- @leeguooooo

## 1.5.146

### Bug Fixes

- **`fill` enters text the way a user does.** For text inputs and textareas it wrote the value with the prototype setter and dispatched synthetic `input`/`change` events (`isTrusted: false`). A page that only honours real input kept its old state while the field showed the new text, the read-back matched, and `fill` printed ✓ (LinkedIn's edit-intro dialog: Save never saved). It now selects the current value and replaces it with a trusted `Input.insertText` (trusted `beforeinput`/`input`, a trusted `change` on blur); `fill ""` clears with a trusted Delete. When the trusted insert cannot produce the value (a `maxlength` or mask), the setter path still runs, `engine` reads `input-synthetic`, and a warning says the page may not have registered it. `type --clear` clears with select-all and a trusted Delete too. (#358)
- **`fill` says when the page did not react.** When the field's form or dialog has a Save/Submit button that was disabled before the fill and is still disabled after it (with no other required field empty), `fill` warns and reports `commitControl` in the JSON. (#358)
- **`click` refuses a disabled control.** The browser delivers no click to a `:disabled` button, so a click on a Save that had not enabled yet printed ✓ and did nothing; it is now an error that points at the edit before it. An `aria-disabled="true"` target is clicked with a warning. (#358)
- **`click` reports how it was delivered.** The response carries `dispatch: pointer | keyboard | dom`. A fallback to `element.click()` (`isTrusted: false`) used to be logged only to the daemon's stderr; it now comes back as a warning that names the reason. (#358)
- **`keyboard type` reads back.** It reports the focused element (`target`) and its value (`readBack`), fails when a focused text field did not change, and warns when nothing editable had focus. (#358)

### Documentation

- The `click` help no longer says relay clicks are DOM-dispatched; they have been trusted pointer clicks since v1.5.124. The skill and docs describe the #358 diagnostics and warn that a `toggles=checkbox(...)` button is a switch that can be destructive (in LinkedIn's profile-language dialog it deletes that language's profile). (#358)

### Contributors

- @leeguooooo

## 1.5.145

### Behavior Changes

- **`find` without an action only locates.** It prints the match (tag, role, name, box, visibility, match and visible counts) and does nothing else; the CLI, daemon and MCP `chrome_use_find` all default to `locate`. Callers that relied on the implicit click must now say `click`. A flag in the action slot (`find role button --name X`) is treated as a bare locate instead of "Missing action verb". (#354)

### Bug Fixes

- **`find text` picks the element that holds the text, and `click` lands on something clickable.** Matching takes the deepest element containing the whole text (text split across children still matches), skips non-rendered nodes, and prefers visible, exact, then shortest matches; `find role` also prefers visible matches. Before clicking, a text or role match climbs to the nearest clickable ancestor (`button`, `a`, `[role=button]` …) and says so. An invisible match is an error, and a click with no DOM change, navigation, focus move or form event within 800 ms returns a warning instead of ✓. `find … type|focus|uncheck` no longer fail with "Unknown subaction". (#354)
- **`type` fails when nothing landed.** It reads the value before and after typing, and again 250 ms later, so a page that clears the field during initialisation is caught. An empty or unchanged field is an error that says whether the text appeared and was cleared or focus was elsewhere, and suggests `fill <sel>` or `click <sel>` + `keyboard type`. A partially rewritten value (masks, formatting) stays a warning. (#355)
- **Refs survive a React remount.** When the node behind a fresh ref is replaced, the ref re-binds to the unique visible node with the same role that carries the old node's stable attributes (`id`, `name`, `name+type`, `data-testid`, `aria-label`, `placeholder`). If re-binding fails, the refusal lists up to three usable selectors such as `[textbox "手机号"] → input[name="username"]` before the `AGENT_BROWSER_VERIFY_REF=0` last resort, and a re-read timeout is no longer reported as a missing element. (#356)
- **`adopt <url>` only adopts a tab whose current URL matches.** It matched a relay URL cache recorded at adoption time, so a tab that had since navigated elsewhere could be taken over; the cache now follows navigations and the extension's answer is checked against the spec. The adopt spec applies to the first connection only, so a reconnect after the page navigated no longer fails with `adopt: no open tab matching …`. (#357)
- **A tab blocked by a foreign extension iframe no longer bricks the session.** The error reads the tab's current URL instead of the one from before the block, and points to `chrome-use tab new <url>` in the same session. `screenshot` reports the real access denial instead of "attached to an unknown target". Chrome still refuses to debug the blocked tab while that iframe is present. (#357, #341)
- **`site` infers a missing argument from the current page.** When a required argument is missing and the adapter describes its URL (for example `linkedin.com/in/<username>`), the value comes from the current tab when host and path match, so `site linkedin/profile` works on a profile page. A remaining `Missing argument` error carries a usage hint, and the top-level `error` includes the adapter's hint. (#359)

### Improvements

- **Snapshot marks buttons that toggle a control.** A clickable element that wraps a checkbox, switch, radio or menuitemcheckbox shows it inline, e.g. `button "简体中文" [toggles=checkbox(checked=true), ref=e1]`, so a destructive toggle does not look like a plain button. (#358)

### Contributors

- @leeguooooo

## 1.5.144

### New Features

- **`chrome-use upgrade --check` and `--json`.** Report the running and latest versions and every installed copy of the agent skill (Claude Code plugin, git checkout, installer folder, copied folder) without changing anything. `--json` prints `name`, `current`, `latest`, `update_available` and `skills[{channel, path, update}]`. A failed check exits 2. (#352)
- **`chrome-use upgrade` refreshes the skill too.** It skips the reinstall when the CLI is already current, prints `old -> new`, runs `claude plugin update` for the plugin, `git pull --ff-only` for a chrome-use checkout, and refreshes installer folders; copied folders get the `npx skills update chrome-use` hint. `AGENT_BROWSER_NO_SKILL=1` leaves every skill folder alone. (#352)
- **Daily update notice follows the *-use family convention.** At most one background check a day (2 s timeout, cached in `${XDG_CACHE_HOME:-~/.cache}/chrome-use/update-check.json`, written atomically). While a newer release exists, each command prints one stderr line: `chrome-use X is available (you have Y). Upgrade: chrome-use upgrade`. stdout and `--json` output are never touched; the `mcp` server and daemon stay silent. Opt out with `CHROME_USE_NO_UPDATE_CHECK=1`, `USE_NO_UPDATE_CHECK=1` or `CI`; hide only the line with `CHROME_USE_NO_UPDATE_NOTICE=1`. (#352)

### Behavior Changes

- The update notice is no longer limited to a terminal or to non-`--json` runs (#170), so agents see it on stderr. Wrappers that quote stderr can set `CHROME_USE_NO_UPDATE_NOTICE=1`. A failed `upgrade` now exits 2 instead of 1. Version comparison follows semver pre-release order. (#352)

### Contributors

- @leeguooooo

## 1.5.143

### Documentation

- **Explain protected extension iframe failures.** Chrome can reject parent-page debugger commands when a foreign extension iframe is present, even with auto-attach disabled. The troubleshooting guides explain how to save work, disable the conflicting extension on the affected site and reload, or use a separate profile. This release does not remove Chrome's permission restriction or turn denied commands into successful replies. (#349, #341)

### Tests

- **Reproduce the browser restriction in isolation.** A standalone Chrome for Testing script checks parent-command denial and failed reattachment with auto-attach both disabled and enabled, then verifies recovery after the disposable iframe is removed by reload. The investigation links the matching Chromium permission checks. (#349)

### Contributors

- @leeguooooo

## 1.5.142


### New Features

- **Bring a tab forward before its renderer responds.** `tab new`, `tab select` and `tab adopt` accept `--activate` (alias `--front`). Activation happens through the browser connection before renderer initialization, so a stalled background tab can be recovered. Tab commands stay in the background by default. (#346)
- **Adopt an existing tab over direct CDP.** Explicit adoption attaches without navigation or reload, verifies the selected page, and protects the adopted tab from session cleanup. (#346)

### Bug Fixes

- **Recover a new tab without creating a duplicate.** If initialization fails after attachment, keep its target ID, ownership, label and selected-tab context. Selecting that target retries setup; a failed stealth setup remains pending and reports an error until a later retry succeeds. (#346)
- **Timeout guidance preserves uncertainty.** A command timeout alone no longer diagnoses a lost connection or recommends replaying a potentially completed action. Verify the target and a read before continuing. (#346)

### Contributors

- @Sean529
- @leeguooooo


## 1.5.141


### Bug Fixes

- **A `--launch` session no longer hangs or switches browsers once its browser is gone.** Follow-up commands don't repeat `--launch`. That was fine while the session's daemon kept running, but once the launched browser was gone (closed after 10 idle minutes, killed, or crashed along with its daemon), the next command read its missing `--launch` as "use my real Chrome". If the Chrome extension wasn't connected, `get url`, `close` and every other command waited on "Chrome relay dropped — reconnecting…" for a connection this session never used; seen on Windows. If the extension was connected, the command would have gone to your real Chrome instead. A session started with `--launch` now stays that way until you `close` it: the next command launches a fresh browser in about a second, with the existing warning that the old window is gone. To move the session to your Chrome, pass `--auto-connect`, `--cdp`, `--browser` or `--provider`.

### Contributors

- @leeguooooo


## 1.5.140

### Improvements

- **Agent skill installation no longer needs Node, npx or Git.** `chrome-use skill install` writes the discovery entry bundled with the CLI, verifies the saved contents, and reports each installed path. It covers shared skills, Claude Code and Cursor, plus detected Pi, OpenCode, Windsurf, CodeBuddy and Trae configurations. Codex uses the shared directory so it does not discover a second copy. `--project`, `skills update` and `skills refresh` use the same offline installer. Other runners can still use skills.sh. (#343)
- **The Windows installer also works with older CLI releases.** It extracts their bundled discovery skill directly, bypassing the old npx installer. UTF-8 decoding preserves Chinese content on Windows PowerShell 5.1. Architecture detection also works when an agent omits the usual environment variables, and CLI JSON failures retain their error details. Replacing an existing skill is staged beside the destination; failed writes leave the previous file intact. (#343)

### Bug Fixes

- **Installers no longer claim everything is ready after a failed step.** Skill installation failures, extension setup failures and doctor failures stop the completion message. The Windows self-check also reports its timeout. An explicitly skipped skill step is identified as skipped; successful CLI installation does not claim that the Chrome extension is connected. (#343)
- **Windows doctor output matches the platform.** Disk space is checked through the Windows API, optional chat-key guidance uses PowerShell syntax, and macOS-only ChooseBrowser paths are omitted. Automatic encryption-key creation no longer comes with an unnecessary Unix setup command. Skill detection recognizes the native installer's destinations and custom runner configuration paths. (#343)

### Contributors

- @leeguooooo

## 1.5.139

### Features

- **One-line install on Windows.** In PowerShell: `irm https://raw.githubusercontent.com/leeguooooo/chrome-use/main/install.ps1 | iex`. It installs to `%LOCALAPPDATA%\Programs\chrome-use` with no admin rights, adds that to your user PATH, and then runs the same extension setup, skill install and self-check as `install.sh`. The download's sha256 is mandatory — a mismatch installs nothing — because an interrupted download otherwise extracts into an `.exe` that dies at launch with an access violation. Your existing PATH entries are kept exactly as they were, `%USERPROFILE%`-style variables included; the usual PowerShell API for this would have expanded them into absolute paths for good. Pin with `$env:AGENT_BROWSER_VERSION`, relocate with `$env:AGENT_BROWSER_BIN_DIR`, or leave PATH alone with `$env:AGENT_BROWSER_NO_PATH = 1`. Tested on Windows 11 with Windows PowerShell 5.1; ARM64 gets the x64 build under emulation, untested.

### Bug Fixes

- **`chrome-use doctor` no longer launches Chrome on Windows.** It read Chrome's version by running `chrome.exe --version`, which on Windows starts the browser — against your real profile — instead of printing a version, and never returns. `doctor --quick --offline` hung indefinitely, and the new installer's self-check with it. The version is now read from the directory beside `chrome.exe`; nothing is launched, and the check finishes in milliseconds. macOS and Linux are unchanged.

### Contributors

- @leeguooooo

## 1.5.138

### Improvements

- **The extension relay forwards less noise** (from community PR #342, thanks @AmeerAliAnwar). Nine high-frequency CDP events that nothing in the CLI reads — `Network.dataReceived`, `DOM.attributeModified` and the like — are no longer serialised across the native-messaging pipe. A test scans the CLI sources and fails if anything starts reading one of them, since the daemon would otherwise get no error. A new tab is also attached immediately instead of after an unconditional 100ms, and duplicateTab's inspection reads are now bounded by its transaction deadline instead of able to hang it.

### Bug Fixes

- **Fixed regressions #342 would have shipped**, each checked before the change:
  - A 1 MB cap on messages from the extension to the host. Chrome's 1 MB limit is on messages *from* the host; messages *to* it may be 64 MiB. A viewport screenshot of a Wikipedia article is ~1.2 MB once base64'd, so the cap would have turned ordinary screenshots over the relay into errors. Removed.
  - Session recovery's retry windows had been cut by more than half. Both were sized from live reproductions — #23 (a cross-process checkout navigation) and #24 (a sign-in hop that "takes seconds to settle", ~6.3s). Restored.
  - duplicateTab no longer fell back when there was no focused window or active tab, and no longer stopped starting stages once its deadline had passed — so side-effecting steps could run after the transaction was over. Both restored, with tests that fail against the PR's version.
  - URL scheme rewriting turned `blob:` and `view-source:` URLs into `https://blob:...`. The CLI already adds a scheme before a URL reaches the extension, so it is removed.
- Removed an uncalled viewport scanner that wrote attributes into the user's page and named its results `@e1`, `@e2`, like the CLI's refs, and a test file whose tests re-implemented the code instead of importing it.
- ab-connect **0.5.28**.

### Contributors

- @AmeerAliAnwar
- @leeguooooo

## 1.5.137

### Bug Fixes

- **`jev run` now completes the 16-question form it could not finish.** Six runs out of six submit it with all twelve fields and all four checkboxes as asked — including leaving one unchecked because the goal said so — and report `done`, in 18.4–23.0s (median 19.8s), checked against the form itself rather than against jev's own status. The cause was what the decision model could not see: its request carried only the last ten actions, and by the checkboxes the run had taken thirteen, so "full name", "company" and "role" left that list at the same moment they scrolled out of the viewport. The goal still asked for them and nothing the model was shown said they were done, so BLOCKED climbed from 0.07 to 0.53 over the last three decisions. The request now carries the whole run; an entry is a few dozen bytes and runs are capped.
- **The text helper is retried once.** A single malformed answer ended the whole run — seen as prose instead of JSON cut off at the token limit, and as an upstream 502. The call only generates text and nothing is typed until it succeeds, so a retry repeats no side effect. One retry, not a loop.
- A field a run already filled with the same text is now marked rather than removed: it stays in the element list under its own name and with its value — the evidence it is done — while offering nothing to type. Removing it (v1.5.136) left it represented only by its `Open <label>` click.

### Corrections

- **v1.5.136 was wrong about why the form stopped.** It said the remaining stop at the checkboxes was "the model's judgement on complete input, and nothing here addresses it". The input was not complete. Controlled runs isolated it: a page with only the checkboxes and a goal about only them completed every time; the same page with the full 16-item goal went straight to BLOCKED (0.77); a pre-filled form with no history stalled too — each time on goal items the model had no evidence for.

### Contributors

- @leeguooooo

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
