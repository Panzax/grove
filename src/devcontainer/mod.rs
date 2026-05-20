// Devcontainer scaffolding for `grove init` Phase 1 (deterministic).
//
// This module reads files from the bare clone via `worktree_manager::show_head_file`
// (no working tree needed), classifies the project's primary stack, and writes a
// minimal-but-valid `.devcontainer/devcontainer.json` skeleton. Phase 2 (the setup
// agent in `crate::agent::setup`) refines the skeleton with mounts, extensions, and
// per-stack tooling.

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
    let primary = stacks_detected.first().copied().unwrap_or(ProjectStack::Unknown);

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
    let has_lefthook = head_file_exists(ctx, "lefthook.yml") || head_file_exists(ctx, "lefthook.yaml");
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
pub fn build_devcontainer_skeleton(project: &ProjectContext) -> Value {
    let mut root = json!({
        "name": project.repo_name,
        "image": project.default_image,
        "remoteUser": "vscode",
        "containerUser": "vscode",
        "updateRemoteUserUID": true,
        "postCreateCommand": "",
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

/// Read whatever devcontainer.json is currently on disk, returning its parsed JSON.
/// Used by Phase 2 to mutate-in-place after the user confirms wizard choices.
pub fn read_devcontainer_json(project_root_path: &Path) -> Result<Value, String> {
    let path = project_root_path.join(".devcontainer").join("devcontainer.json");
    let raw = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    serde_json::from_str(&raw)
        .map_err(|e| format!("Failed to parse {}: {}", path.display(), e))
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
}
