#!/usr/bin/env bash
# End-to-end test for `grove integrate` in the sandbox backend (copy-in
# isolation). Companion to scripts/sandbox-e2e.sh (which covers spawn).
#
# Proves the in-container integrate path with real docker:
#   1. Two `grove spawn` agents each make a real commit on their `agent/*`
#      branch INSIDE the sandbox (a scripted stand-in for claude).
#   2. `grove integrate` stages the read-only context tree under the agent dir
#      `.grove/agents/integrate-<ts>/context/` (NOT the worktree) — visible on
#      the host (bind mount) AND in-container at the identical path; creates the
#      integration worktree + branch IN-CONTAINER; launches the integrate agent.
#   3. A scripted merge driver (stand-in for claude) merges both agent branches
#      and `git push origin` the integration branch — the sole egress.
#   4. ISOLATION: the integration worktree and the integration/agent branches
#      never appear on the host; only the pushed integration branch leaves.
#   5. `grove integrate --abort` removes ONLY integration artifacts (worktree,
#      integration branch, integrate-* agent dir) and leaves the feat agents and
#      the container intact.
#
# The local `origin` lives UNDER `.grove/` (the one bind-mounted dir) so the
# in-container `git push` reaches it without a network — a test convenience that
# still exercises the real `git push origin` code path from inside the sandbox.
#
# Requirements: docker, git, a debug/release `grove`. Run from the repo root
# after `cargo build`:
#
#     ./scripts/sandbox-integrate-e2e.sh
#     sg docker -c './scripts/sandbox-integrate-e2e.sh'   # if docker group not active
#
# Manual/CI harness (needs the docker daemon), not a `cargo test`.
set -uo pipefail   # not -e: assertions are explicit (grep -q + pipefail = SIGPIPE noise)

GROVE_BIN="${GROVE_BIN:-$(pwd)/target/debug/grove}"
IMG="grove-sandbox-e2e:latest"
WORK="$(mktemp -d /tmp/grove-sb-integ.XXXXXX)"
ROOT="$WORK/proj"
BARE="$ROOT/demo.git"
# origin under .grove so the in-container push can reach it via the bind mount.
REMOTE="$ROOT/.grove/remote.git"

note() { printf '\n=== %s ===\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1" >&2; exit 1; }
has()  { printf '%s\n' "$1" | grep -Fq "$2"; }

# Poll `cmd` until it exits 0 or `tries` (1s apart) elapse.
wait_for() {
    local tries="$1"; shift
    local i=0
    while [ "$i" -lt "$tries" ]; do
        if "$@" >/dev/null 2>&1; then return 0; fi
        i=$((i + 1)); sleep 1
    done
    return 1
}

[ -x "$GROVE_BIN" ] || fail "grove binary not found at $GROVE_BIN (run: cargo build)"
command -v docker >/dev/null || fail "docker not on PATH"

note "build the sandbox image (git + tmux)"
docker build -t "$IMG" - >/dev/null <<'DOCKERFILE'
FROM alpine:3.20
RUN apk add --no-cache git tmux bash coreutils
DOCKERFILE

note "clean any prior sandbox containers"
for c in $(docker ps -aq --filter "name=grove-sb-" 2>/dev/null); do docker rm -f "$c" >/dev/null 2>&1 || true; done

note "build a bare-layout grove project (sandbox backend) with origin under .grove"
mkdir -p "$ROOT/.grove/agents" "$ROOT/.grove/logs" "$ROOT/.grove/bus"
git init -q --bare "$REMOTE"
git -C "$REMOTE" symbolic-ref HEAD refs/heads/main
SEED="$WORK/seed"; git init -q -b main "$SEED"
( cd "$SEED"
  git config user.email t@t; git config user.name t
  echo hello > README.md; git add .; git commit -qm init
  echo more >> README.md; git commit -qam second
  git remote add origin "$REMOTE"; git push -q origin main )

git clone -q --bare "$REMOTE" "$BARE"
( cd "$BARE"; git config remote.origin.url "$REMOTE"
  git config remote.origin.fetch '+refs/heads/*:refs/remotes/origin/*' )

cat > "$ROOT/.grove/config.toml" <<TOML
[container]
backend = "sandbox"
[sandbox]
user = "agent"
[mounts]
claude_inherit = "none"
TOML

# Driver scripts live under .grove (bind-mounted → identical path in-container).
cat > "$ROOT/.grove/spawn-driver.sh" <<'SH'
#!/bin/sh
# stand-in for claude: commit one file on this agent's branch, then idle.
set -e
echo "work by ${GROVE_AGENT_NAME}" > "file-${GROVE_AGENT_NAME}.txt"
git add -A
git commit -m "feat(${GROVE_AGENT_NAME}): work"
sleep 600
SH
cat > "$ROOT/.grove/integrate-driver.sh" <<'SH'
#!/bin/sh
# stand-in for the integrate agent: merge every agent/* branch, push, mark done.
set -e
for b in $(git for-each-ref --format='%(refname:short)' 'refs/heads/agent/*'); do
  git merge --no-ff --no-edit "$b"
done
git push origin "$(git rev-parse --abbrev-ref HEAD)"
touch "${GROVE_AGENT_DIR}/INTEGRATE_DONE"
sleep 600
SH

ROOT="$(cd "$ROOT" && pwd)"; BARE="$ROOT/demo.git"; REMOTE="$ROOT/.grove/remote.git"
export GROVE_REPO="$BARE" GROVE_SANDBOX_IMAGE="$IMG"

note "grove spawn feat-a + feat-b (each commits in-container)"
export GROVE_AGENT_COMMAND="sh $ROOT/.grove/spawn-driver.sh"
"$GROVE_BIN" spawn feat-a --no-bootstrap 2>&1 | sed 's/^/  /'
"$GROVE_BIN" spawn feat-b --no-bootstrap 2>&1 | sed 's/^/  /'
CN="$(docker ps --filter "name=grove-sb-" --format '{{.Names}}' | head -1)"
[ -n "$CN" ] || fail "no sandbox container running"
echo "  sandbox container: $CN"

note "wait for both agent commits to land in-container"
wait_for 30 docker exec "$CN" sh -c "git -C '$BARE' log agent/feat-a --oneline | grep -q 'feat(feat-a)'" \
  || fail "feat-a never committed in-container"
wait_for 30 docker exec "$CN" sh -c "git -C '$BARE' log agent/feat-b --oneline | grep -q 'feat(feat-b)'" \
  || fail "feat-b never committed in-container"
echo "  agent/feat-a and agent/feat-b each have a commit"

note "grove integrate (scripted merge driver)"
export GROVE_AGENT_COMMAND="sh $ROOT/.grove/integrate-driver.sh"
INTEG_OUT="$("$GROVE_BIN" integrate 2>&1)"; printf '%s\n' "$INTEG_OUT" | sed 's/^/  /'
has "$INTEG_OUT" "staged read-only context" || fail "integrate did not stage context"

note "CONTEXT: staged under the agent dir on the host (bind mount), not the worktree"
ADIR="$(ls -d "$ROOT"/.grove/agents/integrate-* 2>/dev/null | head -1)"
[ -n "$ADIR" ] || fail "no integrate-* agent dir on host"
[ -f "$ADIR/context/branches.json" ] || fail "branches.json not staged in agent dir"
[ -f "$ADIR/context/overlap.txt" ]   || fail "overlap.txt not staged in agent dir"
[ ! -e "$ROOT/worktrees/.integration/.grove-context" ] || fail ".grove-context LEAKED into worktree path"
echo "  context at ${ADIR#$ROOT/}/context (branches.json + overlap.txt); worktree has no .grove-context"

note "CONTEXT: identical path resolves in-container"
docker exec "$CN" test -f "$ADIR/context/branches.json" || fail "context not visible in-container at agent path"
echo "  in-container sees $ADIR/context/branches.json"

note "ISOLATION: integration worktree + branch exist IN-CONTAINER"
docker exec "$CN" test -d "$ROOT/worktrees/.integration" || fail "integration worktree missing in-container"
CIB="$(docker exec "$CN" git -C "$BARE" branch --list 'integration/*')"
has "$CIB" "integration/" || fail "integration branch missing in-container"
echo "  in-container: worktree + $(printf '%s' "$CIB" | tr -d ' *')"

note "ISOLATION: nothing leaked to the host"
[ ! -e "$ROOT/worktrees/.integration" ] || fail "integration worktree LEAKED to host"
HB="$(git -C "$BARE" branch --list 'integration/*' 'agent/*')"
[ -z "$HB" ] && echo "  host bare clone has no integration/* or agent/* branches (correct)" \
  || fail "branches LEAKED into host bare clone: $HB"

note "EGRESS: wait for the merge driver to push the integration branch to origin"
wait_for 60 sh -c "[ -f \"$ADIR/INTEGRATE_DONE\" ]" || fail "integrate driver never finished (no INTEGRATE_DONE)"
RB="$(git -C "$REMOTE" branch --list 'integration/*')"
has "$RB" "integration/" || fail "integration branch not pushed to origin"
# the integration branch on origin must contain BOTH agents' files
INTEG_REF="$(git -C "$REMOTE" branch --list 'integration/*' | tr -d ' *' | head -1)"
TREE="$(git -C "$REMOTE" ls-tree -r --name-only "$INTEG_REF")"
has "$TREE" "file-feat-a.txt" || fail "merged tree missing feat-a's file"
has "$TREE" "file-feat-b.txt" || fail "merged tree missing feat-b's file"
echo "  origin has $INTEG_REF with both agents' files merged"

note "read command routes to the sandbox: grove agents list shows integrate-*"
AGENTS="$("$GROVE_BIN" agents list 2>&1)"; printf '%s\n' "$AGENTS" | sed 's/^/  /'
has "$AGENTS" "integrate-" || fail "grove agents list did not show the integrate agent"

note "grove integrate --abort removes ONLY integration artifacts"
"$GROVE_BIN" integrate --abort 2>&1 | sed 's/^/  /'
docker exec "$CN" test ! -d "$ROOT/worktrees/.integration" || fail "integration worktree survived abort (in-container)"
CIB2="$(docker exec "$CN" git -C "$BARE" branch --list 'integration/*')"
[ -z "$(printf '%s' "$CIB2" | tr -d ' *')" ] || fail "integration branch survived abort (in-container)"
[ -z "$(ls -d "$ROOT"/.grove/agents/integrate-* 2>/dev/null)" ] || fail "integrate-* agent dir survived abort"
# feat agents + container must be untouched
docker exec "$CN" test -d "$ROOT/feat-a" || fail "abort destroyed the feat-a worktree"
CAB="$(docker exec "$CN" git -C "$BARE" branch --list 'agent/*')"
has "$CAB" "agent/feat-a" && has "$CAB" "agent/feat-b" || fail "abort destroyed agent branches"
docker exec "$CN" true || fail "abort tore down the container"
echo "  integration artifacts gone; feat-a/feat-b agents + container intact"

note "teardown"
docker rm -f "$CN" >/dev/null; rm -rf "$WORK"
printf '\nINTEGRATE E2E PASSED\n'
