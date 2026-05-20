// Per-agent profile seeder + asset installer.
//
// Two responsibilities:
//
//   1. `install_assets()` — drop the tracked framework files (PROTOCOL.md,
//      RALPH-LOOP.md, SHARED.md, PROMPT.template.md) into `.grove/` during
//      `grove init`. Idempotent (re-running overwrites the framework files
//      back to the asset versions; SHARED.md is skipped if a project-edited
//      copy exists so user content isn't clobbered).
//
//   2. `seed_agent()` — write `.grove/agents/<name>/{PROMPT,STATE,loop}.md`
//      during `grove spawn`. Substitutes <AGENT_NAME> into PROMPT.md. Refuses
//      to overwrite an existing agent dir so the loop state for an in-flight
//      agent survives `grove remove` + re-`grove spawn`.

use std::fs;
use std::path::{Path, PathBuf};

use crate::git::worktree_manager::{project_root, RepoContext};

pub const PROMPT_TEMPLATE: &str = include_str!("../../assets/PROMPT.template.md");
pub const RALPH_LOOP_MD: &str = include_str!("../../assets/RALPH-LOOP.md");
pub const PROTOCOL_MD: &str = include_str!("../../assets/PROTOCOL.md");
pub const SHARED_MD: &str = include_str!("../../assets/SHARED.md");

/// Install the framework asset files into `<project_root>/.grove/`. Returns the
/// list of paths actually written (skipped files are not included).
pub fn install_assets(ctx: &RepoContext) -> Result<Vec<PathBuf>, String> {
    let root = project_root(ctx).join(".grove");
    fs::create_dir_all(&root).map_err(|e| format!("create .grove/: {}", e))?;
    let mut written = Vec::new();

    let always_overwrite: &[(&str, &str)] = &[
        ("RALPH-LOOP.md", RALPH_LOOP_MD),
        ("PROTOCOL.md", PROTOCOL_MD),
        ("PROMPT.template.md", PROMPT_TEMPLATE),
    ];
    for (name, body) in always_overwrite {
        let path = root.join(name);
        fs::write(&path, body).map_err(|e| format!("write {}: {}", path.display(), e))?;
        written.push(path);
    }

    // SHARED.md: only write the template if no file exists yet. Once a project
    // has edited it, don't blow away their work on re-init.
    let shared = root.join("SHARED.md");
    if !shared.exists() {
        fs::write(&shared, SHARED_MD).map_err(|e| format!("write {}: {}", shared.display(), e))?;
        written.push(shared);
    }

    Ok(written)
}

/// Default `loop.md` body — a small pointer so the Stop hook's
/// `decision:block` reason stays short. The real per-iteration prompt lives
/// in PROMPT.md, which the agent reads on each turn.
pub const DEFAULT_LOOP_BODY: &str =
    "Read $GROVE_AGENT_DIR/PROMPT.md and do the next smallest task.\n";

/// Write `.grove/agents/<name>/{PROMPT,STATE,loop}.md`. Refuses to clobber an
/// existing agent dir — re-spawning under the same name preserves history.
/// Returns the agent dir path on success.
pub fn seed_agent(
    project_root_path: &Path,
    name: &str,
    task: Option<&str>,
    completion_promise: &str,
    max_iterations: u32,
) -> Result<PathBuf, String> {
    if !is_valid_agent_name(name) {
        return Err(format!(
            "agent name '{}' must be kebab-case (letters, digits, '-', '_')",
            name
        ));
    }
    let dir = project_root_path.join(".grove/agents").join(name);
    if dir.exists() {
        return Err(format!(
            "agent dir already exists: {} (rename, or `grove remove` the worktree and edit the loop state)",
            dir.display()
        ));
    }
    fs::create_dir_all(&dir).map_err(|e| format!("create {}: {}", dir.display(), e))?;

    let prompt = PROMPT_TEMPLATE.replace("<AGENT_NAME>", name);
    fs::write(dir.join("PROMPT.md"), prompt).map_err(|e| format!("write PROMPT.md: {}", e))?;

    let state = state_md_template(name, task);
    fs::write(dir.join("STATE.md"), state).map_err(|e| format!("write STATE.md: {}", e))?;

    let loop_body = loop_md_template(completion_promise, max_iterations);
    fs::write(dir.join("loop.md"), loop_body).map_err(|e| format!("write loop.md: {}", e))?;

    Ok(dir)
}

pub fn is_valid_agent_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn state_md_template(name: &str, task: Option<&str>) -> String {
    let initial = task
        .map(|t| format!("- [ ] {}\n", t))
        .unwrap_or_else(|| "- [ ] (replace this line with your first task)\n".to_string());
    format!(
        r#"# STATE — agent `{}`

bus_last_seen: 1970-01-01T00:00:00Z

## Workitems

{}
## Iteration log

(grove spawn seeded; iteration log starts when the loop activates)
"#,
        name, initial
    )
}

fn loop_md_template(completion_promise: &str, max_iterations: u32) -> String {
    let promise = completion_promise.replace('"', "\\\"");
    format!(
        "---\nactive: false\niteration: 0\nmax_iterations: {}\ncompletion_promise: \"{}\"\nsession_id: \"\"\n---\n{}",
        max_iterations, promise, DEFAULT_LOOP_BODY
    )
}

/// Chmod SHARED.md to 0444 inside a worktree (per-worktree speedbump). Unix-only;
/// no-op on other platforms.
#[cfg(unix)]
pub fn chmod_shared_md_in_worktree(worktree_path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let path = worktree_path.join(".grove").join("SHARED.md");
    if !path.exists() {
        return Ok(());
    }
    let mut perms = fs::metadata(&path)
        .map_err(|e| format!("stat {}: {}", path.display(), e))?
        .permissions();
    perms.set_mode(0o444);
    fs::set_permissions(&path, perms).map_err(|e| format!("chmod {}: {}", path.display(), e))?;
    Ok(())
}

#[cfg(not(unix))]
pub fn chmod_shared_md_in_worktree(_worktree_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("grove-seed-test-{}", name));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn seed_creates_three_files() {
        let dir = tmp("three");
        let agent_dir =
            seed_agent(&dir, "feat-a", Some("build the thing"), "ALL DONE", 20).unwrap();
        assert!(agent_dir.join("PROMPT.md").exists());
        assert!(agent_dir.join("STATE.md").exists());
        assert!(agent_dir.join("loop.md").exists());
        let prompt = fs::read_to_string(agent_dir.join("PROMPT.md")).unwrap();
        assert!(prompt.contains("feat-a"));
        assert!(!prompt.contains("<AGENT_NAME>"));
        let state = fs::read_to_string(agent_dir.join("STATE.md")).unwrap();
        assert!(state.contains("- [ ] build the thing"));
        let loop_md = fs::read_to_string(agent_dir.join("loop.md")).unwrap();
        assert!(loop_md.contains("active: false"));
        assert!(loop_md.contains("max_iterations: 20"));
        assert!(loop_md.contains("ALL DONE"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn seed_rejects_existing_agent_dir() {
        let dir = tmp("existing");
        seed_agent(&dir, "feat-a", None, "DONE", 10).unwrap();
        assert!(seed_agent(&dir, "feat-a", None, "DONE", 10).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn seed_rejects_bad_name() {
        let dir = tmp("badname");
        assert!(seed_agent(&dir, "feat/a", None, "DONE", 10).is_err());
        assert!(seed_agent(&dir, "", None, "DONE", 10).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn loop_md_template_round_trips_through_loop_md_parser() {
        let body = loop_md_template("DONE", 5);
        let state = crate::agent::loop_md::parse(&body).unwrap();
        assert!(!state.active);
        assert_eq!(state.iteration, 0);
        assert_eq!(state.max_iterations, 5);
        assert_eq!(state.completion_promise, "DONE");
    }

    #[test]
    fn valid_agent_name_kebab() {
        assert!(is_valid_agent_name("feat-a"));
        assert!(is_valid_agent_name("data_loader"));
        assert!(is_valid_agent_name("feat-a-2"));
        assert!(!is_valid_agent_name("feat/a"));
        assert!(!is_valid_agent_name(""));
        assert!(!is_valid_agent_name("feat a"));
    }
}
