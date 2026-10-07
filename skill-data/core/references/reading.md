# Reading a page

```bash
chrome-use snapshot                    # full tree (verbose)
chrome-use snapshot -i                 # interactive elements only (preferred)
chrome-use snapshot -i -u              # include href urls on links
chrome-use snapshot -i -c              # compact (no empty structural nodes)
chrome-use snapshot -i -d 3            # cap depth at 3 levels
chrome-use snapshot -s "#main"         # scope to a CSS selector
chrome-use snapshot -i -f "SSH|端口|应用"  # keep only matching lines + ancestors (regex)
chrome-use snapshot -i --dom           # list actionable elements from a DOM walk
                                          # (open + closed shadow roots); automatic
                                          # when the AX tree yields no refs at all
chrome-use snapshot -i --json          # machine-readable output
chrome-use snapshot -i --reveal-values # print card / password / code values
```

## Keep model input focused

Use `snapshot -i -c` when empty structural nodes add no context. Use a scoped
read for a known panel and `--diff` when a previous observation leaves an
unanswered question. Keep full-tree reads for context missing from the compact
view. Do not infer a token saving from byte counts; measure each separately.

Text and refs are the default. For visual state, add
`--with-screenshot <path>` to the snapshot or observed action rather than
performing an unrelated capture later. Do not request images on every step
when labels and values already answer the question.

## Read only the needed region

```bash
chrome-use read                            # article text from active tab
chrome-use snapshot -i -s "#main"          # scoped controls
chrome-use snapshot -i -f "Save|Cancel"    # matching lines and ancestors
chrome-use snapshot -i -u                  # include link URLs
chrome-use get text --main                 # omit surrounding boilerplate
chrome-use get attr @e1 href
chrome-use frames                          # discover child frames
```

Refs can reach cross-origin frames and accessible closed-shadow controls.
`get text` without a selector reads all frames; `get text --pierce` helps with
closed shadow roots. Do not replace bounded reads with a full HTML dump.
Use existing page evidence when it answers the next question; do not request
DOM and screenshots together by default. Choose text/refs for controls and
images for visuals.

**Sensitive values are masked.** A field holding a card number, CVC, expiry,
password or one-time code shows `<filled 19 chars>` instead of its value (decided
by `autocomplete`, `type=password`, `name`/`id`, or the field's label), in
`snapshot` and in `get value`. Pass `--reveal-values` only when the value itself
is what you need to check; it then lands in the transcript.

**A select on its placeholder has no value.** `combobox "Prefecture": (nothing
selected; shows "Select")` means the select still sits on a disabled or
"Select…" option. A field the page has flagged carries `invalid`, and validation
messages inside cross-origin frames (Stripe, payment widgets) are listed as
`alert` lines like the page's own.

**Huge / truncated snapshot on a "desktop-shell" web app?** Synology DSM, NAS /
router admin panels, ExtJS apps render many independent app windows into one
accessibility tree, so `snapshot -i` blows past the token cap and buries the
target controls. Don't pipe to `tail` — use **`-f/--filter <regex>`** (or `-s
<css>` to scope to one window's container): `snapshot -i -f "SSH|端口|应用|确定"`
keeps only the matching lines plus their ancestor context, with refs intact.

Snapshot output looks like:

```
Page: Example - Log in
URL: https://example.com/login

- heading "Log in" [level=1, ref=e1]
- textbox "Email" [ref=e2]
- textbox "Password" [ref=e3]
- button "Continue" [ref=e4]
- link "Forgot password?" [ref=e5]
```

Each line is `- <role> "<accessible name>" [<attrs>, ref=eN]`, indented by nesting
depth. You pass the ref to commands as `@eN` (e.g. `click @e4`). The same DOM
node keeps its ref across snapshots within one document; new nodes receive new
refs. Deliberate cursor styles and compact anchors can appear after the ref,
for example `draggable [cursor:grab, class=address-tag]`.

**Validation errors surface too.** When a form rejects a submit, the reason
(`- alert "字数已超过 8 个字"`, `- alert "Email is required"`) is appended as
top-level `alert` lines in `-i` mode — even when the message is a plain styled
`<span>` (`.is-error`, `.invalid-feedback`, `[role=alert]`, `aria-live`), which
`-i` would otherwise filter out as non-interactive. These lines are
informational and intentionally ref-less (you read them; you don't click them).
So if a `click` on a submit no-ops, just re-`snapshot -i` and read the `alert`
lines instead of guessing why.

For unstructured reading (no refs needed):

**Reading an article / docs / prose page? Reach for `read` — not `eval` +
`querySelector`.** `chrome-use read` runs a readability pass on the active tab
(strips nav/header/sidebar/ads, returns clean main content); `chrome-use read
<url>` skips rendering entirely and HTTP-fetches with markdown negotiation /
`llms.txt` / outline. One command, and it beats `open` + `eval
"document.querySelector('article,main,.prose').innerText"` — that hand-rolled
snippet is just a worse reimplementation of what `read` already does (no
readability, no markdown, no `llms.txt`, no boilerplate stripping).

```bash
chrome-use read <url>                  # fetch + markdownify (llms.txt / outline aware) — no render needed
chrome-use read                        # readability extract of the ACTIVE tab (clean main content)
chrome-use get text                    # WHOLE PAGE — all frames by default (see below)
chrome-use get text @e1                # visible text of one element (or a CSS selector)
chrome-use get text --main             # main content only — skip nav/header/sidebar
chrome-use get text --pierce           # read through CLOSED shadow DOM (injected panels)
chrome-use frames                      # list every frame + where the text lives
chrome-use get html @e1                # innerHTML
chrome-use get attr @e1 href           # any attribute
chrome-use get value @e1               # input value
chrome-use get title                   # page title
chrome-use get url                     # current URL
chrome-use get count ".item"           # count matching elements
```

**Whole-page text is cross-frame by default.** `chrome-use get text` with no
selector aggregates visible text across **every** frame — top document plus
same-process child frames plus cross-origin iframes — so you never silently miss
content that lives in an iframe (Yahoo Auctions / Rakuten / Mercari shop
descriptions, embedded checkout/spec frames). Each child frame is delimited with
a `----- frame [kind] url -----` marker. You do **not** need to remember a flag —
the default already reads all frames. (`--all-frames` is still accepted as an
explicit alias.)

So: when text looks missing or wrong, you don't have to guess — just
`chrome-use get text` reads everything. To **see** the structure (which frame
holds what), run `chrome-use frames`. To **cut boilerplate** (global nav/header/
footer, "related items" sidebars), use `chrome-use get text --main`. If content
is lazy-loaded, `scroll` it into view first, then read.

**`eval` runs in the MAIN frame by default.** It does not silently bind to
whichever frame Chrome returns — a bare `eval` always targets the top document.
To run inside a child frame (e.g. a cross-origin Google account-picker), pass
`--frame <index|url-substring|@ref|css-selector>` (indices/urls come from
`chrome-use frames`): `eval --frame accounts.google.com "location.href"`.
Cross-origin (out-of-process) frames run in their own **main world**; same-process
in-page frames run in an **isolated world** (DOM readable, page JS globals not).

**Closed shadow DOM.** Some injected UI (browser-extension debug panels, web
components) renders into a *closed* shadow root that `eval`/`innerText` cannot
read. `chrome-use get text --pierce` reads through closed shadow roots and child
documents via the CDP DOM tree — use it when content is clearly on screen (you
see it in a screenshot) but `get text`/`eval` come back empty. Good news for
*clicking*: `snapshot -i` is built from the accessibility tree, which **already
pierces closed shadow roots** — a closed-shadow `<button>` / `[role=button]`
shows up as a normal `@ref`. So shadow-rendered controls with a11y semantics are
clickable the usual way; only a bare non-semantic clickable `<div>` inside a
*closed* root can slip past both the AX tree and the cursor-element scan.
If the AX tree comes back with **no refs at all** (web-component SPAs such as
developer.apple.com/contact, whose whole page lives inside shadow roots),
`snapshot` / `snapshot -i` automatically lists actionable elements from a
`DOM.getDocument(pierce:true)` walk (open and closed shadow roots, same-process
child documents) instead of printing "(no interactive elements)". Those
`[ref=eN]` work with `click`/`type`/`fill` like normal refs; the JSON carries
`source: "dom"` plus a `note`, and roles/names are derived from tags and
attributes. `snapshot --dom` forces that path.

**Canvas / WebGL UIs (game boards, voice-room mic seats, map tiles, design
canvases).** These paint to a `<canvas>` — there is **no DOM node and no
accessibility node** behind what you see, so `snapshot`/`find`/`eval
querySelector` will never return a ref for them. This is a hard limitation, not a
missing feature. To work with them:
- **Read** the rendered pixels with `chrome-use canvas list` then `chrome-use
  canvas capture [selector] <file>` (extracts the canvas bitmap), or a normal
  `screenshot` of the region — then *you* interpret it.
- **Act** by coordinate: compute the target point and `chrome-use click <x> <y>`
  (or `box @ref` on a container to get its CSS-px box first). Coordinates are the
  *correct* tool here — the snapshot-first rule explicitly carves out canvas.
- On the **relay**, a coordinate click can drift onto the user's foreground tab;
  prefer a `--launch`/owned tab for heavy canvas coordinate work, or confirm the
  underlying state via the app's backend/API instead of driving the canvas.

## Screenshot assets

Use `screenshot [selector] [--clip x,y,w,h] <file>` for reusable image assets,
visual evidence, or canvas state. Screenshots are downscaled to at most 1200px
on the longest edge by default (width only for `--full`, so a long page stays
readable); `--full-res` keeps the captured size, and `--max-width`,
`--max-height`, and `--scale` control the output size. Do not infer CSS click coordinates from a resized
image: for a known ref, `box @ref` returns CSS-pixel bounds and `centerX/centerY`.
Prefer semantic refs for ordinary controls; see the canvas guidance above
when the target has no semantic representation. Screenshots serve as requested
assets and visual evidence; they are not a default extra check after a click.
