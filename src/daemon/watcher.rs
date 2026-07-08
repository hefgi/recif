//! Filesystem watcher wrapper (PRD §7.6).
//!
//! Wraps `notify-debouncer-full` to watch the master and every profile root
//! **non-recursively** at the top level (§7.4/§7.6): we must react to new/removed
//! top-level entries and broken links, but must NOT descend into symlinked dirs
//! (changes inside them are ignored). A ~200ms debounce coalesces bursts.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{RecursiveMode, Watcher};
use notify_debouncer_full::{new_debouncer, DebouncedEvent, Debouncer, FileIdMap};

/// Debounce window (within the §7.6 100–500ms range).
pub const DEBOUNCE: Duration = Duration::from_millis(200);

/// A debounced batch of events, already classified by which root changed.
#[derive(Debug, Default, Clone)]
pub struct ChangeBatch {
    /// The master root saw a change.
    pub master_changed: bool,
    /// These profile roots saw changes.
    pub profiles_changed: Vec<PathBuf>,
    /// The config file changed (daemon should reload profile list).
    pub config_changed: bool,
}

impl ChangeBatch {
    pub fn is_empty(&self) -> bool {
        !self.master_changed && self.profiles_changed.is_empty() && !self.config_changed
    }
}

/// Owns the watcher and the event receiver.
pub struct FsWatcher {
    _debouncer: Debouncer<notify::RecommendedWatcher, FileIdMap>,
    rx: Receiver<Result<Vec<DebouncedEvent>, Vec<notify::Error>>>,
    master: PathBuf,
    profiles: Vec<PathBuf>,
    config_path: PathBuf,
}

impl FsWatcher {
    /// Create a watcher over the master, all profile roots, and the config file.
    pub fn new(master: &Path, profiles: &[PathBuf], config_path: &Path) -> Result<Self> {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut debouncer = new_debouncer(DEBOUNCE, None, move |res| {
            let _ = tx.send(res);
        })
        .context("failed to create fs debouncer")?;

        // Non-recursive watch on master + each profile (we don't want to descend
        // into symlinked dirs). Watch the config file's parent to catch edits.
        debouncer
            .watcher()
            .watch(master, RecursiveMode::NonRecursive)
            .with_context(|| format!("failed to watch master {}", master.display()))?;
        for p in profiles {
            // A profile may not exist yet if config lists it but dir is gone;
            // skip missing ones gracefully.
            if p.exists() {
                debouncer
                    .watcher()
                    .watch(p, RecursiveMode::NonRecursive)
                    .with_context(|| format!("failed to watch profile {}", p.display()))?;
            }
        }
        if let Some(cfg_dir) = config_path.parent() {
            if cfg_dir.exists() {
                let _ = debouncer.watcher().watch(cfg_dir, RecursiveMode::NonRecursive);
            }
        }

        // FSEvents/notify report fully-resolved (realpath) paths on macOS
        // (e.g. /var/... → /private/var/...). Store canonicalized roots so
        // top-level classification matches the paths we receive.
        Ok(FsWatcher {
            _debouncer: debouncer,
            rx,
            master: canon_or(master),
            profiles: profiles.iter().map(|p| canon_or(p)).collect(),
            config_path: canon_or(config_path),
        })
    }

    /// Block until the next debounced batch, then classify it by root. Returns
    /// `None` if the watcher channel closed.
    pub fn next_batch(&self) -> Option<ChangeBatch> {
        let res = self.rx.recv().ok()?;
        Some(self.batch_from(res))
    }

    /// Like [`next_batch`], but returns an empty batch if `timeout` elapses with
    /// no events (so the caller can perform periodic work like heartbeats).
    /// Returns `None` only if the watcher channel is disconnected.
    pub fn next_batch_timeout(&self, timeout: Duration) -> Option<ChangeBatch> {
        use std::sync::mpsc::RecvTimeoutError;
        match self.rx.recv_timeout(timeout) {
            Ok(res) => Some(self.batch_from(res)),
            Err(RecvTimeoutError::Timeout) => Some(ChangeBatch::default()),
            Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    fn batch_from(
        &self,
        res: Result<Vec<DebouncedEvent>, Vec<notify::Error>>,
    ) -> ChangeBatch {
        match res {
            Ok(events) => self.classify(events.iter().flat_map(|e| e.paths.clone())),
            Err(_errs) => ChangeBatch::default(),
        }
    }

    /// Classify a set of changed paths into which roots they belong to.
    ///
    /// Incoming paths are normalized (parent realpath'd) so they compare equal
    /// to the canonicalized roots stored at construction, whether they come from
    /// notify (already resolved) or from a test using raw tempdir paths.
    pub fn classify(&self, paths: impl IntoIterator<Item = PathBuf>) -> ChangeBatch {
        let mut batch = ChangeBatch::default();
        for path in paths {
            let norm = canon_or(&path);
            if norm == self.config_path {
                batch.config_changed = true;
                continue;
            }
            // A path directly under master (parent == master) is a top-level
            // master change.
            if norm.parent() == Some(self.master.as_path()) {
                batch.master_changed = true;
                continue;
            }
            // A path directly under a profile root is a top-level profile change.
            let mut matched = false;
            for p in &self.profiles {
                if norm.parent() == Some(p.as_path()) {
                    if !batch.profiles_changed.contains(p) {
                        batch.profiles_changed.push(p.clone());
                    }
                    matched = true;
                    break;
                }
            }
            let _ = matched;
            // Paths deeper than top level (inside a symlinked dir or daemon/) are
            // ignored by falling through.
        }
        batch
    }
}

/// Canonicalize a path if it exists on disk; otherwise canonicalize its parent
/// and re-append the final component (so a not-yet-created config file still
/// resolves to the realpath form its events will carry). Falls back to the
/// input unchanged if even the parent can't be resolved.
fn canon_or(path: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(path) {
        return c;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
        if let Ok(cp) = std::fs::canonicalize(parent) {
            return cp.join(name);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_master_and_profile_toplevel() {
        let tmp = tempfile::tempdir().unwrap();
        let master = tmp.path().join("master");
        let p1 = tmp.path().join(".claude-a");
        std::fs::create_dir_all(&master).unwrap();
        std::fs::create_dir_all(&p1).unwrap();
        let config = tmp.path().join(".recif/config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();

        let w = FsWatcher::new(&master, &[p1.clone()], &config).unwrap();

        let batch = w.classify([
            master.join("new.json"),
            p1.join("leaked.json"),
            // deep path inside a symlinked dir → ignored
            p1.join("sessions/abc.jsonl"),
            master.join("daemon/sock"), // deep → ignored
        ]);
        assert!(batch.master_changed);
        // profiles_changed carries the canonicalized root form
        assert_eq!(batch.profiles_changed, vec![canon_or(&p1)]);
        assert!(!batch.config_changed);
    }

    #[test]
    fn classify_config_change() {
        let tmp = tempfile::tempdir().unwrap();
        let master = tmp.path().join("master");
        std::fs::create_dir_all(&master).unwrap();
        let config = tmp.path().join(".recif/config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "x").unwrap();

        let w = FsWatcher::new(&master, &[], &config).unwrap();
        let batch = w.classify([config.clone()]);
        assert!(batch.config_changed);
        assert!(!batch.master_changed);
    }
}
