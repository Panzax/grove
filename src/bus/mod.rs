// File-based collaboration bus.
//
// Layout (all runtime-created, gitignored — see init's gitignore patch):
//
//   .grove/bus/
//     log.d/<ISO8601>-<sender>.md         broadcast feed
//     inbox/<recipient>/                  direct mail
//       <ISO8601>-from-<sender>.md
//       archive/                          recipient mv's read mail here
//     contracts/<feature-pair>.md         negotiated interface agreements
//
// One file per event. Atomic file create eliminates the multi-writer append race;
// readers glob-by-mtime to pull new mail. Each message file is a Markdown body
// with a small YAML frontmatter block carrying metadata. The format is the same
// one humans can write by hand if they want to inject test traffic.

#![allow(dead_code)] // public bus API; some helpers are only invoked from commands
                     // we haven't fully wired yet (e.g. `grove agents status` reading
                     // the inbox for a given recipient).

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::models::{BusMessage, MessageKind};

/// Default bus root (used when GroveConfig.bus.dir is empty).
pub const DEFAULT_BUS_DIR: &str = ".grove/bus";

/// Compute the on-disk path for a message.
pub fn message_path(bus_root: &Path, msg: &BusMessage) -> PathBuf {
    let ts = msg.ts.format("%Y%m%dT%H%M%S%3fZ").to_string();
    match msg.kind {
        MessageKind::Broadcast | MessageKind::Status => {
            bus_root
                .join("log.d")
                .join(format!("{}-{}.md", ts, slug(&msg.from)))
        }
        MessageKind::Direct => bus_root.join("inbox").join(slug(&msg.to)).join(format!(
            "{}-from-{}.md",
            ts,
            slug(&msg.from)
        )),
        MessageKind::Contract => bus_root.join("contracts").join(format!(
            "{}.md",
            msg.contract.as_deref().map(slug).unwrap_or_else(|| format!(
                "{}-{}-{}",
                ts,
                slug(&msg.from),
                slug(&msg.to)
            ))
        )),
    }
}

/// Append-by-create: write a single message file. Fails fast if the file
/// already exists (timestamp + sender slug collisions are vanishingly rare;
/// callers can retry with `now()` to get a fresh stamp).
pub fn send(bus_root: &Path, msg: &BusMessage) -> Result<PathBuf, String> {
    let path = message_path(bus_root, msg);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {}", parent.display(), e))?;
    }
    if path.exists() && msg.kind != MessageKind::Contract {
        return Err(format!(
            "bus message file already exists at {}",
            path.display()
        ));
    }
    let body = format_message(msg);
    fs::write(&path, body).map_err(|e| format!("write {}: {}", path.display(), e))?;
    Ok(path)
}

/// Read every broadcast in `log.d/` newer than `since` (Unix ms, 0 = all).
pub fn read_log(bus_root: &Path, since_unix_ms: i64) -> Result<Vec<BusMessage>, String> {
    let dir = bus_root.join("log.d");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| format!("read_dir {}: {}", dir.display(), e))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let raw = match fs::read_to_string(&path) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let msg = parse_message(&raw, MessageKind::Broadcast)?;
        if since_unix_ms == 0 || msg.ts.timestamp_millis() > since_unix_ms {
            out.push(msg);
        }
    }
    out.sort_by_key(|m| m.ts);
    Ok(out)
}

/// Read every direct-mail file in `inbox/<recipient>/` (excluding `archive/`).
pub fn read_inbox(bus_root: &Path, recipient: &str) -> Result<Vec<BusMessage>, String> {
    let dir = bus_root.join("inbox").join(slug(recipient));
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| format!("read_dir {}: {}", dir.display(), e))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            continue; // skip archive/
        }
        let raw = match fs::read_to_string(&path) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let msg = parse_message(&raw, MessageKind::Direct)?;
        out.push(msg);
    }
    out.sort_by_key(|m| m.ts);
    Ok(out)
}

/// Move all inbox files older than `now` into `inbox/<recipient>/archive/`.
/// Idempotent. Used by agents on each loop iteration after they've processed mail.
pub fn archive_inbox(bus_root: &Path, recipient: &str) -> Result<usize, String> {
    let dir = bus_root.join("inbox").join(slug(recipient));
    if !dir.exists() {
        return Ok(0);
    }
    let archive = dir.join("archive");
    fs::create_dir_all(&archive).map_err(|e| format!("create {}: {}", archive.display(), e))?;
    let mut moved = 0;
    for entry in fs::read_dir(&dir).map_err(|e| format!("read_dir {}: {}", dir.display(), e))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        let dest = archive.join(name);
        fs::rename(&path, &dest)
            .map_err(|e| format!("rename {} -> {}: {}", path.display(), dest.display(), e))?;
        moved += 1;
    }
    Ok(moved)
}

/// Write the wire format: small YAML frontmatter + Markdown body.
pub fn format_message(msg: &BusMessage) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("id: \"{}\"\n", msg.id));
    out.push_str(&format!("from: \"{}\"\n", msg.from));
    out.push_str(&format!("to: \"{}\"\n", msg.to));
    out.push_str(&format!(
        "ts: \"{}\"\n",
        msg.ts.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    ));
    out.push_str(&format!("kind: \"{}\"\n", msg.kind.as_str()));
    if let Some(c) = &msg.contract {
        out.push_str(&format!("contract: \"{}\"\n", c));
    }
    out.push_str("---\n");
    out.push_str(&msg.body);
    if !msg.body.ends_with('\n') {
        out.push('\n');
    }
    out
}

pub fn parse_message(raw: &str, fallback_kind: MessageKind) -> Result<BusMessage, String> {
    let mut id = String::new();
    let mut from = String::new();
    let mut to = String::new();
    let mut ts_str = String::new();
    let mut kind_str = fallback_kind.as_str().to_string();
    let mut contract: Option<String> = None;
    let mut body = String::new();

    let mut iter = raw.lines();
    let first = iter.next().unwrap_or("");
    if first.trim() != "---" {
        return Err("bus message missing opening --- frontmatter delimiter".into());
    }
    let mut closed = false;
    let mut consumed = first.len() + 1;
    for line in iter.by_ref() {
        if line.trim() == "---" {
            closed = true;
            consumed += line.len() + 1;
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim().trim_matches('"').to_string();
            match k.trim() {
                "id" => id = v,
                "from" => from = v,
                "to" => to = v,
                "ts" => ts_str = v,
                "kind" => kind_str = v,
                "contract" => contract = Some(v),
                _ => {}
            }
        }
        consumed += line.len() + 1;
    }
    if !closed {
        return Err("bus message missing closing --- frontmatter delimiter".into());
    }
    if consumed < raw.len() {
        body = raw[consumed..].trim_start_matches('\n').to_string();
    }

    let ts = chrono::DateTime::parse_from_rfc3339(&ts_str)
        .map(|t| t.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| Utc::now());
    let kind = match kind_str.as_str() {
        "broadcast" => MessageKind::Broadcast,
        "direct" => MessageKind::Direct,
        "contract" => MessageKind::Contract,
        "status" => MessageKind::Status,
        _ => fallback_kind,
    };
    Ok(BusMessage {
        id,
        from,
        to,
        ts,
        kind,
        body,
        contract,
    })
}

fn slug(input: &str) -> String {
    let raw: String = input
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else if c == '.' {
                // single dots are fine, but leading/trailing get stripped below to
                // prevent path traversal slugs like ".." or ".hidden"
                '.'
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = raw.trim_matches(|c: char| c == '-' || c == '.');
    trimmed.to_string()
}

/// Build a fresh BusMessage with `Utc::now()` and a v4 UUID.
pub fn new_message(from: &str, to: &str, kind: MessageKind, body: impl Into<String>) -> BusMessage {
    BusMessage {
        id: uuid::Uuid::new_v4().to_string(),
        from: from.to_string(),
        to: to.to_string(),
        ts: Utc::now(),
        kind,
        body: body.into(),
        contract: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("grove-bus-test-{}", name));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn send_broadcast_writes_log_d_file() {
        let dir = tmp("send-bcast");
        let msg = new_message("feat-a", "broadcast", MessageKind::Broadcast, "hello world");
        let path = send(&dir, &msg).unwrap();
        assert!(path.starts_with(dir.join("log.d")));
        assert!(path.exists());
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.starts_with("---\n"));
        assert!(raw.contains("from: \"feat-a\""));
        assert!(raw.contains("hello world"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn send_direct_writes_inbox_file() {
        let dir = tmp("send-direct");
        let msg = new_message("feat-a", "feat-b", MessageKind::Direct, "ping");
        let path = send(&dir, &msg).unwrap();
        assert!(path.starts_with(dir.join("inbox").join("feat-b")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_log_returns_chronological_order() {
        let dir = tmp("read-log");
        let mut a = new_message("feat-a", "broadcast", MessageKind::Broadcast, "first");
        a.ts = chrono::Utc::now() - chrono::Duration::seconds(10);
        let mut b = new_message("feat-b", "broadcast", MessageKind::Broadcast, "second");
        b.ts = chrono::Utc::now();
        send(&dir, &a).unwrap();
        send(&dir, &b).unwrap();
        let msgs = read_log(&dir, 0).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].body.trim(), "first");
        assert_eq!(msgs[1].body.trim(), "second");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_log_respects_since() {
        let dir = tmp("read-log-since");
        let mut older = new_message("feat-a", "broadcast", MessageKind::Broadcast, "old");
        older.ts = chrono::Utc::now() - chrono::Duration::hours(1);
        let newer = new_message("feat-a", "broadcast", MessageKind::Broadcast, "new");
        send(&dir, &older).unwrap();
        send(&dir, &newer).unwrap();
        let cutoff = chrono::Utc::now()
            .checked_sub_signed(chrono::Duration::minutes(30))
            .unwrap()
            .timestamp_millis();
        let msgs = read_log(&dir, cutoff).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].body.trim(), "new");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_inbox_and_archive_round_trip() {
        let dir = tmp("inbox");
        let m1 = new_message("feat-a", "feat-b", MessageKind::Direct, "msg1");
        std::thread::sleep(std::time::Duration::from_millis(5));
        let m2 = new_message("feat-a", "feat-b", MessageKind::Direct, "msg2");
        send(&dir, &m1).unwrap();
        send(&dir, &m2).unwrap();

        let read = read_inbox(&dir, "feat-b").unwrap();
        assert_eq!(read.len(), 2);

        let moved = archive_inbox(&dir, "feat-b").unwrap();
        assert_eq!(moved, 2);
        let read_again = read_inbox(&dir, "feat-b").unwrap();
        assert!(read_again.is_empty());
        let archived = fs::read_dir(dir.join("inbox/feat-b/archive"))
            .unwrap()
            .count();
        assert_eq!(archived, 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_round_trip_preserves_metadata() {
        let original = new_message("a", "b", MessageKind::Direct, "body line\nsecond line");
        let raw = format_message(&original);
        let parsed = parse_message(&raw, MessageKind::Direct).unwrap();
        assert_eq!(parsed.from, "a");
        assert_eq!(parsed.to, "b");
        assert_eq!(parsed.kind, MessageKind::Direct);
        assert_eq!(parsed.body.trim(), "body line\nsecond line");
    }

    #[test]
    fn contract_writes_under_contracts_dir() {
        let dir = tmp("contract");
        let mut msg = new_message("feat-a", "feat-b", MessageKind::Contract, "interface body");
        msg.contract = Some("feat-a-feat-b".to_string());
        let path = send(&dir, &msg).unwrap();
        assert!(path.starts_with(dir.join("contracts")));
        assert!(path.ends_with("feat-a-feat-b.md"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn slug_strips_unsafe_chars() {
        assert_eq!(slug("feat/a"), "feat-a");
        assert_eq!(slug("../escape"), "escape");
        assert_eq!(slug("alpha_beta.gamma"), "alpha_beta.gamma");
    }
}
