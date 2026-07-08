//! Daemon heartbeat file (decision #2, PRD §7.7 / §9).
//!
//! The daemon writes an RFC3339 timestamp to `~/.recif/daemon.status` every
//! `HEARTBEAT_INTERVAL_SECS`. Health consumers treat the daemon as responsive
//! only if the heartbeat is within `2 × interval`.

use std::path::PathBuf;

use anyhow::Result;

use crate::config::Config;

pub const HEARTBEAT_INTERVAL_SECS: u64 = 15;
pub const HEARTBEAT_STALE_SECS: i64 = 30;

/// Path of the heartbeat file, derived from the config log_file's directory.
pub fn status_path(cfg: &Config) -> PathBuf {
    let dir = cfg
        .daemon
        .log_file
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    dir.join("daemon.status")
}

/// Write the current timestamp to the heartbeat file.
pub fn write_heartbeat(cfg: &Config) -> Result<()> {
    let path = status_path(cfg);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let now = chrono::Utc::now().to_rfc3339();
    std::fs::write(&path, now)?;
    Ok(())
}

/// Return `Some(true)` if the heartbeat is fresh, `Some(false)` if stale, and
/// `None` if the file is absent/unreadable.
pub fn heartbeat_fresh(cfg: &Config) -> Option<bool> {
    let path = status_path(cfg);
    let text = std::fs::read_to_string(&path).ok()?;
    let ts = chrono::DateTime::parse_from_rfc3339(text.trim()).ok()?;
    let age = chrono::Utc::now().signed_duration_since(ts.with_timezone(&chrono::Utc));
    Some(age.num_seconds() <= HEARTBEAT_STALE_SECS)
}
