// Rewrite a worktree's two `.git` pointer files from absolute → relative paths.
//
// `git worktree add` writes ABSOLUTE host paths into both:
//   1. `<worktree>/.git`                          (forward pointer → main gitdir)
//   2. `<main_gitdir>/worktrees/<name>/gitdir`    (back pointer    → worktree's .git)
//
// When the project is bind-mounted into a devcontainer at a different path
// (e.g. host `/home/u/proj` → container `/workspaces/proj`), neither absolute
// path resolves inside the container — every git op inside the worktree breaks.
//
// Fix: rewrite both pointers to RELATIVE paths. Relative resolution is
// mount-agnostic — host and container see the same files and compute the same
// target dir.
//
// Layout-agnostic: the function discovers the main gitdir's per-worktree
// subdir by reading the existing forward pointer, so it works for both the
// bare layout (worktrees as siblings to `<repo>.git/`) and the in-place layout
// (`worktrees/<name>/` next to `.git/`).

use std::fs;
use std::path::{Component, Path, PathBuf};

/// Rewrite `<worktree_path>/.git` and its back-pointer to use relative paths.
///
/// Idempotent: if the pointers are already relative AND point to the same
/// targets, no change is made. If an existing relative pointer's target
/// disagrees with the worktree's actual location, we overwrite (latest wins).
///
/// Errors are returned as `Result<(), String>` so callers can decide whether
/// to hard-fail (spawn) or warn-and-continue (`repair-pointers` bulk run).
pub fn make_worktree_pointers_relative(worktree_path: &Path) -> Result<(), String> {
    let forward_file = worktree_path.join(".git");
    let forward_raw = fs::read_to_string(&forward_file)
        .map_err(|e| format!("read {}: {}", forward_file.display(), e))?;
    let forward_target_str = forward_raw
        .lines()
        .find_map(|l| l.strip_prefix("gitdir: "))
        .ok_or_else(|| format!("{}: missing 'gitdir:' line", forward_file.display()))?
        .trim()
        .to_string();

    // Resolve the gitdir worktree subdir (`<main_gitdir>/worktrees/<n>`) as
    // an absolute path. If the existing forward pointer is relative, resolve
    // against `worktree_path`; if absolute, use as-is.
    let gitdir_subdir_raw = if Path::new(&forward_target_str).is_absolute() {
        PathBuf::from(&forward_target_str)
    } else {
        worktree_path.join(&forward_target_str)
    };

    // Canonicalize both sides before component matching so that Windows
    // short-name (8.3) vs long-name (`RUNNER~1` vs `runneradmin`) or symlink
    // chains don't produce diverging components for what is actually the
    // same directory tree. Lexical-only normalization (the previous behavior)
    // fails on Windows GitHub runners where git canonicalizes worktree paths
    // to the long form but `std::env::temp_dir()` returns the short form.
    // Falls back to lexical normalize if canonicalize fails (e.g. file
    // missing on disk).
    let gitdir_subdir_abs = gitdir_subdir_raw
        .canonicalize()
        .map(|p| normalize(&p))
        .unwrap_or_else(|_| normalize(&gitdir_subdir_raw));
    let worktree_abs = worktree_path
        .canonicalize()
        .map(|p| normalize(&p))
        .unwrap_or_else(|_| normalize(worktree_path));

    let forward_rel = relative_path(&worktree_abs, &gitdir_subdir_abs);
    let new_forward = format!("gitdir: {}\n", forward_rel.display());
    write_if_changed(&forward_file, &new_forward)?;

    let back_file = gitdir_subdir_abs.join("gitdir");
    let back_rel = relative_path(&gitdir_subdir_abs, &worktree_abs.join(".git"));
    let new_back = format!("{}\n", back_rel.display());
    write_if_changed(&back_file, &new_back)?;

    // Tell git the relative worktree pointers are intentional.
    //
    // `extensions.relativeWorktrees` was added in git 2.46. The extension
    // requires `core.repositoryFormatVersion=1`, which old git (<2.42)
    // can't read. Grove's minimum git version is 2.46 (documented in
    // README); operators on older git should upgrade (Ubuntu:
    // `ppa:git-core/ppa`).
    //
    // Why this matters: with the extension set, git natively accepts
    // relative pointers during `worktree remove` / `worktree repair` /
    // etc. Without it, git rejects with "file does not contain absolute
    // path to the working tree location" even though our rewrite is
    // correct.
    //
    // We set the config on the LOCAL gitdir only; this never travels
    // with `git push` so collaborators on older git aren't affected by
    // their clone's format.
    let main_gitdir = gitdir_subdir_abs
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf());
    if let Some(gd) = main_gitdir {
        // Order matters: version must be 1 BEFORE the extension key is
        // read, otherwise git complains "extension found in repository
        // format version 0".
        let _ = std::process::Command::new("git")
            .arg(format!("--git-dir={}", gd.display()))
            .args(["config", "core.repositoryFormatVersion", "1"])
            .status();
        let _ = std::process::Command::new("git")
            .arg(format!("--git-dir={}", gd.display()))
            .args(["config", "extensions.relativeWorktrees", "true"])
            .status();
    }

    Ok(())
}

/// `path` lexically normalized: collapses `.` and `..` components without
/// touching the filesystem (no symlink resolution). Used to keep `..` from
/// leaking into our component-matching logic.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            _ => out.push(c.as_os_str()),
        }
    }
    out
}

/// Compute the relative path FROM the directory `from_dir` TO the path `to`.
/// Both inputs MUST be absolute and pre-normalized.
fn relative_path(from_dir: &Path, to: &Path) -> PathBuf {
    let from: Vec<_> = from_dir.components().collect();
    let to_c: Vec<_> = to.components().collect();
    let common = from
        .iter()
        .zip(to_c.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut result = PathBuf::new();
    for _ in 0..(from.len() - common) {
        result.push("..");
    }
    for c in &to_c[common..] {
        result.push(c.as_os_str());
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    result
}

fn write_if_changed(path: &Path, new: &str) -> Result<(), String> {
    if let Ok(existing) = fs::read_to_string(path) {
        if existing == new {
            return Ok(());
        }
    }
    fs::write(path, new).map_err(|e| format!("write {}: {}", path.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn tmp_dir(label: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("grove-wt-paths-{}-{}-{}", label, pid, nanos));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn git(cwd: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("git");
        if !out.status.success() {
            panic!(
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    #[test]
    fn relative_path_basic_sibling() {
        let rel = relative_path(Path::new("/a/b/c"), Path::new("/a/b/d/e"));
        assert_eq!(rel, PathBuf::from("../d/e"));
    }

    #[test]
    fn relative_path_to_same_dir() {
        let rel = relative_path(Path::new("/a/b/c"), Path::new("/a/b/c"));
        assert_eq!(rel, PathBuf::from("."));
    }

    #[test]
    fn relative_path_in_place_layout() {
        // worktree at <root>/worktrees/<n>, gitdir at <root>/.git/worktrees/<n>
        let rel = relative_path(
            Path::new("/root/worktrees/x"),
            Path::new("/root/.git/worktrees/x"),
        );
        assert_eq!(rel, PathBuf::from("../../.git/worktrees/x"));
    }

    #[test]
    fn relative_path_bare_layout() {
        // worktree at <root>/<n>, gitdir at <root>/<repo>.git/worktrees/<n>
        let rel = relative_path(
            Path::new("/root/x"),
            Path::new("/root/proj.git/worktrees/x"),
        );
        assert_eq!(rel, PathBuf::from("../proj.git/worktrees/x"));
    }

    #[test]
    fn end_to_end_in_place_layout_rewrite() {
        let root = tmp_dir("in-place");
        // Init repo with one commit so we can branch + add a worktree.
        git(&root, &["init", "-q", "-b", "main"]);
        fs::write(root.join("README"), "x").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "init"]);

        let wt = root.join("worktrees").join("agent-a");
        fs::create_dir_all(wt.parent().unwrap()).unwrap();
        git(
            &root,
            &[
                "worktree",
                "add",
                wt.to_str().unwrap(),
                "-b",
                "agent/agent-a",
            ],
        );

        // Sanity: git writes absolute paths by default.
        let forward = fs::read_to_string(wt.join(".git")).unwrap();
        let forward_target = forward
            .lines()
            .next()
            .unwrap()
            .trim_start_matches("gitdir: ")
            .trim();
        assert!(
            Path::new(forward_target).is_absolute(),
            "expected absolute path in forward pointer; got {}",
            forward
        );

        // Rewrite.
        make_worktree_pointers_relative(&wt).unwrap();

        // Both files now contain relative paths and git still agrees.
        // Normalize platform separators (Windows uses `\`) so assertions stay
        // separator-agnostic.
        let forward = fs::read_to_string(wt.join(".git")).unwrap();
        let forward_norm = forward.replace('\\', "/");
        assert!(
            forward_norm.starts_with("gitdir: ../../.git/worktrees/agent-a"),
            "forward not relative: {}",
            forward
        );
        let back = fs::read_to_string(
            root.join(".git")
                .join("worktrees")
                .join("agent-a")
                .join("gitdir"),
        )
        .unwrap();
        let back_norm = back.replace('\\', "/");
        assert!(
            back_norm.starts_with("../../../worktrees/agent-a/.git"),
            "back not relative: {}",
            back
        );

        // git can still resolve.
        let out = Command::new("git")
            .args(["rev-parse", "--git-dir"])
            .current_dir(&wt)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git rev-parse failed after rewrite: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // Idempotent: second run is a no-op.
        make_worktree_pointers_relative(&wt).unwrap();
        let forward2 = fs::read_to_string(wt.join(".git")).unwrap();
        assert_eq!(forward, forward2);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn already_relative_is_idempotent() {
        let root = tmp_dir("idempotent");
        git(&root, &["init", "-q", "-b", "main"]);
        fs::write(root.join("README"), "x").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "init"]);

        let wt = root.join("worktrees").join("ag");
        fs::create_dir_all(wt.parent().unwrap()).unwrap();
        git(
            &root,
            &["worktree", "add", wt.to_str().unwrap(), "-b", "agent/ag"],
        );

        make_worktree_pointers_relative(&wt).unwrap();
        let forward1 = fs::read_to_string(wt.join(".git")).unwrap();
        let mtime1 = fs::metadata(wt.join(".git")).unwrap().modified().unwrap();
        // Sleep > 1s so any rewrite would bump mtime.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        make_worktree_pointers_relative(&wt).unwrap();
        let forward2 = fs::read_to_string(wt.join(".git")).unwrap();
        let mtime2 = fs::metadata(wt.join(".git")).unwrap().modified().unwrap();
        assert_eq!(forward1, forward2);
        assert_eq!(mtime1, mtime2, "idempotent run should not rewrite the file");

        let _ = fs::remove_dir_all(&root);
    }
}
