// `grove agents <list|status|kill>` — full impl lands in T10.

use colored::Colorize;

pub fn list() {
    eprintln!(
        "{} grove agents not yet implemented on this branch (T10).",
        "Note:".yellow()
    );
    std::process::exit(1);
}

pub fn status(_name: &str) {
    list();
}

pub fn kill(_name: &str) {
    list();
}
