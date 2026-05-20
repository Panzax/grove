// `grove spawn <name>` — create an isolated worktree, seed an agent profile,
// and launch a Claude Code session bound to it.
//
// Builds on the same git primitives `grove add` uses (add_worktree +
// branch_exists), but with agent-aware seeding instead of bootstrap commands.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use colored::Colorize;

use crate::agent::seed;
use crate::git::worktree_manager::{
    add_worktree, branch_exists, discover_repo, layout, project_root,
};
use crate::models::{AgentMetadata, GroveConfig, ProjectLayout};
use crate::session::container::{self, ContainerInfo};
use crate::session::tmux::{launch_detached, SessionSpec};

const DEFAULT_MAX_ITERATIONS: u32 = 30;
const DEFAULT_PROMISE: &str = "All workitems in STATE.md are [x]";

pub fn run(
    name: &str,
    branch: Option<&str>,
    task: Option<&str>,
    promise: Option<&str>,
    max_iter: Option<u32>,
) {
    let ctx = match discover_repo() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "{} grove spawn must run inside a grove-initialized project: {}",
                "Error:".red(),
                e
            );
            std::process::exit(1);
        }
    };

    if !seed::is_valid_agent_name(name) {
        eprintln!(
            "{} agent name '{}' must be kebab-case (letters, digits, '-', '_')",
            "Error:".red(),
            name
        );
        std::process::exit(1);
    }

    let project_root_path = project_root(&ctx).to_path_buf();

    // Layout-aware worktree placement:
    //   Bare layout    -> sibling to the bare clone (<root>/<name>/), same as `grove add`.
    //   In-place layout -> <root>/worktrees/<name>/, so we don't scatter dirs across
    //                      the user's project root.
    let worktree_path: PathBuf = match layout(&ctx) {
        ProjectLayout::Bare => project_root_path.join(name),
        ProjectLayout::InPlace => {
            let nested = project_root_path.join("worktrees");
            if let Err(e) = std::fs::create_dir_all(&nested) {
                eprintln!("{} create worktrees/: {}", "Error:".red(), e);
                std::process::exit(1);
            }
            nested.join(name)
        }
    };
    let worktree_path_str = worktree_path.to_string_lossy().to_string();

    // Resolve target branch:
    //   - --branch X uses an existing branch (errors if it doesn't exist, OR if the
    //     branch is already checked out in another worktree).
    //   - default: agent/<name>, creating it if it doesn't already exist
    let (target_branch, create_new) = match branch {
        Some(b) => {
            if !branch_exists(&ctx, b) {
                eprintln!(
                    "{} --branch {} does not exist. Create it first or omit --branch to use agent/{}.",
                    "Error:".red(),
                    b,
                    name
                );
                std::process::exit(1);
            }
            if let Some(other_wt) = branch_already_checked_out(&project_root_path, b) {
                eprintln!(
                    "{} --branch {} is already checked out at {} (git allows only one worktree per branch).",
                    "Error:".red(),
                    b,
                    other_wt
                );
                std::process::exit(1);
            }
            (b.to_string(), false)
        }
        None => {
            let agent_branch = format!("agent/{}", name);
            let exists = branch_exists(&ctx, &agent_branch);
            (agent_branch, !exists)
        }
    };

    if let Err(e) = add_worktree(&ctx, &worktree_path_str, &target_branch, create_new, None) {
        eprintln!("{} create worktree: {}", "Error:".red(), e);
        std::process::exit(1);
    }
    println!(
        "{} worktree at {} on {}",
        "✓".green(),
        worktree_path.display(),
        target_branch.bold()
    );

    // Symlink the project's .grove/ into the worktree so the Stop hook + agent
    // PROMPT.md references resolve from the worktree's cwd.
    if let Err(e) = seed::link_grove_into_worktree(&worktree_path, &project_root_path) {
        eprintln!("  {} link .grove into worktree: {}", "Warning:".yellow(), e);
    } else {
        println!(
            "  {} linked .grove -> {}/.grove (so Stop hook + agent docs resolve from worktree cwd)",
            "·".dimmed(),
            project_root_path.display()
        );
    }

    // Seed the per-agent state + register metadata. Treated as a single
    // transaction: if either fails, roll back the agent dir so `grove agents`
    // doesn't observe a half-seeded entry.
    let promise_val = promise.unwrap_or(DEFAULT_PROMISE);
    let max_iter_val = max_iter.unwrap_or(DEFAULT_MAX_ITERATIONS);
    let agent_dir =
        match seed::seed_agent(&project_root_path, name, task, promise_val, max_iter_val) {
            Ok(p) => {
                println!("{} seeded {}", "✓".green(), p.display());
                p
            }
            Err(e) => {
                eprintln!(
                "{} seed agent state: {} (worktree still in place; remove with `grove remove`).",
                "Error:".red(),
                e
            );
                std::process::exit(1);
            }
        };

    let metadata = AgentMetadata {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.to_string(),
        worktree: worktree_path_str.clone(),
        branch: target_branch.clone(),
        task: task.map(|s| s.to_string()),
        tmux_session: Some(crate::session::tmux::session_name(name)),
        spawned_at: Utc::now(),
        provider: "claude-code".to_string(),
    };
    let agent_toml = agent_dir.join("agent.toml");
    let body = match toml::to_string_pretty(&metadata) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{} serialize agent.toml: {}", "Error:".red(), e);
            let _ = std::fs::remove_dir_all(&agent_dir);
            std::process::exit(1);
        }
    };
    if let Err(e) = std::fs::write(&agent_toml, body) {
        eprintln!(
            "{} write agent.toml: {} — rolling back seeded agent dir.",
            "Error:".red(),
            e
        );
        let _ = std::fs::remove_dir_all(&agent_dir);
        std::process::exit(1);
    }

    // Launch tmux session running `claude` (or the configured command).
    let agent_dir_abs = agent_dir.clone();

    let mut env: HashMap<String, String> = HashMap::new();
    env.insert(
        "GROVE_AGENT_DIR".into(),
        agent_dir_abs.to_string_lossy().to_string(),
    );
    env.insert("GROVE_AGENT_NAME".into(), name.to_string());

    let cmd_tokens = launch_command_tokens();
    let spec = SessionSpec {
        name,
        workdir: &worktree_path,
        env,
        command: cmd_tokens.clone(),
    };

    // Resolve the spawn target: container or host. When
    // `.grove/config.toml [devcontainer] enabled = true`, ensure the
    // devcontainer is up and route tmux through `devcontainer exec`. When
    // disabled, fall back to host tmux (legacy behavior, useful for
    // grove projects that don't use containers).
    let container = resolve_container(&project_root_path);
    if let Some(info) = &container {
        println!(
            "  {} devcontainer ready (workspace_target {})",
            "·".dimmed(),
            info.workspace_target.display()
        );
        // Hard-fail (not silent host fallback) when the container is up but
        // grove's session backend isn't in it. Silent fallback was hiding
        // bad container images from the user.
        if !crate::session::container::is_up(&project_root_path)
            || !tool_in_container(info, "tmux")
        {
            eprintln!(
                "{} devcontainer is enabled but tmux is missing inside the container.",
                "Error:".red()
            );
            eprintln!(
                "  Add tmux to your devcontainer's postCreateCommand (grove init's scaffold does this automatically), then `grove devcontainer rebuild`."
            );
            eprintln!(
                "  Run `grove devcontainer doctor` to audit every prereq (tmux, jq, perl, claude)."
            );
            std::process::exit(1);
        }
    }
    match launch_detached(&spec, container.as_ref()) {
        Ok(session_name) => {
            println!(
                "{} launched tmux session {} ({}) {}",
                "✓".green(),
                session_name.bold(),
                cmd_tokens.join(" "),
                if container.is_some() {
                    "[in container]".dimmed().to_string()
                } else {
                    "[host]".dimmed().to_string()
                }
            );
            println!(
                "  attach: {}",
                crate::session::tmux::attach_instructions(name, container.as_ref())
            );
        }
        Err(e) => {
            eprintln!(
                "{} could not launch tmux session: {}",
                "Warning:".yellow(),
                e
            );
            println!(
                "  the worktree + agent dir are still in place; you can launch claude manually:"
            );
            println!(
                "    cd {} && GROVE_AGENT_DIR={} {}",
                worktree_path.display(),
                agent_dir_abs.display(),
                cmd_tokens.join(" ")
            );
        }
    }

    println!();
    println!(
        "{}",
        "Next: edit PROMPT.md / STATE.md, then flip loop.md `active: true` to start the loop."
            .dimmed()
    );
}

/// Returns the path of the worktree that already has `branch` checked out, if any.
/// Walks `git worktree list --porcelain` against the project root (works for both
/// bare and in-place layouts via cwd handling).
fn branch_already_checked_out(project_root: &Path, branch: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(project_root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let mut current_path: Option<String> = None;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            current_path = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("branch ") {
            let trimmed = rest.trim_start_matches("refs/heads/");
            if trimmed == branch {
                return current_path.clone();
            }
        }
    }
    None
}

/// Decide whether to run the spawned session in the project's devcontainer.
///
/// Reads `.grove/config.toml [devcontainer]`. When `enabled = false`, returns
/// None (host tmux). When `enabled = true`, calls `container::ensure_up` and
/// returns the resulting ContainerInfo. On any failure to ensure the
/// container, prints a warning and falls back to host tmux so the user isn't
/// hard-blocked.
fn resolve_container(project_root: &Path) -> Option<ContainerInfo> {
    let cfg_path = project_root.join(".grove").join("config.toml");
    let raw = std::fs::read_to_string(&cfg_path).ok()?;
    let cfg: GroveConfig = toml::from_str(&raw).ok()?;
    if !cfg.devcontainer.enabled {
        return None;
    }
    match container::ensure_up(project_root) {
        Ok(info) => Some(info),
        Err(e) => {
            eprintln!(
                "  {} `devcontainer up` failed: {} — falling back to host tmux.",
                "Warning:".yellow(),
                e
            );
            None
        }
    }
}

/// Probe whether `tool` is on PATH inside the running container. Used to
/// hard-fail spawn when a prereq is missing rather than silently fall back
/// to host tmux.
fn tool_in_container(info: &ContainerInfo, tool: &str) -> bool {
    let script = format!("command -v {} >/dev/null 2>&1", tool);
    let argv: Vec<&str> = vec!["sh", "-c", &script];
    container::exec(info, &argv)
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Command vec passed to tmux. Honors `GROVE_AGENT_COMMAND` env override so tests
/// can substitute `bash` or `echo` for `claude`.
fn launch_command_tokens() -> Vec<String> {
    if let Ok(raw) = std::env::var("GROVE_AGENT_COMMAND") {
        let tokens: Vec<String> = raw.split_whitespace().map(|s| s.to_string()).collect();
        if !tokens.is_empty() {
            return tokens;
        }
    }
    vec!["claude".into(), "--dangerously-skip-permissions".into()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_command_uses_claude() {
        std::env::remove_var("GROVE_AGENT_COMMAND");
        let tokens = launch_command_tokens();
        assert_eq!(tokens[0], "claude");
    }

    #[test]
    fn env_override_picks_up_tokens() {
        std::env::set_var("GROVE_AGENT_COMMAND", "bash -c 'sleep 30'");
        let tokens = launch_command_tokens();
        std::env::remove_var("GROVE_AGENT_COMMAND");
        assert_eq!(tokens[0], "bash");
        assert!(tokens.iter().any(|t| t.contains("sleep")));
    }
}
