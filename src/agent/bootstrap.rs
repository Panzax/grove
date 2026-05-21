// Build the bootstrap prompt that `grove spawn` injects as the spawned
// claude session's first turn.
//
// Two flavors per fresh-vs-resume × task-vs-none:
//   Fresh + task    → orientation + task-driven self-init protocol
//   Fresh + no task → orientation + "user will define" hint
//   Resume          → short "you're resuming, read STATE.md + loop.md" note
//
// Templates live in `assets/AGENT_BOOTSTRAP*.md` and are baked into the
// binary at compile time via `include_str!`, so the binary is fully
// self-contained (no extra files to ship). Placeholders use
// `<UPPER_SNAKE>` so they're visible if substitution ever misses one.
//
// The output is one big string passed as a final argv token to `claude
// --dangerously-skip-permissions <prompt>`. Claude treats that argv as
// its initial user message.

const ORIENT: &str = include_str!("../../assets/AGENT_BOOTSTRAP.md");
const TASK_SECTION: &str = include_str!("../../assets/AGENT_BOOTSTRAP_TASK.md");
const NO_TASK_SECTION: &str = include_str!("../../assets/AGENT_BOOTSTRAP_NO_TASK.md");
const RESUME_SECTION: &str = include_str!("../../assets/AGENT_BOOTSTRAP_RESUME.md");

#[derive(Debug, Clone)]
pub struct BootstrapSpec<'a> {
    pub agent_name: &'a str,
    pub repo_name: &'a str,
    /// Container-side absolute path to the worktree (claude's cwd).
    pub container_worktree_path: &'a str,
    /// Container-side absolute path to `$GROVE_AGENT_DIR`.
    pub container_agent_dir: &'a str,
    /// `Some(task)` when `--task` was provided on a fresh spawn.
    pub task: Option<&'a str>,
    /// True when this is a resume; emits the short resume section instead
    /// of the full bootstrap.
    pub resume: bool,
}

pub fn build_bootstrap_prompt(spec: &BootstrapSpec<'_>) -> String {
    let mut out = String::with_capacity(ORIENT.len() + 800);
    out.push_str(ORIENT);

    if spec.resume {
        out.push_str(RESUME_SECTION);
    } else {
        match spec.task {
            Some(_) => out.push_str(TASK_SECTION),
            None => out.push_str(NO_TASK_SECTION),
        }
    }

    let task_value = spec.task.unwrap_or("");
    out = out
        .replace("<AGENT_NAME>", spec.agent_name)
        .replace("<REPO_NAME>", spec.repo_name)
        .replace("<CONTAINER_WORKTREE_PATH>", spec.container_worktree_path)
        .replace("<CONTAINER_AGENT_DIR>", spec.container_agent_dir)
        .replace("<TASK>", task_value);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> BootstrapSpec<'static> {
        BootstrapSpec {
            agent_name: "feat-x",
            repo_name: "demo",
            container_worktree_path: "/workspaces/demo/worktrees/feat-x",
            container_agent_dir: "/workspaces/demo/.grove/agents/feat-x",
            task: None,
            resume: false,
        }
    }

    #[test]
    fn substitutes_orientation_placeholders() {
        let p = build_bootstrap_prompt(&base());
        assert!(p.contains("agent `feat-x`"));
        assert!(p.contains("project `demo`"));
        assert!(p.contains("/workspaces/demo/worktrees/feat-x"));
        assert!(p.contains("/workspaces/demo/.grove/agents/feat-x"));
        // No leftover placeholders.
        assert!(!p.contains("<AGENT_NAME>"));
        assert!(!p.contains("<REPO_NAME>"));
        assert!(!p.contains("<CONTAINER_WORKTREE_PATH>"));
        assert!(!p.contains("<CONTAINER_AGENT_DIR>"));
    }

    #[test]
    fn no_task_includes_no_task_section() {
        let p = build_bootstrap_prompt(&base());
        assert!(p.contains("No task specified"));
        assert!(!p.contains("Bootstrap protocol"));
        assert!(!p.contains("Resuming after"));
    }

    #[test]
    fn with_task_includes_task_section() {
        let mut s = base();
        s.task = Some("add a Rust crate");
        let p = build_bootstrap_prompt(&s);
        assert!(p.contains("Bootstrap protocol"));
        assert!(p.contains("Your task: \"add a Rust crate\""));
        assert!(!p.contains("No task specified"));
        assert!(!p.contains("Resuming after"));
        assert!(!p.contains("<TASK>"));
    }

    #[test]
    fn resume_overrides_task_branch() {
        // Even if task is Some, resume wins — we don't re-bootstrap on resume.
        let mut s = base();
        s.task = Some("ignored");
        s.resume = true;
        let p = build_bootstrap_prompt(&s);
        assert!(p.contains("Resuming after"));
        assert!(!p.contains("Bootstrap protocol"));
        assert!(!p.contains("No task specified"));
    }

    #[test]
    fn empty_task_value_safe() {
        let mut s = base();
        s.task = Some("");
        let p = build_bootstrap_prompt(&s);
        // Empty quotes are fine, no leftover placeholder.
        assert!(p.contains("Your task: \"\""));
        assert!(!p.contains("<TASK>"));
    }
}
