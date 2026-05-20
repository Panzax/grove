// `loop.md` frontmatter parsing.
//
// Lives next to the bash Stop-hook engine which also reads the same file. The Rust
// side reads it for `grove loop`, `grove agents status`, etc; the bash side reads it
// to decide whether to re-inject the prompt.
//
// Wire format (YAML between two `---` lines):
//
//     ---
//     active: true
//     iteration: 0
//     max_iterations: 30
//     completion_promise: "ALL TESTS PASS"
//     session_id: ""
//     ---
//     <prompt body>
//
// We don't pull in a YAML crate — the schema is fixed and tiny, so a hand-rolled
// parser is the right tool. This also matches what the bash engine does
// (sed/awk-style line probing), which means write-back from Rust stays compatible.

#![allow(dead_code)]

use std::path::Path;

use crate::models::LoopState;

pub fn read_loop_md(path: &Path) -> Result<LoopState, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    parse(&raw)
}

pub fn write_loop_md(path: &Path, state: &LoopState) -> Result<(), String> {
    let body = serialize(state);
    std::fs::write(path, body).map_err(|e| format!("write {}: {}", path.display(), e))?;
    Ok(())
}

pub fn parse(raw: &str) -> Result<LoopState, String> {
    let mut state = LoopState {
        active: false,
        iteration: 0,
        max_iterations: 30,
        completion_promise: String::new(),
        session_id: String::new(),
        status: None,
        last_action: None,
        last_updated: None,
        body: String::new(),
    };

    // Frontmatter block delimited by lines containing only `---`.
    let mut lines = raw.lines();
    match lines.next() {
        Some(l) if l.trim() == "---" => {}
        _ => return Err("loop.md missing opening --- frontmatter delimiter".into()),
    }
    let mut body_start = 0usize;
    let mut bytes = raw.find('\n').map(|n| n + 1).unwrap_or(raw.len());
    for line in lines {
        if line.trim() == "---" {
            body_start = bytes + line.len() + 1;
            break;
        }
        parse_frontmatter_line(line, &mut state);
        bytes += line.len() + 1;
    }

    if body_start < raw.len() {
        state.body = raw[body_start..].trim_start_matches('\n').to_string();
    }
    Ok(state)
}

fn parse_frontmatter_line(line: &str, state: &mut LoopState) {
    let trimmed = line.trim();
    let Some((key, value)) = trimmed.split_once(':') else {
        return;
    };
    let key = key.trim();
    let value = value.trim().trim_matches('"').trim();
    match key {
        "active" => state.active = value == "true",
        "iteration" => {
            if let Ok(n) = value.parse() {
                state.iteration = n;
            }
        }
        "max_iterations" => {
            if let Ok(n) = value.parse() {
                state.max_iterations = n;
            }
        }
        "completion_promise" => state.completion_promise = value.to_string(),
        "session_id" => state.session_id = value.to_string(),
        "last_action" => state.last_action = Some(value.to_string()),
        _ => {}
    }
}

pub fn serialize(state: &LoopState) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("active: {}\n", state.active));
    out.push_str(&format!("iteration: {}\n", state.iteration));
    out.push_str(&format!("max_iterations: {}\n", state.max_iterations));
    out.push_str(&format!(
        "completion_promise: \"{}\"\n",
        state.completion_promise.replace('"', "\\\"")
    ));
    out.push_str(&format!(
        "session_id: \"{}\"\n",
        state.session_id.replace('"', "\\\"")
    ));
    if let Some(last) = &state.last_action {
        out.push_str(&format!("last_action: \"{}\"\n", last.replace('"', "\\\"")));
    }
    out.push_str("---\n");
    out.push_str(&state.body);
    if !state.body.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_then_serialize_roundtrip() {
        let raw = "---\nactive: true\niteration: 4\nmax_iterations: 30\ncompletion_promise: \"DONE\"\nsession_id: \"abc\"\n---\nbody line one\nbody line two\n";
        let state = parse(raw).unwrap();
        assert!(state.active);
        assert_eq!(state.iteration, 4);
        assert_eq!(state.max_iterations, 30);
        assert_eq!(state.completion_promise, "DONE");
        assert_eq!(state.session_id, "abc");
        let again = serialize(&state);
        let reparsed = parse(&again).unwrap();
        assert_eq!(reparsed.active, state.active);
        assert_eq!(reparsed.iteration, state.iteration);
        assert_eq!(reparsed.completion_promise, state.completion_promise);
        assert_eq!(reparsed.session_id, state.session_id);
    }

    #[test]
    fn parse_inactive() {
        let raw = "---\nactive: false\niteration: 0\nmax_iterations: 10\ncompletion_promise: \"\"\nsession_id: \"\"\n---\n";
        let state = parse(raw).unwrap();
        assert!(!state.active);
        assert_eq!(state.iteration, 0);
    }

    #[test]
    fn parse_rejects_missing_frontmatter() {
        let raw = "active: true\n";
        assert!(parse(raw).is_err());
    }
}
