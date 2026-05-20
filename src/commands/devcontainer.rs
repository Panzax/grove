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
    require_enabled(&root);
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
    require_enabled(&root);
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
    if !cfg.devcontainer.enabled {
        println!(
            "{} devcontainer is disabled in .grove/config.toml [devcontainer] enabled = false",
            "Note:".yellow()
        );
        return;
    }
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
    require_enabled(&root);
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
    require_enabled(&root);
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

pub fn logs() {
    let (_ctx, root) = discover_or_exit();
    require_enabled(&root);
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

fn require_enabled(project_root: &std::path::Path) {
    let cfg = read_config(project_root);
    if !cfg.devcontainer.enabled {
        eprintln!(
            "{} [devcontainer] enabled = false in .grove/config.toml. Edit the file or re-run `grove init --reconfigure` to enable.",
            "Error:".red()
        );
        std::process::exit(1);
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
