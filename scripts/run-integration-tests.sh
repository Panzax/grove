#!/usr/bin/env bash
# scripts/run-integration-tests.sh
#
# Run the .hone test bodies as plain bash for fast local iteration.
# CI still runs the real hone CLI; this is a substitute when you don't want
# to install hone (or can't reach the install endpoint). Behavior should
# match the .hone semantics: each TEST gets a fresh subshell; assertions
# inspect exit_code / stdout / stderr of the most recent labeled RUN.
#
# Usage:
#   scripts/run-integration-tests.sh                    # all three .hone files
#   scripts/run-integration-tests.sh agentic            # one file (without .hone)
#
# Prereqs: a built `grove` binary on PATH.

set -uo pipefail

GROVE_BIN="${GROVE_BIN:-$(pwd)/target/release/grove}"
if [[ ! -x "$GROVE_BIN" ]]; then
  echo "error: grove binary not found at $GROVE_BIN" >&2
  echo "       run \`cargo build --release\` first, or set GROVE_BIN=<path>." >&2
  exit 1
fi
export PATH="$(dirname "$GROVE_BIN"):$PATH"

PASS=0
FAIL=0
FAILED_NAMES=()

# Per-test helpers — each TEST runs in a subshell so they don't leak state.
run() {
  local label="$1"; shift
  local out err code
  out=$(bash -c "$*" 2>/tmp/grove-hone-stderr)
  code=$?
  err=$(cat /tmp/grove-hone-stderr)
  declare -g "${label}_stdout=$out"
  declare -g "${label}_stderr=$err"
  declare -g "${label}_exit=$code"
}

assert_eq() {
  local expected="$1" actual="$2" what="$3"
  if [[ "$expected" != "$actual" ]]; then
    echo "  FAIL: $what — expected '$expected', got '$actual'"
    return 1
  fi
  return 0
}

assert_ne() {
  local left="$1" right="$2" what="$3"
  if [[ "$left" == "$right" ]]; then
    echo "  FAIL: $what — expected != '$right', got '$left'"
    return 1
  fi
  return 0
}

assert_contains() {
  local haystack="$1" needle="$2" what="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    echo "  FAIL: $what — expected to contain '$needle'"
    echo "        actual: $(echo "$haystack" | head -3)"
    return 1
  fi
  return 0
}

# Each test wraps its body in this helper.
TEST() {
  local name="$1"
  local body="$2"
  echo -n "  $name … "
  local out
  out=$(bash -c "$body" 2>&1)
  local rc=$?
  if [[ $rc -eq 0 ]]; then
    echo "OK"
    PASS=$((PASS + 1))
  else
    echo "FAIL"
    echo "$out" | sed 's/^/    /'
    FAIL=$((FAIL + 1))
    FAILED_NAMES+=("$name")
  fi
}

# Exported helpers must be visible inside the per-TEST subshells.
export GROVE_BIN
export -f run assert_eq assert_ne assert_contains

# ----------------------------------------------------------------------------
# Test bodies. Mirrors test/integration/*.hone — keep in sync.
# ----------------------------------------------------------------------------

run_agentic_tests() {
  echo
  echo "== agentic.hone =="

  TEST "grove --help lists the new agentic commands" '
    run h "grove --help"
    assert_eq 0 "$h_exit" "exit code"
    for cmd in spawn agents loop msg integrate; do
      assert_contains "$h_stdout" "$cmd" "help contains $cmd"
    done
  '

  TEST "grove spawn --help shows --task and --branch" '
    run h "grove spawn --help"
    assert_eq 0 "$h_exit" "exit code"
    assert_contains "$h_stdout" "--task" "task flag"
    assert_contains "$h_stdout" "--branch" "branch flag"
  '

  TEST "grove init --help advertises --no-agent and --reconfigure" '
    run h "grove init --help"
    assert_eq 0 "$h_exit" "exit code"
    assert_contains "$h_stdout" "no-agent" "no-agent flag"
    assert_contains "$h_stdout" "no-devcontainer" "no-devcontainer flag"
    assert_contains "$h_stdout" "reconfigure" "reconfigure flag"
  '

  for cmd in "agents list" "loop" "msg broadcast hi" "integrate"; do
    label=$(echo "$cmd" | tr " " "-")
    TEST "grove $cmd outside a grove repo errors cleanly" "
      d=/tmp/grove-localtest-$label
      rm -rf \$d && mkdir -p \$d
      run h \"cd \$d && grove $cmd\"
      assert_ne 0 \"\$h_exit\" 'non-zero exit'
      assert_contains \"\$h_stderr\" 'Error' 'stderr contains Error'
      rm -rf \$d
    "
  done

  TEST "grove init with no args adopts a fresh git repo in cwd" '
    d=/tmp/grove-localtest-init-cwd
    rm -rf $d && mkdir -p $d
    (cd $d && git init -q && git -c user.email=t@t -c user.name=t commit --allow-empty -q -m init)
    run h "cd $d && grove init --no-agent"
    assert_eq 0 "$h_exit" "init exit"
    assert_contains "$h_stdout" "Adopting" "adopting line"
    assert_contains "$h_stdout" "in-place" "layout marker"
    run ls "ls $d/.grove"
    assert_contains "$ls_stdout" "config.toml" "config.toml present"
    assert_contains "$ls_stdout" "PROMPT.template.md" "PROMPT.template present"
    rm -rf $d
  '

  TEST "grove init <path> adopts the supplied directory" '
    d=/tmp/grove-localtest-init-path
    rm -rf $d && mkdir -p $d
    (cd $d && git init -q && git -c user.email=t@t -c user.name=t commit --allow-empty -q -m init)
    run h "grove init $d --no-agent"
    assert_eq 0 "$h_exit" "init exit"
    assert_contains "$h_stdout" "Adopting" "adopting line"
    [[ -f $d/.grove/config.toml ]] || { echo "  FAIL: config.toml not written"; exit 1; }
    rm -rf $d
  '

  TEST "grove init refuses path that isnt a git repo" '
    d=/tmp/grove-localtest-init-nongit
    rm -rf $d && mkdir -p $d
    run h "cd $d && grove init --no-agent"
    assert_ne 0 "$h_exit" "exit non-zero"
    assert_contains "$h_stderr" "not a git repository" "error message"
    rm -rf $d
  '
}

run_grove_tests() {
  echo
  echo "== grove.hone =="

  TEST "grove --help" '
    run h "grove --help"
    assert_eq 0 "$h_exit" "exit"
    assert_contains "$h_stdout" "grove" "help text"
    assert_contains "$h_stdout" "Git worktree management tool" "tagline preserved"
    assert_contains "$h_stdout" "Commands:" "commands header"
  '

  TEST "grove --version" '
    run h "grove --version"
    assert_eq 0 "$h_exit" "exit"
    [[ "$h_stdout" =~ [0-9]+\.[0-9]+\.[0-9]+ ]] || { echo "  FAIL: version format"; exit 1; }
  '

  TEST "grove with invalid command shows error" '
    run h "grove invalid-command"
    assert_eq 1 "$h_exit" "exit 1"
    assert_contains "$h_stderr" "Invalid command" "error message"
  '

  TEST "grove list outside grove shows error" '
    d=/tmp/grove-localtest-list-nonrepo
    rm -rf $d && mkdir -p $d
    run h "cd $d && grove list"
    assert_ne 0 "$h_exit" "exit non-zero"
    assert_contains "$h_stderr" "Not in a grove repository" "upstream wording"
    rm -rf $d
  '

  TEST "grove init invalid URL shows error (exit 2)" '
    d=/tmp/grove-localtest-bad-url
    rm -rf $d && mkdir -p $d
    run h "cd $d && grove init https://garbage with spaces"
    assert_eq 2 "$h_exit" "exit 2"
    assert_contains "$h_stderr" "Invalid git URL format" "error message"
    rm -rf $d
  '

  TEST "grove add without branch name shows error" '
    run h "grove add"
    assert_ne 0 "$h_exit" "exit non-zero"
  '

  TEST "grove remove without branch name shows error" '
    run h "grove remove"
    assert_ne 0 "$h_exit" "exit non-zero"
  '
}

# ----------------------------------------------------------------------------

filter="${1:-all}"
case "$filter" in
  agentic) run_agentic_tests ;;
  grove)   run_grove_tests   ;;
  all|"")
    run_agentic_tests
    run_grove_tests
    ;;
  *) echo "unknown filter: $filter (try: agentic, grove, all)"; exit 2 ;;
esac

echo
echo "----------------------------------------"
echo "passed: $PASS    failed: $FAIL"
if [[ $FAIL -gt 0 ]]; then
  for n in "${FAILED_NAMES[@]}"; do
    echo "  - $n"
  done
  exit 1
fi
