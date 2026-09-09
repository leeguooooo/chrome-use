# Behaviour: how to act, not just which command

Rules for the loop between commands. Load this when a task is running long,
when you catch yourself repeating a step, or before a lookup that could turn
into a crawl.

**Related**: [waiting.md](waiting.md) for what a read means,
[trust-boundaries.md](trust-boundaries.md) for what needs the user's say-so.

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
exit code, not a habit.

Once the requested task is complete, stop exploring. Answer, or move to the
next task.

## Sessions and tabs

- Name the session before opening anything: `session name "🔎 short task
  name"`. The user sees it as the tab group label.
- Your tabs are scratch. The daemon closes them when it goes idle. Use `keep`
  only for a tab that is itself the deliverable (a document you edited, a
  checkout the user must finish, a page they asked to see) or that a later
  turn must continue from. Never `keep` research, search, source, duplicate,
  blank, or error tabs.
- An empty `tab list`, a tab that went stale, or one command that timed out is
  not a disconnected browser. Keep the session, `open` the URL you need, and
  carry on. Do not restart the daemon, re-run setup, or re-read this skill for
  those errors. Restart only when an error says the browser itself is gone.

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
