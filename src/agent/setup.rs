// Phase 2 of `grove init` — interactive setup wizard.
//
// Five interactive prompts (driven by `dialoguer`):
//
//   1. Project secrets mount        (path, RO/RW, env var name)
//   2. .claude scope + auth         (scoped/full/none + creds strategy)
//   3. GitHub auth                  (RO PAT recommended; replaces full mount)
//   4. Inferred extra mounts        (heuristic scan over root manifests + README
//                                    for env-var refs like MARKET_DATA_DIR, etc.)
//   5. Extensions + container pkgs  (fixed defaults + per-stack inferred)
//
// Steps 4 and 5 are pure heuristics today (no LLM call). Wiring `claude -p` for
// richer inference is documented as a P1 follow-up in AGENTIC-FLOW-REPORT.md.
//
// All decisions are persisted to `.grove/config.toml` AND applied to
// `.devcontainer/devcontainer.json` (mutate-in-place). The wizard is
// idempotent — running `grove init --reconfigure` re-prompts with current
// values as defaults so users can tighten security incrementally.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use colored::Colorize;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, MultiSelect, Select};
use serde_json::{json, Value};

use crate::devcontainer::stack;
use crate::devcontainer::{read_devcontainer_json, write_devcontainer_json};
use crate::git::worktree_manager::{project_root, RepoContext};
use crate::models::{ContainerBackendKind, ExtraMount, GroveConfig, ProjectContext, ProjectStack};
use crate::session::container::{self, ContainerInfo};
use crate::session::tmux;

pub fn run_setup_wizard(
    ctx: &RepoContext,
    project: &ProjectContext,
    is_reconfigure: bool,
) -> Result<(), String> {
    let project_root_path = project_root(ctx).to_path_buf();

    // Bail gracefully if we can't actually prompt (CI / non-interactive shell).
    if !atty::is(atty::Stream::Stdin) {
        println!(
            "  {} Phase 2 wizard skipped (stdin is not a TTY). Run `grove init --reconfigure` interactively to enable.",
            "·".dimmed()
        );
        return Ok(());
    }

    println!();
    println!(
        "{} {}",
        "▶".cyan(),
        if is_reconfigure {
            "Re-running setup wizard with current config as defaults."
        } else {
            "Setup wizard — answering a few short prompts; you can re-run anytime via `grove init --reconfigure`."
        }
    );

    let mut config = read_or_default_config(&project_root_path);

    // ----- Prompt 0: container backend (devcontainer vs sandbox) -----
    // Chosen here so it's available at `grove init` and re-promptable via
    // `grove init --reconfigure` (both route through this wizard). All grove
    // commands read the result from `.grove/config.toml`, so they stay
    // arg-free.
    let old_backend = config.container.backend;
    prompt_container_backend(&mut config)?;

    // Reconfigure safety: switching backend on a live project orphans the old
    // backend's containers/agents. Require teardown first (B7).
    if is_reconfigure && config.container.backend != old_backend {
        if !handle_backend_switch(&project_root_path, old_backend, config.container.backend)? {
            config.container.backend = old_backend;
            println!(
                "  {} keeping the {} backend (switch cancelled)",
                "·".dimmed(),
                old_backend.as_str().bold()
            );
        }
    }

    // Sandbox mode has a trimmed flow: no devcontainer.json, no VS Code
    // extensions — just the image/user preset, credentials, and config.
    if config.container.backend == ContainerBackendKind::Sandbox {
        return run_sandbox_wizard(project, &project_root_path, config);
    }

    let mut devcontainer = read_devcontainer_json(&project_root_path).unwrap_or_else(|_| {
        json!({
            "name": project.repo_name,
            "image": project.default_image,
            "mounts": [],
            "containerEnv": {},
            "customizations": { "vscode": { "extensions": [] } }
        })
    });

    // ----- Prompt 0.5: environment preset (image + agentic toolchain) -----
    // Picking a preset sets the image, the container user, the devcontainer
    // features (Node/git/gh/claude-code), and the extensions in one shot —
    // nothing for the operator to hand-fix afterward.
    prompt_environment_preset(project, &mut config, &mut devcontainer)?;

    // The container user follows whatever the (possibly preset-updated)
    // devcontainer.json declares — never a hardcoded literal. Mount targets
    // below route to this user.
    let user = container_user_for_targets(&devcontainer);
    println!(
        "  {} container user (mount targets route to): {}",
        "·".dimmed(),
        user.bold()
    );

    // ----- Prompt 1: project secrets mount -----
    prompt_secrets_mount(project, &mut config, &mut devcontainer, &user)?;

    // ----- Prompt 2: .claude scope + auth -----
    prompt_claude_scope(&mut config, &mut devcontainer, &user)?;

    // ----- Prompt 3: GitHub auth -----
    prompt_github_auth(&mut config, &mut devcontainer, &user, &project.repo_name)?;

    // ----- Prompt 4: agent-inferred extra mounts -----
    prompt_inferred_mounts(ctx, project, &mut config, &mut devcontainer, &user)?;

    // ----- Prompt 5: extensions + container packages -----
    prompt_extensions_and_packages(project, &mut config, &mut devcontainer)?;

    // Apply container packages: postCreateCommand assembled from package_manager_install
    // plus pre-commit/husky/lefthook installers when detected.
    apply_post_create(project, &mut devcontainer);

    // Persist everything.
    write_config(&project_root_path, &config)?;
    write_devcontainer_json(&project_root_path, &devcontainer)?;
    println!();
    println!(
        "{} wrote .grove/config.toml + .devcontainer/devcontainer.json",
        "✓".green()
    );
    Ok(())
}

// ---------- sandbox flow ----------

/// Trimmed setup flow for the sandbox backend. No devcontainer.json, no VS
/// Code extensions (irrelevant to `docker run`): pick the environment preset
/// (records image + user into `[sandbox]`), choose claude credential scope and
/// GitHub auth, and persist `.grove/config.toml`. The sandbox provisions the
/// preset image at `grove spawn` time.
fn run_sandbox_wizard(
    project: &ProjectContext,
    project_root_path: &Path,
    mut config: GroveConfig,
) -> Result<(), String> {
    use crate::devcontainer::preset;

    println!();
    println!(
        "{}",
        "Sandbox backend selected — code is copied into the container; the only egress is `git push`."
            .dimmed()
    );

    // Preset → records the image + user the sandbox runs.
    let chosen = select_preset(project)?;
    if chosen.id != preset::PresetId::Custom {
        config.sandbox.image = Some(chosen.image.to_string());
        config.sandbox.user = Some(chosen.remote_user.to_string());
        // Keep the canonical user field consistent across backends.
        config.devcontainer.remote_user = chosen.remote_user.to_string();
        println!(
            "  {} sandbox image: {} (user {})",
            "·".dimmed(),
            chosen.image.bold(),
            chosen.remote_user.bold()
        );
    } else {
        println!(
            "  {} custom: set `[sandbox] image` in .grove/config.toml manually",
            "·".dimmed()
        );
    }

    // Claude credential scope — applied as container mounts at spawn time.
    prompt_claude_scope_sandbox(&mut config)?;

    // GitHub auth — the sandbox injects GH_TOKEN from the host's GH_TOKEN_RO.
    prompt_github_auth_sandbox(&mut config, &project.repo_name)?;

    write_config(project_root_path, &config)?;
    println!();
    println!("{} wrote .grove/config.toml (sandbox backend)", "✓".green());
    println!(
        "  {} first `grove spawn` will pull the image and seed the sandbox; this can take a while.",
        "·".dimmed()
    );
    Ok(())
}

/// Claude credential scope for sandbox mode. Records the preference; the
/// SandboxBackend turns it into bind mounts (scoped = three RO resources,
/// full = ~/.claude RW, none = bring-your-own) when the container is created.
fn prompt_claude_scope_sandbox(config: &mut GroveConfig) -> Result<(), String> {
    let theme = ColorfulTheme::default();
    println!();
    let options = vec![
        "Scoped (recommended) — mount ~/.claude/{plugins,.credentials.json,settings.json} RO",
        "Full (RW)            — mount all of ~/.claude read-write; advanced only",
        "None                 — provision claude auth inside the image yourself",
    ];
    let default_idx = match config.mounts.claude_inherit.as_deref() {
        Some("full") => 1,
        Some("none") => 2,
        _ => 0,
    };
    let idx = Select::with_theme(&theme)
        .with_prompt("Claude credentials in the sandbox")
        .items(&options)
        .default(default_idx)
        .interact()
        .map_err(|e| format!("prompt: {}", e))?;
    config.mounts.claude_inherit = Some(
        match idx {
            1 => "full",
            2 => "none",
            _ => "scoped",
        }
        .to_string(),
    );
    Ok(())
}

/// Prompt for the host env-var NAME that holds this project's GitHub PAT, store
/// it in `[mounts] gh_token_env`, and return it. Fine-grained PATs are
/// repo-scoped, so each project points at its own var (e.g. `GH_PAT_FREQTRADE`);
/// grove never stores the token value. Defaults to the current value or the
/// legacy global `GH_TOKEN_RO`.
fn prompt_gh_token_env(config: &mut GroveConfig, repo_name: &str) -> Result<String, String> {
    let theme = ColorfulTheme::default();
    let suggested: String = repo_name
        .to_ascii_uppercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let default = config
        .mounts
        .gh_token_env
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "GH_TOKEN_RO".to_string());
    println!(
        "  {} Fine-grained PATs are per-repo. Name a host var per project, e.g. GH_PAT_{}.",
        "Tip:".cyan(),
        suggested
    );
    let name: String = Input::with_theme(&theme)
        .with_prompt("Host env var holding the GitHub PAT")
        .default(default)
        .interact_text()
        .map_err(|e| format!("prompt: {}", e))?;
    let name = name.trim().to_string();
    config.mounts.gh_token_env = Some(name.clone());
    println!(
        "  {} Export {} on your host with a fine-grained PAT scoped to this repo \
         (Contents: Read/Write to push + Pull requests: Read/Write for `gh pr create`).",
        "Tip:".cyan(),
        name.bold()
    );
    Ok(name)
}

/// GitHub auth for sandbox mode. The sandbox forwards the host PAT (named by
/// `[mounts] gh_token_env`) as `GH_TOKEN`; this records whether to do so.
fn prompt_github_auth_sandbox(config: &mut GroveConfig, repo_name: &str) -> Result<(), String> {
    let theme = ColorfulTheme::default();
    println!();
    let options = vec![
        "PAT via a host env var (recommended) — forwarded into the sandbox as GH_TOKEN",
        "Skip (no GitHub access from inside the sandbox)",
    ];
    let default_idx = match config.mounts.gh_auth.as_deref() {
        Some("none") => 1,
        _ => 0,
    };
    let idx = Select::with_theme(&theme)
        .with_prompt("GitHub authentication")
        .items(&options)
        .default(default_idx)
        .interact()
        .map_err(|e| format!("prompt: {}", e))?;
    if idx == 0 {
        config.mounts.gh_auth = Some("pat".to_string());
        prompt_gh_token_env(config, repo_name)?;
    } else {
        config.mounts.gh_auth = Some("none".to_string());
    }
    Ok(())
}

/// B7 — reconfigure safety. When the backend changes, the old backend's
/// containers/agents would be orphaned. If agents are live on the old backend,
/// require explicit teardown before switching. Returns Ok(true) to proceed
/// with the switch, Ok(false) to keep the old backend.
///
/// `.grove/config.toml` still holds the OLD backend at this point (the wizard
/// writes the new value only at the end), so `container::*`/`tmux::*` route to
/// the old backend here.
fn handle_backend_switch(
    project_root_path: &Path,
    old: ContainerBackendKind,
    new: ContainerBackendKind,
) -> Result<bool, String> {
    let info = backend_container_info(project_root_path, old);
    let sessions = tmux::list_grove_sessions(Some(&info)).unwrap_or_default();

    if sessions.is_empty() {
        // No live agents — safe to switch. Best-effort teardown of the old
        // container so it isn't orphaned.
        let _ = container::down(project_root_path);
        return Ok(true);
    }

    println!();
    println!(
        "  {} {} agent session(s) are live on the {} backend: {}",
        "Warning:".yellow(),
        sessions.len(),
        old.as_str(),
        sessions.join(", ")
    );
    let confirm = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt(format!(
            "Tear them down + remove the old {} container to switch to {}?",
            old.as_str(),
            new.as_str()
        ))
        .default(false)
        .interact()
        .map_err(|e| format!("prompt: {}", e))?;
    if !confirm {
        return Ok(false);
    }
    for s in &sessions {
        let _ = tmux::kill_session(s, Some(&info));
    }
    container::down(project_root_path)?;
    println!(
        "  {} tore down the old {} backend",
        "·".dimmed(),
        old.as_str()
    );
    Ok(true)
}

/// Build a `ContainerInfo` addressing the given backend for a project, used to
/// probe/tear down the *current* (pre-switch) backend during reconfigure.
fn backend_container_info(project_root_path: &Path, kind: ContainerBackendKind) -> ContainerInfo {
    match kind {
        ContainerBackendKind::Sandbox => crate::session::backend::sandbox_info(project_root_path),
        ContainerBackendKind::Devcontainer => {
            let cfg = read_or_default_config(project_root_path);
            let target = cfg.devcontainer.workspace_target.unwrap_or_else(|| {
                format!(
                    "/workspaces/{}",
                    project_root_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("workspace")
                )
            });
            ContainerInfo::new(
                project_root_path.to_path_buf(),
                std::path::PathBuf::from(target),
                cfg.devcontainer.remote_user,
            )
        }
    }
}

// ---------- prompts ----------

/// Prompt 0: choose the container backend. Defaults to the current config
/// value so `grove init --reconfigure` re-prompts with the existing choice
/// pre-selected.
fn prompt_container_backend(config: &mut GroveConfig) -> Result<(), String> {
    let theme = ColorfulTheme::default();
    let items = [
        "devcontainer — bind-mounted; edits land on the host live (default)",
        "sandbox — copy-in isolation; the only way edits escape is `git push`",
    ];
    let default_idx = match config.container.backend {
        ContainerBackendKind::Devcontainer => 0,
        ContainerBackendKind::Sandbox => 1,
    };
    println!();
    let idx = Select::with_theme(&theme)
        .with_prompt("Container backend")
        .items(&items)
        .default(default_idx)
        .interact()
        .map_err(|e| format!("prompt: {}", e))?;
    config.container.backend = if idx == 1 {
        ContainerBackendKind::Sandbox
    } else {
        ContainerBackendKind::Devcontainer
    };
    println!(
        "  {} backend: {}",
        "·".dimmed(),
        config.container.backend.as_str().bold()
    );
    Ok(())
}

/// Prompt 0.5: choose the environment preset. Pre-selects the preset matching
/// the detected stack. A non-custom choice rewrites the devcontainer.json image,
/// container user, features, and extensions, and records the user in config —
/// so the container is ready to use with nothing to hand-edit. `custom` leaves
/// the existing image/devcontainer.json untouched.
/// Run the environment-preset `Select` prompt, pre-selecting the preset that
/// matches the detected stack. Shared by the devcontainer and sandbox flows.
fn select_preset(
    project: &ProjectContext,
) -> Result<&'static crate::devcontainer::preset::EnvironmentPreset, String> {
    use crate::devcontainer::preset;
    let theme = ColorfulTheme::default();
    let presets = preset::all();
    let labels: Vec<&str> = presets.iter().map(|p| p.label).collect();
    let detected = preset::for_stack(project.stack.unwrap_or(ProjectStack::Unknown));
    let default_idx = presets
        .iter()
        .position(|p| p.id == detected.id)
        .unwrap_or(0);
    println!();
    let idx = Select::with_theme(&theme)
        .with_prompt("Environment preset (image + agentic toolchain)")
        .items(&labels)
        .default(default_idx)
        .interact()
        .map_err(|e| format!("prompt: {}", e))?;
    Ok(presets[idx])
}

fn prompt_environment_preset(
    project: &ProjectContext,
    config: &mut GroveConfig,
    devcontainer: &mut Value,
) -> Result<(), String> {
    use crate::devcontainer::preset;
    let chosen = select_preset(project)?;

    if chosen.id == preset::PresetId::Custom {
        println!(
            "  {} keeping existing image / devcontainer.json",
            "·".dimmed()
        );
        return Ok(());
    }

    if let Some(obj) = devcontainer.as_object_mut() {
        obj.insert("image".to_string(), json!(chosen.image));
        obj.insert("remoteUser".to_string(), json!(chosen.remote_user));
        obj.insert("containerUser".to_string(), json!(chosen.remote_user));
        obj.insert("features".to_string(), preset::features_object(chosen));
    }
    let exts: Vec<String> = chosen.extensions.iter().map(|s| s.to_string()).collect();
    set_extensions(devcontainer, &exts);
    config.devcontainer.remote_user = chosen.remote_user.to_string();

    println!(
        "  {} preset: {} (user {})",
        "·".dimmed(),
        chosen.label.bold(),
        chosen.remote_user.bold()
    );
    Ok(())
}

/// Pick the user that wizard-written mount targets should route to. Delegates
/// to the canonical `devcontainer::remote_user_from_value`.
fn container_user_for_targets(value: &Value) -> String {
    crate::devcontainer::remote_user_from_value(value)
}

fn prompt_secrets_mount(
    project: &ProjectContext,
    config: &mut GroveConfig,
    devcontainer: &mut Value,
    user: &str,
) -> Result<(), String> {
    let theme = ColorfulTheme::default();
    let default_path = config
        .mounts
        .secrets_path
        .clone()
        .unwrap_or_else(|| format!("~/.config/{}", project.repo_name));
    println!();
    let do_mount = Confirm::with_theme(&theme)
        .with_prompt("Mount a project secrets directory into the devcontainer?")
        .default(config.mounts.secrets_path.is_some())
        .interact()
        .map_err(|e| format!("prompt: {}", e))?;
    if !do_mount {
        config.mounts.secrets_path = None;
        config.mounts.secrets_mode = None;
        return Ok(());
    }
    let path: String = Input::with_theme(&theme)
        .with_prompt("Host path to project secrets")
        .default(default_path)
        .interact_text()
        .map_err(|e| format!("prompt: {}", e))?;
    let mode_idx = Select::with_theme(&theme)
        .with_prompt("Mount mode")
        .items(&["read-only (recommended)", "read-write"])
        .default(0)
        .interact()
        .map_err(|e| format!("prompt: {}", e))?;
    let mode = if mode_idx == 0 { "ro" } else { "rw" };
    let env_name_default = format!(
        "{}_SECRETS_DIR",
        project
            .repo_name
            .to_ascii_uppercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>()
    );
    let env_name: String = Input::with_theme(&theme)
        .with_prompt("Env var to expose the path inside the container")
        .default(env_name_default)
        .interact_text()
        .map_err(|e| format!("prompt: {}", e))?;

    let target = format!("/home/{}/.config/{}", user, project.repo_name);
    add_mount(devcontainer, &path, &target, mode);
    set_remote_env(devcontainer, &env_name, &target);

    config.mounts.secrets_path = Some(path);
    config.mounts.secrets_mode = Some(mode.to_string());
    Ok(())
}

/// Phase-2 refinement of the `.claude/*` mounts.
///
/// Phase 1 already added the three `scoped` mounts as baseline:
///   ~/.claude/plugins              (RO)
///   ~/.claude/.credentials.json    (RO)
///   ~/.claude/settings.json        (RO)
///
/// This prompt offers three choices:
///   - **Scoped (default)** — keep the baseline as-is.
///   - **Full (override)**  — REMOVE the three RO mounts, add `~/.claude` RW.
///                            Exposes more (session history, write access);
///                            recommended only for advanced users.
///   - **None (remove)**    — REMOVE the three RO mounts entirely. User
///                            authenticates inside the container per rebuild
///                            (interactive `claude login` won't work in a
///                            detached tmux; mostly useful for custom
///                            container images that bake in auth + hooks).
fn prompt_claude_scope(
    config: &mut GroveConfig,
    devcontainer: &mut Value,
    user: &str,
) -> Result<(), String> {
    let theme = ColorfulTheme::default();
    println!();
    println!(
        "{}",
        "grove init already added the recommended `scoped` mounts (~/.claude/{plugins, .credentials.json, settings.json} RO). Adjust if needed:".dimmed()
    );
    let options = vec![
        "Keep scoped (recommended) — the three RO mounts grove init added",
        "Switch to full (RW)       — exposes session history + container can write settings; advanced only",
        "Remove all .claude mounts — bring your own auth + hooks inside the container",
    ];
    let default_idx = match config.mounts.claude_inherit.as_deref() {
        Some("full") => 1,
        Some("none") => 2,
        _ => 0,
    };
    let idx = Select::with_theme(&theme)
        .with_prompt("Claude resource inheritance")
        .items(&options)
        .default(default_idx)
        .interact()
        .map_err(|e| format!("prompt: {}", e))?;
    let key = match idx {
        0 => "scoped",
        1 => "full",
        _ => "none",
    };
    config.mounts.claude_inherit = Some(key.to_string());

    match key {
        "scoped" => {
            // Baseline mounts are already in place from Phase 1. No-op.
            println!(
                "  {} keeping Phase 1 baseline mounts (scoped).",
                "·".dimmed()
            );
        }
        "full" => {
            // Remove the three RO mounts, add one RW. Adjusts Phase 1's choice.
            remove_claude_mounts(devcontainer);
            let claude_target = format!("/home/{}/.claude", user);
            add_mount(
                devcontainer,
                "${localEnv:HOME}/.claude",
                &claude_target,
                "rw",
            );
            println!(
                "  {} swapped baseline → full ~/.claude RW. WARNING: session history readable + container writes propagate to host.",
                "Note:".cyan()
            );
        }
        "none" => {
            remove_claude_mounts(devcontainer);
            println!(
                "  {} removed all .claude mounts. You'll need to provision claude + Stop hook inside the container (custom image or postCreateCommand).",
                "Note:".cyan()
            );
        }
        _ => {}
    }
    Ok(())
}

/// Strip every `source=...claude...,target=...claude...` mount entry from
/// `devcontainer.json mounts`. Used when switching from scoped → full / none.
fn remove_claude_mounts(devcontainer: &mut Value) {
    let Some(mounts) = devcontainer
        .as_object_mut()
        .and_then(|o| o.get_mut("mounts"))
        .and_then(|m| m.as_array_mut())
    else {
        return;
    };
    mounts.retain(|v| {
        let s = match v.as_str() {
            Some(s) => s,
            None => return true,
        };
        !(s.contains(".claude"))
    });
}

fn prompt_github_auth(
    config: &mut GroveConfig,
    devcontainer: &mut Value,
    user: &str,
    repo_name: &str,
) -> Result<(), String> {
    let theme = ColorfulTheme::default();
    println!();
    let options = vec![
        "PAT via a host env var (per-project fine-grained PAT) — recommended",
        "Mount ~/.config/gh read-only (any scope, still leaks token)",
        "Mount ~/.config/gh read-write (matches the freqtrade default, NOT recommended)",
        "Skip (no GitHub access from inside the container)",
    ];
    let default_idx = match config.mounts.gh_auth.as_deref() {
        Some("ro-mount") => 1,
        Some("rw-mount") => 2,
        Some("none") => 3,
        _ => 0,
    };
    let idx = Select::with_theme(&theme)
        .with_prompt("GitHub authentication")
        .items(&options)
        .default(default_idx)
        .interact()
        .map_err(|e| format!("prompt: {}", e))?;
    let key = match idx {
        0 => "pat",
        1 => "ro-mount",
        2 => "rw-mount",
        _ => "none",
    };
    config.mounts.gh_auth = Some(key.to_string());
    match key {
        "pat" => {
            let var = prompt_gh_token_env(config, repo_name)?;
            set_remote_env(devcontainer, "GH_TOKEN", &format!("${{localEnv:{}}}", var));
        }
        "ro-mount" => {
            let gh_target = format!("/home/{}/.config/gh", user);
            add_mount(
                devcontainer,
                "${localEnv:HOME}/.config/gh",
                &gh_target,
                "ro",
            );
        }
        "rw-mount" => {
            let gh_target = format!("/home/{}/.config/gh", user);
            add_mount(
                devcontainer,
                "${localEnv:HOME}/.config/gh",
                &gh_target,
                "rw",
            );
        }
        _ => {}
    }
    Ok(())
}

fn prompt_inferred_mounts(
    ctx: &RepoContext,
    project: &ProjectContext,
    config: &mut GroveConfig,
    devcontainer: &mut Value,
    user: &str,
) -> Result<(), String> {
    println!();
    let candidates = infer_extra_mount_candidates(ctx, project, user);
    if candidates.is_empty() {
        println!(
            "  {} no extra mount candidates found in this repo",
            "·".dimmed()
        );
        return Ok(());
    }

    let theme = ColorfulTheme::default();
    let labels: Vec<String> = candidates
        .iter()
        .map(|c| {
            format!(
                "{:<24} -> {} ({}; {})",
                c.source, c.target, c.mode, c.reason
            )
        })
        .collect();
    let picks = MultiSelect::with_theme(&theme)
        .with_prompt("Inferred extra mounts (space to toggle, enter to confirm)")
        .items(&labels)
        .defaults(&vec![true; candidates.len()])
        .interact()
        .map_err(|e| format!("prompt: {}", e))?;

    for i in picks {
        let c = &candidates[i];
        add_mount(devcontainer, &c.source, &c.target, &c.mode);
        config.mounts.extra.push(c.clone());
    }
    Ok(())
}

fn prompt_extensions_and_packages(
    project: &ProjectContext,
    config: &mut GroveConfig,
    devcontainer: &mut Value,
) -> Result<(), String> {
    println!();
    let stack = project.stack.unwrap_or(ProjectStack::Unknown);
    let defaults = stack::default_extensions(stack);

    // Stack-inferred extras based on config-file presence (Dockerfile, *.proto,
    // *.tf, husky etc).
    let mut inferred: Vec<&'static str> = Vec::new();
    if project.has_dockerfile {
        inferred.push("ms-azuretools.vscode-docker");
    }
    if project.has_pre_commit {
        // pre-commit doesn't have a single canonical extension; rely on
        // language-specific ones already in defaults.
    }
    if project
        .root_files
        .iter()
        .any(|p| p == "Makefile" || p == "justfile")
    {
        inferred.push("nefrob.vscode-just-syntax");
    }
    if project
        .root_files
        .iter()
        .any(|p| p.ends_with(".tf") || p.ends_with(".tfvars"))
    {
        inferred.push("hashicorp.terraform");
    }
    if project.root_files.iter().any(|p| p.ends_with(".proto")) {
        inferred.push("zxh404.vscode-proto3");
    }

    let mut all: Vec<&str> = defaults
        .iter()
        .copied()
        .chain(inferred.iter().copied())
        .collect();
    // Dedup while preserving order.
    let mut seen = std::collections::HashSet::new();
    all.retain(|s| seen.insert(*s));

    let theme = ColorfulTheme::default();
    let picks = MultiSelect::with_theme(&theme)
        .with_prompt("VS Code extensions (defaults + inferred; toggle to taste)")
        .items(&all)
        .defaults(&vec![true; all.len()])
        .interact()
        .map_err(|e| format!("prompt: {}", e))?;
    let chosen: Vec<String> = picks.into_iter().map(|i| all[i].to_string()).collect();

    set_extensions(devcontainer, &chosen);
    config.stack.detected = Some(stack.as_str().to_string());
    // Stack ext lists in [extensions] — we don't have a typed section for them
    // yet; leave them in devcontainer.json which is the source of truth.

    Ok(())
}

// ---------- devcontainer.json helpers ----------

fn add_mount(devcontainer: &mut Value, source: &str, target: &str, mode: &str) {
    let mounts = devcontainer.as_object_mut().and_then(|o| {
        o.entry("mounts")
            .or_insert_with(|| json!([]))
            .as_array_mut()
    });
    let Some(arr) = mounts else {
        return;
    };
    let entry = if mode == "ro" {
        format!("source={},target={},type=bind,readonly", source, target)
    } else {
        format!("source={},target={},type=bind", source, target)
    };
    if !arr.iter().any(|v| v == &Value::String(entry.clone())) {
        arr.push(Value::String(entry));
    }
}

/// Set an env var in devcontainer.json's `remoteEnv` (NOT `containerEnv`).
/// `remoteEnv` is applied by the devcontainer CLI on every `devcontainer exec`
/// / attach, so changing a value (e.g. rotating a PAT, or editing the host var
/// `${localEnv:...}` resolves) takes effect on the next `grove spawn` with NO
/// container rebuild. `containerEnv` is baked at container-create and would
/// require `--remove-existing-container`. Prefer non-rebuild methods.
fn set_remote_env(devcontainer: &mut Value, key: &str, value: &str) {
    // Migrate away any stale `containerEnv` copy (older grove wrote there) so the
    // baked-at-create value can't shadow/duplicate the live remoteEnv one.
    if let Some(ce) = devcontainer
        .as_object_mut()
        .and_then(|o| o.get_mut("containerEnv"))
        .and_then(|c| c.as_object_mut())
    {
        ce.remove(key);
    }
    let env = devcontainer.as_object_mut().and_then(|o| {
        o.entry("remoteEnv")
            .or_insert_with(|| json!({}))
            .as_object_mut()
    });
    if let Some(map) = env {
        map.insert(key.to_string(), Value::String(value.to_string()));
    }
}

fn set_extensions(devcontainer: &mut Value, exts: &[String]) {
    let root = devcontainer.as_object_mut();
    let Some(obj) = root else {
        return;
    };
    let custom = obj.entry("customizations").or_insert_with(|| json!({}));
    let vscode = custom.as_object_mut().and_then(|m| {
        m.entry("vscode")
            .or_insert_with(|| json!({}))
            .as_object_mut()
    });
    if let Some(vs) = vscode {
        vs.insert(
            "extensions".to_string(),
            Value::Array(exts.iter().map(|s| Value::String(s.clone())).collect()),
        );
    }
}

/// Append project-stack installs to the existing postCreateCommand. The
/// existing command is grove's container prereqs (set by Phase 1's
/// `build_devcontainer_skeleton`); we preserve it so even `--no-agent` users
/// get tmux + jq + perl + claude in the container.
fn apply_post_create(project: &ProjectContext, devcontainer: &mut Value) {
    let pm = project.package_manager.as_deref();
    let install = stack::package_manager_install(pm);
    let mut parts: Vec<String> = Vec::new();
    if !install.is_empty() {
        parts.push(install);
    }
    if project.has_pre_commit {
        parts.push("pip install --user pre-commit && pre-commit install".to_string());
    }
    if project.has_lefthook {
        parts.push("which lefthook >/dev/null && lefthook install || true".to_string());
    }
    // husky usually installs via package.json "prepare" so we trust npm/yarn/pnpm
    // install above to fire it.
    let appended = parts.join(" && ");
    if appended.is_empty() {
        return; // nothing to add; keep Phase 1's grove-prereqs line as-is
    }
    let obj = match devcontainer.as_object_mut() {
        Some(o) => o,
        None => return,
    };
    // Concatenate Phase 1's existing postCreate with project-stack steps.
    // If Phase 1 didn't run (somehow) and the field is empty/missing, the
    // grove prereqs are missing — log so the user can fix manually.
    let existing = obj
        .get("postCreateCommand")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let combined = if existing.trim().is_empty() {
        appended
    } else {
        format!("{} && {}", existing, appended)
    };
    obj.insert("postCreateCommand".to_string(), Value::String(combined));
}

// ---------- inferred-mount scan ----------

fn infer_extra_mount_candidates(
    ctx: &RepoContext,
    project: &ProjectContext,
    user: &str,
) -> Vec<ExtraMount> {
    let mut out: Vec<ExtraMount> = Vec::new();

    let hf_home = format!("/home/{}/.cache/huggingface", user);
    let torch_home = format!("/home/{}/.cache/torch", user);
    let xdg_data_home = format!("/home/{}/.local/share", user);
    let well_known_env_vars: &[(&str, &str, &str)] = &[
        (
            "MARKET_DATA_DIR",
            "/mnt/market_data",
            "trading-data convention",
        ),
        ("DATA_DIR", "/mnt/data", "generic data directory"),
        ("DATASET_PATH", "/mnt/datasets", "generic dataset directory"),
        ("MODEL_DIR", "/mnt/models", "ML model directory"),
        ("CACHE_DIR", "/mnt/cache", "shared cache directory"),
        ("HF_HOME", &hf_home, "HuggingFace cache"),
        ("TORCH_HOME", &torch_home, "PyTorch cache"),
        ("XDG_DATA_HOME", &xdg_data_home, "XDG data home"),
    ];

    // Sweep root files looking for env var references in code / README.
    for envvar in well_known_env_vars {
        let referenced = project.root_files.iter().any(|p| {
            let full = project_root(ctx).join(p);
            // We don't have a working tree, but root_files came from ls-tree HEAD,
            // so re-read each via show_head_file.
            let raw = crate::devcontainer::read_head_or_empty(ctx, p);
            raw.contains(envvar.0)
                || raw.contains(&format!("${}", envvar.0))
                || full.exists()
                    && std::fs::read_to_string(&full)
                        .map(|s| s.contains(envvar.0))
                        .unwrap_or(false)
        });
        if referenced {
            out.push(ExtraMount {
                source: format!("${{localEnv:{}}}", envvar.0),
                target: envvar.1.to_string(),
                mode: "ro".to_string(),
                required: false,
                reason: envvar.2.to_string(),
            });
        }
    }

    // README "place your data at <path>" heuristics — very lightweight.
    for readme in ["README.md", "CONTRIBUTING.md"] {
        let raw = crate::devcontainer::read_head_or_empty(ctx, readme);
        if raw.to_lowercase().contains("place your data") {
            out.push(ExtraMount {
                source: "${localEnv:HOME}/data".to_string(),
                target: "/mnt/data".to_string(),
                mode: "ro".to_string(),
                required: false,
                reason: format!("{} mentions a data path", readme),
            });
            break;
        }
    }

    // Dedup by (source, target).
    let mut seen: HashMap<String, ()> = HashMap::new();
    out.retain(|m| {
        seen.insert(format!("{}|{}", m.source, m.target), ())
            .is_none()
    });
    out
}

// ---------- config I/O ----------

fn read_or_default_config(project_root_path: &Path) -> GroveConfig {
    let path = project_root_path.join(".grove").join("config.toml");
    fs::read_to_string(&path)
        .ok()
        .and_then(|raw| toml::from_str(&raw).ok())
        .unwrap_or_default()
}

fn write_config(project_root_path: &Path, config: &GroveConfig) -> Result<(), String> {
    let dir = project_root_path.join(".grove");
    fs::create_dir_all(&dir).map_err(|e| format!("create .grove/: {}", e))?;
    let path = dir.join("config.toml");
    let body = toml::to_string_pretty(config).map_err(|e| format!("serialize config: {}", e))?;
    fs::write(&path, body).map_err(|e| format!("write {}: {}", path.display(), e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_mount_appends_ro_entry() {
        let mut v = json!({ "mounts": [] });
        add_mount(&mut v, "/host", "/container", "ro");
        let arr = v["mounts"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        let s = arr[0].as_str().unwrap();
        assert!(s.contains("source=/host"));
        assert!(s.contains("target=/container"));
        assert!(s.contains("readonly"));
    }

    #[test]
    fn add_mount_is_idempotent() {
        let mut v = json!({ "mounts": [] });
        add_mount(&mut v, "/host", "/container", "ro");
        add_mount(&mut v, "/host", "/container", "ro");
        assert_eq!(v["mounts"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn set_remote_env_inserts_key() {
        let mut v = json!({ "remoteEnv": {} });
        set_remote_env(&mut v, "FOO", "bar");
        assert_eq!(v["remoteEnv"]["FOO"], "bar");
    }

    #[test]
    fn set_remote_env_creates_map_when_absent() {
        let mut v = json!({});
        set_remote_env(&mut v, "GH_TOKEN", "${localEnv:GH_PAT_FREQTRADE}");
        assert_eq!(v["remoteEnv"]["GH_TOKEN"], "${localEnv:GH_PAT_FREQTRADE}");
    }

    #[test]
    fn set_remote_env_migrates_stale_container_env_key() {
        let mut v =
            json!({ "containerEnv": { "GH_TOKEN": "${localEnv:GH_TOKEN_RO}", "KEEP": "1" } });
        set_remote_env(&mut v, "GH_TOKEN", "${localEnv:GH_PAT_FREQTRADE}");
        assert_eq!(v["remoteEnv"]["GH_TOKEN"], "${localEnv:GH_PAT_FREQTRADE}");
        assert!(v["containerEnv"].get("GH_TOKEN").is_none());
        assert_eq!(v["containerEnv"]["KEEP"], "1");
    }

    #[test]
    fn set_extensions_replaces_list() {
        let mut v = json!({});
        set_extensions(&mut v, &["a".to_string(), "b".to_string()]);
        let arr = v["customizations"]["vscode"]["extensions"]
            .as_array()
            .unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0], "a");
    }

    #[test]
    fn apply_post_create_includes_install_line() {
        // When postCreateCommand is empty (no Phase 1 baseline), apply
        // populates it with just the project-stack install.
        let mut v = json!({});
        let project = ProjectContext {
            stack: Some(ProjectStack::Python),
            package_manager: Some("uv".into()),
            has_pre_commit: false,
            ..Default::default()
        };
        apply_post_create(&project, &mut v);
        assert_eq!(v["postCreateCommand"], "uv sync");
    }

    #[test]
    fn apply_post_create_chains_pre_commit() {
        let mut v = json!({});
        let project = ProjectContext {
            stack: Some(ProjectStack::Python),
            package_manager: Some("uv".into()),
            has_pre_commit: true,
            ..Default::default()
        };
        apply_post_create(&project, &mut v);
        let cmd = v["postCreateCommand"].as_str().unwrap();
        assert!(cmd.contains("uv sync"));
        assert!(cmd.contains("pre-commit install"));
        assert!(cmd.contains("&&"));
    }

    #[test]
    fn apply_post_create_appends_to_phase1_grove_prereqs() {
        // Simulate Phase 1 having written the grove prereqs into postCreate.
        let mut v = json!({
            "postCreateCommand": "(command -v tmux >/dev/null || sudo apt-get install -y tmux)"
        });
        let project = ProjectContext {
            stack: Some(ProjectStack::Python),
            package_manager: Some("uv".into()),
            has_pre_commit: true,
            ..Default::default()
        };
        apply_post_create(&project, &mut v);
        let cmd = v["postCreateCommand"].as_str().unwrap();
        // Grove prereqs are PRESERVED — Phase 2's project-stack steps
        // append, not overwrite.
        assert!(cmd.contains("command -v tmux"));
        // Project-stack steps came in after.
        assert!(cmd.contains("uv sync"));
        assert!(cmd.contains("pre-commit install"));
        // Order: prereqs first, project install second.
        let prereqs_idx = cmd.find("command -v tmux").unwrap();
        let install_idx = cmd.find("uv sync").unwrap();
        assert!(prereqs_idx < install_idx);
    }

    #[test]
    fn apply_post_create_with_no_project_steps_preserves_phase1() {
        // Bare Unknown-stack project with no PM, no hooks. apply_post_create
        // should be a no-op so Phase 1's grove prereqs stay.
        let mut v = json!({
            "postCreateCommand": "GROVE_PREREQS_LINE_HERE"
        });
        let project = ProjectContext {
            stack: Some(ProjectStack::Unknown),
            package_manager: None,
            has_pre_commit: false,
            has_lefthook: false,
            ..Default::default()
        };
        apply_post_create(&project, &mut v);
        assert_eq!(v["postCreateCommand"], "GROVE_PREREQS_LINE_HERE");
    }
}
