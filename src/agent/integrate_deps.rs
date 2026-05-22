// Branch metadata + file-overlap helpers for `grove integrate`.
//
// `grove integrate` snapshots two pieces of dependency context into
// `worktrees/.integration/.grove-context/` so the spawned integrate agent
// can make an informed merge-ordering decision:
//
//   branches.json — machine-readable per-branch facts: head sha, files
//                   changed since base, commit count, last few commit
//                   subjects.
//   overlap.txt   — human-readable pairwise file-overlap matrix.
//                   Branches that touch the same files have a high
//                   conflict-risk and should generally be merged
//                   adjacent (one resolved → the next's conflicts may
//                   be smaller).
//
// The agent reads both, picks an order, and rewrites STATE.md workitems.
// This module produces the data; it does not pick the order itself.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct BranchMeta {
    pub name: String,
    pub head_sha: String,
    pub files_changed: Vec<String>,
    pub commit_count: u32,
    pub tip_log: Vec<String>,
}

/// Top-level metadata struct serialized into `branches.json`. Fields are
/// declared in the order the agent reads them so the file is also
/// human-skimmable.
#[derive(Debug, Clone, Serialize)]
pub struct IntegrationContext {
    pub base: String,
    pub base_sha: String,
    pub integration_branch: String,
    pub verify_cmd: Vec<String>,
    pub no_test: bool,
    pub branches: Vec<BranchMeta>,
}

impl IntegrationContext {
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| format!("serialize branches.json: {}", e))
    }
}

/// Compute per-branch metadata. Routes `git` through the backend dispatcher:
/// `project_root` selects host vs sandbox container, `repo_path` is the cwd
/// (the bare clone in bare layout, the working-tree root in-place). Works in
/// both layouts and both backends.
pub fn compute_branch_metadata(
    project_root: &Path,
    repo_path: &Path,
    branches: &[String],
    base: &str,
) -> Result<Vec<BranchMeta>, String> {
    let mut out = Vec::with_capacity(branches.len());
    for name in branches {
        let head_sha = git_oneline(project_root, repo_path, &["rev-parse", name])?;
        let files_raw = git_oneline_multi(
            project_root,
            repo_path,
            &["diff", "--name-only", base, name],
        )?;
        let files_changed: Vec<String> = files_raw
            .lines()
            .filter(|l| !l.is_empty())
            .map(|s| s.to_string())
            .collect();
        let commit_count_raw = git_oneline(
            project_root,
            repo_path,
            &["rev-list", "--count", &format!("{}..{}", base, name)],
        )?;
        let commit_count: u32 = commit_count_raw.trim().parse().unwrap_or(0);
        let tip_log_raw = git_oneline_multi(
            project_root,
            repo_path,
            &[
                "log",
                "--pretty=%s",
                "-n",
                "5",
                &format!("{}..{}", base, name),
            ],
        )?;
        let tip_log: Vec<String> = tip_log_raw
            .lines()
            .filter(|l| !l.is_empty())
            .map(|s| s.to_string())
            .collect();
        out.push(BranchMeta {
            name: name.clone(),
            head_sha: head_sha.trim().to_string(),
            files_changed,
            commit_count,
            tip_log,
        });
    }
    Ok(out)
}

/// Render a human-readable pairwise file-overlap matrix. Sorted by
/// overlap-count descending so the agent sees the highest-risk pairs
/// first.
pub fn pairwise_overlap(branches: &[BranchMeta]) -> String {
    let mut lines = String::new();
    lines.push_str("File overlap matrix (branches that touch the same files = conflict-risk):\n\n");

    // Collect all pairs and rank by shared count.
    let mut pairs: Vec<(usize, usize, Vec<String>)> = Vec::new();
    for i in 0..branches.len() {
        for j in (i + 1)..branches.len() {
            let a: BTreeSet<&str> = branches[i]
                .files_changed
                .iter()
                .map(|s| s.as_str())
                .collect();
            let b: BTreeSet<&str> = branches[j]
                .files_changed
                .iter()
                .map(|s| s.as_str())
                .collect();
            let shared: Vec<String> = a.intersection(&b).map(|s| s.to_string()).collect();
            pairs.push((i, j, shared));
        }
    }
    pairs.sort_by(|a, b| b.2.len().cmp(&a.2.len()));

    for (i, j, shared) in &pairs {
        let count = shared.len();
        let label = if count == 1 { "file" } else { "files" };
        lines.push_str(&format!(
            "{} vs {}: {} shared {}\n",
            branches[*i].name, branches[*j].name, count, label
        ));
        for f in shared {
            lines.push_str(&format!("  {}\n", f));
        }
        lines.push('\n');
    }

    lines.push_str(
        "Heuristic: merge smaller-overlap branches first (less conflict cascade). \
         Re-order STATE.md workitems before flipping loop.md active:true.\n",
    );
    lines
}

/// Run `git -C <repo_path> <args>` via the backend dispatcher (host or
/// sandbox container, selected by `project_root`); returns stdout on success.
fn git_oneline(project_root: &Path, repo_path: &Path, args: &[&str]) -> Result<String, String> {
    let out = crate::git::git_exec::run(project_root, repo_path, args)?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn git_oneline_multi(
    project_root: &Path,
    repo_path: &Path,
    args: &[&str],
) -> Result<String, String> {
    git_oneline(project_root, repo_path, args)
}

/// Resolve the absolute SHA of the base branch so it gets recorded into
/// branches.json (auditability — if main moves between integrate runs we
/// know what we tried to merge against).
pub fn resolve_base_sha(
    project_root: &Path,
    repo_path: &Path,
    base: &str,
) -> Result<String, String> {
    Ok(git_oneline(project_root, repo_path, &["rev-parse", base])?
        .trim()
        .to_string())
}

/// Extract verify command from .grove/config.toml. Returns empty vec if
/// none set — agent treats that as "no verify".
pub fn read_verify_command(project_root: &Path) -> Vec<String> {
    let path = project_root.join(".grove").join("config.toml");
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let parsed: Value = match toml::from_str::<Value>(&raw) {
        Ok(v) => serde_json::to_value(v).unwrap_or(Value::Null),
        Err(_) => return Vec::new(),
    };
    parsed
        .get("verify")
        .and_then(|v| v.get("test"))
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    fn tmp_repo(label: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p =
            std::env::temp_dir().join(format!("grove-integrate-deps-{}-{}-{}", label, pid, nanos));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        run_git(&p, &["init", "-q", "-b", "main"]);
        run_git(&p, &["config", "user.email", "t@t"]);
        run_git(&p, &["config", "user.name", "t"]);
        fs::write(p.join("README"), "x").unwrap();
        run_git(&p, &["add", "."]);
        run_git(&p, &["commit", "-q", "-m", "init"]);
        p
    }

    fn run_git(repo: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git");
        if !out.status.success() {
            panic!(
                "git {:?} failed:\nstdout: {}\nstderr: {}",
                args,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    fn make_branch(repo: &Path, name: &str, files: &[(&str, &str)]) {
        run_git(repo, &["checkout", "-q", "-b", name, "main"]);
        for (path, content) in files {
            fs::write(repo.join(path), content).unwrap();
            run_git(repo, &["add", path]);
        }
        run_git(
            repo,
            &["commit", "-q", "-m", &format!("feat({}): work", name)],
        );
        run_git(repo, &["checkout", "-q", "main"]);
    }

    #[test]
    fn branch_metadata_records_files_and_commit_count() {
        let repo = tmp_repo("meta");
        make_branch(&repo, "agent/feat-a", &[("a.rs", "fn a(){}")]);
        let meta = compute_branch_metadata(&repo, &repo, &["agent/feat-a".into()], "main").unwrap();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0].name, "agent/feat-a");
        assert_eq!(meta[0].files_changed, vec!["a.rs"]);
        assert_eq!(meta[0].commit_count, 1);
        assert!(meta[0]
            .tip_log
            .iter()
            .any(|s| s.contains("feat(agent/feat-a)")));
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn overlap_finds_shared_files() {
        let repo = tmp_repo("overlap");
        make_branch(&repo, "agent/a", &[("shared.rs", "a"), ("a.rs", "a")]);
        make_branch(&repo, "agent/b", &[("shared.rs", "b"), ("b.rs", "b")]);
        let meta =
            compute_branch_metadata(&repo, &repo, &["agent/a".into(), "agent/b".into()], "main")
                .unwrap();
        let text = pairwise_overlap(&meta);
        assert!(text.contains("agent/a vs agent/b: 1 shared file"));
        assert!(text.contains("  shared.rs"));
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn overlap_ranks_by_shared_count_descending() {
        let repo = tmp_repo("rank");
        make_branch(
            &repo,
            "agent/a",
            &[("x.rs", "a"), ("y.rs", "a"), ("z.rs", "a")],
        );
        make_branch(&repo, "agent/b", &[("x.rs", "b"), ("y.rs", "b")]); // overlap 2 with a
        make_branch(&repo, "agent/c", &[("x.rs", "c")]); // overlap 1 with a, 1 with b
        let meta = compute_branch_metadata(
            &repo,
            &repo,
            &["agent/a".into(), "agent/b".into(), "agent/c".into()],
            "main",
        )
        .unwrap();
        let text = pairwise_overlap(&meta);
        // a vs b (2 shared) should come before any pair with 1 shared
        let ab = text.find("agent/a vs agent/b").unwrap();
        let ac = text.find("agent/a vs agent/c").unwrap();
        let bc = text.find("agent/b vs agent/c").unwrap();
        assert!(ab < ac, "a-b (2) should rank before a-c (1)");
        assert!(ab < bc, "a-b (2) should rank before b-c (1)");
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn integration_context_serializes_to_json() {
        let ctx = IntegrationContext {
            base: "main".into(),
            base_sha: "abc1234".into(),
            integration_branch: "integration/20260521T010203Z".into(),
            verify_cmd: vec!["cargo".into(), "test".into()],
            no_test: false,
            branches: vec![BranchMeta {
                name: "agent/x".into(),
                head_sha: "def5678".into(),
                files_changed: vec!["src/x.rs".into()],
                commit_count: 3,
                tip_log: vec!["feat(x): one".into(), "fix(x): two".into()],
            }],
        };
        let json = ctx.to_json().unwrap();
        assert!(json.contains("\"base\": \"main\""));
        assert!(json.contains("\"integration_branch\": \"integration/20260521T010203Z\""));
        assert!(json.contains("\"verify_cmd\""));
        assert!(json.contains("\"no_test\": false"));
        assert!(json.contains("\"name\": \"agent/x\""));
    }

    #[test]
    fn empty_branch_list_produces_empty_overlap_text() {
        let text = pairwise_overlap(&[]);
        assert!(text.contains("File overlap matrix"));
        assert!(text.contains("Heuristic"));
    }
}
