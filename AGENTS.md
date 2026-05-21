Instructions for coding agents working on the Grove repository.

> **Fork note.** This is the `Panzax/grove` fork. The upstream `captainsafia/grove`
> ships only the worktree primitives; this fork adds an agentic-workflow layer
> (devcontainer-aware setup wizard, `grove spawn` / `agents` / `loop` / `msg` /
> `integrate`, file-based collaboration bus, Stop-hook Ralph loop engine). All
> additions live in new modules; existing modules retain upstream behavior.

## Project Overview

Grove is a CLI tool written in Rust that manages Git worktrees and drives
long-running coding agents inside isolated worktrees. Targets Linux + macOS
(Windows via WSL).

**Key technologies:**
- Language: Rust (2021 edition)
- CLI Framework: clap (derive macros)
- Git Operations: git CLI shell-outs via `std::process::Command`
- Terminal UI: dialoguer (fuzzy-select), colored
- Serialization: serde, serde_json
- Testing: Rust's built-in test framework (`cargo test`)

## Repository Structure

```
src/
├── main.rs                  # CLI entry point, clap command registration
├── models.rs                # Worktree + agentic types (ProjectContext, AgentMetadata,
│                            #   LoopState, BusMessage, GroveConfig + sections)
├── utils.rs                 # Helper functions (discovery, formatting, .groverc parsing)
├── commands/                # One file per CLI command (worktree + agentic)
│   ├── mod.rs
│   ├── add.rs               # (upstream) grove add — worktree + bootstrap commands
│   ├── go.rs                # (upstream) grove go — navigate to a worktree
│   ├── init.rs              # (extended) bare clone + Phase 1 scaffold + Phase 2 hook
│   ├── list.rs              # (upstream)
│   ├── pr.rs                # (upstream)
│   ├── prune.rs             # (upstream)
│   ├── remove.rs            # (upstream)
│   ├── self_update.rs       # stubbed on this fork (no hosted install endpoint yet)
│   ├── shell_init.rs        # (upstream)
│   ├── sync.rs              # (upstream)
│   ├── spawn.rs             # NEW: worktree + seed agent + launch tmux session
│   ├── agents.rs            # NEW: list/status/kill running agents
│   ├── loop_.rs             # NEW: print/watch .grove/agents/<n>/loop.md
│   ├── msg.rs               # NEW: bus messaging (broadcast/direct/contract)
│   └── integrate.rs         # NEW: merge agent/* into integration/<ts> + headless resolver
├── agent/                   # NEW: Ralph loop infrastructure
│   ├── mod.rs
│   ├── hook.rs              #   install loop-hook.sh into ~/.claude/settings.json
│   ├── loop_md.rs           #   YAML frontmatter parser for loop.md
│   ├── seed.rs              #   write .grove/agents/<n>/{PROMPT,STATE,loop}.md + assets
│   └── setup.rs             #   Phase 2 interactive wizard (5 prompts)
├── bus/                     # NEW: per-file event bus (broadcast/direct/contract)
│   └── mod.rs
├── devcontainer/            # NEW: stack detection + scaffold + CI-parity scrape
│   ├── mod.rs
│   ├── stack.rs             #   detect_all_stacks, infer_*, verify_defaults, cache_volumes
│   └── ci_scrape.rs         #   extract verify commands from .github/workflows/*.yml
├── session/                 # NEW: tmux backend
│   ├── mod.rs
│   └── tmux.rs
└── git/
    ├── mod.rs
    └── worktree_manager.rs  # Core Git worktree operations (extended with
                             # show_head_file, ls_head_files, head_file_exists for the
                             # bare-clone manifest probe).

assets/                      # NEW: bundled framework files (include_str!'d into binary)
├── loop-hook.sh             # Stop-hook engine (bash); copied to .grove/tools/ on init
├── PROMPT.template.md       # per-agent prompt skeleton
├── RALPH-LOOP.md            # loop-authoring guide (installed to .grove/)
├── PROTOCOL.md              # bus + agent/shared hub-branch spec (installed to .grove/)
└── SHARED.md                # canonical project context template (installed to .grove/)

test/
└── integration/             # Hone integration tests
```

## Development Commands

```bash
# Build debug binary
cargo build

# Build optimized release binary
cargo build --release

# Run directly in development
cargo run -- <command>

# Type check without building
cargo check

# Run all tests
cargo test

# Clean build artifacts
cargo clean
```

Always run `cargo check` and `cargo test` before committing changes.

## Updating Documentation

### README.md

The README at the repository root is the primary documentation. When updating:

1. Keep the existing section structure:
   - Features
   - Installation
   - Quick Start
   - Commands (with examples)
   - Development

2. When adding a new command:
   - Add it to the Commands section with usage syntax and examples
   - Include all flags/options with descriptions
   - Show realistic example output if helpful

3. When changing command behavior:
   - Update the corresponding command documentation
   - Update any affected examples

### Agentic module conventions (fork-specific)

- **Git operations stay in `git/worktree_manager.rs`.** New helpers added in
  this fork (`show_head_file`, `ls_head_files`, `head_file_exists`,
  `get_default_branch`) follow the same pattern — wrap the `git` CLI via
  `git_raw`. Don't bypass it.
- **Agent state lives at the project root** (`.grove/agents/<n>/`), never
  inside a worktree. This is intentional — agent state survives
  `grove remove`. New code that touches per-agent state should call
  `project_root(ctx).join(".grove/agents/<n>")`, not the worktree path.
- **Two project layouts.** `ProjectLayout::Bare` (upstream — bare clone at
  `<root>/<name>.git/`, worktrees as siblings) and `ProjectLayout::InPlace`
  (fork addition — normal `.git/` checkout, worktrees under
  `<root>/worktrees/<name>/`). `RepoContext::layout` carries the choice;
  new commands that compute paths must branch on it. `discover_repo()`
  prefers Bare so existing grove projects keep working unchanged; falls back
  to InPlace via `discover_in_place()`.
- **One file per bus event.** Never append to a shared log; the per-file
  pattern eliminates the multi-writer append race. `bus::send` enforces this.
- **Stop hook is registered at user-level only.** Worktrees never carry a
  project-level `.claude/`. The bash engine self-disables when
  `GROVE_AGENT_DIR` is unset, so the hook is safe to leave registered.
- **`assets/`** is the source of truth for the framework files; `grove init`
  copies them into `.grove/`. Users editing `SHARED.md` after init is fine
  (their copy survives re-init); `RALPH-LOOP.md` / `PROTOCOL.md` /
  `PROMPT.template.md` / `loop-hook.sh` get overwritten on every `grove init`
  to keep the framework consistent.
- **Devcontainer integration.** Spawned agents run inside ONE devcontainer
  per repo (`[agent] isolation = "shared"`; per-worktree containers are a
  future flag). `src/session/container.rs` wraps the `devcontainer` CLI;
  `src/session/tmux.rs` takes `Option<&ContainerInfo>` and routes through
  `devcontainer exec` when present. Path translation (host → container)
  happens at the `tmux.rs` boundary; downstream code never sees raw host
  paths.
- **Container lifecycle ownership.** `grove spawn` is the *only* command
  that auto-`up`s the container. `agents list/status/kill` and `integrate`
  adopt the container if it's already running but never bring it up. This
  is so read-only operations don't trigger a 30-60s container boot. The
  manual `grove devcontainer up/down/...` subcommand is for debugging.
- **Env override pattern.** `GROVE_DEVCONTAINER_COMMAND`,
  `GROVE_AGENT_COMMAND`, `GROVE_RESOLVE_COMMAND` substitute stub binaries
  in tests. Always check for the override at the top of the relevant
  `*_command_tokens()` helper.
- **Stop hook visibility inside the container.** The Phase-2 wizard's
  `scoped` claude mount option mounts `~/.claude/{plugins,
  .credentials.json, settings.json}` RO. The settings.json mount is what
  brings the Stop hook into the container; without it, the loop never
  engages for in-container claude sessions.
- **Worktree `.git` pointers must be relative.** `git worktree add`
  writes absolute host paths into `<worktree>/.git` and the back-pointer
  at `<gitdir>/worktrees/<n>/gitdir`. Those don't resolve inside the
  bind-mounted container. `grove spawn` calls
  `git::worktree_paths::make_worktree_pointers_relative` immediately
  after `add_worktree` (in both fresh and resume paths) to rewrite both
  files to relative paths. `grove agents repair-pointers` exposes the
  same helper as a one-shot bulk fix.
- **Bootstrap prompt asset conventions.** `assets/AGENT_BOOTSTRAP*.md`
  files are baked into the binary via `include_str!`. Placeholders use
  `<UPPER_SNAKE>` (e.g. `<AGENT_NAME>`, `<CONTAINER_WORKTREE_PATH>`).
  `agent::bootstrap::build_bootstrap_prompt` concatenates a fixed
  orientation section with one of three variant sections (task,
  no-task, resume) and substitutes the placeholders. Paths in the
  prompt are **container-side** (translated via
  `container::host_to_container_path`) because the prompt is consumed
  inside the container by claude.
- **Host tmux config bind.** Phase 1 probes `$HOME/.config/tmux/tmux.conf`
  (XDG) then `$HOME/.tmux.conf` (legacy). If found, adds a RO mount to
  `/home/vscode/.tmux.conf` using the `${localEnv:HOME}/...` form so
  devcontainer.json stays portable across machines. Helper lives in
  `src/devcontainer/mod.rs::apply_baseline_tmux_mount`; idempotent;
  silently skips when no host conf exists. The mount is always present
  as the legacy path because tmux reads legacy first regardless of host
  source location.

## Commit Message and PR Title Format

This repository uses [Conventional Commits](https://www.conventionalcommits.org/). All commit messages and PR titles must follow this format:

```
<type>: <subject>
```

### Types

| Type    | Use For                                           |
|---------|---------------------------------------------------|
| `feat`  | New features or functionality                     |
| `fix`   | Bug fixes                                         |
| `chore` | Build, CI/CD, dependencies, maintenance           |
| `test`  | Adding or updating tests                          |
| `doc`   | Documentation only changes                        |

### Rules

1. **Use lowercase** for type and subject
2. **No period** at the end of the subject
3. **Use imperative mood** ("add" not "added" or "adds")
4. **Keep subject under 72 characters**
5. **Reference PR number** when applicable: `(#123)`
6. **Use the same Conventional Commit format for PR titles**

### Examples

```
feat: add support for branch tracking in add command
fix: handle missing git config gracefully
chore: update dependencies to latest versions
test: add edge case tests for prune command
doc: update readme with new installation method
fix: address edge cases in worktree detection (#17)
chore: add notarization for macos binaries (#15)
```

### Multi-line Commits

For complex changes, add a body separated by a blank line:

```
feat: add self-update command

Allows users to update grove to the latest version or a specific
version directly from the CLI. Supports installing PR preview builds
with the --pr flag.
```

## Code Conventions

### Command Files

Each command in `src/commands/` is implemented as a public function that takes parsed arguments and executes the command logic. Commands are registered in `src/main.rs` using clap's derive macros:

```rust
// In src/main.rs
#[derive(Subcommand)]
enum Commands {
    /// Short description of command
    Example {
        /// Argument description
        name: String,
        /// Flag description
        #[arg(short, long)]
        flag: bool,
    },
}
```

Command implementations live in their respective files under `src/commands/`:

```rust
// In src/commands/example.rs
pub fn execute(name: &str, flag: bool) -> Result<(), Box<dyn std::error::Error>> {
    // Implementation
    Ok(())
}
```

### WorktreeManager

Git operations go through `src/git/worktree_manager.rs`. This module uses `std::process::Command` to shell out to the `git` CLI. Extend this module when adding new Git functionality rather than calling git directly in commands.

### Error Handling

Use the utility functions from `src/utils.rs`:

```rust
use crate::utils::{format_error, format_warning};

println!("{}", format_error("Something went wrong"));
println!("{}", format_warning("Proceed with caution"));
```

### Rust

- Edition 2021
- Define shared types in `src/models.rs`
- Use explicit return types for public functions
- Use `#[cfg(test)]` modules for inline unit tests

### Testing

- Unit tests are inline `#[cfg(test)]` modules in the source files they test
- Integration tests are in `test/integration/` (Hone test files)
- Run all tests with `cargo test`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_something() {
        assert_eq!(result, expected);
    }
}
```

## CI/CD

- **CI runs on all PRs**: Type check (`cargo check`), tests (`cargo test`), and build verification (`cargo build --release`)
- **Releases trigger on tags**: Version tags like `v1.0.0` create releases with cross-compiled binaries
- **PR builds**: Each PR gets preview builds for Linux (x64, arm64) and macOS (x64, arm64) with download links posted as comments

## Platform Support

Grove supports:
- Linux (x64, arm64)
- macOS (x64, arm64)
- Windows (x64)

Prefer cross-platform implementations when adding features, and avoid Unix-only command assumptions in user-facing workflows.
