//! `recif list` (PRD §8.1). Shows per-profile health/sync + a daemon line.

use anyhow::Result;
use colored::Colorize;
use tabled::settings::Style;
use tabled::{Table, Tabled};

use crate::aliases;
use crate::health::{profile_health, sync_status};
use crate::keychain;
use crate::launchd::{DaemonState, Launchd, ServiceManager};

#[derive(Tabled)]
struct Row {
    #[tabled(rename = "NAME")]
    name: String,
    #[tabled(rename = "HEALTHY")]
    healthy: String,
    #[tabled(rename = "SYNCED")]
    synced: String,
    #[tabled(rename = "ALIAS")]
    alias: String,
    #[tabled(rename = "KEYCHAIN")]
    keychain: String,
    #[tabled(rename = "PATH")]
    path: String,
}

pub fn run() -> Result<()> {
    let (_path, cfg) = crate::commands::load_config()?;

    // Daemon health line.
    let svc = Launchd::new(cfg.daemon.launchd_plist.clone(), cfg.daemon.log_file.clone());
    let state = svc.state().unwrap_or(DaemonState::NotLoaded);
    let daemon_line = match state {
        DaemonState::Running => "running".green().to_string(),
        DaemonState::LoadedNoPid => "loaded but not running".yellow().to_string(),
        DaemonState::NotLoaded => "not installed".red().to_string(),
    };
    // Heartbeat freshness augments the state (§9), if the file is present.
    let hb = crate::daemon_status::heartbeat_fresh(&cfg).map(|f| if f { "fresh" } else { "stale" });
    match hb {
        Some(h) => println!("Daemon: {daemon_line} (heartbeat {h})"),
        None => println!("Daemon: {daemon_line}"),
    }

    let rc = aliases::default_rc_file(None).ok();

    let mut rows = Vec::new();
    for p in &cfg.profiles {
        let health = profile_health(&p.path);
        let synced = sync_status(&cfg.master, &p.path)
            .map(|s| s.synced())
            .unwrap_or(false);
        let alias_present = rc
            .as_ref()
            .and_then(|rc| std::fs::read_to_string(rc).ok())
            .map(|c| c.contains(&format!("alias claude-{}=", p.name)))
            .unwrap_or(false);
        let kc = keychain_status(&p.path, &cfg.master);

        rows.push(Row {
            name: p.name.clone(),
            healthy: yn(health.healthy()),
            synced: yn(synced),
            alias: yn(alias_present),
            keychain: kc,
            path: p.path.display().to_string(),
        });
    }

    if rows.is_empty() {
        println!("No profiles configured. Run `recif add <name>`.");
        return Ok(());
    }

    let mut table = Table::new(rows);
    table.with(Style::rounded());
    println!("{table}");
    Ok(())
}

fn keychain_status(path: &std::path::Path, master: &std::path::Path) -> String {
    let names = keychain::candidate_service_names(path, master);
    let exists = names.iter().any(|n| keychain::slot_exists(n));
    if exists {
        "yes".to_string()
    } else {
        "needs /login".to_string()
    }
}

fn yn(b: bool) -> String {
    if b {
        "yes".to_string()
    } else {
        "no".to_string()
    }
}
