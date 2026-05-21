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
pub fn run(target: Option<&str>, no_agent: bool, reconfigure: bool, assume_yes: bool) {
    // Hard-require git ≥ 2.46. Grove writes relative worktree pointers so
    // host and devcontainer see the worktree at the same logical path;
    // native git acceptance of relative worktree pointers landed in 2.46
    // (gated by `extensions.relativeWorktrees`, which itself requires
    // `core.repositoryFormatVersion=1`). Older git rejects either the
    // pointers or the format. Fail fast so the operator can upgrade
    // before scaffolding anything.
    if let Err(e) = require_git_supports_relative_worktrees() {
        eprintln!("{} {}", "Error:".red(), e);
        std::process::exit(1);
    }

    if reconfigure {
        run_reconfigure();
        return;
    }

    match resolve_target(target) {
        Target::Clone(url) => run_clone(&url, no_agent, assume_yes),
        Target::InPlace(path) => run_in_place(&path, no_agent, assume_yes),
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
    value.contains('/') || value.contains('\\') || value.starts_with('.') || value.starts_with('~')
}

// =============================================================================
// Mode 1 — clone-mode (upstream behavior, preserved)
// =============================================================================

fn run_clone(git_url: &str, no_agent: bool, assume_yes: bool) {
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
    run_phase1_and_phase2(&context, &project, no_agent, assume_yes);

    println!();
    print_next_steps_with_danger_accept(&repo_name);

    if let Some(cwd) = original_cwd {
        let _ = std::env::set_current_dir(cwd);
    }
}

// =============================================================================
// Mode 2 — in-place adoption (fork addition)
// =============================================================================

fn run_in_place(path: &Path, no_agent: bool, assume_yes: bool) {
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

    run_phase1_and_phase2(&context, &project, no_agent, assume_yes);

    println!();
    print_next_steps_with_danger_accept("");
}

/// One-time setup reminder + standard next-steps. Operators MUST run
/// `claude --dangerously-skip-permissions` once on the host and accept
/// the warning before their first `grove spawn` — otherwise the
/// in-container claude blocks on the acknowledgement prompt and the
/// bootstrap turn hangs forever (no human there to press Y). Easy to
/// miss; printing it here makes it impossible to miss.
fn print_next_steps_with_danger_accept(repo_name_for_cd: &str) {
    println!("{}", "Next steps:".bold());
    println!();
    println!(
        "  {} {} {}",
        "1.".bold(),
        "(ONE-TIME, on host)".yellow(),
        "Accept the dangerous-mode warning so spawned agents don't hang:"
    );
    println!(
        "     {}",
        "claude --dangerously-skip-permissions   # accept the prompt, then exit".dimmed()
    );
    println!(
        "     {}",
        "# Persists in ~/.claude.json; grove's baseline mount surfaces it into every container."
            .dimmed()
    );
    println!();
    if !repo_name_for_cd.is_empty() {
        println!("  {} {} {}", "2.".bold(), "cd".dimmed(), repo_name_for_cd);
        println!(
            "  {} {} <name>                  # create a regular worktree under worktrees/<name>",
            "3.".bold(),
            "grove add".dimmed()
        );
        println!(
            "  {} {} <name> --task \"...\"   # spawn an agent in an isolated worktree",
            "4.".bold(),
            "grove spawn".dimmed()
        );
    } else {
        println!(
            "  {} {} <name>                  # create a regular worktree under worktrees/<name>",
            "2.".bold(),
            "grove add".dimmed()
        );
        println!(
            "  {} {} <name> --task \"...\"   # spawn an agent in an isolated worktree",
            "3.".bold(),
            "grove spawn".dimmed()
        );
    }
}

// =============================================================================
// Shared phases
// =============================================================================

fn run_phase1_and_phase2(
    context: &worktree_manager::RepoContext,
    project: &ProjectContext,
    no_agent: bool,
    assume_yes: bool,
) {
    let project_root = worktree_manager::project_root(context);

    // ---- Phase 1 (deterministic; always runs, even with --no-agent) ----
    handle_devcontainer(context, project, assume_yes);
    handle_grove_config(context, project, assume_yes);
    handle_groverc(project_root);
    handle_gitignore(project_root);
    if project.has_dockerfile {
        handle_dockerignore(project_root);
    }
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
    // Baseline .claude/* mounts so the in-container claude can authenticate
    // and see the Stop hook even when the Phase 2 wizard is skipped (e.g.
    // `grove init --no-agent`). Phase 2's prompt_claude_scope can later
    // swap these for "full" or "none" — but the default is "scoped" so
    // agent-spawning works out of the box.
    if let Err(e) = apply_baseline_claude_mounts(project_root) {
        eprintln!(
            "  {} failed to add baseline .claude/* mounts: {}",
            "Warning:".yellow(),
            e
        );
    } else {
        println!(
            "  {} added baseline .claude/{{settings.json, .credentials.json, plugins}} RO mounts (scoped default)",
            "·".dimmed()
        );
    }

    // Optional: bind the host's tmux config RO so the container's tmux
    // inherits the user's keybinds, theme, status line. Skipped silently
    // when neither ~/.config/tmux/tmux.conf nor ~/.tmux.conf exists on
    // the host.
    match crate::devcontainer::apply_baseline_tmux_mount(project_root) {
        Ok(true) => println!(
            "  {} bound host tmux conf RO into /home/vscode/.tmux.conf",
            "·".dimmed()
        ),
        Ok(false) => {}
        Err(e) => eprintln!(
            "  {} failed to add tmux conf mount: {}",
            "Warning:".yellow(),
            e
        ),
    }

    // Ensure the container's git is ≥ 2.46 by pinning the official git
    // devcontainer feature with ppa: true + version: latest. Grove's
    // relative-worktree-pointer rewrite is unreadable by older git, so
    // host AND container both need 2.46+. Idempotent; preserves any
    // user-set version override.
    match ensure_container_git_feature(project_root) {
        Ok(true) => println!(
            "  {} pinned `ghcr.io/devcontainers/features/git:1` (ppa, latest) so container git ≥ 2.46",
            "·".dimmed()
        ),
        Ok(false) => {}
        Err(e) => eprintln!(
            "  {} could not add git feature: {} (container git may stay outdated)",
            "Warning:".yellow(),
            e
        ),
    }

    // Lint: warn about literal `~` in mount source= clauses. docker does NOT
    // expand `~`; the bind silently fails or refuses with `invalid mount
    // path: '~/...' mount path must be absolute`. Hardest-to-diagnose
    // devcontainer.json bug there is — surface at init time.
    warn_about_tilde_mounts(project_root);

    // Lint: warn if `containerUser` doesn't match `remoteUser`. docker
    // applies containerUser via `-u`, so if the base image lacks that user
    // the container can't even start (error: "unable to find user X").
    // Common failure when grove init scaffolds against a non-vscode base
    // image (e.g. freqtrade's `ftuser`).
    warn_about_user_mismatch(project_root);

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

    // build_grove_config runs BEFORE devcontainer.json may be on disk
    // (clone mode) or alongside an existing one (in-place). We don't
    // need a perfect user here — the actual devcontainer mounts come
    // from apply_cache_volumes_to_devcontainer which re-reads
    // devcontainer.json. This is .grove/config.toml metadata only;
    // "vscode" is the safe scaffold default.
    config.caches.volumes =
        crate::devcontainer::stack::cache_volumes(stack_enum, &project.repo_name, "vscode")
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
    // Capture workspaceFolder + remoteUser from devcontainer.json if present so
    // src/session/container.rs::ensure_up has the host→container path map
    // without re-parsing JSON each call. Falls back to /workspaces/<repo> +
    // "vscode" when devcontainer.json is absent (e.g., --no-devcontainer).
    if let Ok(value) = crate::devcontainer::read_devcontainer_json(project_root) {
        let (workspace_target, remote_user) =
            crate::devcontainer::extract_workspace_metadata(&value);
        if let Some(t) = workspace_target {
            config.devcontainer.workspace_target = Some(t);
        }
        if let Some(u) = remote_user {
            config.devcontainer.remote_user = u;
        }
    }
    if config.devcontainer.workspace_target.is_none() {
        config.devcontainer.workspace_target = Some(format!("/workspaces/{}", project.repo_name));
    }
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
    // Devcontainer workspace metadata: prefer existing user-set values; fall
    // back to defaults derived from devcontainer.json on this run. The
    // workspace_target Option semantics differ from remote_user (which has
    // a non-Option default), so handle each.
    if existing.devcontainer.workspace_target.is_none() {
        existing.devcontainer.workspace_target = defaults.devcontainer.workspace_target;
    }
    // remote_user always has a default; only swap if the existing config
    // still has the literal default and the new detection produced something
    // different.
    if existing.devcontainer.remote_user == "vscode"
        && defaults.devcontainer.remote_user != "vscode"
    {
        existing.devcontainer.remote_user = defaults.devcontainer.remote_user;
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

/// Ensure `.groverc` exists with a valid (possibly empty) bootstrap section.
///
/// Earlier revisions of this fork inserted a `devcontainer up
/// --workspace-folder .` entry here so `grove add` would bring the container
/// up. That's been **removed**: `grove spawn` is now the single entry point
/// that brings the container up via `container::ensure_up` (operates on the
/// project root, not the new worktree). `grove add` stays a generic worktree
/// CLI with no container responsibility.
///
/// To preserve idempotency on projects that previously had the entry, this
/// function also REMOVES any pre-existing `devcontainer up ...` bootstrap
/// command. User-added entries (`npm install`, `cargo check`, etc.) are
/// preserved.
fn ensure_groverc_bootstrap(project_root: &Path) -> Result<(), String> {
    let path = project_root.join(".groverc");
    let existed = path.exists();
    let mut value: serde_json::Value = if existed {
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
    if let Some(bootstrap) = obj.get_mut("bootstrap") {
        if let Some(bootstrap_obj) = bootstrap.as_object_mut() {
            if let Some(commands) = bootstrap_obj
                .get_mut("commands")
                .and_then(|c| c.as_array_mut())
            {
                commands.retain(|c| {
                    !(c.get("program").and_then(|p| p.as_str()) == Some("devcontainer")
                        && c.get("args")
                            .and_then(|a| a.as_array())
                            .map(|a| a.iter().any(|v| v == "up"))
                            .unwrap_or(false))
                });
            }
        }
    }

    // If we created the file from scratch (no prior `.groverc`), make sure
    // there's at least an empty bootstrap.commands block so future user edits
    // have something to extend.
    if !existed {
        obj.entry("bootstrap")
            .or_insert_with(|| serde_json::json!({ "commands": [] }));
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

/// Phase-1 baseline: mount `~/.claude/{settings.json, .credentials.json,
/// plugins}` RO into the container. These are load-bearing for grove's
/// agentic workflow:
///   - `.credentials.json` → in-container claude can authenticate
///   - `settings.json`     → Stop hook + enabledPlugins flow into the container
///   - `plugins/`          → claude plugins (skills, hooks) available
///
/// The mounts use `${localEnv:HOME}` so they resolve to the host user's home
/// at devcontainer-up time. The Phase 2 wizard's `prompt_claude_scope` can
/// later REMOVE these mounts and replace them with the "full" or "none"
/// alternative if the user wants different isolation.
fn apply_baseline_claude_mounts(project_root: &Path) -> Result<(), String> {
    let dev_path = project_root.join(".devcontainer").join("devcontainer.json");
    if !dev_path.exists() {
        return Ok(());
    }
    let raw =
        fs::read_to_string(&dev_path).map_err(|e| format!("read {}: {}", dev_path.display(), e))?;
    let mut value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {}", dev_path.display(), e))?;

    // Detect the container user (`remoteUser` || `containerUser` || vscode
    // default). Earlier versions hardcoded `vscode` here, which silently
    // broke when init ran against a project whose base image used a
    // different user (e.g. freqtrade's `ftuser`). Mount targets now route
    // to whichever user the existing devcontainer.json declares.
    let user = remote_user_from_devcontainer(&value);

    let obj = value
        .as_object_mut()
        .ok_or_else(|| "devcontainer.json top-level is not a JSON object".to_string())?;
    let mounts = obj
        .entry("mounts")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or_else(|| "devcontainer.json `mounts` is not an array".to_string())?;

    let baseline_subpaths = [
        ".claude/plugins",
        ".claude/.credentials.json",
        ".claude/settings.json",
        // Without ~/.claude.json the in-container claude sees a "fresh
        // install" → onboarding + danger-warning prompts block the
        // bootstrap turn. Mounting the host's onboarded state RO skips
        // both. RO is intentional: agent's session-state writes don't
        // leak back to the host's main claude profile.
        ".claude.json",
    ];
    for sub in baseline_subpaths {
        let source = format!("${{localEnv:HOME}}/{}", sub);
        let target = format!("/home/{}/{}", user, sub);
        let entry = format!("source={},target={},type=bind,readonly", source, target);
        // Skip if a mount with this exact target already exists (matches the
        // idempotency we use elsewhere — protects user edits and reruns).
        if mounts.iter().filter_map(|v| v.as_str()).any(|s| {
            s.contains(&format!("target={}", target)) || s.contains(&format!("target={},", target))
        }) {
            continue;
        }
        mounts.push(serde_json::Value::String(entry));
    }
    let body = serde_json::to_string_pretty(&value)
        .map_err(|e| format!("serialize {}: {}", dev_path.display(), e))?;
    fs::write(&dev_path, body).map_err(|e| format!("write {}: {}", dev_path.display(), e))?;
    Ok(())
}

/// Pick the user that mount targets should route to. Priority:
///   1. `remoteUser`    — the user the IDE/tools attach as (and what the
///                        agent runs as inside the container).
///   2. `containerUser` — fallback if remoteUser isn't set.
///   3. `"vscode"`      — last-resort default (matches the Microsoft
///                        devcontainers base images grove init scaffolds).
///
/// Public so devcontainer/mod.rs::apply_baseline_tmux_mount can call it
/// without duplicating the lookup.
pub(crate) fn remote_user_from_devcontainer(value: &serde_json::Value) -> String {
    value
        .get("remoteUser")
        .and_then(|v| v.as_str())
        .or_else(|| value.get("containerUser").and_then(|v| v.as_str()))
        .unwrap_or("vscode")
        .to_string()
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
    let user = remote_user_from_devcontainer(&value);
    let stack = project
        .stack
        .unwrap_or(crate::models::ProjectStack::Unknown);
    let vols = crate::devcontainer::stack::cache_volumes(stack, &project.repo_name, &user);

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

/// Pin `ghcr.io/devcontainers/features/git:1` in devcontainer.json's
/// `features` map so the container ends up with git ≥ 2.46. Required
/// because grove writes relative worktree pointers which older git
/// can't parse, and base images often ship 2.34 (Ubuntu 22.04).
///
/// Idempotent: if a `ghcr.io/devcontainers/features/git:1` key already
/// exists (regardless of version pin), leave it alone — operator may
/// have customized it. Returns Ok(true) if a new entry was added,
/// Ok(false) if already present, Err on parse/write failure.
fn ensure_container_git_feature(project_root: &Path) -> Result<bool, String> {
    let dev_path = project_root.join(".devcontainer").join("devcontainer.json");
    if !dev_path.exists() {
        return Ok(false);
    }
    let raw =
        fs::read_to_string(&dev_path).map_err(|e| format!("read {}: {}", dev_path.display(), e))?;
    let mut value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {}", dev_path.display(), e))?;
    let obj = value
        .as_object_mut()
        .ok_or_else(|| "devcontainer.json top-level is not a JSON object".to_string())?;
    let features = obj
        .entry("features")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| "devcontainer.json `features` is not an object".to_string())?;
    let key = "ghcr.io/devcontainers/features/git:1";
    if features.contains_key(key) {
        return Ok(false);
    }
    features.insert(
        key.to_string(),
        serde_json::json!({ "ppa": true, "version": "latest" }),
    );
    let body = serde_json::to_string_pretty(&value)
        .map_err(|e| format!("serialize {}: {}", dev_path.display(), e))?;
    fs::write(&dev_path, body).map_err(|e| format!("write {}: {}", dev_path.display(), e))?;
    Ok(true)
}

/// Scan `.devcontainer/devcontainer.json` for mount entries whose
/// `source=` value starts with a literal `~`. docker silently refuses
/// these at `docker run` time with "invalid mount path: must be absolute"
/// — debugging is brutal because the cause is far from the symptom.
/// Print a clear warning + fix hint per finding.
///
/// Handles both the string form (`"source=~/x,target=/y,type=bind"`) and
/// the object form (`{"source": "~/x", "target": "/y", ...}`).
fn warn_about_tilde_mounts(project_root: &Path) {
    let path = project_root.join(".devcontainer").join("devcontainer.json");
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return,
    };
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return,
    };
    let mounts = match value.get("mounts").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return,
    };

    let mut findings: Vec<String> = Vec::new();
    for m in mounts {
        if let Some(src) = mount_source_with_literal_tilde(m) {
            findings.push(src);
        }
    }

    if findings.is_empty() {
        return;
    }

    eprintln!(
        "  {} `.devcontainer/devcontainer.json` mount source(s) use a literal `~`:",
        "Warning:".yellow()
    );
    for src in &findings {
        eprintln!("    source={}", src.bold());
    }
    eprintln!(
        "    {} docker does NOT expand `~`; container creation will fail with `invalid mount path`.",
        "·".dimmed()
    );
    eprintln!(
        "    {} Fix: replace `~/...` with `${{localEnv:HOME}}/...` in each offending source.",
        "·".dimmed()
    );
}

/// If `mount` (string or object form) has a `source` value that begins
/// with a literal `~`, return that source string. Otherwise None.
fn mount_source_with_literal_tilde(mount: &serde_json::Value) -> Option<String> {
    if let Some(s) = mount.as_str() {
        // "source=~/.foo,target=/bar,..." — split on `,`, look for source=.
        for part in s.split(',') {
            if let Some(rest) = part.strip_prefix("source=") {
                if rest.starts_with('~') {
                    return Some(rest.to_string());
                }
                return None;
            }
        }
        return None;
    }
    if let Some(obj) = mount.as_object() {
        if let Some(src) = obj.get("source").and_then(|v| v.as_str()) {
            if src.starts_with('~') {
                return Some(src.to_string());
            }
        }
    }
    None
}

/// Minimum git version grove requires: 2.46 (when
/// `extensions.relativeWorktrees` landed). Returns Err with an actionable
/// upgrade hint if `git --version` is older or unparseable.
fn require_git_supports_relative_worktrees() -> Result<(), String> {
    const MIN_MAJOR: u32 = 2;
    const MIN_MINOR: u32 = 46;
    let out = std::process::Command::new("git")
        .arg("--version")
        .output()
        .map_err(|e| {
            format!(
                "could not run `git --version`: {} (grove needs git ≥ {}.{} on PATH)",
                e, MIN_MAJOR, MIN_MINOR
            )
        })?;
    if !out.status.success() {
        return Err(format!(
            "`git --version` exited non-zero (grove needs git ≥ {}.{} on PATH)",
            MIN_MAJOR, MIN_MINOR
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let (major, minor) = parse_git_version(&stdout).ok_or_else(|| {
        format!(
            "could not parse `git --version` output: {} (grove needs git ≥ {}.{} on PATH)",
            stdout.trim(),
            MIN_MAJOR,
            MIN_MINOR
        )
    })?;
    if (major, minor) < (MIN_MAJOR, MIN_MINOR) {
        return Err(format!(
            "git {}.{} is too old; grove needs git ≥ {}.{} for relative worktree support \
             (host ↔ devcontainer path parity). Upgrade via your package manager — on \
             Ubuntu/Debian: `sudo add-apt-repository ppa:git-core/ppa && sudo apt-get \
             update && sudo apt-get install --only-upgrade git`. macOS Homebrew: `brew \
             upgrade git`.",
            major, minor, MIN_MAJOR, MIN_MINOR
        ));
    }
    Ok(())
}

/// Parse "git version 2.54.0" / "git version 2.49.0.windows.1" → (2, 54).
/// Returns None on malformed input.
fn parse_git_version(s: &str) -> Option<(u32, u32)> {
    let rest = s.trim().strip_prefix("git version ")?;
    let mut parts = rest.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor_raw = parts.next()?;
    // Strip non-digit suffix (e.g. "49-rc1" → 49) so unusual builds parse.
    let minor_digits: String = minor_raw
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let minor: u32 = minor_digits.parse().ok()?;
    Some((major, minor))
}

/// Warn if `containerUser` is set to a different value than `remoteUser`.
/// `containerUser` controls `docker run -u`; if it names a user the base
/// image lacks, container creation fails immediately with "unable to find
/// user X: no matching entries in passwd file". Common when grove's
/// scaffold (which defaults both to `vscode`) lands in a project whose
/// base image only has a different user — the operator changed
/// `remoteUser` but forgot `containerUser`, or vice versa.
fn warn_about_user_mismatch(project_root: &Path) {
    let path = project_root.join(".devcontainer").join("devcontainer.json");
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return,
    };
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return,
    };
    let remote = value.get("remoteUser").and_then(|v| v.as_str());
    let container = value.get("containerUser").and_then(|v| v.as_str());
    if let (Some(r), Some(c)) = (remote, container) {
        if r != c {
            eprintln!(
                "  {} `.devcontainer/devcontainer.json` has containerUser={} but remoteUser={}",
                "Warning:".yellow(),
                c.bold(),
                r.bold()
            );
            eprintln!(
                "    {} docker `run -u {}` will fail if the base image doesn't have that user.",
                "·".dimmed(),
                c
            );
            eprintln!(
                "    {} Fix: set both to the same value (usually the user the base image provides).",
                "·".dimmed()
            );
        }
    }
}

fn patch_gitignore(project_root: &Path) -> Result<(), String> {
    let path = project_root.join(".gitignore");
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let entries = [
        "# grove",
        ".grove/agents/",
        ".grove/bus/",
        ".grove/logs/",
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
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        // Fresh file gets an empty bootstrap.commands block so user can
        // extend later. No devcontainer entry — `grove spawn` handles that.
        assert!(v["bootstrap"]["commands"].is_array());
        assert_eq!(v["bootstrap"]["commands"].as_array().unwrap().len(), 0);
        assert!(!body.contains("devcontainer"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_groverc_bootstrap_preserves_user_entries() {
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
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0]["program"], "npm");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_groverc_bootstrap_strips_legacy_devcontainer_entry() {
        // Older grove versions wrote a devcontainer-up bootstrap entry. Make
        // sure re-running init removes it (it ran from worktree cwd, which
        // is wrong for the shared-container model — grove spawn handles
        // container lifecycle now).
        let dir = tmp("groverc-strip-legacy");
        fs::write(
            dir.join(".groverc"),
            r#"{
              "bootstrap": {
                "commands": [
                  {"program":"devcontainer","args":["up","--workspace-folder","."]},
                  {"program":"npm","args":["install"]}
                ]
              }
            }"#,
        )
        .unwrap();
        ensure_groverc_bootstrap(&dir).unwrap();
        let body = fs::read_to_string(dir.join(".groverc")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let cmds = v["bootstrap"]["commands"].as_array().unwrap();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0]["program"], "npm");
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

    #[test]
    fn tilde_mount_string_form_detected() {
        let v = serde_json::json!("source=~/.config/x,target=/y,type=bind,readonly");
        let got = mount_source_with_literal_tilde(&v);
        assert_eq!(got, Some("~/.config/x".to_string()));
    }

    #[test]
    fn tilde_mount_object_form_detected() {
        let v = serde_json::json!({
            "source": "~/.config/x",
            "target": "/y",
            "type": "bind"
        });
        let got = mount_source_with_literal_tilde(&v);
        assert_eq!(got, Some("~/.config/x".to_string()));
    }

    #[test]
    fn local_env_home_mount_not_flagged() {
        let v = serde_json::json!("source=${localEnv:HOME}/.config/x,target=/y,type=bind");
        assert_eq!(mount_source_with_literal_tilde(&v), None);
        let v2 = serde_json::json!({"source": "${localEnv:HOME}/.config/x", "target": "/y"});
        assert_eq!(mount_source_with_literal_tilde(&v2), None);
    }

    #[test]
    fn absolute_path_mount_not_flagged() {
        let v = serde_json::json!("source=/home/martin/.config/x,target=/y,type=bind");
        assert_eq!(mount_source_with_literal_tilde(&v), None);
    }

    #[test]
    fn named_volume_mount_not_flagged() {
        let v = serde_json::json!("source=grove-uv-cache,target=/cache,type=volume");
        assert_eq!(mount_source_with_literal_tilde(&v), None);
    }

    #[test]
    fn mount_without_source_clause_not_flagged() {
        let v = serde_json::json!("type=tmpfs,target=/tmp");
        assert_eq!(mount_source_with_literal_tilde(&v), None);
    }

    #[test]
    fn remote_user_prefers_remote_user_field() {
        let v = serde_json::json!({
            "remoteUser": "ftuser",
            "containerUser": "vscode"
        });
        assert_eq!(remote_user_from_devcontainer(&v), "ftuser");
    }

    #[test]
    fn remote_user_falls_back_to_container_user() {
        let v = serde_json::json!({"containerUser": "ubuntu"});
        assert_eq!(remote_user_from_devcontainer(&v), "ubuntu");
    }

    #[test]
    fn parse_git_version_modern() {
        assert_eq!(parse_git_version("git version 2.54.0\n"), Some((2, 54)));
        assert_eq!(parse_git_version("git version 2.46.1"), Some((2, 46)));
        assert_eq!(parse_git_version("git version 3.0.0"), Some((3, 0)));
    }

    #[test]
    fn parse_git_version_handles_suffixes() {
        assert_eq!(
            parse_git_version("git version 2.49.0.windows.1\n"),
            Some((2, 49))
        );
        assert_eq!(
            parse_git_version("git version 2.34.1-1ubuntu1.17"),
            Some((2, 34))
        );
    }

    #[test]
    fn parse_git_version_rejects_garbage() {
        assert_eq!(parse_git_version("not a git version"), None);
        assert_eq!(parse_git_version(""), None);
        assert_eq!(parse_git_version("git version weird"), None);
    }

    #[test]
    fn remote_user_default_is_vscode() {
        let v = serde_json::json!({});
        assert_eq!(remote_user_from_devcontainer(&v), "vscode");
    }

    #[test]
    fn baseline_claude_mounts_route_to_detected_user() {
        let root = tmp("baseline-user");
        let dc = root.join(".devcontainer");
        fs::create_dir_all(&dc).unwrap();
        fs::write(
            dc.join("devcontainer.json"),
            r#"{"name":"x","image":"y","remoteUser":"ftuser","containerUser":"ftuser","mounts":[]}"#,
        )
        .unwrap();
        apply_baseline_claude_mounts(&root).unwrap();
        let body = fs::read_to_string(dc.join("devcontainer.json")).unwrap();
        assert!(body.contains("target=/home/ftuser/.claude/plugins"));
        assert!(body.contains("target=/home/ftuser/.claude/.credentials.json"));
        assert!(body.contains("target=/home/ftuser/.claude/settings.json"));
        assert!(!body.contains("/home/vscode/"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn baseline_claude_mounts_default_user_is_vscode() {
        let root = tmp("baseline-default-user");
        let dc = root.join(".devcontainer");
        fs::create_dir_all(&dc).unwrap();
        // No remoteUser / containerUser declared → fallback "vscode".
        fs::write(
            dc.join("devcontainer.json"),
            r#"{"name":"x","image":"y","mounts":[]}"#,
        )
        .unwrap();
        apply_baseline_claude_mounts(&root).unwrap();
        let body = fs::read_to_string(dc.join("devcontainer.json")).unwrap();
        assert!(body.contains("target=/home/vscode/.claude/plugins"));
        let _ = fs::remove_dir_all(&root);
    }
}
