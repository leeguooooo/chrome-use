# Handoff: making chrome-use cheaper for agents to drive

Written 2026-09-08, at the end of the first pass. If you are picking this up
cold, read this first, then `bench/README.md` for the measurement harness.

## Why this work exists

The prompt that started it: another agent's browser tool felt dramatically
faster to use, and we did not know where to begin.

Two rounds of side-by-side benchmarking answered it. The short version: **we
were not slower, we were chattier.**

| | round 1 (read + navigate, HN) | round 2 (same-page interaction, saucedemo) |
|---|---|---|
| their round trips | 2 | 17 |
| their bytes returned | 322,516 | 22,088 (median 762) |
| ours | 4 calls / 196 KB | stalled on the same element they did |
| our tool time per call | 0.86s | — |
| their tool time per call | 1.01s | 0.83s |

Two things fall out of that table, and both were surprises:

**Transport is not the problem.** Our per-call latency was already lower than
theirs. The number that matters is round trips, because the model time attached
to each one measured 6–7.7s while the tool itself took under a second.

**Task shape decides who wins.** Round 1 (reading, navigating) favoured us —
our `-i` filter returned half the bytes theirs did. Round 2 (a form flow, many
small same-page changes) favoured them — their default AX diff returned a
median of 762 bytes where we sent the whole tree every time. One round would
have produced a confident wrong conclusion. It nearly did.

## What shipped (v1.5.102 → v1.5.105)

- `snapshot --diff` — only what changed since this session's last snapshot of
  the same page. On a 143 KB comment thread an unchanged re-read costs 1 byte.
- `snapshot --max-bytes` / `--from` — cut a huge tree on whole-node boundaries,
  report what was omitted, hand back a cursor.
- `navigate --observe` and friends — the page and its interactive tree in one
  call. Halved round trips on the round-1 task (6 → 3) at identical bytes.
- `--observe` actually works now — it was invisible in text mode and its refs
  were minted into a throwaway map, so they resolved to nothing.
- `actions` / `do` — what an element supports beyond a click, and performing
  exactly one of those.
- `tab select` / `tab adopt` no longer report a switch that did not happen.

## Where to start

Ordered by value, with the reasoning, not just the list:

1. **#228 adaptive wait.** Highest value because it is a *correctness* bug, not
   just a speed one: `--observe` sleeps a hard-coded 250ms, and when that is not
   enough it returns a mid-transition tree as though it were the result. That is
   a silent wrong answer. `snapshot` waits not at all. Replace both with a
   signal (DOM quiet, network idle, transition end) plus a ceiling that *says*
   when it timed out.
2. **#227 `paste --format`.** In a rich-text editor, pasting `text/html` and
   typing character by character produce different documents. Today the only
   route is hand-rolled `ClipboardEvent` synthesis in `eval`. Note the
   constraint written into that issue: do not touch the user's real clipboard —
   we drive their actual Chrome.
3. **#226 `selectText`.** Editing one phrase inside a long field, or placing a
   cursor, currently needs `eval`.
4. **#229 combined structure + pixels.** Narrow value (the project has a hard
   rule against using screenshots as input) and it should follow #228 so both
   are captured at the same moment.
5. **#231 benchmark rigour.** Add a pass/fail assertion per task and record
   warm/cold plus the binary version. Three wrong conclusions in this pass came
   from measuring carelessly; the harness should record the facts rather than
   rely on a README warning.
6. **#230 `showMenu` is a click.** Known trade-off from this pass. Do not add
   retry heuristics speculatively — collect real components that fail first.

Also open, from the same investigation: **#224**, controls with no accessible
name have no `@ref` recovery path. There is a concrete lead in its comments:
`actions` / `do` work fine on nameless elements because they probe by
`backendNodeId` and never re-anchor by role + name. The fix direction is to use
a different discriminator when the name is empty, not to relax verification.

## Two things worth internalising before you touch anything

**The recurring defect is the silent success.** Four separate instances in this
pass. `AGENTS.md` has the list and the rule that follows from it. Every new
command here should be read with the question: what does it print when it did
nothing?

**Verify against the browser, not the protocol reference.** The accessibility
work started from a wrong conclusion — that CDP could not express what the
other tool did — and a probe page corrected it in ten minutes. Two specifics
that the docs would have led you to get wrong: `valuenow` is not reported as a
property at all (the number rides on the node's `value`), and `pressed` arrives
as the JSON string `"true"`, not a boolean. `AGENT_BROWSER_AX_DUMP_PROPS=<path>`
appends every unparsed accessibility property to a file; it costs nothing when
unset and it is how those two were found.

## The other tool, for context

It drives macOS accessibility, not CDP. The evidence is in
`~/.codex/computer-use/Codex Computer Use.app` — the binary carries the standard
action vocabulary (`AXPress`, `AXShowMenu`, `AXIncrement`, `AXDecrement`,
`AXConfirm`, `AXCancel`, `AXPick`, `AXRaise`, `AXShowAlternateUI`,
`AXScrollToVisible`). Its rule "only use an action actually exposed for that
element, do not guess action names" is `AXUIElementCopyActionNames` showing
through.

That is a different path, not a better one. Everything it does that we wanted
turned out to be derivable from CDP accessibility properties. Where its design
is genuinely worth copying, it is the *discipline* rather than the mechanism:
observations diff by default, actions are declared before they can be invoked,
and the tool waits so the model does not have to guess how long to sleep.
