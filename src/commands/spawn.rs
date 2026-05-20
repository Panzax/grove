// `grove spawn <name>` — create an isolated worktree, seed an agent profile,
// and launch a Claude Code session bound to it.
//
// Builds on the same git primitives `grove add` uses (add_worktree +
// branch_exists), but with agent-aware seeding instead of bootstrap commands.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::Utc;
use colored::Colorize;

use crate::agent::seed;
use crate::git::worktree_manager::{
    add_worktree, branch_exists, discover_repo, project_root,
};
use crate::models::AgentMetadata;
use crate::session::tmux::{launch_detached, SessionSpec};

const DEFAULT_MAX_ITERATIONS: u32 = 30;
const DEFAULT_PROMISE: &str = "All workitems in STATE.md are [x]";

pub fn run(name: &str, branch: Option<&str>, task: Option<&str>) {
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
    let worktrees_dir = project_root_path.join("worktrees");
    if let Err(e) = std::fs::create_dir_all(&worktrees_dir) {
        eprintln!("{} create worktrees/: {}", "Error:".red(), e);
        std::process::exit(1);
    }
    let worktree_path = worktrees_dir.join(name);
    let worktree_path_str = worktree_path.to_string_lossy().to_string();

    // Resolve target branch:
    //   - --branch X uses an existing branch (errors if it doesn't exist)
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
            (b.to_string(), false)
        }
        None => {
            let agent_branch = format!("agent/{}", name);
            let exists = branch_exists(&ctx, &agent_branch);
            (agent_branch, !exists)
        }
    };

    if let Err(e) = add_worktree(
        &ctx,
        &worktree_path_str,
        &target_branch,
        create_new,
        None,
    ) {
        eprintln!("{} create worktree: {}", "Error:".red(), e);
        std::process::exit(1);
    }
    println!(
        "{} worktree at {} on {}",
        "✓".green(),
        worktree_path.display(),
        target_branch.bold()
    );

    // Seed the per-agent state. Don't fail spawn if seeding fails — the worktree
    // is already in place; the user can fix by hand.
    match seed::seed_agent(
        &project_root_path,
        name,
        task,
        DEFAULT_PROMISE,
        DEFAULT_MAX_ITERATIONS,
    ) {
        Ok(p) => println!("{} seeded {}", "✓".green(), p.display()),
        Err(e) => eprintln!("  {} seed agent state: {}", "Warning:".yellow(), e),
    }

    // chmod SHARED.md to 0444 inside the worktree (per-worktree speedbump).
    if let Err(e) = seed::chmod_shared_md_in_worktree(&worktree_path) {
        eprintln!("  {} chmod SHARED.md: {}", "Warning:".yellow(), e);
    }

    // Write agent.toml metadata so `grove agents list` can find this agent.
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
    let agent_toml = project_root_path
        .join(".grove")
        .join("agents")
        .join(name)
        .join("agent.toml");
    let body = toml::to_string_pretty(&metadata).unwrap_or_else(|_| String::new());
    if let Err(e) = std::fs::write(&agent_toml, body) {
        eprintln!("  {} write agent.toml: {}", "Warning:".yellow(), e);
    }

    // Launch tmux session running `claude` (or the configured command).
    let agent_dir_abs = project_root_path
        .join(".grove")
        .join("agents")
        .join(name);

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
    match launch_detached(&spec) {
        Ok(session_name) => {
            println!(
                "{} launched tmux session {} ({})",
                "✓".green(),
                session_name.bold(),
                cmd_tokens.join(" ")
            );
            println!(
                "  attach: {}",
                crate::session::tmux::attach_instructions(name)
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
        "Next: edit PROMPT.md / STATE.md, then flip loop.md `active: true` to start the loop.".dimmed()
    );
    let _ = PathBuf::from(&agent_toml);
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
