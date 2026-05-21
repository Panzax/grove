// tmux-based session backend for `grove spawn`.
//
// Each operation takes `Option<&ContainerInfo>`:
//   - None       → tmux runs on the host (legacy behavior, used when
//                  `[devcontainer] enabled = false`).
//   - Some(info) → tmux runs inside the project's devcontainer via
//                  `devcontainer exec --workspace-folder <root> -- tmux ...`.
//
// The session name we use is `grove-<agent-name>` so it's easy to spot in
// `tmux ls` alongside whatever the user is running elsewhere.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::session::container::{self, ContainerInfo};

pub const SESSION_PREFIX: &str = "grove-";

#[derive(Debug, Clone)]
pub struct SessionSpec<'a> {
    pub name: &'a str,
    pub workdir: &'a Path,
    pub env: HashMap<String, String>,
    /// Command executed inside the session. Typically `claude` or
    /// `claude --dangerously-skip-permissions`. The default value is just
    /// `bash` so tests can exercise the launcher without a real Claude
    /// binary on PATH.
    pub command: Vec<String>,
}

pub fn session_name(agent: &str) -> String {
    format!("{}{}", SESSION_PREFIX, agent)
}

/// Returns Ok(()) if tmux is callable on the chosen target (host or container).
pub fn ensure_tmux_available(container: Option<&ContainerInfo>) -> Result<(), String> {
    let output = run_tmux(container, &["-V"])?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "tmux not available: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Launch a detached session. Idempotent: if a session of this name already
/// exists, return Ok(name) without creating a new one. When `container` is
/// Some, the tmux invocation runs inside the devcontainer and any paths in
/// `spec` are translated to container-side paths first.
pub fn launch_detached(
    spec: &SessionSpec<'_>,
    container: Option<&ContainerInfo>,
) -> Result<String, String> {
    ensure_tmux_available(container)?;
    let name = session_name(spec.name);
    if has_session(&name, container)? {
        return Ok(name);
    }

    // Translate the workdir to a container-side path when targeting a container.
    let workdir_container = translate_path(container, spec.workdir)?;

    // Build the tmux argv (`new-session -d -s <n> -c <workdir>` + `-e KEY=VAL`s
    // + the command tokens). When we run inside the container, env values that
    // reference the workspace root also need translation.
    let mut argv: Vec<String> = vec![
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        name.clone(),
        "-c".into(),
        workdir_container.to_string_lossy().to_string(),
    ];

    for (k, v) in &spec.env {
        let translated = translate_env_value(container, v);
        argv.push("-e".into());
        argv.push(format!("{}={}", k, translated));
    }
    for tok in &spec.command {
        argv.push(tok.clone());
    }

    let argv_refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    let output = run_tmux(container, &argv_refs)?;
    if !output.status.success() {
        return Err(format!(
            "tmux new-session failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(name)
}

/// Check whether a named session is alive on the chosen target.
pub fn has_session(name: &str, container: Option<&ContainerInfo>) -> Result<bool, String> {
    let out = run_tmux(container, &["has-session", "-t", name])?;
    Ok(out.status.success())
}

/// List every grove-managed session (those prefixed with SESSION_PREFIX).
pub fn list_grove_sessions(container: Option<&ContainerInfo>) -> Result<Vec<String>, String> {
    let out = run_tmux(container, &["list-sessions", "-F", "#{session_name}"])?;
    if !out.status.success() {
        // `tmux list-sessions` returns non-zero when the server isn't running.
        return Ok(Vec::new());
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    Ok(raw
        .lines()
        .filter(|l| l.starts_with(SESSION_PREFIX))
        .map(|s| s.to_string())
        .collect())
}

/// Send SIGTERM-equivalent via tmux. Idempotent: missing session = Ok(()).
/// Mirror a tmux session's pane output to a file via `tmux pipe-pane`.
/// Unlike piping the launched command through `tee`, this keeps claude's
/// stdout as a real PTY (required for TUI rendering) while still
/// archiving everything it draws to the log.
///
/// `log_path` is interpreted INSIDE the container (or on the host if
/// `container` is None). Caller is responsible for ensuring the path is
/// container-side when targeting a container.
///
/// `-o` toggles only-when-not-piped, meaning calling this twice is
/// effectively idempotent (second call is a no-op while the first is
/// active). Best-effort: returns Err on failure but doesn't kill the
/// session.
pub fn pipe_pane_to_log(
    name: &str,
    log_path: &str,
    container: Option<&ContainerInfo>,
) -> Result<(), String> {
    // `mkdir -p` defends against the log dir not yet existing inside the
    // container's view (host workspace mounts are usually fine, but
    // belt+suspenders). `cat >>` (NOT >) appends so re-launching an
    // agent with the same name preserves history.
    let shell_cmd = format!(
        "mkdir -p \"$(dirname {log})\" && cat >> {log}",
        log = shell_quote(log_path)
    );
    let target = format!("{}:", name); // session target, default window/pane
    let out = run_tmux(container, &["pipe-pane", "-o", "-t", &target, &shell_cmd])?;
    if !out.status.success() {
        return Err(format!(
            "tmux pipe-pane: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Minimal POSIX shell-escape for a single argument embedded in a shell
/// command string. Wraps in single quotes when the value contains
/// anything beyond identifier-safe characters.
fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_' | '=' | ':'))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

pub fn kill_session(name: &str, container: Option<&ContainerInfo>) -> Result<(), String> {
    if !has_session(name, container)? {
        return Ok(());
    }
    let out = run_tmux(container, &["kill-session", "-t", name])?;
    if !out.status.success() {
        return Err(format!(
            "tmux kill-session: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Print attach instructions for the user. Container target wraps the
/// command in `devcontainer exec --workspace-folder <root> --`.
pub fn attach_instructions(agent: &str, container: Option<&ContainerInfo>) -> String {
    let name = session_name(agent);
    match container {
        None => format!("tmux attach -t {}", name),
        Some(info) => format!(
            "devcontainer exec --workspace-folder {} -- tmux attach -t {}",
            info.workspace_root.display(),
            name
        ),
    }
}

// =============================================================================
// Internals
// =============================================================================

/// Run a tmux command on the chosen target. With `Some(container)`, wraps in
/// `devcontainer exec`. With `None`, runs `tmux` directly on the host.
fn run_tmux(container: Option<&ContainerInfo>, args: &[&str]) -> Result<Output, String> {
    match container {
        None => Command::new("tmux")
            .args(args)
            .output()
            .map_err(|e| format!("tmux: {}", e)),
        Some(info) => {
            let mut wrapped: Vec<&str> = vec!["tmux"];
            wrapped.extend_from_slice(args);
            container::exec(info, &wrapped)
        }
    }
}

/// Translate a host path to its container-side equivalent when targeting a
/// container. Passes through unchanged for host targets.
fn translate_path(container: Option<&ContainerInfo>, host: &Path) -> Result<PathBuf, String> {
    match container {
        None => Ok(host.to_path_buf()),
        Some(info) => container::host_to_container_path(info, host),
    }
}

/// Translate an env-var VALUE that may contain a host path under
/// `workspace_root`. If the value starts with the workspace_root prefix, swap
/// in workspace_target. Otherwise return the value verbatim.
fn translate_env_value(container: Option<&ContainerInfo>, value: &str) -> String {
    let Some(info) = container else {
        return value.to_string();
    };
    let root = info.workspace_root.to_string_lossy().to_string();
    if let Some(rest) = value.strip_prefix(&root) {
        let target = info.workspace_target.to_string_lossy().to_string();
        return format!("{}{}", target, rest);
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctr() -> ContainerInfo {
        ContainerInfo::new(
            PathBuf::from("/home/me/proj"),
            PathBuf::from("/workspaces/proj"),
            "vscode".to_string(),
        )
    }

    #[test]
    fn session_name_uses_prefix() {
        assert_eq!(session_name("feat-a"), "grove-feat-a");
    }

    #[test]
    fn attach_instructions_host_form() {
        let s = attach_instructions("feat-a", None);
        assert!(s.contains("grove-feat-a"));
        assert!(s.starts_with("tmux attach"));
    }

    #[test]
    fn attach_instructions_container_form() {
        let info = ctr();
        let s = attach_instructions("feat-a", Some(&info));
        assert!(s.contains("devcontainer exec --workspace-folder /home/me/proj"));
        assert!(s.contains("tmux attach -t grove-feat-a"));
    }

    #[test]
    fn translate_env_value_passes_through_when_no_container() {
        let v = translate_env_value(None, "/home/me/proj/foo");
        assert_eq!(v, "/home/me/proj/foo");
    }

    #[test]
    fn translate_env_value_swaps_workspace_prefix() {
        let info = ctr();
        let v = translate_env_value(Some(&info), "/home/me/proj/.grove/agents/feat-a");
        assert_eq!(v, "/workspaces/proj/.grove/agents/feat-a");
    }

    #[test]
    fn translate_env_value_leaves_non_workspace_paths_alone() {
        let info = ctr();
        let v = translate_env_value(Some(&info), "/tmp/elsewhere");
        assert_eq!(v, "/tmp/elsewhere");
    }

    /// This test runs only if tmux is present on the test host. It exercises
    /// the full create/list/kill round-trip against a real tmux server (host
    /// target only — we don't spin up a real devcontainer in unit tests).
    #[test]
    fn round_trip_against_real_tmux_host_target() {
        if ensure_tmux_available(None).is_err() {
            eprintln!("skipping: tmux not installed");
            return;
        }
        let tmpdir = std::env::temp_dir();
        let unique = format!(
            "test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        );
        let mut env = HashMap::new();
        env.insert("GROVE_AGENT_DIR".into(), "/tmp/nonexistent".into());
        let spec = SessionSpec {
            name: &unique,
            workdir: &tmpdir,
            env,
            command: vec!["sleep".into(), "30".into()],
        };
        let name = launch_detached(&spec, None).unwrap();
        assert!(has_session(&name, None).unwrap());
        // Idempotency.
        let name2 = launch_detached(&spec, None).unwrap();
        assert_eq!(name2, name);
        // Cleanup.
        kill_session(&name, None).unwrap();
        assert!(!has_session(&name, None).unwrap());
    }
}
