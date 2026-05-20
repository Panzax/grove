// `grove integrate` — merge every agent/* branch into a disposable
// integration branch, with a headless Claude conflict resolver fed by bus +
// per-agent STATE.md context.
//
// Implementation port of `agent.sh cmd_integrate`.

use std::path::Path;
use std::process::Command;

use chrono::Utc;
use colored::Colorize;

use crate::git::worktree_manager::{add_worktree, discover_repo, get_default_branch, project_root};
use crate::models::GroveConfig;

pub fn run(into: Option<&str>, no_test: bool) {
    let ctx = match discover_repo() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} {}", "Error:".red(), e);
            std::process::exit(1);
        }
    };
    let project = project_root(&ctx).to_path_buf();

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

    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let integration_branch = format!("integration/{}", stamp);
    let integration_path = project.join("worktrees").join(".integration");
    if let Err(e) = std::fs::create_dir_all(integration_path.parent().unwrap()) {
        eprintln!("{} create worktrees/: {}", "Error:".red(), e);
        std::process::exit(1);
    }
    if integration_path.exists() {
        eprintln!(
            "{} {} already exists; remove it first (it is meant to be transient)",
            "Error:".red(),
            integration_path.display()
        );
        std::process::exit(1);
    }
    let integration_str = integration_path.to_string_lossy().to_string();
    // Branch the integration off `base` explicitly so the merges produce a clean
    // history relative to the user's base branch.
    if let Err(e) = git_in(&project, &["branch", &integration_branch, &base]) {
        eprintln!(
            "{} could not create integration branch off {}: {}",
            "Error:".red(),
            base,
            e
        );
        std::process::exit(1);
    }
    if let Err(e) = add_worktree(&ctx, &integration_str, &integration_branch, false, None) {
        eprintln!(
            "{} create integration worktree on branch {}: {}",
            "Error:".red(),
            integration_branch,
            e
        );
        std::process::exit(1);
    }

    // Snapshot bus + per-agent STATE.md BEFORE we start merging, so the
    // conflict resolver always sees the pre-integration intent.
    if let Err(e) = snapshot_context(&project, &integration_path) {
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

    // Reset base, then merge each agent branch.
    let agent_branches = list_agent_branches(&project).unwrap_or_default();
    if agent_branches.is_empty() {
        println!(
            "{} no agent/* branches found; nothing to integrate",
            "Note:".yellow()
        );
        return;
    }

    let config = read_config(&project);
    let verify_cmd = if no_test {
        None
    } else {
        first_verify_command(&config)
    };

    let mut merged = Vec::new();
    let mut failed = Vec::new();
    for branch in &agent_branches {
        println!();
        println!("{} merging {}", "▶".cyan(), branch.bold());
        match git_in(
            &integration_path,
            &["merge", "--no-ff", "--no-edit", branch],
        ) {
            Ok(_) => {
                println!("  {} merge clean", "✓".green());
                if let Some(cmd) = &verify_cmd {
                    if !run_verify(&integration_path, cmd) {
                        failed.push((branch.clone(), "verify failed".to_string()));
                        continue;
                    }
                }
                merged.push(branch.clone());
            }
            Err(e) => {
                eprintln!("  {} merge produced conflicts: {}", "!".yellow(), e);
                match resolve_conflicts(&integration_path) {
                    Ok(()) => {
                        println!("  {} conflicts resolved", "✓".green());
                        if let Some(cmd) = &verify_cmd {
                            if !run_verify(&integration_path, cmd) {
                                failed.push((branch.clone(), "verify failed".to_string()));
                                continue;
                            }
                        }
                        merged.push(branch.clone());
                    }
                    Err(re) => {
                        eprintln!("  {} {}", "Error:".red(), re);
                        // Try to abort the in-progress merge so the integration branch
                        // stays in a clean state.
                        let _ = git_in(&integration_path, &["merge", "--abort"]);
                        failed.push((branch.clone(), re));
                    }
                }
            }
        }
    }

    println!();
    println!(
        "{} integration branch: {}",
        "✓".green(),
        integration_branch.bold()
    );
    println!("  worktree: {}", integration_path.display());
    println!("  merged   : {}", merged.len());
    println!("  failed   : {}", failed.len());
    for (b, why) in &failed {
        println!("    - {} ({})", b, why);
    }
    println!();
    println!(
        "{}",
        "Review and (if happy) merge integration/<ts> into the base branch yourself.".dimmed()
    );
}

fn snapshot_context(project: &Path, integration: &Path) -> Result<(), String> {
    let target = integration.join(".grove-context");
    std::fs::create_dir_all(&target).map_err(|e| format!("mkdir {}: {}", target.display(), e))?;
    let bus_src = project.join(".grove").join("bus");
    let bus_dst = target.join("bus");
    if bus_src.exists() {
        copy_dir(&bus_src, &bus_dst)?;
        make_readonly(&bus_dst)?;
    }
    let agents_src = project.join(".grove").join("agents");
    let agents_dst = target.join("agents");
    if agents_src.exists() {
        std::fs::create_dir_all(&agents_dst)
            .map_err(|e| format!("mkdir {}: {}", agents_dst.display(), e))?;
        for entry in std::fs::read_dir(&agents_src).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            let state_src = entry.path().join("STATE.md");
            if !state_src.exists() {
                continue;
            }
            let dst_dir = agents_dst.join(&name);
            std::fs::create_dir_all(&dst_dir).map_err(|e| e.to_string())?;
            let dst = dst_dir.join("STATE.md");
            std::fs::copy(&state_src, &dst).map_err(|e| e.to_string())?;
        }
        make_readonly(&agents_dst)?;
    }
    Ok(())
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
        Ok(())
    }
    walk(path)
}

#[cfg(not(unix))]
fn make_readonly(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn list_agent_branches(project: &Path) -> Result<Vec<String>, String> {
    // We want branches named exactly `agent/*` (but NOT `agent/shared`, which is
    // the hub branch — its content gets in via the regular merges anyway).
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

fn read_config(project: &Path) -> GroveConfig {
    let path = project.join(".grove").join("config.toml");
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    toml::from_str(&raw).unwrap_or_default()
}

fn first_verify_command(config: &GroveConfig) -> Option<Vec<String>> {
    if !config.verify.test.is_empty() {
        Some(config.verify.test.clone())
    } else {
        None
    }
}

fn run_verify(integration: &Path, cmd: &[String]) -> bool {
    if cmd.is_empty() {
        return true;
    }
    println!("  {} verify: {}", "·".dimmed(), cmd.join(" ").bold());
    let mut iter = cmd.iter();
    let program = iter.next().unwrap();
    let args: Vec<&String> = iter.collect();
    let status = Command::new(program)
        .args(args.iter().map(|s| s.as_str()))
        .current_dir(integration)
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("  {} verify passed", "✓".green());
            true
        }
        Ok(s) => {
            eprintln!(
                "  {} verify failed (exit {})",
                "!".yellow(),
                s.code().unwrap_or(-1)
            );
            false
        }
        Err(e) => {
            eprintln!("  {} verify could not run: {}", "!".yellow(), e);
            false
        }
    }
}

/// Try to resolve conflicts with the headless Claude resolver. Honors
/// GROVE_RESOLVE_COMMAND for tests / alternative providers; defaults to
/// `claude -p` with a context-aware prompt. Falls back to leaving the
/// conflicts in place if no resolver is available.
fn resolve_conflicts(integration: &Path) -> Result<(), String> {
    let conflicted =
        git_in(integration, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
    if conflicted.trim().is_empty() {
        return Ok(());
    }
    let prompt = format!(
        "There are git merge conflicts inside this worktree (paths below). \
You have read-only access to .grove-context/{{bus,agents}}/ for intent context. \
Resolve each conflict to match the merging branches' intent, then `git add` the \
resolved files. Do NOT run `git commit` — the orchestrator will commit after \
verification. Conflicted files:\n{}",
        conflicted
    );

    let cmd_tokens = resolver_command_tokens();
    println!(
        "  {} resolving conflicts via {}",
        "·".dimmed(),
        cmd_tokens.join(" ")
    );
    let mut iter = cmd_tokens.iter();
    let program = iter.next().ok_or("resolver command empty")?;
    let args: Vec<&String> = iter.collect();
    let status = Command::new(program)
        .args(args.iter().map(|s| s.as_str()))
        .arg(&prompt)
        .current_dir(integration)
        .status()
        .map_err(|e| format!("invoke {}: {}", program, e))?;
    if !status.success() {
        return Err(format!("resolver exited {}", status.code().unwrap_or(-1)));
    }
    // Commit the resolved state to close out the merge.
    git_in(integration, &["commit", "--no-edit"])?;
    Ok(())
}

fn resolver_command_tokens() -> Vec<String> {
    if let Ok(raw) = std::env::var("GROVE_RESOLVE_COMMAND") {
        let tokens: Vec<String> = raw.split_whitespace().map(|s| s.to_string()).collect();
        if !tokens.is_empty() {
            return tokens;
        }
    }
    vec![
        "claude".into(),
        "-p".into(),
        "--dangerously-skip-permissions".into(),
    ]
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
