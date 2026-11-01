# Real Bitwarden menu diagnostic

Run the existing Windows capture probe workflow with experiment `bitwarden`.
This is a CI-only diagnostic for #780, not a production recovery implementation.

It uses the official Bitwarden 2026.10.1 Chrome ZIP with a pinned archive hash,
a freshly built exact-source CLI, current source relay,
Chrome for Testing and a disposable profile. It seeds synthetic account metadata
and a deliberately invalid fixture token with no user key. Each key uses the
official serialized shape and is written separately: Bitwarden intentionally
ignores multi-key storage change notifications. External DNS is
blocked. No real account, vault, password, login or online submission is used.
The fixture server contains only a login form; submitting it is always an error.
Only fixture-site host access is added to the copied relay manifest.

The job runs three fresh profiles: the default agent-window behavior, an
explicitly foregrounded fixture, and a fixture covered by a CI-owned ordinary
window. It continues only after each owned process tree is verified stopped.
The cover is not topmost, so normal foregrounding can raise Chrome over it.

The observer first requires actual Bitwarden extension frames. Their identity
comes from the official owner's runtime URLs and runtime contexts, since
webNavigation can omit the protected frames and resource URLs can be dynamic.
The controller also requires adoption of the synthetic account in Bitwarden's
actual auth observable, live content monitoring and a qualified email field. It records an
ordinary relay read and whether the menu remains. If protected access is
reproduced, the candidate foregrounds only the exact CI-owned fixture window,
checks that its bound tab was already active, and sends one native Escape
key-down/up pair if the menu remains after focusing. It distinguishes menu
absence after the CLI read, after native focus, and after native Escape. It
never replays input after an uncertain result. A subsequent successful read
must name the exact fixture document and leave the menu absent. A new local
button is then clicked exactly once, its page-owned counter is read back, and
the menu must remain absent. This is a new continuation action, never a replay
of the failed original command. It sends no form submission.

When native focus leaves the real menu present, a native desktop crop records
only the exact foreground CI-owned fixture window before Escape. It is an
illustration for review, not a claim about a maintainer's browser or vault.

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
