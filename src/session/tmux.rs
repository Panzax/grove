// tmux-based session backend for `grove spawn`.
//
// All operations shell out to the `tmux` CLI. The session name we use is
// `grove-<agent-name>` so it's easy to spot in `tmux ls` alongside whatever
// the user is running elsewhere.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

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

/// Returns Ok(()) if tmux is callable, Err otherwise.
pub fn ensure_tmux_available() -> Result<(), String> {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|_| ())
        .map_err(|e| format!("tmux not available: {}", e))
}

/// Launch a detached session. Idempotent: if a session of this name already
/// exists, return Ok(name) without creating a new one.
pub fn launch_detached(spec: &SessionSpec<'_>) -> Result<String, String> {
    ensure_tmux_available()?;
    let name = session_name(spec.name);
    if has_session(&name)? {
        return Ok(name);
    }

    let mut cmd = Command::new("tmux");
    cmd.args(["new-session", "-d", "-s", &name, "-c"])
        .arg(spec.workdir);
    for (k, v) in &spec.env {
        cmd.args(["-e", &format!("{}={}", k, v)]);
    }
    // Append the command tokens last; tmux joins them with spaces (no shell).
    for tok in &spec.command {
        cmd.arg(tok);
    }

    let output = cmd
        .output()
        .map_err(|e| format!("tmux new-session failed: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "tmux new-session failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(name)
}

/// Check whether a named session is alive.
pub fn has_session(name: &str) -> Result<bool, String> {
    let out = Command::new("tmux")
        .args(["has-session", "-t", name])
        .output()
        .map_err(|e| format!("tmux has-session: {}", e))?;
    Ok(out.status.success())
}

/// List every grove-managed session (those prefixed with SESSION_PREFIX).
pub fn list_grove_sessions() -> Result<Vec<String>, String> {
    let out = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .map_err(|e| format!("tmux list-sessions: {}", e))?;
    if !out.status.success() {
        // `tmux list-sessions` returns non-zero when the server isn't running.
        return Ok(Vec::new());
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let names: Vec<String> = raw
        .lines()
        .filter(|l| l.starts_with(SESSION_PREFIX))
        .map(|s| s.to_string())
        .collect();
    Ok(names)
}

/// Send SIGTERM-equivalent via tmux. Idempotent: missing session = Ok(()).
pub fn kill_session(name: &str) -> Result<(), String> {
    if !has_session(name)? {
        return Ok(());
    }
    let out = Command::new("tmux")
        .args(["kill-session", "-t", name])
        .output()
        .map_err(|e| format!("tmux kill-session: {}", e))?;
    if !out.status.success() {
        return Err(format!(
            "tmux kill-session: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Print attach instructions instead of execing tmux directly — Claude Code's
/// hook environment can't usefully take over the user's terminal.
pub fn attach_instructions(agent: &str) -> String {
    let name = session_name(agent);
    format!("tmux attach -t {}", name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_name_uses_prefix() {
        assert_eq!(session_name("feat-a"), "grove-feat-a");
    }

    #[test]
    fn attach_instructions_includes_session() {
        let s = attach_instructions("feat-a");
        assert!(s.contains("grove-feat-a"));
        assert!(s.starts_with("tmux attach"));
    }

    /// This test runs only if tmux is present on the test host. It exercises the
    /// full create/list/kill round-trip against a real tmux server.
    #[test]
    fn round_trip_against_real_tmux() {
        if ensure_tmux_available().is_err() {
            eprintln!("skipping: tmux not installed");
            return;
        }
        let tmpdir = std::env::temp_dir();
        let unique = format!("test-{}-{}", std::process::id(), uuid::Uuid::new_v4().simple());
        let mut env = HashMap::new();
        env.insert("GROVE_AGENT_DIR".into(), "/tmp/nonexistent".into());
        let spec = SessionSpec {
            name: &unique,
            workdir: &tmpdir,
            env,
            command: vec!["sleep".into(), "30".into()],
        };
        let name = launch_detached(&spec).unwrap();
        assert!(has_session(&name).unwrap());
        // Idempotency.
        let name2 = launch_detached(&spec).unwrap();
        assert_eq!(name2, name);
        // Cleanup.
        kill_session(&name).unwrap();
        assert!(!has_session(&name).unwrap());
    }
}
