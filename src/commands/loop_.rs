// `grove loop [--watch] [--agent <name>]` — full impl lands in T11.

use colored::Colorize;

pub fn run(_agent: Option<&str>, _watch: bool) {
    eprintln!(
        "{} grove loop not yet implemented on this branch (T11).",
        "Note:".yellow()
    );
    std::process::exit(1);
}
