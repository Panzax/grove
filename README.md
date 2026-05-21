# grove

> **Panzax fork** of [`captainsafia/grove`](https://github.com/captainsafia/grove). Adds
> a portable agentic-workflow layer (devcontainer-aware setup wizard, multi-agent worktree
> spawning, file-based collaboration bus, Stop-hook Ralph loop engine) on top of the
> original worktree-management CLI. Upstream credit preserved; see `LICENSE`.

Grove is a CLI for managing Git worktrees and driving long-running coding agents inside
isolated worktrees. The worktree primitives are upstream; the `init` wizard, `spawn`,
`agents`, `loop`, `msg`, and `integrate` commands are new in this fork.

## Features

**Worktree management (upstream)**
- Initialize repos with a bare clone optimized for worktrees
- Create, list, and remove worktrees
- Sync with origin and prune stale worktrees
- Run commands from anywhere within the project hierarchy
- Shell integration for seamless directory navigation
- Self-update to the latest version or PR build

**Agentic workflow (this fork)**
- `grove init` runs a stack-aware setup wizard: scaffolds `.devcontainer/`, proposes
  mounts (project secrets, `.claude` plugins, GitHub PAT, data dirs), suggests VS Code
  extensions + container packages tailored to the detected stack.
- `grove spawn <name>` creates an isolated worktree, seeds an agent profile, and launches
  a Claude Code session bound to a Stop-hook Ralph loop.
- `grove agents list|status|kill` to manage running agent sessions.
- `grove loop [--watch]` to inspect per-agent loop state (iteration, last action, status).
- `grove msg <to> "<text>"` for direct/broadcast messaging between agents via a
  per-file event bus.
- `grove integrate` merges every `agent/*` branch into a disposable integration branch
  with a headless conflict resolver that reads bus + STATE context.

## Platform Support

- Linux (x64, arm64)
- macOS (x64, arm64)
- Windows: WSL only (tmux + devcontainer required for the agentic layer)

## Installation

### Build from source

```bash
git clone https://github.com/Panzax/grove.git
cd grove
cargo install --path .
```

The binary lands at `~/.cargo/bin/grove`.

### Prerequisites for the agentic layer

- [Claude Code CLI](https://docs.claude.com/en/docs/claude-code) installed and authenticated
- Docker + the [Dev Containers CLI](https://github.com/devcontainers/cli) (`npm i -g @devcontainers/cli`)
- `tmux`
- `jq`
- git ≥ 2.46 on the host (Ubuntu 22.04 default is 2.34 — upgrade via `ppa:git-core/ppa`). Container git is auto-installed via the `ghcr.io/devcontainers/features/git:1` feature grove pins at init time.

#### One-time: accept `--dangerously-skip-permissions` on the host

**Do this BEFORE your first `grove spawn` / `grove integrate`.** Otherwise every spawned agent will hang on the in-container claude prompt asking you to acknowledge `--dangerously-skip-permissions` — and the agent has no human to acknowledge it, so the bootstrap turn freezes.

```bash
# Run claude once with the flag, accept the warning when prompted, then exit.
claude --dangerously-skip-permissions
# (type "I understand" / Y / whatever the prompt asks)
# (Ctrl-C or /exit to leave)

# Verify the accept state landed:
grep -i 'bypass\|danger\|accept' ~/.claude.json
```

Once the accept flag is in your host's `~/.claude.json`, grove's baseline mount of that file (RO) carries the state into every devcontainer. The acknowledgement is now skipped for every spawn.

> **Why:** `claude --dangerously-skip-permissions` shows a one-time interactive acknowledgement before granting the agent unrestricted shell + file access. The acceptance persists in `~/.claude.json`. Grove agents run inside the devcontainer with that mount RO from the host, so the host's accept state is what counts. Without it, the first prompt sits forever and the Stop hook never sees an assistant turn complete.

Worktree primitives (`add`, `go`, `list`, `remove`, `prune`, `sync`, `pr`) work without
any of these.

## Usage

### Initialize a new project

Two modes — pick whichever fits the situation:

**Clone fresh (upstream layout):**
```bash
grove init https://github.com/user/repo.git
```
Produces `<repo>/<repo>.git/` (bare clone) with worktrees as siblings.

**Adopt an existing repo in-place (fork addition):**
```bash
cd your-existing-repo
grove init                # uses cwd by default
# or
grove init /path/to/repo  # explicit path
```
The supplied directory must already be a git checkout. Worktrees go under
`<root>/worktrees/<name>/`. Use `--yes` to skip the merge/overwrite prompt
on existing `.devcontainer/` or `.grove/` files (defaults to overwrite).

**grove is agentic by design — devcontainer is required.** Phase 1 always
scaffolds a devcontainer with grove's runtime prereqs (tmux, jq, perl,
Claude Code CLI) auto-installed via `postCreateCommand`, plus three RO
mounts of `~/.claude/{plugins, .credentials.json, settings.json}` so the
in-container claude can authenticate and the Stop hook engages. `grove
spawn` hard-fails if it can't bring up a devcontainer; if your project
doesn't need agentic spawning, you don't need grove.

If a host tmux config is present (`~/.config/tmux/tmux.conf` or
`~/.tmux.conf`), Phase 1 also adds a RO bind mount so the in-container
tmux inherits your keybinds, theme, and status line. Skipped silently
when neither exists. `grove devcontainer doctor` reports whether the
mount is live.

In both modes:

1. **Phase 1 (deterministic)**: detect stack (Python/Rust/Node/Go/.NET), scaffold
   `.devcontainer/devcontainer.json` + `.grove/config.toml` + extend `.groverc`
   bootstrap entry + patch `.gitignore`. For in-place mode, existing `.devcontainer/`
   or `.grove/` files trigger a `[merge / overwrite / skip]` prompt (or just use
   `--yes` to default to overwrite, with the Phase 2 wizard refining afterwards).
2. **Phase 2 (setup wizard, skippable via `--no-agent`)**: launches a Claude Code session
   that asks five interactive prompts:
   - Project secrets mount (path, RO/RW, env var name)
   - `.claude` scope (`scoped` plugins+creds RO, `full` mount, or `none`) + auth strategy
   - GitHub authentication (read-only fine-grained PAT recommended)
   - Agent-inferred extra mounts (data dirs, env-var-referenced paths, README hints)
   - VS Code extensions + container packages (fixed defaults + per-stack inferred)

Re-run Phase 2 only:

```bash
grove init --reconfigure
```

### Worktree commands (upstream behavior preserved)

```bash
grove add feature/new-feature
grove add feature/new-feature --track origin/feature/new-feature
grove list
grove list --details
grove remove feature/new-feature
grove prune --dry-run
grove sync
grove go feature-branch
```

See the **Worktree commands** section below for full flag coverage — behavior matches
upstream `captainsafia/grove` v2.1.0.

### Agent commands (new)

#### Spawn an agent in an isolated worktree

```bash
grove spawn feat-auth --task "implement OAuth login flow" \
                     --promise "All workitems in STATE.md are [x]" \
                     --max-iter 30
```

Creates a worktree (sibling-to-bare in bare layout, under `worktrees/` in in-place
layout), seeds `.grove/agents/feat-auth/{PROMPT,STATE,loop,agent}.md`, symlinks
the project's `.grove/` into the worktree so the Stop hook + agent docs resolve
from the worktree's cwd, brings the devcontainer up (idempotent), and launches a
tmux session **inside the container** with `claude` and `GROVE_AGENT_DIR`
exported. Per-spawn flags:

- `--task "<text>"` seeds STATE.md with one initial workitem AND switches the
  injected bootstrap prompt into "self-init" mode (see below).
- `--promise "<text>"` sets the `<promise>X</promise>` completion contract.
- `--max-iter N` caps the loop (default 30; 0 = unlimited).
- `--branch <existing>` attaches the worktree to an existing branch instead of
  creating `agent/<name>`. Refuses if the branch is already checked out elsewhere.
- `--no-bootstrap` skips the bootstrap-prompt injection (advanced — claude
  launches with an empty turn instead of a grove-aware first prompt).

##### Bootstrap prompt (injected as claude's first turn)

`grove spawn` appends a bootstrap prompt as the final argv token to
`claude --dangerously-skip-permissions`, so the agent's first turn already
explains where it is and what to do:

- **Orientation** (always): tells the agent its name, repo, worktree cwd,
  `$GROVE_AGENT_DIR`, and points at `.grove/{RALPH-LOOP.md, PROTOCOL.md,
  SHARED.md}` + the three editable files (`PROMPT.md`, `STATE.md`,
  `loop.md`).
- **Fresh + `--task`**: instructs the agent to use the plan workflow to
  decompose the task into 3–10 verifiable workitems, write them into
  STATE.md, tune PROMPT.md, set `loop.md.completion_promise` + `active:
  true`, then stop. The Stop hook then re-injects PROMPT.md as turn 2 and
  the loop begins. Agents bootstrap their own loop with zero hand-holding.
- **Fresh + no task**: tells the agent the user will define PROMPT/STATE.
- **Resume**: short "you're resuming — read loop.md + STATE.md, don't redo
  bootstrap" note.

Templates live in `assets/AGENT_BOOTSTRAP*.md`. Edit if you want to change
the project-wide bootstrap content.

##### How spawn finds the agent across host/container

```
HOST                                    CONTAINER
~/Documents/GitHub/myrepo/              /workspaces/myrepo/
├── .git/                               ├── .git/
├── .grove/                             ├── .grove/                  (same files)
│   ├── agents/feat-auth/               │   ├── agents/feat-auth/
│   │   ├── PROMPT.md                   │   │   ├── PROMPT.md
│   │   ├── STATE.md                    │   │   ├── STATE.md
│   │   ├── loop.md                     │   │   ├── loop.md
│   │   └── agent.toml                  │   │   └── agent.toml
│   ├── bus/                            │   ├── bus/                 (collab channel)
│   └── tools/loop-hook.sh              │   └── tools/loop-hook.sh
├── worktrees/feat-auth/                ├── worktrees/feat-auth/
│   └── .grove ─→ ../../.grove          │   └── .grove ─→ ../../.grove
└── ...                                 └── ...

                                        $GROVE_AGENT_DIR = /workspaces/myrepo/.grove/agents/feat-auth
                                        claude --dangerously-skip-permissions
                                            │
                                            └─ Stop hook fires → bash loop-hook.sh
                                                  → re-feed PROMPT.md as next turn
```

The container mounts the host project root at `/workspaces/<repo>/`. `grove
spawn` brings the devcontainer up (one container per repo, shared by all
agents), translates host paths to container paths, and launches the tmux
session via `devcontainer exec -- tmux new-session -d ...`. The Stop hook
(installed in `~/.claude/settings.json`, mounted RO into the container)
re-injects the prompt as each turn ends.

##### Devcontainer fallback

When `.grove/config.toml [devcontainer] enabled = false`, or when the
`devcontainer` CLI isn't installed, spawn falls back to host tmux and prints
`[host]` next to the launched session — useful for grove projects that
don't use containers, or for development against a remote dev VM.

#### Manual devcontainer control

```bash
grove devcontainer up        # ensure container is up (idempotent)
grove devcontainer down      # stop
grove devcontainer status    # up/down + list grove- tmux sessions inside
grove devcontainer exec bash # one-off shell in the container
grove devcontainer rebuild   # `devcontainer up --remove-existing-container`
grove devcontainer logs      # `devcontainer logs`
```

`grove spawn` calls `up` automatically; these are for debugging, teardown,
and one-off in-container commands.

#### Inspect loop state

```bash
grove loop                  # snapshot of every active loop
grove loop --watch          # live updates via fs watcher
grove loop --agent feat-auth
```

#### Manage running agents

```bash
grove agents list                       # one line per agent
grove agents status feat-auth
grove agents kill feat-auth             # stop the tmux session; loop.md flipped active:false
grove agents purge feat-auth            # delete .grove/agents/<name>/ entirely (so next spawn starts FRESH instead of resuming). Worktree NOT removed — use `grove remove`.
grove agents repair-pointers            # rewrite every worktree's .git pointer files to relative paths
grove agents repair-pointers feat-auth  # ...or just one
```

`repair-pointers` rewrites the worktree's `.git` forward pointer and the
matching back-pointer (`<gitdir>/worktrees/<n>/gitdir`) from absolute → relative
so they resolve identically on host and inside the devcontainer. Fresh `grove
spawn` already does this — run `repair-pointers` to fix worktrees created by
older grove versions.

#### Attach to a running agent

```bash
grove attach feat-auth   # devcontainer exec ... -- tmux attach -t grove-feat-auth
```

Reattaches your terminal to the agent's tmux session inside the devcontainer.
Detach with `Ctrl-b d` (default tmux prefix) and the agent keeps running.
Errors cleanly if the container is down or the session doesn't exist.

#### Resume semantics

`grove spawn <name>` with an existing agent state RESUMES:
- Refuses if the tmux session is already alive (run `grove agents kill <name>` first).
- Re-creates the worktree if it was removed via `grove remove`.
- Re-creates the `.grove` symlink if missing.
- Clears any stale `session_id` in `loop.md` so the new claude session is accepted by the Stop hook.
- Preserves PROMPT.md, STATE.md, agent.toml, and the loop's `active` / `iteration` state.

`--task`, `--promise`, `--max-iter`, `--branch` are ignored on resume; edit the files directly or `grove agents purge <name>` and respawn to change them.

#### Inter-agent messaging

```bash
grove msg feat-data "hyperliquid client ready on agent/shared"   # direct
grove msg broadcast "API contract v2 published in contracts/"    # broadcast
```

Messages land in `.grove/bus/`. Direct goes to `inbox/<recipient>/`; broadcast to
`log.d/`. Agents read their inboxes each loop iteration.

#### Integrate finished work

```bash
grove integrate --into main                        # merge every agent/* branch
grove integrate feat-a feat-b --into develop       # merge only the named branches
grove integrate agent/feat-a feature/x --into main # mix shorthand and full ref names
grove attach integrate-<ts>                        # watch the agent do its thing
```

Positional branch names are optional. With none, the orchestrator merges
every `agent/*` branch (minus `agent/shared`). With one or more names, only
those branches are merged. Each name is resolved literally first, then with
an `agent/` prefix — so `feat-a` resolves to `agent/feat-a` if no literal
`feat-a` branch exists. Non-`agent/*` branches (like `feature/x`) work too;
the agent prefix is only a fallback. Unknown names abort the run before any
worktree side-effects.

`grove integrate` is agent-driven: it sets up an integration worktree on
`integration/<ts>` (branched off `--into`), snapshots bus + per-branch
`STATE.md` plus auto-generated `branches.json` + `overlap.txt` (file
dependency hints) into a read-only `.grove-context/` directory, then
**spawns a Ralph-loop integration agent inside the devcontainer** to:

1. Decide a merge order from the overlap matrix (lower overlap first).
2. Merge each `agent/*` branch, resolving conflicts using per-branch
   intent from the context snapshot.
3. Run the project's `[verify].test_command` (or skip with `--no-test`).
4. Open a PR against `--into` via `gh pr create` with a standardized
   body summarizing per-branch deliverables and conflict resolutions.

Container requirement is hard: the resolver is an autonomous agent, so
sandboxing is mandatory. No `--allow-host` escape hatch.

`gh` is installed in the container automatically (Phase 1 prereqs); the
host's `GH_TOKEN_RO` env var is mapped to the container's `GH_TOKEN`
(see `.devcontainer/devcontainer.json::containerEnv`). The agent runs
`gh auth status` first and roadblocks on auth failure instead of looping.

Monitor live with `grove attach integrate-<ts>` or check status via
`grove agents status integrate-<ts>`. The agent's PROMPT.md / STATE.md /
loop.md are in `.grove/agents/integrate-<ts>/` — operator can edit + flip
`active: true` to resume if the agent roadblocks.

### Worktree commands (full reference)

#### Add

```bash
grove add                                  # auto-generated adjective-noun name
grove add feature/new-feature
grove add feature/new-feature --track origin/feature/new-feature
```

`.groverc` schema (upstream-compatible):

```json
{
  "branchPrefix": "panzax",
  "bootstrap": {
    "commands": [
      { "program": "devcontainer", "args": ["up", "--workspace-folder", "."] },
      { "program": "cargo", "args": ["check"] }
    ]
  }
}
```

#### Remove

```bash
grove remove feature/new-feature
grove remove feature/foo bugfix/bar --force
grove remove feature/foo --yes
```

#### Navigate

```bash
grove go feature-branch
```

Set up shell integration to make `grove go` change the current shell's directory:

```bash
echo 'eval "$(grove shell-init bash)"' >> ~/.bashrc      # bash
echo 'eval "$(grove shell-init zsh)"'  >> ~/.zshrc       # zsh
echo 'eval "$(grove shell-init fish)"' >> ~/.config/fish/config.fish  # fish
```

#### List / Sync / Prune

```bash
grove list [--details|--dirty|--locked]
grove sync [--branch <name>]
grove prune [--dry-run|--force] [--base <branch>|--older-than <duration>]
```

`prune --older-than` accepts `30d`, `2w`, `6M`, `1y`, or ISO 8601 (`P30D`, `P2W`, ...).

#### Run commands from anywhere

Grove discovers the bare clone by walking up from `cwd` and caches the path in
`GROVE_REPO`. Subsequent commands skip the discovery walk.

#### Self-update

```bash
grove self-update                  # latest release
grove self-update v1.0.0           # pinned version
grove self-update --pr 42          # PR build (requires gh CLI)
```

## Commands

**Worktree primitives**
- `grove init <git-url>` — Create a new worktree setup (extended with agentic scaffold)
- `grove add [name] [options]` — Create a new worktree
- `grove go <name>` — Navigate to a worktree
- `grove remove [names]... [options]` — Remove one or more worktrees
- `grove list [options]` — List all worktrees
- `grove sync [options]` — Sync the bare clone with origin
- `grove prune [options]` — Remove worktrees for merged branches
- `grove pr <number>` — Check out a PR into a worktree
- `grove shell-init <shell>` — Emit shell integration
- `grove self-update [version] [options]` — Update grove

**Agentic workflow**
- `grove spawn <name> [options]` — Spawn agent in isolated worktree
- `grove agents <list|status|kill>` — Manage running agents
- `grove loop [--watch] [--agent <name>]` — Inspect Ralph loop state
- `grove msg <to> "<text>"` — Send a message via the bus
- `grove integrate` — Merge all `agent/*` branches into an integration branch

## Project layout after `grove init`

```
<repo_name>/
├── <repo_name>.git/         # bare clone (managed by grove)
├── .devcontainer/
│   └── devcontainer.json    # scaffolded; refined by setup wizard
├── .grove/
│   ├── config.toml          # extended config (agent, mounts, verify, caches, ...)
│   ├── RALPH-LOOP.md        # loop-authoring guide
│   ├── PROTOCOL.md          # bus + agent/shared spec
│   ├── SHARED.md            # canonical project context (chmod 0444 in worktrees)
│   ├── PROMPT.template.md   # skeleton for grove spawn
│   ├── tools/
│   │   └── loop-hook.sh     # Stop-hook engine
│   ├── agents/              # gitignored: per-agent state
│   │   └── <name>/{PROMPT,STATE,loop}.md
│   └── bus/                 # gitignored: collaboration channel
│       ├── log.d/           #   broadcast events (one file per event)
│       ├── inbox/<agent>/   #   direct mail
│       └── contracts/       #   negotiated interfaces
├── .groverc                 # upstream-compatible bootstrap config
└── worktrees/               # gitignored: agent worktrees
    └── <name>/
```

## Development

### Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) (stable toolchain)
- Git, jq, tmux (for the agentic layer)

### Setup

```bash
git clone https://github.com/Panzax/grove.git
cd grove
cargo build --release
```

### Common commands

```bash
cargo build             # debug binary
cargo build --release   # optimized binary
cargo run -- <cmd>      # run without installing
cargo check             # type-check
cargo test --all        # unit + integration tests
```

### Development workflow (editable install + warm rebuilds)

Grove has no `pip install -e .` equivalent in cargo. The closest setup that
keeps `grove` "live" against your checkout is:

**1. Shell function** in `~/.bashrc` / `~/.zshrc`:

```bash
grove() {
  cargo run --quiet --manifest-path "$HOME/Documents/GitHub/grove/Cargo.toml" -- "$@"
}
```

Calling `grove` from any cwd compiles+runs the current checkout. Cargo
skips the rebuild if nothing changed, so subsequent invocations are
near-instant. The function uses `--manifest-path` so cwd doesn't matter —
important since grove is meant to operate on *other* repos.

If you need to call an installed `grove` binary instead of the function,
use `command grove ...` or `~/.cargo/bin/grove ...`.

For release-mode smoke tests:

```bash
groverel() {
  cargo run --release --quiet --manifest-path "$HOME/Documents/GitHub/grove/Cargo.toml" -- "$@"
}
```

**2. Bacon** ([dystroy.org/bacon](https://dystroy.org/bacon/)) in a background
terminal pane keeps the incremental compile cache warm:

```bash
cd ~/Documents/GitHub/grove
bacon            # default: `cargo check --all-targets` — surfaces type
                 # errors as you save
bacon build      # keeps target/debug/grove warm so the shell function's
                 # next invocation is ~50ms
bacon test       # `cargo test` on every save
bacon clippy     # linter
```

The `bacon.toml` at the repo root defines all four jobs.

**3. Make `grove` available on `$PATH`** (for hooks, scripts, and any
non-interactive shell — the shell function above only fires in interactive
shells):

```bash
# One-time: build a release binary, then symlink it into ~/.cargo/bin/
# (which is on $PATH for any user with a Rust toolchain).
cargo build --release
mkdir -p ~/.cargo/bin
ln -sf "$HOME/Documents/GitHub/grove/target/release/grove" ~/.cargo/bin/grove
```

Then run `bacon release` in a background pane (defined in `bacon.toml`)
so the symlink target stays current as you edit. The first invocation
after a save costs a release-mode rebuild (a few seconds — bacon does it,
not you). Subsequent invocations resolve through the symlink at native
binary speed: no cargo overhead per call.

If you'd rather keep dev-iteration faster and `grove` on PATH only
periodically (e.g. ship-when-stable), use `cargo install --path .`
instead — it copies the binary into `~/.cargo/bin/grove` but doesn't
auto-update on edits. Run again after each change you want to publish.

**4. Recommended setup overall**:
- `bacon release` in a background pane (keeps the PATH-symlink target warm).
- The `grove()` shell function in your rc file (interactive dev convenience).
- `~/.cargo/bin/grove` symlink → `target/release/grove` (covers hooks +
  non-interactive callers + the multi-agent worktree scenarios where grove
  invokes itself recursively).

When the function and the symlink both exist: interactive shells call the
function (cargo overhead, debug build, faster recompile); scripts /
sub-processes call the binary via PATH (release, near-zero overhead).
Both run against the same source tree.

**Tradeoffs by approach**

| Approach | Editable? | Speed per invocation | Available to non-interactive callers? |
|---|---|---|---|
| Symlink `target/release/grove` → `~/.cargo/bin/grove` + `bacon release` | Yes | Native binary | Yes |
| Shell function + `bacon build` | Yes | ~50ms cargo overhead | No (functions are shell-local) |
| `cargo install --path .` | No (reinstall per change) | Native | Yes |
| `cargo watch -x run` | Only in the watching terminal | N/A | No |

## License

MIT. Copyright (c) 2025 Safia Abdalla (upstream). Fork additions copyright (c) 2026
Panzax. See `LICENSE`.

## Credits

- Upstream: [`captainsafia/grove`](https://github.com/captainsafia/grove) — the worktree
  primitives. Blog post: ["git worktrees"](https://blog.safia.rocks/2025/09/03/git-worktrees/).
- Stop-hook loop engine adapted from the official Anthropic
  [`ralph-loop`](https://github.com/anthropics/claude-plugins-official/tree/main/plugins/ralph-loop)
  plugin (`stop-hook.sh`).
- Ralph technique: [Geoffrey Huntley — "Ralph Wiggum as a software engineer"](https://ghuntley.com/ralph/).
