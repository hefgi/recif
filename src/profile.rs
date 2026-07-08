//! Profile reconcile logic (PRD §6, §7.4, §7.5, §8.1).
//!
//! The single reconcile routine used by `add`, the daemon's full/scoped scan,
//! and `doctor`'s auto-fix — so they can never drift. It brings a profile to
//! the desired end state given the current master:
//!
//! - `daemon/` is a real directory,
//! - one symlink per non-denylisted master top-level entry,
//! - stale/broken links removed,
//! - leaked real profile files moved up to master then symlinked back (§7.5).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::denylist::{self, desired_symlinks};
use crate::symlink::{self, LinkOutcome};

/// A record of what a reconcile pass changed, for logging/reporting.
#[derive(Debug, Default, Clone)]
pub struct ReconcileReport {
    pub created: Vec<String>,
    pub replaced: Vec<String>,
    pub removed_stale: Vec<String>,
    pub moved_to_master: Vec<String>,
    pub daemon_recreated: bool,
    pub warnings: Vec<String>,
}

impl ReconcileReport {
    pub fn changed(&self) -> bool {
        !self.created.is_empty()
            || !self.replaced.is_empty()
            || !self.removed_stale.is_empty()
            || !self.moved_to_master.is_empty()
            || self.daemon_recreated
    }
}

/// Ensure `profile/daemon` is a real directory (§6.3, §10). If it is a symlink
/// (corruption / manual error), unlink and recreate as a real dir.
pub fn ensure_daemon_dir(profile: &Path) -> Result<bool> {
    let daemon = profile.join("daemon");
    match std::fs::symlink_metadata(&daemon) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                std::fs::remove_file(&daemon)
                    .with_context(|| format!("failed to unlink bad daemon symlink {}", daemon.display()))?;
                std::fs::create_dir(&daemon)
                    .with_context(|| format!("failed to recreate daemon dir {}", daemon.display()))?;
                return Ok(true);
            }
            if !meta.file_type().is_dir() {
                // A real file named `daemon` — remove and recreate as dir.
                std::fs::remove_file(&daemon)
                    .with_context(|| format!("failed to remove non-dir daemon {}", daemon.display()))?;
                std::fs::create_dir(&daemon)
                    .with_context(|| format!("failed to recreate daemon dir {}", daemon.display()))?;
                return Ok(true);
            }
            Ok(false)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(&daemon)
                .with_context(|| format!("failed to create daemon dir {}", daemon.display()))?;
            Ok(true)
        }
        Err(e) => Err(e).with_context(|| format!("failed to stat {}", daemon.display())),
    }
}

/// Reconcile a single profile against the master. Idempotent. This is the
/// shared end-state engine.
pub fn reconcile_profile(master: &Path, profile: &Path) -> Result<ReconcileReport> {
    let mut report = ReconcileReport::default();

    // 1. Profile dir exists.
    std::fs::create_dir_all(profile)
        .with_context(|| format!("failed to create profile dir {}", profile.display()))?;

    // 2. daemon/ is a real directory.
    report.daemon_recreated = ensure_daemon_dir(profile)?;

    // 3. Move any leaked real files up to master FIRST (§7.5), so the desired
    //    set computed next includes them.
    let existing = list_top_level(profile)?;
    for (name, kind) in &existing {
        if name == "daemon" {
            continue;
        }
        match kind {
            EntryKind::Symlink => {} // handled in the desired loop below
            EntryKind::RealFile | EntryKind::RealDir => {
                if denylist::is_denied(name, matches!(kind, EntryKind::RealDir)) {
                    // A denied real entry leaked into the profile; leave it but warn.
                    report
                        .warnings
                        .push(format!("denied entry present in profile: {name}"));
                    continue;
                }
                move_leaked_to_master(master, profile, name, &mut report)?;
            }
        }
    }

    // 4. Desired symlink set from master (shared denylist source of truth),
    //    computed AFTER moving leaked files so those newly-mastered entries are
    //    included and get linked back.
    let desired: BTreeSet<String> = desired_symlinks(master)?.into_iter().collect();

    // 5. Ensure a symlink for every desired entry.
    for name in &desired {
        let link = profile.join(name);
        let target = master.join(name);
        match symlink::ensure_symlink(&link, &target) {
            Ok(LinkOutcome::Created) => report.created.push(name.clone()),
            Ok(LinkOutcome::Replaced) => report.replaced.push(name.clone()),
            Ok(LinkOutcome::AlreadyCorrect) => {}
            Err(crate::symlink::SymlinkError::NotASymlink(_)) => {
                // Real entry that we already tried to move; force to master copy
                // (§7.5 master wins). Only reachable if move left a copy behind.
                symlink::force_symlink(&link, &target)
                    .with_context(|| format!("failed to force symlink {}", link.display()))?;
                report.replaced.push(name.clone());
            }
            Err(e) => return Err(e).with_context(|| format!("symlink {}", link.display())),
        }
    }

    // 6. Remove stale/broken symlinks: any profile symlink whose name is no
    //    longer desired (master entry gone, or now denied).
    for (name, kind) in &existing {
        if name == "daemon" {
            continue;
        }
        if matches!(kind, EntryKind::Symlink) && !desired.contains(name) {
            let link = profile.join(name);
            std::fs::remove_file(&link)
                .with_context(|| format!("failed to remove stale symlink {}", link.display()))?;
            report.removed_stale.push(name.clone());
        }
    }

    Ok(report)
}

/// Reconcile every profile in a list against the master.
pub fn reconcile_all(master: &Path, profiles: &[PathBuf]) -> Result<Vec<(PathBuf, ReconcileReport)>> {
    let mut out = Vec::new();
    for p in profiles {
        let report = reconcile_profile(master, p)?;
        out.push((p.clone(), report));
    }
    Ok(out)
}

/// Move a leaked real entry `name` from `profile` up to `master`, then leave the
/// desired-symlink loop to create the link (§7.5 move-to-master flow).
///
/// If master already has `name`, master wins: the profile copy is discarded
/// (removed) so the subsequent symlink step points at the master version.
fn move_leaked_to_master(
    master: &Path,
    profile: &Path,
    name: &str,
    report: &mut ReconcileReport,
) -> Result<()> {
    let src = profile.join(name);
    let dst = master.join(name);

    if dst.exists() {
        // Master wins: discard the profile-local copy.
        let meta = std::fs::symlink_metadata(&src)
            .with_context(|| format!("failed to stat {}", src.display()))?;
        if meta.file_type().is_dir() {
            std::fs::remove_dir_all(&src)
                .with_context(|| format!("failed to remove conflicting profile dir {}", src.display()))?;
        } else {
            std::fs::remove_file(&src)
                .with_context(|| format!("failed to remove conflicting profile file {}", src.display()))?;
        }
        report
            .warnings
            .push(format!("{name}: existed in both, master wins (profile copy discarded)"));
        return Ok(());
    }

    // Prefer atomic rename (same volume under $HOME). Fall back to copy+remove
    // across devices.
    match std::fs::rename(&src, &dst) {
        Ok(()) => {}
        Err(_) => {
            copy_recursive(&src, &dst)
                .with_context(|| format!("failed to copy {} -> {}", src.display(), dst.display()))?;
            let meta = std::fs::symlink_metadata(&src)?;
            if meta.file_type().is_dir() {
                std::fs::remove_dir_all(&src)?;
            } else {
                std::fs::remove_file(&src)?;
            }
        }
    }
    report.moved_to_master.push(name.to_string());
    Ok(())
}

fn copy_recursive(src: &Path, dst: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.file_type().is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

/// Classification of a top-level profile entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Symlink,
    RealFile,
    RealDir,
}

/// List top-level entries of a directory with their kind (never follows links).
pub fn list_top_level(dir: &Path) -> Result<Vec<(String, EntryKind)>> {
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e).with_context(|| format!("read_dir {}", dir.display())),
    };
    for entry in rd {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let ft = entry.file_type()?;
        let kind = if ft.is_symlink() {
            EntryKind::Symlink
        } else if ft.is_dir() {
            EntryKind::RealDir
        } else {
            EntryKind::RealFile
        };
        out.push((name, kind));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_master(root: &Path) -> PathBuf {
        let master = root.join("master");
        std::fs::create_dir(&master).unwrap();
        std::fs::write(master.join("settings.json"), b"cfg").unwrap();
        std::fs::create_dir(master.join("sessions")).unwrap();
        master
    }

    #[test]
    fn reconcile_creates_symlinks_and_daemon() {
        let tmp = tempfile::tempdir().unwrap();
        let master = fake_master(tmp.path());
        let profile = tmp.path().join(".claude-x");

        let r = reconcile_profile(&master, &profile).unwrap();
        assert!(r.daemon_recreated);
        assert!(r.created.contains(&"settings.json".to_string()));
        assert!(r.created.contains(&"sessions".to_string()));
        assert!(profile.join("daemon").is_dir());
        assert!(std::fs::symlink_metadata(profile.join("settings.json"))
            .unwrap()
            .file_type()
            .is_symlink());

        // idempotent
        let r2 = reconcile_profile(&master, &profile).unwrap();
        assert!(!r2.changed());
    }

    #[test]
    fn reconcile_removes_stale_link() {
        let tmp = tempfile::tempdir().unwrap();
        let master = fake_master(tmp.path());
        let profile = tmp.path().join(".claude-x");
        reconcile_profile(&master, &profile).unwrap();

        // remove a master entry -> stale link
        std::fs::remove_file(master.join("settings.json")).unwrap();
        let r = reconcile_profile(&master, &profile).unwrap();
        assert!(r.removed_stale.contains(&"settings.json".to_string()));
        assert!(!profile.join("settings.json").exists());
    }

    #[test]
    fn reconcile_moves_leaked_file_to_master() {
        let tmp = tempfile::tempdir().unwrap();
        let master = fake_master(tmp.path());
        let profile = tmp.path().join(".claude-x");
        reconcile_profile(&master, &profile).unwrap();

        // create a new real file in the profile
        std::fs::write(profile.join("new.json"), b"leaked").unwrap();
        let r = reconcile_profile(&master, &profile).unwrap();
        assert!(r.moved_to_master.contains(&"new.json".to_string()));
        // moved to master
        assert_eq!(std::fs::read(master.join("new.json")).unwrap(), b"leaked");
        // and now a symlink in the profile
        assert!(std::fs::symlink_metadata(profile.join("new.json"))
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn reconcile_master_wins_on_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let master = fake_master(tmp.path());
        let profile = tmp.path().join(".claude-x");
        std::fs::create_dir(&profile).unwrap();
        // real file in profile that ALSO exists in master
        std::fs::write(profile.join("settings.json"), b"profile-copy").unwrap();

        let r = reconcile_profile(&master, &profile).unwrap();
        assert!(r.warnings.iter().any(|w| w.contains("master wins")));
        // master value preserved, profile now a symlink to it
        assert_eq!(std::fs::read(master.join("settings.json")).unwrap(), b"cfg");
        assert!(std::fs::symlink_metadata(profile.join("settings.json"))
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn reconcile_fixes_symlinked_daemon() {
        let tmp = tempfile::tempdir().unwrap();
        let master = fake_master(tmp.path());
        let profile = tmp.path().join(".claude-x");
        std::fs::create_dir(&profile).unwrap();
        // daemon/ is wrongly a symlink
        std::os::unix::fs::symlink(master.join("sessions"), profile.join("daemon")).unwrap();

        let r = reconcile_profile(&master, &profile).unwrap();
        assert!(r.daemon_recreated);
        assert!(profile.join("daemon").is_dir());
        assert!(!std::fs::symlink_metadata(profile.join("daemon"))
            .unwrap()
            .file_type()
            .is_symlink());
    }
}
