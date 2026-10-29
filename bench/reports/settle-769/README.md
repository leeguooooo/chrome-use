# Snapshot settle on scroll-linked animations

A warm interactive snapshot of the public GitHub repository took a median
1122.22 ms before this change and 226.43 ms after it. Five baseline/candidate
pairs alternated on the same eight-core macOS 26.5.2 ARM host, using separate
headed Chrome 156 profiles. Each target was activated outside the timed call.
Both release binaries were checked against their remote build receipt SHA-256.
No personal Chrome profile or vault was used.

| Page and default snapshot | Baseline median (range), ms | Candidate median (range), ms |
|---|---|---|
| GitHub repository | 1122.22 (1114.13–1124.21) | 226.43 (224.09–227.98) |
| Static fixture | 124.33 (123.04–132.50) | 128.27 (118.70–140.79) |
| Hidden attribute ticker | 1025.06 (1017.65–1026.62) | 1022.35 (1017.77–1024.55) |

There were no failed calls. All sixty timed snapshots passed content checks,
and each page had an identical normalized snapshot-tree hash across binaries
and modes. Fixture counters proved the hidden ticker actually ran. Warmup calls
were excluded. The static-page ranges overlap, so no static-page speed change
is established.

The removed delay was the settle wait, not faster tree extraction: every
baseline GitHub call reached the animation ceiling; every candidate call went
quiet. A separate diagnostic run found two GitHub underline-navigation
ScrollTimeline animations whose computed duration was a CSSNumericValue
percentage (`100%`), not milliseconds. The old Infinity-only check treated these
as finite clock animations. The new finite-number check excludes them while
retaining ordinary finite animations. DOM mutation, network and post-action
reaction checks were not changed; the hidden ticker still reaches the ceiling.

The host load was 1.77 before and 2.31 after the alternating run, on eight cores.
No compiler was active at the first check; one Cargo/rustc process was active at
the final check. Alternation exposes both sides to that background load, but
these numbers remain one controlled run, not a general latency promise.

Raw samples, receipt identities, source-file hashes, output hashes and activity
counters are in [ab.json](ab.json). Animation data from the separate diagnostic
run is in [animations.json](animations.json). Paths and the private host name
were omitted from the public copies. Candidate runtime, test, help and skill-source hashes matched the
release-build manifest; the build receipt source commits
name the common base because the candidate was built from working-tree edits.

Run on the configured SSH build host after building both release binaries:

```sh
python3 bench/settle-snapshot-check.py /path/to/baseline \
  --receipt /path/to/baseline-receipt.json \
  --compare-binary /path/to/candidate \
  --compare-receipt /path/to/candidate-receipt.json \
  --output /tmp/settle-ab.json
```

The script refuses an existing output file, checks release artifact hashes,
uses only its new test sessions, and closes both profiles afterwards. It also
captures `--no-settle` controls. Do not run it on the maintainer's working desktop.
