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
use crate::models::{AgentMetadata, ProjectLayout};
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

    // Layout-aware worktree placement.
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

    let agent_dir = project_root_path.join(".grove").join("agents").join(name);

    // Container is mandatory. Bring it up before any tmux-liveness probes.
    let container = ensure_container_up(&project_root_path);
    println!(
        "  {} devcontainer ready (workspace_target {})",
        "·".dimmed(),
        container.workspace_target.display()
    );
    if !tool_in_container(&container, "tmux") {
        eprintln!("{} tmux is missing inside the container.", "Error:".red());
        eprintln!("  Run `grove devcontainer doctor` to audit (tmux, jq, perl, claude),");
        eprintln!("  then `grove devcontainer rebuild` after fixing devcontainer.json.");
        std::process::exit(1);
    }

    // Refuse to spawn if a session for this name is already live in the
    // container — the running agent owns its worktree/loop.md/state.
    let session_name = crate::session::tmux::session_name(name);
    if crate::session::tmux::has_session(&session_name, Some(&container)).unwrap_or(false) {
        eprintln!(
            "{} agent '{}' is already running (tmux session {} alive). Run `grove agents kill {}` first if you want to restart it.",
            "Error:".red(),
            name,
            session_name,
            name
        );
        std::process::exit(1);
    }

    // Two flows from here: RESUME if the agent dir already exists, FRESH otherwise.
    let resume = agent_dir.exists();
    let (final_agent_dir, target_branch) = if resume {
        resume_agent(
            &ctx,
            &project_root_path,
            &worktree_path,
            &worktree_path_str,
            &agent_dir,
            name,
            branch,
            task,
        )
    } else {
        fresh_agent(
            &ctx,
            &project_root_path,
            &worktree_path,
            &worktree_path_str,
            name,
            branch,
            task,
            promise.unwrap_or(DEFAULT_PROMISE),
            max_iter.unwrap_or(DEFAULT_MAX_ITERATIONS),
        )
    };
    let agent_dir = final_agent_dir;

    // Build the tmux session spec. Path translation to container-side
    // happens inside `launch_detached` via the ContainerInfo.
    let mut env: HashMap<String, String> = HashMap::new();
    env.insert(
        "GROVE_AGENT_DIR".into(),
        agent_dir.to_string_lossy().to_string(),
    );
    env.insert("GROVE_AGENT_NAME".into(), name.to_string());

    let cmd_tokens = launch_command_tokens();
    let spec = SessionSpec {
        name,
        workdir: &worktree_path,
        env,
        command: cmd_tokens.clone(),
    };

    match launch_detached(&spec, Some(&container)) {
        Ok(session_name_str) => {
            let verb = if resume { "Resumed" } else { "Spawned" };
            println!(
                "{} {} agent {} on {} (tmux {} {}) [in container]",
                "✓".green(),
                verb,
                name.bold(),
                target_branch.bold(),
                session_name_str.bold(),
                cmd_tokens.join(" ").dimmed()
            );
            println!(
                "  attach: {}",
                crate::session::tmux::attach_instructions(name, Some(&container))
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
                agent_dir.display(),
                cmd_tokens.join(" ")
            );
        }
    }

    println!();
    if resume {
        println!(
            "{}",
            "Loop resumed from previous state. PROMPT.md / STATE.md / loop.md unchanged.".dimmed()
        );
    } else {
        println!(
            "{}",
            "Next: edit PROMPT.md / STATE.md, then flip loop.md `active: true` to start the loop."
                .dimmed()
        );
    }
}

/// Fresh agent path. Creates worktree, seeds .grove/agents/<n>/, writes agent.toml.
#[allow(clippy::too_many_arguments)]
fn fresh_agent(
    ctx: &crate::git::worktree_manager::RepoContext,
    project_root_path: &Path,
    worktree_path: &Path,
    worktree_path_str: &str,
    name: &str,
    branch: Option<&str>,
    task: Option<&str>,
    promise: &str,
    max_iter: u32,
) -> (PathBuf, String) {
    // Resolve target branch.
    let (target_branch, create_new) = match branch {
        Some(b) => {
            if !branch_exists(ctx, b) {
                eprintln!(
                    "{} --branch {} does not exist. Create it first or omit --branch to use agent/{}.",
                    "Error:".red(),
                    b,
                    name
                );
                std::process::exit(1);
            }
            if let Some(other_wt) = branch_already_checked_out(project_root_path, b) {
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
            let exists = branch_exists(ctx, &agent_branch);
            (agent_branch, !exists)
        }
    };

    if let Err(e) = add_worktree(ctx, worktree_path_str, &target_branch, create_new, None) {
        eprintln!("{} create worktree: {}", "Error:".red(), e);
        std::process::exit(1);
    }
    println!(
        "  {} worktree at {} on {}",
        "·".dimmed(),
        worktree_path.display(),
        target_branch.bold()
    );

    if let Err(e) = seed::link_grove_into_worktree(worktree_path, project_root_path) {
        eprintln!("  {} link .grove into worktree: {}", "Warning:".yellow(), e);
    } else {
        println!(
            "  {} linked .grove -> {}/.grove",
            "·".dimmed(),
            project_root_path.display()
        );
    }

    let agent_dir = match seed::seed_agent(project_root_path, name, task, promise, max_iter) {
        Ok(p) => {
            println!("  {} seeded {}", "·".dimmed(), p.display());
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
        worktree: worktree_path_str.to_string(),
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
    (agent_dir, target_branch)
}

/// Resume agent path. Re-uses the existing .grove/agents/<n>/ state.
///
/// Repair semantics (handles crashes / partial state):
/// - Re-create the worktree if it was removed by `grove remove`.
/// - Re-create the .grove symlink if it's gone.
/// - Clear loop.md `session_id` so the Stop hook accepts the new session.
/// - Preserve PROMPT.md, STATE.md, agent.toml — user / agent edits survive.
///
/// `--branch`, `--task`, `--promise`, `--max-iter` are IGNORED on resume to
/// avoid silent drift from the recorded agent.toml + loop.md. To change
/// these, edit the files directly or `grove agents purge <name>` and respawn.
#[allow(clippy::too_many_arguments)]
fn resume_agent(
    ctx: &crate::git::worktree_manager::RepoContext,
    project_root_path: &Path,
    worktree_path: &Path,
    worktree_path_str: &str,
    agent_dir: &Path,
    name: &str,
    branch_override: Option<&str>,
    task_override: Option<&str>,
) -> (PathBuf, String) {
    // Read recorded agent.toml for branch + worktree. If agent.toml is
    // missing (older grove version, partial state), fall back to defaults.
    let agent_toml = agent_dir.join("agent.toml");
    let recorded_branch = if agent_toml.exists() {
        std::fs::read_to_string(&agent_toml)
            .ok()
            .and_then(|raw| toml::from_str::<AgentMetadata>(&raw).ok())
            .map(|m| m.branch)
            .unwrap_or_else(|| format!("agent/{}", name))
    } else {
        format!("agent/{}", name)
    };

    if branch_override.is_some() && branch_override != Some(recorded_branch.as_str()) {
        eprintln!(
            "  {} --branch ignored on resume (agent is recorded against {}). Edit .grove/agents/{}/agent.toml or purge + respawn to change.",
            "Note:".yellow(),
            recorded_branch,
            name
        );
    }
    if task_override.is_some() {
        eprintln!(
            "  {} --task ignored on resume (STATE.md already seeded). Edit STATE.md to add new workitems.",
            "Note:".yellow()
        );
    }

    // Re-add the worktree if it's gone (e.g. user ran `grove remove` then
    // `grove spawn` to resume). add_worktree refuses if the worktree already
    // exists, so we only call it when the dir is missing.
    if !worktree_path.exists() {
        let create_new = !branch_exists(ctx, &recorded_branch);
        if let Err(e) = add_worktree(ctx, worktree_path_str, &recorded_branch, create_new, None) {
            eprintln!("{} recreate worktree on resume: {}", "Error:".red(), e);
            std::process::exit(1);
        }
        println!(
            "  {} re-created worktree at {} on {}",
            "·".dimmed(),
            worktree_path.display(),
            recorded_branch.bold()
        );
    }

    // Re-link .grove (idempotent — Ok if symlink exists).
    if let Err(e) = seed::link_grove_into_worktree(worktree_path, project_root_path) {
        eprintln!("  {} link .grove into worktree: {}", "Warning:".yellow(), e);
    }

    // Clear stale session_id in loop.md so the Stop hook's isolation guard
    // doesn't silently reject the new claude session.
    let loop_path = agent_dir.join("loop.md");
    if let Err(e) = crate::agent::loop_md::clear_session_id(&loop_path) {
        eprintln!(
            "  {} clear loop.md session_id: {} (you may need to edit manually)",
            "Warning:".yellow(),
            e
        );
    } else {
        println!(
            "  {} cleared stale session_id in loop.md (hook will accept the new session)",
            "·".dimmed()
        );
    }

    (agent_dir.to_path_buf(), recorded_branch)
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

/// Bring the project's devcontainer up. Hard-errors on failure — grove is an
/// agentic tool, agents run inside the container, no devcontainer means no
/// grove.
fn ensure_container_up(project_root: &Path) -> ContainerInfo {
    match container::ensure_up(project_root) {
        Ok(info) => info,
        Err(e) => {
            eprintln!("{} `devcontainer up` failed: {}", "Error:".red(), e);
            eprintln!("  grove requires a working devcontainer. Install the devcontainer CLI ");
            eprintln!("  (`npm i -g @devcontainers/cli`) and Docker, then retry.");
            std::process::exit(1);
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
