---
name: canvas
description: Drive canvas / WebGL / game UIs that paint to a <canvas> with no DOM or accessibility node — game boards, voice-room mic seats, map tiles, 3D viewers, design canvases (Figma/mockitt), HUDs. Use when snapshot/find/eval querySelector return no ref for something clearly on screen, when the label shows in `get text` but has no @ref, or when you need real-time coordinate/keyboard control. Covers canvas capture, coordinate clicks, hold-to-move, batch timed sequences, reading engine globals, and the real-time WebSocket stream.
allowed-tools: Bash(chrome-use:*), Bash(chrome-use:*), Bash(abs:*), Bash(npx chrome-use:*), Bash(npx chrome-use:*)
---

# chrome-use canvas / WebGL / games

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

## Canvas / WebGL apps (games, map & 3D viewers, drawing tools)

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
