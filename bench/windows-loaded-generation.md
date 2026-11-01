# Windows foreground handoff probe

This independent branch contains default-off Windows identity/TCP features and
an experimental on-demand CLI callback. PR #834 remains a separate draft. The
new callback has not passed the complete native recovery matrix or release gates.
Ordinary builds do not compile this callback or carry its image marker.

With both prototype features enabled, only this invocation's explicit
`--allow-focus-recovery` can negotiate the private callback. The daemon requests
it after its existing ownership/page checks, sole Permit consumption, native
preparation and existing cleanup-lease registration. The CLI verifies its actual
root socket, both directions of the daemon-selected socket, matching tool images
and a bounded Chrome parent. It may call AllowSetForegroundWindow once for that
held Chrome process. It never grants the daemon, uses a wildcard, activates a
window or sends input. NativeBegin retains the separate exact-window, document,
last-input and cancellation guards. OS admission does not prove focus or delivery.

The source descriptor comes from the actual Plain WebSocket before split. A
process-local generation and reader-owned invalidation prevent reuse after that
reader ends, including while pending-command cleanup is blocked. Descriptor
capture is not kernel ownership proof or global browser selection. The browser
owner and CLI callback independently enforce those facts.

Private negotiation is removed before generic dispatch/history. A nonce, schema,
one-use request and three-second deadline bind the callback to its root connection.
Malformed/unfinished controls abort before NativeBegin; late ACKs cannot become a
new command. Offered-root transport failures report unknown outcome without
replaying the operation or removing a healthy daemon registration. Once a request
has been delivered, cancellation cannot atomically retract an OS grant; eligibility
is process scoped and is not revoked by Drop or native Finish. A valid OS denial
or genuinely unsupported legacy negotiation can use the previous guarded path.

Run Windows agent panel manually with `probe_windows_identity=true` for the
composition check. It builds both features and the actual GNU release product,
retaining thin LTO/stripping, archives it and validates archive/product/test hashes
on Windows. A separate MSVC job runs the Rust unit and metadata suites. These
owned-daemon fixtures send no browser commands. `probe_tcp_owner=true` separately
checks exact socket ownership and real local WebSocket capture without Chrome;
`probe_focus_protocol=true` exercises scripted private transport only.

After composition passes, the separate Windows capture workflow can run
experiment bitwarden with runtime_focus_recovery and client_foreground_handoff
enabled. That mode requires a fresh controlled build and forbids controller
delegation/source replay. It retains an immutable product copy before linking
its separate test harness. Each positive recovery requires the CLI's redacted
foregroundAuthorization receipt, plus the existing trusted Escape pair, exact
window binding, one continuation, foreground verification and cleanup gates.
AlreadyForegroundProcess does not establish that an OS grant was exercised;
only Admitted records that observation. The fixture still uses a synthetic
account and says nothing about a real vault or installed release.

The image checker pins read-only executable files and compares complete artifact
SHA256 before acquiring VM_READ. It reads only bounded main-image PE headers and
one read-only 64-byte `.cufg` marker. The caller's actual static marker must match
its pinned file. Same-file and identical-copy matches are separate observations.
While both file pins prohibit ordinary write/delete sharing, revalidation checks
held process birth/liveness, principal and the current path's volume/file ID;
repeatedly hashing the same pinned bytes adds no replacement evidence. Borrowed
module/desktop handles are never closed. This is matching artifact content and an
observed embedded generation, not mapped-code integrity, Authenticode, unique
installation identity or a defense against compromised same-user processes.

GNU and MSVC fixtures expose the actual module base in the loaded PE32+ ImageBase
field. Comparison accepts only the original preferred value or that exact actual
base for this field; all other header bytes remain exact. Section layout, marker
bytes and memory protections are checked independently. Bounded diagnostics
identified that field before changing the initial strict comparison.

Controlled builds require fresh nonzero 16-byte hex CHROME_USE_BUILD_GENERATION
for every Cargo invocation and exact locked/offline dependency metadata at
CHROME_USE_MARKER_DEPENDENCIES. Source/generated/embedded resources, dependency
contents, build flags and compiler/linker information are hashed. Product kind 1
requires exactly one chrome-use binary target. Test harnesses use kind 2 and
cannot anchor foreground grants, avoiding a shared build-script output being
mistaken for an identical product generation. Reusing a nonce for a later link
violates the controlled contract. Production build/release invalidation remains
an adoption gate; this branch is not ready to publish merely because its metadata
checks pass.

KernelOwner is an internal unsafe caller contract, not authority from a JSON PID.
The TCP bridge supplies actual kernel row/birth observations; matching-image proof
alone does not establish native-host role. Chrome ancestry and caller input desktop
checks precede a grant, while the exact target-window/input proof remains native.
Windows Known Folders are not redirected by HOME; tests use explicit private
socket/temp paths and disable automatic site refresh. An OS-refused live-image
rename is inconclusive, never a passing replacement test.
