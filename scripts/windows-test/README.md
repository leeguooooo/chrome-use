# Windows tests

Tests that run on a real Windows machine. CI covers the Windows build, unit
tests and `scripts/test-install-windows.ps1` (the installer's functions in
isolation); these cover what CI does not — the whole installer against a real
user profile, and the commands against a real Chrome on Windows.

From macOS or Linux, over SSH:

```bash
scripts/windows-test/run-remote.sh user@windows-host            # both
scripts/windows-test/run-remote.sh user@windows-host --smoke    # commands only
scripts/windows-test/run-remote.sh user@windows-host --install  # installer only
```

The host needs OpenSSH Server with PowerShell as its shell (the default on
Windows 10/11) and Chrome installed. Set `WINDOWS_MAC` to wake a sleeping host.
Each script can also be run on the Windows machine directly:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File smoke.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File install-test.ps1
```

Both exit 0 only when every check passes.

## `smoke.ps1`

Drives a throwaway browser (`--launch`, an isolated profile) through open,
snapshot, fill, select-all and insert, select, check, click, `wait --text`,
scroll, screenshots, tabs, `--json`, `doctor`, and close. Each step is checked
against the page itself rather than the exit code. It uses its own socket
directory and session, so it does not touch real sessions on the machine, and
it checks that the daemon it started has exited.

To test a build before it is released, cross-compile and pass it with `--exe`:

```bash
cargo build --release --manifest-path cli/Cargo.toml --target x86_64-pc-windows-gnu
scripts/windows-test/run-remote.sh user@windows-host --smoke \
  --exe cli/target/x86_64-pc-windows-gnu/release/chrome-use.exe
```

Releases use the msvc target, so a gnu build shows whether a change works on
Windows, not that the shipped binary does.

## `install-test.ps1`

Runs the real `install.ps1`, piped into `iex` as a user would, through five
scenarios: install or upgrade to the latest release, run it twice (PATH gains
one entry; existing entries and the registry type are unchanged), reinstall
while a daemon is running from the binary, pin a version, and install to a
custom directory without touching PATH.

**It changes the machine while it runs:** the per-user install at
`%LOCALAPPDATA%\Programs\chrome-use` and the user PATH. Afterwards it restores
both — the previous binary (or no install) and the exact PATH value and type.
Pass `--keep` (`-Keep` on Windows) to leave the new install in place.

To test an `install.ps1` change before pushing it: `--installer install.ps1`.

## What these have caught

- `doctor` ran `chrome.exe --version`, which on Windows starts Chrome against
  the user's real profile instead of printing a version and never returns; the
  installer's self-check hung on it (fixed in v1.5.139).
- Windows PowerShell 5.1 returns a downloaded `.sha256` as a byte array, so the
  installer's first version compared the hash with "50".
- The usual PowerShell API for editing PATH expands `%USERPROFILE%` entries into
  absolute paths for good; the installer edits the registry value instead.

Both scripts only check processes in their own logon session when looking for a
stray Chrome, and are kept to ASCII: Windows PowerShell 5.1 reads a BOM-less
script in the system code page.
