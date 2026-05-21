// `grove spawn <name>` — create an isolated worktree, seed an agent profile,
// and launch a Claude Code session bound to it.
//
// Builds on the same git primitives `grove add` uses (add_worktree +
// branch_exists), but with agent-aware seeding instead of bootstrap commands.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use colored::Colorize;

use crate::agent::seed;
use crate::git::worktree_manager::{
    add_worktree, branch_exists, discover_repo, layout, project_root,
};
use crate::git::worktree_paths::make_worktree_pointers_relative;
use crate::models::{AgentMetadata, ProjectLayout};
use crate::session::container::{self, ContainerInfo};
use crate::session::tmux::{launch_detached, SessionSpec};

const DEFAULT_MAX_ITERATIONS: u32 = 30;
const DEFAULT_PROMISE: &str = "All workitems in STATE.md are [x]";

pub fn run(
    name: &str,
    branch: Option<&str>,
    task: Option<&str>,
    promise: Option<&str>,
    max_iter: Option<u32>,
    no_bootstrap: bool,
) {
    let ctx = match discover_repo() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "{} grove spawn must run inside a grove-initialized project: {}",
                "Error:".red(),
                e
            );
            std::process::exit(1);
        }
    };

    if !seed::is_valid_agent_name(name) {
        eprintln!(
            "{} agent name '{}' must be kebab-case (letters, digits, '-', '_')",
            "Error:".red(),
            name
        );
        std::process::exit(1);
    }

    let project_root_path = project_root(&ctx).to_path_buf();

    // Layout-aware worktree placement.
    let worktree_path: PathBuf = match layout(&ctx) {
        ProjectLayout::Bare => project_root_path.join(name),
        ProjectLayout::InPlace => {
            let nested = project_root_path.join("worktrees");
            if let Err(e) = std::fs::create_dir_all(&nested) {
                eprintln!("{} create worktrees/: {}", "Error:".red(), e);
                std::process::exit(1);
            }
            nested.join(name)
        }
    };
    let worktree_path_str = worktree_path.to_string_lossy().to_string();

    let agent_dir = project_root_path.join(".grove").join("agents").join(name);

    // Container is mandatory. Bring it up before any tmux-liveness probes.
    let container = ensure_container_up(&project_root_path);
    println!(
        "  {} devcontainer ready (workspace_target {})",
        "·".dimmed(),
        container.workspace_target.display()
    );
    if !tool_in_container(&container, "tmux") {
        eprintln!("{} tmux is missing inside the container.", "Error:".red());
        eprintln!("  Run `grove devcontainer doctor` to audit (tmux, jq, perl, claude),");
        eprintln!("  then `grove devcontainer rebuild` after fixing devcontainer.json.");
        std::process::exit(1);
    }

    // Refuse to spawn if a session for this name is already live in the
    // container — the running agent owns its worktree/loop.md/state.
    let session_name = crate::session::tmux::session_name(name);
    if crate::session::tmux::has_session(&session_name, Some(&container)).unwrap_or(false) {
        eprintln!(
            "{} agent '{}' is already running (tmux session {} alive). Run `grove agents kill {}` first if you want to restart it.",
            "Error:".red(),
            name,
            session_name,
            name
        );
        std::process::exit(1);
    }

    // Two flows from here: RESUME if the agent dir already exists, FRESH otherwise.
    let resume = agent_dir.exists();
    let (final_agent_dir, target_branch) = if resume {
        resume_agent(
            &ctx,
            &project_root_path,
            &worktree_path,
            &worktree_path_str,
            &agent_dir,
            name,
            branch,
            task,
        )
    } else {
        fresh_agent(
            &ctx,
            &project_root_path,
            &worktree_path,
            &worktree_path_str,
            name,
            branch,
            task,
            promise.unwrap_or(DEFAULT_PROMISE),
            max_iter.unwrap_or(DEFAULT_MAX_ITERATIONS),
        )
    };
    let agent_dir = final_agent_dir;

    // Build the bootstrap prompt (Option<String>) unless suppressed by
    // --no-bootstrap. `grove integrate` will reuse `launch_agent_in_container`
    // with its own pre-built bootstrap prompt.
    let bootstrap_prompt = if no_bootstrap {
        None
    } else {
        let container_worktree_path = container::host_to_container_path(&container, &worktree_path)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| worktree_path.to_string_lossy().to_string());
        let container_agent_dir = container::host_to_container_path(&container, &agent_dir)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| agent_dir.to_string_lossy().to_string());
        let repo_name = project_root_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("workspace");
        let spec = crate::agent::bootstrap::BootstrapSpec {
            agent_name: name,
            repo_name,
            container_worktree_path: &container_worktree_path,
            container_agent_dir: &container_agent_dir,
            task,
            resume,
        };
        let prompt = crate::agent::bootstrap::build_bootstrap_prompt(&spec);
        println!(
            "  {} bootstrap prompt injected ({})",
            "·".dimmed(),
            if resume {
                "resume"
            } else if task.is_some() {
                "fresh + task"
            } else {
                "fresh + no-task"
            }
        );
        Some(prompt)
    };

    let verb = if resume { "Resumed" } else { "Spawned" };
    launch_agent_in_container(&LaunchContext {
        agent_name: name,
        worktree_path: &worktree_path,
        agent_dir: &agent_dir,
        container: &container,
        bootstrap_prompt: bootstrap_prompt.as_deref(),
        display_branch: &target_branch,
        verb_past: verb,
    });

    println!();
    if resume {
        println!(
            "{}",
            "Loop resumed from previous state. PROMPT.md / STATE.md / loop.md unchanged.".dimmed()
        );
    } else {
        println!(
            "{}",
            "Next: edit PROMPT.md / STATE.md, then flip loop.md `active: true` to start the loop."
                .dimmed()
        );
    }
}

/// Context for `launch_agent_in_container` — bundle the args so spawn and
/// integrate can both call it without a 7-arg signature.
pub(crate) struct LaunchContext<'a> {
    pub agent_name: &'a str,
    pub worktree_path: &'a Path,
    pub agent_dir: &'a Path,
    pub container: &'a ContainerInfo,
    /// Bootstrap prompt appended as claude's initial user message.
    /// `None` means launch claude without an initial prompt (raw session).
    pub bootstrap_prompt: Option<&'a str>,
    /// Branch name shown in the success banner. Caller's choice — `grove
    /// spawn` passes the worktree's branch, `grove integrate` passes the
    /// integration branch.
    pub display_branch: &'a str,
    /// Past-tense verb in the success banner: "Spawned", "Resumed",
    /// "Started", etc.
    pub verb_past: &'a str,
}

/// Build the env + cmd_tokens, construct the SessionSpec, and call
/// `launch_detached` inside the project's devcontainer. Prints success or
/// fallback-manual-launch instructions. Reused by `grove spawn` and
/// `grove integrate`.
///
/// Side effects beyond launch:
///   - Writes a launch summary to `.grove/logs/launch-<agent>-<ts>.log`
///     (host path; works because `.grove/` is in the workspace bind-mount).
///     Captures: container info, env vars, the rendered claude command,
///     tmux launch exit status, attach instructions. Diagnostic record
///     for when "agent didn't start" or "agent died immediately" — past
///     versions provided no trace.
///   - Wraps claude in `bash -c "exec claude ... 2>&1 | tee <pane.log>"`
///     so claude's stdout + stderr ALSO get archived to
///     `.grove/logs/pane-<agent>-<ts>.log`. Lets the operator (and
///     agents grepping their own log) see WHY a session exited even
///     after tmux killed the session.
pub(crate) fn launch_agent_in_container(ctx: &LaunchContext<'_>) {
    // Logs live under the project root (host side); workspace bind-mount
    // makes them visible inside the container too. agent_dir's parent
    // walks back to `<project_root>/.grove/agents/<name>` → up two →
    // `<project_root>/.grove`.
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let grove_dir = ctx
        .agent_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| ctx.agent_dir.to_path_buf());
    let log_dir = grove_dir.join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let launch_log = log_dir.join(format!("launch-{}-{}.log", ctx.agent_name, stamp));
    let pane_log = log_dir.join(format!("pane-{}-{}.log", ctx.agent_name, stamp));

    let mut env: HashMap<String, String> = HashMap::new();
    env.insert(
        "GROVE_AGENT_DIR".into(),
        ctx.agent_dir.to_string_lossy().to_string(),
    );
    env.insert("GROVE_AGENT_NAME".into(), ctx.agent_name.to_string());

    // Wrap claude in a bash + tee pipeline so claude's stdout/stderr land
    // in pane-<agent>-<ts>.log inside the container (path is the
    // container-side workspace, which the bind-mount surfaces back to host
    // disk). Translation: agent_dir is on host → grove_dir parent → log_dir
    // on host. Inside the container we re-build the same path with the
    // workspace_target prefix.
    let container_pane_log = container::host_to_container_path(ctx.container, &pane_log)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| pane_log.to_string_lossy().to_string());

    let base_cmd_tokens = launch_command_tokens();
    let mut quoted = base_cmd_tokens
        .iter()
        .map(|t| shell_escape(t))
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(prompt) = ctx.bootstrap_prompt {
        quoted.push(' ');
        quoted.push_str(&shell_escape(prompt));
    }
    let pane_log_quoted = shell_escape(&container_pane_log);
    // `mkdir -p` for the log dir inside the container; exec wraps so signals
    // pass through cleanly; pipefail keeps claude's exit code as the session
    // exit code (not tee's). 2>&1 merges stderr so both streams archive.
    let inner = format!(
        "mkdir -p \"$(dirname {pane})\" && set -o pipefail && exec {cmd} 2>&1 | tee -a {pane}",
        cmd = quoted,
        pane = pane_log_quoted
    );
    let cmd_tokens: Vec<String> = vec!["bash".into(), "-lc".into(), inner];

    let spec = SessionSpec {
        name: ctx.agent_name,
        workdir: ctx.worktree_path,
        env: env.clone(),
        command: cmd_tokens.clone(),
    };

    let cmd_summary = summarize_command(&cmd_tokens);
    let launch_result = launch_detached(&spec, Some(ctx.container));

    // Always write the launch log regardless of outcome — diagnosis of a
    // failed launch is exactly when the log matters most.
    write_launch_log(
        &launch_log,
        ctx,
        &env,
        &cmd_tokens,
        &pane_log,
        &container_pane_log,
        &launch_result,
    );

    match launch_result {
        Ok(session_name_str) => {
            println!(
                "{} {} agent {} on {} (tmux {} {}) [in container]",
                "✓".green(),
                ctx.verb_past,
                ctx.agent_name.bold(),
                ctx.display_branch.bold(),
                session_name_str.bold(),
                cmd_summary.dimmed()
            );
            println!(
                "  {} launch log: {}",
                "·".dimmed(),
                launch_log.display().to_string().bold()
            );
            println!(
                "  {} session output: {}",
                "·".dimmed(),
                pane_log.display().to_string().bold()
            );
            println!(
                "  attach: {}",
                crate::session::tmux::attach_instructions(ctx.agent_name, Some(ctx.container))
            );
        }
        Err(e) => {
            eprintln!(
                "{} could not launch tmux session: {}",
                "Warning:".yellow(),
                e
            );
            eprintln!(
                "  {} launch log (with full context): {}",
                "·".dimmed(),
                launch_log.display()
            );
            println!(
                "  the worktree + agent dir are still in place; you can launch claude manually:"
            );
            println!(
                "    cd {} && GROVE_AGENT_DIR={} {}",
                ctx.worktree_path.display(),
                ctx.agent_dir.display(),
                cmd_summary
            );
        }
    }
}

/// Emit the launch-time diagnostic log. Captures the rendered command (with
/// the bootstrap prompt truncated to keep the log readable), all env vars,
/// container info, and the tmux launch outcome.
fn write_launch_log(
    log_path: &Path,
    ctx: &LaunchContext<'_>,
    env: &HashMap<String, String>,
    cmd_tokens: &[String],
    pane_log: &Path,
    container_pane_log: &str,
    result: &Result<String, String>,
) {
    let mut body = String::new();
    body.push_str(&format!(
        "grove launch log\nagent: {}\nworktree (host): {}\nagent_dir (host): {}\nstamp: {}\ncontainer workspace_root: {}\ncontainer workspace_target: {}\ncontainer remote_user: {}\ndisplay_branch: {}\nverb: {}\nbootstrap_prompt: {}\n\n",
        ctx.agent_name,
        ctx.worktree_path.display(),
        ctx.agent_dir.display(),
        chrono::Utc::now().to_rfc3339(),
        ctx.container.workspace_root.display(),
        ctx.container.workspace_target.display(),
        ctx.container.remote_user,
        ctx.display_branch,
        ctx.verb_past,
        if ctx.bootstrap_prompt.is_some() { "yes" } else { "no" },
    ));
    body.push_str("environment passed to tmux session:\n");
    let mut keys: Vec<&String> = env.keys().collect();
    keys.sort();
    for k in keys {
        body.push_str(&format!("  {}={}\n", k, env.get(k).unwrap()));
    }
    body.push_str("\n");
    body.push_str(&format!(
        "pane log (host): {}\npane log (container path): {}\n\n",
        pane_log.display(),
        container_pane_log,
    ));
    body.push_str("rendered tmux command tokens:\n");
    for tok in cmd_tokens {
        body.push_str(&format!("  {}\n", summarize_one(tok)));
    }
    body.push_str("\n");
    match result {
        Ok(session) => body.push_str(&format!("tmux launch: OK (session={})\n", session)),
        Err(e) => body.push_str(&format!("tmux launch: FAILED ({})\n", e)),
    }
    body.push_str("\nattach: ");
    body.push_str(&crate::session::tmux::attach_instructions(
        ctx.agent_name,
        Some(ctx.container),
    ));
    body.push('\n');
    if let Err(e) = std::fs::write(log_path, body) {
        eprintln!(
            "  {} could not write launch log {}: {}",
            "Warning:".yellow(),
            log_path.display(),
            e
        );
    }
}

/// Render a single token for log display: abbreviate long ones (the
/// bootstrap prompt is multi-KB; full text is in PROMPT.md / STATE.md
/// already).
fn summarize_one(tok: &str) -> String {
    if tok.len() <= 200 {
        tok.to_string()
    } else {
        let head: String = tok.chars().take(80).collect();
        format!("'{}…' ({} chars)", head, tok.len())
    }
}

/// Minimal POSIX shell-escape for the inner `bash -c` script. Single-quote
/// wraps everything except plain identifier-shaped tokens.
fn shell_escape(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_' | '=' | ':'))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Fresh agent path. Creates worktree, seeds .grove/agents/<n>/, writes agent.toml.
#[allow(clippy::too_many_arguments)]
fn fresh_agent(
    ctx: &crate::git::worktree_manager::RepoContext,
    project_root_path: &Path,
    worktree_path: &Path,
    worktree_path_str: &str,
    name: &str,
    branch: Option<&str>,
    task: Option<&str>,
    promise: &str,
    max_iter: u32,
) -> (PathBuf, String) {
    // Resolve target branch.
    let (target_branch, create_new) = match branch {
        Some(b) => {
            if !branch_exists(ctx, b) {
                eprintln!(
                    "{} --branch {} does not exist. Create it first or omit --branch to use agent/{}.",
                    "Error:".red(),
                    b,
                    name
                );
                std::process::exit(1);
            }
            if let Some(other_wt) = branch_already_checked_out(project_root_path, b) {
                eprintln!(
                    "{} --branch {} is already checked out at {} (git allows only one worktree per branch).",
                    "Error:".red(),
                    b,
                    other_wt
                );
                std::process::exit(1);
            }
            (b.to_string(), false)
        }
        None => {
            let agent_branch = format!("agent/{}", name);
            let exists = branch_exists(ctx, &agent_branch);
            (agent_branch, !exists)
        }
    };

    if let Err(e) = add_worktree(ctx, worktree_path_str, &target_branch, create_new, None) {
        eprintln!("{} create worktree: {}", "Error:".red(), e);
        std::process::exit(1);
    }
    println!(
        "  {} worktree at {} on {}",
        "·".dimmed(),
        worktree_path.display(),
        target_branch.bold()
    );

    // Rewrite the two .git pointer files (forward + back) to use RELATIVE
    // paths so the worktree resolves identically on host and inside the
    // devcontainer (host /home/u/proj vs container /workspaces/proj).
    if let Err(e) = make_worktree_pointers_relative(worktree_path) {
        eprintln!(
            "  {} rewrite worktree pointers to relative: {} (git ops inside the container may fail)",
            "Warning:".yellow(),
            e
        );
    } else {
        println!(
            "  {} relativized .git pointers (works under host + container)",
            "·".dimmed()
        );
    }

    if let Err(e) = seed::link_grove_into_worktree(worktree_path, project_root_path) {
        eprintln!("  {} link .grove into worktree: {}", "Warning:".yellow(), e);
    } else {
        println!(
            "  {} linked .grove -> {}/.grove",
            "·".dimmed(),
            project_root_path.display()
        );
    }

    let agent_dir = match seed::seed_agent(project_root_path, name, task, promise, max_iter) {
        Ok(p) => {
            println!("  {} seeded {}", "·".dimmed(), p.display());
            p
        }
        Err(e) => {
            eprintln!(
                "{} seed agent state: {} (worktree still in place; remove with `grove remove`).",
                "Error:".red(),
                e
            );
            std::process::exit(1);
        }
    };

    let metadata = AgentMetadata {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.to_string(),
        worktree: worktree_path_str.to_string(),
        branch: target_branch.clone(),
        task: task.map(|s| s.to_string()),
        tmux_session: Some(crate::session::tmux::session_name(name)),
        spawned_at: Utc::now(),
        provider: "claude-code".to_string(),
    };
    let agent_toml = agent_dir.join("agent.toml");
    let body = match toml::to_string_pretty(&metadata) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{} serialize agent.toml: {}", "Error:".red(), e);
            let _ = std::fs::remove_dir_all(&agent_dir);
            std::process::exit(1);
        }
    };
    if let Err(e) = std::fs::write(&agent_toml, body) {
        eprintln!(
            "{} write agent.toml: {} — rolling back seeded agent dir.",
            "Error:".red(),
            e
        );
        let _ = std::fs::remove_dir_all(&agent_dir);
        std::process::exit(1);
    }
    (agent_dir, target_branch)
}

/// Resume agent path. Re-uses the existing .grove/agents/<n>/ state.
///
/// Repair semantics (handles crashes / partial state):
/// - Re-create the worktree if it was removed by `grove remove`.
/// - Re-create the .grove symlink if it's gone.
/// - Clear loop.md `session_id` so the Stop hook accepts the new session.
/// - Preserve PROMPT.md, STATE.md, agent.toml — user / agent edits survive.
///
/// `--branch`, `--task`, `--promise`, `--max-iter` are IGNORED on resume to
/// avoid silent drift from the recorded agent.toml + loop.md. To change
/// these, edit the files directly or `grove agents purge <name>` and respawn.
#[allow(clippy::too_many_arguments)]
fn resume_agent(
    ctx: &crate::git::worktree_manager::RepoContext,
    project_root_path: &Path,
    worktree_path: &Path,
    worktree_path_str: &str,
    agent_dir: &Path,
    name: &str,
    branch_override: Option<&str>,
    task_override: Option<&str>,
) -> (PathBuf, String) {
    // Read recorded agent.toml for branch + worktree. If agent.toml is
    // missing (older grove version, partial state), fall back to defaults.
    let agent_toml = agent_dir.join("agent.toml");
    let recorded_branch = if agent_toml.exists() {
        std::fs::read_to_string(&agent_toml)
            .ok()
            .and_then(|raw| toml::from_str::<AgentMetadata>(&raw).ok())
            .map(|m| m.branch)
            .unwrap_or_else(|| format!("agent/{}", name))
    } else {
        format!("agent/{}", name)
    };

    if branch_override.is_some() && branch_override != Some(recorded_branch.as_str()) {
        eprintln!(
            "  {} --branch ignored on resume (agent is recorded against {}). Edit .grove/agents/{}/agent.toml or purge + respawn to change.",
            "Note:".yellow(),
            recorded_branch,
            name
        );
    }
    if task_override.is_some() {
        eprintln!(
            "  {} --task ignored on resume (STATE.md already seeded). Edit STATE.md to add new workitems.",
            "Note:".yellow()
        );
    }

    // Re-add the worktree if it's gone (e.g. user ran `grove remove` then
    // `grove spawn` to resume). add_worktree refuses if the worktree already
    // exists, so we only call it when the dir is missing.
    if !worktree_path.exists() {
        let create_new = !branch_exists(ctx, &recorded_branch);
        if let Err(e) = add_worktree(ctx, worktree_path_str, &recorded_branch, create_new, None) {
            eprintln!("{} recreate worktree on resume: {}", "Error:".red(), e);
            std::process::exit(1);
        }
        println!(
            "  {} re-created worktree at {} on {}",
            "·".dimmed(),
            worktree_path.display(),
            recorded_branch.bold()
        );
    }

    // Always relativize pointers on resume — covers two cases: (1) a fresh
    // re-create above; (2) a worktree from an older grove that wrote absolute
    // paths. Idempotent when already relative.
    if let Err(e) = make_worktree_pointers_relative(worktree_path) {
        eprintln!(
            "  {} rewrite worktree pointers to relative: {}",
            "Warning:".yellow(),
            e
        );
    }

    // Re-link .grove (idempotent — Ok if symlink exists).
    if let Err(e) = seed::link_grove_into_worktree(worktree_path, project_root_path) {
        eprintln!("  {} link .grove into worktree: {}", "Warning:".yellow(), e);
    }

    // Clear stale session_id in loop.md so the Stop hook's isolation guard
    // doesn't silently reject the new claude session.
    let loop_path = agent_dir.join("loop.md");
    if let Err(e) = crate::agent::loop_md::clear_session_id(&loop_path) {
        eprintln!(
            "  {} clear loop.md session_id: {} (you may need to edit manually)",
            "Warning:".yellow(),
            e
        );
    } else {
        println!(
            "  {} cleared stale session_id in loop.md (hook will accept the new session)",
            "·".dimmed()
        );
    }

    (agent_dir.to_path_buf(), recorded_branch)
}

/// Returns the path of the worktree that already has `branch` checked out, if any.
/// Walks `git worktree list --porcelain` against the project root (works for both
/// bare and in-place layouts via cwd handling).
fn branch_already_checked_out(project_root: &Path, branch: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(project_root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let mut current_path: Option<String> = None;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            current_path = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("branch ") {
            let trimmed = rest.trim_start_matches("refs/heads/");
            if trimmed == branch {
                return current_path.clone();
            }
        }
    }
    None
}

/// Bring the project's devcontainer up. Hard-errors on failure — grove is an
/// agentic tool, agents run inside the container, no devcontainer means no
/// grove.
fn ensure_container_up(project_root: &Path) -> ContainerInfo {
    match container::ensure_up(project_root) {
        Ok(info) => info,
        Err(e) => {
            eprintln!("{} `devcontainer up` failed: {}", "Error:".red(), e);
            eprintln!("  grove requires a working devcontainer. Install the devcontainer CLI ");
            eprintln!("  (`npm i -g @devcontainers/cli`) and Docker, then retry.");
            std::process::exit(1);
        }
    }
}

/// Probe whether `tool` is on PATH inside the running container. Used to
/// hard-fail spawn when a prereq is missing rather than silently fall back
/// to host tmux.
fn tool_in_container(info: &ContainerInfo, tool: &str) -> bool {
    let script = format!("command -v {} >/dev/null 2>&1", tool);
    let argv: Vec<&str> = vec!["sh", "-c", &script];
    container::exec(info, &argv)
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Render `cmd_tokens` for human display without flooding the terminal with
/// the multi-KB bootstrap prompt. Tokens shorter than 200 chars are kept
/// verbatim; longer ones are abbreviated to first 60 chars + "…".
fn summarize_command(cmd_tokens: &[String]) -> String {
    cmd_tokens
        .iter()
        .map(|tok| {
            if tok.len() <= 200 {
                tok.clone()
            } else {
                let head: String = tok.chars().take(60).collect();
                format!("'{}…' ({} chars)", head, tok.len())
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Command vec passed to tmux. Honors `GROVE_AGENT_COMMAND` env override so tests
/// can substitute `bash` or `echo` for `claude`.
fn launch_command_tokens() -> Vec<String> {
    launch_command_tokens_with(std::env::var("GROVE_AGENT_COMMAND").ok().as_deref())
}

/// Inner helper that accepts the override directly. Tests call this so they
/// never mutate the global `GROVE_AGENT_COMMAND` env var (parallel-test race).
fn launch_command_tokens_with(override_value: Option<&str>) -> Vec<String> {
    if let Some(raw) = override_value {
        let tokens: Vec<String> = raw.split_whitespace().map(|s| s.to_string()).collect();
        if !tokens.is_empty() {
            return tokens;
        }
    }
    vec!["claude".into(), "--dangerously-skip-permissions".into()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_command_uses_claude() {
        let tokens = launch_command_tokens_with(None);
        assert_eq!(tokens[0], "claude");
        assert_eq!(tokens[1], "--dangerously-skip-permissions");
    }

    #[test]
    fn env_override_picks_up_tokens() {
        let tokens = launch_command_tokens_with(Some("bash -c 'sleep 30'"));
        assert_eq!(tokens[0], "bash");
        assert!(tokens.iter().any(|t| t.contains("sleep")));
    }

    #[test]
    fn empty_override_falls_back_to_default() {
        let tokens = launch_command_tokens_with(Some("   "));
        assert_eq!(tokens[0], "claude");
    }
}
