#!/bin/sh
# Exercise the real installer tail with a fixture CLI and no terminal/network.
set -eu
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
test_dir=$(mktemp -d)
trap 'rm -f "$test_dir/chrome-use" "$test_dir/tail.sh" "$test_dir/run.sh" "$test_dir/output" "$test_dir/calls"; rmdir "$test_dir"' EXIT
cat > "$test_dir/chrome-use" <<'EOF'
#!/bin/sh
printf '%s\n' "$1" >> "$TEST_CALLS"
case "$1" in
  skill) exit "${SKILL_EXIT:-0}" ;;
  doctor) exit "${DOCTOR_EXIT:-0}" ;;
esac
EOF
chmod +x "$test_dir/chrome-use"
sed -n '/^# --- guided setup:/,$p' "$repo/install.sh" > "$test_dir/tail.sh"
cat > "$test_dir/run.sh" <<'EOF'
set -eu
BIN_NAME=chrome-use
bindir=$TEST_DIR
have_tty() { return 1; }
info() { printf '%s\n' "$1"; }
err() { printf '%s\n' "$1" >&2; exit 1; }
. "$TEST_DIR/tail.sh"
EOF
export TEST_DIR="$test_dir" TEST_CALLS="$test_dir/calls"
for scenario in skill-failure doctor-failure success explicit-skip; do
  export SKILL_EXIT=0 DOCTOR_EXIT=0 AGENT_BROWSER_NO_SKILL=''
  expected=0
  case "$scenario" in
    skill-failure) SKILL_EXIT=7; expected=1 ;;
    doctor-failure) DOCTOR_EXIT=9; expected=1 ;;
    explicit-skip) SKILL_EXIT=7; AGENT_BROWSER_NO_SKILL=1 ;;
  esac
  : > "$TEST_CALLS"
  actual=0
  sh "$test_dir/run.sh" > "$test_dir/output" 2>&1 || actual=$?
  [ "$actual" -eq "$expected" ] || { cat "$test_dir/output"; exit 1; }
  completed=0
  grep -q 'CLI installation complete' "$test_dir/output" && completed=1
  [ "$completed" -ne "$expected" ] || { cat "$test_dir/output"; exit 1; }
  if [ "$scenario" = explicit-skip ]; then
    if grep -q '^skill$' "$TEST_CALLS"; then exit 1; fi
  fi
  printf 'pass: %s\n' "$scenario"
done
