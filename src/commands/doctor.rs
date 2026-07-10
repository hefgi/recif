//! `recif doctor` (PRD §8.1, §7.2). Read-only checks first, then auto-fix.

use anyhow::{Context, Result};
use colored::Colorize;

use crate::config::Config;
use crate::health::profile_health;
use crate::keychain;
use crate::launchd::{DaemonState, Launchd, ServiceManager};
use crate::profile::reconcile_profile;

#[derive(Default)]
struct Summary {
    checks: usize,
    fixed: usize,
    manual: Vec<String>,
    errors: Vec<String>,
}

pub fn run() -> Result<()> {
    let (config_path, cfg) = crate::commands::load_config()?;
    let mut summary = Summary::default();

    // 1. Master dir exists and is a real directory.
    summary.checks += 1;
    if cfg.master.is_dir() {
        ok(&format!("master {} exists", cfg.master.display()));
    } else {
        summary
            .errors
            .push(format!("master directory missing: {}", cfg.master.display()));
        bad(&format!("master {} MISSING", cfg.master.display()));
    }

    // 2. Config parses + version == 1 (already validated by load; report).
    summary.checks += 1;
    ok(&format!("config valid (version {})", cfg.version));

    // 3. Each profile path is canonical.
    for p in &cfg.profiles {
        summary.checks += 1;
        match crate::canonicalize::canonicalize_profile_path(&p.path) {
            Ok(canon) if canon == p.path => {}
            Ok(canon) => {
                summary.manual.push(format!(
                    "profile '{}' path not canonical: {} (should be {})",
                    p.name,
                    p.path.display(),
                    canon.display()
                ));
                warn(&format!("profile '{}' path not canonical", p.name));
            }
            Err(e) => summary
                .errors
                .push(format!("profile '{}' path invalid: {e}", p.name)),
        }
    }

    // 4. Daemon loaded + responsive; reinstall/reload if not.
    summary.checks += 1;
    let svc = Launchd::new(
        cfg.daemon.launchd_plist.clone(),
        cfg.daemon.log_file.clone(),
        config_path.clone(),
    );
    let state = svc.state().unwrap_or(DaemonState::NotLoaded);
    let fresh = crate::daemon_status::heartbeat_fresh(&cfg);
    let responsive = matches!(state, DaemonState::Running) && fresh != Some(false);
    if responsive {
        ok("daemon running and responsive");
    } else {
        warn(&format!(
            "daemon not healthy ({state:?}, heartbeat={fresh:?}); reinstalling/reloading"
        ));
        match reinstall_daemon(&svc) {
            Ok(()) => {
                summary.fixed += 1;
                ok("daemon reinstalled/reloaded");
            }
            Err(e) => summary.errors.push(format!("daemon reinstall failed: {e}")),
        }
    }

    // 5. Per-profile structural + symlink reconcile fixes.
    let master = cfg.master.clone();
    for p in &cfg.profiles {
        summary.checks += 1;
        let health = profile_health(&p.path);
        if health.healthy() {
            // still run reconcile to catch sync drift (missing/stale links)
            match reconcile_profile(&master, &p.path) {
                Ok(r) if r.changed() => {
                    summary.fixed += 1;
                    ok(&format!("profile '{}' resynced", p.name));
                }
                Ok(_) => ok(&format!("profile '{}' healthy and synced", p.name)),
                Err(e) => summary
                    .errors
                    .push(format!("profile '{}' reconcile error: {e}", p.name)),
            }
        } else {
            warn(&format!(
                "profile '{}' unhealthy: {}",
                p.name,
                health.issues.join("; ")
            ));
            match reconcile_profile(&master, &p.path) {
                Ok(_) => {
                    summary.fixed += 1;
                    ok(&format!("profile '{}' repaired", p.name));
                }
                Err(e) => summary
                    .errors
                    .push(format!("profile '{}' repair failed: {e}", p.name)),
            }
        }

        // Keychain slot (report-only, never fix).
        summary.checks += 1;
        let names = keychain::candidate_service_names(&p.path, &master);
        if names.iter().any(|n| keychain::slot_exists(n)) {
            ok(&format!("profile '{}' Keychain slot present", p.name));
        } else {
            summary.manual.push(format!(
                "profile '{}' has no Keychain slot — run `claude-{}` and `/login` once",
                p.name, p.name
            ));
            warn(&format!("profile '{}' missing Keychain slot", p.name));
        }
    }

    // Persist any canonicalization or reconcile side effects (config unchanged
    // in fixes above, but save defensively to normalize formatting).
    let _ = config_save_best_effort(&config_path, &cfg);

    // Summary.
    println!();
    println!(
        "{} {} checks, {} auto-fixed",
        "doctor:".bold(),
        summary.checks,
        summary.fixed
    );
    for m in &summary.manual {
        println!("  {} {}", "manual:".yellow(), m);
    }
    for e in &summary.errors {
        println!("  {} {}", "error:".red(), e);
    }
    if summary.errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("{} unresolved issue(s)", summary.errors.len())
    }
}

fn reinstall_daemon(svc: &dyn ServiceManager) -> Result<()> {
    // Always regenerate the plist from the CURRENT binary path, not just reload
    // the existing file. Reloading a stale plist would keep pointing at an old
    // binary location (e.g. a dev-tree target/ path after the binary was moved
    // to ~/.local/bin), silently preserving the breakage doctor is meant to fix.
    // install() writes the plist then (re)loads it, and is idempotent.
    let exe = std::env::current_exe().context("could not resolve recif binary path")?;
    svc.install(&exe)
}

fn config_save_best_effort(path: &std::path::Path, cfg: &Config) -> Result<()> {
    crate::config::save(path, cfg)
}

fn ok(msg: &str) {
    println!("  {} {}", "✓".green(), msg);
}
fn warn(msg: &str) {
    println!("  {} {}", "!".yellow(), msg);
}
fn bad(msg: &str) {
    println!("  {} {}", "✗".red(), msg);
}
