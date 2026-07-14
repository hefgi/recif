//! `recif uninstall` (PRD §8.3) — remove Recif from the system.
//!
//! Reverses install: stop/unload the launchd daemon and delete its plist,
//! remove all Recif-managed shell aliases, safely remove profile directories
//! (unless `--keep-profiles`), then delete `~/.recif`. NEVER touches the master
//! directory or Keychain credentials (§8.3).
//!
//! Best-effort and idempotent: if the config is missing it still tears down the
//! default launchd agent, so a partially-installed setup can always be cleaned.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use colored::Colorize;

use crate::aliases;
use crate::cli::UninstallArgs;
use crate::config::{self, Config};
use crate::launchd::Launchd;
use crate::launchd::ServiceManager;
use crate::saferemove;

pub fn run(args: UninstallArgs) -> Result<()> {
    let config_path = config::default_config_path()?;
    let cfg = config::load_optional(&config_path)?;

    // Confirmation (mandatory unless --yes): this is destructive and removes a
    // background service.
    if !args.yes && !confirm(cfg.as_ref(), args.keep_profiles)? {
        println!("Aborted.");
        return Ok(());
    }

    // 1. Stop + remove the launchd agent. Use config paths if available, else
    //    fall back to the standard default locations so a broken/partial config
    //    can still be cleaned.
    let (plist, log) = daemon_paths(cfg.as_ref())?;
    let svc = Launchd::new(plist.clone(), log, config_path.clone());
    match svc.uninstall() {
        Ok(()) => println!("{} stopped and removed launchd agent", "✓".green()),
        Err(e) => eprintln!("{} could not fully remove launchd agent: {e}", "warning:".yellow()),
    }

    // 2. Remove profiles (safe-unlink) + their aliases.
    let rc = aliases::default_rc_file(args.rc.as_deref())?;
    let mut removed_profiles = 0usize;
    let mut removed_aliases = 0usize;
    if let Some(cfg) = &cfg {
        for profile in &cfg.profiles {
            if !args.keep_profiles {
                match saferemove::remove_profile(&profile.path, false) {
                    Ok(_) => removed_profiles += 1,
                    Err(e) => eprintln!(
                        "{} could not remove profile {} ({}): master untouched",
                        "warning:".yellow(),
                        profile.name,
                        e
                    ),
                }
            }
            if aliases::remove_alias(&rc, &profile.name).unwrap_or(false) {
                removed_aliases += 1;
            }
        }
    }
    if !args.keep_profiles {
        println!("{} removed {removed_profiles} profile director{}", "✓".green(), plural(removed_profiles));
    } else {
        println!("  profile directories left on disk (--keep-profiles)");
    }
    if removed_aliases > 0 {
        println!("{} removed {removed_aliases} shell alias(es) from {}", "✓".green(), rc.display());
    }

    // 3. Remove ~/.recif (config, logs, lock, heartbeat).
    let recif_dir = recif_dir(&config_path);
    match std::fs::remove_dir_all(&recif_dir) {
        Ok(()) => println!("{} removed {}", "✓".green(), recif_dir.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("{} could not remove {}: {e}", "warning:".yellow(), recif_dir.display()),
    }

    println!(
        "\n{} Recif uninstalled. The master directory and Keychain credentials were left untouched.",
        "✓".green().bold()
    );
    if args.keep_profiles {
        println!(
            "  Profile directories remain on disk. Their entries are symlinks into the\n  master, so deleting a profile dir is safe ONLY via unlink — prefer removing it\n  before uninstalling, or delete each symlink individually (never `rm -rf` a dir\n  that a recursive delete would follow into the master)."
        );
    }
    Ok(())
}

/// Resolve the daemon plist + log paths from config, or fall back to the
/// standard default locations so uninstall works even without a valid config.
fn daemon_paths(cfg: Option<&Config>) -> Result<(PathBuf, PathBuf)> {
    if let Some(cfg) = cfg {
        return Ok((cfg.daemon.launchd_plist.clone(), cfg.daemon.log_file.clone()));
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    Ok((
        home.join("Library/LaunchAgents/com.recif.daemon.plist"),
        home.join(".recif/daemon.log"),
    ))
}

/// The `~/.recif` directory, derived from the config path's parent.
fn recif_dir(config_path: &std::path::Path) -> PathBuf {
    config_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".recif"))
}

fn confirm(cfg: Option<&Config>, keep_profiles: bool) -> Result<bool> {
    if !crate::tty::stdin_is_tty() {
        return Err(anyhow!(
            "refusing to uninstall without confirmation (no TTY); pass --yes to proceed"
        ));
    }
    let n = cfg.map(|c| c.profiles.len()).unwrap_or(0);
    println!("{} About to uninstall Recif:", "⚠".yellow());
    println!("  • stop and remove the launchd sync daemon");
    println!("  • remove Recif-managed shell aliases");
    if keep_profiles {
        println!("  • KEEP the {n} profile director(ies) on disk");
    } else {
        println!("  • safely remove {n} profile director(ies) (unlink only; master untouched)");
    }
    println!("  • delete ~/.recif");
    println!(
        "  {} the master directory (~/.claude) and Keychain credentials are NOT touched.",
        "Note:".bold()
    );
    print!("  Proceed? [y/N] ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("failed to read confirmation")?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "Yes"))
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        "y"
    } else {
        "ies"
    }
}
