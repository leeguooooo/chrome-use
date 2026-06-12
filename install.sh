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

case ":$PATH:" in
  *":$bindir:"*) : ;;
  *) printf '\033[33mnote:\033[0m %s is not on your PATH. Add:\n  export PATH="%s:$PATH"\n' "$bindir" "$bindir" >&2 ;;
esac
