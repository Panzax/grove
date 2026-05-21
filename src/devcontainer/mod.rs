// Devcontainer scaffolding for `grove init` Phase 1 (deterministic).
//
// This module reads files from the bare clone via `worktree_manager::show_head_file`
// (no working tree needed), classifies the project's primary stack, and writes a
// minimal-but-valid `.devcontainer/devcontainer.json` skeleton. Phase 2 (the setup
// agent in `crate::agent::setup`) refines the skeleton with mounts, extensions, and
// per-stack tooling.

pub mod ci_scrape;
pub mod stack;

use std::fs;
use std::path::Path;

use serde_json::{json, Value};

use crate::git::worktree_manager::{
    head_file_exists, ls_head_files, project_root, show_head_file, RepoContext,
};
use crate::models::{ProjectContext, ProjectStack};

/// Top-level scan: list HEAD files once, then run every probe against the in-memory list.
/// Cheap and consistent (one git invocation per detection pass, not one per probe).
pub fn detect_project_context(ctx: &RepoContext, repo_name: &str) -> ProjectContext {
    let head_files: Vec<String> = ls_head_files(ctx).unwrap_or_default();

    let stacks_detected = stack::detect_all_stacks(&head_files);
    let primary = stacks_detected
        .first()
        .copied()
        .unwrap_or(ProjectStack::Unknown);

    let toolchain_version = stack::infer_toolchain_version(ctx, primary, &head_files);
    let package_manager = stack::infer_package_manager(primary, &head_files);

    let has_tests = head_files.iter().any(|p| {
        p.starts_with("tests/")
            || p.starts_with("test/")
            || p.starts_with("__tests__/")
            || p.ends_with("_test.go")
            || p.ends_with(".test.ts")
            || p.ends_with(".test.tsx")
            || p.ends_with(".test.js")
            || p.ends_with(".test.jsx")
            || p.ends_with(".spec.ts")
            || p.ends_with(".spec.js")
            || p == "pytest.ini"
            || p == "jest.config.js"
            || p == "jest.config.ts"
    });
    let has_dockerfile = head_files.iter().any(|p| {
        p == "Dockerfile"
            || p.starts_with("docker-compose")
            || p == "compose.yml"
            || p == "compose.yaml"
            || p.starts_with("Dockerfile.")
    });
    let has_pre_commit = head_file_exists(ctx, ".pre-commit-config.yaml")
        || head_file_exists(ctx, ".pre-commit-config.yml");
    let has_husky = head_files.iter().any(|p| p.starts_with(".husky/"));
    let has_lefthook =
        head_file_exists(ctx, "lefthook.yml") || head_file_exists(ctx, "lefthook.yaml");
    let has_claude_md = head_file_exists(ctx, "CLAUDE.md")
        || head_file_exists(ctx, "docs/CLAUDE.md")
        || head_file_exists(ctx, ".claude/CLAUDE.md");

    // Default branch detection lives in worktree_manager; we don't fail init if it's missing.
    let default_branch = crate::git::worktree_manager::get_default_branch(ctx).ok();

    ProjectContext {
        stack: Some(primary),
        stacks_detected,
        root_files: head_files
            .iter()
            .filter(|p| !p.contains('/'))
            .cloned()
            .collect(),
        default_image: primary.default_image().to_string(),
        has_tests,
        has_dockerfile,
        has_pre_commit,
        has_husky,
        has_lefthook,
        has_claude_md,
        package_manager,
        toolchain_version,
        default_branch,
        repo_name: repo_name.to_string(),
    }
}

/// Write a minimal valid devcontainer.json at `<project_root>/.devcontainer/devcontainer.json`.
///
/// Refuses to overwrite an existing devcontainer.json (so re-running `grove init` on
/// an established project doesn't clobber user customizations). Returns Ok(true) if a
/// file was written, Ok(false) if a file was already present (no-op).
pub fn scaffold_devcontainer(ctx: &RepoContext, project: &ProjectContext) -> Result<bool, String> {
    let devcontainer_dir = project_root(ctx).join(".devcontainer");
    let devcontainer_file = devcontainer_dir.join("devcontainer.json");

    if devcontainer_file.exists() {
        return Ok(false);
    }

    fs::create_dir_all(&devcontainer_dir)
        .map_err(|e| format!("Failed to create .devcontainer/: {}", e))?;

    let skeleton = build_devcontainer_skeleton(project);
    let body = serde_json::to_string_pretty(&skeleton)
        .map_err(|e| format!("Failed to serialize devcontainer.json: {}", e))?;
    fs::write(&devcontainer_file, body)
        .map_err(|e| format!("Failed to write devcontainer.json: {}", e))?;
    Ok(true)
}

/// Build the JSON skeleton (deterministic Phase 1). Phase 2 mutates this structure to
/// add mounts, extensions, and language-specific features.
///
/// `workspaceFolder` is set to `/workspaces/<repo_name>` so the container-side
/// path is stable and matches the default of the Microsoft devcontainers base
/// images. `container::host_to_container_path` derives container paths from
/// this, so the skeleton stays the source of truth.
///
/// `postCreateCommand` is seeded with grove's container prereqs (tmux, jq,
/// perl, claude). Phase 2's `apply_post_create` APPENDS project-stack installs
/// (uv sync, npm ci, pre-commit install) so even `grove init --no-agent`
/// users get a usable container.
pub fn build_devcontainer_skeleton(project: &ProjectContext) -> Value {
    let workspace_folder = format!("/workspaces/{}", project.repo_name);
    let mut root = json!({
        "name": project.repo_name,
        "image": project.default_image,
        "remoteUser": "vscode",
        "containerUser": "vscode",
        "updateRemoteUserUID": true,
        "workspaceFolder": workspace_folder,
        "postCreateCommand": grove_container_prereqs_command(),
        "containerEnv": {},
        "mounts": [],
        "customizations": {
            "vscode": {
                "extensions": []
            }
        }
    });

    // Per-stack default extensions (Phase 1 — wizard will overwrite/refine).
    let stack = project.stack.unwrap_or(ProjectStack::Unknown);
    let exts = stack::default_extensions(stack);
    if let Some(obj) = root
        .get_mut("customizations")
        .and_then(|c| c.get_mut("vscode"))
        .and_then(|v| v.get_mut("extensions"))
    {
        *obj = json!(exts);
    }

    root
}

/// Idempotent install line for grove's container prereqs. Runs as part of
/// `postCreateCommand`. Each tool is gated on `command -v <tool>` so
/// images that already include the tool (or come with newer alternatives)
/// don't get clobbered.
///
/// Uses `apt-get` because all Microsoft devcontainers base images we
/// scaffold against (ubuntu, python:3.12, rust, javascript-node, go,
/// dotnet:8.0) are Debian-based. Users on Alpine or custom images can
/// edit `.devcontainer/devcontainer.json` after `grove init`; we document
/// this in README.
///
/// `sudo` is included because the postCreateCommand sometimes runs as the
/// remoteUser (vscode), which lacks root by default. devcontainer base
/// images include passwordless sudo for the default user.
pub fn grove_container_prereqs_command() -> String {
    [
        // Apt step bundled so we don't repeat update for each tool.
        r#"(command -v tmux >/dev/null && command -v jq >/dev/null && command -v perl >/dev/null) || sudo apt-get update"#,
        r#"(command -v tmux >/dev/null || sudo apt-get install -y tmux)"#,
        r#"(command -v jq   >/dev/null || sudo apt-get install -y jq)"#,
        r#"(command -v perl >/dev/null || sudo apt-get install -y perl)"#,
        // Claude Code via official npm install. Skipped if claude is already
        // on PATH (user can pre-bake it into a custom image).
        r#"(command -v claude >/dev/null || sudo npm install -g @anthropic-ai/claude-code)"#,
    ]
    .join(" && ")
}

/// Read whatever devcontainer.json is currently on disk, returning its parsed JSON.
/// Used by Phase 2 to mutate-in-place after the user confirms wizard choices.
pub fn read_devcontainer_json(project_root_path: &Path) -> Result<Value, String> {
    let path = project_root_path
        .join(".devcontainer")
        .join("devcontainer.json");
    let raw = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    serde_json::from_str(&raw).map_err(|e| format!("Failed to parse {}: {}", path.display(), e))
}

/// Write a devcontainer.json (overwrites). Used by Phase 2.
pub fn write_devcontainer_json(project_root_path: &Path, value: &Value) -> Result<(), String> {
    let dir = project_root_path.join(".devcontainer");
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create .devcontainer/: {}", e))?;
    let path = dir.join("devcontainer.json");
    let body = serde_json::to_string_pretty(value)
        .map_err(|e| format!("Failed to serialize devcontainer.json: {}", e))?;
    fs::write(&path, body).map_err(|e| format!("Failed to write devcontainer.json: {}", e))?;
    Ok(())
}

/// Read a manifest file from HEAD; returns Ok("") if the file isn't tracked (so callers
/// can do simple substring probes without juggling Result).
pub fn read_head_or_empty(ctx: &RepoContext, file: &str) -> String {
    show_head_file(ctx, file).unwrap_or_default()
}

/// Probe the host filesystem for a tmux config file. Returns the
/// `${localEnv:HOME}`-relative path so the bind mount survives
/// `grove init`'s host-side write into devcontainer.json. We don't
/// inline the absolute host path because devcontainer.json gets
/// committed; users on different machines should still pick up their
/// own conf at devcontainer-up time.
///
/// Lookup order matches tmux's own search order:
///   1. `$HOME/.config/tmux/tmux.conf` (XDG)
///   2. `$HOME/.tmux.conf`             (legacy)
///
/// `$TMUX_CONF` isn't honored: it points at an absolute path, which
/// defeats the localEnv:HOME pattern; users with non-standard locations
/// can edit devcontainer.json by hand.
///
/// Returns the `${localEnv:HOME}/...` source string, or None if no
/// host config is present (init prints a skip note).
pub fn detect_host_tmux_conf() -> Option<&'static str> {
    let home = std::env::var("HOME").ok().filter(|s| !s.is_empty())?;
    detect_host_tmux_conf_in(Path::new(&home))
}

/// Inner helper that takes the home directory as a parameter instead of
/// reading the env. Tests use this directly so they don't have to mutate
/// the global `HOME` env var (which races under parallel `cargo test`).
fn detect_host_tmux_conf_in(home: &Path) -> Option<&'static str> {
    let candidates: [(&str, &str); 2] = [
        (
            ".config/tmux/tmux.conf",
            "${localEnv:HOME}/.config/tmux/tmux.conf",
        ),
        (".tmux.conf", "${localEnv:HOME}/.tmux.conf"),
    ];
    for (suffix, mount_source) in candidates {
        if home.join(suffix).is_file() {
            return Some(mount_source);
        }
    }
    None
}

/// Append a RO bind mount for the host's tmux config to the project's
/// devcontainer.json `mounts` array, so the in-container tmux inherits
/// the host user's keybinds, theme, etc.
///
/// Container target is always `/home/vscode/.tmux.conf` (legacy path);
/// tmux reads both legacy and XDG locations but the legacy form is
/// honored by every tmux version we ship against.
///
/// Idempotent: skips if a mount already targets the legacy path.
/// Returns Ok(true) if a mount was added, Ok(false) if no host conf was
/// detected or the mount was already present.
pub fn apply_baseline_tmux_mount(project_root: &Path) -> Result<bool, String> {
    apply_baseline_tmux_mount_with(project_root, detect_host_tmux_conf())
}

/// Inner helper that accepts the pre-detected mount source. Tests call
/// this directly so they don't have to set `HOME` (parallel-test race).
fn apply_baseline_tmux_mount_with(
    project_root: &Path,
    mount_source: Option<&str>,
) -> Result<bool, String> {
    let Some(mount_source) = mount_source else {
        return Ok(false);
    };
    let dev_path = project_root.join(".devcontainer").join("devcontainer.json");
    if !dev_path.exists() {
        return Ok(false);
    }
    let raw =
        fs::read_to_string(&dev_path).map_err(|e| format!("read {}: {}", dev_path.display(), e))?;
    let mut value: Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {}", dev_path.display(), e))?;
    let obj = value
        .as_object_mut()
        .ok_or_else(|| "devcontainer.json top-level is not a JSON object".to_string())?;
    let mounts = obj
        .entry("mounts")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| "devcontainer.json `mounts` is not an array".to_string())?;
    let target = "/home/vscode/.tmux.conf";
    if mounts
        .iter()
        .filter_map(|v| v.as_str())
        .any(|s| s.contains(&format!("target={}", target)))
    {
        return Ok(false);
    }
    let entry = format!(
        "source={},target={},type=bind,readonly",
        mount_source, target
    );
    mounts.push(Value::String(entry));
    let body = serde_json::to_string_pretty(&value)
        .map_err(|e| format!("serialize {}: {}", dev_path.display(), e))?;
    fs::write(&dev_path, body).map_err(|e| format!("write {}: {}", dev_path.display(), e))?;
    Ok(true)
}

/// Extract `(workspaceFolder, remoteUser)` from a parsed devcontainer.json.
/// Used by `grove init` to populate `.grove/config.toml [devcontainer]
/// workspace_target` + `remote_user` so the `container` module can translate
/// host paths without re-parsing the JSON every call.
///
/// Returns None for either field if it isn't in the JSON. Callers fall back
/// to the conventional defaults (`/workspaces/<basename>`, `vscode`).
pub fn extract_workspace_metadata(value: &Value) -> (Option<String>, Option<String>) {
    let workspace_folder = value
        .get("workspaceFolder")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let remote_user = value
        .get("remoteUser")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    (workspace_folder, remote_user)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_carries_repo_name_and_image() {
        let project = ProjectContext {
            stack: Some(ProjectStack::Rust),
            default_image: ProjectStack::Rust.default_image().to_string(),
            repo_name: "demo".to_string(),
            ..Default::default()
        };
        let skel = build_devcontainer_skeleton(&project);
        assert_eq!(skel["name"], "demo");
        assert_eq!(skel["image"], ProjectStack::Rust.default_image());
        assert_eq!(skel["remoteUser"], "vscode");
        // rust default ext present
        let exts = skel["customizations"]["vscode"]["extensions"]
            .as_array()
            .unwrap();
        let ids: Vec<&str> = exts.iter().filter_map(|v| v.as_str()).collect();
        assert!(ids.iter().any(|s| *s == "rust-lang.rust-analyzer"));
    }

    #[test]
    fn skeleton_seeds_grove_prereqs_in_post_create() {
        let project = ProjectContext {
            stack: Some(ProjectStack::Rust),
            default_image: ProjectStack::Rust.default_image().to_string(),
            repo_name: "demo".to_string(),
            ..Default::default()
        };
        let skel = build_devcontainer_skeleton(&project);
        let post = skel["postCreateCommand"].as_str().unwrap();
        // Each prereq is gated on `command -v` first; idempotent on images
        // that already have them.
        assert!(post.contains("command -v tmux"));
        assert!(post.contains("command -v jq"));
        assert!(post.contains("command -v perl"));
        assert!(post.contains("command -v claude"));
        // Claude install path is npm (most portable). Devs can override.
        assert!(post.contains("@anthropic-ai/claude-code"));
    }

    #[test]
    fn skeleton_unknown_stack_still_valid() {
        let project = ProjectContext {
            stack: Some(ProjectStack::Unknown),
            default_image: ProjectStack::Unknown.default_image().to_string(),
            repo_name: "demo".to_string(),
            ..Default::default()
        };
        let skel = build_devcontainer_skeleton(&project);
        assert_eq!(skel["image"], ProjectStack::Unknown.default_image());
    }

    fn tmp_dir(label: &str) -> std::path::PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("grove-tmux-mount-{}-{}-{}", label, pid, nanos));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_devcontainer(project_root: &std::path::Path) {
        let dc_dir = project_root.join(".devcontainer");
        fs::create_dir_all(&dc_dir).unwrap();
        fs::write(
            dc_dir.join("devcontainer.json"),
            r#"{"name":"x","image":"y","mounts":[]}"#,
        )
        .unwrap();
    }

    #[test]
    fn detect_returns_xdg_when_present() {
        let home = tmp_dir("xdg");
        fs::create_dir_all(home.join(".config/tmux")).unwrap();
        fs::write(home.join(".config/tmux/tmux.conf"), "set -g status off").unwrap();
        let got = detect_host_tmux_conf_in(&home);
        assert_eq!(got, Some("${localEnv:HOME}/.config/tmux/tmux.conf"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn detect_falls_back_to_legacy() {
        let home = tmp_dir("legacy");
        fs::write(home.join(".tmux.conf"), "set -g status off").unwrap();
        let got = detect_host_tmux_conf_in(&home);
        assert_eq!(got, Some("${localEnv:HOME}/.tmux.conf"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn detect_xdg_wins_over_legacy() {
        let home = tmp_dir("both");
        fs::create_dir_all(home.join(".config/tmux")).unwrap();
        fs::write(home.join(".config/tmux/tmux.conf"), "xdg").unwrap();
        fs::write(home.join(".tmux.conf"), "legacy").unwrap();
        let got = detect_host_tmux_conf_in(&home);
        assert_eq!(got, Some("${localEnv:HOME}/.config/tmux/tmux.conf"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn detect_returns_none_when_no_conf() {
        let home = tmp_dir("empty");
        let got = detect_host_tmux_conf_in(&home);
        assert_eq!(got, None);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn apply_baseline_tmux_mount_writes_entry() {
        let project_root = tmp_dir("apply-proj");
        write_devcontainer(&project_root);
        let added =
            apply_baseline_tmux_mount_with(&project_root, Some("${localEnv:HOME}/.tmux.conf"))
                .unwrap();
        assert!(added);
        let body =
            fs::read_to_string(project_root.join(".devcontainer/devcontainer.json")).unwrap();
        assert!(body.contains("${localEnv:HOME}/.tmux.conf"));
        assert!(body.contains("target=/home/vscode/.tmux.conf"));
        assert!(body.contains("readonly"));
        let _ = fs::remove_dir_all(&project_root);
    }

    #[test]
    fn apply_baseline_tmux_mount_idempotent() {
        let project_root = tmp_dir("idem-proj");
        write_devcontainer(&project_root);
        let first =
            apply_baseline_tmux_mount_with(&project_root, Some("${localEnv:HOME}/.tmux.conf"))
                .unwrap();
        let second =
            apply_baseline_tmux_mount_with(&project_root, Some("${localEnv:HOME}/.tmux.conf"))
                .unwrap();
        assert!(first);
        assert!(!second, "second call should be a no-op");
        let body =
            fs::read_to_string(project_root.join(".devcontainer/devcontainer.json")).unwrap();
        let count = body.matches("target=/home/vscode/.tmux.conf").count();
        assert_eq!(count, 1, "mount must appear exactly once");
        let _ = fs::remove_dir_all(&project_root);
    }

    #[test]
    fn apply_baseline_tmux_mount_skips_without_host_conf() {
        let project_root = tmp_dir("none-proj");
        write_devcontainer(&project_root);
        let added = apply_baseline_tmux_mount_with(&project_root, None).unwrap();
        assert!(!added);
        let body =
            fs::read_to_string(project_root.join(".devcontainer/devcontainer.json")).unwrap();
        assert!(!body.contains("tmux.conf"));
        let _ = fs::remove_dir_all(&project_root);
    }
}
