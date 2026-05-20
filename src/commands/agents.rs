// `grove agents <list|status|kill>` — manage live grove-spawned agents.
//
// Source of truth is `.grove/agents/<name>/{agent.toml, loop.md, STATE.md}`
// plus the live tmux session list. Each `grove agents list` row crosses
// disk state with tmux state so the user can see "this agent's loop is on
// iter 4, status running, tmux session attached".

use std::fs;
use std::path::Path;

use colored::Colorize;

use crate::agent::loop_md;
use crate::git::worktree_manager::{discover_repo, project_root};
use crate::models::{AgentMetadata, GroveConfig};
use crate::session::container::{self, ContainerInfo};
use crate::session::tmux;

pub fn list() {
    let ctx = match discover_repo() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} {}", "Error:".red(), e);
            std::process::exit(1);
        }
    };
    let agents_dir = project_root(&ctx).join(".grove").join("agents");
    let entries = match collect_agents(&agents_dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("{} {}", "Error:".red(), e);
            std::process::exit(1);
        }
    };
    if entries.is_empty() {
        println!("(no agents found in {})", agents_dir.display());
        return;
    }
    // Cross-reference with live tmux sessions so we can mark each agent
    // attached or detached. When [devcontainer] enabled, query in-container
    // tmux; when disabled or unreachable, fall back to host tmux.
    let project_root_path = project_root(&ctx).to_path_buf();
    let container = resolve_container_for_query(&project_root_path);
    let live_sessions: std::collections::HashSet<String> =
        tmux::list_grove_sessions(container.as_ref())
            .unwrap_or_default()
            .into_iter()
            .collect();

    println!(
        "{:<24} {:<10} {:<5} {:<8} {:<12} TASK",
        "NAME".bold(),
        "BRANCH".bold(),
        "ITER".bold(),
        "MAX".bold(),
        "STATUS".bold()
    );
    for row in entries {
        let status_word = row.loop_status();
        let session_name = tmux::session_name(&row.metadata.name);
        let attached = if live_sessions.contains(&session_name) {
            "•".green().to_string()
        } else {
            "·".dimmed().to_string()
        };
        let task = row.metadata.task.clone().unwrap_or_else(|| "—".into());
        println!(
            "{}{:<22} {:<10} {:>5} {:>8} {:<12} {}",
            attached,
            row.metadata.name,
            short_branch(&row.metadata.branch),
            row.iteration,
            row.max_iterations,
            status_word,
            task
        );
    }
}

pub fn status(name: &str) {
    let ctx = match discover_repo() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} {}", "Error:".red(), e);
            std::process::exit(1);
        }
    };
    let agents_dir = project_root(&ctx).join(".grove").join("agents");
    let dir = agents_dir.join(name);
    if !dir.exists() {
        eprintln!(
            "{} no agent named {}: dir {} does not exist",
            "Error:".red(),
            name,
            dir.display()
        );
        std::process::exit(1);
    }
    let row = match load_agent(&dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{} {}", "Error:".red(), e);
            std::process::exit(1);
        }
    };
    let session_name = tmux::session_name(name);
    let project_root_path = project_root(&ctx).to_path_buf();
    let container = resolve_container_for_query(&project_root_path);
    let attached = tmux::has_session(&session_name, container.as_ref()).unwrap_or(false);
    println!(
        "{} ({})",
        row.metadata.name.bold(),
        row.metadata.id.dimmed()
    );
    println!("  branch        : {}", row.metadata.branch);
    println!("  worktree      : {}", row.metadata.worktree);
    println!(
        "  task          : {}",
        row.metadata.task.as_deref().unwrap_or("—")
    );
    println!("  spawned_at    : {}", row.metadata.spawned_at.to_rfc3339());
    println!("  loop status   : {}", row.loop_status());
    println!(
        "  iteration     : {} / {}",
        row.iteration, row.max_iterations
    );
    println!(
        "  promise       : {}",
        if row.completion_promise.is_empty() {
            "(unset)".dimmed().to_string()
        } else {
            row.completion_promise.clone()
        }
    );
    println!(
        "  tmux session  : {} ({})",
        session_name,
        if attached {
            "attached".green().to_string()
        } else {
            "detached".dimmed().to_string()
        }
    );
    println!(
        "  attach        : {}",
        tmux::attach_instructions(name, container.as_ref())
    );
}

pub fn kill(name: &str) {
    let session_name = tmux::session_name(name);
    let project_root_path = discover_repo().ok().map(|c| project_root(&c).to_path_buf());
    let container = project_root_path
        .as_deref()
        .and_then(resolve_container_for_query);
    match tmux::kill_session(&session_name, container.as_ref()) {
        Ok(()) => println!("{} killed {}", "✓".green(), session_name),
        Err(e) => {
            eprintln!("{} {}", "Error:".red(), e);
            std::process::exit(1);
        }
    }

    // Best-effort: flip loop.md active -> false so the loop doesn't auto-resume
    // if someone re-launches the session.
    let ctx = match discover_repo() {
        Ok(c) => c,
        Err(_) => return,
    };
    let loop_path = project_root(&ctx)
        .join(".grove")
        .join("agents")
        .join(name)
        .join("loop.md");
    if !loop_path.exists() {
        return;
    }
    if let Ok(mut state) = loop_md::read_loop_md(&loop_path) {
        if state.active {
            state.active = false;
            let _ = loop_md::write_loop_md(&loop_path, &state);
            println!("  {} flipped loop.md active -> false", "·".dimmed());
        }
    }
}

/// `grove agents purge <name>` — fully delete an agent's state.
///
/// `grove remove <name>` removes the worktree but keeps `.grove/agents/<n>/`
/// (intentional — survives crashes so resume works). `purge` deletes the
/// agent dir + agent.toml so a subsequent `grove spawn <n>` starts FRESH
/// instead of resuming. Refuses if the tmux session is still alive.
pub fn purge(name: &str) {
    let ctx = match discover_repo() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} {}", "Error:".red(), e);
            std::process::exit(1);
        }
    };
    let project_root_path = project_root(&ctx).to_path_buf();
    let session_name = tmux::session_name(name);
    let container = resolve_container_for_query(&project_root_path);
    if tmux::has_session(&session_name, container.as_ref()).unwrap_or(false) {
        eprintln!(
            "{} agent '{}' is still running (tmux session {}). Run `grove agents kill {}` first.",
            "Error:".red(),
            name,
            session_name,
            name
        );
        std::process::exit(1);
    }
    let agent_dir = project_root_path.join(".grove").join("agents").join(name);
    if !agent_dir.exists() {
        println!(
            "{} no agent state at {} — nothing to purge.",
            "Note:".yellow(),
            agent_dir.display()
        );
        return;
    }
    match fs::remove_dir_all(&agent_dir) {
        Ok(()) => println!(
            "{} purged {} (worktree, if any, was NOT removed — use `grove remove {}`)",
            "✓".green(),
            agent_dir.display(),
            name
        ),
        Err(e) => {
            eprintln!("{} remove {}: {}", "Error:".red(), agent_dir.display(), e);
            std::process::exit(1);
        }
    }
}

/// `grove agents repair-pointers [<name>]` — rewrite a worktree's two `.git`
/// pointer files (forward + back) from absolute → relative paths so they
/// resolve identically on host and inside the devcontainer.
///
/// Without `<name>` repairs every agent that has an `agent.toml` recording
/// a `worktree` path. With `<name>` repairs just that one. Missing worktrees
/// (e.g. user ran `grove remove`) are skipped with a note.
pub fn repair_pointers(name: Option<&str>) {
    let ctx = match discover_repo() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} {}", "Error:".red(), e);
            std::process::exit(1);
        }
    };
    let agents_dir = project_root(&ctx).join(".grove").join("agents");

    let targets: Vec<(String, std::path::PathBuf)> = match name {
        Some(n) => {
            let dir = agents_dir.join(n);
            if !dir.exists() {
                eprintln!(
                    "{} no agent named {}: dir {} does not exist",
                    "Error:".red(),
                    n,
                    dir.display()
                );
                std::process::exit(1);
            }
            match load_agent(&dir) {
                Ok(row) => vec![(row.metadata.name, std::path::PathBuf::from(row.metadata.worktree))],
                Err(e) => {
                    eprintln!("{} {}", "Error:".red(), e);
                    std::process::exit(1);
                }
            }
        }
        None => match collect_agents(&agents_dir) {
            Ok(rows) => rows
                .into_iter()
                .map(|r| {
                    (
                        r.metadata.name,
                        std::path::PathBuf::from(r.metadata.worktree),
                    )
                })
                .collect(),
            Err(e) => {
                eprintln!("{} {}", "Error:".red(), e);
                std::process::exit(1);
            }
        },
    };

    if targets.is_empty() {
        println!("(no agents found in {})", agents_dir.display());
        return;
    }

    let mut fixed = 0u32;
    let mut skipped = 0u32;
    let mut failed = 0u32;
    for (agent_name, worktree_path) in &targets {
        if !worktree_path.exists() {
            println!(
                "  {} {} — worktree {} missing; skipping",
                "·".dimmed(),
                agent_name,
                worktree_path.display()
            );
            skipped += 1;
            continue;
        }
        match crate::git::worktree_paths::make_worktree_pointers_relative(worktree_path) {
            Ok(()) => {
                println!(
                    "  {} {} — pointers relative ({})",
                    "✓".green(),
                    agent_name,
                    worktree_path.display()
                );
                fixed += 1;
            }
            Err(e) => {
                eprintln!(
                    "  {} {} — {}",
                    "✗".red(),
                    agent_name,
                    e
                );
                failed += 1;
            }
        }
    }
    println!(
        "{} repair-pointers: {} fixed, {} skipped, {} failed",
        if failed == 0 { "✓".green() } else { "!".yellow() },
        fixed,
        skipped,
        failed
    );
    if failed > 0 {
        std::process::exit(1);
    }
}

struct AgentRow {
    metadata: AgentMetadata,
    iteration: u32,
    max_iterations: u32,
    completion_promise: String,
    active: bool,
}

impl AgentRow {
    fn loop_status(&self) -> String {
        if self.active {
            "running".green().to_string()
        } else if self.iteration >= self.max_iterations && self.max_iterations > 0 {
            "complete".cyan().to_string()
        } else {
            "paused".yellow().to_string()
        }
    }
}

fn collect_agents(agents_dir: &Path) -> Result<Vec<AgentRow>, String> {
    if !agents_dir.exists() {
        return Ok(Vec::new());
    }
    let mut rows = Vec::new();
    for entry in
        fs::read_dir(agents_dir).map_err(|e| format!("read {}: {}", agents_dir.display(), e))?
    {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if let Ok(row) = load_agent(&path) {
            rows.push(row);
        }
    }
    rows.sort_by(|a, b| a.metadata.name.cmp(&b.metadata.name));
    Ok(rows)
}

fn load_agent(agent_dir: &Path) -> Result<AgentRow, String> {
    let metadata_path = agent_dir.join("agent.toml");
    if !metadata_path.exists() {
        return Err(format!(
            "{} has no agent.toml — probably wasn't created by grove spawn",
            agent_dir.display()
        ));
    }
    let raw = fs::read_to_string(&metadata_path)
        .map_err(|e| format!("read {}: {}", metadata_path.display(), e))?;
    let metadata: AgentMetadata =
        toml::from_str(&raw).map_err(|e| format!("parse {}: {}", metadata_path.display(), e))?;

    let loop_path = agent_dir.join("loop.md");
    let loop_state = if loop_path.exists() {
        loop_md::read_loop_md(&loop_path).ok()
    } else {
        None
    };
    let (iteration, max_iterations, completion_promise, active) = match loop_state {
        Some(s) => (
            s.iteration,
            s.max_iterations,
            s.completion_promise,
            s.active,
        ),
        None => (0, 0, String::new(), false),
    };
    Ok(AgentRow {
        metadata,
        iteration,
        max_iterations,
        completion_promise,
        active,
    })
}

/// Resolve a ContainerInfo for read queries (list/status/kill) WITHOUT
/// bringing the container up. If the container is already running, return
/// Some; otherwise None (read commands report "not running" rather than
/// triggering a 30-60s container boot).
fn resolve_container_for_query(project_root: &Path) -> Option<ContainerInfo> {
    if !container::is_up(project_root) {
        return None;
    }
    let cfg_path = project_root.join(".grove").join("config.toml");
    let raw = std::fs::read_to_string(&cfg_path).ok()?;
    let cfg: GroveConfig = toml::from_str(&raw).ok()?;
    let workspace_target = cfg
        .devcontainer
        .workspace_target
        .unwrap_or_else(|| format!("/workspaces/{}", project_root_basename(project_root)));
    Some(ContainerInfo::new(
        project_root.to_path_buf(),
        std::path::PathBuf::from(workspace_target),
        cfg.devcontainer.remote_user,
    ))
}

fn project_root_basename(p: &Path) -> String {
    p.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace")
        .to_string()
}

fn short_branch(branch: &str) -> String {
    let trimmed = branch.strip_prefix("agent/").unwrap_or(branch);
    if trimmed.len() > 10 {
        format!("{}…", &trimmed[..9])
    } else {
        trimmed.to_string()
    }
}
