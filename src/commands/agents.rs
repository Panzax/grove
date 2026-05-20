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
use crate::models::AgentMetadata;
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
    // attached or detached.
    let live_sessions: std::collections::HashSet<String> = tmux::list_grove_sessions()
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
    let attached = tmux::has_session(&session_name).unwrap_or(false);
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
    println!(
        "  spawned_at    : {}",
        row.metadata.spawned_at.to_rfc3339()
    );
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
    println!("  attach        : {}", tmux::attach_instructions(name));
}

pub fn kill(name: &str) {
    let session_name = tmux::session_name(name);
    match tmux::kill_session(&session_name) {
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
            println!(
                "  {} flipped loop.md active -> false",
                "·".dimmed()
            );
        }
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
    for entry in fs::read_dir(agents_dir).map_err(|e| format!("read {}: {}", agents_dir.display(), e))? {
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
    let metadata: AgentMetadata = toml::from_str(&raw)
        .map_err(|e| format!("parse {}: {}", metadata_path.display(), e))?;

    let loop_path = agent_dir.join("loop.md");
    let loop_state = if loop_path.exists() {
        loop_md::read_loop_md(&loop_path).ok()
    } else {
        None
    };
    let (iteration, max_iterations, completion_promise, active) = match loop_state {
        Some(s) => (s.iteration, s.max_iterations, s.completion_promise, s.active),
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

fn short_branch(branch: &str) -> String {
    let trimmed = branch.strip_prefix("agent/").unwrap_or(branch);
    if trimmed.len() > 10 {
        format!("{}…", &trimmed[..9])
    } else {
        trimmed.to_string()
    }
}
