use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct Worktree {
    pub path: String,
    pub branch: String,
    pub head: String,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
    #[serde(rename = "isDirty")]
    pub is_dirty: bool,
    #[serde(rename = "isLocked")]
    pub is_locked: bool,
    #[serde(rename = "isPrunable")]
    pub is_prunable: bool,
    #[serde(rename = "isMain")]
    pub is_main: bool,
}

pub struct WorktreeListOptions {
    pub dirty: bool,
    pub locked: bool,
    pub details: bool,
}

#[allow(dead_code)]
pub struct PruneOptions {
    pub dry_run: bool,
    pub force: bool,
    pub base_branch: String,
    pub older_than: Option<u64>, // Age threshold in milliseconds
}

// =============================================================================
// Agentic types — added in the Panzax fork (agentic-flow).
// All new structs use #[serde(default)] on Option fields so partial / older
// config files keep deserializing cleanly.
// =============================================================================

/// Where the project's git directory lives relative to grove's view of it.
///
/// - `Bare`    — the upstream layout: `<root>/<name>.git/` (created by `grove init <url>`)
///               with worktrees as siblings: `<root>/<branch>/`.
/// - `InPlace` — the fork-added layout: a normal `.git/` checkout adopted via
///               `grove init [<path>]`; worktrees go under `<root>/worktrees/<name>/`
///               to avoid scattering them in the project root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectLayout {
    Bare,
    InPlace,
}

impl Default for ProjectLayout {
    fn default() -> Self {
        ProjectLayout::Bare
    }
}

impl ProjectLayout {
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectLayout::Bare => "bare",
            ProjectLayout::InPlace => "in-place",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectStack {
    Python,
    Rust,
    Node,
    Go,
    DotNet,
    Unknown,
}

impl ProjectStack {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectStack::Python => "python",
            ProjectStack::Rust => "rust",
            ProjectStack::Node => "node",
            ProjectStack::Go => "go",
            ProjectStack::DotNet => "dotnet",
            ProjectStack::Unknown => "unknown",
        }
    }

    /// Default base image for the detected stack. The setup-agent's Phase 2 can
    /// refine this; this is the conservative deterministic Phase 1 choice.
    pub fn default_image(self) -> &'static str {
        match self {
            ProjectStack::Python => "mcr.microsoft.com/devcontainers/python:3.12",
            ProjectStack::Rust => "mcr.microsoft.com/devcontainers/rust:latest",
            ProjectStack::Node => "mcr.microsoft.com/devcontainers/javascript-node:22",
            ProjectStack::Go => "mcr.microsoft.com/devcontainers/go:latest",
            ProjectStack::DotNet => "mcr.microsoft.com/devcontainers/dotnet:8.0",
            ProjectStack::Unknown => "mcr.microsoft.com/devcontainers/base:ubuntu",
        }
    }
}

/// The output of `devcontainer::detect_project_context`. Captures everything Phase 1
/// needs to scaffold a sensible devcontainer + `.grove/config.toml`, and gives Phase 2
/// (the setup agent) a starting point for refinement.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectContext {
    #[serde(default)]
    pub stack: Option<ProjectStack>,
    #[serde(default)]
    pub stacks_detected: Vec<ProjectStack>, // multi-stack monorepos
    #[serde(default)]
    pub root_files: Vec<String>,
    #[serde(default)]
    pub default_image: String,
    #[serde(default)]
    pub has_tests: bool,
    #[serde(default)]
    pub has_dockerfile: bool,
    #[serde(default)]
    pub has_pre_commit: bool,
    #[serde(default)]
    pub has_husky: bool,
    #[serde(default)]
    pub has_lefthook: bool,
    #[serde(default)]
    pub has_claude_md: bool,
    #[serde(default)]
    pub package_manager: Option<String>, // "uv", "poetry", "pnpm", etc.
    #[serde(default)]
    pub toolchain_version: Option<String>, // e.g. "python-3.11"
    #[serde(default)]
    pub default_branch: Option<String>,
    #[serde(default)]
    pub repo_name: String,
}

/// Per-agent metadata persisted to `.grove/agents/<name>/agent.toml` when an agent is
/// spawned via `grove spawn`. Read by `grove agents list|status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMetadata {
    pub id: String,
    pub name: String,
    pub worktree: String,
    pub branch: String,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub tmux_session: Option<String>,
    pub spawned_at: DateTime<Utc>,
    #[serde(default = "default_provider")]
    pub provider: String,
}

fn default_provider() -> String {
    "claude-code".to_string()
}

/// Status of an agent's Ralph loop. Persisted as the `active` flag + iteration counter
/// in `loop.md` frontmatter; this enum is the in-Rust view of that state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoopStatus {
    Running,
    Paused,
    Complete,
    Failed,
    Blocked,
}

impl LoopStatus {
    #[allow(dead_code)] // serialized via serde for status JSON output; reserved for future API
    pub fn as_str(self) -> &'static str {
        match self {
            LoopStatus::Running => "running",
            LoopStatus::Paused => "paused",
            LoopStatus::Complete => "complete",
            LoopStatus::Failed => "failed",
            LoopStatus::Blocked => "blocked",
        }
    }
}

/// In-Rust view of `.grove/agents/<name>/loop.md` frontmatter.
/// Wire-format is YAML (the Stop hook reads it as plain text); we parse a small
/// subset on demand. `body` carries the prompt body (everything after the closing
/// `---` of the frontmatter).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoopState {
    pub active: bool,
    #[serde(default)]
    pub iteration: u32,
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    #[serde(default)]
    pub completion_promise: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub status: Option<LoopStatus>, // optional layer on top of `active`
    #[serde(default)]
    pub last_action: Option<String>,
    #[serde(default)]
    pub last_updated: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub body: String,
}

fn default_max_iterations() -> u32 {
    30
}

/// A single bus event. Persisted as a Markdown file at
/// `.grove/bus/log.d/<ts>-<sender>.md` (broadcast) or
/// `.grove/bus/inbox/<recipient>/<ts>-from-<sender>.md` (direct).
///
/// Wire format is human-readable Markdown with a short YAML frontmatter block carrying
/// the metadata fields. This module reads/writes both halves.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusMessage {
    pub id: String,
    pub from: String,
    pub to: String, // recipient name or "broadcast"
    pub ts: DateTime<Utc>,
    pub kind: MessageKind,
    pub body: String,
    #[serde(default)]
    pub contract: Option<String>, // for contract-kind messages: filename slug
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageKind {
    Broadcast,
    Direct,
    Contract,
    Status,
}

impl MessageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MessageKind::Broadcast => "broadcast",
            MessageKind::Direct => "direct",
            MessageKind::Contract => "contract",
            MessageKind::Status => "status",
        }
    }
}

/// Top-level deserialization target for `.grove/config.toml`. All sections are
/// optional so partial / hand-edited configs keep parsing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GroveConfig {
    #[serde(default)]
    pub project: ProjectSection,
    #[serde(default)]
    pub agent: AgentSection,
    #[serde(default)]
    pub devcontainer: DevcontainerSection,
    #[serde(default)]
    pub bus: BusSection,
    #[serde(default)]
    pub hook: HookSection,
    #[serde(default)]
    pub mounts: MountsSection,
    #[serde(default)]
    pub stack: StackSection,
    #[serde(default)]
    pub verify: VerifySection,
    #[serde(default)]
    pub caches: CachesSection,
    #[serde(default)]
    pub hooks: HooksSection,
    #[serde(default)]
    pub meta: MetaSection,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectSection {
    #[serde(default)]
    pub layout: ProjectLayout,
    #[serde(default)]
    pub root: Option<String>,
}

/// Agent-isolation strategy. `Shared` = one devcontainer hosts every agent's
/// worktree (current default; matches the freqtrade harness). `PerWorktree` is
/// a future flag — accepted by config parsing for forward-compat but currently
/// rejected at runtime with a "not implemented" notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentIsolation {
    Shared,
    PerWorktree,
}

impl Default for AgentIsolation {
    fn default() -> Self {
        AgentIsolation::Shared
    }
}

impl AgentIsolation {
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            AgentIsolation::Shared => "shared",
            AgentIsolation::PerWorktree => "per-worktree",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSection {
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    #[serde(default = "default_session_backend")]
    pub session_backend: String, // "tmux" | "background"
    #[serde(default)]
    pub isolation: AgentIsolation, // shared (default) | per-worktree (future)
}

impl Default for AgentSection {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            max_iterations: default_max_iterations(),
            session_backend: default_session_backend(),
            isolation: AgentIsolation::default(),
        }
    }
}

fn default_session_backend() -> String {
    "tmux".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevcontainerSection {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub auto_up: bool,
    /// Container-side path that the host project root is mounted at.
    /// Detected from devcontainer.json's `workspaceFolder` field; defaults to
    /// `/workspaces/<repo>` when scaffolded.
    #[serde(default)]
    pub workspace_target: Option<String>,
    /// Container user, matches devcontainer.json `remoteUser`. Default
    /// "vscode" for the Microsoft base images.
    #[serde(default = "default_remote_user")]
    pub remote_user: String,
}

impl Default for DevcontainerSection {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_up: true,
            workspace_target: None,
            remote_user: default_remote_user(),
        }
    }
}

fn default_remote_user() -> String {
    "vscode".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusSection {
    #[serde(default = "default_bus_dir")]
    pub dir: String,
}

impl Default for BusSection {
    fn default() -> Self {
        Self {
            dir: default_bus_dir(),
        }
    }
}

fn default_bus_dir() -> String {
    ".grove/bus".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookSection {
    #[serde(default = "default_true")]
    pub auto_install: bool,
}

impl Default for HookSection {
    fn default() -> Self {
        Self { auto_install: true }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MountsSection {
    #[serde(default)]
    pub secrets_path: Option<String>,
    #[serde(default)]
    pub secrets_mode: Option<String>, // "ro" | "rw"
    #[serde(default)]
    pub claude_inherit: Option<String>, // "full" | "scoped" | "none"
    #[serde(default)]
    pub gh_auth: Option<String>, // "pat" | "ro-mount" | "rw-mount" | "none"
    #[serde(default, rename = "extra")]
    pub extra: Vec<ExtraMount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtraMount {
    pub source: String,
    pub target: String,
    #[serde(default = "default_mount_mode")]
    pub mode: String, // "ro" | "rw"
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub reason: String,
}

fn default_mount_mode() -> String {
    "ro".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StackSection {
    #[serde(default)]
    pub detected: Option<String>,
    #[serde(default)]
    pub toolchain: Option<String>,
    #[serde(default)]
    pub package_mgr: Option<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerifySection {
    #[serde(default)]
    pub test: Vec<String>,
    #[serde(default)]
    pub lint: Vec<String>,
    #[serde(default)]
    pub format: Vec<String>,
    #[serde(default)]
    pub typecheck: Vec<String>,
    #[serde(default)]
    pub source: Option<String>, // "ci-scrape" | "stack-default" | "user"
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CachesSection {
    #[serde(default)]
    pub volumes: Vec<CacheVolume>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheVolume {
    pub source: String, // named volume
    pub target: String, // container path
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HooksSection {
    #[serde(default)]
    pub pre_commit: bool,
    #[serde(default)]
    pub husky: bool,
    #[serde(default)]
    pub lefthook: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetaSection {
    #[serde(default)]
    pub gitignore_patched: bool,
    #[serde(default)]
    pub dockerignore_patched: bool,
    #[serde(default)]
    pub claude_md_strategy: Option<String>, // "reference" | "draft" | "merge" | "absent"
    #[serde(default)]
    pub initialized_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub schema_version: u32,
}
