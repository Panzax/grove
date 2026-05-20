// Stop-hook installer for Claude Code's user-level settings.json.
//
// Filled out in T4.

#![allow(dead_code)]

use std::path::Path;

pub fn install_stop_hook(_settings_path: &Path, _hook_command: &str) -> Result<(), String> {
    Err("hook installer not yet implemented (T4)".to_string())
}
