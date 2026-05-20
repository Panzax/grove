// Per-agent profile seeder.
//
// Writes `.grove/agents/<name>/{PROMPT,STATE,loop}.md` using `assets/PROMPT.template.md`
// with `<AGENT_NAME>` substituted.
//
// Filled out in T7.

#![allow(dead_code)]

use std::path::Path;

pub fn seed_agent(_project_root: &Path, _name: &str, _task: Option<&str>) -> Result<(), String> {
    Err("agent seeder not yet implemented (T7)".to_string())
}
