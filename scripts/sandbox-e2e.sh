#!/usr/bin/env bash
# End-to-end test for grove's sandbox backend (copy-in isolation).
#
# Proves the headline isolation property with real docker:
#   1. `grove spawn` creates the worktree + agent branch INSIDE the per-project
#      sandbox container; neither appears on the host (copy-in, not bind-mount).
#   2. `.grove/` is observable on the host (the bind-mounted control plane):
#      seeded agent state lands on host disk and the worktree's `.grove`
#      symlink resolves to it inside the container.
#   3. A second spawn reuses the existing sandbox (no re-seed; in-flight work
#      preserved).
#   4. `docker rm -f` tears it down cleanly.
#
# Requirements: docker, git, a release/debug `grove` binary. Run from the repo
# root after `cargo build`:
#
#     ./scripts/sandbox-e2e.sh
#
# If your user needs the docker group activated for the session:
#
#     sg docker -c './scripts/sandbox-e2e.sh'
#
# This is a manual/CI harness (it needs the docker daemon), not a `cargo test`.
set -uo pipefail   # not -e: assertions are explicit (grep -q + pipefail = SIGPIPE noise)

GROVE_BIN="${GROVE_BIN:-$(pwd)/target/debug/grove}"
IMG="grove-sandbox-e2e:latest"
WORK="$(mktemp -d /tmp/grove-sb-e2e.XXXXXX)"
ROOT="$WORK/proj"
REMOTE="$WORK/remote.git"
BARE="$ROOT/demo.git"

note() { printf '\n=== %s ===\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1" >&2; exit 1; }
has()  { printf '%s\n' "$1" | grep -Fq "$2"; }

[ -x "$GROVE_BIN" ] || fail "grove binary not found at $GROVE_BIN (run: cargo build)"
command -v docker >/dev/null || fail "docker not on PATH"

note "build a lightweight sandbox image (git + tmux)"
docker build -t "$IMG" - >/dev/null <<'DOCKERFILE'
FROM alpine:3.20
RUN apk add --no-cache git tmux bash coreutils
DOCKERFILE

note "clean any prior sandbox containers"
for c in $(docker ps -aq --filter "name=grove-sb-" 2>/dev/null); do docker rm -f "$c" >/dev/null 2>&1 || true; done

note "create an upstream remote + commits"
git init -q --bare "$REMOTE"
SEED="$WORK/seed"; git init -q -b main "$SEED"
( cd "$SEED"
  git config user.email t@t; git config user.name t
  echo hello > README.md; git add .; git commit -qm init
  echo more >> README.md; git commit -qam second
  git remote add origin "$REMOTE"; git push -q origin main )
git -C "$REMOTE" symbolic-ref HEAD refs/heads/main

note "build a bare-layout grove project (mimics 'grove init <url>')"
mkdir -p "$ROOT"
git clone -q --bare "$REMOTE" "$BARE"
( cd "$BARE"; git config remote.origin.url "$REMOTE"
  git config remote.origin.fetch '+refs/heads/*:refs/remotes/origin/*' )
mkdir -p "$ROOT/.grove/agents" "$ROOT/.grove/logs" "$ROOT/.grove/bus"
cat > "$ROOT/.grove/config.toml" <<TOML
[container]
backend = "sandbox"
[sandbox]
user = "agent"
[mounts]
claude_inherit = "none"
TOML

ROOT="$(cd "$ROOT" && pwd)"; BARE="$ROOT/demo.git"
export GROVE_REPO="$BARE" GROVE_SANDBOX_IMAGE="$IMG" GROVE_AGENT_COMMAND="sleep 600"

note "grove spawn feat-a"
"$GROVE_BIN" spawn feat-a --no-bootstrap 2>&1 | sed 's/^/  /'
CN="$(docker ps --filter "name=grove-sb-" --format '{{.Names}}' | head -1)"
[ -n "$CN" ] || fail "no sandbox container running"
echo "  sandbox container: $CN"

note "ISOLATION: branch + worktree exist INSIDE the container"
CBR="$(docker exec "$CN" git -C "$BARE" branch)"; printf '%s\n' "$CBR" | sed 's/^/  [container] /'
has "$CBR" "agent/feat-a" || fail "agent/feat-a not in container bare clone"
docker exec "$CN" test -d "$ROOT/feat-a" || fail "worktree dir missing in container"

note "ISOLATION: nothing leaked to the host"
[ ! -e "$ROOT/feat-a" ] || fail "worktree LEAKED to host"
has "$(git -C "$BARE" branch)" "agent/feat-a" && fail "branch LEAKED into host bare clone"
echo "  host has no worktree and no agent/feat-a branch (correct)"

note "CONTROL PLANE: .grove/ observable on the host (bind mount)"
[ -f "$ROOT/.grove/agents/feat-a/STATE.md" ] || fail "agent state not on host"
docker exec "$CN" sh -c "test -L '$ROOT/feat-a/.grove'" || fail "worktree .grove symlink missing"
echo "  host sees seeded agent state; container worktree .grove symlink resolves"

note "tmux session live in the container"
docker exec -u "$(id -u):$(id -g)" "$CN" tmux has-session -t grove-feat-a || fail "tmux session missing"
echo "  grove-feat-a alive"

note "REUSE: second spawn reuses the SAME container (no re-seed)"
CID1="$(docker inspect -f '{{.Id}}' "$CN")"
"$GROVE_BIN" spawn feat-b --no-bootstrap >/dev/null 2>&1
CID2="$(docker inspect -f '{{.Id}}' "$(docker ps --filter "name=grove-sb-" --format '{{.Names}}' | head -1)")"
[ "$CID1" = "$CID2" ] || fail "sandbox was recreated (expected reuse)"
docker exec "$CN" test -d "$ROOT/feat-a" || fail "feat-a worktree lost on reuse"
CBR2="$(docker exec "$CN" git -C "$BARE" branch)"
has "$CBR2" "agent/feat-a" && has "$CBR2" "agent/feat-b" || fail "branches not both present after reuse"
echo "  reused $CID2 — feat-a preserved, feat-b added"

note "teardown"
docker rm -f "$CN" >/dev/null; rm -rf "$WORK"
printf '\nE2E PASSED\n'
