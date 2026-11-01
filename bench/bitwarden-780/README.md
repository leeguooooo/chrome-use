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

With `inspect_native_ui` enabled, the job also reads native UI Automation
Document metadata before sending Escape. When the runtime reader exists, the
controller uses that exact script and records its hash; otherwise it uses the
CI prototype. Only Document URL values are read, never Edit or Password values.
The browser's raw UI tree initially may contain empty Document widgets. Bounded
read-only rechecks require the fixture URL and a known menu resource URL, rather
than treating a document count as identity proof. Each menu document records its
containing page URL. The privileged Bitwarden owner probe remains a separate
fixture oracle and is not a production capability.

The background case keeps Chrome in front until the single new local action
and its readback finish, then restores only the controller's original cover.
It refuses restoration if foreground ownership or window identity changed.
Restoration requires observing the original cover in front; a successful native
API return alone is insufficient. A post-restoration read must still identify
the same page and counter value, and the final owner probe must find no menu.
Returning focus before the new action is an insufficient sequence in observed
runs: the later click can encounter protected-access denial again.

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

## Production recovery path

Set the workflow input `runtime_focus_recovery` to test the actual CLI flag,
relay handler, native host and foreground cleanup. This mode verifies a zero
page counter first, reproduces the real protected-access refusal, then invokes
`--allow-focus-recovery click #continue` once. The Python controller sends no
native focus or key input in this mode. It requires the runtime's native lease
receipt, verifies the final counter is one, checks the original cover is back
in front, and independently checks the same page, owner menu absence and trusted
Escape history. Native input admission remains distinct from observed delivery.
A failure is not retried to obtain a successful fixture outcome.

## Verified build replay for controller changes

`source_build_run` optionally selects a completed fresh dispatch of this same
repository/workflow. Its Cargo build step must have succeeded, the binary must
match its retained SHA-256 receipt, and Git must confirm identical tracked
product sources with no untracked product files. Only this controller directory,
this manual workflow and its artifact output are excluded from comparison.
Changes to CLI code, extension code, embedded skills or any other product path
require a fresh build. Replays cannot be chained.

Records distinguish `binaryBuildGitSha`, `binarySource` and `probeGitSha`.
An earlier failed diagnostic is usable only when its fresh build succeeded and
all source/hash checks pass. This changes the observer, not the product binary.
Startup waits for a readable, valid debugging port record, retaining file-read
errors without restarting Chrome. Worker discovery re-lists the same Chrome process after read-only
metadata errors, retaining those errors. It never restarts Chrome or repeats
fixture writes/input to obtain a successful response.

The process preflight validates the current controller's creation/ancestry
snapshot; later Chrome identities are still bound and verified independently.
Early digest/fixture/platform failures write INCONCLUSIVE summaries. The fixture
must first reproduce an actual protected-access denial, even if the menu later
disappears during the ordinary CLI read. Session closure must acknowledge verified
tab absence before another profile starts. The background cover's actual native
extended style is checked to reject an accidentally topmost occluder.

Input admission and delivery are separate facts. A native attempt is initially
unattributed. This fixture reports observed Escape delivery only after its
complete page history contains exactly one trusted Escape down/up pair. If an
iframe consumes those events and delivery is not observable, closure remains
unattributed and this diagnostic stays INCONCLUSIVE.

## Foreground permission control (diagnostic only)

`grant_foreground_control` requires `runtime_focus_recovery` and the Bitwarden
experiment. It runs only the background fixture. The first real runtime call
must refuse at the 500 ms native-foreground guard with no admitted input and no
fixture key event. Only then does the controller, which owns the foreground
cover, call `AllowSetForegroundWindow` once for its own Chrome PID. It verifies
that this grant did not itself change the foreground and invokes one new
runtime operation. Unknown-effect failures cannot enter this path.

The original unassisted refusal and the explicit grant remain in the report.
The same-page counter, trusted Escape pair, menu and cleanup checks still apply.
A PASS here means the assisted control worked; it is **not unassisted production
recovery acceptance**. The controller never grants ASFW_ANY, injects Escape,
activates Chrome itself, or bypasses Windows foreground policy. Use an exact
verified source build with identical product sources; do not use this condition
to relabel the original background result.

The control uses `samples` bounded fresh profiles. If the first operation already
runs or fails for another reason, no permission is granted and the operation is
never repeated. Its result and native window/cover state are retained. A successful
operation also gets an immediate read-only counter observation before evaluating
its restoration receipt, so an unconfirmed restore cannot hide the action result.

`foreground_grant_client` further requires `grant_foreground_control`. It builds
a small standalone Rust child that performs only the same public delegation and
read-only foreground queries. The child is launched with the same subprocess
mechanism as a CLI invocation. Its PID, OS result, before/after foreground and
absence of activation/input are retained and checked; denial or malformed data
never falls back to a controller grant. This tests a fresh child's eligibility,
not a product IPC implementation or a guarantee for a persistent daemon, another
host, another Windows session, or an agent whose parent lacks foreground rights.
