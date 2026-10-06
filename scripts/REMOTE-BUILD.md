# Remote developer builds

Configure an existing SSH alias in the checkout's local Git configuration:

```sh
git config --local chromeuse.remoteHost your-build-host
pnpm build:190
pnpm test:190
pnpm build:native
```

The host setting stays outside committed files. `--host` or
`CHROME_USE_BUILD_HOST` overrides it for one invocation. SSH uses existing keys
and strict host-key checking. A connection failure returns an error and never
starts a local Cargo build.

## Cargo checks

```sh
python3 scripts/remote-cargo.py test --bin chrome-use
python3 scripts/remote-cargo.py test --features e2e-tests e2e_mock_request_body_uses_captured_iframe_session
python3 scripts/remote-cargo.py fmt -- --check
python3 scripts/remote-cargo.py clippy
python3 scripts/remote-cargo.py --output cli/target/release/chrome-use build --release
```

Use `--headless` before `test` when explicitly running isolated browser-backed
tests on a display-less build host. Real authenticated browser acceptance uses
the authorized browser session separately.

## Source and results

Only Git-listed working-tree files are uploaded; edits need not be committed.
Use `git add -N path/to/new.rs` to include a new source file without staging its
contents. Ignored build output, browser profiles and the Git directory are not
uploaded.

Each source manifest has a SHA-256. The remote runner verifies every listed file
before and after Cargo execution, uses an isolated short checkout path, and
serializes its shared target cache. Short source paths keep macOS Unix socket
fixtures within their path limit. Developer/test debug symbols are disabled to
reduce memory and disk use.

Receipts under `cli/target/remote-build-receipts/` record the input hash, commit,
host, toolchain, command, duration, exit status and artifact checksum. A fetched
native binary must match its receipt's SHA-256 before replacing an output file.
`pnpm build:native` fetches the release binary before copying it into `bin/`.

`--jobs` defaults to 4; `--timeout` defaults to 1800 seconds. The runner requires
at least 2 GiB free before execution and terminates only its own process group if
free space drops below 1 GiB. It does not delete unrelated caches or projects.
GitHub platform release jobs retain their existing CI environments.
