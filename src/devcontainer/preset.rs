// Curated agentic environment presets.
//
// Each preset is a complete, ready-to-use container environment: a base image,
// the non-root user that image actually ships, the devcontainer features that
// install the agentic toolchain (Node + git + GitHub CLI + the official
// Anthropic claude-code feature), and stack-appropriate VS Code extensions.
//
// Presets replace the old per-stack `default_image` + hand-rolled
// `grove_container_prereqs_command` approach: tooling now comes from maintained
// devcontainer features instead of bespoke apt/npm shell, and the container
// user is a property of the chosen image rather than a hardcoded "vscode".
// Different images legitimately ship different users (`vscode`, `node`,
// `codespace`, ...), which is exactly why the user must travel with the preset
// rather than being assumed at call sites.

use serde_json::{json, Map, Value};

use crate::models::ProjectStack;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetId {
    General,
    WebTs,
    Rust,
    Python,
    Custom,
}

pub struct EnvironmentPreset {
    pub id: PresetId,
    pub label: &'static str,
    /// Base image. Empty for `Custom` (keep whatever the project already has).
    pub image: &'static str,
    /// The non-root user the base image ships. Single source of truth for the
    /// container user — never hardcoded at call sites.
    pub remote_user: &'static str,
    /// `(feature ref, JSON options literal)` pairs merged into devcontainer.json.
    pub features: &'static [(&'static str, &'static str)],
    pub extensions: &'static [&'static str],
}

// Maintained devcontainer features — the standard, composable way to provision
// the agentic toolchain (replaces the bespoke apt/npm postCreate shell).
const F_NODE: (&str, &str) = ("ghcr.io/devcontainers/features/node:1", "{}");
// git `ppa:true` keeps git >= 2.46 so grove's relative worktree pointers parse
// inside the container.
const F_GIT: (&str, &str) = (
    "ghcr.io/devcontainers/features/git:1",
    r#"{"ppa": true, "version": "latest"}"#,
);
const F_GH: (&str, &str) = ("ghcr.io/devcontainers/features/github-cli:1", "{}");
// Official Anthropic feature: installs the Claude Code CLI + VS Code extension.
// Requires Node, which is why non-Node base images pair it with F_NODE.
const F_CLAUDE: (&str, &str) = (
    "ghcr.io/anthropics/devcontainer-features/claude-code:1",
    "{}",
);

const GENERAL: EnvironmentPreset = EnvironmentPreset {
    id: PresetId::General,
    label: "general — universal multi-language agentic baseline (default)",
    image: "mcr.microsoft.com/devcontainers/universal:noble",
    remote_user: "codespace",
    // The universal image already bundles Node, so F_NODE is omitted.
    features: &[F_GIT, F_GH, F_CLAUDE],
    extensions: &[],
};

const WEB_TS: EnvironmentPreset = EnvironmentPreset {
    id: PresetId::WebTs,
    label: "web / react / typescript",
    image: "mcr.microsoft.com/devcontainers/typescript-node:22",
    remote_user: "node",
    // Node ships in the typescript-node image.
    features: &[F_GIT, F_GH, F_CLAUDE],
    extensions: &["dbaeumer.vscode-eslint", "esbenp.prettier-vscode"],
};

const RUST: EnvironmentPreset = EnvironmentPreset {
    id: PresetId::Rust,
    label: "rust",
    image: "mcr.microsoft.com/devcontainers/rust:latest",
    remote_user: "vscode",
    features: &[F_NODE, F_GIT, F_GH, F_CLAUDE],
    extensions: &["rust-lang.rust-analyzer", "vadimcn.vscode-lldb"],
};

const PYTHON: EnvironmentPreset = EnvironmentPreset {
    id: PresetId::Python,
    label: "python",
    image: "mcr.microsoft.com/devcontainers/python:3.12",
    remote_user: "vscode",
    features: &[F_NODE, F_GIT, F_GH, F_CLAUDE],
    extensions: &["ms-python.python", "ms-python.vscode-pylance"],
};

const CUSTOM: EnvironmentPreset = EnvironmentPreset {
    id: PresetId::Custom,
    label: "custom — keep my existing image / devcontainer.json",
    image: "",
    remote_user: "vscode",
    features: &[],
    extensions: &[],
};

/// All presets, in the order the wizard offers them.
pub fn all() -> [&'static EnvironmentPreset; 5] {
    [&GENERAL, &WEB_TS, &RUST, &PYTHON, &CUSTOM]
}

#[allow(dead_code)] // used by tests now; sandbox preset resolution (Part B) next
pub fn by_id(id: PresetId) -> &'static EnvironmentPreset {
    match id {
        PresetId::General => &GENERAL,
        PresetId::WebTs => &WEB_TS,
        PresetId::Rust => &RUST,
        PresetId::Python => &PYTHON,
        PresetId::Custom => &CUSTOM,
    }
}

/// Pre-select a preset from the detected stack. Rust→rust, Python→python,
/// Node→web-ts, everything else→general.
pub fn for_stack(stack: ProjectStack) -> &'static EnvironmentPreset {
    match stack {
        ProjectStack::Rust => &RUST,
        ProjectStack::Python => &PYTHON,
        ProjectStack::Node => &WEB_TS,
        _ => &GENERAL,
    }
}

/// Build the devcontainer.json `features` object for a preset.
pub fn features_object(preset: &EnvironmentPreset) -> Value {
    let mut map = Map::new();
    for (reference, opts) in preset.features {
        let opts_val: Value = serde_json::from_str(opts).unwrap_or_else(|_| json!({}));
        map.insert((*reference).to_string(), opts_val);
    }
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_maps_to_expected_preset() {
        assert_eq!(for_stack(ProjectStack::Rust).id, PresetId::Rust);
        assert_eq!(for_stack(ProjectStack::Python).id, PresetId::Python);
        assert_eq!(for_stack(ProjectStack::Node).id, PresetId::WebTs);
        assert_eq!(for_stack(ProjectStack::Go).id, PresetId::General);
        assert_eq!(for_stack(ProjectStack::Unknown).id, PresetId::General);
    }

    #[test]
    fn every_non_custom_preset_includes_claude_feature() {
        for p in all() {
            if p.id == PresetId::Custom {
                continue;
            }
            let feats = features_object(p);
            assert!(
                feats
                    .get("ghcr.io/anthropics/devcontainer-features/claude-code:1")
                    .is_some(),
                "preset {:?} missing claude-code feature",
                p.id
            );
        }
    }

    #[test]
    fn presets_declare_distinct_users_not_just_vscode() {
        // Guards against regressing to a single hardcoded user: the universal
        // and typescript-node images do not use `vscode`.
        assert_eq!(by_id(PresetId::General).remote_user, "codespace");
        assert_eq!(by_id(PresetId::WebTs).remote_user, "node");
    }

    #[test]
    fn git_feature_pins_ppa_for_relative_worktrees() {
        let feats = features_object(by_id(PresetId::Rust));
        let git = &feats["ghcr.io/devcontainers/features/git:1"];
        assert_eq!(git["ppa"], true);
        assert_eq!(git["version"], "latest");
    }
}
