// Phase 2 of `grove init` — interactive setup wizard.
//
// Full implementation lands in T13. For T3 this is a deferred-no-op so init.rs
// compiles and produces a Phase-1-complete project.

use colored::Colorize;

use crate::git::worktree_manager::RepoContext;
use crate::models::ProjectContext;

/// Run the Phase 2 wizard. `is_reconfigure` is true when invoked via
/// `grove init --reconfigure` (so we re-prompt with current values as defaults).
///
/// Until T13 lands this prints a notice and returns Ok(()). The fork's Phase 1
/// output is fully functional without Phase 2; the wizard is a refinement layer.
pub fn run_setup_wizard(
    _ctx: &RepoContext,
    _project: &ProjectContext,
    is_reconfigure: bool,
) -> Result<(), String> {
    if is_reconfigure {
        println!(
            "{} the Phase 2 setup wizard is not implemented yet on this branch.",
            "Note:".yellow()
        );
        println!(
            "       Phase 1 output (devcontainer skeleton + .grove/config.toml) is in place;"
        );
        println!("       you can edit those files by hand for now.");
    } else {
        println!(
            "  {} Phase 2 setup wizard not yet wired (see T13 in plan); Phase 1 done.",
            "·".dimmed()
        );
    }
    Ok(())
}
