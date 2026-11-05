# Position marker acceptance (2026-11-05)

`--show-position` (#849–#851) on Chrome for Testing 156.0.8078.80, macOS arm64,
DPR 2, on the isolated build host: an isolated `HOME`, a throwaway profile per
run, a loopback fixture page. The binary is the pre-release build of this
branch (1.5.280 + the marker); the relay runs load this branch's unpacked
extension (0.5.105). Scripts: `scripts/position-marker/`.

Pixels come from a second CDP connection to the same browser (the
`inspector` in `acceptance.mjs`), never from the daemon that draws the marker.
The marker is opaque orange (247,140,0); a pixel counts when r > 220,
100 < g < 185 and b < 90.

## What the page can see: marker on vs off

20 humanized `click`s, a `hover`, five `mouse move`s, a `screenshot`, then the
fixture's own record (`acceptance.json` → `page`).

| Check | `--launch` | relay |
|---|---|---|
| New page globals, symbols, DOM nodes, mutations | same | same |
| Events, their order and `isTrusted` | same | same |
| `pointermove` events; `getCoalescedEvents` total (aligned input) | off 72/72, on 70/70, zoom 73/73 | off 73/73, on 73/73 |
| Frame cadence while the marker is up (`requestAnimationFrame`, 1 s) | 122 frames, median 8.3 ms, either way | same |
| Focus, visibility, viewport (within a run), requests | same | same |
| botcheck.leeguoo.com (`botcheck/`) | 18/19 off, 18/19 on (scroll-source fails either way) | — |

The pointer-move counts differ by a few between runs because humanized paths
are random; every move arrives on its own (coalesced = 1), as without the
marker.

## Marker behaviour (`launch/`, `relay/` `checks.json`, all pass)

| Check | `--launch` | relay |
|---|---|---|
| Drawn at the press (`on-after-click.png`: a 16 DIP square with a white outline; its orange inside is 28×28 = 784 device px at DPR 2, within the button) | yes | yes |
| Not in `chrome-use screenshot` (`on-cu-screenshot.png`) | 0 px | 0 px |
| Back after the screenshot (`on-after-screenshot.png`) | yes | yes |
| Gone 2.6 s after the last point (`on-after-expiry.png`) | 0 px | 0 px |
| Drawn for `hover` | yes | yes |
| Gone after a navigation | yes | yes |
| Off draws nothing | yes | yes |
| Another tab gets a point: the first tab's marker goes | yes | yes |
| Renderer busy 6 s (`eval` loop): cleared once it recovers | yes | yes |
| Daemon stopped (`SIGSTOP`) right after a click | stays until it resumes (no other CDP client) | cleared by the extension after 3.9 s |
| Page zoom 150%: inside the button | yes (976–1003, 640–667 in a 780–1140 × 540–720 button) | — |

## Geometry (`geometry/`)

`geometry.mjs` draws the hinge at the raw CSS point at page zoom 50/100/150/200%
and DPR 1/2. Clicks report the same CSS `clientX/Y` at every zoom, while the
hinge stays at the same device-independent pixels (`z2-d2.png`: dot at 2× the
marker's position). The hinge rectangle is in DIP, so chrome-use multiplies by
`cssVisualViewport.zoom` from `Page.getLayoutMetrics` (no page script). In
the same runs, the first `Page.startScreencast` frame never carried the marker
(0 px in all 8 cases): the `stream` view does not show it. `record` captures
with `Page.captureScreenshot` and keeps it. Desktop capture carries it (Linux
evidence from 2026-10-26, `../position-indicator-20261026-linux/`).

## Cost

| | median click | browser CPU during 20 clicks |
|---|---|---|
| `--launch` off | 333 ms | 0.62 s |
| `--launch` on | 334 ms | 1.97 s |
| `--launch` on, zoom 150% | 332 ms | 2.08 s |
| relay off | 369 ms | 1.33 s |
| relay on | 365 ms | 3.05 s |

Click latency does not change. While the marker is up, input makes the
renderer and GPU repaint the overlay: about 70–90 ms of browser CPU per click (67 ms on `--launch`, 86 ms on the relay)
(`cpuClicks.byType`). A static marker costs 0.14 s per 10 s (`cpu.mjs`).
Taking the marker down during pointer travel was tried and saved little
(1.94 → 1.71 s) for two extra commands per click, so it was not kept.

The relay "off" run had one click of 30.4 s (p90 in `relay/acceptance.json`)
with the marker off. It is not the marker's: `ABExt.sitePolicy` waited for a
`chrome.storage.managed` read that never settles, about every sixth click on
the relay, on 1.5.280 too; fixed in #863.

## Rerun

```sh
CHROME_USE_ISOLATED_GUI_HOST=1 CU=<chrome-use> CHROME=<Chrome for Testing> node scripts/position-marker/acceptance.mjs <out> launch
CHROME_USE_ISOLATED_GUI_HOST=1 CU=<chrome-use> CHROME=<...> EXT=<unpacked extensions/ab-connect> node scripts/position-marker/acceptance.mjs <out> relay
CHROME_USE_ISOLATED_GUI_HOST=1 CHROME=<...> node scripts/position-marker/geometry.mjs <out>
CHROME_USE_ISOLATED_GUI_HOST=1 CU=<chrome-use> CHROME=<...> sh scripts/position-marker/botcheck.sh <out>
```
