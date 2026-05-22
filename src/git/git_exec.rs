// Git execution dispatcher — the single seam that routes a `git` invocation to
// the host or into the project's sandbox container.
//
// In the default (devcontainer) backend the host bind-mounts the worktree, so
// running git on the host directly affects what the container sees — git stays
// on the host exactly as before. In the sandbox backend the code is copied in
// (not mounted), so the seeded repo and every worktree live INSIDE the
// container; worktree/branch/merge ops must run there via `docker exec`. The
// identical-path model means the same `cwd`/`-C` path is valid on both sides,
// so call sites pass host paths unchanged.
//
// Every git call site that touches *live repository state* (as opposed to
// init-time bookkeeping against the host bare clone) funnels through `run`.

use std::path::Path;
use std::process::Output;

use crate::session::backend;
use crate::session::container;

/// Run `git -C <cwd> <args>` against the right target for `project_root`:
/// inside the sandbox container when the project is sandbox-backed, on the host
/// otherwise. Returns the raw `Output` so callers keep their existing
/// success/stderr handling.
pub fn run(project_root: &Path, cwd: &Path, args: &[&str]) -> Result<Output, String> {
    if backend::project_is_sandbox(project_root) {
        let info = backend::sandbox_info(project_root);
        let cwd_str = cwd.to_string_lossy().to_string();
        let mut full: Vec<&str> = vec!["git", "-C", &cwd_str];
        full.extend_from_slice(args);
        container::exec(&info, &full)
    } else {
        std::process::Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .map_err(|e| format!("Failed to execute git: {}", e))
    }
}
