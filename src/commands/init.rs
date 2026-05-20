use colored::Colorize;
use dialoguer::theme::ColorfulTheme;
use dialoguer::Select;
use std::fs;
use std::path::{Path, PathBuf};

use crate::devcontainer;
use crate::git::clone_bare_repository;
use crate::git::worktree_manager;
use crate::models::{GroveConfig, ProjectContext, ProjectLayout};
use crate::utils::{extract_repo_name, find_grove_repo, is_valid_git_url};

/// What to do with an existing on-disk file/dir during in-place init.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConflictAction {
    /// Merge grove's defaults into the existing file (preserves user content).
    Merge,
    /// Replace the existing file with grove's default; user content is lost.
    Overwrite,
    /// Leave the existing file alone.
    Skip,
}

/// `grove init` entry point. Three surface modes — dispatched by the shape of <target>:
///
/// 1. `grove init <git_url>`     — clone-mode: bare clone + scaffold (upstream layout).
/// 2. `grove init [<path>]`      — in-place: adopt an existing checkout (defaults to ".").
/// 3. `grove init --reconfigure` — Phase 2 wizard only, against an already-initialized
///                                 project; no clone, no scaffold.
pub fn run(
    target: Option<&str>,
    no_agent: bool,
    no_devcontainer: bool,
    reconfigure: bool,
    assume_yes: bool,
) {
    if reconfigure {
        run_reconfigure();
        return;
    }

    match resolve_target(target) {
        Target::Clone(url) => run_clone(&url, no_agent, no_devcontainer, assume_yes),
        Target::InPlace(path) => run_in_place(&path, no_agent, no_devcontainer, assume_yes),
        Target::InvalidUrl(s) => {
            eprintln!(
                "{} Invalid git URL format. Supported formats:\n  - HTTPS: https://github.com/user/repo.git\n  - SSH: git@github.com:user/repo.git\n  - SSH: ssh://git@github.com/user/repo.git\n  Got: {}",
                "Error:".red(),
                s
            );
            // Exit 2 to match upstream's clap-validator behavior (existing
            // grove.hone test asserts exit_code == 2 here).
            std::process::exit(2);
        }
        Target::AmbiguousNonExistent(s) => {
            eprintln!(
                "{} '{}' is not a valid git URL and does not exist as a directory.\n  Pass a git URL to clone, OR a path to an existing checkout (default \".\").",
                "Error:".red(),
                s
            );
            std::process::exit(1);
        }
    }
}

enum Target {
    Clone(String),
    InPlace(PathBuf),
    InvalidUrl(String),
    AmbiguousNonExistent(String),
}

/// Disambiguate the positional argument:
/// - None              → in-place at cwd
/// - Existing dir      → in-place at that path
/// - Looks like a URL  → Clone (if valid) or InvalidUrl
/// - Looks like a path → AmbiguousNonExistent (path that doesn't exist or isn't a dir)
/// - Anything else     → InvalidUrl (preserves upstream behavior: bare strings
///                       that aren't URL-shaped AND aren't path-shaped get the
///                       upstream "Invalid git URL format" error with exit 2)
fn resolve_target(target: Option<&str>) -> Target {
    let Some(value) = target else {
        return Target::InPlace(PathBuf::from("."));
    };
    // Existing dir wins regardless of shape — `grove init my-existing-repo`.
    let path = PathBuf::from(value);
    if path.is_dir() {
        return Target::InPlace(path);
    }
    if looks_like_git_url(value) {
        return if is_valid_git_url(value) {
            Target::Clone(value.to_string())
        } else {
            Target::InvalidUrl(value.to_string())
        };
    }
    if looks_like_path(value) {
        return Target::AmbiguousNonExistent(value.to_string());
    }
    // Not URL-shaped, not path-shaped, doesn't exist — most likely a typo'd URL
    // (e.g. `not-a-valid-url`). Upstream's clap validator rejected these with
    // exit 2; we preserve that here.
    Target::InvalidUrl(value.to_string())
}

/// Heuristic: would a reasonable user type this as a git URL? URLs always
/// contain `://` or follow the `git@host:path` shape.
fn looks_like_git_url(value: &str) -> bool {
    value.contains("://") || value.starts_with("git@")
}

/// Heuristic: looks like a filesystem path? Either contains a path separator
/// or starts with a directory-prefix marker.
fn looks_like_path(value: &str) -> bool {
    value.contains('/')
        || value.contains('\\')
        || value.starts_with('.')
        || value.starts_with('~')
}

// =============================================================================
// Mode 1 — clone-mode (upstream behavior, preserved)
// =============================================================================

fn run_clone(git_url: &str, no_agent: bool, no_devcontainer: bool, assume_yes: bool) {
    if let Some(existing) = find_grove_repo(None) {
        eprintln!(
            "{} Cannot initialize grove inside an existing grove repository.\nDetected grove repository at: {}\n",
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

    // Preserve upstream init.hone's "Initialized worktree setup" assertion verbatim.
    println!(
        "{} {}",
        "✓ Initialized worktree setup:".green(),
        repo_name.bold()
    );
    println!("  {} {}", "Bare repository:".dimmed(), bare_repo_dir);

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
    run_phase1_and_phase2(&context, &project, no_agent, no_devcontainer, assume_yes);

    println!();
    println!("{}", "Next steps:".bold());
    println!("  {} {}", "cd".dimmed(), repo_name);
    println!(
        "  {} <name>                  # create a regular worktree",
        "grove add".dimmed()
    );
    println!(
        "  {} <name> --task \"...\"   # spawn an agent in an isolated worktree",
        "grove spawn".dimmed()
    );

    if let Some(cwd) = original_cwd {
        let _ = std::env::set_current_dir(cwd);
    }
}

// =============================================================================
// Mode 2 — in-place adoption (fork addition)
// =============================================================================

fn run_in_place(path: &Path, no_agent: bool, no_devcontainer: bool, assume_yes: bool) {
    let canonical = match path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "{} {} is not accessible: {}",
                "Error:".red(),
                path.display(),
                e
            );
            std::process::exit(1);
        }
    };

    // Adopt the supplied path; reject if not a git working copy.
    let context = match worktree_manager::discover_in_place(Some(&canonical)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "{} {} is not a git repository. {}",
                "Error:".red(),
                canonical.display(),
                e
            );
            eprintln!(
                "  Run `git init` first, or pass a git URL to clone fresh: `grove init <url>`."
            );
            std::process::exit(1);
        }
    };

    let project_root_path = worktree_manager::project_root(&context).to_path_buf();
    let repo_name = project_root_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string();
    if project_root_path
        .join(".grove")
        .join("config.toml")
        .exists()
    {
        println!(
            "{} {} is already grove-initialized; re-running scaffold (existing files will prompt for merge/overwrite/skip).",
            "Note:".yellow(),
            project_root_path.display()
        );
    }
    println!(
        "{} {} (layout=in-place)",
        "▶ Adopting".cyan(),
        project_root_path.display()
    );
    let project = devcontainer::detect_project_context(&context, &repo_name);
    print_detection_summary(&project);

    run_phase1_and_phase2(&context, &project, no_agent, no_devcontainer, assume_yes);

    println!();
    println!("{}", "Next steps:".bold());
    println!(
        "  {} <name>                  # create a regular worktree under worktrees/<name>",
        "grove add".dimmed()
    );
    println!(
        "  {} <name> --task \"...\"   # spawn an agent in an isolated worktree",
        "grove spawn".dimmed()
    );
}

// =============================================================================
// Shared phases
// =============================================================================

fn run_phase1_and_phase2(
    context: &worktree_manager::RepoContext,
    project: &ProjectContext,
    no_agent: bool,
    no_devcontainer: bool,
    assume_yes: bool,
) {
    let project_root = worktree_manager::project_root(context);

    // ---- Phase 1 ----
    if no_devcontainer {
        println!(
            "  {} --no-devcontainer set; skipping .devcontainer/ scaffold.",
            "·".dimmed()
        );
    } else {
        handle_devcontainer(context, project, assume_yes);
    }

    handle_grove_config(context, project, assume_yes);
    handle_groverc(project_root);
    handle_gitignore(project_root);
    if project.has_dockerfile {
        handle_dockerignore(project_root);
    }
    if !no_devcontainer {
        if let Err(e) = apply_cache_volumes_to_devcontainer(project_root, project) {
            eprintln!(
                "  {} failed to add cache volumes to devcontainer.json: {}",
                "Warning:".yellow(),
                e
            );
        } else {
            println!(
                "  {} applied named cache volumes to devcontainer.json",
                "·".dimmed()
            );
        }
    }

    match crate::agent::hook::install_engine(context) {
        Ok(p) => println!("  {} installed engine at {}", "·".dimmed(), p.display()),
        Err(e) => eprintln!(
            "  {} failed to install loop engine: {}",
            "Warning:".yellow(),
            e
        ),
    }
    handle_framework_assets(context, project_root, assume_yes);
    match crate::agent::hook::default_user_settings_path() {
        Some(path) => match crate::agent::hook::install_stop_hook(
            &path,
            crate::agent::hook::HOOK_COMMAND,
        ) {
            Ok(report) => {
                let verb = if report.added {
                    "registered"
                } else {
                    "already present"
                };
                println!(
                    "  {} Stop hook {} in {} ({} total)",
                    "·".dimmed(),
                    verb,
                    report.path.display(),
                    report.total_stop_hooks
                );
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

    // ---- Phase 2 ----
    if no_agent {
        println!(
            "  {} --no-agent set; skipping the Phase 2 setup wizard.",
            "·".dimmed()
        );
    } else {
        match crate::agent::setup::run_setup_wizard(context, project, false) {
            Ok(()) => {}
            Err(e) => eprintln!(
                "  {} setup wizard did not complete: {} (you can retry with `grove init --reconfigure`)",
                "Warning:".yellow(),
                e
            ),
        }
    }
}

fn handle_devcontainer(
    context: &worktree_manager::RepoContext,
    project: &ProjectContext,
    assume_yes: bool,
) {
    let project_root = worktree_manager::project_root(context);
    let dev_path = project_root.join(".devcontainer").join("devcontainer.json");

    if !dev_path.exists() {
        match devcontainer::scaffold_devcontainer(context, project) {
            Ok(_) => println!("  {} wrote .devcontainer/devcontainer.json", "·".dimmed()),
            Err(e) => eprintln!(
                "  {} devcontainer scaffold failed: {}",
                "Warning:".yellow(),
                e
            ),
        }
        return;
    }

    let action = prompt_existing("`.devcontainer/devcontainer.json`", assume_yes);
    match action {
        ConflictAction::Skip => println!(
            "  {} kept existing .devcontainer/devcontainer.json",
            "·".dimmed()
        ),
        ConflictAction::Overwrite => {
            // Remove and rewrite.
            let _ = fs::remove_file(&dev_path);
            match devcontainer::scaffold_devcontainer(context, project) {
                Ok(_) => println!(
                    "  {} overwrote .devcontainer/devcontainer.json (Phase 2 wizard refines)",
                    "·".dimmed()
                ),
                Err(e) => eprintln!(
                    "  {} devcontainer overwrite failed: {}",
                    "Warning:".yellow(),
                    e
                ),
            }
        }
        ConflictAction::Merge => match merge_devcontainer_into_existing(project_root, project) {
            Ok(()) => println!(
                "  {} merged grove defaults into existing .devcontainer/devcontainer.json",
                "·".dimmed()
            ),
            Err(e) => eprintln!("  {} devcontainer merge failed: {}", "Warning:".yellow(), e),
        },
    }
}

fn handle_grove_config(
    context: &worktree_manager::RepoContext,
    project: &ProjectContext,
    assume_yes: bool,
) {
    let project_root = worktree_manager::project_root(context);
    let cfg_path = project_root.join(".grove").join("config.toml");

    if !cfg_path.exists() {
        if let Err(e) = write_grove_config(project_root, project, context) {
            eprintln!(
                "  {} failed to write .grove/config.toml: {}",
                "Warning:".yellow(),
                e
            );
        } else {
            println!("  {} wrote .grove/config.toml", "·".dimmed());
        }
        return;
    }

    let action = prompt_existing("`.grove/config.toml`", assume_yes);
    match action {
        ConflictAction::Skip => println!("  {} kept existing .grove/config.toml", "·".dimmed()),
        ConflictAction::Overwrite => {
            let _ = fs::remove_file(&cfg_path);
            if let Err(e) = write_grove_config(project_root, project, context) {
                eprintln!("  {} overwrite failed: {}", "Warning:".yellow(), e);
            } else {
                println!(
                    "  {} overwrote .grove/config.toml (Phase 2 wizard refines)",
                    "·".dimmed()
                );
            }
        }
        ConflictAction::Merge => match merge_grove_config(project_root, project, context) {
            Ok(()) => println!(
                "  {} merged grove defaults into existing .grove/config.toml",
                "·".dimmed()
            ),
            Err(e) => eprintln!(
                "  {} .grove/config.toml merge failed: {}",
                "Warning:".yellow(),
                e
            ),
        },
    }
}

fn handle_groverc(project_root: &Path) {
    if let Err(e) = ensure_groverc_bootstrap(project_root) {
        eprintln!("  {} failed to update .groverc: {}", "Warning:".yellow(), e);
    } else {
        println!("  {} updated .groverc bootstrap commands", "·".dimmed());
    }
}

fn handle_gitignore(project_root: &Path) {
    if let Err(e) = patch_gitignore(project_root) {
        eprintln!(
            "  {} failed to patch .gitignore: {}",
            "Warning:".yellow(),
            e
        );
    } else {
        println!(
            "  {} patched .gitignore (.grove/, worktrees/)",
            "·".dimmed()
        );
    }
}

fn handle_dockerignore(project_root: &Path) {
    if let Err(e) = patch_dockerignore(project_root) {
        eprintln!(
            "  {} failed to patch .dockerignore: {}",
            "Warning:".yellow(),
            e
        );
    } else {
        println!(
            "  {} patched .dockerignore (excludes .grove/ + worktrees/)",
            "·".dimmed()
        );
    }
}

fn handle_framework_assets(
    context: &worktree_manager::RepoContext,
    project_root: &Path,
    assume_yes: bool,
) {
    let shared = project_root.join(".grove").join("SHARED.md");
    let shared_existed = shared.exists();
    if shared_existed {
        match prompt_existing(".grove/SHARED.md", assume_yes) {
            ConflictAction::Skip => {
                // SHARED.md left as-is; install_assets's logic also skips it when
                // it already exists. Other framework files always overwrite.
                println!(
                    "  {} kept existing .grove/SHARED.md (framework files refreshed regardless)",
                    "·".dimmed()
                );
            }
            ConflictAction::Overwrite => {
                let _ = fs::remove_file(&shared);
                println!(
                    "  {} overwrote .grove/SHARED.md with the template",
                    "·".dimmed()
                );
            }
            ConflictAction::Merge => {
                // For SHARED.md "merge" === keep existing content (it's user-owned
                // canonical context). No structured merge applies.
                println!(
                    "  {} kept existing .grove/SHARED.md (merge ≡ keep for user-owned content)",
                    "·".dimmed()
                );
            }
        }
    }
    match crate::agent::seed::install_assets(context) {
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
}

// =============================================================================
// Conflict prompt
// =============================================================================

fn prompt_existing(label: &str, assume_yes: bool) -> ConflictAction {
    if assume_yes {
        return ConflictAction::Overwrite;
    }
    if !atty::is(atty::Stream::Stdin) {
        println!(
            "  {} {} already exists; non-interactive shell, defaulting to skip.",
            "·".dimmed(),
            label
        );
        return ConflictAction::Skip;
    }
    println!("  {} {} already exists.", "!".yellow(), label);
    let theme = ColorfulTheme::default();
    let options = vec![
        "Merge   — fill in grove defaults but keep existing values",
        "Overwrite — replace with grove's defaults (Phase 2 wizard refines)",
        "Skip   — leave the existing file untouched",
    ];
    let idx = Select::with_theme(&theme)
        .with_prompt(format!("How should grove handle {}?", label))
        .items(&options)
        .default(0)
        .interact()
        .unwrap_or(2);
    match idx {
        0 => ConflictAction::Merge,
        1 => ConflictAction::Overwrite,
        _ => ConflictAction::Skip,
    }
}

// =============================================================================
// Reconfigure (Phase 2 only)
// =============================================================================

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

// =============================================================================
// Detection summary + config writers (unchanged from earlier in the branch)
// =============================================================================

fn print_detection_summary(project: &ProjectContext) {
    let stack = project.stack.map(|s| s.as_str()).unwrap_or("unknown");
    let pm = project.package_manager.as_deref().unwrap_or("none");
    let tc = project
        .toolchain_version
        .as_deref()
        .unwrap_or("unspecified");
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

fn build_grove_config(
    project: &ProjectContext,
    scraped: crate::devcontainer::ci_scrape::ScrapeResult,
    layout: ProjectLayout,
) -> GroveConfig {
    let mut config = GroveConfig::default();
    config.project.layout = layout;
    config.project.root = Some(".".to_string());

    if let Some(s) = project.stack {
        config.stack.detected = Some(s.as_str().to_string());
    }
    config.stack.toolchain = project.toolchain_version.clone();
    config.stack.package_mgr = project.package_manager.clone();
    config.stack.default_branch = project.default_branch.clone();

    let stack_enum = project
        .stack
        .unwrap_or(crate::models::ProjectStack::Unknown);
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

    config.caches.volumes =
        crate::devcontainer::stack::cache_volumes(stack_enum, &project.repo_name)
            .into_iter()
            .map(|(source, target)| crate::models::CacheVolume { source, target })
            .collect();

    config.hooks.pre_commit = project.has_pre_commit;
    config.hooks.husky = project.has_husky;
    config.hooks.lefthook = project.has_lefthook;
    config.meta.claude_md_strategy = Some(if project.has_claude_md {
        "reference".to_string()
    } else {
        "absent".to_string()
    });
    config
}

fn write_grove_config(
    project_root: &Path,
    project: &ProjectContext,
    ctx: &worktree_manager::RepoContext,
) -> Result<(), String> {
    let dir = project_root.join(".grove");
    fs::create_dir_all(&dir).map_err(|e| format!("create .grove/: {}", e))?;
    let path = dir.join("config.toml");

    let scraped = crate::devcontainer::ci_scrape::scrape(ctx);
    let layout = worktree_manager::layout(ctx);
    let mut config = build_grove_config(project, scraped, layout);
    config.devcontainer.enabled = true;
    config.devcontainer.auto_up = true;
    config.meta.initialized_at = Some(chrono::Utc::now());
    config.meta.schema_version = 1;

    let body = toml::to_string_pretty(&config)
        .map_err(|e| format!("serialize .grove/config.toml: {}", e))?;
    fs::write(&path, body).map_err(|e| format!("write .grove/config.toml: {}", e))?;
    Ok(())
}

/// Merge grove's defaults into an existing `.grove/config.toml`. User-set values win;
/// grove fills in anything the user hasn't touched.
fn merge_grove_config(
    project_root: &Path,
    project: &ProjectContext,
    ctx: &worktree_manager::RepoContext,
) -> Result<(), String> {
    let path = project_root.join(".grove").join("config.toml");
    let raw = fs::read_to_string(&path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    let existing: GroveConfig =
        toml::from_str(&raw).map_err(|e| format!("parse existing .grove/config.toml: {}", e))?;
    let scraped = crate::devcontainer::ci_scrape::scrape(ctx);
    let layout = worktree_manager::layout(ctx);
    let defaults = build_grove_config(project, scraped, layout);

    let merged = merge_configs(existing, defaults);
    let body = toml::to_string_pretty(&merged)
        .map_err(|e| format!("serialize .grove/config.toml: {}", e))?;
    fs::write(&path, body).map_err(|e| format!("write .grove/config.toml: {}", e))?;
    Ok(())
}

/// Existing values win; grove's defaults only fill empty/None fields.
fn merge_configs(mut existing: GroveConfig, defaults: GroveConfig) -> GroveConfig {
    if existing.stack.detected.is_none() {
        existing.stack.detected = defaults.stack.detected;
    }
    if existing.stack.toolchain.is_none() {
        existing.stack.toolchain = defaults.stack.toolchain;
    }
    if existing.stack.package_mgr.is_none() {
        existing.stack.package_mgr = defaults.stack.package_mgr;
    }
    if existing.stack.default_branch.is_none() {
        existing.stack.default_branch = defaults.stack.default_branch;
    }
    if existing.verify.test.is_empty() {
        existing.verify.test = defaults.verify.test;
    }
    if existing.verify.lint.is_empty() {
        existing.verify.lint = defaults.verify.lint;
    }
    if existing.verify.format.is_empty() {
        existing.verify.format = defaults.verify.format;
    }
    if existing.verify.typecheck.is_empty() {
        existing.verify.typecheck = defaults.verify.typecheck;
    }
    if existing.verify.source.is_none() {
        existing.verify.source = defaults.verify.source;
    }
    if existing.caches.volumes.is_empty() {
        existing.caches.volumes = defaults.caches.volumes;
    }
    if existing.meta.claude_md_strategy.is_none() {
        existing.meta.claude_md_strategy = defaults.meta.claude_md_strategy;
    }
    // Always update layout to current detection; this isn't a user-set preference.
    existing.project.layout = defaults.project.layout;
    if existing.project.root.is_none() {
        existing.project.root = defaults.project.root;
    }
    existing
}

/// Merge grove defaults into an existing devcontainer.json, preserving every user field.
/// Adds the per-stack extensions list IF the user's array is empty; otherwise leaves the
/// extensions list alone. Sets remoteUser/containerUser only if absent.
fn merge_devcontainer_into_existing(
    project_root: &Path,
    project: &ProjectContext,
) -> Result<(), String> {
    let path = project_root.join(".devcontainer").join("devcontainer.json");
    let raw = fs::read_to_string(&path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    let mut value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {}", path.display(), e))?;
    let skeleton = devcontainer::build_devcontainer_skeleton(project);

    let (obj_existing, obj_default) = match (value.as_object_mut(), skeleton.as_object()) {
        (Some(a), Some(b)) => (a, b),
        _ => return Err("devcontainer.json must be a JSON object".into()),
    };
    for (k, v) in obj_default {
        match k.as_str() {
            "customizations" => {
                // Drill into customizations.vscode.extensions: only fill the
                // extensions array if the user's is empty.
                let target = obj_existing
                    .entry("customizations")
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(target_obj) = target.as_object_mut() {
                    let vscode_target = target_obj
                        .entry("vscode")
                        .or_insert_with(|| serde_json::json!({}));
                    if let Some(vscode_obj) = vscode_target.as_object_mut() {
                        let exts = vscode_obj
                            .entry("extensions")
                            .or_insert_with(|| serde_json::json!([]));
                        let is_empty = exts.as_array().map(|a| a.is_empty()).unwrap_or(true);
                        if is_empty {
                            if let Some(def_exts) =
                                v.get("vscode").and_then(|v| v.get("extensions"))
                            {
                                *exts = def_exts.clone();
                            }
                        }
                    }
                }
            }
            "mounts" => {
                // Don't clobber existing mounts. Add cache-volume defaults later via
                // apply_cache_volumes_to_devcontainer().
                obj_existing
                    .entry("mounts")
                    .or_insert_with(|| serde_json::json!([]));
            }
            _ => {
                obj_existing.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
    }
    let body = serde_json::to_string_pretty(&value)
        .map_err(|e| format!("serialize {}: {}", path.display(), e))?;
    fs::write(&path, body).map_err(|e| format!("write {}: {}", path.display(), e))?;
    Ok(())
}

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

    let obj = value
        .as_object_mut()
        .ok_or_else(|| ".groverc is not a JSON object".to_string())?;
    let bootstrap = obj
        .entry("bootstrap")
        .or_insert_with(|| serde_json::json!({ "commands": [] }));
    let commands = bootstrap
        .as_object_mut()
        .and_then(|b| {
            b.entry("commands")
                .or_insert_with(|| serde_json::json!([]))
                .as_array_mut()
        })
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

fn apply_cache_volumes_to_devcontainer(
    project_root: &Path,
    project: &ProjectContext,
) -> Result<(), String> {
    let dev_path = project_root.join(".devcontainer").join("devcontainer.json");
    if !dev_path.exists() {
        return Ok(());
    }
    let raw =
        fs::read_to_string(&dev_path).map_err(|e| format!("read {}: {}", dev_path.display(), e))?;
    let mut value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {}", dev_path.display(), e))?;
    let stack = project
        .stack
        .unwrap_or(crate::models::ProjectStack::Unknown);
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
        let entry = format!("source={},target={},type=volume", source, target);
        if !mounts
            .iter()
            .any(|v| v == &serde_json::Value::String(entry.clone()))
        {
            mounts.push(serde_json::Value::String(entry));
        }
    }
    let body = serde_json::to_string_pretty(&value)
        .map_err(|e| format!("serialize {}: {}", dev_path.display(), e))?;
    fs::write(&dev_path, body).map_err(|e| format!("write {}: {}", dev_path.display(), e))?;
    Ok(())
}

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
    fn resolve_target_url() {
        let t = resolve_target(Some("https://github.com/x/y.git"));
        assert!(matches!(t, Target::Clone(_)));
    }

    #[test]
    fn resolve_target_default_is_cwd() {
        let t = resolve_target(None);
        match t {
            Target::InPlace(p) => assert_eq!(p, PathBuf::from(".")),
            _ => panic!("expected InPlace(.)"),
        }
    }

    #[test]
    fn resolve_target_existing_dir() {
        let dir = tmp("resolve-existing");
        let t = resolve_target(Some(dir.to_str().unwrap()));
        assert!(matches!(t, Target::InPlace(_)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_target_bare_string_is_invalid_url() {
        // not-a-url-not-a-path: not a URL, no path separators → treated as a
        // typo'd URL (matches upstream grove.hone:41).
        let t = resolve_target(Some("not-a-url-not-a-path-xyz123"));
        assert!(matches!(t, Target::InvalidUrl(_)));
    }

    #[test]
    fn resolve_target_path_shaped_non_existent_is_ambiguous() {
        let t = resolve_target(Some("./does-not-exist-zzz"));
        assert!(matches!(t, Target::AmbiguousNonExistent(_)));
        let t = resolve_target(Some("/tmp/grove-resolve-nonexistent-zzz"));
        assert!(matches!(t, Target::AmbiguousNonExistent(_)));
    }

    #[test]
    fn resolve_target_url_shaped_but_invalid_is_invalid_url() {
        let t = resolve_target(Some("https://garbage url with spaces"));
        assert!(matches!(t, Target::InvalidUrl(_)));
    }

    #[test]
    fn looks_like_git_url_detects_common_shapes() {
        assert!(looks_like_git_url("https://github.com/x/y.git"));
        assert!(looks_like_git_url("ssh://git@github.com/x/y"));
        assert!(looks_like_git_url("git@github.com:x/y.git"));
        assert!(!looks_like_git_url("./relative"));
        assert!(!looks_like_git_url("Cargo.toml"));
        assert!(!looks_like_git_url("not-a-url-not-a-path-xyz123"));
    }

    #[test]
    fn looks_like_path_detects_separators() {
        assert!(looks_like_path("./relative"));
        assert!(looks_like_path("/abs/path"));
        assert!(looks_like_path("~/home/path"));
        assert!(looks_like_path("nested/dir"));
        assert!(!looks_like_path("bare-string"));
        assert!(!looks_like_path("not-a-url-not-a-path-xyz123"));
    }

    #[test]
    fn build_grove_config_carries_layout() {
        let project = ProjectContext {
            stack: Some(crate::models::ProjectStack::Rust),
            repo_name: "demo".into(),
            ..Default::default()
        };
        let cfg = build_grove_config(
            &project,
            crate::devcontainer::ci_scrape::ScrapeResult::default(),
            ProjectLayout::InPlace,
        );
        assert!(matches!(cfg.project.layout, ProjectLayout::InPlace));
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
            ProjectLayout::Bare,
        );
        assert_eq!(config.stack.detected.as_deref(), Some("rust"));
        assert_eq!(config.stack.package_mgr.as_deref(), Some("cargo"));
        assert_eq!(config.verify.source.as_deref(), Some("stack-default"));
        assert_eq!(config.verify.test[0], "cargo");
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
        let config = build_grove_config(&project, scraped, ProjectLayout::Bare);
        assert_eq!(config.verify.source.as_deref(), Some("ci-scrape"));
        assert_eq!(config.verify.test[0], "pytest");
    }

    #[test]
    fn merge_configs_user_values_win() {
        let mut existing = GroveConfig::default();
        existing.verify.test = vec!["my-custom-test".into()];
        existing.stack.detected = Some("custom".into());
        let mut defaults = GroveConfig::default();
        defaults.verify.test = vec!["pytest".into()];
        defaults.verify.lint = vec!["ruff".into(), "check".into(), ".".into()];
        defaults.stack.detected = Some("python".into());
        let merged = merge_configs(existing, defaults);
        assert_eq!(merged.verify.test[0], "my-custom-test");
        assert_eq!(merged.verify.lint[0], "ruff"); // grove fills empty
        assert_eq!(merged.stack.detected.as_deref(), Some("custom"));
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

    #[test]
    fn merge_devcontainer_preserves_user_extensions() {
        let dir = tmp("merge-dc");
        fs::create_dir_all(dir.join(".devcontainer")).unwrap();
        fs::write(
            dir.join(".devcontainer/devcontainer.json"),
            r#"{"name":"demo","image":"my-custom","customizations":{"vscode":{"extensions":["my.ext"]}}}"#,
        )
        .unwrap();
        let project = ProjectContext {
            stack: Some(crate::models::ProjectStack::Rust),
            default_image: crate::models::ProjectStack::Rust
                .default_image()
                .to_string(),
            repo_name: "demo".into(),
            ..Default::default()
        };
        merge_devcontainer_into_existing(&dir, &project).unwrap();
        let body = fs::read_to_string(dir.join(".devcontainer/devcontainer.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["name"], "demo");
        assert_eq!(v["image"], "my-custom"); // user's image preserved
        let exts = v["customizations"]["vscode"]["extensions"]
            .as_array()
            .unwrap();
        assert_eq!(exts.len(), 1); // user's extensions preserved (non-empty array)
        assert_eq!(exts[0], "my.ext");
        // Grove keys that were missing get filled in:
        assert!(v.get("remoteUser").is_some());
        assert!(v.get("mounts").is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_devcontainer_fills_empty_extensions() {
        let dir = tmp("merge-dc-empty-ext");
        fs::create_dir_all(dir.join(".devcontainer")).unwrap();
        fs::write(
            dir.join(".devcontainer/devcontainer.json"),
            r#"{"name":"demo","customizations":{"vscode":{"extensions":[]}}}"#,
        )
        .unwrap();
        let project = ProjectContext {
            stack: Some(crate::models::ProjectStack::Rust),
            default_image: crate::models::ProjectStack::Rust
                .default_image()
                .to_string(),
            repo_name: "demo".into(),
            ..Default::default()
        };
        merge_devcontainer_into_existing(&dir, &project).unwrap();
        let body = fs::read_to_string(dir.join(".devcontainer/devcontainer.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let exts = v["customizations"]["vscode"]["extensions"]
            .as_array()
            .unwrap();
        assert!(!exts.is_empty());
        assert!(exts.iter().any(|e| e == "rust-lang.rust-analyzer"));
        let _ = fs::remove_dir_all(&dir);
    }
}
