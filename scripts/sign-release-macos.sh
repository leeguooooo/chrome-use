#!/usr/bin/env bash
# Sign + notarize the macOS binaries of a published release, in place.
#
# Why this exists alongside the CI step: signing in CI means putting the
# Developer ID private key into GitHub's secret store. That is normal practice
# and the workflow supports it — but it is not required. This script does the
# same job from a machine that already holds the key, so the key never leaves
# it.
#
# What signing buys (measured, not assumed): an ad-hoc signature does NOT
# survive being copied — macOS then SIGKILLs the process as soon as it spawns a
# thread, which surfaces as a daemon dying with rc=137. A Developer ID
# signature survives. Notarization additionally flips `spctl -a` from
# `rejected` to `accepted / source=Notarized Developer ID`, which is what a
# browser-downloaded (quarantined) copy is checked against.
#
# Usage:
#   scripts/sign-release-macos.sh v1.5.113                # sign + notarize + re-upload
#   DRY_RUN=1 scripts/sign-release-macos.sh v1.5.113      # do everything except upload
#
# Requires: a `Developer ID Application` identity in the keychain, and a
# notarytool keychain profile (default `chrome-use-notary`; create with
# `xcrun notarytool store-credentials`).
set -euo pipefail

TAG="${1:-}"
[ -n "$TAG" ] || { echo "usage: $0 <tag> (e.g. v1.5.113)" >&2; exit 2; }
REPO="${REPO:-leeguooooo/chrome-use}"
IDENTITY="${MACOS_SIGN_IDENTITY:-Developer ID Application: LI GUO (6ZPXG4KVVS)}"
PROFILE="${NOTARY_PROFILE:-chrome-use-notary}"
DRY_RUN="${DRY_RUN:-}"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

for asset in chrome-use-darwin-arm64 chrome-use-darwin-x64; do
  echo "── $asset"
  ( cd "$work" && gh release download "$TAG" -R "$REPO" -p "$asset.tar.gz" --clobber )
  mkdir -p "$work/$asset" && tar xzf "$work/$asset.tar.gz" -C "$work/$asset"
  bin="$work/$asset/chrome-use"

  before="$(codesign -dv "$bin" 2>&1 | grep -c 'Signature=adhoc' || true)"
  codesign --force --options runtime --timestamp -s "$IDENTITY" "$bin"
  codesign --verify --strict "$bin"
  echo "   signed (was adhoc: $before)"

  ditto -c -k --keepParent "$bin" "$work/$asset.notarize.zip"
  xcrun notarytool submit "$work/$asset.notarize.zip" \
    --keychain-profile "$PROFILE" --wait --timeout 30m
  # A bare executable cannot hold a stapled ticket (only .app/.dmg/.pkg can);
  # Gatekeeper verifies notarization online. Confirm the outcome rather than
  # assuming the submission implies it.
  spctl -a -vv -t install "$bin" 2>&1 | sed 's/^/   /'

  ( cd "$work/$asset" && tar czf "../$asset.tar.gz" chrome-use LICENSE* )
  ( cd "$work" && shasum -a 256 "$asset.tar.gz" > "$asset.tar.gz.sha256" )

  if [ -n "$DRY_RUN" ]; then
    echo "   DRY_RUN: not uploading. Rebuilt archive at $work/$asset.tar.gz"
  else
    gh release upload "$TAG" -R "$REPO" \
      "$work/$asset.tar.gz" "$work/$asset.tar.gz.sha256" --clobber
    echo "   uploaded (sha256 replaced too)"
  fi
done

echo
echo "Done. Anyone who already downloaded the old archive keeps a working but"
echo "ad-hoc-signed binary; the checksum published now matches the signed one."
