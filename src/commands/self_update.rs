use colored::Colorize;

use crate::utils::get_self_update_command;

pub fn run(version: Option<&str>, pr: Option<u64>) {
    // Fork note: upstream's self-update hits https://i.safia.sh (captainsafia's hosted
    // install endpoint) which does not serve this fork's binaries. Until Panzax/grove
    // ships a hosted install script of its own, this command prints a notice rather
    // than attempting a download. `get_self_update_command` is preserved for upstream
    // unit-test compatibility.
    let base_url = "https://github.com/Panzax/grove/releases";
    let install_url = if let Some(pr_num) = pr {
        format!("{}/pr/{}", base_url, pr_num)
    } else if let Some(ver) = version {
        let version_tag = if ver.starts_with('v') {
            ver.to_string()
        } else {
            format!("v{}", ver)
        };
        format!("{}/{}", base_url, version_tag)
    } else {
        base_url.to_string()
    };

    eprintln!(
        "{} grove self-update is not available on this fork yet.",
        "Note:".yellow()
    );
    eprintln!(
        "       Build from source: cargo install --git {}",
        "https://github.com/Panzax/grove"
    );
    eprintln!("       Target URL was: {}", install_url);

    // Keep helper referenced to avoid an unused-import warning; behavior gated off.
    let _keepalive: fn(&str) -> (String, Vec<String>) = get_self_update_command;
    let _ = _keepalive;
}
