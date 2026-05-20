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
    let raw =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    parse(&raw)
}

pub fn write_loop_md(path: &Path, state: &LoopState) -> Result<(), String> {
    let body = serialize(state);
    std::fs::write(path, body).map_err(|e| format!("write {}: {}", path.display(), e))?;
    Ok(())
}

/// Clear the `session_id` field in a loop.md frontmatter. Called by
/// `grove spawn` on the RESUME path: the previous claude session crashed
/// or was killed, so its session_id in loop.md is stale. The Stop hook's
/// session-isolation guard would silently block the new session from
/// driving the loop if we left the stale value in place.
///
/// Idempotent: if the field is already empty (or loop.md is missing /
/// malformed), no-op.
pub fn clear_session_id(loop_path: &Path) -> Result<(), String> {
    if !loop_path.exists() {
        return Ok(());
    }
    let mut state = read_loop_md(loop_path)?;
    if state.session_id.is_empty() {
        return Ok(());
    }
    state.session_id.clear();
    write_loop_md(loop_path, &state)
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

    // Frontmatter block delimited by lines containing only `---`. Use line-based
    // splitting throughout so CRLF input doesn't desynchronize byte offsets.
    let normalized = raw.replace("\r\n", "\n");
    let mut lines = normalized.split('\n');
    match lines.next() {
        Some(l) if l.trim() == "---" => {}
        _ => return Err("loop.md missing opening --- frontmatter delimiter".into()),
    }
    let mut body_lines: Vec<&str> = Vec::new();
    let mut closed = false;
    let mut in_body = false;
    for line in lines {
        if in_body {
            body_lines.push(line);
        } else if line.trim() == "---" {
            in_body = true;
            closed = true;
        } else {
            parse_frontmatter_line(line, &mut state);
        }
    }
    if !closed {
        return Err("loop.md missing closing --- frontmatter delimiter".into());
    }
    state.body = body_lines.join("\n").trim_start_matches('\n').to_string();
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
        "last_updated" => {
            state.last_updated = chrono::DateTime::parse_from_rfc3339(value)
                .map(|t| t.with_timezone(&chrono::Utc))
                .ok();
        }
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
    if let Some(ts) = &state.last_updated {
        out.push_str(&format!(
            "last_updated: \"{}\"\n",
            ts.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        ));
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

    #[test]
    fn parse_handles_crlf_line_endings() {
        let raw = "---\r\nactive: true\r\niteration: 3\r\nmax_iterations: 30\r\ncompletion_promise: \"DONE\"\r\nsession_id: \"\"\r\n---\r\nbody one\r\nbody two\r\n";
        let state = parse(raw).unwrap();
        assert!(state.active);
        assert_eq!(state.iteration, 3);
        assert_eq!(state.completion_promise, "DONE");
        assert!(state.body.contains("body one"));
        assert!(state.body.contains("body two"));
    }

    #[test]
    fn clear_session_id_clears_non_empty_value() {
        let tmpdir = std::env::temp_dir().join(format!(
            "grove-clear-session-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&tmpdir).unwrap();
        let path = tmpdir.join("loop.md");
        std::fs::write(
            &path,
            "---\nactive: true\niteration: 4\nmax_iterations: 30\ncompletion_promise: \"DONE\"\nsession_id: \"stale-abc-123\"\n---\nbody\n",
        )
        .unwrap();
        clear_session_id(&path).unwrap();
        let after = read_loop_md(&path).unwrap();
        assert_eq!(after.session_id, "");
        // Other fields preserved.
        assert!(after.active);
        assert_eq!(after.iteration, 4);
        assert_eq!(after.completion_promise, "DONE");
        let _ = std::fs::remove_dir_all(&tmpdir);
    }

    #[test]
    fn clear_session_id_noop_if_already_empty() {
        let tmpdir = std::env::temp_dir().join(format!(
            "grove-clear-session-noop-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&tmpdir).unwrap();
        let path = tmpdir.join("loop.md");
        std::fs::write(
            &path,
            "---\nactive: false\niteration: 0\nmax_iterations: 30\ncompletion_promise: \"\"\nsession_id: \"\"\n---\n",
        )
        .unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        clear_session_id(&path).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(before, after);
        let _ = std::fs::remove_dir_all(&tmpdir);
    }

    #[test]
    fn clear_session_id_missing_file_is_ok() {
        let nonexistent = std::env::temp_dir().join(format!(
            "grove-no-such-loop-{}.md",
            uuid::Uuid::new_v4().simple()
        ));
        assert!(clear_session_id(&nonexistent).is_ok());
    }

    #[test]
    fn parse_last_updated_round_trip() {
        let now = chrono::Utc::now();
        let raw = format!(
            "---\nactive: true\niteration: 1\nmax_iterations: 10\ncompletion_promise: \"\"\nsession_id: \"\"\nlast_updated: \"{}\"\n---\n",
            now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        );
        let state = parse(&raw).unwrap();
        assert!(state.last_updated.is_some());
        let again = serialize(&state);
        assert!(again.contains("last_updated:"));
    }
}
