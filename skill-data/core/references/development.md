# Repository build and test workflow

When changing chrome-use source, use the repository's SSH Cargo runner so
compilation and automated tests do not consume the workstation's CPU/RAM.

```bash
git config --local chromeuse.remoteHost <ssh-alias>
pnpm test:190
pnpm build:190
pnpm build:native
```

The alias is local configuration, not a value to commit. No local fallback is
performed. `scripts/REMOTE-BUILD.md` documents Cargo passthrough, disk limits and
verified artifact retrieval. Add new source paths with `git add -N <path>` before
uploading: the snapshot includes Git-listed working files and uncommitted edits.

Read the receipt's input hash, toolchain and exit code before claiming remote
validation. A built binary, remote unit tests and acceptance in a real logged-in
browser are separate results. Do not send browser profiles, vault data, SMS
codes or session state as build inputs.
