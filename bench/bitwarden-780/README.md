# Real Bitwarden menu diagnostic

Run the existing Windows capture probe workflow with experiment `bitwarden`.
This is a CI-only diagnostic for #780, not a production recovery implementation.

It uses the official Bitwarden 2026.10.1 Chrome ZIP with a pinned archive hash,
a checksum-verified chrome-use 1.5.276 reference CLI, current source relay,
Chrome for Testing and a disposable profile. It seeds synthetic account metadata
and a deliberately invalid fixture token with no user key. External DNS is
blocked. No real account, vault, password, login or online submission is used.
The fixture server contains only a login form; submitting it is always an error.
Only fixture-site host access is added to the copied relay manifest.

The observer first requires actual Bitwarden extension frames. It records an
ordinary relay read and whether the menu remains. If protected access is
reproduced, the candidate foregrounds only the exact CI-owned fixture window,
checks that its bound tab was already active, and sends one native Escape
key-down/up pair if the menu remains after focusing. It distinguishes menu
absence after the CLI read, after native focus, and after native Escape. It
never replays input after an uncertain result. A subsequent successful read
must name the exact fixture document and leave the menu absent.

Artifacts retain window/frame snapshots, CLI outcomes, fixture event trust,
input admission, binary/source hashes and process cleanup proof. Missing menu
reproduction, access failures or unverified cleanup remain INCONCLUSIVE. PASS
is one disposable fixture outcome; it does not prove the original macOS report,
an unlocked real vault, fullscreen/background behavior, cross-platform recovery
or the entire stealth benchmark. No production default is changed.

Both the Python entrypoint and extension-worker controller refuse ordinary
local execution. Do not run this on the maintainer's Chrome or copy their
profile. The extension-worker CDP helper configures only the disposable fixture and
reads browser metadata. For initialization diagnostics it also reads
Bitwarden's own isolated content-script realm on the local fixture through
the official extension's scripting API. Those probes add no main-world
property, DOM node, event listener or event, and return no token contents.
Stored account metadata alone is not proof that Bitwarden adopted that state
or that a locked/unlocked menu was rendered.
