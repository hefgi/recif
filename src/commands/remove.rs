//! `recif remove` (PRD §8.1, §10).

use std::io::Write;

use anyhow::{anyhow, Result};
use colored::Colorize;

use crate::aliases;
use crate::cli::RemoveArgs;
use crate::config;
use crate::saferemove;

pub fn run(args: RemoveArgs) -> Result<()> {
    let (config_path, mut cfg) = crate::commands::load_config()?;

    let profile = cfg
        .profile(&args.name)
        .ok_or_else(|| anyhow!("no such profile '{}' in config", args.name))?
        .clone();

    // Mandatory confirmation prompt unless --yes (§8.1).
    if !args.yes && !confirm(&args.name, &profile.path.display().to_string())? {
        println!("Aborted.");
        return Ok(());
    }

    // Safe removal — never follows symlinks into master (§8.1, §10).
    let report = saferemove::remove_profile(&profile.path, args.keep_daemon)
        .map_err(|e| anyhow!("safe removal failed: {e}"))?;

    // Remove config entry + alias.
    cfg.remove_profile(&args.name);
    config::save(&config_path, &cfg)?;

    let rc = aliases::default_rc_file(args.rc.as_deref())?;
    let alias_removed = aliases::remove_alias(&rc, &args.name).unwrap_or(false);

    println!(
        "{} removed profile {} ({} link(s){})",
        "✓".green(),
        args.name.bold(),
        report.symlinks,
        if args.keep_daemon {
            ", daemon/ preserved"
        } else {
            ""
        }
    );
    if !report.real_files.is_empty() {
        eprintln!(
            "  {} removed {} unshared file(s) that were not in master",
            "note:".yellow(),
            report.real_files.len()
        );
    }
    if alias_removed {
        println!("  removed alias from {}", rc.display());
    }
    println!("  Master directory and Keychain credentials were left untouched.");
    if cfg.profiles.is_empty() {
        println!("  This was the last profile; the daemon remains installed.");
    }
    Ok(())
}

fn confirm(name: &str, path: &str) -> Result<bool> {
    // Non-interactive stdin -> refuse (safer default for a destructive op).
    if !atty_stdin() {
        return Err(anyhow!(
            "refusing to remove '{name}' without confirmation (no TTY); pass --yes to proceed"
        ));
    }
    println!(
        "{} About to remove profile {} at {}.",
        "⚠".yellow(),
        name.bold(),
        path
    );
    println!("  This profile shares the master's data via symlinks; the master will NOT be touched.");
    print!("  Remove it? [y/N] ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "Yes"))
}

fn atty_stdin() -> bool {
    crate::tty::stdin_is_tty()
}
