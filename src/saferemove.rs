//! Safe profile removal (PRD §8.1, §10) — unlink without recursion.
//!
//! The danger: a naive `remove_dir_all` over profile entries would traverse the
//! symlinks into the master and destroy shared data. This module classifies
//! every entry with `symlink_metadata` (which never follows the final link) and:
//!
//! - symlink → `remove_file` (unlinks the *link only*, even for dir symlinks),
//! - real dir (only `daemon/` expected) → `remove_dir_all`, but only after
//!   verifying no child symlink escapes the profile subtree,
//! - real file (a leaked/unshared file) → `remove_file`, logged/reported.
//!
//! The profile root is never `remove_dir_all`'d; after entries are cleared it is
//! `remove_dir`'d so anything unexpected fails loudly.

use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SafeRemoveError {
    #[error("io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("refusing to recursively remove {path}: contains a symlink escaping the profile")]
    EscapingSymlink { path: String },
}

type Result<T> = std::result::Result<T, SafeRemoveError>;

fn io_err(path: &Path, source: std::io::Error) -> SafeRemoveError {
    SafeRemoveError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// What was done to a single entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removed {
    /// A symlink was unlinked (target untouched).
    Symlink,
    /// A real directory was recursively removed (verified safe).
    RealDir,
    /// A real file was removed (was unshared/leaked data).
    RealFile,
    /// Nothing was present.
    Nothing,
}

/// Remove a single top-level profile entry safely, returning what was done.
///
/// `profile_root` is the profile directory the entry must stay within; it is
/// used to detect symlinks that would escape the subtree during a recursive
/// `daemon/` removal.
pub fn remove_profile_entry(entry: &Path, profile_root: &Path) -> Result<Removed> {
    let meta = match std::fs::symlink_metadata(entry) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Removed::Nothing),
        Err(e) => return Err(io_err(entry, e)),
    };

    let ft = meta.file_type();

    if ft.is_symlink() {
        // Unlinks the link only — never follows into the target. Works on
        // dangling links too.
        std::fs::remove_file(entry).map_err(|e| io_err(entry, e))?;
        return Ok(Removed::Symlink);
    }

    if ft.is_dir() {
        // A real directory (normally only `daemon/`). Belt-and-suspenders:
        // ensure no descendant is a symlink escaping the profile before we
        // recurse. If any is, refuse.
        assert_no_escaping_symlink(entry, profile_root)?;
        std::fs::remove_dir_all(entry).map_err(|e| io_err(entry, e))?;
        return Ok(Removed::RealDir);
    }

    // Real file: leaked/unshared data. Removing is acceptable (caller logs it).
    std::fs::remove_file(entry).map_err(|e| io_err(entry, e))?;
    Ok(Removed::RealFile)
}

/// Walk `dir` and error if any descendant is a symlink whose (lexical) target
/// escapes `profile_root`. A symlink pointing *within* the subtree is allowed
/// (`remove_dir_all` unlinks it without following); one pointing outside is a
/// red flag we refuse to risk.
fn assert_no_escaping_symlink(dir: &Path, profile_root: &Path) -> Result<()> {
    let rd = std::fs::read_dir(dir).map_err(|e| io_err(dir, e))?;
    for entry in rd {
        let entry = entry.map_err(|e| io_err(dir, e))?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).map_err(|e| io_err(&path, e))?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            let target = std::fs::read_link(&path).map_err(|e| io_err(&path, e))?;
            let resolved = if target.is_absolute() {
                target
            } else {
                path.parent().unwrap_or(dir).join(&target)
            };
            let normalized = crate::canonicalize::lexical_normalize(&resolved);
            let root_norm = crate::canonicalize::lexical_normalize(profile_root);
            if !normalized.starts_with(&root_norm) {
                return Err(SafeRemoveError::EscapingSymlink {
                    path: path.display().to_string(),
                });
            }
            // in-subtree symlink: fine, don't recurse into it.
        } else if ft.is_dir() {
            assert_no_escaping_symlink(&path, profile_root)?;
        }
    }
    Ok(())
}

/// Summary of a full profile removal.
#[derive(Debug, Default)]
pub struct RemovalReport {
    pub symlinks: usize,
    pub real_files: Vec<PathBuf>,
    pub real_dirs: usize,
    pub root_removed: bool,
}

/// Safely remove all top-level entries of a profile, then the (empty) root.
///
/// `keep_daemon` preserves the `daemon/` directory (and therefore leaves the
/// root in place too, per decision #1). Returns a report of what happened.
pub fn remove_profile(profile_root: &Path, keep_daemon: bool) -> Result<RemovalReport> {
    let mut report = RemovalReport::default();

    let rd = std::fs::read_dir(profile_root).map_err(|e| io_err(profile_root, e))?;
    let mut entries: Vec<PathBuf> = Vec::new();
    for e in rd {
        entries.push(e.map_err(|err| io_err(profile_root, err))?.path());
    }

    for entry in entries {
        let is_daemon = entry.file_name().map(|n| n == "daemon").unwrap_or(false);
        if keep_daemon && is_daemon {
            continue;
        }
        match remove_profile_entry(&entry, profile_root)? {
            Removed::Symlink => report.symlinks += 1,
            Removed::RealDir => report.real_dirs += 1,
            Removed::RealFile => report.real_files.push(entry),
            Removed::Nothing => {}
        }
    }

    if keep_daemon {
        // Root intentionally left on disk for forensics (decision #1).
        return Ok(report);
    }

    // remove_dir (NOT remove_dir_all): fails loudly if anything unexpected
    // remains in the root.
    std::fs::remove_dir(profile_root).map_err(|e| io_err(profile_root, e))?;
    report.root_removed = true;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fake master with real files + subdirs, and a profile of symlinks
    /// into it. Removal must leave the master fully intact.
    #[test]
    fn removal_leaves_master_intact() {
        let tmp = tempfile::tempdir().unwrap();
        let master = tmp.path().join("master");
        std::fs::create_dir(&master).unwrap();
        std::fs::write(master.join("settings.json"), b"cfg").unwrap();
        std::fs::create_dir(master.join("sessions")).unwrap();
        std::fs::write(master.join("sessions/s1.jsonl"), b"session").unwrap();

        let profile = tmp.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        std::os::unix::fs::symlink(master.join("settings.json"), profile.join("settings.json"))
            .unwrap();
        std::os::unix::fs::symlink(master.join("sessions"), profile.join("sessions")).unwrap();
        std::fs::create_dir(profile.join("daemon")).unwrap();
        std::fs::write(profile.join("daemon/pid"), b"123").unwrap();

        let report = remove_profile(&profile, false).unwrap();
        assert_eq!(report.symlinks, 2);
        assert_eq!(report.real_dirs, 1);
        assert!(report.root_removed);
        assert!(!profile.exists());

        // Master fully intact.
        assert_eq!(std::fs::read(master.join("settings.json")).unwrap(), b"cfg");
        assert_eq!(
            std::fs::read(master.join("sessions/s1.jsonl")).unwrap(),
            b"session"
        );
    }

    #[test]
    fn dangling_symlink_unlinked() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("nonexistent"), profile.join("dead")).unwrap();

        assert_eq!(
            remove_profile_entry(&profile.join("dead"), &profile).unwrap(),
            Removed::Symlink
        );
        assert!(!profile.join("dead").exists());
    }

    /// A directory symlink in the profile must be unlinked, its target dir and
    /// contents untouched (regression guard for remove_dir_all following links).
    #[test]
    fn directory_symlink_target_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let master = tmp.path().join("master");
        std::fs::create_dir(&master).unwrap();
        let sessions = master.join("sessions");
        std::fs::create_dir(&sessions).unwrap();
        std::fs::write(sessions.join("s1.jsonl"), b"data").unwrap();

        let profile = tmp.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        std::os::unix::fs::symlink(&sessions, profile.join("sessions")).unwrap();

        assert_eq!(
            remove_profile_entry(&profile.join("sessions"), &profile).unwrap(),
            Removed::Symlink
        );
        // Target dir and contents intact.
        assert!(sessions.exists());
        assert_eq!(std::fs::read(sessions.join("s1.jsonl")).unwrap(), b"data");
    }

    #[test]
    fn daemon_with_real_files_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("profile");
        let daemon = profile.join("daemon");
        std::fs::create_dir_all(&daemon).unwrap();
        std::fs::write(daemon.join("sock"), b"x").unwrap();
        std::fs::create_dir(daemon.join("sub")).unwrap();
        std::fs::write(daemon.join("sub/f"), b"y").unwrap();

        assert_eq!(
            remove_profile_entry(&daemon, &profile).unwrap(),
            Removed::RealDir
        );
        assert!(!daemon.exists());
    }

    #[test]
    fn daemon_with_escaping_symlink_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("precious"), b"keep").unwrap();

        let profile = tmp.path().join("profile");
        let daemon = profile.join("daemon");
        std::fs::create_dir_all(&daemon).unwrap();
        std::os::unix::fs::symlink(&outside, daemon.join("escape")).unwrap();

        let err = remove_profile_entry(&daemon, &profile).unwrap_err();
        assert!(matches!(err, SafeRemoveError::EscapingSymlink { .. }));
        // daemon and the escaping target both intact.
        assert!(daemon.exists());
        assert_eq!(std::fs::read(outside.join("precious")).unwrap(), b"keep");
    }

    #[test]
    fn keep_daemon_preserves_root_and_daemon() {
        let tmp = tempfile::tempdir().unwrap();
        let master = tmp.path().join("master");
        std::fs::create_dir(&master).unwrap();
        std::fs::write(master.join("settings.json"), b"cfg").unwrap();

        let profile = tmp.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        std::os::unix::fs::symlink(master.join("settings.json"), profile.join("settings.json"))
            .unwrap();
        std::fs::create_dir(profile.join("daemon")).unwrap();
        std::fs::write(profile.join("daemon/log"), b"forensic").unwrap();

        let report = remove_profile(&profile, true).unwrap();
        assert_eq!(report.symlinks, 1);
        assert!(!report.root_removed);
        assert!(profile.exists());
        assert!(profile.join("daemon/log").exists());
        assert!(!profile.join("settings.json").exists());
    }
}
