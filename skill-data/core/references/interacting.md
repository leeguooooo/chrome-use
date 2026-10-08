# Interacting

## Ordinary actions

Choose refs from the live page; the numbers below are examples.

```bash
chrome-use click @e3 --observe
chrome-use fill @e2 "hello" --observe       # replace the field value
chrome-use type @e2 " world"               # append
chrome-use press Enter --selector @e2      # focus this control before the key
chrome-use select @e4 "option-value"       # native select
chrome-use pick @e4 --option "Europe" --observe  # custom combobox, with the delta it caused
chrome-use pick @e6 --option "Kyoto"       # autocomplete field: types, clicks the suggestion
chrome-use check @e5
chrome-use uncheck @e5
chrome-use scroll down 500
chrome-use get value @e2
chrome-use get text @e6
chrome-use wait --text "<expected page text>"  # case-sensitive substring
```

`wait --text` matches an exact, case-sensitive substring of the page's visible
text. Use the actual expected page wording: `Saved` will not match
`Delivery saved.` When a receipt or confirmation is already visible, that is
the answer; waiting for it again only spends the budget. A `Wait timed out`
says the condition was not observed; on its own it tells you nothing about
the connection.

Prefer dedicated verbs over handwritten JavaScript: they check ref identity,
handle frames, and dispatch the events widgets expect.

Autocomplete field (type-to-search combobox: suggestions appear only after
you type, and the form wants one of them chosen) → `pick @ref --option "<text>"`.
It clears the field, types the text, waits for the suggestions, clicks the
best match and reports the field's value and any hidden code it set. `select`
does not type, so it cannot reach those suggestions. Use `type --key-events`
(or `--enter` to commit a tag) only when the field reacts to keystrokes alone
or `pick` reports that no suggestion appeared.

A field showing your text does not prove the page saved it. Heed ⚠
warnings from `fill`, `click` and `keyboard type`: a Save still disabled after
a fill, a `dispatch: dom` click, or a refused click on a disabled control all
mean the edit did not register. A ref marked `toggles=checkbox(...)` is a
switch, not a link. It can be destructive, so do not click it to navigate.

If the page discards the element handle part-way through a `fill` (CDP
"Could not find object with given id", seen on the extension relay), `fill`
re-resolves the element once and reads it before doing anything else: a field
that already holds the value is reported with a ⚠ note and not typed again; a
field that does not is filled once more and says so. A field that cannot be
read back at all is unknown, not different: the fill fails with "value is
unknown" and writes nothing; check it with `get value <ref>` first. Do not
repeat the fill yourself after any of these.

## Before you write `eval`

In real sessions most `eval` calls re-implemented a command that already
exists, and lost its verification and hints. Use the command:

| About to eval | Use instead |
|---|---|
| `document.body.innerText`, `el.innerText` | `get text <sel>`, or `read` for the main content |
| `[...].find(b => b.textContent === '查询').click()` | `click "text=查询"` or `find text "查询" click` |
| `getBoundingClientRect()` | `get box <sel or @ref>` |
| patching `fetch`/XHR to see an API response | `network requests --filter api`, then `network request <id>` (reads the body from its original renderer; `responseBodyError` explains an unavailable body) |
| `sleep N` or a polling loop | `wait --text "…"`, `wait <sel>`, `wait --url <pattern>`, `wait --fn "<expr>"` |
| setting `.value` through a native setter | `fill @eN "…"`: it reads the value back and says when it did not stick |
| injecting a script before the page runs | `addinitscript <js>`, then `reload` |

Keep `eval` for what no command does: page globals, framework stores, canvas,
or a diagnostic question the verbs cannot answer, such as hidden form
validity. Do not dump credential-bearing forms or bypass blockers just because
an action failed. `eval` targets the main frame unless `--frame` is set.
`eval` prints a string as text; a `JSON.stringify(...)` result prints as JSON
you parse once.

## Command list

```bash
chrome-use click @e1                   # click
chrome-use click @e1 --new-tab         # open link in new tab instead of navigating
chrome-use dblclick @e1                # double-click
chrome-use hover @e1                   # hover
chrome-use focus @e1                   # focus (useful before keyboard input)
chrome-use fill @e2 "hello"            # clear then type (verified by read-back;
                                          # a mismatch error quotes both values and
                                          # says when the page filtered non-Latin input)
chrome-use type @e2 " world"           # type without clearing (read back too: ⚠
                                          # warning, exit 0, if the page rewrote it)
chrome-use type @e5 "100-0001" --key-events  # real keystrokes (not insertText) —
                                          # use for autocomplete/combobox fields that
                                          # only react to key events (e.g. a postal box
                                          # that auto-fills city/prefecture, Google Places)
chrome-use type @e6 "ChatGPT" --enter  # type (real keystrokes, implies --key-events)
                                          # then press Enter to COMMIT the candidate in an
                                          # async-autocomplete / tag widget. Use when typing
                                          # alone shows no dropdown and the field needs a tag
                                          # confirmed (e.g. juejin 「添加标签」). If you'd rather
                                          # pick from the list, use `pick @e6 --option "…"`.
chrome-use press Enter                 # press a key at current focus (down+up).
                                          # The output names where it landed
                                          # ("Pressed Enter → textarea[name=q]").
                                          # A prior `click <input>` focuses it, also
                                          # over the relay. ⚠ warning (exit 0) when a
                                          # JS-only key (Arrow/Escape/Enter on a bare
                                          # input) finds no key listener on the page:
                                          # it cannot react, click the option instead.
chrome-use press Enter --selector @e2  # focus the target first (alias --on) —
                                          # each CLI call is its own process, so
                                          # don't assume focus stayed put
chrome-use press Control+a             # key combination
chrome-use keydown d                   # HOLD a key down (no auto-release)
chrome-use keyup d                     # release it — pair them to hold-to-move
                                          # in a game: `keydown d; sleep; keyup d`
chrome-use check @e3                   # check checkbox
chrome-use uncheck @e3                 # uncheck
chrome-use select @e4 "option-value"   # native <select> (React/Vue-safe native setter)
chrome-use select @e4 "a" "b"          # select multiple
chrome-use pick @e4 --option "Europe"  # ANY combobox (react-select / ARIA /
                                          # native): opens it, waits for the menu
                                          # (incl. portal-rendered), matches by
                                          # visible text, fires the right events,
                                          # and ERRORS if the option never shows
                                          # (no silent no-op). Use this for custom
                                          # dropdowns where `select` returns ✓ but
                                          # changes nothing.
chrome-use pick @e6 --option "Kyoto"   # AUTOCOMPLETE field (role=combobox input,
                                          # aria-autocomplete=list): types the text,
                                          # waits for suggestions, clicks the best
                                          # match (exact > case > prefix), verifies
                                          # the value; `pick @e6 "Kyoto"` also works
chrome-use upload @e5 file1.pdf        # upload file(s) — works over the extension relay too:
                                          # chrome.debugger forbids setFileInputFiles, so the
                                          # file's bytes are streamed into the page and rebuilt as
                                          # a File there (chunked under native-messaging's 1 MiB cap).
                                          # A consumed/cleared React dropzone returns exit 0 with
                                          # a warning instead of falsely reporting rejection.
chrome-use scroll down 500             # scroll page (up/down/left/right)
chrome-use scroll down 700 --at 640,400 # wheel at a pixel — scrolls a cross-origin
                                          # iframe (Payments/Stripe/checkout/KYC) that
                                          # plain page scroll can't reach
chrome-use scroll down 700 --frame 2    # scroll frame 2 from `chrome-use frames`
chrome-use scroll down --until "#comments"  # step until it is in the viewport (also
                                          # @ref, text=Label, --until-text "…");
                                          # exit 1 naming how far it went if not.
                                          # --max-steps N (30) / --timeout ms;
                                          # add --selector .feed for a scroll container
chrome-use scrollintoview @e1          # scroll element into view
chrome-use drag @e1 @e2                # drag and drop
chrome-use drag @e1 60                 # drag a handle by +60px (slider/canvas); `+60,-3` for dx,dy
chrome-use solve-slider                # auto-solve a 网易易盾 slider-puzzle captcha on the page
chrome-use solve-slider 5              # ...retry up to 5 times (refreshes the puzzle on a miss)
```

**The `@ref` contract: a ref acts on the element it named, or it errors.** Every
verb above re-checks the ref's role + name against the live accessibility tree
before touching anything. If the page re-rendered, chrome-use re-anchors on that
exact role + name, then on the element's stored fingerprint — and if neither
lands confidently it **fails loudly** telling you to re-snapshot. It never
"best-effort" clicks the node that used to be there: a React reconciler handing
the same DOM node to a different component is common, and a silent mis-click
there is how an agent opens the wrong menu (or submits the wrong form) while the
CLI prints `Done`. So an error here is the guard working — re-snapshot and
re-target rather than reaching for `AGENT_BROWSER_VERIFY_REF=0`.

**A relocation is never silent, and never a rename.** When the ref's original
node is gone, chrome-use acts on a replacement only if it is the *same control*:
same role and the same accessible name (ignoring case and extra whitespace),
re-found by role + name, by the replaced node's DOM attributes, or by
fingerprint. One exception: a text field (textbox / searchbox / combobox /
spinbutton) re-found by its own `id`, form `name` or test id is the same field
even when the page rewrote its placeholder ("手机号" → "手机号或邮箱"), so it is
filled, and the label change shows in the report. The response then says so: `--json` gets `data.relocated: [{ref,
how: "role-name"|"dom-identity"|"adaptive", score?, role, name, was: {role,
name}}]` (also on a failed action), and text output prints one `⚠ @e5 relocated
(…)` line on stderr. **A ref that cannot be resolved is refused with
suggestions, never guessed.** That includes a confident match whose name
changed ("Save" → "Save now"): it is not clicked. Instead the error names it
and gives it a ref of its own (`try @e12 [button] "Save now"`), followed by up
to three refs from the current snapshot that are closest by role + name and
still resolve, plus "run `snapshot -i` to refresh". Nothing acts on a
suggestion: pick one yourself, or re-snapshot.

| Env var | Effect |
|---|---|
| `AGENT_BROWSER_VERIFY_REF_TIMEOUT_MS` | Budget for the identity check (default 2s direct CDP, 5s over the extension relay). Raise it on very large pages if you see "identity could not be confirmed". |
| `AGENT_BROWSER_ADAPTIVE_REF=0` | Disable fingerprint relocation (exact role+name only). When on, a fingerprint match is acted on only if its role + name equal the snapshot's; a renamed match is offered as a suggested ref instead. |
| `AGENT_BROWSER_VERIFY_REF=0` | Last resort — skips the check entirely and accepts that clicks may land on a re-rendered node. |

**Slider-puzzle captchas (网易易盾 / yidun).** Unattended/headless logins can't
dodge the login slider (no human, no pre-logged-in session), so `solve-slider`
clears it: it fetches the captcha's own background + jigsaw slices by URL (no
screenshot), locates the gap offline (edge + masked cross-correlation), then
drags the handle into it with a humanized, self-calibrating closed-loop
trajectory — the human motion is what passes yidun's behavioural check. Works in
both float (embedded) and popup (modal, e.g. Zhihu) modes. It auto-detects the
captcha on the active page; run it right after the submit that triggers the
slider. The drag forces the humanize trajectory regardless of the global
`AGENT_BROWSER_HUMANIZE` setting. The built-in detector also handles Yidun's
rotating icon-shaped slider: it calibrates angular/linear motion, matches the
main silhouette while ignoring small decoys, corrects the inline CSS position,
and verifies the result on the same challenge. Ordered icons use a different
workflow. For a readable click-in-order challenge, view its screenshot, identify the requested
order, convert image coordinates to CSS pixels, click, and verify the site's
result. Do not hand off merely because it is a CAPTCHA. Load `core/captcha`
for the full workflow and bounded retries. Exhausted solver attempts return
an error and a nonzero exit code; dispatched input is not verification success.

**Cross-origin iframes (embedded payment / checkout / KYC widgets — Google
Payments, Stripe, etc.) — drive them by ref, never by screenshot.** `snapshot -i`
pierces these out-of-process iframes and lists their elements by `@ref`
(including input values); `get text --all-frames` reads their text. Then just act
on the refs: `click @e`, `type @e`, `hover @e`, `dblclick @e`, `drag @a @b` all
work into the iframe. Over the extension relay these are dispatched through the
DOM (in the element's own frame), so they hit the right element in the right tab
— a coordinate click/scroll there can drift onto whatever tab is in the
foreground, so prefer refs. For below-the-fold content in such a frame, scroll it
with `scroll down N --at x,y` (a pixel over the frame) or `--frame n`. For a
postal/autocomplete box inside the frame, `type @e "…" --key-events`.

**Never target the `<iframe>` element itself.** `focus` and `press` on an
Iframe ref land on the *container* in the parent document — the keystroke goes
to the parent page, not to the field inside. That used to read as a plain `✓`
on the wrong target; it now returns a warning naming the boundary. When you see
it, go through the frame instead: `frames` lists them, then `frame <id>` and
act on a ref *inside* the frame, or `eval --frame <id> "…"`. Clicking an Iframe
ref is the same trap in reverse — click the control inside, not the box around
it.

When a click makes a cross-origin frame appear (a payment sheet, an OAuth
picker, a captcha overlay), the observation after that click reports it as
`newFrames` with a note — the frame's *content* is not in the tree you just
got. Follow the note into the frame instead of concluding the click did
nothing. The ids it reports are accepted by `frame <id>` and `eval --frame
<id>` directly.

> **Caveat: `find` can't reach a CLOSED shadow root or a cross-origin iframe.**
> `find`/selectors match the page DOM (`querySelectorAll`), so they error
> "Element not found" for elements inside either — even though `snapshot -i` lists
> them (it walks the CDP accessibility tree, which pierces both) and `get text`
> reads them. For those, target the element by its **snapshot `@ref`**, not by
> `find`. (`box @ref` gives a coordinate fallback.)
>
> **And verify the exact label before assuming it's missing** — translations
> differ. Real case: LinkedIn's "Save" button is labelled `收藏`, not `保存`, so
> `find text "保存"` finds nothing while `snapshot -i` shows `button "收藏" [ref=eN]`
> all along — just `click @eN`. (issue #55)

Semantic `find role/text/label/placeholder/alt/title/testid` requires exactly one visible match, including locate-only queries. Ambiguity returns up to eight candidates with visible state and selector/context hints; no action is dispatched and input values are omitted. Narrow `--name`/`--exact`, or use `--within <CSS|@ref>` to restrict the query to exactly one container. A scope must belong to the active tab's main document; cross-frame refs and an explicitly selected iframe are refused; use direct frame refs or `frame main`. `find first/last/nth` explicitly selects an order and keeps its existing behavior. Plain CSS actions are unchanged. Roles and label names use Chrome accessibility data, including `aria-labelledby`; a semantic query re-resolves on each call. A detached target before dispatch fails safely; uncertain actions are never replayed.

The existing `chrome_use_find` tool in `mcp --tools all` accepts `within` with the same unique-scope rules; tool count is unchanged. `text` for fill/type is passed literally, including `--name --observe`, rather than parsed as CLI options.

Semantic find retains the existing `--observe` unsupported warning; use one `batch` containing the scoped find action and a task-specific `get text` receipt when both are known. The MCP find tool does not advertise an observe field.

Discover a scope from the page first: `find query "Beta account"` returns candidate selector anchors; choose the article/container corresponding to that heading, then use `find role button click --name Save --exact --within "<returned-selector>"`. Inspect an ambiguous scope with `snapshot -s "<selector>"` and narrow it rather than accepting the first container. Unknown semantic-find options and duplicate `--within` are refused; use `find label Email fill -- --name` to enter literal flag-looking text. `--exact false` requests substring matching.


### An edit that shows but does not save (issue #358)

`fill` and `type` enter text the way a user does: trusted `beforeinput` /
`input` events, and a trusted `change` on blur. A page that ignores synthetic
events still registers them. The field showing your text is not proof the
page's own state took it, so read what the commands report:

- `fill` warns when the field's form or dialog had a disabled Save/Submit
  before the fill and still has it after (JSON `commitControl`), and when it
  had to fall back to a synthetic write (`engine: input-synthetic`).
- `fill` accepts a field that only reformatted the value (Stripe turns `1234`
  into `12 / 34`, a card number into groups of four) and says so with a ⚠ note;
  a value that differs in any other way is still an error, and a card or
  password value is never echoed in it.
- `click` refuses a `:disabled` control with an error, and warns on
  `aria-disabled="true"`. A Save that stays disabled means the edit did not
  register: fix the edit, do not force the click.
- `click` reports `dispatch: pointer | keyboard | dom`. `dom` is
  `element.click()` (`isTrusted: false`) and comes with a warning.
- `keyboard type` reports `target` and `readBack`. It fails when the focused
  field did not change and warns when nothing editable had focus.
- For a Save/Submit, use `click @ref --observe`: it reports the requests the
  click sent. None, and no DOM change, means nothing was saved.

**Buttons that are really switches.** A button that wraps a checkbox, switch
or radio shows as `button "简体中文" [toggles=checkbox(checked=true), ref=e1]`.
Clicking it flips that control. In LinkedIn's "edit profile language" dialog
such a button queues **deleting that language's profile**. Treat any
`toggles=` ref as a setting and do not click it to navigate or pick a tab.

### When refs don't work or you don't want to snapshot

Use semantic locators:

```bash
chrome-use find "edit web service settings button"  # ranked candidates, never acts
chrome-use find query "编辑 Web服务规则 设置按钮"
chrome-use find text "Sign In"                     # locate only: what matched, no click
chrome-use find role button click --name "Submit"
chrome-use find text "Sign In" click
chrome-use find text "Sign In" click --exact     # exact match only
chrome-use find label "Email" fill "user@test.com"
chrome-use find placeholder "Search" type "query"
chrome-use find testid "submit-btn" click
chrome-use find first ".card" click
chrome-use find nth 2 ".card" hover
```

A bare description is a safe discovery query: it returns up to 12 ranked
candidates with role/name/text, computed cursor, and compact selector anchors.
It never clicks automatically. Choose a candidate, then use its selector or
take a snapshot and act on the matching `@ref`.

Or a raw CSS selector:

```bash
chrome-use click "#submit"
chrome-use fill "input[name=email]" "user@test.com"
chrome-use click "button.primary"
```

Escalation ladder: snapshot + `@eN` refs are quickest for straightforward
pages → `find role/text/label` when you'd rather skip the snapshot → raw CSS
→ **`eval` the moment any of those fight you** (stale refs, hidden state,
occluded clicks). Don't retry a flaky structured locator three times; drop to
`eval` and act on the DOM directly.

Selectors starting with `//`, `/`, `(`, `./` or `..` are XPath automatically (no
`xpath=` prefix). `text()` only tests the FIRST direct text node, so a label
split across nodes (React `{a} - {b}`) or nested in a child never matches; use
`contains(normalize-space(.), '…')`, `find "<label>"`, `text=<label>` or a
snapshot `@ref`. "Element not found" keeps the selector plus the resolver's
diagnosis, and an XPath with `text()` that matched nothing explains this.

`click` auto-scrolls into view. If something else covers the target (a cookie
banner, a modal backdrop, a sticky header), the click is **refused** with an
error naming what covers it, because a click there would hit the cover, not the
control. Dismiss the cover and click again; `click --allow-dom` clicks the
covered element through the DOM (`element.click()`, `isTrusted: false`) instead,
for when you know the page accepts that. If a click *reports success but nothing happened* —
classic for an autocomplete/menu `<li>` that closes on the input's blur — retry
that one with `AGENT_BROWSER_CLICK_MODE=dom chrome-use click ...`, or just
`chrome-use eval "<select the item via JS>"`. A DOM-dispatched click moves focus like a real click: the clicked element, its
nearest focusable ancestor, or a label's control gets focus unless the handler
already moved it, so `click <input>` then `press Meta+a` lands on that input. A
plain `<li>` has no focusable target, so the input keeps focus and still selects.

Click a raw pixel point when the only handle you have is a coordinate (canvas,
a marker from a screenshot, a target with no stable selector):

```bash
chrome-use click 449 320            # click viewport point (x y)
chrome-use click 449,320            # same, comma form
chrome-use click --coords 449,320   # same, explicit flag
```

A bare-number argument is always a coordinate, never a selector.

### Canvas / WebGL apps (games, map & 3D viewers, drawing tools)

For semantic controls, prefer refs over pixels. Canvas/WebGL targets can lack
DOM or accessibility nodes: capture their pixels and use coordinates when
needed. Coordinate input over the relay may hit the foreground tab; use an
owned isolated test tab for such work. For a ref's coordinates, `box @ref`
provides CSS-pixel bounds and its center.

These paint everything to a `<canvas>` and expose **almost no accessibility
tree**, so `snapshot` comes back near-empty and refs are a dead end. `snapshot`
detects this, prints a one-line hint, and also saves a viewport screenshot and
prints `screenshot: <path>` — view that image instead of calling `screenshot`
again (JSON: `data.screenshot`, `screenshotReason: "sparse"`; opt out with
`AGENT_BROWSER_SPARSE_SCREENSHOT=0`). Drive them the screenshot way:

```bash
chrome-use canvas list                 # enumerate <canvas> elements (size, type)
chrome-use canvas capture out.png      # save the canvas's RENDERED pixels to PNG —
                                          # toDataURL (full backing-store res, e.g.
                                          # Figma 2522x1904), screenshot fallback for
                                          # WebGL w/o preserveDrawingBuffer / tainted.
                                          # Gets the RENDER, not hidden source data
                                          # (those live in the app's binary store/API).
chrome-use screenshot /tmp/s.png       # SEE the state (your only read path —
                                          # eval/get text return nothing useful)
chrome-use click 640 360               # interact by viewport coordinate
chrome-use press d --hold 800          # hold-to-move, precise (timed in-daemon —
                                          # NOT keydown+shell-sleep+keyup, which
                                          # adds ~250ms jitter per round-trip)
chrome-use press Space                 # discrete actions (jump/attack/confirm)
```

**Symptom: the label shows in `get text` but has no `@ref`.** Voice-room mic-seats
(Zego/Agora), prototype canvases (mockitt/modao), game HUDs, and some web
components paint their controls, so the text appears in `get text`/`read_page`
("Add Add Add…") yet `snapshot -i` lists nothing and `querySelectorAll` returns 0
— there is no addressable node, so `@ref`/`find` can't reach it. Drive by position:

```bash
chrome-use get text --pierce        # FIRST: if it's a CLOSED shadow root (not
                                    #   canvas), this reads through it — cheap to try
chrome-use screenshot /tmp/s.png    # else SEE where the control sits
chrome-use click <x> <y>            # click the pixel (bare numbers = coordinate)
```

Why there's no ref: `<canvas>`/WebGL hit-regions and **closed** shadow roots expose
no DOM/AX node for the painted control, so no amount of snapshot work can mint a
ref — coordinates are the only handle. (Open shadow roots and same-origin /
cross-origin iframes ARE surfaced by `snapshot -i`; only canvas + closed-shadow are
coordinate-only.) On the relay, foreground the agent's own tab first so the
coordinate click can't drift onto the user's other tab.

**Don't drive frame-by-frame with one CLI call per action** — that's the slowest,
lowest-fidelity way (each call is a process spawn + round-trip). Script a *timed
sequence in a single round-trip* with `batch` (it sends each step to the running
daemon; `press --hold` and `wait` block in-daemon, so timing is precise):

```bash
chrome-use batch "press d --hold 900" "press j" "press j" "wait 200" "press d --hold 500"
```

**When a step needs to *use the result of an earlier step*, or you need a loop or a
condition, go up one level to `chrome-use script`** — a whole observe→decide→act→verify
flow in ONE round-trip over the daemon, with the decision logic running next to the
browser instead of bouncing every step back through the model. Two forms:

*JSON op-list* (machine-generatable, dry-runnable) — a later op reads an earlier op's
result via `{{name.path}}`, plus `waitUntil` / `forEach` / `assert` / `set` / `push` / `return`:

```bash
chrome-use script - <<'JSON'
[
  {"do":"navigate","url":"https://example.com"},
  {"do":"evaluate","script":"document.title","bind":"t"},
  {"assert":{"contains":["{{t.result}}","Example"]},"msg":"wrong page"},
  {"return":"{{t.result}}"}
]
JSON
```

*JS program* (ego-style "code base") — a real synchronous JS script with `cu.*` helpers
(`cu.snapshot/eval/open/click/fill/find/waitFor/wait/extract/log`) driving your real,
already-logged-in Chrome. No `await` (each `cu.*` call blocks until the browser returns),
and the engine lives in the daemon so it survives hard navigations:

```bash
chrome-use script --timeout 120000 <<'JS'
  cu.open('https://news.ycombinator.com');
  const out = [];
  for (let page = 0; page < 3; page++) {
    const rows = cu.eval("[...document.querySelectorAll('.athing')].map(r=>({id:r.id,title:r.querySelector('.titleline a')?.innerText}))");
    for (const r of rows) {
      const pts = cu.eval(`+(document.querySelector('#score_${r.id}')?.innerText.match(/\\d+/)?.[0] ?? 0)`); // raw DOM read, no round-trip decision
      if (pts >= 100) out.push({ ...r, points: pts });
    }
    if (!cu.visible('a.morelink')) break;
    cu.click('a.morelink');            // hard navigation — engine survives it
    cu.waitFor('.athing', 8000);
  }
  return out;                          // becomes the script's return value
JS
```

Exit codes: 0 ok · 1 runtime failure / failed `assert` · 2 invalid program. `--dry-run`
validates a JSON program without touching the browser; `--arg k=v` seeds a variable.

Also try reading real state instead of pixels: `eval` runs in the page's main
world, so for a framework/engine game you can often reach its globals (e.g. a
Phaser/PIXI/Three instance, a store, `window.__GAME__`) and read positions/score
directly — far better than guessing from a screenshot.

**For genuinely real-time driving, drop the CLI entirely and use the WebSocket.**
`chrome-use stream enable` opens a bidirectional WS (`stream status` prints the
`ws://127.0.0.1:<port>`). Connect once and you get a live ~60fps screencast AND
can send input on the same socket — no per-action process spawn, no round-trip,
works over the extension relay:

```js
// node (global WebSocket): live frames + locally-timed input
const ws = new WebSocket("ws://127.0.0.1:PORT")
ws.onmessage = e => { const m = JSON.parse(e.data); if (m.type==="frame") {/* base64 jpeg */} }
const k = (eventType,key,code,vk) => ws.send(JSON.stringify({type:"input_keyboard",eventType,key,code,windowsVirtualKeyCode:vk}))
k("keyDown"," ","Space",32); setTimeout(()=>k("keyUp"," ","Space",32), 80)   // a jump
// also: {type:"input_mouse",eventType:"mousePressed",x,y,button:"left",clickCount:1}
```

This is the difference between watching a slideshow and playing the game. Reserve
screenshots for one-off checks; use the WS for any sustained real-time control.

### Virtualized rich editors (Google Docs/Sheets, Notion, Figma, Lark/Feishu)

Unlike canvas, these DO have a DOM — but it **lies**. The editing surface is a
virtualized layer, and the DOM around it is littered with decoys: a hidden
`<textarea>` mirror, an offscreen input, a toolbar/search box, a title field.
`fill @ref` / `type @ref` on a ref plucked from `snapshot -i` often lands your
text in one of those decoys — the title bar, a find box — not the document. The
snapshot-first rule still holds for *navigation* (menus, buttons, dialogs), but
for the **main editing surface**, prove where your keystrokes go before you
commit a paragraph:

```bash
# 1. WRITE PROBE — type one throwaway token, don't dump the whole payload yet
chrome-use click 520 300                # click into the document body by coordinate
chrome-use keyboard type "zzprobe"      # a real keystroke sequence (not fill/insertText)

# 2. VERIFY it landed in the document — not the title/toolbar/a hidden input
chrome-use screenshot /tmp/probe.png    # SEE where "zzprobe" actually appeared
#    (or read it back through the app's export/API path if it has one)

# 3a. Probe landed in the doc  → continue with real keystrokes
chrome-use keyboard type "the real content…"
# 3b. Probe landed in the wrong field → STOP using DOM/fill for this surface.
#     Drive it visually: click the body by coordinate, then real keyboard only.
```

Rules of thumb for these apps:

- **Don't trust `fill @ref` / `type @ref` for the document surface** until a probe
  proves the target. Toolbars, menus, comment boxes, the share dialog — those are
  real DOM and `@ref` is fine. The canvas/grid itself is the trap.
- **Prefer real keystrokes** (`keyboard type` / `keyboard press`) over
  `fill`/`insertText` — virtualized editors listen for key events, and CDP
  `insertText` silently no-ops or writes to the mirror.
- **Verify by readback, not assumption** — screenshot the region, or use the app's
  export/download/API to confirm the content actually landed, before reporting done.
- A one-token probe costs one round-trip and saves the classic failure of a whole
  document typed into the title bar.

## Two ways to drive a page — and when to drop to `eval`

You have a **real Chrome with the user's DOM**. Two layers, mix them freely:

1. **Structured** (`snapshot` + `@ref`, `find`, typed actions) — convenient and
   readable; best for straightforward forms and navigation. Its limit is what
   the a11y view *cannot see*: hidden inputs never appear in it, and overlays
   can still block a coordinate click. (Refs themselves survive ordinary
   re-renders — see the self-heal note above; when relocation genuinely fails
   you are told, rather than left pointing at the wrong element.)
2. **eval-first** (`chrome-use eval "<js>"`) — your eyes and hands on the real
   DOM: read hidden inputs, reach into Shadow DOM / iframes, inspect
   `form.elements` and `.validity`, extract the exact shape you want, or call
   `el.click()` directly. **Inspect blockers first; use `eval` when the dedicated commands cannot
   answer the diagnostic question** — it's the fast way to find *why* something
   failed (e.g. a hidden `point_choice=none` the UI never exposes).

```bash
# "what's actually in this form / why won't it submit?"
chrome-use eval "[...document.forms[0].elements].map(e=>[e.name,e.type,e.value,e.checked])"
chrome-use eval "document.querySelector('[name=point_choice]')?.value"
chrome-use eval "[...document.forms[0].elements].filter(e=>!e.validity.valid).map(e=>e.name+': '+e.validationMessage)"
chrome-use eval "document.querySelector('#stubborn').click()"   # direct DOM click, bypasses overlays
```

> **`eval` shows you *why*; the verb *does the thing*.** The snippets above are
> for introspection (`.validity`, hidden inputs, `form.elements`) and the cases
> no verb covers — that's exactly where `eval` shines. But for a **standard
> operation**, don't hand-roll JS: there's a dedicated command that's shorter and
> smarter (it heals stale refs, pierces cross-origin iframes, fires the events
> React/Vue listen for, and returns structured output — raw `eval` gets none of
> that). Reach for the verb first:
>
> | Instead of `eval …` | Use |
> |---|---|
> | `querySelector('article,main').innerText` | `read` / `get text --main` |
> | `querySelector('#x').click()` | `click @ref` / `click <sel>` (DOM-dispatch bypasses overlays) |
> | `el.value = …` on an input | `fill @ref <v>` (native setter → React/Vue register it) |
> | clicking a `<select>` / combobox option | `select @ref <text>` / `pick` (portal-aware) |
> | `querySelector('[name=x]').value` | `get value @ref` |
> | `querySelectorAll('.x').length` | `get count <sel>` |
> | `getAttribute('href')` | `get attr @ref href` |
> | `el.scrollIntoView()` | `scroll --selector <sel>` |
> | polling a condition in a loop | `wait --text` / `--selector` / `--function`, or `expect` |
> | scraping a repeating list into JSON | `extract --schema` |
> | reading a whole article / docs page | `read` (see the reading section) |
>
> Drop to `eval` when the verb genuinely doesn't fit (custom widget, closed
> shadow, a page global) — not as the default for things a verb already does.
