// Container backend abstraction.
//
// Grove can run agents in one of two container backends, selected per-project
// in `.grove/config.toml` (`[container] backend`):
//   - `devcontainer` (default): bind-mounted devcontainer via the `devcontainer`
//     CLI. The repo is mounted from the host; edits land on the host live.
//   - `sandbox`: copy-in isolation via plain `docker run` + `docker exec`. The
//     repo is copied in; the only egress is `git push`.
//
// The trait funnels the lifecycle + exec surface so call sites (spawn, attach,
// integrate, agents) stay backend-agnostic — they call the free functions in
// `container.rs`, which delegate here via `backend_for`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};

use super::container::{self, ContainerInfo};
use crate::models::{ContainerBackendKind, GroveConfig};

pub trait ContainerBackend {
    /// Bring the project's container up. Idempotent: reuse if already running.
    fn ensure_up(&self, project_root: &Path) -> Result<ContainerInfo, String>;
    /// Stop and remove the project's container.
    fn down(&self, project_root: &Path) -> Result<(), String>;
    /// Best-effort check that the container is currently up.
    fn is_up(&self, project_root: &Path) -> bool;
    /// Run a command in the container, capturing output.
    fn exec(&self, info: &ContainerInfo, argv: &[&str]) -> Result<Output, String>;
    /// Run a command in the container with stdio inherited (no capture).
    fn exec_streaming(&self, info: &ContainerInfo, argv: &[&str]) -> Result<ExitStatus, String>;
    /// Human-readable "how to attach" line.
    fn attach_instructions(&self, info: &ContainerInfo, session_name: &str) -> String;
}

/// Select the backend for a project from its `.grove/config.toml`. Unknown or
/// missing config falls back to `Devcontainer`, so existing projects (and any
/// path without a config) keep their current behavior.
pub fn backend_for(project_root: &Path) -> Box<dyn ContainerBackend> {
    match read_backend_kind(project_root) {
        ContainerBackendKind::Sandbox => Box::new(SandboxBackend::default()),
        ContainerBackendKind::Devcontainer => Box::new(DevcontainerBackend::default()),
    }
}

fn read_backend_kind(project_root: &Path) -> ContainerBackendKind {
    let path = project_root.join(".grove").join("config.toml");
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return ContainerBackendKind::default(),
    };
    match toml::from_str::<GroveConfig>(&raw) {
        Ok(cfg) => cfg.container.backend,
        Err(_) => ContainerBackendKind::default(),
    }
}

// =============================================================================
// Devcontainer backend — the historical implementation, unchanged. The actual
// bodies live in `container.rs` as `dc_*` so the existing unit tests stay put.
// =============================================================================

#[derive(Default)]
pub struct DevcontainerBackend;

impl ContainerBackend for DevcontainerBackend {
    fn ensure_up(&self, project_root: &Path) -> Result<ContainerInfo, String> {
        container::dc_ensure_up(project_root)
    }
    fn down(&self, project_root: &Path) -> Result<(), String> {
        container::dc_down(project_root)
    }
    fn is_up(&self, project_root: &Path) -> bool {
        container::dc_is_up(project_root)
    }
    fn exec(&self, info: &ContainerInfo, argv: &[&str]) -> Result<Output, String> {
        container::dc_exec(info, argv)
    }
    fn exec_streaming(&self, info: &ContainerInfo, argv: &[&str]) -> Result<ExitStatus, String> {
        container::dc_exec_streaming(info, argv)
    }
    fn attach_instructions(&self, info: &ContainerInfo, session_name: &str) -> String {
        container::dc_attach_instructions(info, session_name)
    }
}

// =============================================================================
// Sandbox backend — copy-in isolation via plain `docker run` + `docker exec`.
//
// Topology (Model 1 — the sandbox is a self-contained git universe):
//   - One long-lived container per project, named `grove-sb-<hash(root)>`.
//   - The container mirrors host *absolute paths* exactly: the seeded git repo
//     and every worktree live at the SAME path inside the container as the
//     host bare clone would on disk. That makes `host_to_container_path` an
//     identity (workspace_target == workspace_root), so all of grove's
//     path-translation, tmux launch, and pane-log machinery works unchanged.
//   - Only `.grove/` is bind-mounted (the control plane: bus, agent state,
//     logs, hooks). The *code* is copied in via a git bundle, never mounted,
//     so an agent's working-tree edits never touch the host — the sole egress
//     is `git push` from inside the container.
//   - The container runs as the host uid:gid so the bind-mounted `.grove/`
//     stays readable/writable from both sides (no root-owned files leaking
//     onto the host).
// =============================================================================

#[derive(Default)]
pub struct SandboxBackend;

/// Built-in fallback image when neither `[sandbox] image` nor the
/// `GROVE_SANDBOX_IMAGE` env var specifies one. Ships git + claude.
const DEFAULT_SANDBOX_IMAGE: &str = "docker/sandbox-templates:claude-code";

impl ContainerBackend for SandboxBackend {
    fn ensure_up(&self, project_root: &Path) -> Result<ContainerInfo, String> {
        let root = canonical(project_root);
        let name = sandbox_container_name(&root);
        match inspect_running(&name)? {
            Some(true) => { /* already running — reuse, never re-seed */ }
            Some(false) => {
                // Exists but stopped: restart it. The seeded repo + worktrees
                // (in-flight, possibly unpushed agent work) are preserved.
                docker_run_checked(&["start", &name], "docker start")?;
            }
            None => create_and_seed(&root, &name)?,
        }
        Ok(sandbox_container_info(&root))
    }

    fn down(&self, project_root: &Path) -> Result<(), String> {
        let name = sandbox_container_name(&canonical(project_root));
        // `rm -f` stops + removes in one shot; ignore "no such container".
        let out = docker_command()
            .args(["rm", "-f", &name])
            .output()
            .map_err(|e| format!("invoke docker: {}", e))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            if err.contains("No such container") {
                return Ok(());
            }
            return Err(format!("docker rm -f {}: {}", name, err.trim()));
        }
        Ok(())
    }

    fn is_up(&self, project_root: &Path) -> bool {
        let name = sandbox_container_name(&canonical(project_root));
        matches!(inspect_running(&name), Ok(Some(true)))
    }

    fn exec(&self, info: &ContainerInfo, argv: &[&str]) -> Result<Output, String> {
        let name = sandbox_container_name(&info.workspace_root);
        let exec_args = build_exec_args(&name, &info.workspace_root, argv);
        let refs: Vec<&str> = exec_args.iter().map(|s| s.as_str()).collect();
        docker_command()
            .args(&refs)
            .output()
            .map_err(|e| format!("invoke docker exec: {}", e))
    }

    fn exec_streaming(&self, info: &ContainerInfo, argv: &[&str]) -> Result<ExitStatus, String> {
        let name = sandbox_container_name(&info.workspace_root);
        let exec_args = build_exec_args(&name, &info.workspace_root, argv);
        let refs: Vec<&str> = exec_args.iter().map(|s| s.as_str()).collect();
        docker_command()
            .args(&refs)
            .status()
            .map_err(|e| format!("invoke docker exec: {}", e))
    }

    fn attach_instructions(&self, info: &ContainerInfo, session_name: &str) -> String {
        let name = sandbox_container_name(&info.workspace_root);
        let docker = docker_argv().join(" ");
        format!(
            "{} exec -it -u {} {} tmux attach -t {}",
            docker,
            host_uid_gid().0,
            name,
            session_name
        )
    }
}

// -----------------------------------------------------------------------------
// Identity + configuration
// -----------------------------------------------------------------------------

/// True when the project at `project_root` is configured for the sandbox
/// backend. Read by the git layer to decide whether worktree/branch ops run on
/// the host or inside the per-project sandbox container.
pub fn project_is_sandbox(project_root: &Path) -> bool {
    matches!(
        read_backend_kind(project_root),
        ContainerBackendKind::Sandbox
    )
}

/// The identical-path `ContainerInfo` for a sandbox project (workspace_target
/// == workspace_root). Cheap to build — used to address the sandbox for
/// `container::exec` from git call sites that only hold a project root.
pub fn sandbox_info(project_root: &Path) -> ContainerInfo {
    sandbox_container_info(&canonical(project_root))
}

/// Canonicalize a path, falling back to the input when it doesn't exist yet.
fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Per-project container name, keyed on the *canonical project root path* (not
/// the repo name — two clones of the same repo must get distinct sandboxes).
/// Stable across processes: `DefaultHasher` uses a fixed key.
pub fn sandbox_container_name(canonical_root: &Path) -> String {
    let mut h = DefaultHasher::new();
    canonical_root.hash(&mut h);
    format!("grove-sb-{:016x}", h.finish())
}

/// `docker` (or the `GROVE_DOCKER_COMMAND` override) split into argv tokens.
/// Tests point this at a stub script the same way `GROVE_DEVCONTAINER_COMMAND`
/// works for the devcontainer backend.
fn docker_argv() -> Vec<String> {
    docker_argv_with(std::env::var("GROVE_DOCKER_COMMAND").ok().as_deref())
}

fn docker_argv_with(override_value: Option<&str>) -> Vec<String> {
    if let Some(raw) = override_value {
        let tokens: Vec<String> = raw.split_whitespace().map(|s| s.to_string()).collect();
        if !tokens.is_empty() {
            return tokens;
        }
    }
    vec!["docker".to_string()]
}

/// A `Command` pre-loaded with the docker argv (program + any wrapper args).
fn docker_command() -> Command {
    let argv = docker_argv();
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd
}

/// Resolve the sandbox image: `GROVE_SANDBOX_IMAGE` env > `[sandbox] image` in
/// config > built-in default.
fn sandbox_image(project_root: &Path) -> String {
    if let Ok(img) = std::env::var("GROVE_SANDBOX_IMAGE") {
        if !img.trim().is_empty() {
            return img;
        }
    }
    read_config(project_root)
        .and_then(|c| c.sandbox.image)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_SANDBOX_IMAGE.to_string())
}

/// GitHub PAT forwarded into the sandbox as `GH_TOKEN`. Reads the host env var
/// NAMED by `[mounts] gh_token_env` (per-project; defaults to `GH_TOKEN_RO`).
/// `None` when that var is unset/empty — the integrate agent then roadblocks
/// with a clear message instead of pushing with no/wrong credentials.
fn sandbox_gh_token(project_root: &Path) -> Option<String> {
    let var = read_config(project_root)
        .map(|c| c.mounts.gh_token_env_name().to_string())
        .unwrap_or_else(|| "GH_TOKEN_RO".to_string());
    std::env::var(var).ok().filter(|s| !s.trim().is_empty())
}

/// Display user recorded for the sandbox (informational; the process runs as
/// the host uid). From `[sandbox] user`, defaulting to `agent`.
fn sandbox_user(project_root: &Path) -> String {
    read_config(project_root)
        .and_then(|c| c.sandbox.user)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "agent".to_string())
}

fn read_config(project_root: &Path) -> Option<GroveConfig> {
    let raw = std::fs::read_to_string(project_root.join(".grove").join("config.toml")).ok()?;
    toml::from_str(&raw).ok()
}

/// Host uid/gid as strings. The sandbox runs as these so bind-mounted `.grove`
/// files are owned consistently on host and container. Falls back to 1000:1000.
fn host_uid_gid() -> (String, String) {
    fn id(flag: &str) -> Option<String> {
        let out = Command::new("id").arg(flag).output().ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
    (
        id("-u").unwrap_or_else(|| "1000".to_string()),
        id("-g").unwrap_or_else(|| "1000".to_string()),
    )
}

/// Container-side HOME for the sandbox process. A plain container-fs path (not
/// host-identical — HOME needn't be) so claude/git config + caches live in the
/// copy-in filesystem, isolated from the host. Persists across stop/start;
/// reset on container removal (re-seeded on recreate).
const SANDBOX_HOME: &str = "/sbhome";

/// Expand a leading `~` against the host HOME. Used to resolve mount sources
/// like `~/.claude` declared in config.
fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{}/{}", home.trim_end_matches('/'), rest);
        }
    }
    path.to_string()
}

fn sandbox_container_info(canonical_root: &Path) -> ContainerInfo {
    // Identical paths: workspace_target == workspace_root makes
    // `host_to_container_path` and tmux env translation no-ops.
    ContainerInfo::new(
        canonical_root.to_path_buf(),
        canonical_root.to_path_buf(),
        sandbox_user(canonical_root),
    )
}

// -----------------------------------------------------------------------------
// docker exec arg construction (pure, unit-tested)
// -----------------------------------------------------------------------------

/// Build `exec -u <uid>:<gid> -w <root> <name> <argv...>`. We pass `-u`
/// explicitly so every exec runs as the same host uid the container was
/// started with (docker exec does not otherwise inherit `run --user`), and
/// `-w` so relative work resolves under the project root.
fn build_exec_args(name: &str, workspace_root: &Path, argv: &[&str]) -> Vec<String> {
    let (uid, gid) = host_uid_gid();
    let mut out = vec![
        "exec".to_string(),
        "-u".to_string(),
        format!("{}:{}", uid, gid),
        "-w".to_string(),
        workspace_root.to_string_lossy().to_string(),
        name.to_string(),
    ];
    out.extend(argv.iter().map(|s| s.to_string()));
    out
}

// -----------------------------------------------------------------------------
// Lifecycle internals
// -----------------------------------------------------------------------------

/// `docker inspect -f '{{.State.Running}}'`: Ok(Some(true|false)) when the
/// container exists, Ok(None) when it doesn't, Err on docker failure.
fn inspect_running(name: &str) -> Result<Option<bool>, String> {
    let out = docker_command()
        .args(["inspect", "-f", "{{.State.Running}}", name])
        .output()
        .map_err(|e| format!("invoke docker inspect: {}", e))?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(Some(s == "true"))
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("No such object") || err.contains("No such container") {
            Ok(None)
        } else {
            Err(format!("docker inspect {}: {}", name, err.trim()))
        }
    }
}

fn docker_run_checked(args: &[&str], label: &str) -> Result<String, String> {
    let out = docker_command()
        .args(args)
        .output()
        .map_err(|e| format!("invoke docker: {}", e))?;
    if !out.status.success() {
        return Err(format!(
            "{} failed: {}",
            label,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run a command inside the sandbox during seeding (before a ContainerInfo
/// exists). `user` selects the exec user: `Some("0")` for the privileged
/// directory bootstrap, `None` for the host uid.
fn seed_exec(name: &str, root: &Path, user: Option<&str>, argv: &[&str]) -> Result<(), String> {
    let (uid, gid) = host_uid_gid();
    let user_arg = match user {
        Some(u) => u.to_string(),
        None => format!("{}:{}", uid, gid),
    };
    let mut full = vec!["exec", "-u", &user_arg, "-w"];
    let root_str = root.to_string_lossy().to_string();
    full.push(&root_str);
    full.push(name);
    full.extend_from_slice(argv);
    let out = docker_command()
        .args(&full)
        .output()
        .map_err(|e| format!("invoke docker exec: {}", e))?;
    if !out.status.success() {
        return Err(format!(
            "sandbox seed step `{}` failed: {}",
            argv.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Create the per-project sandbox container and seed it with the committed
/// history from the host bare clone (via `git bundle`). Network-free: origin is
/// configured but uncontacted until an integrate push.
fn create_and_seed(root: &Path, name: &str) -> Result<(), String> {
    // The host bare clone is the bundle source. Its path is reused verbatim as
    // the in-container repo path (identical-path model).
    let bare = crate::utils::discover_bare_clone(Some(root)).map_err(|e| {
        format!(
            "sandbox seeding needs a bare clone under {} (run `grove init <url>`): {}",
            root.display(),
            e.message
        )
    })?;

    let grove_dir = root.join(".grove");
    if !grove_dir.exists() {
        return Err(format!(
            "sandbox needs the control plane at {} — run `grove init` first",
            grove_dir.display()
        ));
    }

    // 1. Bundle the full committed history (all refs) from the host bare clone.
    let bundle = std::env::temp_dir().join(format!("{}.bundle", name));
    let _ = std::fs::remove_file(&bundle);
    let out = Command::new("git")
        .current_dir(&bare)
        .args(["bundle", "create", &bundle.to_string_lossy(), "--all"])
        .output()
        .map_err(|e| format!("invoke git bundle: {}", e))?;
    if !out.status.success() {
        return Err(format!(
            "git bundle create failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    // 2. Capture the true origin URL + committer identity from the host repo so
    //    pushes from inside the sandbox reach the real remote. Also capture the
    //    symbolic HEAD (`git bundle --all` carries branch refs but NOT the
    //    symbolic HEAD, so a bundle clone otherwise defaults HEAD to a
    //    nonexistent `master` and `git worktree add` fails).
    let head_ref = resolve_seed_head(&bare);
    let origin_url = git_config_get(&bare, "remote.origin.url");
    let user_name = git_config_get(&bare, "user.name")
        .or_else(|| git_global_config_get("user.name"))
        .unwrap_or_else(|| "grove agent".to_string());
    let user_email = git_config_get(&bare, "user.email")
        .or_else(|| git_global_config_get("user.email"))
        .unwrap_or_else(|| "grove@localhost".to_string());

    // 3. Create the container: long-lived (`sleep infinity`), host uid, GH
    //    token injected, the .grove control plane bind-mounted, plus any
    //    claude-credential mounts the config asks for.
    let extra_mounts = sandbox_extra_mounts(root);
    let create_args = build_create_args(
        name,
        root,
        &sandbox_image(root),
        SANDBOX_HOME,
        sandbox_gh_token(root).as_deref(),
        &extra_mounts,
    );
    let refs: Vec<&str> = create_args.iter().map(|s| s.as_str()).collect();
    docker_run_checked(&refs, "docker run")
        .map_err(|e| format!("{}\n  (image: {})", e, sandbox_image(root)))?;

    // 4. As root: make the project root + HOME writable by the host uid so the
    //    subsequent (unprivileged) git clone + worktree adds can create the
    //    bare repo and worktree dirs. Only the root node is chowned — the
    //    bind-mounted .grove keeps its host ownership.
    let (uid, gid) = host_uid_gid();
    let chown_target = format!("{}:{}", uid, gid);
    seed_exec(
        name,
        root,
        Some("0"),
        &["mkdir", "-p", &root.to_string_lossy(), SANDBOX_HOME],
    )?;
    seed_exec(
        name,
        root,
        Some("0"),
        &[
            "chown",
            &chown_target,
            &root.to_string_lossy(),
            SANDBOX_HOME,
        ],
    )?;

    // 5. Copy the bundle in and clone it as a bare repo at the host bare-clone
    //    path. `docker cp` lands it as root; readable by the clone.
    let bundle_in = "/tmp/grove-seed.bundle";
    docker_run_checked(
        &[
            "cp",
            &bundle.to_string_lossy(),
            &format!("{}:{}", name, bundle_in),
        ],
        "docker cp seed bundle",
    )?;
    let bare_str = bare.to_string_lossy().to_string();
    seed_exec(
        name,
        root,
        None,
        &["git", "clone", "--bare", bundle_in, &bare_str],
    )?;

    // 5b. Point HEAD at the real default branch so worktree creation has a
    //     valid base ref.
    if let Some(head) = head_ref {
        seed_exec(
            name,
            root,
            None,
            &["git", "-C", &bare_str, "symbolic-ref", "HEAD", &head],
        )?;
    }

    // 6. Restore origin + fetch refspec; configure committer identity + mark
    //    the seeded paths safe (uid owns them, but belt-and-suspenders).
    if let Some(url) = origin_url {
        seed_exec(
            name,
            root,
            None,
            &["git", "-C", &bare_str, "remote", "set-url", "origin", &url],
        )?;
        seed_exec(
            name,
            root,
            None,
            &[
                "git",
                "-C",
                &bare_str,
                "config",
                "remote.origin.fetch",
                "+refs/heads/*:refs/remotes/origin/*",
            ],
        )?;
    }
    seed_exec(
        name,
        root,
        None,
        &["git", "config", "--global", "user.name", &user_name],
    )?;
    seed_exec(
        name,
        root,
        None,
        &["git", "config", "--global", "user.email", &user_email],
    )?;
    seed_exec(
        name,
        root,
        None,
        &["git", "config", "--global", "--add", "safe.directory", "*"],
    )?;

    let _ = std::fs::remove_file(&bundle);
    Ok(())
}

/// Mounts to add beyond `.grove/`, derived from config (claude credentials).
/// Each entry is `(host_source, container_target, readonly)`; only sources that
/// exist on the host are included so `docker run` never auto-creates empty
/// root-owned stand-ins.
fn sandbox_extra_mounts(project_root: &Path) -> Vec<(String, String, bool)> {
    let claude = read_config(project_root)
        .and_then(|c| c.mounts.claude_inherit)
        .unwrap_or_else(|| "scoped".to_string());
    let mut out = Vec::new();
    let claude_home = format!("{}/.claude", SANDBOX_HOME);
    match claude.as_str() {
        "none" => {}
        "full" => {
            let src = expand_home("~/.claude");
            if Path::new(&src).exists() {
                out.push((src, claude_home, false));
            }
        }
        // scoped (default): the same three RO resources the devcontainer
        // baseline mounts, but only when present on the host.
        _ => {
            for leaf in ["plugins", ".credentials.json", "settings.json"] {
                let src = expand_home(&format!("~/.claude/{}", leaf));
                if Path::new(&src).exists() {
                    out.push((src, format!("{}/{}", claude_home, leaf), true));
                }
            }
        }
    }
    out
}

/// Build the `docker run` argv for the sandbox container (pure, unit-tested).
fn build_create_args(
    name: &str,
    root: &Path,
    image: &str,
    home: &str,
    gh_token: Option<&str>,
    extra_mounts: &[(String, String, bool)],
) -> Vec<String> {
    let (uid, gid) = host_uid_gid();
    let root_str = root.to_string_lossy().to_string();
    // Build with an explicit `/` (not Path::join) so it matches the container
    // target below on every host platform — these are docker mount paths under
    // the Linux identical-path model, never native Windows paths.
    let grove_src = format!("{}/.grove", root_str);
    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        name.to_string(),
        "-u".to_string(),
        format!("{}:{}", uid, gid),
        "-w".to_string(),
        root_str.clone(),
        "-e".to_string(),
        format!("HOME={}", home),
        // Bind only the control plane — code is copied in, never mounted.
        "--mount".to_string(),
        format!("type=bind,source={},target={}/.grove", grove_src, root_str),
    ];
    for (src, target, ro) in extra_mounts {
        args.push("--mount".to_string());
        let mut spec = format!("type=bind,source={},target={}", src, target);
        if *ro {
            spec.push_str(",readonly");
        }
        args.push(spec);
    }
    if let Some(tok) = gh_token {
        if !tok.trim().is_empty() {
            args.push("-e".to_string());
            args.push(format!("GH_TOKEN={}", tok));
        }
    }
    // Override the image entrypoint (some images auto-launch claude) so the
    // container is a long-lived exec host.
    args.push("--entrypoint".to_string());
    args.push("sleep".to_string());
    args.push(image.to_string());
    args.push("infinity".to_string());
    args
}

fn git_config_get(repo: &Path, key: &str) -> Option<String> {
    let out = Command::new("git")
        .current_dir(repo)
        .args(["config", "--get", key])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Resolve the `refs/heads/<branch>` the seeded clone's HEAD should point at.
/// `git bundle --all` doesn't carry the symbolic HEAD, and the host bare
/// clone's HEAD may itself be invalid (e.g. an upstream whose HEAD points at a
/// branch that no longer exists), so we pick a branch that actually exists:
/// the symbolic-HEAD target if valid, else main, else master, else the first
/// local branch.
fn resolve_seed_head(repo: &Path) -> Option<String> {
    if let Some(sym) = git_symbolic_head(repo) {
        let branch = sym.trim_start_matches("refs/heads/");
        if local_branch_exists(repo, branch) {
            return Some(sym);
        }
    }
    for cand in ["main", "master"] {
        if local_branch_exists(repo, cand) {
            return Some(format!("refs/heads/{}", cand));
        }
    }
    first_local_branch(repo).map(|b| format!("refs/heads/{}", b))
}

/// The repo's symbolic HEAD (e.g. `refs/heads/main`), or None if detached/unset.
fn git_symbolic_head(repo: &Path) -> Option<String> {
    let out = Command::new("git")
        .current_dir(repo)
        .args(["symbolic-ref", "HEAD"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn local_branch_exists(repo: &Path, branch: &str) -> bool {
    Command::new("git")
        .current_dir(repo)
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{}", branch),
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn first_local_branch(repo: &Path) -> Option<String> {
    let out = Command::new("git")
        .current_dir(repo)
        .args(["branch", "--format=%(refname:short)"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|s| s.to_string())
}

fn git_global_config_get(key: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["config", "--global", "--get", key])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ContainerBackendKind, GroveConfig};

    #[test]
    fn empty_config_defaults_to_devcontainer() {
        let cfg: GroveConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.container.backend, ContainerBackendKind::Devcontainer);
    }

    #[test]
    fn sandbox_backend_parses_from_config() {
        let cfg: GroveConfig = toml::from_str("[container]\nbackend = \"sandbox\"\n").unwrap();
        assert_eq!(cfg.container.backend, ContainerBackendKind::Sandbox);
    }

    #[test]
    fn unknown_section_keys_do_not_break_default() {
        // A config that predates the [container] section still loads with the
        // devcontainer default, so existing projects are unaffected.
        let cfg: GroveConfig =
            toml::from_str("[devcontainer]\nremote_user = \"vscode\"\n").unwrap();
        assert_eq!(cfg.container.backend, ContainerBackendKind::Devcontainer);
    }

    #[test]
    fn sandbox_section_parses_image_and_user() {
        let cfg: GroveConfig = toml::from_str(
            "[container]\nbackend = \"sandbox\"\n[sandbox]\nimage = \"img:tag\"\nuser = \"agent\"\n",
        )
        .unwrap();
        assert_eq!(cfg.sandbox.image.as_deref(), Some("img:tag"));
        assert_eq!(cfg.sandbox.user.as_deref(), Some("agent"));
    }

    #[test]
    fn container_name_is_deterministic_and_path_keyed() {
        let a = sandbox_container_name(Path::new("/home/u/proj"));
        let a2 = sandbox_container_name(Path::new("/home/u/proj"));
        let b = sandbox_container_name(Path::new("/home/u/other"));
        assert_eq!(a, a2, "same path must hash stably across calls");
        assert_ne!(a, b, "different paths must get different sandboxes");
        assert!(a.starts_with("grove-sb-"));
    }

    #[test]
    fn docker_argv_override_splits_tokens() {
        let argv = docker_argv_with(Some("podman --cgroup-manager cgroupfs"));
        assert_eq!(argv[0], "podman");
        assert!(argv.iter().any(|t| t == "cgroupfs"));
    }

    #[test]
    fn docker_argv_default_is_docker() {
        assert_eq!(docker_argv_with(None), vec!["docker".to_string()]);
        assert_eq!(docker_argv_with(Some("   ")), vec!["docker".to_string()]);
    }

    #[test]
    fn build_exec_args_runs_as_host_uid_in_workspace() {
        let args = build_exec_args("grove-sb-x", Path::new("/home/u/proj"), &["git", "status"]);
        assert_eq!(args[0], "exec");
        assert_eq!(args[1], "-u");
        // -w <root>
        let w = args.iter().position(|a| a == "-w").unwrap();
        assert_eq!(args[w + 1], "/home/u/proj");
        // container name precedes the command
        let name_idx = args.iter().position(|a| a == "grove-sb-x").unwrap();
        assert_eq!(args[name_idx + 1], "git");
        assert_eq!(args.last().unwrap(), "status");
    }

    #[test]
    fn build_create_args_binds_only_grove_and_overrides_entrypoint() {
        let args = build_create_args(
            "grove-sb-x",
            Path::new("/home/u/proj"),
            "img:tag",
            "/sbhome",
            Some("ghp_test"),
            &[(
                "/host/.claude/settings.json".to_string(),
                "/sbhome/.claude/settings.json".to_string(),
                true,
            )],
        );
        assert_eq!(args[0], "run");
        assert!(args.contains(&"-d".to_string()));
        // entrypoint override + sleep infinity
        let ep = args.iter().position(|a| a == "--entrypoint").unwrap();
        assert_eq!(args[ep + 1], "sleep");
        assert_eq!(args.last().unwrap(), "infinity");
        // image sits just before `infinity`
        let inf = args.iter().position(|a| a == "infinity").unwrap();
        assert_eq!(args[inf - 1], "img:tag");
        // bind mount targets exactly <root>/.grove and nothing else
        let mount = args
            .iter()
            .position(|a| a == "--mount")
            .map(|i| &args[i + 1])
            .unwrap();
        assert!(mount.contains("source=/home/u/proj/.grove"));
        assert!(mount.contains("target=/home/u/proj/.grove"));
        // GH token forwarded
        assert!(args.iter().any(|a| a == "GH_TOKEN=ghp_test"));
        // extra claude mount present + readonly
        assert!(args
            .iter()
            .any(|a| a.contains("/sbhome/.claude/settings.json") && a.contains("readonly")));
    }

    #[test]
    fn build_create_args_omits_gh_token_when_absent() {
        let args = build_create_args("grove-sb-x", Path::new("/p"), "img", "/sbhome", None, &[]);
        assert!(!args.iter().any(|a| a.starts_with("GH_TOKEN=")));
    }
}
