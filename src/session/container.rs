// Devcontainer lifecycle + exec wrapper.
//
// One container per grove project, N worktrees inside (see plan doc — the
// freqtrade-proven model). All grove agents share this container.
//
// `devcontainer up` / `exec` / `down` are shelled out via the `devcontainer`
// CLI (https://github.com/devcontainers/cli). We don't talk to the Docker
// daemon directly so the same module works against docker, podman, or any
// other backend the CLI supports.
//
// The `GROVE_DEVCONTAINER_COMMAND` env var lets tests substitute a stub for
// the real `devcontainer` binary. Same pattern as `GROVE_AGENT_COMMAND` and
// `GROVE_RESOLVE_COMMAND` elsewhere in the codebase.

#![allow(dead_code)] // wired into commands by later commits

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output};

/// Result of `devcontainer up` — captures the container ID + the host →
/// container path mapping needed to translate paths for `tmux -c` and the
/// `GROVE_AGENT_DIR` env var.
#[derive(Debug, Clone)]
pub struct ContainerInfo {
    /// Host filesystem path of the project root (passed as `--workspace-folder`).
    pub workspace_root: PathBuf,
    /// Container-side mount target the project root appears at.
    /// E.g., host `/home/martin/proj` → container `/workspaces/proj`.
    pub workspace_target: PathBuf,
    /// Container user, matches devcontainer.json `remoteUser`.
    pub remote_user: String,
}

impl ContainerInfo {
    pub fn new(
        workspace_root: PathBuf,
        workspace_target: PathBuf,
        remote_user: String,
    ) -> Self {
        Self {
            workspace_root,
            workspace_target,
            remote_user,
        }
    }
}

/// Bring the project's devcontainer up. Idempotent — calling twice returns
/// Ok without re-creating. Reads `workspace_target` and `remote_user` from
/// the project's `.grove/config.toml` first, then falls back to whatever the
/// `devcontainer up` JSON output reports.
///
/// On success, exports `GROVE_CONTAINER_UP=1` so subsequent calls in the
/// same process can skip the up-probe entirely.
pub fn ensure_up(project_root: &Path) -> Result<ContainerInfo, String> {
    let (workspace_target_hint, remote_user_hint) = read_config_hints(project_root);

    let argv = base_argv();
    let cmd = argv.first().cloned().unwrap_or_else(default_command);
    let extra_args = argv.iter().skip(1).cloned().collect::<Vec<_>>();

    let mut command = Command::new(&cmd);
    command
        .args(&extra_args)
        .arg("up")
        .arg("--workspace-folder")
        .arg(project_root);

    let output = command
        .output()
        .map_err(|e| format!("invoke `{}`: {}", cmd, e))?;
    if !output.status.success() {
        return Err(format!(
            "devcontainer up failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    // `devcontainer up` prints a JSON object on stdout describing the result.
    // Fields we care about (when present): `outcome`, `containerId`,
    // `remoteUser`, `composeProjectName`. The CLI version varies; we
    // tolerate missing fields and fall back to config hints.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or(serde_json::Value::Null);

    let remote_user = parsed
        .get("remoteUser")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or(remote_user_hint)
        .unwrap_or_else(|| "vscode".to_string());

    // workspaceFolder isn't always in `up`'s output; rely on the config hint
    // (which init.rs populates from devcontainer.json's workspaceFolder).
    // Final fallback: /workspaces/<basename>.
    let workspace_target = workspace_target_hint
        .map(PathBuf::from)
        .unwrap_or_else(|| default_workspace_target(project_root));

    std::env::set_var("GROVE_CONTAINER_UP", "1");

    Ok(ContainerInfo {
        workspace_root: project_root.to_path_buf(),
        workspace_target,
        remote_user,
    })
}

/// Stop and remove the project's devcontainer.
pub fn down(project_root: &Path) -> Result<(), String> {
    let argv = base_argv();
    let cmd = argv.first().cloned().unwrap_or_else(default_command);
    let extra_args = argv.iter().skip(1).cloned().collect::<Vec<_>>();

    let output = Command::new(&cmd)
        .args(&extra_args)
        .arg("down")
        .arg("--workspace-folder")
        .arg(project_root)
        .output()
        .map_err(|e| format!("invoke `{}`: {}", cmd, e))?;
    if !output.status.success() {
        return Err(format!(
            "devcontainer down failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    std::env::remove_var("GROVE_CONTAINER_UP");
    Ok(())
}

/// Best-effort check that the container is currently up. Returns false on
/// any error (including "devcontainer CLI not installed") so callers can
/// gracefully degrade.
pub fn is_up(project_root: &Path) -> bool {
    let argv = base_argv();
    let cmd = match argv.first() {
        Some(c) => c.clone(),
        None => return false,
    };
    let extra_args = argv.iter().skip(1).cloned().collect::<Vec<_>>();

    // Cheapest probe: `devcontainer exec --workspace-folder <root> -- true`.
    // If the container is up it succeeds quickly; if not it errors.
    Command::new(&cmd)
        .args(&extra_args)
        .arg("exec")
        .arg("--workspace-folder")
        .arg(project_root)
        .arg("--")
        .arg("true")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Run a command in the project's devcontainer. Captures output.
pub fn exec(info: &ContainerInfo, argv: &[&str]) -> Result<Output, String> {
    let dc_argv = base_argv();
    let cmd = dc_argv.first().cloned().unwrap_or_else(default_command);
    let extra_args = dc_argv.iter().skip(1).cloned().collect::<Vec<_>>();

    let output = Command::new(&cmd)
        .args(&extra_args)
        .arg("exec")
        .arg("--workspace-folder")
        .arg(&info.workspace_root)
        .arg("--")
        .args(argv)
        .output()
        .map_err(|e| format!("invoke `{}`: {}", cmd, e))?;
    Ok(output)
}

/// Run a command in the container with stdio inherited (no capture).
pub fn exec_streaming(info: &ContainerInfo, argv: &[&str]) -> Result<ExitStatus, String> {
    let dc_argv = base_argv();
    let cmd = dc_argv.first().cloned().unwrap_or_else(default_command);
    let extra_args = dc_argv.iter().skip(1).cloned().collect::<Vec<_>>();

    let status = Command::new(&cmd)
        .args(&extra_args)
        .arg("exec")
        .arg("--workspace-folder")
        .arg(&info.workspace_root)
        .arg("--")
        .args(argv)
        .status()
        .map_err(|e| format!("invoke `{}`: {}", cmd, e))?;
    Ok(status)
}

/// Translate a host-side path under `workspace_root` to its container-side
/// equivalent under `workspace_target`. Returns Err if the path is outside
/// the workspace root.
pub fn host_to_container_path(
    info: &ContainerInfo,
    host_path: &Path,
) -> Result<PathBuf, String> {
    let host_canon = host_path
        .canonicalize()
        .unwrap_or_else(|_| host_path.to_path_buf());
    let root_canon = info
        .workspace_root
        .canonicalize()
        .unwrap_or_else(|_| info.workspace_root.clone());
    let relative = host_canon.strip_prefix(&root_canon).map_err(|_| {
        format!(
            "path {} is not under workspace root {}",
            host_path.display(),
            info.workspace_root.display()
        )
    })?;
    if relative.as_os_str().is_empty() {
        Ok(info.workspace_target.clone())
    } else {
        Ok(info.workspace_target.join(relative))
    }
}

/// Print attach instructions for a session inside the container.
pub fn attach_instructions(info: &ContainerInfo, session_name: &str) -> String {
    format!(
        "{} {} -- tmux attach -t {}",
        default_command(),
        format_args!(
            "exec --workspace-folder {}",
            info.workspace_root.display()
        ),
        session_name
    )
}

// =============================================================================
// Internals
// =============================================================================

/// Default command. Tests override via `GROVE_DEVCONTAINER_COMMAND`. The
/// env var contents are split on whitespace, so a value of `bash -c stub.sh`
/// becomes ["bash", "-c", "stub.sh"].
fn base_argv() -> Vec<String> {
    if let Ok(raw) = std::env::var("GROVE_DEVCONTAINER_COMMAND") {
        let tokens: Vec<String> = raw.split_whitespace().map(|s| s.to_string()).collect();
        if !tokens.is_empty() {
            return tokens;
        }
    }
    vec![default_command()]
}

fn default_command() -> String {
    "devcontainer".to_string()
}

/// Last-resort workspace_target guess: `/workspaces/<basename>`. Matches the
/// default `workspaceFolder` of the Microsoft devcontainers base images.
fn default_workspace_target(project_root: &Path) -> PathBuf {
    let basename = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace");
    PathBuf::from("/workspaces").join(basename)
}

/// Read `[devcontainer] workspace_target` + `[devcontainer] remote_user` from
/// the project's config without panicking on missing/malformed files.
fn read_config_hints(project_root: &Path) -> (Option<String>, Option<String>) {
    let path = project_root.join(".grove").join("config.toml");
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return (None, None),
    };
    let cfg: crate::models::GroveConfig = match toml::from_str(&raw) {
        Ok(c) => c,
        Err(_) => return (None, None),
    };
    (cfg.devcontainer.workspace_target, Some(cfg.devcontainer.remote_user))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(root: &str, target: &str) -> ContainerInfo {
        ContainerInfo::new(
            PathBuf::from(root),
            PathBuf::from(target),
            "vscode".to_string(),
        )
    }

    #[test]
    fn host_to_container_path_strips_root() {
        let i = info("/home/martin/proj", "/workspaces/proj");
        let mapped =
            host_to_container_path(&i, Path::new("/home/martin/proj/worktrees/feat-a")).unwrap();
        assert_eq!(mapped, PathBuf::from("/workspaces/proj/worktrees/feat-a"));
    }

    #[test]
    fn host_to_container_path_root_itself() {
        let i = info("/home/martin/proj", "/workspaces/proj");
        let mapped = host_to_container_path(&i, Path::new("/home/martin/proj")).unwrap();
        assert_eq!(mapped, PathBuf::from("/workspaces/proj"));
    }

    #[test]
    fn host_to_container_path_rejects_outside_root() {
        let i = info("/home/martin/proj", "/workspaces/proj");
        let err = host_to_container_path(&i, Path::new("/tmp/other")).unwrap_err();
        assert!(err.contains("not under workspace root"));
    }

    #[test]
    fn default_workspace_target_uses_basename() {
        let target = default_workspace_target(Path::new("/home/martin/foo-bar"));
        assert_eq!(target, PathBuf::from("/workspaces/foo-bar"));
    }

    #[test]
    fn attach_instructions_format() {
        let i = info("/home/martin/proj", "/workspaces/proj");
        let s = attach_instructions(&i, "grove-feat-a");
        assert!(s.contains("devcontainer"));
        assert!(s.contains("exec --workspace-folder /home/martin/proj"));
        assert!(s.contains("tmux attach -t grove-feat-a"));
    }

    #[test]
    fn env_override_redirects_commands() {
        // Saturate the env override; ensure base_argv() picks it up.
        std::env::set_var("GROVE_DEVCONTAINER_COMMAND", "echo --");
        let argv = base_argv();
        std::env::remove_var("GROVE_DEVCONTAINER_COMMAND");
        assert_eq!(argv, vec!["echo".to_string(), "--".to_string()]);
    }

    #[test]
    fn is_up_returns_false_when_command_missing() {
        // Point GROVE_DEVCONTAINER_COMMAND at a binary that doesn't exist.
        std::env::set_var(
            "GROVE_DEVCONTAINER_COMMAND",
            "/nonexistent/grove-test-no-such-cmd",
        );
        let up = is_up(Path::new("/tmp"));
        std::env::remove_var("GROVE_DEVCONTAINER_COMMAND");
        assert!(!up);
    }
}
