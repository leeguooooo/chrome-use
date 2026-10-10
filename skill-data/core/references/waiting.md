# Waiting (read this)

Agents fail more often from bad waits than from bad selectors. Pick the
right wait for the situation:

```bash
chrome-use wait @e1                     # until an element appears
chrome-use wait 2000                    # dumb wait, milliseconds (last resort)
chrome-use wait --text "Success"        # until the text appears on the page
chrome-use wait --url "**/dashboard"    # until URL matches pattern (glob)
chrome-use wait --load networkidle      # until network idle (post-navigation)
chrome-use wait --load domcontentloaded # until DOMContentLoaded
chrome-use wait --fn "window.myApp.ready === true"  # until JS condition
```

After any page-changing action, pick one:

- Wait for a specific element you expect to appear: `wait @ref` or `wait --text "..."`.
- `--text` is an EXACT, case-sensitive substring of the page's visible text.
  `--text "Saved"` does not match a page reading `Delivery saved.` Read the page
  and match what it actually renders.
- If a result you can already see answers the question, that IS the answer. A
  receipt or confirmation already on screen does not need a `wait` to confirm it
  a second time; that only spends the budget.
- `Wait timed out after …` means the condition was not observed in time. A probe
  that fails is retried until the deadline, so the message reads the same whether
  the page answered every poll or none of them: it tells you nothing about the
  connection either way. Check the page and the condition first; do not reconnect
  on the strength of this message alone.
- Wait for URL change: `wait --url "**/new-page"`.
- Wait for network idle (catch-all for SPA navigation): `wait --load networkidle`.

Avoid bare `wait 2000` except when debugging — it makes scripts slow and
flaky. Timeouts default to 25 seconds.

**Do not sleep before reading the page.** `snapshot` and `--observe` wait by
themselves: before capturing they wait for the DOM to stop mutating, for finite
transitions to finish, and for requests fired by the action to come back —
whichever takes longest, up to a 1 second ceiling, and they return the moment
the page goes quiet (a static page costs about 100ms, not the ceiling). A
`wait 2000` in front of a snapshot buys nothing and costs two seconds. If the
ceiling expires with the page still moving, the reply says so — `Page had not
settled after 1000ms (request in flight still active: GET example.com/api/list
(0.8s)) — this capture may be mid-transition. Re-read to confirm, or raise the
ceiling with AGENT_BROWSER_SETTLE_MS.` — so re-read rather than trusting that
tree. A network wait names up to three requests (host and path, never the
query) and how long each has been in flight (`settle.pendingRequests` in
JSON), as they were when the wait stopped; `state at the deadline unknown`
means the requests it waited on are no longer counted (finished, or past the
stale cutoff). `page did not answer the
settle check` means the page was busy or navigating, not that its DOM was
changing.

The two differ in one way. A plain `snapshot` has no action to react to, so a
still page is its answer and it returns as soon as everything is quiet. After a
mutating action, `--observe` keeps watching for a *first* reaction for half the
ceiling (500ms by default) before it is willing to report `changed:false` —
otherwise a control that renders on a 300ms timer reads as "nothing happened".
Only actions that really change nothing pay that; anything that reacts ends the
window at once, including a re-render the action's own handler made while it was
being dispatched (a click that redraws a list synchronously settles in about the
100ms quiet window, not 500ms). Spinners that loop forever are ignored on purpose; they never end. Tune
with `--settle-ms <ms>` (or `AGENT_BROWSER_SETTLE_MS`) and switch it off with
`--no-settle` when you deliberately want the page mid-flight. Since the wait
already happened, `--with-screenshot <path>` saves the pixels from that same
settled moment (`snapshot -i --with-screenshot ./page.png`, or an action with
`--observe`) — the tree is still what you read the page from; the image is an
output to look at or attach, and never a substitute for the structural read. Waiting for
something *specific* is still `wait`'s job — the settle only knows that the
page stopped, not that what you wanted appeared. `observed.status: complete`
means the capture is complete, not the task: a page that shows `Loading` and
renders its result on a timer is quiet at that moment, so follow with
`wait --text "Report ready"` (or the page's own final signal).

**Repeated unchanged attempts are advisory.** `observed.noProgress` appears
from the third identical observed `click`, `dblclick`, or `press` on the same
target and screen, with at most 60 seconds between attempts. Each observation
must be complete and quiet, with settling enabled (`waitedMs > 0`),
`changed:false`, `sawChange:false`, no pending work, and no detected requests,
resources, new frames or human-check script. Activity or incomplete evidence
breaks the streak. The hint sets `retryAction:false`; it never changes success
or runs another action. This detector only covers the evidence collected by
observe: an unchanged tree does not establish that a server write failed.

Ordinary CLI/MCP connection preparation preserves this streak only on successful reuse of the same browser connection, target and session, without loading storage state. A new browser, rebind, failed preparation or storage-state load still clears the count.

**When `--observe` says `no change`, read the `why:` line under it.** The
daemon probes the target after a quiet action and reports the first decisive
finding: the control is disabled, it is not rendered, it sits outside the
viewport, or another element covers it (named). Fix that one thing and retry
the same semantic action. When the note says none of those apply, the action
reached a control that genuinely changes nothing visible; do not read an
empty delta as failure, and do not repeat the action.

**When an action replaced the page** (a link click, a submit that
navigated), `--observe` returns the new tree under `observed snapshot:`
instead of a diff, with a line saying how many lines of the old page are
gone. Refs in that tree are live; use them directly.

### Confirm an action worked — `expect`

After acting, **assert the result instead of eyeballing a snapshot**. `expect`
is a pass/fail verb with an exit code (0 pass / 1 false / 2 un-evaluable), so it
composes with `&&` and `chrome-use batch`, and costs ~1 line instead of a
snapshot you have to read:

```bash
chrome-use click @e8 && chrome-use expect "#toast" visible      # did the toast show?
chrome-use expect count ".result" ">=" 1                        # results loaded?
chrome-use expect text @e3 contains "Saved"                     # success message?
chrome-use expect url contains /dashboard                       # navigation landed?
chrome-use expect "#spinner" gone                               # finished loading?
chrome-use network requests --clear && chrome-use click @save \
  && chrome-use expect request /api/save --status 2xx           # the POST fired & 2xx?
chrome-use expect no-errors                                     # no console errors?
```

**Fill a whole form in one call — `form fill --map`.** Instead of N
`fill`/`select`/`check` steps, pass a `{label-or-selector: value}` map: it
resolves each field (by `<label>`, aria-label, placeholder, name, or CSS),
dispatches the right control type (string → text/select/radio, `true/false` →
checkbox), optionally submits (`--submit "<text|selector>"`), and returns
`{filled, submitted, errors}` — `errors` are the inline validation messages, so a
rejected submit tells you why in the same call. `chrome-use form fill --map
'{"Email":"a@b.com","Country":"US","Subscribe":true}' --submit "Sign up"`. For
rich editors (DraftJS/Monaco/CodeMirror) fill those fields with `fill` instead —
it handles them and verifies the exact editor-model readback before reporting
success. Monaco instances that hide their model API use one trusted editor
paste plus editor-generated copy readback, with the browser clipboard restored
afterward; if either operation cannot be verified, `fill` fails. `form fill`
covers standard controls.

Observations report `status: complete|partial|unavailable` separately from the
action result. Failed captures do not become empty pages or URLs. `changed:null`
means the evidence cannot establish whether anything changed; a known change can
still be true in a partial observation. If the baseline failed but the after-tree
was captured, that tree is returned instead of a fabricated diff. Incomplete
observations include errors and `retryAction:false`: inspect current state rather
than replaying the action. The command keeps its original action success value.
When only the post-action capture fails, `observed.refs` says what became of the
session's refs: `kept-unverified` keeps the previous snapshot's refs, but each
is checked live (same tab, frame, document and element) before it is used and
refused otherwise, never re-anchored by role or name; refs from a DOM-walk
snapshot are always refused then. `dropped` means the document may have
changed. Either way, run `snapshot -i` and do not repeat the action.
For `form fill`, unavailable validation is `errors:null`, not an empty error list.

Action observations include at most 20 request summaries, each capped at 256 UTF-8
bytes. Data URLs show their media header and encoded payload size instead of the
payload. `requestsTotal`, `requestsOmitted`, and `requestsShortened` describe the
summary; use `network requests --json` for full captured details. Request summaries remain
visible when `changed:false`: that flag describes tree/URL changes, not network activity.

`resources` lists what the page fetched during the action (from resource timing,
so it works with the Network domain off), scripts and fetches first, up to 10,
with `resourcesTotal`.

**`blocked_by_human_check`.** When an action changed nothing but loaded a known
human-check script (OpenAI Sentinel, hCaptcha, Cloudflare Turnstile, reCAPTCHA,
Arkose, DataDome, HUMAN, GeeTest), `observed.humanCheck` names the vendor and the
result carries a `blocked_by_human_check` warning. This is evidence of a script
load and an unchanged page, not proof a person is required. Do not repeat the
original submit. Inspect the current challenge and, within the authorized task
and host rules, try its ordinary controls using `core/captcha`. Verify the
result; hand off only when attempts fail or personal presence is required.

**See what an action changed — `--observe`.** Add it to a mutating action
(`click`/`fill`/`type`/`select`/`check`/`press`/`eval`) and instead of you
running act → wait → `snapshot` → `diff`, the result carries an `observed` delta:
the added/removed interactive lines (new toasts/validation included via the alert
surface), any url change, and requests fired — or `{changed:false}` if nothing
moved. e.g. `chrome-use click @e8 --observe` → see the dialog/toast/row that
appeared in one ~20-80 token reply. Use `expect` when you want a hard pass/fail
gate; use `--observe` when you want to *see* what happened.

On a small page (post-action tree at most 4 KB and 60 lines) the observation
carries the whole current tree as `observed.snapshot`, refs registered, plus
`observed.changes`: only the `+`/`-` lines, no context. That tree is the fresh
post-action read, so a `snapshot` straight after it adds nothing. Larger pages
keep the unified `delta`. A partial or unavailable capture says so in
`observed.status` either way; a snapshot beside `status: partial` is a real
tree, but the observation as a whole is still incomplete.

`observed.text` lists the visible text lines, across all frames, that
appeared (`+`) or went away (`-`): the static text an interactive tree leaves
out, such as an iframe's receipt, a log line, or a "Page 2 of 3" counter.
Lines from a child frame start with `[frame <name>]`. A text change alone sets
`changed: true`. At most 20 lines and 1 KB, 200 bytes a line; `textOmitted`
counts the rest and `textTruncated: true` says so.

The text is read from the rendered DOM, not from `innerText`: `<input>`,
`<textarea>`, `<select>` and every editable element (the editable host, all
of its content, any document in `designMode`) are skipped with their
subtrees, including inside open shadow roots. When any password field in any
frame (open shadow roots included) holds a value before or after the action,
no text lines are returned at all and `textStatus` is `redacted`: a page can
copy a password into any frame's text, so no line is treated as safe. The
password values themselves never leave the page.

Each frame is read in its own isolated world and checked against the frame
tree, and the inventory is the frame tree read again after the reads. One
capture reads at most 32 frames across all sessions within 2.5 s; a frame
not read (over budget, or one that appeared during the read) is `partial`,
and a frame whose read failed or whose document changed during the read is
`unavailable`. Any such frame withholds every text line (it might hold a
password), is listed in `textFrames` with its reason, and makes the
observation incomplete: `textStatus` is `unavailable`, `status` is at best
`partial`, an unchanged tree no longer reads as `changed: false`, and no
`noProgress` hint is given. A frame the final inventory confirms is `gone`,
and a frame whose document changed (`frameDocumentChanged`, e.g. a same-URL
reload), count as changes. The small-page `changes` list is capped at 60 lines
and 4 KB, 240 bytes a line (`changesOmitted`, `changesShortened`).

`navigate`/`reload`/`back`/`forward` take `--observe` too, and return the
post-navigation **snapshot** instead of a delta — across a page swap a diff
shares no nodes with the old tree, so it would be 100% removals plus 100%
additions. This is the one flag that collapses `navigate` + `snapshot` into a
single call: `chrome-use navigate <url> --observe` gets you the page AND its
interactive tree in one round trip. On a real task that halved the calls (6 → 3)
at identical bytes returned.

**Edit one phrase inside a field — `select-text`.** `fill` replaces the whole
value and `type` appends, so changing a single word in a written paragraph, or
placing the cursor and carrying on, used to need hand-written `eval`.
`chrome-use select-text @e3 "confirm" --prefix "please "` selects exactly
`confirm` (the prefix is context for finding it, not part of the selection);
`--cursor-before` / `--cursor-after` leave a caret instead, so the next `type`
lands there. Works on `<input>`, `<textarea>` and contenteditable. A phrase that
appears more than once is refused with the count instead of resolved to the
first one, and "not found", "found but not with that prefix/suffix" and "matches
N places" are three different messages because they have three different fixes.
Monaco and CodeMirror are refused by name — they keep their own selection model,
where a DOM selection looks applied and does nothing; use `fill` there.

**Paste with a MIME type — `paste`.** In a rich-text editor, typing and pasting
produce different documents: `type "<b>bold</b>"` gives you those eleven
characters, `chrome-use paste "<b>bold</b>" --format html --selector "#editor"`
gives you bold text. Newlines are the other reason: `type` sends Enter, which in
most editors submits or splits a block, while `paste` inserts the break — so
prefer `paste` for multi-line content. `--format md` inserts Markdown source as
plain text. **Your real clipboard is never touched**: the content rides on a
synthetic ClipboardEvent, with no `navigator.clipboard` call and no Ctrl+V. Such
an event is untrusted and has no default action, so an editor that listens gets
it through its own handler and one that ignores it gets a real insert; the reply
names which path ran, and a paste that changed nothing is an error, not a ✓.

**Name the session before you open tabs — `session name`.** On the user's real
Chrome your tabs are collected into a tab group, and by default that group is
labelled with the session id (`cu-myproject-fb0742`). That is a routing key; it
tells the person whose browser this is nothing about what you are doing in it.
Set a short, task-relevant label with a leading emoji as the first thing you do:
`chrome-use session name "🔎 track a parcel"`. Do it *before* opening tabs —
tabs already open keep the old label unless the installed extension is new
enough to rename the group, and the command tells you which happened.

**Operate a control that click alone won't move — `actions` / `do`.** Some
elements expose more than a click: a disclosure expands, a menu button opens a
popup, a spinbutton or slider steps through a range. `chrome-use actions @e7`
lists what *that* element supports right now (read live — a disclosure that was
collapsed when you snapshotted may be open now), and `chrome-use do @e7 expand`
performs one of exactly those. An action outside the reported set is refused
with the supported list, never attempted. After acting it reports the set again,
so a control that did not move cannot read as success.

**Re-read a page for a few bytes — `snapshot --diff`.** After an action, most
of the tree is what it was. `--diff` returns only the lines that changed since
this session's last snapshot of the same page (same options): on a 143 KB HN
comment thread, an unchanged re-read costs 1 byte instead of 143,490. With no
valid baseline — no previous snapshot, the page navigated, different options —
it returns the full tree and says which, because an empty diff and an unchanged
page are indistinguishable. Use `--observe` when you want the delta caused by a
specific action; use `--diff` when you are simply reading the page again.

**Budget a huge tree — `snapshot --max-bytes <n>` / `--from <n>`.** A long
comment thread or feed can snapshot to ~140 KB, nearly all of it prose unrelated
to the element you want. Unlike `-i/-s/-d/-f`, a budget needs no advance
knowledge of what you're looking for. It cuts between whole nodes (never a
severed `[ref=eN]`) and tells you what it left out plus where to resume:
`truncated: nodes 0-70 of 1908 (1838 omitted)` / `read on with: --from 70`.
A tree that fits is never flagged truncated.

**Optional steps — `--if-present` (alias `--optional`).** Add it to any selector
action to make it a no-op success (`↷ skipped`, exit 0) when the target is
absent, instead of erroring — no pre-check read needed, and flows stay
re-runnable: `chrome-use click ".cookie-accept" --if-present` (dismiss a banner
that may not be there), `chrome-use check "#opt-in" --if-present`. Only "element
absent" is skipped; real failures still error.

It **waits** up to the timeout for the condition to hold (poll), so you often
don't need a separate `wait`. Conditions: element `visible|hidden|gone|present`;
`count <css> <op> <n>`; `text|value|attr … equals|contains|matches`; `url …`;
`request <substr> [--status 2xx]`; `no-errors`. `--not` inverts, `--no-wait`
checks once. (`expect request` only sees requests captured after tracking is on —
`network requests --clear` first; `no-errors` needs console capture — run `console` once.)
