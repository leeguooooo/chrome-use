#!/usr/bin/env bash
set -euo pipefail

UPSTREAM_REMOTE="upstream"
UPSTREAM_BRANCH="main"
BASE_BRANCH="main"
TRACK_BRANCH="upstream-main"
SYNC_BRANCH=""
PUSH_BRANCH=false

usage() {
  cat <<'EOF'
Synchronize upstream changes into a dedicated sync branch.

Usage:
  ./scripts/sync-upstream.sh [options]

Options:
  --push                      Push the created sync branch to origin
  --upstream-remote <name>    Upstream remote name (default: upstream)
  --upstream-branch <name>    Upstream branch to sync from (default: main)
  --base-branch <name>        Local base branch for sync branch (default: main)
  --track-branch <name>       Local branch tracking upstream (default: upstream-main)
  --sync-branch <name>        Explicit sync branch name (default: sync/YYYY-MM-DD)
  -h, --help                  Show this help message
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --push)
      PUSH_BRANCH=true
      shift
      ;;
    --upstream-remote)
      UPSTREAM_REMOTE="${2:-}"
      shift 2
      ;;
    --upstream-branch)
      UPSTREAM_BRANCH="${2:-}"
      shift 2
      ;;
    --base-branch)
      BASE_BRANCH="${2:-}"
      shift 2
      ;;
    --track-branch)
      TRACK_BRANCH="${2:-}"
      shift 2
      ;;
    --sync-branch)
      SYNC_BRANCH="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage
      exit 1
      ;;
  esac
done

for var_name in UPSTREAM_REMOTE UPSTREAM_BRANCH BASE_BRANCH TRACK_BRANCH; do
  if [[ -z "${!var_name}" ]]; then
    echo "Error: ${var_name} cannot be empty." >&2
    exit 1
  fi
done

if [[ -n "$(git status --porcelain)" ]]; then
  echo "Error: working tree is not clean. Commit or stash changes first." >&2
  exit 1
fi

if ! git remote get-url "$UPSTREAM_REMOTE" >/dev/null 2>&1; then
  echo "Error: remote '$UPSTREAM_REMOTE' does not exist." >&2
  exit 1
fi

echo "Fetching upstream branch: ${UPSTREAM_REMOTE}/${UPSTREAM_BRANCH}"
git fetch "$UPSTREAM_REMOTE" "$UPSTREAM_BRANCH"

if git show-ref --verify --quiet "refs/heads/$TRACK_BRANCH"; then
  echo "Updating local track branch: $TRACK_BRANCH"
  git switch "$TRACK_BRANCH" >/dev/null
  git merge --ff-only "${UPSTREAM_REMOTE}/${UPSTREAM_BRANCH}"
else
  echo "Creating local track branch: $TRACK_BRANCH"
  git branch "$TRACK_BRANCH" "${UPSTREAM_REMOTE}/${UPSTREAM_BRANCH}"
fi

echo "Switching to base branch: $BASE_BRANCH"
git switch "$BASE_BRANCH" >/dev/null

if git show-ref --verify --quiet "refs/remotes/origin/$BASE_BRANCH"; then
  echo "Fast-forwarding ${BASE_BRANCH} from origin/${BASE_BRANCH}"
  git fetch origin "$BASE_BRANCH"
  git merge --ff-only "origin/${BASE_BRANCH}"
fi

if [[ -z "$SYNC_BRANCH" ]]; then
  SYNC_BRANCH="sync/$(date +%F)"
fi

if git show-ref --verify --quiet "refs/heads/$SYNC_BRANCH"; then
  suffix=1
  while git show-ref --verify --quiet "refs/heads/${SYNC_BRANCH}-${suffix}"; do
    suffix=$((suffix + 1))
  done
  SYNC_BRANCH="${SYNC_BRANCH}-${suffix}"
fi

echo "Creating sync branch: $SYNC_BRANCH"
git switch -c "$SYNC_BRANCH" "$BASE_BRANCH" >/dev/null

merge_message="chore(sync): merge ${UPSTREAM_REMOTE}/${UPSTREAM_BRANCH} into ${BASE_BRANCH}"
echo "Merging $TRACK_BRANCH into $SYNC_BRANCH"
if ! git merge --no-ff "$TRACK_BRANCH" -m "$merge_message"; then
  echo ""
  echo "Merge conflict detected. Resolve conflicts, then run:"
  echo "  git add <resolved-files>"
  echo "  git commit"
  if [[ "$PUSH_BRANCH" == true ]]; then
    echo "  git push -u origin $SYNC_BRANCH"
  fi
  exit 1
fi

echo "Upstream merge completed on branch: $SYNC_BRANCH"

if [[ "$PUSH_BRANCH" == true ]]; then
  echo "Pushing branch to origin: $SYNC_BRANCH"
  git push -u origin "$SYNC_BRANCH"
  echo "Done. Open a PR: ${SYNC_BRANCH} -> ${BASE_BRANCH}"
else
  echo "Branch is local only. Push when ready:"
  echo "  git push -u origin $SYNC_BRANCH"
fi
