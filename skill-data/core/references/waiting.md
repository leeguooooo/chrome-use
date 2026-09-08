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
settled after 1000ms (request in flight still active) — this capture may be
mid-transition. Re-read to confirm, or raise the ceiling with
AGENT_BROWSER_SETTLE_MS.` — so re-read rather than trusting that tree.

The two differ in one way. A plain `snapshot` has no action to react to, so a
still page is its answer and it returns as soon as everything is quiet. After a
mutating action, `--observe` keeps watching for a *first* reaction for half the
ceiling (500ms by default) before it is willing to report `changed:false` —
otherwise a control that renders on a 300ms timer reads as "nothing happened".
Only actions that really change nothing pay that; anything that reacts ends the
window at once. Spinners that loop forever are ignored on purpose; they never end. Tune
with `--settle-ms <ms>` (or `AGENT_BROWSER_SETTLE_MS`) and switch it off with
`--no-settle` when you deliberately want the page mid-flight. Since the wait
already happened, `--with-screenshot <path>` saves the pixels from that same
settled moment (`snapshot -i --with-screenshot ./page.png`, or an action with
`--observe`) — the tree is still what you read the page from; the image is an
output to look at or attach, and never a substitute for the structural read. Waiting for
something *specific* is still `wait`'s job — the settle only knows that the
page stopped, not that what you wanted appeared.

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
chrome-use requests --clear && chrome-use click @save \
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

**See what an action changed — `--observe`.** Add it to a mutating action
(`click`/`fill`/`type`/`select`/`check`/`press`/`eval`) and instead of you
running act → wait → `snapshot` → `diff`, the result carries an `observed` delta:
the added/removed interactive lines (new toasts/validation included via the alert
surface), any url change, and requests fired — or `{changed:false}` if nothing
moved. e.g. `chrome-use click @e8 --observe` → see the dialog/toast/row that
appeared in one ~20-80 token reply. Use `expect` when you want a hard pass/fail
gate; use `--observe` when you want to *see* what happened.

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
`requests --clear` first; `no-errors` needs console capture — run `console` once.)
