#!/usr/bin/env bash
# Run the Windows tests on a Windows machine over SSH, from macOS or Linux.
#
#   scripts/windows-test/run-remote.sh <user@host> [--smoke] [--install] [--exe <local chrome-use.exe>] [--keep]
#
# With neither --smoke nor --install, both run. The Windows side needs OpenSSH
# Server with PowerShell as the shell (the default on Windows 10/11), and Chrome
# installed for the smoke test.
#
#   --smoke            smoke.ps1: the everyday commands against a throwaway browser.
#                      Tests `chrome-use` on the remote PATH, or the install location.
#   --install          install-test.ps1: install.ps1's scenarios. Changes the remote
#                      user's install and PATH while it runs and puts both back.
#   --exe <path>       copy this local chrome-use.exe over and smoke-test it instead,
#                      e.g. a cross-compiled build that has not been released yet:
#                        cargo build --release --manifest-path cli/Cargo.toml --target x86_64-pc-windows-gnu
#                        scripts/windows-test/run-remote.sh me@win --smoke \
#                          --exe cli/target/x86_64-pc-windows-gnu/release/chrome-use.exe
#   --installer <path> run install-test.ps1 against this local install.ps1 instead
#                      of the one on main, to test a change before it is pushed
#   --keep             leave the fresh install in place after --install
#
# WINDOWS_MAC=aa:bb:cc:dd:ee:ff sends a Wake-on-LAN packet first if the host
# does not answer — test machines tend to be asleep.
#
# Exit status: 0 when every test passed, 1 otherwise.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
host="${1:-}"
[[ -n "$host" && "$host" != -* ]] || { sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }
shift

smoke=0 install=0 exe="" installer="" keep=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --smoke) smoke=1 ;;
    --install) install=1 ;;
    --exe) exe="$2"; shift ;;
    --installer) installer="$2"; shift ;;
    --keep) keep=1 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done
[[ $smoke -eq 0 && $install -eq 0 ]] && smoke=1 && install=1

ssh_opts=(-o ConnectTimeout=15 -o BatchMode=yes -o ServerAliveInterval=10 -o ServerAliveCountMax=60)
# OpenSSH warns on every connection to a Windows host without post-quantum key
# exchange; the warning is not the test's business.
quiet() { grep -v -e 'post-quantum' -e 'store now, decrypt later' -e 'openssh.com/pq.html' || true; }
remote() { ssh "${ssh_opts[@]}" "$host" "$@" 2>&1 | quiet; return "${PIPESTATUS[0]}"; }

reachable() { ssh "${ssh_opts[@]}" "$host" 'exit 0' >/dev/null 2>&1; }
if ! reachable; then
  if [[ -n "${WINDOWS_MAC:-}" ]]; then
    echo "==> $host is not answering; sending Wake-on-LAN to $WINDOWS_MAC"
    python3 - "$WINDOWS_MAC" <<'PY'
import socket, sys
mac = sys.argv[1].replace(':', '').replace('-', '')
packet = b'\xff' * 6 + bytes.fromhex(mac) * 16
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
for port in (7, 9):
    s.sendto(packet, ('255.255.255.255', port))
PY
    for _ in $(seq 1 18); do reachable && break; sleep 10; done
  fi
  reachable || { echo "error: cannot reach $host over ssh" >&2; exit 1; }
fi

# A per-run directory under the remote user's profile, removed at the end.
dir="chrome-use-test-$(date +%s)"
remote "New-Item -ItemType Directory -Force -Path \"\$env:USERPROFILE\\$dir\" | Out-Null"
cleanup() { remote "Remove-Item -Recurse -Force \"\$env:USERPROFILE\\$dir\" -ErrorAction SilentlyContinue" >/dev/null || true; }
trap cleanup EXIT

copy() { scp -q "${ssh_opts[@]}" "$1" "$host:$dir/$2" 2>&1 | quiet; }
copy "$here/smoke.ps1" smoke.ps1
copy "$here/install-test.ps1" install-test.ps1
[[ -n "$exe" ]] && copy "$exe" chrome-use.exe
[[ -n "$installer" ]] && copy "$installer" install.ps1

run_ps() { remote "powershell -NoProfile -ExecutionPolicy Bypass -File \"\$env:USERPROFILE\\$dir\\$1\" $2"; }

status=0
if [[ $smoke -eq 1 ]]; then
  echo "==> smoke test on $host"
  args=""
  [[ -n "$exe" ]] && args="-Exe \"\$env:USERPROFILE\\$dir\\chrome-use.exe\""
  run_ps smoke.ps1 "$args" || status=1
fi
if [[ $install -eq 1 ]]; then
  echo "==> installer test on $host"
  args=""
  [[ -n "$installer" ]] && args="-Installer \"\$env:USERPROFILE\\$dir\\install.ps1\""
  [[ $keep -eq 1 ]] && args="$args -Keep"
  run_ps install-test.ps1 "$args" || status=1
fi
exit $status
