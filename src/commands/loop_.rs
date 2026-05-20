// `grove loop [--watch] [--agent <name>]` — inspect Ralph loop state.
//
// One-shot mode prints a compact table once and exits.
// --watch uses `notify` to re-print whenever any loop.md changes; survives
// missing files so the table updates as new agents spawn.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::time::Duration;

use colored::Colorize;
use notify::{Event, RecursiveMode, Watcher};

use crate::agent::loop_md;
use crate::git::worktree_manager::{discover_repo, project_root};

pub fn run(filter: Option<&str>, watch: bool) {
    let ctx = match discover_repo() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} {}", "Error:".red(), e);
            std::process::exit(1);
        }
    };
    let agents_dir = project_root(&ctx).join(".grove").join("agents");
    print_snapshot(&agents_dir, filter);

    if !watch {
        return;
    }

    let (tx, rx) = channel::<notify::Result<Event>>();
    let tx_clone = tx.clone();
    let mut watcher = match notify::recommended_watcher(move |res| {
        let _ = tx_clone.send(res);
    }) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("{} watcher: {}", "Error:".red(), e);
            std::process::exit(1);
        }
    };
    if let Err(e) = watcher.watch(&agents_dir, RecursiveMode::Recursive) {
        eprintln!("{} watch {}: {}", "Error:".red(), agents_dir.display(), e);
        std::process::exit(1);
    }
    println!();
    println!("{}", "watching for changes (Ctrl+C to exit)…".dimmed());
    loop {
        // Coalesce bursts: take everything queued within a 250ms window before
        // redrawing.
        match rx.recv() {
            Ok(_) => {
                while rx.recv_timeout(Duration::from_millis(250)).is_ok() {}
                println!();
                print_snapshot(&agents_dir, filter);
            }
            Err(_) => break,
        }
    }
}

fn print_snapshot(agents_dir: &Path, filter: Option<&str>) {
    let rows = collect(agents_dir);
    if rows.is_empty() {
        println!("(no loops found in {})", agents_dir.display());
        return;
    }
    println!(
        "{:<24} {:<10} {:<8} {:<12} PROMISE",
        "AGENT".bold(),
        "ACTIVE".bold(),
        "ITER".bold(),
        "PROGRESS".bold()
    );
    for row in rows {
        if let Some(f) = filter {
            if row.name != f {
                continue;
            }
        }
        let active_word = if row.state.active {
            "running".green().to_string()
        } else {
            "paused".yellow().to_string()
        };
        let max = row.state.max_iterations;
        let progress = if max == 0 {
            "—".to_string()
        } else {
            format!("{}/{}", row.state.iteration, max)
        };
        println!(
            "{:<24} {:<10} {:<8} {:<12} {}",
            row.name, active_word, row.state.iteration, progress, row.state.completion_promise
        );
    }
}

struct LoopRow {
    name: String,
    state: crate::models::LoopState,
}

fn collect(agents_dir: &Path) -> Vec<LoopRow> {
    let mut rows = Vec::new();
    let read = match fs::read_dir(agents_dir) {
        Ok(r) => r,
        Err(_) => return rows,
    };
    for entry in read.flatten() {
        let path: PathBuf = entry.path();
        if !path.is_dir() {
            continue;
        }
        let loop_md_path = path.join("loop.md");
        if !loop_md_path.exists() {
            continue;
        }
        let state = match loop_md::read_loop_md(&loop_md_path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        rows.push(LoopRow { name, state });
    }
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}
