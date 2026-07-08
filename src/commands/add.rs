//! `recif add` (PRD §8.1).

use anyhow::{anyhow, Context, Result};
use colored::Colorize;

use crate::aliases;
use crate::canonicalize::canonicalize_profile_path;
use crate::cli::AddArgs;
use crate::config::{self, Config, Profile};
use crate::launchd::{Launchd, ServiceManager};
use crate::profile::reconcile_profile;

pub fn run(args: AddArgs) -> Result<()> {
    crate::commands::validate_name(&args.name)?;

    let config_path = config::default_config_path()?;

    // 1. Load or create config.
    let mut cfg = match config::load_optional(&config_path)? {
        Some(c) => c,
        None => {
            // initialize with default master (canonicalized below)
            let home = dirs::home_dir().ok_or_else(|| anyhow!("no home directory"))?;
            Config::new_default(home.join(".claude"))?
        }
    };

    // 2. Resolve + canonicalize master.
    let requested_master = args
        .master
        .map(|m| canonicalize_profile_path(&m))
        .transpose()?;
    let stored_master = canonicalize_profile_path(&cfg.master)?;

    let master = match requested_master {
        Some(m) => {
            if config::load_optional(&config_path)?.is_some() && m != stored_master {
                return Err(anyhow!(
                    "requested master {} differs from configured master {} — changing master is out of scope",
                    m.display(),
                    stored_master.display()
                ));
            }
            m
        }
        None => stored_master,
    };
    if !master.is_dir() {
        return Err(anyhow!("master directory does not exist: {}", master.display()));
    }
    cfg.master = master.clone();

    // 3. Compute canonical profile path `~/.claude-<name>`.
    let home = dirs::home_dir().ok_or_else(|| anyhow!("no home directory"))?;
    let profile_path = canonicalize_profile_path(&home.join(format!(".claude-{}", args.name)))?;

    // 4. If the dir exists and is NOT a Recif profile (not in config, has content
    //    that isn't a symlink set), warn but proceed idempotently.
    let already_known = cfg.profile(&args.name).is_some();
    if profile_path.exists() && !already_known {
        // Allow if it looks like a converted/prior profile; otherwise inform.
        eprintln!(
            "{} {} already exists; converting it into a Recif profile (files not in master will be moved to master).",
            "note:".yellow(),
            profile_path.display()
        );
    }

    // 5–7. Reconcile (creates dir, real daemon/, symlink set, moves leaked files).
    let report = reconcile_profile(&master, &profile_path)
        .with_context(|| format!("failed to reconcile profile {}", profile_path.display()))?;

    // 8. Write/update config entry.
    let created_at = existing_created_at(&cfg, &args.name)
        .unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string());
    cfg.upsert_profile(Profile {
        name: args.name.clone(),
        description: args.description.unwrap_or_default(),
        created_at,
        path: profile_path.clone(),
    });
    config::save(&config_path, &cfg)?;

    // 9. Install + start daemon (unless disabled for tests).
    if !args.no_daemon_install {
        let svc = Launchd::new(cfg.daemon.launchd_plist.clone(), cfg.daemon.log_file.clone());
        install_daemon(&svc)?;
    }

    // 10. Alias.
    if args.alias {
        let rc = aliases::default_rc_file(args.rc.as_deref())?;
        let added = aliases::add_alias(&rc, &args.name)?;
        if added {
            println!("Added alias claude-{} to {}", args.name, rc.display());
            println!("  Run: source {}", rc.display());
        }
    }

    // Report.
    println!(
        "{} profile {} at {}",
        "✓".green(),
        args.name.bold(),
        profile_path.display()
    );
    if report.changed() {
        if !report.created.is_empty() {
            println!("  linked {} entr{}", report.created.len(), plural(report.created.len()));
        }
        if !report.moved_to_master.is_empty() {
            println!(
                "  moved {} leaked file(s) to master: {}",
                report.moved_to_master.len(),
                report.moved_to_master.join(", ")
            );
        }
        if !report.removed_stale.is_empty() {
            println!("  removed {} stale link(s)", report.removed_stale.len());
        }
    }
    for w in &report.warnings {
        eprintln!("  {} {}", "warning:".yellow(), w);
    }
    Ok(())
}

fn install_daemon(svc: &dyn ServiceManager) -> Result<()> {
    let exe = std::env::current_exe().context("could not resolve recif binary path")?;
    svc.install(&exe)
        .context("failed to install/start the Recif daemon via launchd")?;
    Ok(())
}

fn existing_created_at(cfg: &Config, name: &str) -> Option<String> {
    cfg.profile(name).map(|p| p.created_at.clone())
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        "y"
    } else {
        "ies"
    }
}

/// Test-only entrypoint that skips daemon install and uses an explicit config
/// path + home, so integration tests never touch the real environment.
#[cfg(any(test, feature = "testutil"))]
pub fn run_with_paths(
    config_path: &std::path::Path,
    master: &std::path::Path,
    profile_path: &std::path::Path,
    name: &str,
    description: Option<String>,
) -> Result<()> {
    let mut cfg = match config::load_optional(config_path)? {
        Some(c) => c,
        None => Config::new_default(master.to_path_buf())?,
    };
    cfg.master = master.to_path_buf();
    reconcile_profile(master, profile_path)?;
    let created_at = existing_created_at(&cfg, name)
        .unwrap_or_else(|| "2026-01-01T00:00:00Z".to_string());
    cfg.upsert_profile(Profile {
        name: name.to_string(),
        description: description.unwrap_or_default(),
        created_at,
        path: profile_path.to_path_buf(),
    });
    config::save(config_path, &cfg)?;
    Ok(())
}
