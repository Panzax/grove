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
    // Ensure the parent exists, then atomically create the agent dir. The
    // non-recursive create fails with AlreadyExists if a concurrent spawn won
    // the race; the prior exists() check we used to do had a TOCTOU window.
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {}", parent.display(), e))?;
    }
    if let Err(e) = fs::DirBuilder::new().recursive(false).create(&dir) {
        return Err(format!(
            "agent dir already exists or could not be created: {} ({}). Rename or `grove remove` the worktree and edit the loop state.",
            dir.display(),
            e
        ));
    }

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

/// Symlink the project's `.grove/` into the worktree so the Stop-hook command
/// `$CLAUDE_PROJECT_DIR/.grove/tools/loop-hook.sh` resolves from inside the
/// worktree, AND so the agent's PROMPT.md references like `.grove/PROTOCOL.md`
/// work from the agent's cwd (without `../..` gymnastics).
///
/// The symlink is relative so the worktree stays portable. We also append `.grove`
/// to the worktree's local `info/exclude` so `git status` inside the worktree
/// doesn't flag the symlink as untracked.
///
/// Unix-only (Linux + macOS, per v1 target platforms). No-op on other platforms.
#[cfg(unix)]
pub fn link_grove_into_worktree(worktree_path: &Path, project_root: &Path) -> Result<(), String> {
    use std::os::unix::fs::symlink;

    let link_path = worktree_path.join(".grove");
    if link_path.exists() || link_path.is_symlink() {
        // Already linked (or, more likely, a stale prior link) — idempotent.
        return Ok(());
    }

    // Compute a relative path from the worktree to the project's .grove/.
    // We can't rely on `pathdiff` (extra dep); spell out the canonical case.
    // Worktrees live at <project_root>/<a>/.../<b>/  (1 level for bare-sibling
    // layout, 2 levels for `worktrees/<name>/` in-place layout). Walk parents
    // to count the depth.
    let target = relative_path_to_grove(worktree_path, project_root).ok_or_else(|| {
        format!(
            "worktree {} is not inside project root {}",
            worktree_path.display(),
            project_root.display()
        )
    })?;
    symlink(&target, &link_path).map_err(|e| {
        format!(
            "symlink {} -> {}: {}",
            link_path.display(),
            target.display(),
            e
        )
    })?;
    // Add `.grove` to the worktree's local exclude so `git status` stays clean.
    // Failure is non-fatal — the symlink is the load-bearing piece.
    let _ = add_grove_to_worktree_exclude(worktree_path);
    Ok(())
}

#[cfg(not(unix))]
pub fn link_grove_into_worktree(_worktree_path: &Path, _project_root: &Path) -> Result<(), String> {
    eprintln!(
        "Warning: grove symlinks .grove/ into each worktree on Unix only; the hook \
         engine will not auto-resolve from worktree sessions on this platform."
    );
    Ok(())
}

/// Sandbox variant of [`link_grove_into_worktree`]. The worktree lives inside
/// the sandbox container (not on the host), so the symlink and the per-worktree
/// `info/exclude` entry are created in the container via the backend exec seam.
///
/// `.grove/` is bind-mounted at `<project_root>/.grove` (identical path on both
/// sides), so the relative `../.grove` target resolves to the shared control
/// plane exactly as it does for a host worktree.
pub fn link_grove_into_worktree_sandbox(
    worktree_path: &Path,
    project_root: &Path,
) -> Result<(), String> {
    let target = relative_path_to_grove_lexical(worktree_path, project_root).ok_or_else(|| {
        format!(
            "worktree {} is not under project root {}",
            worktree_path.display(),
            project_root.display()
        )
    })?;
    let info = crate::session::backend::sandbox_info(project_root);
    let link = worktree_path.join(".grove");

    // `ln -sfn` is idempotent: replaces a stale symlink, leaves a correct one.
    let out = crate::session::container::exec(
        &info,
        &[
            "ln",
            "-sfn",
            &target.to_string_lossy(),
            &link.to_string_lossy(),
        ],
    )?;
    if !out.status.success() {
        return Err(format!(
            "ln -s .grove in sandbox: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    // Append `.grove` to the worktree's git info/exclude (idempotently) so
    // `git status` inside the worktree doesn't flag the symlink as untracked.
    let wt = worktree_path.to_string_lossy().to_string();
    let script = format!(
        "d=$(git -C {wt} rev-parse --git-dir) && mkdir -p \"$d/info\" && \
         (grep -qxF .grove \"$d/info/exclude\" 2>/dev/null || echo .grove >> \"$d/info/exclude\")",
        wt = shell_single_quote(&wt)
    );
    let _ = crate::session::container::exec(&info, &["sh", "-c", &script]);
    Ok(())
}

/// Lexical (no-canonicalize) variant of `relative_path_to_grove` for sandbox
/// worktrees, whose paths don't exist on the host so `canonicalize` would fail.
/// Both inputs are already clean absolute paths (`project_root.join(name)`).
#[cfg(unix)]
fn relative_path_to_grove_lexical(worktree_path: &Path, project_root: &Path) -> Option<PathBuf> {
    let relative = worktree_path.strip_prefix(project_root).ok()?;
    let depth = relative.components().count();
    if depth == 0 {
        return None;
    }
    let mut up = PathBuf::new();
    for _ in 0..depth {
        up.push("..");
    }
    up.push(".grove");
    Some(up)
}

#[cfg(not(unix))]
fn relative_path_to_grove_lexical(_worktree_path: &Path, _project_root: &Path) -> Option<PathBuf> {
    None
}

/// Minimal single-quote shell escape for embedding a path in an `sh -c` string.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Compute `../../...../.grove` such that, when used as a symlink target inside
/// the worktree, it resolves to `<project_root>/.grove`.
fn relative_path_to_grove(worktree_path: &Path, project_root: &Path) -> Option<PathBuf> {
    let worktree_canon = worktree_path
        .canonicalize()
        .unwrap_or_else(|_| worktree_path.to_path_buf());
    let root_canon = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let relative = worktree_canon.strip_prefix(&root_canon).ok()?;
    let depth = relative.components().count();
    if depth == 0 {
        return None; // worktree == project root? Shouldn't happen.
    }
    let mut up = PathBuf::new();
    for _ in 0..depth {
        up.push("..");
    }
    up.push(".grove");
    Some(up)
}

/// Find the per-worktree gitdir and append `.grove` to its `info/exclude`.
///
/// For linked worktrees, `git rev-parse --git-dir` returns `<main_gitdir>/worktrees/<wt_name>`.
/// Each linked worktree has its own `info/exclude` there.
fn add_grove_to_worktree_exclude(worktree_path: &Path) -> Result<(), String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(worktree_path)
        .output()
        .map_err(|e| format!("git rev-parse: {}", e))?;
    if !out.status.success() {
        return Err(format!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let gitdir_raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let gitdir = if gitdir_raw.starts_with('/') {
        PathBuf::from(gitdir_raw)
    } else {
        worktree_path.join(gitdir_raw)
    };
    let exclude = gitdir.join("info").join("exclude");
    if let Some(parent) = exclude.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {}", parent.display(), e))?;
    }
    let existing = fs::read_to_string(&exclude).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == ".grove") {
        return Ok(());
    }
    let mut out = existing.clone();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(".grove\n");
    fs::write(&exclude, out).map_err(|e| format!("write {}: {}", exclude.display(), e))?;
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
    fn relative_path_to_grove_one_level() {
        let root = tmp("rp1");
        let wt = root.join("feat-a");
        std::fs::create_dir_all(&wt).unwrap();
        let target = relative_path_to_grove(&wt, &root).unwrap();
        assert_eq!(target, PathBuf::from("..").join(".grove"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn relative_path_to_grove_two_levels() {
        let root = tmp("rp2");
        let wt = root.join("worktrees").join("feat-a");
        std::fs::create_dir_all(&wt).unwrap();
        let target = relative_path_to_grove(&wt, &root).unwrap();
        assert_eq!(target, PathBuf::from("..").join("..").join(".grove"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn relative_path_to_grove_outside_root_is_none() {
        let root = tmp("rp-outside");
        let elsewhere = std::env::temp_dir().join("grove-rp-outside-other");
        std::fs::create_dir_all(&elsewhere).unwrap();
        assert!(relative_path_to_grove(&elsewhere, &root).is_none());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }

    #[test]
    fn seed_atomic_create_rejects_concurrent_double_spawn() {
        // First seed succeeds; second seed with the same name fails with AlreadyExists.
        let dir = tmp("atomic");
        seed_agent(&dir, "feat-a", None, "DONE", 10).unwrap();
        let second = seed_agent(&dir, "feat-a", None, "DONE", 10);
        assert!(second.is_err());
        let _ = std::fs::remove_dir_all(&dir);
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
