// Stop-hook installer.
//
// Idempotently injects a single hook command into Claude Code's user-level
// settings.json. The hook command shells out to `.grove/tools/loop-hook.sh` and
// no-ops if `GROVE_AGENT_DIR` is unset, so registering once at the user level
// covers every project + every worktree without firing on unrelated Claude
// sessions.
//
// Matches the freqtrade harness `ensure-ralph-hook.sh` (jq script) but stays in
// Rust so we have one canonical implementation.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::git::worktree_manager::{project_root, RepoContext};

/// The shell snippet stored under `hooks.Stop[].hooks[].command`. The literal
/// `$CLAUDE_PROJECT_DIR` is intentionally preserved (single-quote outer string in
/// JSON) so Claude Code substitutes the project root at hook-fire time.
pub const HOOK_COMMAND: &str =
    r#"H="$CLAUDE_PROJECT_DIR/.grove/tools/loop-hook.sh"; [ -x "$H" ] && bash "$H" || exit 0"#;

pub const LOOP_HOOK_BYTES: &str = include_str!("../../assets/loop-hook.sh");

/// Write the bash engine into `<project_root>/.grove/tools/loop-hook.sh` and chmod +x.
/// Idempotent: overwrites the existing file (the asset is the source of truth — users
/// who want to customize the engine should fork grove).
pub fn install_engine(ctx: &RepoContext) -> Result<PathBuf, String> {
    let dir = project_root(ctx).join(".grove").join("tools");
    fs::create_dir_all(&dir).map_err(|e| format!("create .grove/tools/: {}", e))?;
    let path = dir.join("loop-hook.sh");
    fs::write(&path, LOOP_HOOK_BYTES).map_err(|e| format!("write loop-hook.sh: {}", e))?;
    set_executable(&path)?;
    Ok(path)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .map_err(|e| format!("stat {}: {}", path.display(), e))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).map_err(|e| format!("chmod {}: {}", path.display(), e))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), String> {
    // Windows: bash hook will run under WSL where the +x bit doesn't transit;
    // bash invokes the script via `bash $H` so the +x bit isn't load-bearing.
    Ok(())
}

/// Locate the user-level `~/.claude/settings.json`. Honors `CLAUDE_HOME` if set.
pub fn default_user_settings_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_HOME") {
        return Some(PathBuf::from(dir).join("settings.json"));
    }
    let home = dirs::home_dir()?;
    Some(home.join(".claude").join("settings.json"))
}

/// Idempotently inject the Stop hook command into the given settings.json.
///
/// - Creates parent dirs and the file if missing (empty {}).
/// - Reads existing JSON if present; refuses if the top-level isn't an object.
/// - Adds `hooks.Stop[]` with `{ "hooks": [ { "type": "command", "command": HOOK_COMMAND } ] }`
///   only when the exact `command` isn't already registered.
/// - Pretty-prints the result, preserves every other top-level key untouched.
pub fn install_stop_hook(
    settings_path: &Path,
    hook_command: &str,
) -> Result<HookInstallReport, String> {
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {}", parent.display(), e))?;
    }

    let mut value: Value = if settings_path.exists() {
        let raw = fs::read_to_string(settings_path)
            .map_err(|e| format!("read {}: {}", settings_path.display(), e))?;
        if raw.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&raw)
                .map_err(|e| format!("parse {}: {}", settings_path.display(), e))?
        }
    } else {
        json!({})
    };

    let obj = value
        .as_object_mut()
        .ok_or_else(|| format!("{} top-level is not a JSON object", settings_path.display()))?;

    // Ensure hooks.Stop is an array.
    let hooks_val = obj.entry("hooks").or_insert_with(|| json!({}));
    let hooks_obj = hooks_val
        .as_object_mut()
        .ok_or_else(|| "settings.json `hooks` is not an object".to_string())?;
    let stop_val = hooks_obj.entry("Stop").or_insert_with(|| json!([]));
    let stop_arr = stop_val
        .as_array_mut()
        .ok_or_else(|| "settings.json `hooks.Stop` is not an array".to_string())?;

    // Already registered?
    let already_present = stop_arr.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|cmds| {
                cmds.iter()
                    .any(|c| c.get("command").and_then(|v| v.as_str()) == Some(hook_command))
            })
            .unwrap_or(false)
    });

    let mut report = HookInstallReport {
        path: settings_path.to_path_buf(),
        added: false,
        total_stop_hooks: 0,
    };

    if !already_present {
        stop_arr.push(json!({
            "hooks": [ { "type": "command", "command": hook_command } ]
        }));
        report.added = true;
    }
    report.total_stop_hooks = stop_arr.len();

    let body = serde_json::to_string_pretty(&value)
        .map_err(|e| format!("serialize {}: {}", settings_path.display(), e))?;
    fs::write(settings_path, body)
        .map_err(|e| format!("write {}: {}", settings_path.display(), e))?;
    Ok(report)
}

#[derive(Debug, Clone)]
pub struct HookInstallReport {
    pub path: PathBuf,
    pub added: bool,
    pub total_stop_hooks: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("grove-hook-test-{}", name));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn install_creates_file_when_missing() {
        let dir = tmp("missing");
        let path = dir.join("settings.json");
        let report = install_stop_hook(&path, HOOK_COMMAND).unwrap();
        assert!(report.added);
        assert_eq!(report.total_stop_hooks, 1);
        let raw = fs::read_to_string(&path).unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert!(v["hooks"]["Stop"].is_array());
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], HOOK_COMMAND);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_is_idempotent() {
        let dir = tmp("idem");
        let path = dir.join("settings.json");
        let r1 = install_stop_hook(&path, HOOK_COMMAND).unwrap();
        let r2 = install_stop_hook(&path, HOOK_COMMAND).unwrap();
        assert!(r1.added);
        assert!(!r2.added);
        assert_eq!(r1.total_stop_hooks, 1);
        assert_eq!(r2.total_stop_hooks, 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_preserves_other_keys() {
        let dir = tmp("preserve");
        let path = dir.join("settings.json");
        let pre = json!({
            "theme": "dark",
            "enabledPlugins": ["caveman:caveman"],
            "hooks": {
                "PreToolUse": [{
                    "hooks": [{ "type": "command", "command": "echo pre" }]
                }]
            }
        });
        fs::write(&path, serde_json::to_string_pretty(&pre).unwrap()).unwrap();

        install_stop_hook(&path, HOOK_COMMAND).unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["enabledPlugins"][0], "caveman:caveman");
        assert_eq!(
            v["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "echo pre"
        );
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], HOOK_COMMAND);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_rejects_non_object_root() {
        let dir = tmp("bad");
        let path = dir.join("settings.json");
        fs::write(&path, "[1,2,3]").unwrap();
        assert!(install_stop_hook(&path, HOOK_COMMAND).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn loop_hook_asset_is_embedded() {
        // include_str! pulled in the freqtrade-derived script.
        assert!(LOOP_HOOK_BYTES.starts_with("#!/bin/bash"));
        assert!(LOOP_HOOK_BYTES.contains("GROVE_AGENT_DIR"));
        assert!(!LOOP_HOOK_BYTES.contains("RALPH_DIR")); // fully renamed
    }
}
