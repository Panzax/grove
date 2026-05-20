// CI-parity scrape — extract verify-class commands from `.github/workflows/*.yml`.
//
// Strategy: walk every workflow file, split out `run:` blocks (one-line and
// multi-line `run: |`), then classify each line by regex into test / lint /
// format / typecheck buckets. CI authoritative; user can edit
// `.grove/config.toml [verify]` afterward.
//
// Hand-rolled YAML parsing: we don't need full YAML semantics, just the lines
// that begin with `run:`. Avoids the serde_yaml dep.

use std::path::Path;

use crate::git::worktree_manager::{head_file_exists, ls_head_files, show_head_file, RepoContext};

#[derive(Debug, Clone, Default)]
pub struct ScrapeResult {
    pub test: Vec<Vec<String>>,
    pub lint: Vec<Vec<String>>,
    pub format: Vec<Vec<String>>,
    pub typecheck: Vec<Vec<String>>,
}

impl ScrapeResult {
    pub fn is_empty(&self) -> bool {
        self.test.is_empty() && self.lint.is_empty() && self.format.is_empty() && self.typecheck.is_empty()
    }
}

pub fn scrape(ctx: &RepoContext) -> ScrapeResult {
    let mut out = ScrapeResult::default();
    let head_files = ls_head_files(ctx).unwrap_or_default();
    let workflow_files: Vec<String> = head_files
        .into_iter()
        .filter(|p| p.starts_with(".github/workflows/") && (p.ends_with(".yml") || p.ends_with(".yaml")))
        .collect();
    for wf in workflow_files {
        if !head_file_exists(ctx, &wf) {
            continue;
        }
        let raw = show_head_file(ctx, &wf).unwrap_or_default();
        for cmd in extract_run_commands(&raw) {
            classify_and_push(&cmd, &mut out);
        }
    }
    out
}

/// Pure helper for unit testing.
pub fn classify_and_push(line: &str, out: &mut ScrapeResult) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }
    let lower = trimmed.to_ascii_lowercase();
    let tokens: Vec<String> = trimmed.split_whitespace().map(|s| s.to_string()).collect();

    let buckets: &[(&[&str], fn(&mut ScrapeResult) -> &mut Vec<Vec<String>>)] = &[
        (
            &[
                "pytest", "cargo test", "go test", "npm test", "pnpm test",
                "yarn test", "bun test", "dotnet test", "rspec", "jest", "vitest",
            ],
            |o| &mut o.test,
        ),
        (
            &[
                "ruff check", "flake8", "eslint", "clippy", "golangci-lint",
                "pylint", "rubocop",
            ],
            |o| &mut o.lint,
        ),
        (
            &[
                "ruff format", "black --check", "prettier --check",
                "cargo fmt", "gofmt", "dotnet format",
            ],
            |o| &mut o.format,
        ),
        (
            &[
                "mypy", "tsc --noemit", "tsc -p", "cargo check",
            ],
            |o| &mut o.typecheck,
        ),
    ];

    for (patterns, get) in buckets {
        if patterns.iter().any(|p| lower.contains(p)) {
            let bucket = get(out);
            if !bucket.iter().any(|existing| existing == &tokens) {
                bucket.push(tokens.clone());
            }
            return;
        }
    }
}

/// Extract every `run:` body from a workflow YAML. Handles both one-liner
///   - run: pytest -q
/// and folded/literal blocks
///   - run: |
///       pytest -q
///       ruff check .
pub fn extract_run_commands(yaml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut lines = yaml.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("run:") {
            let rest = rest.trim_start();
            if rest.is_empty() || rest == "|" || rest == ">" || rest == "|-" || rest == ">-" {
                // Literal/folded block. Capture until we hit a line with indent
                // <= the `run:` line's indent.
                let run_indent = line.len() - trimmed.len();
                while let Some(next) = lines.peek() {
                    let body_indent = next.len() - next.trim_start().len();
                    if next.trim().is_empty() {
                        lines.next();
                        continue;
                    }
                    if body_indent <= run_indent {
                        break;
                    }
                    out.push(next.trim().to_string());
                    lines.next();
                }
            } else {
                // One-liner.
                out.push(rest.trim().to_string());
            }
        }
    }
    out
}

pub fn into_verify_section(result: ScrapeResult) -> crate::models::VerifySection {
    let pick_one = |v: Vec<Vec<String>>| -> Vec<String> {
        v.into_iter().next().unwrap_or_default()
    };
    crate::models::VerifySection {
        test: pick_one(result.test),
        lint: pick_one(result.lint),
        format: pick_one(result.format),
        typecheck: pick_one(result.typecheck),
        source: Some("ci-scrape".to_string()),
    }
}

#[allow(dead_code)]
fn unused_path(_p: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_oneliner_run() {
        let yaml = r#"
jobs:
  ci:
    steps:
      - name: test
        run: cargo test
"#;
        let cmds = extract_run_commands(yaml);
        assert_eq!(cmds, vec!["cargo test"]);
    }

    #[test]
    fn extract_multiline_run() {
        let yaml = r#"
jobs:
  ci:
    steps:
      - name: quality
        run: |
          cargo check
          cargo test
          cargo fmt --check
"#;
        let cmds = extract_run_commands(yaml);
        assert_eq!(cmds.len(), 3);
        assert_eq!(cmds[0], "cargo check");
        assert_eq!(cmds[2], "cargo fmt --check");
    }

    #[test]
    fn classify_pytest_into_test() {
        let mut out = ScrapeResult::default();
        classify_and_push("pytest -q tests/", &mut out);
        assert_eq!(out.test.len(), 1);
        assert!(out.test[0].iter().any(|s| s == "pytest"));
    }

    #[test]
    fn classify_clippy_into_lint() {
        let mut out = ScrapeResult::default();
        classify_and_push("cargo clippy --all -- -D warnings", &mut out);
        assert_eq!(out.lint.len(), 1);
        assert!(out.lint[0].iter().any(|s| s == "clippy"));
    }

    #[test]
    fn classify_dedupes() {
        let mut out = ScrapeResult::default();
        classify_and_push("cargo test", &mut out);
        classify_and_push("cargo test", &mut out);
        assert_eq!(out.test.len(), 1);
    }

    #[test]
    fn into_verify_section_picks_first() {
        let mut r = ScrapeResult::default();
        classify_and_push("cargo test", &mut r);
        classify_and_push("cargo nextest run", &mut r);
        let v = into_verify_section(r);
        assert_eq!(v.test[0], "cargo");
        assert_eq!(v.source.as_deref(), Some("ci-scrape"));
    }
}
