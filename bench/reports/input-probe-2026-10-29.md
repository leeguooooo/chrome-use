# #771 native input evidence

This is behavior evidence from an isolated Linux headless Chrome with direct
CDP. It is not a relay, headed, Windows, physical-mouse or detector-pass result,
and native-helper timing excludes CLI/daemon/model overhead.

## Baseline

- Workflow: https://github.com/leeguooooo/chrome-use/actions/runs/40846832278
- Source: `327b3df57c0a1d9d83021c0f5d00302ce426e380`
- Release test binary SHA-256: `db3407cc9739f88e8f73ce51d2d00752a30cd7bbd92d051ba16b040e9a831804`
- Chrome `156.0.8078.47`, Linux x86_64, Rust `1.99.1`, four logical cores.
- One warmup and five recorded clicks per level. All 15 recorded clicks had
  received trusted movement, exactly one down and one trusted click. First jumps,
  move intervals and down gaps were recomputed from the raw events against the
  observed trusted rest at (20,20).

| Level | Recorded clicks | First jump, px (range) | Last move to down, ms (range) | Move/coalesced observations |
|---|---:|---:|---:|---|
| Off | 5 | 800.39 | 15.5–15.8 | 1 move per click; each list size 1 |
| Fast | 5 | 28.13–28.28 | 5.6–6.8 | 6 moves per click; each list size 1 |
| Human | 5 | 1.25–1.36 | 2.5–11.6 | 18 moves per click; each list size 1 |

The reported large Fast **first** jump was not reproduced in this fixture. Its
maximum movement step was about 233px, which is different from a first jump
out of rest. No Fast path change is justified by this sample alone.

All 125 measured moves had one coalesced point and one raw update in this
context. That does not establish that CDP can never coalesce, or that a faster
sampling schedule would match physical input. No sampling-cadence change is
made from this observation.

A top-level ref under the fixed semantic header failed with `element_occluded`
after its existing retries. Target/header click counts were both zero and
scrollY remained 600. This is the reproduced header case the new trusted-wheel
path targets; arbitrary modal overlays must remain refusals.

The original workflow artifact contains full raw/coalesced points, Cargo
artifact metadata, binary hash, Chrome/toolchain versions and before/after
machine state. The initial/final runner load averages were 0.23 and 2.34;
these diagnostic intervals are not a platform-independent performance result.

## Intermediate candidate (not the final header path)

Run [40892533162](https://github.com/leeguooooo/chrome-use/actions/runs/40892533162)
used source `7f46efe502f0e483ba64f23a495ada9362485e21` and the same Chrome version.
All native-click work-count checks and the four targeting safety cases passed;
Human move-to-down gaps were 115.9–190.2ms. Coalesced sizes remained one.

That implementation still used programmatic centering for the new header
recovery. It was not accepted as the final stealth boundary. The final path
must use a bounded trusted wheel gesture and be rerun at its own exact source
and binary hash. Intermediate success is not final wheel-version evidence.

## Final trusted-wheel candidate

- Workflow: https://github.com/leeguooooo/chrome-use/actions/runs/40905516867
- Source: `6f4290d8eeaaabd790710384814dcc0e3a84c195`
- Fresh Release test binary SHA-256: `dd3035aa7386925f2d33be5351185435f0eb295c044b4d021a8020490f929717`
- Chrome `156.0.8078.47`, Linux x86_64, Rust `1.99.1`, four logical cores.
- The Cargo artifact records optimization level 3, test mode and `e2e-tests`.
  The executed test path matches the Cargo executable and binary hash record.
- The same one warmup and five measured clicks per level passed: all 15
  measured clicks had trusted movement, one down and one trusted click.
  First jumps, maximum jumps, intervals and down gaps were recomputed from raw
  events. Each raw update matched its pointermove timestamp and coordinates.

| Level | First jump, px (range) | Last move to down, ms (range) | Median gap, ms |
|---|---:|---:|---:|
| Off | 800.39 | 15.6–15.8 | 15.6 |
| Fast | 28.13–28.20 | 5.6–6.6 | 5.7 |
| Human | 1.25–1.28 | 64.6–124.8 | 87.9 |

All 125 moves still had a coalesced list of size one in this environment.
This is diagnostic behavior evidence, with no speedup, hardware-equivalence,
relay, Windows or detector-pass claim. Initial/final load averages were 2.43
and 2.45 on four logical cores; no cross-machine latency conclusion follows.

The original sticky-ref reproduction now clicked its target once and its
header zero times. The separate targeting fixture recorded these outcomes:

| Case | Trusted wheels | Pointer downs | Target/header/danger clicks | Outcome |
|---|---:|---:|---|---|
| Normal fixed header | 4 | 1 | 1/0/0 | Target clicked |
| Modal overlay | 0 | 0 | 0/0/0 | Refused; scrollY stayed 600 |
| Unmarked overlay | 0 | 0 | 0/0/0 | Refused; scrollY stayed 600 |
| Same-name target replacement | 1 | 0 | 0/0/0 | Refused original detached handle |
| Target moves during settling | 4 | 0 | 0/0/0 | Refused stale landing point |

Every recorded wheel was trusted. The normal/modal/unknown cases observed no
DOM mutation or added page-global names; the replacement/movement cases each
observed exactly the fixture's own intentional mutation. The guarded path
keeps the original handle and document epoch, performs only read-only checks
between wheel segments and refuses instead of falling back to another scroll
or click. Separate fake-CDP tests fault a pending wheel acknowledgement and a
changed document epoch, verifying the wall-clock budget and no subsequent
wheel/press replay.

The original large Fast first-jump observation and the occasional
`scroll --until` false negative remain unconfirmed here. Coalescing and Off
mode input limitations are not changed by these two fixes.
