// Agentic layer entry point.
//
// Submodules:
//   - `hook`    : install / verify the Claude Code Stop hook into ~/.claude/settings.json
//   - `seed`    : write per-agent PROMPT/STATE/loop.md files
//   - `setup`   : Phase 2 of grove init (interactive wizard backed by `claude -p`)
//   - `loop_md` : parse / mutate the YAML frontmatter of loop.md
//
// The Stop-hook engine itself is the bash script in `assets/loop-hook.sh`, which is
// `include_str!`'d and dropped into `.grove/tools/loop-hook.sh` during init.

pub mod bootstrap;
pub mod hook;
pub mod integrate_deps;
pub mod integrate_seed;
pub mod loop_md;
pub mod seed;
pub mod setup;
