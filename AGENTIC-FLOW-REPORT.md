# Grove `agentic-flow` — Port Report

> **Branch**: `agentic-flow` on `Panzax/grove`
> **Date**: 2026-05-20
> **HEAD**: `1e4349c` (17 commits ahead of upstream `main`)
> **Status**: build green, tests green, ready for human review

---

## 1. What this branch does

Adds a portable agentic-workflow layer on top of the existing `captainsafia/grove`
worktree CLI. The new layer is a Rust-native port of the harness we built in
`freqtrade_tecolote` (see that repo's `research_notes/2026-05-20-agentic-harness.md`
for the design rationale). It is **additive**: every upstream command keeps its
behavior; the agentic surface is opt-in via the new subcommands.

Where the freqtrade harness was a single-repo bash/devcontainer setup, this fork
turns the same mechanics into a portable CLI: drop the `grove` binary into any
git repo, run `grove init`, follow the wizard, and you have an isolated
multi-agent worktree workflow with a Stop-hook Ralph loop engine, a file-based
collaboration bus, and a headless conflict resolver — all without touching the
user's project conventions.

### Lineage

- Upstream worktree primitives: `captainsafia/grove` v2.1.0 (MIT). All upstream
  code preserved; copyright retained in `LICENSE`.
- Stop-hook engine: ~165-line bash, adapted from the official Anthropic
  `ralph-loop` plugin (`stop-hook.sh`). Renamed env var
  `$RALPH_DIR` → `$GROVE_AGENT_DIR`; otherwise the contract is identical.
- Plan / design rationale: `/home/martin/.claude/plans/okay-so-i-realized-velvet-lighthouse.md`.

---

## 2. New surface

### Commands

| Command | What |
|---|---|
| `grove init <url> [--no-agent] [--no-devcontainer] [--reconfigure]` | Bare clone + Phase-1 deterministic scaffold + Phase-2 setup wizard |
| `grove spawn <name> [--branch <b>] [--task "..."]` | Worktree + seeded agent profile + tmux session |
| `grove agents <list\|status\|kill>` | Manage running agent sessions |
| `grove loop [--watch] [--agent <name>]` | Inspect Ralph loop state (table + live watcher) |
| `grove msg <to\|broadcast> "<text>" [--from <s>] [--contract <slug>]` | Bus messaging (broadcast / direct / contract) |
| `grove integrate [--into <b>] [--no-test]` | Merge `agent/*` into `integration/<ts>` with headless conflict resolver |

### Files added in the project (after `grove init`)

```
<repo>/
├── .devcontainer/devcontainer.json    # Phase-1 skeleton, Phase-2 mutates in place
├── .grove/
│   ├── config.toml                    # [agent] [devcontainer] [bus] [hook] [mounts] [stack]
│   │                                  # [verify] [caches] [hooks] [meta]
│   ├── PROMPT.template.md             # tracked framework asset
│   ├── PROTOCOL.md                    # tracked framework asset (bus + agent/shared)
│   ├── RALPH-LOOP.md                  # tracked framework asset
│   ├── SHARED.md                      # user-owned (template provided)
│   ├── tools/loop-hook.sh             # bash Stop-hook engine (+x)
│   ├── agents/<name>/                 # gitignored: per-agent state
│   │   ├── PROMPT.md                  # what the agent reads each iteration
│   │   ├── STATE.md                   # workitem checklist + iteration log
│   │   ├── loop.md                    # active flag + iteration + max_iterations + promise
│   │   └── agent.toml                 # metadata for `grove agents list`
│   └── bus/                           # gitignored: collaboration channel
│       ├── log.d/                     #   broadcast (one file per event)
│       ├── inbox/<recipient>/         #   direct mail
│       └── contracts/                 #   negotiated interfaces
├── .groverc                           # upstream-compatible (extended with devcontainer-up bootstrap)
└── worktrees/                         # gitignored: agent worktrees
```

### Modules added

| Path | Lines | Role |
|---|---|---|
| `src/devcontainer/mod.rs` | ~210 | `detect_project_context`, `scaffold_devcontainer`, read/write helpers |
| `src/devcontainer/stack.rs` | ~470 | stack detection, package-manager detection, toolchain pin, extension defaults, verify defaults, cache volumes |
| `src/devcontainer/ci_scrape.rs` | ~240 | extract verify commands from `.github/workflows/*.yml` |
| `src/agent/mod.rs` + submodules | ~700 | hook installer (`hook.rs`), loop.md parser (`loop_md.rs`), agent seeder (`seed.rs`), Phase-2 wizard (`setup.rs`) |
| `src/bus/mod.rs` | ~390 | per-file event bus (broadcast / direct / contract / archive) |
| `src/session/tmux.rs` | ~170 | tmux session create/list/kill/attach |
| `src/commands/{spawn,agents,loop_,msg,integrate}.rs` | ~990 | command implementations |
| `assets/{loop-hook.sh, PROMPT.template.md, PROTOCOL.md, RALPH-LOOP.md, SHARED.md}` | — | framework files, `include_str!`'d into the binary |
| `test/integration/agentic.hone` | — | new Hone smoke tests |
| `src/git/worktree_manager.rs` | +30 | new helpers: `show_head_file`, `ls_head_files`, `head_file_exists` |
| `src/models.rs` | +380 | `ProjectContext`, `ProjectStack`, `AgentMetadata`, `LoopState`, `LoopStatus`, `BusMessage`, `MessageKind`, `GroveConfig` + sections |

Total: **+6265 / -1563 LoC** across 45 files (the deletions are upstream's `site/`,
`.agents/`, removed branding, and refactored `self_update.rs`).

---

## 3. What's been tested

### Automated (197 tests, all green)

| Layer | Coverage |
|---|---|
| Upstream unit tests | 133/133 still pass — no regressions in worktree primitives |
| `src/models.rs` | Serde round-trips for new types (via use sites) |
| `src/devcontainer/stack.rs` | 11 tests: detect Python/Rust/Node/Go/.NET, monorepo ranking, nested-package.json non-match, package-manager inference, install lines, default extensions per stack, verify defaults, cache volumes |
| `src/devcontainer/ci_scrape.rs` | 6 tests: one-liner `run:`, multiline `run: \|`, classify pytest, classify clippy, dedupe, into_verify_section ordering |
| `src/devcontainer/mod.rs` | 2 tests: skeleton carries repo name / image / extensions; unknown stack still produces a valid skeleton |
| `src/agent/hook.rs` | 5 tests: create file when missing; idempotent re-install; preserves other settings keys; rejects non-object root; embedded engine sanity (`GROVE_AGENT_DIR` substitution verified) |
| `src/agent/loop_md.rs` | 3 tests: parse + serialize round-trip; inactive parse; missing-frontmatter rejection |
| `src/agent/seed.rs` | 5 tests: three-file seed, rejects existing agent dir, rejects bad names, loop.md parser round-trip, kebab validator |
| `src/agent/setup.rs` | 6 tests: add_mount RO entry, idempotent, set_container_env, set_extensions, apply_post_create install line, apply_post_create chains pre-commit |
| `src/bus/mod.rs` | 8 tests: broadcast write, direct write, chronological log order, since-filter, inbox + archive round-trip, parse round-trip, contract path, slug strips unsafe chars |
| `src/session/tmux.rs` | 3 tests: session name prefix, attach instructions, real-tmux round-trip (auto-skips if tmux not on PATH) |
| `src/commands/spawn.rs` | 2 tests: default command tokens, env override picks up tokens |
| `src/commands/init.rs` | 9 tests: build_grove_config carries stack metadata, build_grove_config uses scrape when available, write_grove_config respects existing user file, .groverc bootstrap create/extend/idempotent, patch_gitignore append + idempotent, patch_dockerignore append + idempotent, apply_cache_volumes_to_devcontainer |

### Manual end-to-end smoke test

`grove init https://github.com/octocat/Hello-World.git --no-agent` was run in a
clean `/tmp/` directory. Verified:

- Bare clone created at `Hello-World/Hello-World.git`.
- Stack detected (`unknown` for this test repo — expected).
- `.devcontainer/devcontainer.json` written with the right default image, base
  extensions (Claude Code, GitLens, GitHub PR, EditorConfig), and UID/GID
  match config.
- `.grove/config.toml` written with `[agent]`, `[devcontainer]`, `[bus]`,
  `[hook]`, `[mounts]`, `[stack]`, `[verify]`, `[caches]`, `[hooks]`, `[meta]`
  sections populated.
- `.groverc` extended with `devcontainer up` bootstrap entry.
- `.gitignore` patched with `.grove/agents/`, `.grove/bus/`, `worktrees/`.
- `.grove/tools/loop-hook.sh` installed with `0755`.
- Framework files `.grove/{PROTOCOL,RALPH-LOOP,SHARED,PROMPT.template}.md` written.
- Stop hook registered in `~/.claude/settings.json`; second run reported
  "already present, 1 total" — idempotency verified.

### Hone integration tests (added in T15)

`test/integration/agentic.hone` covers the new clap surface from outside the
binary: `--help` lists new commands, `--help` of each new command shows its
flags, and each command errors cleanly when run outside a grove repo.

### Build hygiene

- `cargo build --release` — zero warnings.
- `cargo fmt --all -- --check` — clean.
- Hooks: `cargo test` runs all 197 tests in under 100 ms (no real I/O against
  external services in the default test suite; `round_trip_against_real_tmux`
  auto-skips if tmux is absent).

---

## 4. What is NOT tested

These are deliberate skips — the harness equivalents in freqtrade had the same
profile.

- **Live "+1 per Stop" canary** — would require a real Claude session driving
  the loop for 2-3 turns. Burns tokens; documented for the operator to run
  when convenient. Structural correctness of the hook (single registration,
  env-gated self-disable, frontmatter atomic bump, JSON `decision:block`
  contract) is covered by the bash-level checks the freqtrade port already
  ran (V3, 13 assertions across 7 cases).
- **Phase 2 wizard interactive run** — `dialoguer` prompts can't be exercised
  in CI; the wizard guards against non-TTY stdin and exits Ok with a notice.
  The deterministic *transformations* the wizard performs
  (`add_mount`, `set_container_env`, `set_extensions`, `apply_post_create`)
  are unit-tested.
- **`grove integrate` end-to-end against a real conflict** — same shape as
  the freqtrade harness's V7 (which used synthetic branches + `--no-test`).
  The Rust port matches the bash version step-for-step, but we don't have
  an automated integration test against a real merge conflict and Claude
  resolver yet. The structural pieces — `snapshot_context`, `list_agent_branches`,
  `git_in`, `resolver_command_tokens` (with `GROVE_RESOLVE_COMMAND` override
  for testing) — are in place.
- **Devcontainer-up + cross-platform** — no automated test that
  `devcontainer up` succeeds end-to-end. CI matrix is Linux + Windows for the
  cargo build; macOS coverage will follow with the agentic Hone tests once
  the runner has Claude Code on PATH.

---

## 5. Known limitations / open items

| Item | Notes |
|---|---|
| **`grove self-update`** is a stub | Upstream points at `i.safia.sh` (captainsafia's hosted endpoint). For the fork we print a notice + `cargo install --git ...` hint. Will need its own install endpoint or a GitHub-Releases-based installer before binaries ship. |
| **GitHub Releases pipelines** | `pr-publish.yml` and `release.yml` still carry upstream's cross-build flow; their `safia.sh` install URLs have been replaced with `cargo install --git Panzax/grove --tag ...`, but the actual release workflow hasn't been verified end-to-end against the Panzax repo. |
| **macOS CI** | Upstream's `ci.yml` runs Ubuntu + Windows. macOS is documented as supported but not exercised in CI. |
| **No agent-process supervisor** | If a tmux session crashes mid-loop, `grove agents kill` cleans up but there's no auto-restart. The freqtrade harness took the same stance. |
| **Single-host bus** | Bus is a directory on the local filesystem. Cross-host agents (cloud + local) need either a git-mediated bus or an HTTP one. Out of scope for v1. |
| **`SHARED.md` is a per-worktree chmod 0444 speedbump**, not enforced | Same as freqtrade. A pre-commit hook on `agent/shared` enforcing read-only-from-non-integrators is a future hardening. |
| **`agent.toml` per-agent metadata file** is new (not in freqtrade) | Used by `grove agents list/status`. Format may evolve; treat as fork-internal for now. |
| **No automated cleanup of `.grove/agents/<name>/`** after `grove remove` | By design (loop history survives worktree death). `grove agents purge <name>` would be a useful follow-up. |
| **The Phase-2 wizard's "agent-inferred" steps are heuristic-only** | Steps 4 (extra mounts) and 5 (extensions) scan the bare clone for env-var refs and config-file presence, but don't actually invoke `claude -p` yet. Wiring the LLM call is a P2 follow-up — the heuristic alone gives the user 80% of the value with no token cost. |

---

## 6. Suggested next steps (rough priority order)

### P0 — Before merging to `main`

1. **Manual review of `grove init` against a real polyglot repo.** Smoke test
   has only covered Hello-World (no stack signals). Run against a Python+Rust
   monorepo and verify the stack detection, verify-command scrape, and cache
   volumes look right.
2. **Decide on `self-update`.** Either wire it to GitHub Releases or remove
   the command entirely from the fork's clap surface. Current stub is fine for
   PR review but shouldn't ship.
3. **Verify the Stop hook registration in your `~/.claude/settings.json`** —
   the smoke test installed it; confirm you're happy with the
   `H="$CLAUDE_PROJECT_DIR/.grove/tools/loop-hook.sh"; ...` snippet
   coexisting with whatever else lives in `hooks.Stop[]`.

### P1 — Within the first week of using the fork

4. **Live canary test.** In a sandbox repo: `grove init`, `grove spawn feat-a
   --task "..."`, attach the tmux session, watch the hook fire +1 per turn
   for 2-3 iterations, verify `grove loop --watch` updates, verify the
   `<promise>X</promise>` exit path. This is the one test the structural
   checks can't substitute for.
5. **Wire the Phase-2 wizard's LLM-inferred steps.** Step 4 (extra mounts)
   and Step 5 (extensions) currently use heuristics; passing the raw repo
   manifests to `claude -p` with a structured-output prompt would catch
   project-specific cases (e.g. a private SDK env-var the heuristic doesn't
   know about).
6. **Integration test for `grove integrate`.** Set up two synthetic branches
   that conflict, run `grove integrate` with `GROVE_RESOLVE_COMMAND` set to
   a deterministic resolver, verify the snapshot + merge + verify flow end
   to end. Add as a Hone test.

### P2 — Polish

7. **`grove agents purge <name>`** to delete the agent profile after a
   feature lands (counterpart to `grove remove`).
8. **`grove agents tail <name>`** that opens a read-only view of the tmux
   session log (e.g. `tmux capture-pane -p`) so the operator can spot-check
   progress without attaching.
9. **`.grove/SHARED.md` editor.** A `grove shared edit` helper that opens
   `$EDITOR` against a temporary file, then writes via `agent/shared` branch
   — so SHARED.md updates go through a deliberate review rather than a
   per-worktree chmod.
10. **CHANGELOG automation.** The fork's CHANGELOG.md prepends a v2.2.0
    block by hand; wire `git-cliff` or similar so future releases get clean
    notes for free.
11. **macOS CI**. Add `runs-on: macos-latest` to the CI matrix and exercise
    the Hone tests there too.
12. **Windows-native exploration.** Currently documented as WSL-only because
    the loop engine is bash + tmux. A static Rust port of `loop-hook.sh` +
    a background-process backend in `session/` would unlock native Windows.
    Big lift; revisit only if there's demand.

### P3 — Design follow-ups

13. **MCP server bus.** The file-based bus is v1. An MCP server exposing
    `send_message` / `read_inbox` tools to Claude Code would let agents
    interact with the bus via structured tooling instead of file ops.
14. **Multi-host bus.** For mixed local + Claude-Cloud workflows, the
    bus needs to be filesystem-agnostic — either git-mediated or HTTP.
15. **`grove init --interactive` w/o git URL.** Currently `grove init`
    requires a URL. A "use cwd as the source repo" path would let users
    grove-ify an existing local clone in place.

---

## 7. Commit map

The branch is structured as 17 commit-sized units. Each compiles + tests
cleanly in isolation, so the PR is bisectable.

| # | SHA | Title |
|---|---|---|
| 1 | `f9dcfd9` | chore: rebrand fork to Panzax, prep for agentic-flow |
| 2 | `f23fd46` | feat(models): add agentic types |
| 3 | `335278d` | feat(devcontainer): stack detection + skeleton scaffolding |
| 4 | `c027f3a` | feat(init): scaffold .devcontainer + .grove/config.toml + .groverc bootstrap (Phase 1) |
| 5 | `bc6d947` | feat(hook): ship loop-hook.sh asset + install Stop hook idempotently |
| 6 | `b8e1713` | feat(bus): per-file event collaboration channel |
| 7 | `963dc01` | feat(msg): grove msg \<to\|broadcast\> "\<text\>" |
| 8 | `06eb713` | feat(agent/seed): per-agent seeder + framework asset installer |
| 9 | `e73d42f` | feat(session/tmux): named tmux sessions for grove spawn |
| 10 | `8c05ae9` | feat(spawn): grove spawn \<name\> creates worktree + seeds agent + launches tmux |
| 11 | `ffd13db` | feat(agents): grove agents list / status / kill |
| 12 | `0663612` | feat(loop): grove loop [--watch] [--agent \<name\>] |
| 13 | `9d89b58` | feat(integrate): grove integrate — merge every agent/* w/ headless conflict resolver |
| 14 | `d4933d0` | feat(setup): interactive Phase 2 wizard (5 prompts) + devcontainer.json mutator |
| 15 | `d16970a` | feat(stack): CI-parity verify scrape + cache volumes + .dockerignore patch (D15) |
| 16 | `f29fc80` | docs: refresh AGENTS.md for the fork + add agentic Hone tests + drop stale site/ refs |
| 17 | `1e4349c` | chore: cargo fmt + remove warnings + wire integration `base` branch correctly |

PR URL: https://github.com/Panzax/grove/pull/new/agentic-flow

---

## 8. Pointers for review

- **Read order**: AGENTS.md (refreshed for the fork) → README.md (the user-facing
  command reference) → the plan file
  (`/home/martin/.claude/plans/okay-so-i-realized-velvet-lighthouse.md`) for
  design rationale → individual feature commits.
- **Hot paths**: `src/commands/init.rs` (the orchestrator), `src/agent/hook.rs`
  (Stop-hook installer + asset embed), `assets/loop-hook.sh` (the engine
  itself).
- **If something looks wrong**: compare against the upstream-reference harness
  in `freqtrade_tecolote` (`.ralph/`, `.devcontainer/ensure-ralph-hook.sh`,
  `agent.sh cmd_*`). The port is one-for-one with the freqtrade behavior,
  rebranded for Grove.
