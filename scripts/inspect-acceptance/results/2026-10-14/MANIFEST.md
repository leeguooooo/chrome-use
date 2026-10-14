# Inspect & Ask acceptance, 2026-10-14

| Item | Value |
|---|---|
| Source | feat/inspect-and-ask at 5c4cd8c4 (host) with extension tree of 1ef82fcd (no extension change after it) |
| Host binary | macOS arm64, sha256 19e704d76ee9b87bc46e4620dc0b8e655497ceffce6991dc67f39a62e980060a, built on the build host by scripts/remote-cargo.py (receipt d820d0ba75aa4df1885197b36813130d) |
| Browser | Chrome for Testing 155.0.8059.39 on the build host |
| Rust checks | full `test --bin chrome-use` 2076 passed, 0 failed, 3 ignored; `fmt --check` clean; `clippy --all-targets` no new warnings |
| Extension unit tests | 401 passed |

Files:

- `results.json`: 14 headless scenarios, all passed (extension at the final commit; host binary as above). Uses the acceptance copy of the extension (host_permissions in place of the activeTab grant) and the fake ocs, so delivery here is mocked.
- `headed-results.json`: headed Chrome for Testing on the build host's GUI session with the unmodified manifest (activeTab, no host permissions). A real Alt+Shift+C keystroke sent by peekaboo opened the picker; the screenshot was taken through the activeTab grant.
- `real-ocs.json`: the same host binary driven over native messaging on the maintainer's machine, delivering through the real ocs 0.8.3 to a live Claude Code session: state sent, stored true, wake accepted; the same delivery id sent again returned duplicate without calling ocs. The session received the message.

Not covered here: Windows (CI compiles and tests the Windows build), and a single run combining the real shortcut with real ocs on one machine.

## Stealth (2026-10-15)

Limited observations on Chrome for Testing 155 on the build host, not a guarantee that the extension cannot be detected.

Primary run, bound to the shipped package: `stealth-v0556-zip.json` and `timing-v0556-zip.json`.

- Package: `extensions/ab-connect.zip` sha256 `214f64763c34834b2c488a6ae2895e7a39fc38471d95f1c0343d9b066fc4cef5`, unzipped, with the manifest `key` re-added (the Web Store zip strips it; it is needed for the extension id the native host allows). Loaded tree hash recorded in the result; the worker and the relay both reported version 0.5.56.
- Host: released v1.5.213 darwin-arm64 binary (sha256 verified against the release checksum).
- Reproduce from the repo: `CHROME=… HOST_BIN=… ZIP_SHA256=… node scripts/inspect-acceptance/rig.mjs <unpacked dir> <label>` (uses `scripts/stealth-bench.sh`; exits non-zero on any error, failed check or distinguishable probe), and `node scripts/inspect-acceptance/timing.mjs <unpacked dir> <label> --headless=new` (keeps raw samples).

Observed:

- Static-id probes from a page (`fetch`, `<script>`, `<link>` for `inspect-frame.html/js/css`, `inspect-format.js`, `background.js`, `manifest.json`): no difference from a non-existent extension id.
- Timing, 40 fetches per URL: web-accessible, non-accessible and bogus-id URLs have the same median (0.1 ms) and similar tails; raw samples are in the timing file.
- `scripts/stealth-bench.sh --relay` against this browser: every listed check passed.
- Earlier comparison runs `stealth-v0556.json` (pre-hardening tree) and `stealth-v0555.json` (previous release) gave the same probe results and bench passes.
- No picker DOM exists unless the user presses the shortcut. While picking, the host element and probe attribute are random per session and the acceptance asserts no `chrome-use` / `data-cu-` text in the page DOM; the picker UI itself remains visible to the page while open.
