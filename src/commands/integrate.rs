// `grove integrate` — bring up an integration worktree + branch, snapshot
// dependency context, and spawn a Ralph-loop integration agent inside the
// devcontainer. The agent owns the merge loop.
//
// Compared to v1 (which ran the merge loop in Rust and shelled out
// `claude -p` per conflict, sometimes on the HOST), v2:
//
//   - Hard-requires the devcontainer. No host fallback. The resolver
//     (now an autonomous Ralph-loop agent) only ever runs sandboxed.
//   - Generates a dependency hint (branches.json + overlap.txt) so the
//     agent can make an informed merge-ordering decision.
//   - Spawns the agent with an integrate-specific bootstrap prompt that
//     dictates the conflict-resolution + PR-creation protocol.
//   - Exits after spawn; operator monitors via `grove attach integrate-<ts>`.
//
// The orchestrator's responsibilities are: worktree setup, branch creation,
// context snapshot (RO), agent state seed, agent spawn. Nothing more.

use std::path::Path;
use std::process::Command;

use chrono::Utc;
use colored::Colorize;

use crate::agent::integrate_deps::{
    compute_branch_metadata, pairwise_overlap, read_verify_command, resolve_base_sha,
    IntegrationContext,
};
use crate::agent::integrate_seed::{build_integrate_bootstrap_prompt, seed_integrate_agent};
use crate::agent::seed;
use crate::commands::spawn::{launch_agent_in_container, LaunchContext};
use crate::git::worktree_manager::{add_worktree, discover_repo, get_default_branch, project_root};
use crate::git::worktree_paths::make_worktree_pointers_relative;
use crate::models::GroveConfig;
use crate::session::container::{self, ContainerInfo};

pub fn run(into: Option<&str>, no_test: bool) {
    let ctx = match discover_repo() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} {}", "Error:".red(), e);
            std::process::exit(1);
        }
    };
    let project_root_path = project_root(&ctx).to_path_buf();

    // Resolve target branch (PR base).
    let base = match into
        .map(|s| s.to_string())
        .or_else(|| get_default_branch(&ctx).ok())
    {
        Some(b) => b,
        None => {
            eprintln!(
                "{} could not determine base branch; pass --into <branch>",
                "Error:".red()
            );
            std::process::exit(1);
        }
    };

    // List branches BEFORE creating the integration worktree so the agent
    // never tries to merge itself.
    let agent_branches = list_agent_branches(&project_root_path).unwrap_or_default();
    if agent_branches.is_empty() {
        println!(
            "{} no agent/* branches found; nothing to integrate",
            "Note:".yellow()
        );
        return;
    }

    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let integration_branch = format!("integration/{}", stamp);
    let agent_name = format!("integrate-{}", stamp);
    let integration_path = project_root_path.join("worktrees").join(".integration");
    if let Err(e) = std::fs::create_dir_all(integration_path.parent().unwrap()) {
        eprintln!("{} create worktrees/: {}", "Error:".red(), e);
        std::process::exit(1);
    }
    if integration_path.exists() {
        eprintln!(
            "{} {} already exists; remove it first (transient by design)",
            "Error:".red(),
            integration_path.display()
        );
        eprintln!(
            "  hint: `git worktree remove {} && git branch -D <previous-integration-branch>`",
            integration_path.display()
        );
        std::process::exit(1);
    }
    let integration_str = integration_path.to_string_lossy().to_string();

    // Container is mandatory. Resolver is an agent. Agents only run sandboxed.
    let container = ensure_container_up(&project_root_path);
    println!(
        "  {} devcontainer ready (workspace_target {})",
        "·".dimmed(),
        container.workspace_target.display()
    );

    // Branch the integration off `base`, then add the worktree on it.
    if let Err(e) = git_in(&project_root_path, &["branch", &integration_branch, &base]) {
        eprintln!(
            "{} could not create integration branch off {}: {}",
            "Error:".red(),
            base,
            e
        );
        std::process::exit(1);
    }
    if let Err(e) = add_worktree(&ctx, &integration_str, &integration_branch, false, None) {
        // Roll back the freshly-created branch ref.
        let _ = git_in(&project_root_path, &["branch", "-D", &integration_branch]);
        eprintln!(
            "{} create integration worktree on branch {}: {}",
            "Error:".red(),
            integration_branch,
            e
        );
        std::process::exit(1);
    }
    println!(
        "  {} integration worktree at {} on {}",
        "·".dimmed(),
        integration_path.display(),
        integration_branch.bold()
    );

    // Relativize the worktree's .git pointers (same reason as spawn: container
    // bind-mount path differs from host path).
    if let Err(e) = make_worktree_pointers_relative(&integration_path) {
        eprintln!(
            "  {} rewrite worktree pointers to relative: {}",
            "Warning:".yellow(),
            e
        );
    }

    // Symlink .grove into the worktree so Stop hook + framework docs resolve.
    if let Err(e) = seed::link_grove_into_worktree(&integration_path, &project_root_path) {
        eprintln!("  {} link .grove into worktree: {}", "Warning:".yellow(), e);
    }

    // Snapshot context (existing helper, RO chmod).
    if let Err(e) = snapshot_context(&project_root_path, &integration_path) {
        eprintln!(
            "{} snapshot bus/STATE: {} (continuing without context)",
            "Warning:".yellow(),
            e
        );
    } else {
        println!(
            "  {} snapshotted bus + STATE.md into {}",
            "·".dimmed(),
            integration_path.join(".grove-context").display()
        );
    }

    // Build branches.json + overlap.txt.
    let base_sha = resolve_base_sha(&project_root_path, &base).unwrap_or_default();
    let verify_cmd = if no_test {
        Vec::new()
    } else {
        read_verify_command(&project_root_path)
    };
    let branches = match compute_branch_metadata(&project_root_path, &agent_branches, &base) {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "{} compute branch metadata: {} (continuing with names only)",
                "Warning:".yellow(),
                e
            );
            agent_branches
                .iter()
                .map(|name| crate::agent::integrate_deps::BranchMeta {
                    name: name.clone(),
                    head_sha: String::new(),
                    files_changed: Vec::new(),
                    commit_count: 0,
                    tip_log: Vec::new(),
                })
                .collect()
        }
    };
    let integration_ctx = IntegrationContext {
        base: base.clone(),
        base_sha,
        integration_branch: integration_branch.clone(),
        verify_cmd: verify_cmd.clone(),
        no_test,
        branches,
    };

    let context_dir = integration_path.join(".grove-context");
    let _ = std::fs::create_dir_all(&context_dir);
    let json = integration_ctx.to_json().unwrap_or_default();
    if let Err(e) = std::fs::write(context_dir.join("branches.json"), json) {
        eprintln!(
            "  {} write branches.json: {} (agent will not have machine context)",
            "Warning:".yellow(),
            e
        );
    }
    let overlap = pairwise_overlap(&integration_ctx.branches);
    if let Err(e) = std::fs::write(context_dir.join("overlap.txt"), overlap) {
        eprintln!(
            "  {} write overlap.txt: {} (agent will not have overlap hint)",
            "Warning:".yellow(),
            e
        );
    }
    // Chmod the new files RO to match the rest of the context tree.
    let _ = make_context_readonly(&context_dir);
    println!(
        "  {} wrote branches.json + overlap.txt ({} branches)",
        "·".dimmed(),
        integration_ctx.branches.len()
    );

    // Seed the integrate-agent state.
    let agent_dir = match seed_integrate_agent(&project_root_path, &agent_name, &integration_ctx) {
        Ok(p) => {
            println!("  {} seeded {}", "·".dimmed(), p.display());
            p
        }
        Err(e) => {
            eprintln!(
                "{} seed integrate agent: {} (worktree still in place; remove with `git worktree remove`)",
                "Error:".red(),
                e
            );
            std::process::exit(1);
        }
    };

    // Build the bootstrap prompt with container-side paths.
    let container_worktree_path = container::host_to_container_path(&container, &integration_path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| integration_path.to_string_lossy().to_string());
    let container_agent_dir = container::host_to_container_path(&container, &agent_dir)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| agent_dir.to_string_lossy().to_string());
    let repo_name = project_root_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace");
    let bootstrap_prompt = build_integrate_bootstrap_prompt(
        &agent_name,
        repo_name,
        &container_worktree_path,
        &container_agent_dir,
        &integration_branch,
        &base,
    );

    // Hand off to the spawn machinery.
    launch_agent_in_container(&LaunchContext {
        agent_name: &agent_name,
        worktree_path: &integration_path,
        agent_dir: &agent_dir,
        container: &container,
        bootstrap_prompt: Some(&bootstrap_prompt),
        display_branch: &integration_branch,
        verb_past: "Started",
    });

    println!();
    println!(
        "{}",
        "The agent will now read context, plan a merge order, merge each branch, run verify, and open a PR."
            .dimmed()
    );
    println!(
        "{}",
        "Monitor: `grove agents status <name>` or `grove attach <name>`.".dimmed()
    );
}

fn snapshot_context(project: &Path, integration: &Path) -> Result<(), String> {
    let target = integration.join(".grove-context");
    std::fs::create_dir_all(&target).map_err(|e| format!("mkdir {}: {}", target.display(), e))?;
    let bus_src = project.join(".grove").join("bus");
    let bus_dst = target.join("bus");
    if bus_src.exists() {
        copy_dir(&bus_src, &bus_dst)?;
    }
    let agents_src = project.join(".grove").join("agents");
    let agents_dst = target.join("agents");
    if agents_src.exists() {
        std::fs::create_dir_all(&agents_dst)
            .map_err(|e| format!("mkdir {}: {}", agents_dst.display(), e))?;
        for entry in std::fs::read_dir(&agents_src).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            // Skip the orchestrator's own future agent (won't exist yet) and
            // any other integrate-* agent dirs to keep the snapshot focused
            // on the agent branches being merged.
            if let Some(s) = name.to_str() {
                if s.starts_with("integrate-") {
                    continue;
                }
            }
            let state_src = entry.path().join("STATE.md");
            if !state_src.exists() {
                continue;
            }
            let dst_dir = agents_dst.join(&name);
            std::fs::create_dir_all(&dst_dir).map_err(|e| e.to_string())?;
            let dst = dst_dir.join("STATE.md");
            std::fs::copy(&state_src, &dst).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// chmod the context tree RO so the agent can read but not mutate. Called
/// after branches.json + overlap.txt are written so they also get locked.
fn make_context_readonly(context_dir: &Path) -> Result<(), String> {
    make_readonly(context_dir)
}

fn copy_dir(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("mkdir {}: {}", dst.display(), e))?;
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target)?;
        } else {
            std::fs::copy(&path, &target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn make_readonly(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fn walk(p: &Path) -> Result<(), String> {
        for entry in std::fs::read_dir(p).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let pth = entry.path();
            if pth.is_dir() {
                walk(&pth)?;
            } else {
                let mut perms = std::fs::metadata(&pth)
                    .map_err(|e| e.to_string())?
                    .permissions();
                perms.set_mode(0o444);
                std::fs::set_permissions(&pth, perms).map_err(|e| e.to_string())?;
            }
        }
        let mut perms = std::fs::metadata(p)
            .map_err(|e| e.to_string())?
            .permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(p, perms).map_err(|e| e.to_string())?;
        Ok(())
    }
    walk(path)
}

#[cfg(not(unix))]
fn make_readonly(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn list_agent_branches(project: &Path) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .current_dir(project)
        .args(["branch", "--list", "--format=%(refname:short)", "agent/*"])
        .output()
        .map_err(|e| format!("git branch: {}", e))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).to_string());
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let mut branches: Vec<String> = raw
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "agent/shared")
        .collect();
    branches.sort();
    Ok(branches)
}

fn ensure_container_up(project_root: &Path) -> ContainerInfo {
    // Read the config first so we can fall back to it for workspace_target
    // when the CLI doesn't tell us.
    let _ = read_config(project_root); // ensure path exists; result currently unused
    match container::ensure_up(project_root) {
        Ok(info) => info,
        Err(e) => {
            eprintln!("{} `devcontainer up` failed: {}", "Error:".red(), e);
            eprintln!("  grove integrate requires a working devcontainer (the integration agent runs sandboxed).");
            eprintln!("  Install the devcontainer CLI (`npm i -g @devcontainers/cli`) and Docker, then retry.");
            std::process::exit(1);
        }
    }
}

fn read_config(project: &Path) -> GroveConfig {
    let path = project.join(".grove").join("config.toml");
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    toml::from_str(&raw).unwrap_or_default()
}

fn git_in(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {}", args.join(" "), e))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}
