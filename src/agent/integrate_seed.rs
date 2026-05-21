// Seed an integrate-flavor agent state directory.
//
// Mirrors `agent/seed.rs::seed_agent` but with integrate-specific
// templates: STATE.md is pre-populated with one workitem per `agent/*`
// branch plus a verify step plus a PR-creation step. PROMPT.md is the
// integrate-loop per-iteration prompt. loop.md carries an integrate-
// specific completion promise.
//
// Also writes a snapshot of the bootstrap prompt into the agent dir so
// the loop has it as a reference (the bootstrap is also injected as
// claude's initial argv, but a disk copy is useful for re-read).

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::agent::integrate_deps::IntegrationContext;
use crate::models::AgentMetadata;

pub const INTEGRATE_PROMPT_TEMPLATE: &str =
    include_str!("../../assets/INTEGRATE_PROMPT.template.md");
pub const INTEGRATE_STATE_TEMPLATE: &str = include_str!("../../assets/INTEGRATE_STATE.template.md");
pub const INTEGRATE_BOOTSTRAP_TEMPLATE: &str = include_str!("../../assets/INTEGRATE_BOOTSTRAP.md");

/// Default max iterations for integrate agents. Higher than `grove spawn`'s
/// 30 because each branch is at least one iteration, plus verify + PR + any
/// conflict-resolution re-iterations.
pub const INTEGRATE_DEFAULT_MAX_ITERATIONS: u32 = 50;

/// Build the completion promise the integrate agent must satisfy before the
/// Stop hook terminates the loop.
pub fn integrate_completion_promise(branch_count: usize, no_test: bool, base: &str) -> String {
    let verify_fragment = if no_test {
        String::from("verify skipped (--no-test)")
    } else {
        String::from("verify command passed")
    };
    format!(
        "All {} agent/* branches merged into the integration branch with conflicts resolved, {}, and a PR opened against {}.",
        branch_count, verify_fragment, base
    )
}

/// Seed `.grove/agents/<name>/{PROMPT,STATE,loop,agent}.md`. Returns the
/// agent dir on success.
pub fn seed_integrate_agent(
    project_root_path: &Path,
    name: &str,
    ctx: &IntegrationContext,
) -> Result<PathBuf, String> {
    if !crate::agent::seed::is_valid_agent_name(name) {
        return Err(format!(
            "agent name '{}' must be kebab-case (letters, digits, '-', '_')",
            name
        ));
    }
    let dir = project_root_path.join(".grove/agents").join(name);
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {}", parent.display(), e))?;
    }
    if let Err(e) = fs::DirBuilder::new().recursive(false).create(&dir) {
        return Err(format!(
            "agent dir already exists or could not be created: {} ({})",
            dir.display(),
            e
        ));
    }

    fs::write(dir.join("PROMPT.md"), INTEGRATE_PROMPT_TEMPLATE)
        .map_err(|e| format!("write PROMPT.md: {}", e))?;

    let state = state_md_template(name, ctx);
    fs::write(dir.join("STATE.md"), state).map_err(|e| format!("write STATE.md: {}", e))?;

    let promise = integrate_completion_promise(ctx.branches.len(), ctx.no_test, &ctx.base);
    let loop_body = loop_md_template(&promise, INTEGRATE_DEFAULT_MAX_ITERATIONS);
    fs::write(dir.join("loop.md"), loop_body).map_err(|e| format!("write loop.md: {}", e))?;

    // Snapshot the bootstrap prompt into the agent dir for re-read. The
    // orchestrator also passes this as claude's initial argv, but writing
    // it to disk lets the agent grep it again from later iterations.
    fs::write(
        dir.join("INTEGRATE_BOOTSTRAP.snapshot.md"),
        INTEGRATE_BOOTSTRAP_TEMPLATE,
    )
    .map_err(|e| format!("write bootstrap snapshot: {}", e))?;

    // agent.toml so `grove agents list` / `status` / `attach` machinery
    // sees the integrate agent the same way as a spawn agent. Without
    // this the agent dir is invisible to commands::agents::collect_agents.
    let metadata = AgentMetadata {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.to_string(),
        worktree: format!(
            "{}/worktrees/.integration",
            project_root_path.to_string_lossy()
        ),
        branch: ctx.integration_branch.clone(),
        task: Some(format!(
            "Integrate {} agent/* branch(es) into {}",
            ctx.branches.len(),
            ctx.base
        )),
        tmux_session: Some(crate::session::tmux::session_name(name)),
        spawned_at: Utc::now(),
        provider: "claude-code".to_string(),
    };
    let agent_toml = dir.join("agent.toml");
    let body =
        toml::to_string_pretty(&metadata).map_err(|e| format!("serialize agent.toml: {}", e))?;
    fs::write(&agent_toml, body).map_err(|e| format!("write agent.toml: {}", e))?;

    Ok(dir)
}

fn state_md_template(name: &str, ctx: &IntegrationContext) -> String {
    let merge_lines = ctx
        .branches
        .iter()
        .map(|b| format!("- [ ] merge {}", b.name))
        .collect::<Vec<_>>()
        .join("\n");
    let verify_display = if ctx.verify_cmd.is_empty() {
        "(no verify configured)".to_string()
    } else {
        ctx.verify_cmd.join(" ")
    };
    INTEGRATE_STATE_TEMPLATE
        .replace("<AGENT_NAME>", name)
        .replace("<BASE>", &ctx.base)
        .replace("<INTEGRATION_BRANCH>", &ctx.integration_branch)
        .replace("<BRANCH_COUNT>", &ctx.branches.len().to_string())
        .replace("<VERIFY_CMD>", &verify_display)
        .replace("<NO_TEST>", &ctx.no_test.to_string())
        .replace("<MERGE_WORKITEMS>", &merge_lines)
}

/// Render the integrate-flavor bootstrap prompt with the runtime
/// placeholders substituted. Used as claude's initial argv token by the
/// orchestrator.
pub fn build_integrate_bootstrap_prompt(
    agent_name: &str,
    repo_name: &str,
    container_worktree_path: &str,
    container_agent_dir: &str,
    integration_branch: &str,
    base: &str,
) -> String {
    INTEGRATE_BOOTSTRAP_TEMPLATE
        .replace("<AGENT_NAME>", agent_name)
        .replace("<REPO_NAME>", repo_name)
        .replace("<CONTAINER_WORKTREE_PATH>", container_worktree_path)
        .replace("<CONTAINER_AGENT_DIR>", container_agent_dir)
        .replace("<INTEGRATION_BRANCH>", integration_branch)
        .replace("<BASE>", base)
}

fn loop_md_template(completion_promise: &str, max_iterations: u32) -> String {
    let promise = completion_promise.replace('"', "\\\"");
    format!(
        "---\nactive: false\niteration: 0\nmax_iterations: {}\ncompletion_promise: \"{}\"\nsession_id: \"\"\n---\nRead $GROVE_AGENT_DIR/PROMPT.md and execute the next workitem in STATE.md.\n",
        max_iterations, promise
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::integrate_deps::{BranchMeta, IntegrationContext};

    fn tmp(label: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("grove-integ-seed-{}-{}-{}", label, pid, nanos));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn sample_ctx() -> IntegrationContext {
        IntegrationContext {
            base: "main".into(),
            base_sha: "abc1234".into(),
            integration_branch: "integration/20260521T010203Z".into(),
            verify_cmd: vec!["cargo".into(), "test".into(), "--all".into()],
            no_test: false,
            branches: vec![
                BranchMeta {
                    name: "agent/feat-a".into(),
                    head_sha: "aaa".into(),
                    files_changed: vec!["src/a.rs".into()],
                    commit_count: 2,
                    tip_log: vec!["feat(a): init".into()],
                },
                BranchMeta {
                    name: "agent/feat-b".into(),
                    head_sha: "bbb".into(),
                    files_changed: vec!["src/b.rs".into()],
                    commit_count: 1,
                    tip_log: vec!["feat(b): init".into()],
                },
            ],
        }
    }

    #[test]
    fn seed_creates_five_files_including_agent_toml() {
        let root = tmp("five");
        let ctx = sample_ctx();
        let agent_dir = seed_integrate_agent(&root, "integrate-x", &ctx).unwrap();
        assert!(agent_dir.join("PROMPT.md").exists());
        assert!(agent_dir.join("STATE.md").exists());
        assert!(agent_dir.join("loop.md").exists());
        assert!(agent_dir.join("INTEGRATE_BOOTSTRAP.snapshot.md").exists());
        assert!(agent_dir.join("agent.toml").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn agent_toml_records_integration_branch_and_session_name() {
        let root = tmp("agenttoml");
        let ctx = sample_ctx();
        let agent_dir = seed_integrate_agent(&root, "integrate-y", &ctx).unwrap();
        let raw = fs::read_to_string(agent_dir.join("agent.toml")).unwrap();
        assert!(raw.contains("name = \"integrate-y\""));
        assert!(raw.contains("branch = \"integration/20260521T010203Z\""));
        assert!(raw.contains("tmux_session = \"grove-integrate-y\""));
        assert!(raw.contains("Integrate 2 agent/* branch(es) into main"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn state_md_lists_one_workitem_per_branch_plus_verify_plus_pr() {
        let root = tmp("workitems");
        let ctx = sample_ctx();
        let agent_dir = seed_integrate_agent(&root, "integrate-x", &ctx).unwrap();
        let state = fs::read_to_string(agent_dir.join("STATE.md")).unwrap();
        assert!(state.contains("- [ ] merge agent/feat-a"));
        assert!(state.contains("- [ ] merge agent/feat-b"));
        assert!(state.contains("- [ ] verify: cargo test --all"));
        assert!(state.contains(
            "- [ ] open PR: `gh pr create --base main --head integration/20260521T010203Z`"
        ));
        assert!(state.contains("integration/20260521T010203Z"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn loop_md_carries_correct_promise_and_max_iter() {
        let root = tmp("loopmd");
        let ctx = sample_ctx();
        let agent_dir = seed_integrate_agent(&root, "integrate-x", &ctx).unwrap();
        let body = fs::read_to_string(agent_dir.join("loop.md")).unwrap();
        assert!(body.contains("max_iterations: 50"));
        assert!(body.contains("All 2 agent/* branches merged"));
        assert!(body.contains("verify command passed"));
        assert!(body.contains("against main"));
        assert!(body.contains("active: false"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn loop_md_no_test_promise_wording() {
        let root = tmp("notest");
        let mut ctx = sample_ctx();
        ctx.no_test = true;
        let agent_dir = seed_integrate_agent(&root, "integrate-x", &ctx).unwrap();
        let body = fs::read_to_string(agent_dir.join("loop.md")).unwrap();
        assert!(body.contains("verify skipped"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn seed_rejects_existing_agent_dir() {
        let root = tmp("exists");
        let ctx = sample_ctx();
        seed_integrate_agent(&root, "integrate-x", &ctx).unwrap();
        assert!(seed_integrate_agent(&root, "integrate-x", &ctx).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn bootstrap_prompt_substitutes_all_placeholders() {
        let p = build_integrate_bootstrap_prompt(
            "integrate-x",
            "demo",
            "/workspaces/demo/worktrees/.integration",
            "/workspaces/demo/.grove/agents/integrate-x",
            "integration/20260521T010203Z",
            "main",
        );
        assert!(p.contains("agent `integrate-x`"));
        assert!(p.contains("project `demo`"));
        assert!(p.contains("/workspaces/demo/worktrees/.integration"));
        assert!(p.contains("/workspaces/demo/.grove/agents/integrate-x"));
        assert!(p.contains("integration/20260521T010203Z"));
        assert!(p.contains("Base branch (PR target) | `main`"));
        assert!(!p.contains("<AGENT_NAME>"));
        assert!(!p.contains("<BASE>"));
        assert!(!p.contains("<INTEGRATION_BRANCH>"));
    }
}
