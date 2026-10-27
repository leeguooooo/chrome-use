#!/bin/sh
# Prints the cargo test target flags for one shard of the CLI's tests.
# Shard 2 is agent_panel_native alone: its cases share one lock and take
# about 90s. Shard 0 takes the unit tests and every other remaining
# integration test file; shard 1 takes the rest. New test files land in
# shard 0 or 1 automatically.
set -eu
shard=$1
heavy=agent_panel_native
if [ "$shard" = 2 ]; then
  printf -- '--test %s ' "$heavy"
  exit 0
fi
i=0
[ "$shard" = 0 ] && printf -- '--bins '
for f in $(ls "$(dirname "$0")/../cli/tests/"*.rs | sort); do
  name=$(basename "$f" .rs)
  [ "$name" = "$heavy" ] && continue
  [ $((i % 2)) = "$shard" ] && printf -- '--test %s ' "$name"
  i=$((i + 1))
done
