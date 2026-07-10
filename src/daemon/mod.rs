//! Recif sync daemon (PRD §7).
//!
//! Long-running process invoked by launchd. On start it acquires a
//! single-instance advisory lock, sets up file logging, runs a full
//! reconciliation, registers watches on master + profiles + config, then enters
//! the debounced event loop. It reloads config when `config.toml` changes so
//! newly-added profiles are watched without a restart. A heartbeat timestamp is
//! refreshed periodically so health checks can detect a hung-but-alive daemon.

pub mod reconcile;
pub mod watcher;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fs2::FileExt;
use tracing::{error, info};

use crate::config::{self, Config};
use crate::daemon_status;

/// Entry point for `recif daemon`. `config_override` is the `--config` path
/// baked into the launchd plist; launchd does not inherit the interactive
/// shell's `HOME`, so relying on `default_config_path()` (which resolves `~`)
/// would make the daemon read the wrong file and crash-loop under launchd.
pub fn run(config_override: Option<PathBuf>) -> Result<()> {
    let config_path = match config_override {
        Some(p) => p,
        None => config::default_config_path()?,
    };
    let cfg = config::load(&config_path)
        .with_context(|| format!("failed to load config {}", config_path.display()))?;

    // Single-instance lock (decision #9).
    let _lock = acquire_lock(&cfg)?;

    // Logging to daemon.log.
    let _guard = init_logging(&cfg)?;
    info!("recif daemon starting");

    run_loop(config_path, cfg)
}

/// Hold on to the locked file handle for the daemon's lifetime.
struct DaemonLock {
    _file: std::fs::File,
}

fn acquire_lock(cfg: &Config) -> Result<DaemonLock> {
    let lock_path = cfg
        .daemon
        .log_file
        .parent()
        .map(|p| p.join("daemon.lock"))
        .unwrap_or_else(|| PathBuf::from("daemon.lock"));
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("failed to open lock {}", lock_path.display()))?;
    file.try_lock_exclusive()
        .with_context(|| format!("another recif daemon is already running (lock {} held)", lock_path.display()))?;
    Ok(DaemonLock { _file: file })
}

fn init_logging(cfg: &Config) -> Result<tracing_appender::non_blocking::WorkerGuard> {
    if let Some(parent) = cfg.daemon.log_file.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let dir = cfg
        .daemon
        .log_file
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let file_name = cfg
        .daemon
        .log_file
        .file_name()
        .map(|n| n.to_owned())
        .unwrap_or_else(|| std::ffi::OsString::from("daemon.log"));
    let appender = tracing_appender::rolling::never(dir, file_name);
    let (non_blocking, guard) = tracing_appender::non_blocking(appender);
    let subscriber = tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .finish();
    // Ignore error if a global subscriber is already set (e.g. in tests).
    let _ = tracing::subscriber::set_global_default(subscriber);
    Ok(guard)
}

/// The main loop. Watches for changes and heartbeats. Reloads config on change.
fn run_loop(config_path: PathBuf, mut cfg: Config) -> Result<()> {
    let mut profiles = profile_paths(&cfg);

    // Startup: full reconcile so we self-heal after any downtime.
    reconcile::full_reconcile(&cfg.master, &profiles)?;
    daemon_status::write_heartbeat(&cfg).ok();

    loop {
        let w = watcher::FsWatcher::new(&cfg.master, &profiles, &config_path)?;
        info!(profiles = profiles.len(), "watching");

        // Inner loop: process batches until the config changes (then rebuild the
        // watcher with the updated profile set).
        let mut last_heartbeat = Instant::now();
        let need_rebuild = loop {
            // Non-blocking heartbeat cadence: poll with a timeout so we can
            // refresh the heartbeat even when idle.
            match w.next_batch_timeout(Duration::from_secs(
                daemon_status::HEARTBEAT_INTERVAL_SECS,
            )) {
                Some(batch) if !batch.is_empty() => {
                    if batch.config_changed {
                        break true;
                    }
                    if batch.master_changed {
                        reconcile::on_master_change(&cfg.master, &profiles).ok();
                    }
                    for changed in &batch.profiles_changed {
                        reconcile::on_profile_change(&cfg.master, changed, &profiles).ok();
                    }
                }
                Some(_) => {} // timeout tick, no events
                None => {
                    error!("watcher channel closed; exiting");
                    return Ok(());
                }
            }

            if last_heartbeat.elapsed().as_secs() >= daemon_status::HEARTBEAT_INTERVAL_SECS {
                daemon_status::write_heartbeat(&cfg).ok();
                last_heartbeat = Instant::now();
            }
        };

        if need_rebuild {
            info!("config changed; reloading");
            match config::load(&config_path) {
                Ok(new_cfg) => {
                    cfg = new_cfg;
                    profiles = profile_paths(&cfg);
                    reconcile::full_reconcile(&cfg.master, &profiles).ok();
                }
                Err(e) => error!(error = %e, "failed to reload config; keeping old"),
            }
        }
    }
}

fn profile_paths(cfg: &Config) -> Vec<PathBuf> {
    cfg.profiles.iter().map(|p| p.path.clone()).collect()
}
