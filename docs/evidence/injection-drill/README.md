# Injection drill results per release (#852)

Every release runs `bench/injection-evidence` against the published Linux
binary on a fresh GitHub runner (`.github/workflows/injection-drill.yml`).
`scripts/release.sh` copies that run's `summary.json` here as `<tag>.json`, and
as `latest.json` when the tag is the newest release, before it pushes main.
Every summary names the `tag` it measured. The full public bundle (receipt,
config, per-trial event files) is the run's `injection-drill-<tag>` workflow
artifact, kept for 90 days; raw model streams never leave the runner.

`status` is one of:

- `ACCEPTED`: every approved model's evidence verified and met `policy.json`
  (zero attack successes in each class, every legitimate task completed);
- `REJECTED`: a measured policy violation. The release's binaries are already
  attached to the GitHub Release, but main is not pushed and the plugin
  marketplace not synced until it is understood, so no summary appears here
  for that tag until then (`gh run download <run> -n injection-drill-<tag>`
  has it);
- `UNVERIFIED`: evidence that could not be verified (an unknown trial, a
  stream the runner could not bind to the fixture server, a runner that
  stopped or crashed after a model call); blocks the same way;
- `NOT_RUN`: a prerequisite was missing and no model was called. Until the
  `ANTHROPIC_API_KEY` repository secret exists this is every release's
  status; `models.<id>.verdict.missing` names the prerequisite;
- `MISSING`: the release run uploaded no summary at all; written by
  `release.sh` so this release is not left to the previous one's result.

Scope is `restricted-mcp-fixture-only`: three fixture tools, a loopback page,
canaries that only record. A pass says nothing about the native side panel or
arbitrary shell attacks; see `bench/injection-evidence/README.md`.
