#!/bin/sh
# Release chrome-use: bump package.json (sync-version.js carries it into cli/Cargo.toml + Cargo.lock),
# run the fast CI preflight checks, commit "chore(release): prepare vX", push only the vX tag, wait for
# release-binaries.yml to build and publish the GitHub Release, then push main and sync the plugin
# marketplace so Claude Code plugin installs pick the new version up right away (uses your `gh` login).
# main goes last because the marketplace reads the version from main: pushed earlier, the hourly sync
# could advertise a version whose binaries do not exist yet.
# Write the CHANGELOG.md entry (release markers) and both docs/*changelog.html entries first (AGENTS.md).
#   scripts/release.sh [--dry-run] 1.5.145
set -eu
run_ok() {  # run_ok <run-id> [-R owner/repo]: wait until the run completes (gh run watch can drop on a network error), then require success
  _r=$1; shift
  until [ "$(gh run view "$_r" "$@" --json status -q .status 2>/dev/null)" = completed ]; do gh run watch "$_r" "$@" >/dev/null 2>&1 || sleep 15; done
  [ "$(gh run view "$_r" "$@" --json conclusion -q .conclusion)" = success ]
}
DRY=
[ "${1:-}" = --dry-run ] && { DRY=1; shift; }
V=${1:?usage: scripts/release.sh [--dry-run] <version>}
REPO=leeguooooo/chrome-use
MARKETPLACE=leeguooooo/plugins
cd "$(dirname "$0")/.."
die() { echo "error: $*" >&2; exit 1; }
# Release-content checks warn in a dry run (the entry may not be written yet) and stop a real release.
need() { "$@" || { [ -n "$DRY" ] || exit 1; echo "warn: '$*' failed; a real release stops here" >&2; }; }

[ "$(git rev-parse --abbrev-ref HEAD)" = main ] || die "not on main"
[ -z "$(git status --porcelain)" ] || die "working tree not clean"
git fetch -q origin main
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || die "main is not in sync with origin/main"
[ -z "$(git ls-remote --tags origin "refs/tags/v$V")" ] || die "v$V already exists"
[ -n "$DRY" ] && trap 'git checkout -q -- .' EXIT

sed -i.bak "s/^  \"version\": \".*\"/  \"version\": \"$V\"/" package.json && rm package.json.bak
node scripts/sync-version.js >/dev/null
node scripts/check-version-sync.js
need node scripts/release-notes.js "v$V" >/dev/null
for f in docs/changelog.html docs/en/changelog.html; do need grep -q "<strong>v$V</strong>" "$f"; done
# Quiet when they pass; on a failure show what failed instead of exiting silently.
quiet() { _out=$("$@" 2>&1) || { printf '%s\n' "$_out" | tail -40 >&2; die "preflight failed: $*"; }; }
quiet node --test scripts/release-notes.test.js
quiet node --test extensions/ab-connect/*.test.js
quiet sh scripts/test-install.sh
cargo fmt --manifest-path cli/Cargo.toml -- --check
if [ -n "$DRY" ]; then git --no-pager diff; echo "dry run: checks done, version bump reverted"; exit 0; fi

if ! git diff --quiet; then git commit -qam "chore(release): prepare v$V"; fi
git tag "v$V"
git push -q origin "v$V"

# The tag push starts release-binaries.yml, which builds seven binaries from the tag's commit and
# creates the Release. main stays unpushed until it succeeds.
retry() {
  echo "main was NOT pushed. Retry: gh run rerun $1 -R $REPO --failed, and once it is green:" >&2
  echo "  git push origin main && gh workflow run auto-sync-versions.yml -R $MARKETPLACE" >&2
  echo "Or abandon: gh release delete v$V -R $REPO --cleanup-tag -y 2>/dev/null || git push origin :refs/tags/v$V" >&2
  echo "  then: git tag -d v$V && git reset --hard origin/main" >&2
  exit 1
}
RUN=
for _ in 1 2 3 4 5 6 7 8 9 10 11 12; do
  sleep 5
  RUN=$(gh run list -R "$REPO" -w release-binaries.yml -b "v$V" -e push -L 1 --json databaseId -q '.[0].databaseId')
  [ -n "$RUN" ] && break
done
[ -n "$RUN" ] || { echo "error: no release-binaries run for v$V; see https://github.com/$REPO/actions" >&2; retry "<run-id>"; }
echo "waiting for release build $RUN"
run_ok "$RUN" -R "$REPO" || { echo "error: release build $RUN failed" >&2; retry "$RUN"; }
echo "released https://github.com/$REPO/releases/tag/v$V"

# A release prepared as "(Unreleased)" gets its publication date now, as the repo does by hand.
if grep -q "v$V</strong>（未发布）" docs/changelog.html || grep -q "v$V</strong> (Unreleased)" docs/en/changelog.html; then
  sed -i.bak "s|v$V</strong>（未发布）|v$V</strong>（$(date +%F)）|" docs/changelog.html && rm docs/changelog.html.bak
  sed -i.bak "s|v$V</strong> (Unreleased)|v$V</strong> ($(LC_ALL=C date '+%B %-d, %Y'))|" docs/en/changelog.html && rm docs/en/changelog.html.bak
  git commit -qam "docs: record v$V publication date"
fi
# main may have moved while the build ran. Merge, never rebase: the tag must stay an ancestor of main.
git push -q origin main 2>/dev/null || { git pull -q --no-rebase --no-edit origin main && git push -q origin main; } \
  || die "binaries are out but main was not pushed; git pull --no-rebase && git push origin main, then gh workflow run auto-sync-versions.yml -R $MARKETPLACE"

# The marketplace reads the version from package.json on main; run its sync now instead of waiting for the hourly cron.
PREV=$(gh run list -R "$MARKETPLACE" -w auto-sync-versions.yml -e workflow_dispatch -L 1 --json databaseId -q '.[0].databaseId')
gh workflow run auto-sync-versions.yml -R "$MARKETPLACE"
RUN=$PREV
for _ in 1 2 3 4 5 6 7 8 9 10 11 12; do
  sleep 5
  RUN=$(gh run list -R "$MARKETPLACE" -w auto-sync-versions.yml -e workflow_dispatch -L 1 --json databaseId -q '.[0].databaseId')
  [ "$RUN" != "$PREV" ] && break
done
run_ok "$RUN" -R "$MARKETPLACE" && echo "marketplace synced" || echo "warn: marketplace sync run $RUN failed; the hourly run will retry"
gh api "repos/$MARKETPLACE/contents/.claude-plugin/marketplace.json" -q .content | base64 -d \
  | python3 -c "import json,sys; print('marketplace chrome-use:', next(p['version'] for p in json.load(sys.stdin)['plugins'] if p['name']=='chrome-use'))"
