# Windows loaded-generation feasibility probe

This branch tests a prerequisite for issue #780. It does not implement foreground
authorization. The `windows-build-identity-prototype` Cargo feature defaults off.
Normal builds have no embedded marker or new runtime process inspection.

Run the existing Windows agent panel workflow manually with
`probe_windows_identity=true`. It cross-compiles the actual GNU release product
with its normal thin LTO and stripping, archives it, checks the archive and
extracted product hashes on Windows, and runs GUI-free owned-daemon tests. A
separate MSVC job checks the developer build. Neither job launches Chrome,
requests desktop permission, calls AllowSetForegroundWindow, or sends input.

The checker pins read-only executable files, compares complete artifact SHA256,
then reads only bounded PE headers and one 64-byte read-only `.cufg` marker from
the previously bound peer's main image. The caller's actual static marker must
match its pinned disk file. Same-file identity and byte-identical copies are
reported separately. Borrowed module handles are never closed. A failed artifact
comparison occurs before acquiring VM_READ rights. This is matching artifact
content and an observed loaded generation, not mapped-code integrity,
Authenticode, unique installation identity, or a defense against compromised
same-user processes.

Observed GNU and MSVC loaded headers replace the PE32+ ImageBase field with the
actual module base. Header comparison accepts only the unchanged preferred
address or that exact actual base for this one field; all other header bytes
remain exact. Section layout, marker bytes and memory protections are checked
independently. The initial strict comparison refused three positive cases under
each toolchain; bounded fixture diagnostics identified the field before changing
its comparison rule.

The controlled build requires a fresh nonzero 16-byte hex
`CHROME_USE_BUILD_GENERATION` for every Cargo invocation and exact locked/offline
dependency metadata at `CHROME_USE_MARKER_DEPENDENCIES`. The helper hashes source,
generated and embedded resources, resolved dependency contents, build flags,
compiler and linker information. Reusing a nonce across a later relink is outside
this prototype's contract. A production generation strategy and build-invalidation
checks remain required before this marker can authorize anything.

`KernelOwner` is an internal unsafe caller contract. This prototype does not
implement TCP ownership attribution, daemon/native-host role validation, Chrome
ancestry, input-desktop checks, or a foreground grant. The tests derive peers only
from owned actual CLI child handles and disable automatic site refresh. Windows
Known Folders are not redirected by setting HOME; the tests use explicit private
socket/temp directories and issue no browser commands. Live executable replacement
that the OS refuses is reported as inconclusive, never as a passing replacement
test.
