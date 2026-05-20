use colored::Colorize;
use std::fs;
use std::path::{Path, PathBuf};

use crate::devcontainer;
use crate::git::clone_bare_repository;
use crate::git::worktree_manager;
use crate::models::{GroveConfig, ProjectContext};
use crate::utils::{extract_repo_name, find_grove_repo};

/// `grove init` entry point. Two surface modes:
///
/// 1. `grove init <git_url>`              — bare clone + Phase 1 scaffold (+ Phase 2 wizard
///                                          unless --no-agent).
/// 2. `grove init --reconfigure`          — Phase 2 wizard only, against an existing
///                                          grove-initialized project. No clone.
pub fn run(git_url: Option<&str>, no_agent: bool, no_devcontainer: bool, reconfigure: bool) {
    if reconfigure {
        run_reconfigure();
        return;
    }

    let Some(git_url) = git_url else {
        eprintln!(
            "{} grove init requires a git URL (or --reconfigure to re-run the wizard).",
            "Error:".red()
        );
        std::process::exit(1);
    };

    if let Some(existing) = find_grove_repo(None) {
        eprintln!(
            "{} Cannot initialize grove inside an existing grove repository.\nDetected grove repository at: {}\n\nTo create a new grove setup, run 'grove init' from outside this directory hierarchy.",
            "Error:".red(),
            existing.display()
        );
        std::process::exit(1);
    }

    let repo_name = match extract_repo_name(git_url) {
        Ok(name) => name,
        Err(e) => {
            eprintln!("{} {}", "Error:".red(), e);
            std::process::exit(1);
        }
    };

    let mut created_dir = false;
    if !Path::new(&repo_name).exists() {
        if let Err(e) = fs::create_dir_all(&repo_name) {
            eprintln!("{} Failed to create directory: {}", "Error:".red(), e);
            std::process::exit(1);
        }
        created_dir = true;
    }

    let bare_repo_dir = format!("{}/{}.git", repo_name, repo_name);
    if Path::new(&bare_repo_dir).exists() {
        eprintln!(
            "{} Directory {} already exists",
            "Error:".red(),
            bare_repo_dir
        );
        std::process::exit(1);
    }

    if let Err(e) = clone_bare_repository(git_url, &bare_repo_dir) {
        if created_dir {
            let _ = fs::remove_dir_all(&repo_name);
        }
        eprintln!("{} {}", "Error:".red(), e);
        std::process::exit(1);
    }

    println!(
        "{} {}",
        "✓ Cloned bare repository:".green(),
        bare_repo_dir.bold()
    );

    // ---- Phase 1: deterministic scaffold ----
    // Re-discover the bare clone so all worktree_manager helpers work.
    let project_root_path = PathBuf::from(&repo_name);
    let original_cwd = std::env::current_dir().ok();
    if std::env::set_current_dir(&project_root_path).is_err() {
        eprintln!(
            "{} Could not enter project directory {}",
            "Warning:".yellow(),
            project_root_path.display()
        );
        return;
    }

    let context = match worktree_manager::discover_repo() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "{} Could not discover the bare clone after init: {}",
                "Warning:".yellow(),
                e
            );
            if let Some(cwd) = original_cwd {
                let _ = std::env::set_current_dir(cwd);
            }
            return;
        }
    };

    let project = devcontainer::detect_project_context(&context, &repo_name);
    print_detection_summary(&project);

    if no_devcontainer {
        println!(
            "  {} --no-devcontainer set; skipping .devcontainer/ scaffold.",
            "·".dimmed()
        );
    } else {
        match devcontainer::scaffold_devcontainer(&context, &project) {
            Ok(true) => println!("  {} wrote .devcontainer/devcontainer.json", "·".dimmed()),
            Ok(false) => println!(
                "  {} .devcontainer/devcontainer.json already exists; left as-is",
                "·".dimmed()
            ),
            Err(e) => eprintln!("  {} devcontainer scaffold failed: {}", "Warning:".yellow(), e),
        }
    }

    if let Err(e) = write_grove_config(
        worktree_manager::project_root(&context),
        &project,
        &context,
    ) {
        eprintln!("  {} failed to write .grove/config.toml: {}", "Warning:".yellow(), e);
    } else {
        println!("  {} wrote .grove/config.toml", "·".dimmed());
    }

    if let Err(e) = ensure_groverc_bootstrap(worktree_manager::project_root(&context)) {
        eprintln!("  {} failed to update .groverc: {}", "Warning:".yellow(), e);
    } else {
        println!("  {} updated .groverc bootstrap commands", "·".dimmed());
    }

    if let Err(e) = patch_gitignore(worktree_manager::project_root(&context)) {
        eprintln!("  {} failed to patch .gitignore: {}", "Warning:".yellow(), e);
    } else {
        println!("  {} patched .gitignore (.grove/, worktrees/)", "·".dimmed());
    }

    if project.has_dockerfile {
        if let Err(e) = patch_dockerignore(worktree_manager::project_root(&context)) {
            eprintln!("  {} failed to patch .dockerignore: {}", "Warning:".yellow(), e);
        } else {
            println!(
                "  {} patched .dockerignore (excludes .grove/ + worktrees/)",
                "·".dimmed()
            );
        }
    }

    if !no_devcontainer {
        if let Err(e) =
            apply_cache_volumes_to_devcontainer(worktree_manager::project_root(&context), &project)
        {
            eprintln!(
                "  {} failed to add cache volumes to devcontainer.json: {}",
                "Warning:".yellow(),
                e
            );
        } else {
            println!("  {} applied named cache volumes to devcontainer.json", "·".dimmed());
        }
    }

    // Install the Stop-hook engine into the project + register the hook in user
    // settings. These are idempotent so repeated `grove init` runs are safe.
    match crate::agent::hook::install_engine(&context) {
        Ok(p) => println!("  {} installed engine at {}", "·".dimmed(), p.display()),
        Err(e) => eprintln!("  {} failed to install loop engine: {}", "Warning:".yellow(), e),
    }
    match crate::agent::seed::install_assets(&context) {
        Ok(paths) => println!(
            "  {} installed framework files (.grove/{{PROTOCOL,RALPH-LOOP,SHARED,PROMPT.template}}.md): {} written",
            "·".dimmed(),
            paths.len()
        ),
        Err(e) => eprintln!(
            "  {} failed to install framework files: {}",
            "Warning:".yellow(),
            e
        ),
    }
    match crate::agent::hook::default_user_settings_path() {
        Some(path) => match crate::agent::hook::install_stop_hook(
            &path,
            crate::agent::hook::HOOK_COMMAND,
        ) {
            Ok(report) => {
                if report.added {
                    println!(
                        "  {} registered Stop hook in {} (total Stop entries: {})",
                        "·".dimmed(),
                        report.path.display(),
                        report.total_stop_hooks
                    );
                } else {
                    println!(
                        "  {} Stop hook already present in {} ({} total)",
                        "·".dimmed(),
                        report.path.display(),
                        report.total_stop_hooks
                    );
                }
            }
            Err(e) => eprintln!(
                "  {} could not install Stop hook (set CLAUDE_HOME or edit ~/.claude/settings.json manually): {}",
                "Warning:".yellow(),
                e
            ),
        },
        None => eprintln!(
            "  {} could not locate ~/.claude/settings.json; skipping Stop hook registration",
            "Warning:".yellow()
        ),
    }

    // ---- Phase 2 hook: setup wizard ----
    if no_agent {
        println!(
            "  {} --no-agent set; skipping the Phase 2 setup wizard.",
            "·".dimmed()
        );
    } else {
        // Phase 2 dispatcher lives in crate::agent::setup; not yet built — fail soft.
        match crate::agent::setup::run_setup_wizard(&context, &project, false) {
            Ok(()) => {}
            Err(e) => eprintln!(
                "  {} setup wizard did not complete: {} (you can retry with `grove init --reconfigure`)",
                "Warning:".yellow(),
                e
            ),
        }
    }

    println!();
    println!("{}", "Next steps:".bold());
    println!("  {} {}", "cd".dimmed(), repo_name);
    println!("  {} <name>                  # create a regular worktree", "grove add".dimmed());
    println!(
        "  {} <name> --task \"...\"   # spawn an agent in an isolated worktree",
        "grove spawn".dimmed()
    );

    if let Some(cwd) = original_cwd {
        let _ = std::env::set_current_dir(cwd);
    }
}

/// `grove init --reconfigure` — re-run Phase 2 only.
fn run_reconfigure() {
    let context = match worktree_manager::discover_repo() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "{} grove init --reconfigure must run inside a grove-initialized project: {}",
                "Error:".red(),
                e
            );
            std::process::exit(1);
        }
    };

    let repo_root = worktree_manager::project_root(&context).to_path_buf();
    let repo_name = repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string();
    let project = devcontainer::detect_project_context(&context, &repo_name);
    print_detection_summary(&project);

    match crate::agent::setup::run_setup_wizard(&context, &project, true) {
        Ok(()) => {}
        Err(e) => eprintln!(
            "  {} setup wizard did not complete: {}",
            "Warning:".yellow(),
            e
        ),
    }
}

fn print_detection_summary(project: &ProjectContext) {
    let stack = project
        .stack
        .map(|s| s.as_str())
        .unwrap_or("unknown");
    let pm = project.package_manager.as_deref().unwrap_or("none");
    let tc = project.toolchain_version.as_deref().unwrap_or("unspecified");
    println!(
        "{} stack={} pm={} toolchain={} tests={} dockerfile={} CLAUDE.md={}",
        "Detected:".dimmed(),
        stack,
        pm,
        tc,
        project.has_tests,
        project.has_dockerfile,
        project.has_claude_md
    );
}

/// Build the in-memory `.grove/config.toml` view from a detected ProjectContext
/// and a (possibly empty) CI-parity scrape result. Pure function — easy to test.
fn build_grove_config(
    project: &ProjectContext,
    scraped: crate::devcontainer::ci_scrape::ScrapeResult,
) -> GroveConfig {
    let mut config = GroveConfig::default();
    if let Some(s) = project.stack {
        config.stack.detected = Some(s.as_str().to_string());
    }
    config.stack.toolchain = project.toolchain_version.clone();
    config.stack.package_mgr = project.package_manager.clone();
    config.stack.default_branch = project.default_branch.clone();

    let stack_enum = project.stack.unwrap_or(crate::models::ProjectStack::Unknown);
    config.verify = if scraped.is_empty() {
        let defaults = crate::devcontainer::stack::verify_defaults(
            stack_enum,
            project.package_manager.as_deref(),
        );
        crate::models::VerifySection {
            test: defaults.test,
            lint: defaults.lint,
            format: defaults.format,
            typecheck: defaults.typecheck,
            source: Some("stack-default".to_string()),
        }
    } else {
        crate::devcontainer::ci_scrape::into_verify_section(scraped)
    };

    config.caches.volumes = crate::devcontainer::stack::cache_volumes(stack_enum, &project.repo_name)
        .into_iter()
        .map(|(source, target)| crate::models::CacheVolume { source, target })
        .collect();

    config.hooks.pre_commit = project.has_pre_commit;
    config.hooks.husky = project.has_husky;
    config.hooks.lefthook = project.has_lefthook;
    config.meta.claude_md_strategy = Some(
        if project.has_claude_md {
            "reference".to_string()
        } else {
            "absent".to_string()
        },
    );
    config
}

/// Persist a baseline `.grove/config.toml` capturing the Phase 1 detection. Refuses
/// to overwrite an existing file so user edits survive `grove init` re-runs.
fn write_grove_config(
    project_root: &Path,
    project: &ProjectContext,
    ctx: &worktree_manager::RepoContext,
) -> Result<(), String> {
    let dir = project_root.join(".grove");
    fs::create_dir_all(&dir).map_err(|e| format!("create .grove/: {}", e))?;
    let path = dir.join("config.toml");
    if path.exists() {
        return Ok(()); // respect existing user config
    }

    let scraped = crate::devcontainer::ci_scrape::scrape(ctx);
    let mut config = build_grove_config(project, scraped);
    config.devcontainer.enabled = true;
    config.devcontainer.auto_up = true;
    config.meta.initialized_at = Some(chrono::Utc::now());
    config.meta.schema_version = 1;

    let body = toml::to_string_pretty(&config)
        .map_err(|e| format!("serialize .grove/config.toml: {}", e))?;
    fs::write(&path, body).map_err(|e| format!("write .grove/config.toml: {}", e))?;
    Ok(())
}

/// Extend (or create) `.groverc` so `grove add` brings the devcontainer up on each new
/// worktree. Upstream-compatible: if `.groverc` already exists, we read its JSON,
/// append the missing bootstrap command, and rewrite.
fn ensure_groverc_bootstrap(project_root: &Path) -> Result<(), String> {
    let path = project_root.join(".groverc");
    let mut value: serde_json::Value = if path.exists() {
        let raw = fs::read_to_string(&path).map_err(|e| format!("read .groverc: {}", e))?;
        if raw.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(&raw).map_err(|e| format!("parse .groverc: {}", e))?
        }
    } else {
        serde_json::json!({})
    };

    // Ensure bootstrap.commands is an array.
    let obj = value
        .as_object_mut()
        .ok_or_else(|| ".groverc is not a JSON object".to_string())?;
    let bootstrap = obj
        .entry("bootstrap")
        .or_insert_with(|| serde_json::json!({ "commands": [] }));
    let commands = bootstrap
        .as_object_mut()
        .and_then(|b| b.entry("commands").or_insert_with(|| serde_json::json!([])).as_array_mut())
        .ok_or_else(|| ".groverc bootstrap.commands is not an array".to_string())?;

    let devcontainer_cmd = serde_json::json!({
        "program": "devcontainer",
        "args": ["up", "--workspace-folder", "."]
    });
    let already_present = commands.iter().any(|c| {
        c.get("program").and_then(|p| p.as_str()) == Some("devcontainer")
            && c.get("args")
                .and_then(|a| a.as_array())
                .map(|a| a.iter().any(|v| v == "up"))
                .unwrap_or(false)
    });
    if !already_present {
        commands.insert(0, devcontainer_cmd);
    }

    let body =
        serde_json::to_string_pretty(&value).map_err(|e| format!("serialize .groverc: {}", e))?;
    fs::write(&path, body).map_err(|e| format!("write .groverc: {}", e))?;
    Ok(())
}

/// Idempotent: append grove-specific dockerignore entries if missing. Only invoked
/// when a Dockerfile is present in HEAD.
fn patch_dockerignore(project_root: &Path) -> Result<(), String> {
    let path = project_root.join(".dockerignore");
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let entries = ["worktrees/", ".grove/"];
    let mut out = existing.clone();
    if !out.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    let mut needs_header = !existing.contains("# grove");
    for entry in entries.iter() {
        if !existing.lines().any(|l| l.trim() == *entry) {
            if needs_header {
                out.push_str("\n# grove\n");
                needs_header = false;
            }
            out.push_str(entry);
            out.push('\n');
        }
    }
    fs::write(&path, out).map_err(|e| format!("write .dockerignore: {}", e))?;
    Ok(())
}

/// Add named cache volumes (from `stack::cache_volumes`) to devcontainer.json's
/// `mounts` array. No-op if devcontainer.json doesn't exist (e.g. --no-devcontainer).
fn apply_cache_volumes_to_devcontainer(
    project_root: &Path,
    project: &ProjectContext,
) -> Result<(), String> {
    let dev_path = project_root.join(".devcontainer").join("devcontainer.json");
    if !dev_path.exists() {
        return Ok(());
    }
    let raw = fs::read_to_string(&dev_path).map_err(|e| format!("read {}: {}", dev_path.display(), e))?;
    let mut value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("parse {}: {}", dev_path.display(), e))?;
    let stack = project.stack.unwrap_or(crate::models::ProjectStack::Unknown);
    let vols = crate::devcontainer::stack::cache_volumes(stack, &project.repo_name);

    let obj = value
        .as_object_mut()
        .ok_or_else(|| "devcontainer.json top-level is not a JSON object".to_string())?;
    let mounts = obj
        .entry("mounts")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or_else(|| "devcontainer.json `mounts` is not an array".to_string())?;
    for (source, target) in vols {
        let entry = format!(
            "source={},target={},type=volume",
            source, target
        );
        if !mounts.iter().any(|v| v == &serde_json::Value::String(entry.clone())) {
            mounts.push(serde_json::Value::String(entry));
        }
    }
    let body = serde_json::to_string_pretty(&value)
        .map_err(|e| format!("serialize {}: {}", dev_path.display(), e))?;
    fs::write(&dev_path, body).map_err(|e| format!("write {}: {}", dev_path.display(), e))?;
    Ok(())
}

/// Idempotent: append grove-specific gitignore entries if missing. Never rewrites or
/// reorders existing lines.
fn patch_gitignore(project_root: &Path) -> Result<(), String> {
    let path = project_root.join(".gitignore");
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let entries = [
        "# grove",
        ".grove/agents/",
        ".grove/bus/",
        "worktrees/",
        ".devcontainer/.local/",
    ];
    let mut out = existing.clone();
    if !out.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    let mut needs_header = !existing.contains("# grove");
    for entry in entries.iter().skip(1) {
        // entries[0] is the header
        if !existing.lines().any(|l| l.trim() == *entry) {
            if needs_header {
                out.push_str("\n# grove\n");
                needs_header = false;
            }
            out.push_str(entry);
            out.push('\n');
        }
    }
    fs::write(&path, out).map_err(|e| format!("write .gitignore: {}", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("grove-init-test-{}", name));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn build_grove_config_carries_stack_metadata() {
        let project = ProjectContext {
            stack: Some(crate::models::ProjectStack::Rust),
            package_manager: Some("cargo".into()),
            toolchain_version: Some("rust-stable".into()),
            default_branch: Some("main".into()),
            repo_name: "demo".into(),
            has_pre_commit: true,
            ..Default::default()
        };
        let config = build_grove_config(
            &project,
            crate::devcontainer::ci_scrape::ScrapeResult::default(),
        );
        assert_eq!(config.stack.detected.as_deref(), Some("rust"));
        assert_eq!(config.stack.package_mgr.as_deref(), Some("cargo"));
        // Fell back to stack defaults for verify.
        assert_eq!(config.verify.source.as_deref(), Some("stack-default"));
        assert_eq!(config.verify.test[0], "cargo");
        // Cache volumes for Rust include cargo registry + per-repo target.
        assert!(config
            .caches
            .volumes
            .iter()
            .any(|v| v.source == "grove-cargo-registry"));
        assert!(config.hooks.pre_commit);
        assert_eq!(config.meta.claude_md_strategy.as_deref(), Some("absent"));
    }

    #[test]
    fn build_grove_config_uses_scrape_when_available() {
        let project = ProjectContext {
            stack: Some(crate::models::ProjectStack::Rust),
            repo_name: "demo".into(),
            ..Default::default()
        };
        let mut scraped = crate::devcontainer::ci_scrape::ScrapeResult::default();
        crate::devcontainer::ci_scrape::classify_and_push("pytest -q tests/", &mut scraped);
        let config = build_grove_config(&project, scraped);
        assert_eq!(config.verify.source.as_deref(), Some("ci-scrape"));
        assert_eq!(config.verify.test[0], "pytest");
    }

    #[test]
    fn patch_dockerignore_appends_grove_block() {
        let dir = tmp("dockerignore-new");
        patch_dockerignore(&dir).unwrap();
        let body = fs::read_to_string(dir.join(".dockerignore")).unwrap();
        assert!(body.contains("# grove"));
        assert!(body.contains("worktrees/"));
        assert!(body.contains(".grove/"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn patch_dockerignore_is_idempotent() {
        let dir = tmp("dockerignore-idem");
        patch_dockerignore(&dir).unwrap();
        let body1 = fs::read_to_string(dir.join(".dockerignore")).unwrap();
        patch_dockerignore(&dir).unwrap();
        let body2 = fs::read_to_string(dir.join(".dockerignore")).unwrap();
        assert_eq!(body1, body2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_cache_volumes_adds_named_volumes_to_devcontainer() {
        let dir = tmp("apply-vols");
        fs::create_dir_all(dir.join(".devcontainer")).unwrap();
        fs::write(
            dir.join(".devcontainer/devcontainer.json"),
            r#"{"name":"demo","mounts":[]}"#,
        )
        .unwrap();
        let project = ProjectContext {
            stack: Some(crate::models::ProjectStack::Rust),
            repo_name: "demo".into(),
            ..Default::default()
        };
        apply_cache_volumes_to_devcontainer(&dir, &project).unwrap();
        let body = fs::read_to_string(dir.join(".devcontainer/devcontainer.json")).unwrap();
        assert!(body.contains("grove-cargo-registry"));
        assert!(body.contains("type=volume"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_groverc_bootstrap_creates_file_when_absent() {
        let dir = tmp("groverc-fresh");
        ensure_groverc_bootstrap(&dir).unwrap();
        let body = fs::read_to_string(dir.join(".groverc")).unwrap();
        assert!(body.contains("devcontainer"));
        assert!(body.contains("\"up\""));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_groverc_bootstrap_extends_existing_config() {
        let dir = tmp("groverc-extend");
        fs::write(
            dir.join(".groverc"),
            r#"{"branchPrefix":"panzax","bootstrap":{"commands":[{"program":"npm","args":["install"]}]}}"#,
        )
        .unwrap();
        ensure_groverc_bootstrap(&dir).unwrap();
        let body = fs::read_to_string(dir.join(".groverc")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let cmds = v["bootstrap"]["commands"].as_array().unwrap();
        assert_eq!(v["branchPrefix"], "panzax");
        // devcontainer up should be first, npm install still present
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0]["program"], "devcontainer");
        assert_eq!(cmds[1]["program"], "npm");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_groverc_bootstrap_is_idempotent() {
        let dir = tmp("groverc-idem");
        ensure_groverc_bootstrap(&dir).unwrap();
        let body1 = fs::read_to_string(dir.join(".groverc")).unwrap();
        ensure_groverc_bootstrap(&dir).unwrap();
        let body2 = fs::read_to_string(dir.join(".groverc")).unwrap();
        assert_eq!(body1, body2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn patch_gitignore_appends_grove_block() {
        let dir = tmp("gitignore-new");
        fs::write(dir.join(".gitignore"), "target/\nnode_modules/\n").unwrap();
        patch_gitignore(&dir).unwrap();
        let body = fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert!(body.contains("target/"));
        assert!(body.contains("# grove"));
        assert!(body.contains(".grove/agents/"));
        assert!(body.contains(".grove/bus/"));
        assert!(body.contains("worktrees/"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn patch_gitignore_is_idempotent() {
        let dir = tmp("gitignore-idem");
        patch_gitignore(&dir).unwrap();
        let body1 = fs::read_to_string(dir.join(".gitignore")).unwrap();
        patch_gitignore(&dir).unwrap();
        let body2 = fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert_eq!(body1, body2);
        let _ = fs::remove_dir_all(&dir);
    }
}
