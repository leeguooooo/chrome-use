# Interacting

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
chrome-use type @e5 "201-0001" --key-events  # real keystrokes (not insertText) —
                                          # use for autocomplete/combobox fields that
                                          # only react to key events (e.g. a postal box
                                          # that auto-fills city/prefecture, Google Places)
chrome-use type @e6 "ChatGPT" --enter  # type (real keystrokes, implies --key-events)
                                          # then press Enter to COMMIT the candidate in an
                                          # async-autocomplete / tag widget. Use when typing
                                          # alone shows no dropdown and the field needs a tag
                                          # confirmed (e.g. juejin 「添加标签」). If you'd rather
                                          # pick from the list, type --key-events first, then
                                          # snapshot -i and click the candidate.
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

| Env var | Effect |
|---|---|
| `AGENT_BROWSER_VERIFY_REF_TIMEOUT_MS` | Budget for the identity check (default 2s direct CDP, 5s over the extension relay). Raise it on very large pages if you see "identity could not be confirmed". |
| `AGENT_BROWSER_ADAPTIVE_REF=0` | Disable fingerprint relocation (exact role+name only). |
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
`AGENT_BROWSER_HUMANIZE` setting. Note: yidun's *enhanced* slider (icon-shaped
piece + decoys) and its *点选* (click-in-order) captcha are different, harder
challenges not yet handled.

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

### When refs don't work or you don't want to snapshot

Use semantic locators:

```bash
chrome-use find "edit web service settings button"  # ranked candidates, never acts
chrome-use find query "编辑 Web服务规则 设置按钮"
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

`click` auto-scrolls into view and, if the coordinate click is occluded, falls
back to a DOM `.click()`. If a click *reports success but nothing happened* —
classic for an autocomplete/menu `<li>` that closes on the input's blur — retry
that one with `AGENT_BROWSER_CLICK_MODE=dom chrome-use click ...`, or just
`chrome-use eval "<select the item via JS>"`. A DOM-dispatched click (the relay's
default for left clicks) moves focus like a real click: the clicked element, its
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

These paint everything to a `<canvas>` and expose **almost no accessibility
tree**, so `snapshot` comes back near-empty and refs are a dead end. `snapshot`
detects this and prints a one-line hint. Drive them the screenshot way:

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
