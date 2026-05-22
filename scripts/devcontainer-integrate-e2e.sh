#!/usr/bin/env bash
# End-to-end test for `grove integrate` in the DEVCONTAINER backend (the
# default, bind-mounted model). Mirror of scripts/sandbox-integrate-e2e.sh,
# asserting the OPPOSITE of the isolation property: because the worktree is
# bind-mounted (not copied), the agent's worktree + branches ARE visible on the
# host. This is the meaningful behavioural difference between the two backends.
#
# Proves, with the real `devcontainer` CLI + docker:
#   1. `grove spawn` brings the devcontainer up and runs each agent (a scripted
#      stand-in for claude) which commits on its `agent/*` branch — visible on
#      the HOST through the bind mount.
#   2. `grove integrate` stages the read-only context under the agent dir
#      `.grove/agents/integrate-<ts>/context/` (same as sandbox); the
#      integration worktree + branch are created on the HOST.
#   3. A scripted merge driver (in the container) merges both branches and
#      `git push origin` the integration branch — the egress.
#   4. `grove integrate --abort` removes ONLY integration artifacts, leaving the
#      feat agents and the container intact.
#
# Backend-specific wrinkles vs. the sandbox e2e: host paths != container paths,
# so the agent command and `origin.url` use the CONTAINER path
# (/workspaces/proj/...); the local `origin` lives under the (fully bind-mounted)
# project root so the in-container push reaches it without a network.
#
# Requirements: docker, git, the `@devcontainers/cli` (`devcontainer`), and a
# debug/release `grove`. Run from the repo root after `cargo build`:
#
#     ./scripts/devcontainer-integrate-e2e.sh
#     sg docker -c './scripts/devcontainer-integrate-e2e.sh'   # if docker group not active
#
# Manual/CI harness (needs the docker daemon + devcontainer CLI), not a `cargo test`.
set -uo pipefail   # not -e: assertions are explicit (grep -q + pipefail = SIGPIPE noise)

GROVE_BIN="${GROVE_BIN:-$(pwd)/target/debug/grove}"
IMG="grove-sandbox-e2e:latest"
WORK="$(mktemp -d /tmp/grove-dc-integ.XXXXXX)"
ROOT="$WORK/proj"
BARE="$ROOT/demo.git"
REMOTE_HOST="$ROOT/.grove/remote.git"   # host path to the bare origin
WS="/workspaces/proj"                   # container-side workspace mount
REMOTE_CT="$WS/.grove/remote.git"       # container path to origin (= origin.url)

note() { printf '\n=== %s ===\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1" >&2; teardown; exit 1; }
has()  { printf '%s\n' "$1" | grep -Fq "$2"; }

CN=""
teardown() {
    [ -n "${CN:-}" ] && docker rm -f "$CN" >/dev/null 2>&1
    [ -n "${ROOT:-}" ] && docker rm -f $(docker ps -aq --filter "label=devcontainer.local_folder=$ROOT" 2>/dev/null) >/dev/null 2>&1
    rm -rf "$WORK" 2>/dev/null
    return 0
}

wait_for() {
    local tries="$1"; shift
    local i=0
    while [ "$i" -lt "$tries" ]; do
        if "$@" >/dev/null 2>&1; then return 0; fi
        i=$((i + 1)); sleep 1
    done
    return 1
}

[ -x "$GROVE_BIN" ] || { echo "FAIL: grove not at $GROVE_BIN (run cargo build)"; exit 1; }
command -v docker >/dev/null       || { echo "FAIL: docker not on PATH"; exit 1; }
command -v devcontainer >/dev/null || { echo "FAIL: devcontainer CLI not on PATH (npm i -g @devcontainers/cli)"; exit 1; }

note "build the devcontainer image (git + tmux)"
docker build -t "$IMG" - >/dev/null <<'DOCKERFILE'
FROM alpine:3.20
RUN apk add --no-cache git tmux bash coreutils
DOCKERFILE

note "clean any prior devcontainers from this image"
docker rm -f $(docker ps -aq --filter "label=devcontainer.local_folder" --filter "ancestor=$IMG" 2>/dev/null) >/dev/null 2>&1 || true

note "build a bare-layout grove project (devcontainer backend), origin under .grove"
mkdir -p "$ROOT/.grove/agents" "$ROOT/.grove/logs" "$ROOT/.grove/bus" "$ROOT/.devcontainer"
git init -q --bare "$REMOTE_HOST"
git -C "$REMOTE_HOST" symbolic-ref HEAD refs/heads/main
SEED="$WORK/seed"; git init -q -b main "$SEED"
( cd "$SEED"
  git config user.email t@t; git config user.name t
  echo hello > README.md; git add .; git commit -qm init
  echo more >> README.md; git commit -qam second
  git remote add origin "$REMOTE_HOST"; git push -q origin main )

git clone -q --bare "$REMOTE_HOST" "$BARE"
( cd "$BARE"
  # origin.url is the CONTAINER path: the in-container merge driver pushes here;
  # the host never contacts origin during integrate.
  git config remote.origin.url "$REMOTE_CT"
  git config remote.origin.fetch '+refs/heads/*:refs/remotes/origin/*'
  # identity lives in the (bind-mounted) shared repo config so in-container
  # commits + merge commits have an author.
  git config user.email t@t; git config user.name t )

cat > "$ROOT/.grove/config.toml" <<TOML
[container]
backend = "devcontainer"
[devcontainer]
workspace_target = "$WS"
remote_user = "root"
[mounts]
claude_inherit = "none"
TOML

cat > "$ROOT/.devcontainer/devcontainer.json" <<JSON
{
  "image": "$IMG",
  "workspaceFolder": "$WS",
  "remoteUser": "root",
  "overrideCommand": true
}
JSON

# Drivers live under .grove (bind-mounted). Invoked via the CONTAINER path.
# safe.directory: the bind-mounted repo is owned by the host uid but the
# container runs as root → relax git's ownership check for the agent's git ops.
cat > "$ROOT/.grove/spawn-driver.sh" <<'SH'
#!/bin/sh
set -e
git config --global --add safe.directory '*' 2>/dev/null || true
echo "work by ${GROVE_AGENT_NAME}" > "file-${GROVE_AGENT_NAME}.txt"
git add -A
git commit -m "feat(${GROVE_AGENT_NAME}): work"
sleep 600
SH
cat > "$ROOT/.grove/integrate-driver.sh" <<'SH'
#!/bin/sh
set -e
git config --global --add safe.directory '*' 2>/dev/null || true
for b in $(git for-each-ref --format='%(refname:short)' 'refs/heads/agent/*'); do
  git merge --no-ff --no-edit "$b"
done
git push origin "$(git rev-parse --abbrev-ref HEAD)"
touch "${GROVE_AGENT_DIR}/INTEGRATE_DONE"
sleep 600
SH

ROOT="$(cd "$ROOT" && pwd)"; BARE="$ROOT/demo.git"; REMOTE_HOST="$ROOT/.grove/remote.git"
export GROVE_REPO="$BARE"

note "grove spawn feat-a + feat-b (brings devcontainer up; each commits in-container)"
export GROVE_AGENT_COMMAND="sh $WS/.grove/spawn-driver.sh"
"$GROVE_BIN" spawn feat-a --no-bootstrap 2>&1 | sed 's/^/  /'
"$GROVE_BIN" spawn feat-b --no-bootstrap 2>&1 | sed 's/^/  /'
CN="$(docker ps --filter "label=devcontainer.local_folder=$ROOT" --format '{{.Names}}' | head -1)"
[ -n "$CN" ] || fail "no devcontainer running for $ROOT"
echo "  devcontainer: $CN"

note "wait for both agent commits (host-visible through the bind mount)"
wait_for 45 sh -c "git -C '$BARE' log agent/feat-a --oneline 2>/dev/null | grep -q 'feat(feat-a)'" \
  || fail "feat-a never committed"
wait_for 45 sh -c "git -C '$BARE' log agent/feat-b --oneline 2>/dev/null | grep -q 'feat(feat-b)'" \
  || fail "feat-b never committed"
echo "  agent/feat-a and agent/feat-b each have a commit (visible on host)"

note "NON-ISOLATION: worktree + branches ARE on the host (bind mount)"
[ -d "$ROOT/feat-a" ] || fail "feat-a worktree missing on host"
HBR="$(git -C "$BARE" branch --list 'agent/*')"
has "$HBR" "agent/feat-a" && has "$HBR" "agent/feat-b" || fail "agent branches not on host bare clone"
echo "  host has the feat-a worktree and agent/feat-* branches (bind-mount model)"

note "grove integrate (scripted merge driver)"
export GROVE_AGENT_COMMAND="sh $WS/.grove/integrate-driver.sh"
INTEG_OUT="$("$GROVE_BIN" integrate 2>&1)"; printf '%s\n' "$INTEG_OUT" | sed 's/^/  /'
has "$INTEG_OUT" "staged read-only context" || fail "integrate did not stage context"

note "CONTEXT: staged under the agent dir (not the worktree)"
ADIR="$(ls -d "$ROOT"/.grove/agents/integrate-* 2>/dev/null | head -1)"
[ -n "$ADIR" ] || fail "no integrate-* agent dir"
[ -f "$ADIR/context/branches.json" ] || fail "branches.json not staged in agent dir"
[ -f "$ADIR/context/overlap.txt" ]   || fail "overlap.txt not staged in agent dir"
[ ! -e "$ROOT/worktrees/.integration/.grove-context" ] || fail ".grove-context LEAKED into worktree path"
echo "  context at ${ADIR#$ROOT/}/context; worktree has no .grove-context"

note "HOST: integration worktree + branch created on the host"
[ -d "$ROOT/worktrees/.integration" ] || fail "integration worktree missing on host"
HIB="$(git -C "$BARE" branch --list 'integration/*')"
has "$HIB" "integration/" || fail "integration branch missing on host bare clone"
echo "  host: worktree + $(printf '%s' "$HIB" | tr -d ' *')"

note "EGRESS: wait for the merge driver to push the integration branch to origin"
wait_for 90 sh -c "[ -f \"$ADIR/INTEGRATE_DONE\" ]" || fail "integrate driver never finished (no INTEGRATE_DONE)"
RB="$(git -C "$REMOTE_HOST" branch --list 'integration/*')"
has "$RB" "integration/" || fail "integration branch not pushed to origin"
INTEG_REF="$(git -C "$REMOTE_HOST" branch --list 'integration/*' | tr -d ' *' | head -1)"
TREE="$(git -C "$REMOTE_HOST" ls-tree -r --name-only "$INTEG_REF")"
has "$TREE" "file-feat-a.txt" || fail "merged tree missing feat-a's file"
has "$TREE" "file-feat-b.txt" || fail "merged tree missing feat-b's file"
echo "  origin has $INTEG_REF with both agents' files merged"

note "read command routes to the devcontainer: grove agents list shows integrate-*"
AGENTS="$("$GROVE_BIN" agents list 2>&1)"; printf '%s\n' "$AGENTS" | sed 's/^/  /'
has "$AGENTS" "integrate-" || fail "grove agents list did not show the integrate agent"

note "grove integrate --abort removes ONLY integration artifacts"
"$GROVE_BIN" integrate --abort 2>&1 | sed 's/^/  /'
[ ! -e "$ROOT/worktrees/.integration" ] || fail "integration worktree survived abort"
HIB2="$(git -C "$BARE" branch --list 'integration/*')"
[ -z "$(printf '%s' "$HIB2" | tr -d ' *')" ] || fail "integration branch survived abort"
[ -z "$(ls -d "$ROOT"/.grove/agents/integrate-* 2>/dev/null)" ] || fail "integrate-* agent dir survived abort"
[ -d "$ROOT/feat-a" ] || fail "abort destroyed the feat-a worktree"
HAB="$(git -C "$BARE" branch --list 'agent/*')"
has "$HAB" "agent/feat-a" && has "$HAB" "agent/feat-b" || fail "abort destroyed agent branches"
docker exec "$CN" true || fail "abort tore down the container"
echo "  integration artifacts gone; feat-a/feat-b agents + container intact"

note "teardown"
teardown
printf '\nDEVCONTAINER INTEGRATE E2E PASSED\n'
