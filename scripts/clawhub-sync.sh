#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"
SKILL_NAME="agent-browser-stealth"

if ! command -v pnpm >/dev/null 2>&1; then
  echo "pnpm is required for ClawHub sync"
  exit 1
fi

if [ ! -f "skills/${SKILL_NAME}/SKILL.md" ]; then
  echo "Missing skill file: skills/${SKILL_NAME}/SKILL.md"
  exit 1
fi

# Sync only this fork-owned skill to avoid permission errors on other skills.
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT
mkdir -p "$TMP_DIR/skills"
cp -R "skills/${SKILL_NAME}" "$TMP_DIR/skills/${SKILL_NAME}"

echo "Syncing local skill '${SKILL_NAME}' to ClawHub..."
cd "$TMP_DIR"
pnpm dlx clawhub@latest sync --all --root ./skills
echo "ClawHub sync completed."
