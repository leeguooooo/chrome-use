#!/bin/sh
# Prints the cargo test target flags for one half of the CLI's tests.
# Shard 0 takes the unit tests and every other integration test file;
# shard 1 takes the rest. New test files land in a shard automatically.
set -eu
shard=$1
i=0
[ "$shard" = 0 ] && printf -- '--bins '
for f in $(ls "$(dirname "$0")/../cli/tests/"*.rs | sort); do
  [ $((i % 2)) = "$shard" ] && printf -- '--test %s ' "$(basename "$f" .rs)"
  i=$((i + 1))
done
