// `grove attach <name>` — re-attach the user's terminal to a running agent's
// tmux session inside the devcontainer.
//
// Equivalent of the host command:
//   devcontainer exec --workspace-folder <root> -- tmux attach -t grove-<n>
//
// Preconditions enforced (each emits a clear actionable error):
//   1. We're inside a grove-initialized project.
//   2. The devcontainer is already running. We do NOT auto-up — attach is
//      a read-style command and shouldn't trigger a 30-60s container boot
//      as a side effect; user should explicitly `grove devcontainer up`.
//   3. A tmux session named `grove-<n>` exists in the container.
//
// Stdio is inherited so tmux's interactive UI works correctly when the
// parent shell has a TTY.

use std::path::Path;

use colored::Colorize;

use crate::git::worktree_manager::{discover_repo, project_root};
use crate::models::GroveConfig;
use crate::session::container::{self, ContainerInfo};
use crate::session::tmux;

pub fn run(name: &str) {
    let ctx = match discover_repo() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} {}", "Error:".red(), e);
            std::process::exit(1);
        }
    };
    let project_root_path = project_root(&ctx).to_path_buf();

    // Refuse to auto-up. Attach must be cheap.
    if !container::is_up(&project_root_path) {
        eprintln!(
            "{} devcontainer is not running. Run `grove devcontainer up` first.",
            "Error:".red()
        );
        std::process::exit(1);
    }

    let info = match container_info_from_config(&project_root_path) {
        Some(i) => i,
        None => {
            eprintln!(
                "{} could not resolve container info from .grove/config.toml. Run `grove devcontainer doctor` to diagnose.",
                "Error:".red()
            );
            std::process::exit(1);
        }
    };

    let session_name = tmux::session_name(name);
    match tmux::has_session(&session_name, Some(&info)) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!(
                "{} no live tmux session {} in the container. Run `grove agents list` to see what's running, or `grove spawn {}` to start it.",
                "Error:".red(),
                session_name,
                name
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("{} probe tmux: {}", "Error:".red(), e);
            std::process::exit(1);
        }
    }

    // Hand over to tmux. exec_streaming inherits the user's TTY so tmux's
    // interactive UI works. The process call doesn't return until the user
    // detaches; we propagate the exit code.
    let argv: Vec<&str> = vec!["tmux", "attach", "-t", &session_name];
    match container::exec_streaming(&info, &argv) {
        Ok(status) => {
            if !status.success() {
                let code = status.code().unwrap_or(1);
                std::process::exit(code);
            }
        }
        Err(e) => {
            eprintln!("{} attach: {}", "Error:".red(), e);
            std::process::exit(1);
        }
    }
}

/// Read `.grove/config.toml` to build a ContainerInfo. Returns None if the
/// file is missing/malformed — caller errors out with an actionable message.
fn container_info_from_config(project_root_path: &Path) -> Option<ContainerInfo> {
    let cfg_path = project_root_path.join(".grove").join("config.toml");
    let raw = std::fs::read_to_string(&cfg_path).ok()?;
    let cfg: GroveConfig = toml::from_str(&raw).ok()?;
    let workspace_target = cfg
        .devcontainer
        .workspace_target
        .unwrap_or_else(|| format!("/workspaces/{}", basename(project_root_path)));
    Some(ContainerInfo::new(
        project_root_path.to_path_buf(),
        std::path::PathBuf::from(workspace_target),
        cfg.devcontainer.remote_user,
    ))
}

fn basename(p: &Path) -> String {
    p.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace")
        .to_string()
}
