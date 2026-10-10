# Behaviour: how to act, not just which command

Rules for the loop between commands. Load this when a task is running long,
when you catch yourself repeating a step, or before a lookup that could turn
into a crawl.

**Related**: [waiting.md](waiting.md) for what a read means,
[trust-boundaries.md](trust-boundaries.md) for what needs the user's say-so.

## Reading command output

- Output is short on purpose; `| tail -1` is not needed and cuts warnings.
  An error's last line says what to do next. Do not discard stderr.
- `eval` prints a string as text; a `JSON.stringify(...)` result prints as
  JSON you parse once. `--json` gives the exact structured response.
- Use one session name per task. After "session unresponsive", make the one
  move it names and keep that name; a new name per error leaves daemons behind.

- The CLI accepts daemon keepalives every 10 seconds while a command runs. The stall budget is unchanged; the total wait is at least 180 seconds (longer when the command budget requires it). A "daemon still busy" error leaves the command running: wait before checking the session again, and do not replay a side-effecting command.

## One round trip per step

An action and the read that checks it belong in the same command:

```bash
chrome-use click @e3 --observe       # act, then see what changed (delta + requests)
chrome-use fill @e7 "cheese" --observe
chrome-use navigate /cart --observe  # navigation returns the new tree, not a diff
```

Use a separate `snapshot -i` only to discover a page you have not read yet.
When you re-read a page you already have, ask for the change, not the page:

```bash
chrome-use snapshot -i --diff        # only what changed since your last read
```

Both `--observe` and `--diff` say "no change" explicitly. That is evidence,
not a failure: read the `why:` line under it before doing anything else. Do
not re-run the same read without an action in between; the second answer is
the first one.

Read the action result and `observed.status`. An observation returns bounded
changes and request context, or a new tree after a document replacement. An
unavailable observation or `action_outcome_unknown` does not prove the action
failed. Inspect current state before acting again; never blindly replay a
send, submit, purchase, or other action that may have happened. For
asynchronous work, wait on the relevant condition, then read; do not use
fixed sleeps as proof.

## Group known work

Use `batch` for a known action sequence, `script` for bounded conditional
observe/decide/act/verify flows, and `form fill --map` for several fields
instead of one model decision per field. Do not guess refs after a page
replacement. Verify the requested result, not how many calls were sent.
Use `snapshot -i -c` or a scoped read when discovering controls. Text/refs
usually suffice; request a screenshot only for a visual question.

An `observed.noProgress` hint means repeated attempts had unchanged evidence,
not that the action failed. Inspect state or wait for the relevant condition;
do not turn the hint into an automatic replay.

Scripts preserve observed hints in CLI top-level `advisories`
(daemon-envelope `data.advisories`), bounded to 20 entries,
including nested results and failed runs. JSON op-list scripts additionally
keep `noProgress` on the corresponding `steps` entry; text output prints the
aggregate advisories once. A JS failure retains `ok:false`, `return:null`,
`error`, `logs`, and `advisories`. A nested script with daemon-envelope `data.ok:false` fails
the parent even if transport `success` is true. Read program outcome, not
transport acknowledgement.

## An action that did nothing is information

When a click, fill, or select reports no change:

1. Read the `why:` note. It says whether the control was disabled, unrendered,
   off-screen, or covered by another element, and names the cover.
2. Fix that one thing (dismiss the banner, scroll the control into view, wait
   for the field to enable).
3. Retry the most direct semantic action on the same target. `pick` for a
   custom dropdown, `type --key-events` for an autocomplete, `do @ref <action>`
   for an expand or menu.

Do not repeat the identical command hoping for a different result, and do not
drop to `click x y` because a ref click was quiet. Coordinates give up every
check that told you why; use them only for canvas and WebGL.

## Do not reopen the page you are on

`open <url>` on the URL you are already at reloads the page and throws away
whatever the page had in progress: a half-filled form, an expanded panel, an
in-memory login. When you need a fresh load on purpose, say so:

```bash
chrome-use reload --observe
```

Working on a local dev server: after a code or build change, `reload`, then
read again. Hot reload is not a given.

## Lookups: one direct attempt, then the site's own navigation

For a read-only lookup it is fine to go straight to the obvious detail or
search URL derived from what the user asked, then confirm the answer on the
page. That is one navigation.

What it is not: a loop over guessed URL variants, a grid of query strings, or
a list of candidate pages opened in turn. If the one direct attempt fails or
cannot be confirmed, switch to the visible page: its own search box, its own
filters, its own links. If you fall back to a search engine, run one focused
query, open the strongest result, and verify there.

When `read <url>` gives you the answer, you are done; `snapshot` is for pages
you have to act on.

## The page's own signal ends verification

A selected option, a checked box, a success toast, a basket line, a URL
parameter that reflects the sort you chose: when the page exposes one
authoritative signal for the fact you need, that is the answer. Do not confirm
it again through a header badge, a second surface, or another full snapshot.
`expect` exists for the cases where you want the check to be a command with an
exit code, not a habit. One authoritative signal is enough unless another
signal contradicts it; command dispatch alone never settles the question.

Once the requested task is complete, stop exploring. Answer, or move to the
next task.

## Sessions and tabs

- Name the session before opening anything: `session name "🔎 short task
  name"`. The user sees it as the tab group label.
- Your tabs are scratch. The daemon closes them when it goes idle. `keep`
  exempts one, and it takes the reason: `keep --as deliverable` for a tab that
  IS the result (a document you edited, a checkout the user must finish, a page
  they asked to see), `keep --as handoff` for one a later turn resumes from
  (waiting on a login, an approval, a payment, a code). Bare `keep` means
  `deliverable`. `tab list` shows them as `[kept: deliverable]` /
  `[kept: handoff]`, which is how the user finds out why a tab is still open —
  so a wrong reason is worse than none. Never `keep` research, search, source,
  intermediate, duplicate, blank, or error tabs: if everything is kept, the
  mark means nothing and the user's window fills with your scratch work.
- Changed your mind? `keep --release` takes the tab back, so it closes with the
  session again. It works only on a tab you kept earlier — that record is the
  proof you opened it. The tab does not rejoin your tab group; ungrouping is
  one-way.
- An empty `tab list`, a tab that went stale, or one command that timed out is
  not a disconnected browser. Keep the session, `open` the URL you need, and
  carry on. Do not restart the daemon, re-run setup, or re-read this skill for
  those errors. Restart only when an error says the browser itself is gone.
- Use `close` only for the current session when its work is finished.
  `close --all` would close other agents' sessions too, so it refuses while
  they are live; do not add `--force` to get past that.

## Talking to the user

- Report in the user's terms: pages, buttons, fields, what happened. Words
  like daemon, relay, CDP, session id, ref, or the text of a runtime error
  belong in a bug report, not in the answer, unless the user asks for them.
- When browser control was taken away (the user clicked into the tab, the
  extension stopped the session), say that plainly and stop; do not quote
  the raw error.
- A screenshot the user asked for goes in the answer as an image, not as a
  path. When you are testing a site for the user, capture the key moments
  and include them.

## Remember a site's quirks (site notes)

A site behaves the same every time you visit it. When you work out something
durable — a working selector, a URL pattern, a hidden field a form needs, an
anti-bot trap, what requires login — **write it down so the next run doesn't
re-discover it.** Keep one markdown file per domain (these are your own notes,
not shipped with the skill):

```
~/.chrome-use/site-patterns/<domain>.md
```

**Before** working on a domain, read its file if it exists (use your normal file
tools — this is plain markdown you own). Treat it as *hints, not guarantees* —
sites change; verify before relying. **After** a successful session that taught
you something durable, create or update it. Suggested shape:

```markdown
---
domain: app.example.com
updated: 2026-06-05
---
## Platform traits
SPA; form renders ~1s after load (wait --text). Cloudflare on /login.

## Working patterns
- Address pick: the `<li>` closes on blur — select with CLICK_MODE=dom.
- Submit needs hidden `point_choice` set (eval), the UI never exposes it.
- Stable selector for "Continue": button[data-testid=submit]
```
