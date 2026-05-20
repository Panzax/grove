// Phase 2 of `grove init` — interactive setup wizard.
//
// Five interactive prompts (driven by `dialoguer`) plus two agent-inferred
// scans that ask `claude -p` to enrich the choices:
//
//   1. Project secrets mount        (path, RO/RW, env var name)
//   2. .claude scope + auth         (scoped/full/none + creds strategy)
//   3. GitHub auth                  (RO PAT recommended; replaces full mount)
//   4. Agent-inferred extra mounts  (data dirs, env-var-referenced paths)
//   5. Extensions + container pkgs  (fixed defaults + per-stack inferred)
//
// All decisions are persisted to `.grove/config.toml` AND applied to
// `.devcontainer/devcontainer.json` (mutate-in-place). The wizard is
// idempotent — running `grove init --reconfigure` re-prompts with current
// values as defaults so users can tighten security incrementally.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use colored::Colorize;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, MultiSelect, Select};
use serde_json::{json, Value};

use crate::devcontainer::stack;
use crate::devcontainer::{read_devcontainer_json, write_devcontainer_json};
use crate::git::worktree_manager::{project_root, RepoContext};
use crate::models::{ExtraMount, GroveConfig, ProjectContext, ProjectStack};

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
            "Setup wizard — answering 5 short prompts; you can re-run anytime via `grove init --reconfigure`."
        }
    );

    let mut config = read_or_default_config(&project_root_path);
    let mut devcontainer = read_devcontainer_json(&project_root_path)
        .unwrap_or_else(|_| {
            json!({
                "name": project.repo_name,
                "image": project.default_image,
                "mounts": [],
                "containerEnv": {},
                "customizations": { "vscode": { "extensions": [] } }
            })
        });

    // ----- Prompt 1: project secrets mount -----
    prompt_secrets_mount(project, &mut config, &mut devcontainer)?;

    // ----- Prompt 2: .claude scope + auth -----
    prompt_claude_scope(&mut config, &mut devcontainer)?;

    // ----- Prompt 3: GitHub auth -----
    prompt_github_auth(&mut config, &mut devcontainer)?;

    // ----- Prompt 4: agent-inferred extra mounts -----
    prompt_inferred_mounts(ctx, project, &mut config, &mut devcontainer)?;

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

// ---------- prompts ----------

fn prompt_secrets_mount(
    project: &ProjectContext,
    config: &mut GroveConfig,
    devcontainer: &mut Value,
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

    let target = format!("/home/vscode/.config/{}", project.repo_name);
    add_mount(devcontainer, &path, &target, mode);
    set_container_env(devcontainer, &env_name, &target);

    config.mounts.secrets_path = Some(path);
    config.mounts.secrets_mode = Some(mode.to_string());
    Ok(())
}

fn prompt_claude_scope(config: &mut GroveConfig, devcontainer: &mut Value) -> Result<(), String> {
    let theme = ColorfulTheme::default();
    println!();
    println!(
        "{}",
        "Spawned agents need access to your Claude Code plugins/skills. Choose how:".dimmed()
    );
    let options = vec![
        "Scoped: mount ~/.claude/plugins (RO) + ~/.claude/.credentials.json (RO) — recommended",
        "Full:   mount ~/.claude (RW) — matches the freqtrade default, NOT recommended (exposes session history + lets the container write user settings)",
        "None:   no Claude inheritance (you'll authenticate inside the container per rebuild)",
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
            add_mount(
                devcontainer,
                "${localEnv:HOME}/.claude/plugins",
                "/home/vscode/.claude/plugins",
                "ro",
            );
            add_mount(
                devcontainer,
                "${localEnv:HOME}/.claude/.credentials.json",
                "/home/vscode/.claude/.credentials.json",
                "ro",
            );
        }
        "full" => {
            add_mount(
                devcontainer,
                "${localEnv:HOME}/.claude",
                "/home/vscode/.claude",
                "rw",
            );
        }
        _ => {}
    }
    Ok(())
}

fn prompt_github_auth(config: &mut GroveConfig, devcontainer: &mut Value) -> Result<(), String> {
    let theme = ColorfulTheme::default();
    println!();
    let options = vec![
        "PAT via env var GH_TOKEN (RO fine-grained PAT) — recommended",
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
            set_container_env(devcontainer, "GH_TOKEN", "${localEnv:GH_TOKEN_RO}");
            println!(
                "  {} Set GH_TOKEN_RO on your host shell with a fine-grained PAT (Contents: Read-only).",
                "Tip:".cyan()
            );
        }
        "ro-mount" => {
            add_mount(
                devcontainer,
                "${localEnv:HOME}/.config/gh",
                "/home/vscode/.config/gh",
                "ro",
            );
        }
        "rw-mount" => {
            add_mount(
                devcontainer,
                "${localEnv:HOME}/.config/gh",
                "/home/vscode/.config/gh",
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
) -> Result<(), String> {
    println!();
    let candidates = infer_extra_mount_candidates(ctx, project);
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
    if project.root_files.iter().any(|p| p == "Makefile" || p == "justfile") {
        inferred.push("nefrob.vscode-just-syntax");
    }
    if project
        .root_files
        .iter()
        .any(|p| p.ends_with(".tf") || p.ends_with(".tfvars"))
    {
        inferred.push("hashicorp.terraform");
    }
    if project
        .root_files
        .iter()
        .any(|p| p.ends_with(".proto"))
    {
        inferred.push("zxh404.vscode-proto3");
    }

    let mut all: Vec<&str> = defaults.iter().copied().chain(inferred.iter().copied()).collect();
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
    let mounts = devcontainer
        .as_object_mut()
        .and_then(|o| o.entry("mounts").or_insert_with(|| json!([])).as_array_mut());
    let Some(arr) = mounts else {
        return;
    };
    let entry = if mode == "ro" {
        format!(
            "source={},target={},type=bind,readonly",
            source, target
        )
    } else {
        format!("source={},target={},type=bind", source, target)
    };
    if !arr.iter().any(|v| v == &Value::String(entry.clone())) {
        arr.push(Value::String(entry));
    }
}

fn set_container_env(devcontainer: &mut Value, key: &str, value: &str) {
    let env = devcontainer
        .as_object_mut()
        .and_then(|o| o.entry("containerEnv").or_insert_with(|| json!({})).as_object_mut());
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
    let vscode = custom
        .as_object_mut()
        .and_then(|m| m.entry("vscode").or_insert_with(|| json!({})).as_object_mut());
    if let Some(vs) = vscode {
        vs.insert(
            "extensions".to_string(),
            Value::Array(exts.iter().map(|s| Value::String(s.clone())).collect()),
        );
    }
}

fn apply_post_create(project: &ProjectContext, devcontainer: &mut Value) {
    let stack = project.stack.unwrap_or(ProjectStack::Unknown);
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
    let cmd = parts.join(" && ");
    let obj = devcontainer.as_object_mut();
    if let Some(o) = obj {
        if !cmd.is_empty() {
            o.insert("postCreateCommand".to_string(), Value::String(cmd));
        }
    }
    let _ = stack; // silence unused if neither package_manager_install nor stack used elsewhere
}

// ---------- inferred-mount scan ----------

fn infer_extra_mount_candidates(
    ctx: &RepoContext,
    project: &ProjectContext,
) -> Vec<ExtraMount> {
    let mut out: Vec<ExtraMount> = Vec::new();

    let well_known_env_vars: &[(&str, &str, &str)] = &[
        ("MARKET_DATA_DIR", "/mnt/market_data", "trading-data convention"),
        ("DATA_DIR", "/mnt/data", "generic data directory"),
        ("DATASET_PATH", "/mnt/datasets", "generic dataset directory"),
        ("MODEL_DIR", "/mnt/models", "ML model directory"),
        ("CACHE_DIR", "/mnt/cache", "shared cache directory"),
        ("HF_HOME", "/home/vscode/.cache/huggingface", "HuggingFace cache"),
        ("TORCH_HOME", "/home/vscode/.cache/torch", "PyTorch cache"),
        ("XDG_DATA_HOME", "/home/vscode/.local/share", "XDG data home"),
    ];

    // Sweep root files looking for env var references in code / README.
    for envvar in well_known_env_vars {
        let referenced = project.root_files.iter().any(|p| {
            let full = project_root(ctx).join(p);
            // We don't have a working tree, but root_files came from ls-tree HEAD,
            // so re-read each via show_head_file.
            let raw = crate::devcontainer::read_head_or_empty(ctx, p);
            raw.contains(envvar.0) || raw.contains(&format!("${}", envvar.0))
                || full.exists() && std::fs::read_to_string(&full).map(|s| s.contains(envvar.0)).unwrap_or(false)
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
    out.retain(|m| seen.insert(format!("{}|{}", m.source, m.target), ()).is_none());
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
    let body = toml::to_string_pretty(config)
        .map_err(|e| format!("serialize config: {}", e))?;
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
    fn set_container_env_inserts_key() {
        let mut v = json!({ "containerEnv": {} });
        set_container_env(&mut v, "FOO", "bar");
        assert_eq!(v["containerEnv"]["FOO"], "bar");
    }

    #[test]
    fn set_extensions_replaces_list() {
        let mut v = json!({});
        set_extensions(&mut v, &["a".to_string(), "b".to_string()]);
        let arr = v["customizations"]["vscode"]["extensions"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0], "a");
    }

    #[test]
    fn apply_post_create_includes_install_line() {
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
}
