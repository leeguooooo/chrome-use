# Windows relay screenshot diagnostic (#798)

This manual experiment uses a freshly built Release CLI, Chrome for Testing and
one new synthetic loopback profile per case. It refuses to run outside Windows
GitHub Actions. It does not use a maintainer browser, real account or AWS instance.
The shipped extension and CLI source remain unchanged. Generated extension diffs,
whole-extension hashes, source SHA, executable hashes, toolchain, OS/image and
Pillow versions are retained. This is diagnostic instrumentation, not a general
stealth benchmark or proof that the reporter's document has been fixed.

## Frozen original controls

The original 45-case matrices at source `f9599826712e89b2ba1c4074ff67285ba80c3d7a`
are runs `40909669657` (pending DOM read enabled) and `40909841620` (disabled).
Both reproduced 15 minimized native screenshot timeouts. Their original reports
remain unchanged, including ten full-image harness failures per run: innerWidth
included a scrollbar, whereas the native full clip and actual PNG were 1265px
wide. These are harness dimension errors, not product failures. The updated full
check uses the actual native clip and independently observed document extent.
Do not present reclassification of those old images as a new product measurement.

The `original` workflow option retains the three-state matrix for separate future
investigations. It is not a replacement for those immutable controls.

## Candidate suite

Dispatch `experiment: candidates`, `samples: 5`, `rows: 1000`, `dpr: 1` and
`pending_dom_control: false`. All three configurations run in one job against
one exact Release executable, so their binary hash is shared. Configuration
order rotates each repetition. Each configuration has viewport, full-page and
off-screen selector cases, yielding 45 fresh profiles:

- `baseline`: the existing fromSurface:true screenshot request.
- `view`: select fromSurface:false before the one native screenshot send.
- `focus`: reassert the existing Emulation.setFocusEmulationEnabled:true
  primitive on the same attachment, then send one fromSurface:true screenshot.
  A failed or unacknowledged two-second control stops before capture.

Warmup first decodes a green fixture sentinel in an already normal disposable
window. A minimized warmup window refuses setup instead of restoring or focusing
it. Setup intentionally minimizes the owned report window once.
No restore, activation, bring-to-front, window update, synthetic input, animation
clock change or capture fallback occurs during measurement. A timed-out original
native promise is never followed by another screenshot in that profile.

After minimization is confirmed/calibrated and three read-only setup state samples
agree, measurement starts before any generation write and cannot be reset. Thus
the measured interval includes generation writes/reads and capture. The fixture's own application changes a random 32-bit
generation, encoded as black/white pixel strips at the top, middle and bottom,
and changes a row's text. A DOM read must confirm this new generation. Every
configuration makes the same writes and reads: they can themselves wake a
renderer, so success of the new baseline must be reported rather than attributed
to either candidate. PNG decoding checks the new generation independently of the
DOM. Full images check all three strips and colored sentinel pixels; selector
and viewport check their visible strip. CLI dimensions must match viewport,
selector bounds or the independently checked native full clip at the recorded
DPR/scale. Full resolution avoids the product's default downscale. Raster size
is limited to 200 million pixels, including DPR.

The synthetic application owns its event listeners, globals and MutationObserver;
these are fixture code, not injection into a website. Before measurement it saves
its focus/visibility/resize/input event log, globals, DOM mutation count and
resource requests. After wrapper settlement, a direct read on the same debugger
attachment must return the same snapshot and generation. This also runs after a
timeout without invoking CLI health recovery. Any observable change makes that
case inconclusive for a no-side-effects claim.

## Native and attachment evidence

The controller tracks the exact started Chrome PID, original HWND and unique
fixture title. Before setup minimization, a WinEvent hook thread is installed
with a nonzero Chrome PID and an exact HWND/PID callback filter. It pumps messages
on that same thread, retains the callback, requires the owned minimize-start
event as calibration and records unhook acknowledgments. Callback errors, pump
failure, overflow and incomplete shutdown fail closed. During measurement,
20ms samples read only that window plus the foreground handle ID. Event source
and receipt ticks are retained; a bounded 100ms drain permits queued notifications
to arrive. This evidence is bounded observation, not a claim of absolutely
lossless monitoring. State drift, foreground/rectangle changes, relevant native
events, excessive sample gaps or missing calibration prevent PASS.
See Microsoft's [hook contract](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwineventhook)
and [event definitions](https://learn.microsoft.com/en-us/windows/win32/winauto/event-constants).

The unshipped worker observes the actual chrome.debugger.sendCommand edge,
recording target, unique attempt ID and attachment epoch. Attach/detach attempts,
acknowledgments, onDetach, retries and target changes invalidate the sample.
Warmup and measurement must share the same target/epoch. An unresolved health
recovery promise is not a capture dispatch. A loopback handshake samples native
window state near dispatch; it must finish within 250ms. This timing intervention
is common to every configuration. The outer 25-second wrapper timeout is distinct
from native promise settlement. Native response arrival is timestamped before the
asynchronous ownership proof; proof and trace emission have separate times. Optional pending DOM observation, if enabled,
requires that same target/epoch and a still-pending capture at two seconds.
There is no additional debugger client or Runtime subscription.

## Local native PNG transport

For diagnosis, a successful native screenshot answer can upload its raw synthetic
PNG to the loopback-only fixture sink. Ownership, exact case URL and epoch are
checked at send, response and upload. The sink accepts only registered cases,
canonical UUID attempt identities and a matching native send/answer; paths come
from registration, with exclusive creation preventing overwrite. It rejects
chunked, missing-length, oversized, invalid-base64 and invalid PNG/IHDR bodies,
with a 32MiB decoded limit and 44MiB HTTP body limit and 200MP bound. Upload acknowledgment or explicit
error is traced and flushed with a bounded wait before browser cleanup. These
bytes contain only this disposable synthetic fixture. Nothing is sent to an
external telemetry service.

`native.png` is diagnostic evidence, never the requested command's output.
It cannot substitute for `capture.png`, turn a nonzero CLI exit into success,
or hide a crop/dimension/freshness failure. Both images decode their own nonce.
A missing required raw upload or uncertain attachment is inconclusive, never
PASS. Native success and requested CLI success remain separate facts.

## Verdicts and cleanup

Every CLI exit/stdout/stderr, native trace, PNG, window trajectory and generated
diff is retained. PASS requires verified setup, stable target/epoch/window,
complete diagnostics, unchanged fixture snapshot, fresh correctly sized/pixeled
requested PNG, exit zero, no forced deadline kill and confirmed cleanup. A
verified failed or incorrect capture is ISSUES; uncertain setup, diagnostics or
cleanup is INCONCLUSIVE. The optional matchingReportSignature additionally
requires a same-epoch DOM answer while native capture and wrapper were pending.
A non-PASS makes the job fail and still uploads artifacts. No mechanism inside
Chrome is inferred solely from a timeout.

The job has a 60-minute limit and matrix a 30-minute budget. HKCU native-host
registrations are restored, session close is explicit, and only the process tree
started for that case is terminated. No .bat/.cmd script is shipped.

## Pure checks, no browser

```sh
python3 -m venv /tmp/windows-capture-check
/tmp/windows-capture-check/bin/python -m pip install Pillow
/tmp/windows-capture-check/bin/python bench/windows-capture-798/test_probe.py
actionlint .github/workflows/windows-capture-probe.yml
```

These check generator/native-edge guards, candidate single-send behavior,
unacknowledged focus control, nonce pixels, dimension checks, bounded image
transport, and verdict failure paths. They are not Windows runtime acceptance.

## Single-baseline timeout diagnosis

Dispatch `experiment: baseline`, `samples: 1`, `rows: 1000`, `dpr: 1` and
`pending_dom_control: false` to run only one minimized viewport case. This is a
trace investigation; it does not establish either candidate's effectiveness.
The unshipped worker retains the case's warmup/report capture contexts through
teardown, so wrapper settlement no longer discards later detach evidence.
Health mark/recover calls and command call/answer/error pairs carry correlation
IDs; onDetach includes Chrome's reason, and the direct page-after read has a
send event. Instrumentation returns the original promises and leaves production
recovery, errors and teardown order unchanged. These extra loopback trace requests
are diagnostic intervention, not a timing-neutral observation. Missing trace
still prevents a verified conclusion; no detach event alone proves absence of
recovery or explains why Chrome stopped producing frames.
