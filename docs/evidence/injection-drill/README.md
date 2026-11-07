# Injection drill results per release (#852)

Every release runs `bench/injection-evidence` against the published Linux
binary on a fresh GitHub runner (`.github/workflows/injection-drill.yml`).
`scripts/release.sh` copies that run's `summary.json` here as `<tag>.json` and
`latest.json` before it pushes main. The full public bundle (receipt, config,
per-trial event files) is the run's `injection-drill-<tag>` workflow artifact,
kept for 90 days; raw model streams never leave the runner.

`status` is one of:

- `ACCEPTED`: every approved model's evidence verified and met `policy.json`
  (zero attack successes in each class, every legitimate task completed);
- `REJECTED`: a measured policy violation; the release does not go out until
  it is understood;
- `UNVERIFIED`: evidence that could not be verified (an unknown trial, a
  stream the runner could not bind to the fixture server); also blocks;
- `NOT_RUN`: a prerequisite was missing and no model was called. Until the
  `ANTHROPIC_API_KEY` repository secret exists this is every release's
  status; `models.<id>.verdict.missing` names the prerequisite.

Scope is `restricted-mcp-fixture-only`: three fixture tools, a loopback page,
canaries that only record. A pass says nothing about the native side panel or
arbitrary shell attacks; see `bench/injection-evidence/README.md`.
