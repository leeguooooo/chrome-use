#!/bin/sh
# chrome-use installer — downloads the prebuilt binary from the
# GitHub Release (no npm, no auth for you or your users).
#
#   curl -fsSL https://raw.githubusercontent.com/leeguooooo/chrome-use/main/install.sh | sh
#
# Env overrides:
#   AGENT_BROWSER_VERSION=v0.27.0-fork.11   pin a specific release tag
#   AGENT_BROWSER_BIN_DIR=/usr/local/bin    install location (auto-detected otherwise)
set -eu

REPO="leeguooooo/chrome-use"
BIN_NAME="chrome-use"

err() { printf '\033[31merror:\033[0m %s\n' "$1" >&2; exit 1; }
info() { printf '\033[36m==>\033[0m %s\n' "$1" >&2; }

command -v curl >/dev/null 2>&1 || err "curl is required"
command -v tar >/dev/null 2>&1 || err "tar is required"

# --- detect platform -> release asset name -------------------------------
os=$(uname -s)
arch=$(uname -m)
case "$os" in
  Darwin) plat="darwin" ;;
  Linux)  plat="linux" ;;
  *) err "unsupported OS: $os (use the Windows .exe asset from the Releases page)" ;;
esac
case "$arch" in
  x86_64|amd64)   cpu="x64" ;;
  arm64|aarch64)  cpu="arm64" ;;
  *) err "unsupported architecture: $arch" ;;
esac

# musl (Alpine etc.) gets the statically-linked Linux build
libc=""
if [ "$plat" = "linux" ] && ! ldd /bin/sh 2>/dev/null | grep -qi 'gnu\|glibc'; then
  if [ -e /lib/ld-musl-x86_64.so.1 ] || [ -e /lib/ld-musl-aarch64.so.1 ]; then
    libc="-musl"
  fi
fi
asset="chrome-use-${plat}${libc}-${cpu}"

# --- resolve release tag --------------------------------------------------
tag="${AGENT_BROWSER_VERSION:-}"
if [ -z "$tag" ]; then
  info "resolving latest release..."
  # Resolve via the releases/latest redirect on the github.com web host, NOT the
  # api.github.com JSON API (which rate-limits unauthenticated callers to 60/hr).
  # github.com/<repo>/releases/latest -> 302 -> github.com/<repo>/releases/tag/<TAG>
  loc=$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
    "https://github.com/${REPO}/releases/latest" 2>/dev/null || true)
  case "$loc" in
    */releases/tag/*) tag="${loc##*/releases/tag/}" ;;
    *) tag="" ;;
  esac
  [ -n "$tag" ] || err "could not resolve latest release (set AGENT_BROWSER_VERSION=vX.Y.Z)"
fi

base="https://github.com/${REPO}/releases/download/${tag}"
tgz_url="${base}/${asset}.tar.gz"
sha_url="${tgz_url}.sha256"

# --- download + verify ----------------------------------------------------
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
info "downloading ${asset} (${tag})..."
curl -fsSL "$tgz_url" -o "$tmp/pkg.tar.gz" \
  || err "download failed: $tgz_url (is asset '${asset}.tar.gz' attached to release ${tag}?)"

if curl -fsSL "$sha_url" -o "$tmp/pkg.sha256" 2>/dev/null; then
  info "verifying checksum..."
  expected=$(awk '{print $1}' "$tmp/pkg.sha256")
  if command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$tmp/pkg.tar.gz" | awk '{print $1}')
  elif command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp/pkg.tar.gz" | awk '{print $1}')
  else
    actual=""; info "no sha256 tool found, skipping verification"
  fi
  [ -z "$actual" ] || [ "$expected" = "$actual" ] || err "checksum mismatch (expected $expected, got $actual)"
else
  info "no .sha256 published, skipping verification"
fi

tar -xzf "$tmp/pkg.tar.gz" -C "$tmp"
[ -f "$tmp/${BIN_NAME}" ] || err "archive did not contain ${BIN_NAME}"
chmod +x "$tmp/${BIN_NAME}"

# --- choose install dir ---------------------------------------------------
bindir="${AGENT_BROWSER_BIN_DIR:-}"
if [ -z "$bindir" ]; then
  if [ -w /usr/local/bin ] 2>/dev/null; then bindir="/usr/local/bin"; else bindir="$HOME/.local/bin"; fi
fi
mkdir -p "$bindir"

mv "$tmp/${BIN_NAME}" "$bindir/${BIN_NAME}"

info "installed -> ${bindir}/${BIN_NAME}"
"$bindir/${BIN_NAME}" --version 2>/dev/null || true

# --- guided setup: install the Chrome extension + banner preference -----------
# When a real terminal is available (works even under `curl … | sh`, whose stdin
# is the piped script — we borrow /dev/tty), run the guided native-host +
# extension install and ask once whether chrome-use may auto-restart Chrome to
# remove the "started debugging this browser" banner. Non-fatal: a declined or
# failed setup never breaks the binary install. Skip with AGENT_BROWSER_NO_SETUP=1.
if [ -e /dev/tty ] && [ -z "${AGENT_BROWSER_NO_SETUP:-}" ]; then
  info "setting up the Chrome extension + debugging-banner preference..."
  "$bindir/${BIN_NAME}" extension install < /dev/tty > /dev/tty 2>&1 || true
else
  info "skipped extension setup (no terminal). Run \`${BIN_NAME} extension install\` later."
fi

# --- install the AI agent skill (delegates to the binary → skills.sh) --------
# The binary alone lets *you* run chrome-use; the skill teaches your AI agent
# (Claude Code, Cursor, Codex, …) how. `chrome-use skill install` shells out to
# `npx skills add …`; it never writes runner dirs itself. Non-fatal, opt-out
# with AGENT_BROWSER_NO_SKILL=1. Branch on tty (NOT exit code) so a no-Node box
# doesn't print the guidance twice.
if [ -z "${AGENT_BROWSER_NO_SKILL:-}" ]; then
  if [ -e /dev/tty ]; then
    "$bindir/${BIN_NAME}" skill install < /dev/tty > /dev/tty 2>&1 || true
  else
    "$bindir/${BIN_NAME}" skill install || true
  fi
fi

# --- self-check + first prompt ----------------------------------------------
# One read-only pass so the user sees binary/extension/skill status at a glance,
# then a copy-paste prompt that exercises the whole chain in their agent.
info "self-check..."
"$bindir/${BIN_NAME}" doctor --quick --offline 2>/dev/null || true
printf '\n\033[36m==>\033[0m %s\n\n    %s\n\n' \
  "All set. Paste this into your AI agent (Claude Code / Cursor / Codex):" \
  "Use chrome-use to open https://news.ycombinator.com and tell me the top 3 titles"

case ":$PATH:" in
  *":$bindir:"*) : ;;
  *) printf '\033[33mnote:\033[0m %s is not on your PATH. Add:\n  export PATH="%s:$PATH"\n' "$bindir" "$bindir" >&2 ;;
esac
