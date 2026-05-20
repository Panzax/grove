// `grove msg <to|broadcast> "<text>"` — write a message to the bus.

use std::path::PathBuf;

use colored::Colorize;

use crate::bus;
use crate::git::worktree_manager::{discover_repo, project_root};
use crate::models::MessageKind;

pub fn run(to: &str, text: &str, from: Option<&str>, contract_name: Option<&str>) {
    let ctx = match discover_repo() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "{} grove msg must run inside a grove-initialized project: {}",
                "Error:".red(),
                e
            );
            std::process::exit(1);
        }
    };

    // Default sender = $GROVE_AGENT_NAME if set (we set this in `grove spawn`), else "human".
    let sender = from
        .map(|s| s.to_string())
        .or_else(|| std::env::var("GROVE_AGENT_NAME").ok())
        .unwrap_or_else(|| "human".to_string());

    let bus_root: PathBuf = project_root(&ctx).join(".grove").join("bus");

    let kind = if contract_name.is_some() {
        MessageKind::Contract
    } else if to == "broadcast" || to == "*" {
        MessageKind::Broadcast
    } else {
        MessageKind::Direct
    };

    let mut msg = bus::new_message(&sender, to, kind, text);
    if let Some(name) = contract_name {
        msg.contract = Some(name.to_string());
    }

    match bus::send(&bus_root, &msg) {
        Ok(path) => {
            println!(
                "{} {} {} -> {} ({})",
                "✓".green(),
                kind.as_str(),
                sender,
                to,
                path.display()
            );
        }
        Err(e) => {
            eprintln!("{} {}", "Error:".red(), e);
            std::process::exit(1);
        }
    }
}
