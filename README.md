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
from the worktree's cwd, and launches a tmux session with `claude` and
`GROVE_AGENT_DIR` exported. Per-spawn flags:

- `--task "<text>"` seeds STATE.md with one initial workitem.
- `--promise "<text>"` sets the `<promise>X</promise>` completion contract.
- `--max-iter N` caps the loop (default 30; 0 = unlimited).
- `--branch <existing>` attaches the worktree to an existing branch instead of
  creating `agent/<name>`. Refuses if the branch is already checked out elsewhere.

#### Inspect loop state

```bash
grove loop                  # snapshot of every active loop
grove loop --watch          # live updates via fs watcher
grove loop --agent feat-auth
```

#### Manage running agents

```bash
grove agents list           # one line per agent
grove agents status feat-auth
grove agents kill feat-auth
```

#### Inter-agent messaging

```bash
grove msg feat-data "hyperliquid client ready on agent/shared"   # direct
grove msg broadcast "API contract v2 published in contracts/"    # broadcast
```

Messages land in `.grove/bus/`. Direct goes to `inbox/<recipient>/`; broadcast to
`log.d/`. Agents read their inboxes each loop iteration.

#### Integrate finished work

```bash
grove integrate
```

Creates a disposable `integration/<timestamp>` branch and merges every `agent/*` branch
into it. On conflict, snapshots bus + per-branch `STATE.md` into a read-only context
directory and invokes a headless Claude session to resolve with intent. Runs the
project's `[verify].test_command` between merges if configured. Human reviews and merges
into the base branch.

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
