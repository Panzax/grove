// `grove devcontainer <up|down|status|exec|rebuild|logs>` — manual
// devcontainer lifecycle. Spawn auto-`up`s; these subcommands are for
// debugging, teardown, and one-off commands inside the container.

use std::path::PathBuf;
use std::process::Command;

use colored::Colorize;

use crate::git::worktree_manager::{discover_repo, project_root};
use crate::models::GroveConfig;
use crate::session::container::{self, ContainerInfo};
use crate::session::tmux;

pub fn up() {
    let (_ctx, root) = discover_or_exit();
    match container::ensure_up(&root) {
        Ok(info) => {
            println!(
                "{} container is up. workspace_target={} remote_user={}",
                "✓".green(),
                info.workspace_target.display(),
                info.remote_user
            );
        }
        Err(e) => {
            eprintln!("{} devcontainer up: {}", "Error:".red(), e);
            std::process::exit(1);
        }
    }
}

pub fn down() {
    let (_ctx, root) = discover_or_exit();
    match container::down(&root) {
        Ok(()) => println!("{} container stopped", "✓".green()),
        Err(e) => {
            eprintln!("{} devcontainer down: {}", "Error:".red(), e);
            std::process::exit(1);
        }
    }
}

pub fn status() {
    let (_ctx, root) = discover_or_exit();
    let cfg = read_config(&root);
    let is_up = container::is_up(&root);
    if !is_up {
        println!(
            "{} container is {} (start with `grove devcontainer up`)",
            "·".dimmed(),
            "DOWN".red()
        );
        return;
    }
    // Build a ContainerInfo from config (we know it's up; cheap).
    let workspace_target = cfg
        .devcontainer
        .workspace_target
        .unwrap_or_else(|| default_workspace_target(&root));
    let info = ContainerInfo::new(
        root.clone(),
        PathBuf::from(workspace_target),
        cfg.devcontainer.remote_user,
    );
    println!(
        "{} container is {}. workspace_target={} remote_user={}",
        "·".dimmed(),
        "UP".green(),
        info.workspace_target.display(),
        info.remote_user
    );
    match tmux::list_grove_sessions(Some(&info)) {
        Ok(sessions) if sessions.is_empty() => {
            println!("  no grove- tmux sessions inside the container.");
        }
        Ok(sessions) => {
            println!("  grove tmux sessions inside the container:");
            for s in sessions {
                println!("    - {}", s);
            }
        }
        Err(e) => {
            eprintln!("  could not query tmux inside container: {}", e);
        }
    }
}

pub fn exec(argv: &[String]) {
    let (_ctx, root) = discover_or_exit();
    if argv.is_empty() {
        eprintln!(
            "{} grove devcontainer exec needs a command (e.g. `grove devcontainer exec bash`)",
            "Error:".red()
        );
        std::process::exit(2);
    }
    let info = match container::ensure_up(&root) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("{} {}", "Error:".red(), e);
            std::process::exit(1);
        }
    };
    let argv_refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    match container::exec_streaming(&info, &argv_refs) {
        Ok(status) => {
            if !status.success() {
                std::process::exit(status.code().unwrap_or(1));
            }
        }
        Err(e) => {
            eprintln!("{} {}", "Error:".red(), e);
            std::process::exit(1);
        }
    }
}

pub fn rebuild() {
    let (_ctx, root) = discover_or_exit();
    // The devcontainer CLI has `--remove-existing-container` for `up` to
    // force a rebuild. Wrap that.
    let output = Command::new("devcontainer")
        .arg("up")
        .arg("--workspace-folder")
        .arg(&root)
        .arg("--remove-existing-container")
        .output();
    match output {
        Ok(out) if out.status.success() => {
            println!("{} container rebuilt", "✓".green());
        }
        Ok(out) => {
            eprintln!(
                "{} rebuild failed: {}",
                "Error:".red(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("{} invoke devcontainer: {}", "Error:".red(), e);
            std::process::exit(1);
        }
    }
}

/// Audit the running container for grove's prereqs. Reports which of
/// `tmux`, `jq`, `perl`, `claude` are present / missing.
///
/// Exits 0 if all present, 1 if any missing. Useful in CI / health checks /
/// before spawning a swarm of agents.
pub fn doctor() {
    let (_ctx, root) = discover_or_exit();
    let _cfg = read_config(&root);
    let info = match container::ensure_up(&root) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("{} devcontainer up: {}", "Error:".red(), e);
            std::process::exit(1);
        }
    };

    println!(
        "{} checking grove prereqs in container ({})...",
        "·".dimmed(),
        info.workspace_target.display()
    );

    let prereqs = [
        ("tmux", "agent session multiplexer"),
        ("jq", "Stop-hook JSON parsing"),
        ("perl", "Stop-hook <promise> extraction"),
        ("claude", "Claude Code CLI (the agent itself)"),
    ];

    let mut missing: Vec<&str> = Vec::new();
    for (tool, reason) in &prereqs {
        let present = is_tool_in_container(&info, tool);
        if present {
            println!("  {} {} present ({})", "✓".green(), tool.bold(), reason);
        } else {
            println!("  {} {} MISSING ({})", "✗".red(), tool.bold(), reason);
            missing.push(*tool);
        }
    }

    // Informational: host tmux config bind mount. Not a prereq, no exit
    // code impact — but useful to know whether the in-container tmux is
    // running with the user's keybinds or stock defaults.
    let has_tmux_conf = file_readable_in_container(&info, "/home/vscode/.tmux.conf");
    if has_tmux_conf {
        println!(
            "  {} {} mounted at /home/vscode/.tmux.conf (in-container tmux inherits host config)",
            "·".dimmed(),
            "tmux.conf".bold()
        );
    } else {
        println!(
            "  {} {} not mounted (stock tmux defaults — see grove init's tmux conf detection)",
            "·".dimmed(),
            "tmux.conf".bold()
        );
    }

    if missing.is_empty() {
        println!();
        println!("{} all grove prereqs are installed", "✓".green());
        return;
    }

    eprintln!();
    eprintln!(
        "{} missing in container: {}",
        "Error:".red(),
        missing.join(", ")
    );
    eprintln!(
        "Fix: ensure your `.devcontainer/devcontainer.json` postCreateCommand installs these,"
    );
    eprintln!("then run `grove devcontainer rebuild` to re-apply.");
    eprintln!("grove's default scaffold installs them via apt-get + npm; if you edited the file,");
    eprintln!("compare against the prereqs line in `src/devcontainer/mod.rs::grove_container_prereqs_command`.");
    std::process::exit(1);
}

fn is_tool_in_container(info: &ContainerInfo, tool: &str) -> bool {
    // `command -v <tool>` exits 0 if the tool is on PATH inside the container.
    let script = format!("command -v {} >/dev/null 2>&1", tool);
    let argv: Vec<&str> = vec!["sh", "-c", &script];
    container::exec(info, &argv)
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn file_readable_in_container(info: &ContainerInfo, path: &str) -> bool {
    let script = format!("test -r {}", path);
    let argv: Vec<&str> = vec!["sh", "-c", &script];
    container::exec(info, &argv)
        .map(|out| out.status.success())
        .unwrap_or(false)
}

pub fn logs() {
    let (_ctx, root) = discover_or_exit();
    let status = Command::new("devcontainer")
        .arg("logs")
        .arg("--workspace-folder")
        .arg(&root)
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => std::process::exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("{} {}", "Error:".red(), e);
            std::process::exit(1);
        }
    }
}

// =============================================================================
// Helpers
// =============================================================================

fn discover_or_exit() -> (crate::git::worktree_manager::RepoContext, PathBuf) {
    match discover_repo() {
        Ok(c) => {
            let root = project_root(&c).to_path_buf();
            (c, root)
        }
        Err(e) => {
            eprintln!("{} {}", "Error:".red(), e);
            std::process::exit(1);
        }
    }
}

fn read_config(project_root: &std::path::Path) -> GroveConfig {
    let path = project_root.join(".grove").join("config.toml");
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    toml::from_str(&raw).unwrap_or_default()
}

fn default_workspace_target(project_root: &std::path::Path) -> String {
    let basename = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace");
    format!("/workspaces/{}", basename)
}
