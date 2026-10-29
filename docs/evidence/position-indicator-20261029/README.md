# Native position indicator: capture and capability follow-up

This is research for #721. No production option or rendering path was added.
The experiment used a new, empty, headed Chrome 156.0.8078.48 profile on macOS
ARM64. Measured geometry was DPR 2, a 900 × 557 CSS viewport and visual viewport
scale 1. Only this private browser was activated, before the baseline, and it
was closed afterwards. No input was sent and no personal profile was opened.

## Observed results

| Check | Result |
|---|---|
| `Page.setInputVisualization({clicks:true})` | Missing from the browser's protocol schema; actual call returned `-32601` |
| Marker with `fromSurface:true` | 2704 marker-colored pixels |
| Marker with `fromSurface:false` | 2704 marker-colored pixels at the same bounds |
| Surface capture after view capture | Still 2704 pixels; view capture did not simply hide the marker |
| Await hide acknowledgement, capture immediately | Zero marker pixels in all five repetitions |
| Queue hide then capture without awaiting hide | Zero marker pixels in five idle repetitions |
| Positive control before each hide trial | 2704 pixels every time |
| End of experiment | Browser close acknowledged |

Both capture flags included the hinge marker. Neither is an exclusion option in
this configuration. Acknowledged hiding needed no explicit sleep before the
next capture; acknowledgement and capture timings are recorded in the raw report. The concurrent trial does not establish safe ordering under a stalled
renderer, different transport or other capture mode. These are capture-path
observations, not an input-latency benchmark or proof of zero overhead.

The positive-control sleeps allow the research marker to paint. There is no
sleep between hide acknowledgement and capture. The probe never calls
`Overlay.enable`, never sends synthetic input, and does not implement a fallback
for a production feature.

The current upstream [Page protocol definition](https://github.com/ChromeDevTools/devtools-protocol/blob/master/pdl/domains/Page.pdl)
contains the experimental click-visualization command. Its presence upstream is
not evidence of availability in the tested Chrome. The
[hinge protocol](https://github.com/ChromeDevTools/devtools-protocol/blob/master/pdl/domains/Overlay.pdl)
has rectangle and color configuration, without an expiry parameter. Neither
observation proves that a compatible indicator is technically impossible.

## Product disposition draft

Do not add a production indicator toggle in this change. Keep strict-stealth
behavior and the existing non-stealth cursor unchanged. The purpose-built native
click API is unsupported on the measured baseline; the hinge prototype still
lacks the acceptance evidence needed to advertise a compatible feature.

Prefer a future prototype of the purpose-built API when a tested browser
actually supports it. Do not silently use DOM injection or enable the general
Overlay inspection mode as a fallback. This is a decision to defer delivery,
not a claim that native visualization cannot work.

If maintainers close the current RFC, record it as research completed with
production work not planned for the present baseline. Reopen only with a
supported candidate and the remaining acceptance matrix below. Do not close it
as a shipped feature or a demonstrated impossibility.

## Minimum production scope and remaining gates

The initial scope would be one optional marker for one session's newly created,
foreground top-level desktop tab. It would use already-resolved action
coordinates after input completion. Unsupported geometry, foreign hinge use,
adopted tabs and uncertain ownership would suppress visualization. It would
never bring a target forward or wait for rendering in the input path.

| Gate | Evidence still required |
|---|---|
| Relay | Independent pixels and unchanged page state/input behavior through the actual extension relay |
| Geometry | Zoom, DPR, scrolling, window movement, panels and iframe coordinates either verified or explicitly suppressed |
| Ownership | Per-tab/session/document/sequence identity; stale updates rejected; unknown or externally owned hinge state preserved |
| Captures | Clip/full-page screenshots, screencasts and desktop recordings separately tested; no contamination or render-induced action failure |
| Lifecycle | Navigation/tab/window races, transport loss, session end, crash and hung-renderer cleanup bounds measured with independent pixels |
| Timing/resources | Equivalent input tasks compared with visualization off/on; no altered trust, pacing, focus, visibility, viewport or site requests |
| Configuration | Off by default, capability negotiation and disabled/unsupported/suppressed/unconfirmed states; legacy cleanup warnings retained |

The earlier #739/#741 probes establish limited direct-CDP pixels, a trusted
click and explicit-detach cleanup. This follow-up adds capture and capability
evidence only. It does not claim the remaining gates passed.

## Reproduce

Run only on an isolated GUI test host, never the maintainer's working desktop:

```sh
CHROME_USE_ISOLATED_GUI_HOST=1 node capture-probe.mjs /tmp/new-cu721-output
node capture-pixels.mjs /tmp/new-cu721-output
```

The output directory must be new. The capture probe records its own SHA-256;
[the raw report](capture-matrix.json) matches the committed script. The pixel
analyzer rejects missing expected images. [pixels.json](pixels.json) records
all trials and [image-sha256.json](image-sha256.json) identifies their images.
These are page captures, including the `fromSurface:false` view path; they must
not be relabeled as independent desktop captures.

The archived pixel helper also now rejects invalid surface names and empty
input before overwriting output. Its original page-image analysis was rerun in
a temporary directory and remained identical.
