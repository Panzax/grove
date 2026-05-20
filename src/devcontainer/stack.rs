// Stack detection + per-stack defaults.
//
// Pure functions over a list of HEAD file paths so they're cheap to unit-test
// without a real bare clone.

use std::collections::BTreeSet;

use crate::git::worktree_manager::{show_head_file, RepoContext};
use crate::models::ProjectStack;

/// Walk a sorted, deduplicated set of detected stacks. Order is significance: the
/// first stack is the "primary" used for default image / postCreate. We rank by a
/// rough heuristic — anything with a Cargo.toml or pyproject.toml is more likely the
/// project's primary stack than a Node helper folder.
pub fn detect_all_stacks(head_files: &[String]) -> Vec<ProjectStack> {
    let mut out: BTreeSet<ProjectStack> = BTreeSet::new();

    for path in head_files {
        // Only consider repo-root manifest hits, not nested ones, so a monorepo with
        // a top-level Cargo.toml + a docs/package.json doesn't get classified as Node.
        if path.contains('/') {
            continue;
        }
        match path.as_str() {
            "pyproject.toml" | "setup.py" | "setup.cfg" | "requirements.txt"
            | "uv.lock" | "poetry.lock" | "pdm.lock" | "Pipfile" | "Pipfile.lock" => {
                out.insert(ProjectStack::Python);
            }
            "Cargo.toml" | "Cargo.lock" => {
                out.insert(ProjectStack::Rust);
            }
            "package.json" | "package-lock.json" | "pnpm-lock.yaml"
            | "yarn.lock" | "bun.lock" | "tsconfig.json" => {
                out.insert(ProjectStack::Node);
            }
            "go.mod" | "go.sum" => {
                out.insert(ProjectStack::Go);
            }
            _ => {
                // .csproj / .sln are typically lowercase-or-mixed; match by suffix.
                let lower = path.to_ascii_lowercase();
                if lower.ends_with(".csproj") || lower.ends_with(".sln") || lower == "global.json" {
                    out.insert(ProjectStack::DotNet);
                }
            }
        }
    }

    // Rank for primary stack: Rust > Python > Go > .NET > Node (Rust+Python rarely
    // appear as helper-only; Node manifests commonly appear next to non-Node code
    // like docs sites or build tooling).
    let mut ordered: Vec<ProjectStack> = out.into_iter().collect();
    ordered.sort_by_key(|s| match s {
        ProjectStack::Rust => 0,
        ProjectStack::Python => 1,
        ProjectStack::Go => 2,
        ProjectStack::DotNet => 3,
        ProjectStack::Node => 4,
        ProjectStack::Unknown => 5,
    });
    ordered
}

/// Infer a coarse toolchain identifier (e.g. "python-3.11", "node-22"). Returns None
/// when no pin is present — Phase 2 setup agent can ask.
pub fn infer_toolchain_version(
    ctx: &RepoContext,
    stack: ProjectStack,
    head_files: &[String],
) -> Option<String> {
    match stack {
        ProjectStack::Python => {
            if head_files.iter().any(|p| p == ".python-version") {
                let raw = show_head_file(ctx, ".python-version").ok()?;
                return Some(format!("python-{}", raw.trim()));
            }
            // requires-python in pyproject.toml as a fallback
            if head_files.iter().any(|p| p == "pyproject.toml") {
                let raw = show_head_file(ctx, "pyproject.toml").unwrap_or_default();
                for line in raw.lines() {
                    let t = line.trim();
                    if let Some(rest) = t.strip_prefix("requires-python") {
                        // requires-python = ">=3.11"
                        let cleaned: String = rest
                            .chars()
                            .filter(|c| c.is_ascii_digit() || *c == '.')
                            .collect();
                        if !cleaned.is_empty() {
                            return Some(format!("python-{}", cleaned));
                        }
                    }
                }
            }
            None
        }
        ProjectStack::Rust => {
            if head_files.iter().any(|p| p == "rust-toolchain.toml") {
                let raw = show_head_file(ctx, "rust-toolchain.toml").ok()?;
                for line in raw.lines() {
                    let t = line.trim();
                    if let Some(rest) = t.strip_prefix("channel") {
                        let v: String = rest
                            .chars()
                            .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-')
                            .collect();
                        if !v.is_empty() {
                            return Some(format!("rust-{}", v));
                        }
                    }
                }
            }
            None
        }
        ProjectStack::Node => {
            if head_files.iter().any(|p| p == ".nvmrc") {
                let raw = show_head_file(ctx, ".nvmrc").ok()?;
                return Some(format!("node-{}", raw.trim().trim_start_matches('v')));
            }
            None
        }
        ProjectStack::Go => {
            let raw = show_head_file(ctx, "go.mod").ok()?;
            for line in raw.lines() {
                let t = line.trim();
                if let Some(rest) = t.strip_prefix("go ") {
                    return Some(format!("go-{}", rest.trim()));
                }
            }
            None
        }
        ProjectStack::DotNet => {
            if head_files.iter().any(|p| p == "global.json") {
                return Some("dotnet-global.json".to_string());
            }
            None
        }
        ProjectStack::Unknown => None,
    }
}

/// Pick a package manager based on which lockfile is present.
pub fn infer_package_manager(stack: ProjectStack, head_files: &[String]) -> Option<String> {
    let has = |name: &str| head_files.iter().any(|p| p == name);
    match stack {
        ProjectStack::Python => {
            if has("uv.lock") {
                Some("uv".into())
            } else if has("poetry.lock") {
                Some("poetry".into())
            } else if has("pdm.lock") {
                Some("pdm".into())
            } else if has("Pipfile.lock") {
                Some("pipenv".into())
            } else if has("requirements.txt") {
                Some("pip".into())
            } else if has("pyproject.toml") {
                Some("pip".into())
            } else {
                None
            }
        }
        ProjectStack::Rust => has("Cargo.lock").then(|| "cargo".to_string()),
        ProjectStack::Node => {
            if has("pnpm-lock.yaml") {
                Some("pnpm".into())
            } else if has("yarn.lock") {
                Some("yarn".into())
            } else if has("bun.lock") {
                Some("bun".into())
            } else if has("package-lock.json") {
                Some("npm".into())
            } else if has("package.json") {
                Some("npm".into())
            } else {
                None
            }
        }
        ProjectStack::Go => has("go.sum").then(|| "go-mod".to_string()),
        ProjectStack::DotNet => Some("dotnet".into()),
        ProjectStack::Unknown => None,
    }
}

/// VS Code extensions to include by default for the given primary stack.
pub fn default_extensions(stack: ProjectStack) -> Vec<&'static str> {
    let mut base = vec![
        "anthropic.claude-code",
        "eamodio.gitlens",
        "GitHub.vscode-pull-request-github",
        "EditorConfig.EditorConfig",
    ];
    let stack_exts: &[&str] = match stack {
        ProjectStack::Python => &[
            "ms-python.python",
            "ms-python.vscode-pylance",
            "charliermarsh.ruff",
        ],
        ProjectStack::Rust => &[
            "rust-lang.rust-analyzer",
            "tamasfe.even-better-toml",
            "vadimcn.vscode-lldb",
        ],
        ProjectStack::Node => &[
            "dbaeumer.vscode-eslint",
            "esbenp.prettier-vscode",
        ],
        ProjectStack::Go => &["golang.go"],
        ProjectStack::DotNet => &["ms-dotnettools.csdevkit", "ms-dotnettools.csharp"],
        ProjectStack::Unknown => &[],
    };
    base.extend_from_slice(stack_exts);
    base
}

/// Post-create install line for the detected package manager. Returns an empty string
/// if there's nothing to install.
pub fn package_manager_install(pm: Option<&str>) -> String {
    match pm {
        Some("uv") => "uv sync".to_string(),
        Some("poetry") => "poetry install --no-interaction --no-ansi".to_string(),
        Some("pdm") => "pdm install".to_string(),
        Some("pipenv") => "pipenv install --dev".to_string(),
        Some("pip") => "pip install -r requirements.txt || true".to_string(),
        Some("pnpm") => "corepack enable && pnpm install --frozen-lockfile".to_string(),
        Some("yarn") => "corepack enable && yarn install --immutable".to_string(),
        Some("bun") => "bun install --frozen-lockfile".to_string(),
        Some("npm") => "npm ci".to_string(),
        Some("cargo") => "cargo fetch".to_string(),
        Some("go-mod") => "go mod download".to_string(),
        Some("dotnet") => "dotnet restore".to_string(),
        _ => String::new(),
    }
}

/// Default verify commands per stack. Used when CI-parity scrape returns nothing.
/// Each inner Vec is `[program, arg, arg, ...]`.
pub fn verify_defaults(
    stack: ProjectStack,
    pm: Option<&str>,
) -> VerifyDefaults {
    match stack {
        ProjectStack::Python => VerifyDefaults {
            test: vec!["pytest".into(), "-q".into()],
            lint: vec!["ruff".into(), "check".into(), ".".into()],
            format: vec!["ruff".into(), "format".into(), "--check".into(), ".".into()],
            typecheck: vec!["mypy".into(), ".".into()],
        },
        ProjectStack::Rust => VerifyDefaults {
            test: vec!["cargo".into(), "test".into(), "--all".into()],
            lint: vec![
                "cargo".into(),
                "clippy".into(),
                "--all".into(),
                "--".into(),
                "-D".into(),
                "warnings".into(),
            ],
            format: vec!["cargo".into(), "fmt".into(), "--all".into(), "--".into(), "--check".into()],
            typecheck: vec!["cargo".into(), "check".into(), "--all".into()],
        },
        ProjectStack::Node => {
            let pm = pm.unwrap_or("npm");
            let bin = match pm {
                "pnpm" => "pnpm",
                "yarn" => "yarn",
                "bun" => "bun",
                _ => "npm",
            };
            let run = if bin == "npm" { "run" } else { "" };
            let mk = |label: &str| {
                if run.is_empty() {
                    vec![bin.to_string(), label.to_string()]
                } else {
                    vec![bin.to_string(), run.to_string(), label.to_string()]
                }
            };
            VerifyDefaults {
                test: mk("test"),
                lint: mk("lint"),
                format: mk("format"),
                typecheck: mk("typecheck"),
            }
        }
        ProjectStack::Go => VerifyDefaults {
            test: vec!["go".into(), "test".into(), "./...".into()],
            lint: vec!["go".into(), "vet".into(), "./...".into()],
            format: vec!["gofmt".into(), "-l".into(), ".".into()],
            typecheck: vec![],
        },
        ProjectStack::DotNet => VerifyDefaults {
            test: vec!["dotnet".into(), "test".into()],
            lint: vec![],
            format: vec!["dotnet".into(), "format".into(), "--verify-no-changes".into()],
            typecheck: vec![],
        },
        ProjectStack::Unknown => VerifyDefaults::default(),
    }
}

#[derive(Debug, Clone, Default)]
pub struct VerifyDefaults {
    pub test: Vec<String>,
    pub lint: Vec<String>,
    pub format: Vec<String>,
    pub typecheck: Vec<String>,
}

/// Named cache volumes appropriate for the detected stack.
pub fn cache_volumes(stack: ProjectStack, repo_name: &str) -> Vec<(String, String)> {
    let safe = repo_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>();
    match stack {
        ProjectStack::Python => vec![
            ("grove-uv-cache".into(), "/home/vscode/.cache/uv".into()),
            ("grove-pip-cache".into(), "/home/vscode/.cache/pip".into()),
        ],
        ProjectStack::Rust => vec![
            ("grove-cargo-registry".into(), "/home/vscode/.cargo/registry".into()),
            (format!("grove-rust-target-{}", safe), format!("/workspaces/{}/target", repo_name)),
        ],
        ProjectStack::Node => vec![
            (format!("grove-node-cache-{}", safe), "/home/vscode/.local/share/pnpm/store".into()),
        ],
        ProjectStack::Go => vec![
            ("grove-go-mod".into(), "/home/vscode/go/pkg/mod".into()),
            ("grove-go-build".into(), "/home/vscode/.cache/go-build".into()),
        ],
        ProjectStack::DotNet => vec![
            ("grove-nuget".into(), "/home/vscode/.nuget/packages".into()),
        ],
        ProjectStack::Unknown => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn detect_python_via_pyproject() {
        let detected = detect_all_stacks(&h(&["pyproject.toml", "uv.lock", "src/main.py"]));
        assert_eq!(detected.first().copied(), Some(ProjectStack::Python));
    }

    #[test]
    fn detect_rust_via_cargo() {
        let detected = detect_all_stacks(&h(&["Cargo.toml", "Cargo.lock"]));
        assert_eq!(detected.first().copied(), Some(ProjectStack::Rust));
    }

    #[test]
    fn detect_node_via_package_json() {
        let detected = detect_all_stacks(&h(&["package.json", "pnpm-lock.yaml"]));
        assert_eq!(detected.first().copied(), Some(ProjectStack::Node));
    }

    #[test]
    fn detect_go_via_gomod() {
        let detected = detect_all_stacks(&h(&["go.mod", "go.sum"]));
        assert_eq!(detected.first().copied(), Some(ProjectStack::Go));
    }

    #[test]
    fn detect_dotnet_via_csproj() {
        let detected = detect_all_stacks(&h(&["MyApp.csproj"]));
        assert_eq!(detected.first().copied(), Some(ProjectStack::DotNet));
    }

    #[test]
    fn monorepo_ranks_rust_first_when_present() {
        let detected = detect_all_stacks(&h(&[
            "Cargo.toml",
            "package.json",
            "pyproject.toml",
            "src/main.rs",
        ]));
        assert_eq!(detected.first().copied(), Some(ProjectStack::Rust));
        assert!(detected.contains(&ProjectStack::Python));
        assert!(detected.contains(&ProjectStack::Node));
    }

    #[test]
    fn nested_package_json_does_not_count() {
        let detected = detect_all_stacks(&h(&["docs/package.json", "Cargo.toml"]));
        // docs/package.json has a '/' so it's ignored; only Rust survives.
        assert_eq!(detected, vec![ProjectStack::Rust]);
    }

    #[test]
    fn unknown_when_no_manifest() {
        assert!(detect_all_stacks(&[]).is_empty());
    }

    #[test]
    fn pm_inference() {
        assert_eq!(
            infer_package_manager(ProjectStack::Python, &h(&["uv.lock"])).as_deref(),
            Some("uv")
        );
        assert_eq!(
            infer_package_manager(ProjectStack::Node, &h(&["pnpm-lock.yaml"])).as_deref(),
            Some("pnpm")
        );
        assert_eq!(
            infer_package_manager(ProjectStack::Rust, &h(&["Cargo.lock"])).as_deref(),
            Some("cargo")
        );
    }

    #[test]
    fn install_line_for_uv() {
        assert_eq!(package_manager_install(Some("uv")), "uv sync");
    }

    #[test]
    fn install_line_for_unknown_is_empty() {
        assert_eq!(package_manager_install(None), "");
    }

    #[test]
    fn default_exts_include_claude_code_for_every_stack() {
        for s in [
            ProjectStack::Python,
            ProjectStack::Rust,
            ProjectStack::Node,
            ProjectStack::Go,
            ProjectStack::DotNet,
            ProjectStack::Unknown,
        ] {
            let e = default_extensions(s);
            assert!(e.contains(&"anthropic.claude-code"));
        }
    }

    #[test]
    fn verify_defaults_for_rust() {
        let v = verify_defaults(ProjectStack::Rust, Some("cargo"));
        assert_eq!(v.test[0], "cargo");
        assert!(v.lint.contains(&"clippy".to_string()));
        assert!(v.format.contains(&"--check".to_string()));
    }

    #[test]
    fn cache_volumes_for_rust_include_target_dir() {
        let vols = cache_volumes(ProjectStack::Rust, "demo");
        assert!(vols
            .iter()
            .any(|(_, t)| t == "/workspaces/demo/target"));
        assert!(vols.iter().any(|(s, _)| s == "grove-cargo-registry"));
    }
}
