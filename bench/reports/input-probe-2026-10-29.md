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
scrollY remained 600. This is the reproduced header case the new centering
path targets; arbitrary modal overlays must remain refusals.

The original workflow artifact contains full raw/coalesced points, Cargo
artifact metadata, binary hash, Chrome/toolchain versions and before/after
machine state. The initial/final runner load averages were 0.23 and 2.34;
these diagnostic intervals are not a platform-independent performance result.

## Candidate

Pending the same manual workflow on the candidate. Check the exact SHA/browser
metadata and real event/click counts before treating any gap as changed. The
candidate must pass modal, same-name replacement and movement-during-settle
refusal tests as well as the positive header case. There is no speedup claim.
