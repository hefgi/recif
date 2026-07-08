//! Shared health / sync criteria (PRD §9 — proposed and decided in the plan).
//!
//! `healthy` = structural checks (cheap, no master enumeration required).
//! `synced`  = relational to master (symlink set equals `desired_symlinks`).
//! Daemon health = launchd loaded + live PID (+ fresh heartbeat if present).

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Result;

use crate::canonicalize::canonicalize_profile_path;
use crate::denylist::{self, desired_symlinks};
use crate::profile::{list_top_level, EntryKind};
use crate::symlink;

/// Structural profile health (§9). Does not require the master to be current.
#[derive(Debug, Clone, Default)]
pub struct ProfileHealth {
    pub dir_is_real: bool,
    pub daemon_is_real_dir: bool,
    pub path_is_canonical: bool,
    pub no_denied_real_entries: bool,
    pub no_broken_symlinks: bool,
    pub issues: Vec<String>,
}

impl ProfileHealth {
    pub fn healthy(&self) -> bool {
        self.dir_is_real
            && self.daemon_is_real_dir
            && self.path_is_canonical
            && self.no_denied_real_entries
            && self.no_broken_symlinks
    }
}

/// Compute structural health for a profile at `path` (as stored in config).
pub fn profile_health(path: &Path) -> ProfileHealth {
    let mut h = ProfileHealth::default();

    // dir is a real directory
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_dir() => h.dir_is_real = true,
        Ok(_) => h.issues.push("profile path is not a real directory".into()),
        Err(_) => h.issues.push("profile directory missing".into()),
    }

    // daemon/ is a real dir
    let daemon = path.join("daemon");
    match std::fs::symlink_metadata(&daemon) {
        Ok(m) if m.file_type().is_dir() && !m.file_type().is_symlink() => {
            h.daemon_is_real_dir = true
        }
        Ok(_) => h.issues.push("daemon/ is not a real directory".into()),
        Err(_) => h.issues.push("daemon/ missing".into()),
    }

    // stored path is canonical
    match canonicalize_profile_path(path) {
        Ok(canon) => {
            if canon == path {
                h.path_is_canonical = true;
            } else {
                h.issues
                    .push(format!("path not canonical (should be {})", canon.display()));
            }
        }
        Err(e) => h.issues.push(format!("path canonicalization failed: {e}")),
    }

    // no denied real entries; no broken symlinks
    match list_top_level(path) {
        Ok(entries) => {
            let mut denied_ok = true;
            let mut broken_ok = true;
            for (name, kind) in &entries {
                if name == "daemon" {
                    continue;
                }
                if matches!(kind, EntryKind::RealFile | EntryKind::RealDir)
                    && denylist::is_denied(name, matches!(kind, EntryKind::RealDir))
                {
                    denied_ok = false;
                    h.issues.push(format!("denied entry present: {name}"));
                }
                if matches!(kind, EntryKind::Symlink) {
                    if let Ok(true) = symlink::is_broken_symlink(&path.join(name)) {
                        broken_ok = false;
                        h.issues.push(format!("broken symlink: {name}"));
                    }
                }
            }
            h.no_denied_real_entries = denied_ok;
            h.no_broken_symlinks = broken_ok;
        }
        Err(e) => h.issues.push(format!("could not list profile: {e}")),
    }

    h
}

/// Relational sync status against the master (§9). The profile's symlink set
/// must exactly equal `desired_symlinks(master)` and each link must point at
/// the corresponding `master/E`.
#[derive(Debug, Clone, Default)]
pub struct SyncStatus {
    pub missing: Vec<String>, // master has entry, profile lacks link
    pub stale: Vec<String>,   // profile has link, master entry gone/denied
    pub wrong_target: Vec<String>,
}

impl SyncStatus {
    pub fn synced(&self) -> bool {
        self.missing.is_empty() && self.stale.is_empty() && self.wrong_target.is_empty()
    }
}

/// Compute sync status of `profile` against `master`.
pub fn sync_status(master: &Path, profile: &Path) -> Result<SyncStatus> {
    let mut status = SyncStatus::default();
    let desired: BTreeSet<String> = desired_symlinks(master)?.into_iter().collect();
    let existing = list_top_level(profile)?;
    let existing_links: BTreeSet<String> = existing
        .iter()
        .filter(|(name, kind)| name != "daemon" && matches!(kind, EntryKind::Symlink))
        .map(|(name, _)| name.clone())
        .collect();

    for name in &desired {
        if !existing_links.contains(name) {
            status.missing.push(name.clone());
        } else {
            let link = profile.join(name);
            let target = master.join(name);
            if !symlink::verify_symlink(&link, &target).unwrap_or(false) {
                status.wrong_target.push(name.clone());
            }
        }
    }
    for name in &existing_links {
        if !desired.contains(name) {
            status.stale.push(name.clone());
        }
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn setup(tmp: &Path) -> (PathBuf, PathBuf) {
        let master = tmp.join("master");
        std::fs::create_dir(&master).unwrap();
        std::fs::write(master.join("settings.json"), b"x").unwrap();
        std::fs::create_dir(master.join("sessions")).unwrap();
        let profile = tmp.join("profile");
        std::fs::create_dir(&profile).unwrap();
        std::fs::create_dir(profile.join("daemon")).unwrap();
        for n in ["settings.json", "sessions"] {
            std::os::unix::fs::symlink(master.join(n), profile.join(n)).unwrap();
        }
        (master, profile)
    }

    #[test]
    fn synced_profile_reports_synced() {
        let tmp = tempfile::tempdir().unwrap();
        let (master, profile) = setup(tmp.path());
        let s = sync_status(&master, &profile).unwrap();
        assert!(s.synced(), "{s:?}");
    }

    #[test]
    fn missing_link_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let (master, profile) = setup(tmp.path());
        std::fs::write(master.join("new.json"), b"x").unwrap();
        let s = sync_status(&master, &profile).unwrap();
        assert_eq!(s.missing, vec!["new.json"]);
        assert!(!s.synced());
    }

    #[test]
    fn stale_link_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let (master, profile) = setup(tmp.path());
        std::os::unix::fs::symlink(master.join("settings.json"), profile.join("ghost")).unwrap();
        let s = sync_status(&master, &profile).unwrap();
        assert_eq!(s.stale, vec!["ghost"]);
    }

    #[test]
    fn structural_health_ok() {
        let tmp = tempfile::tempdir().unwrap();
        // must use a canonical path for path_is_canonical to hold
        let (master, _p) = setup(tmp.path());
        let _ = master;
        let canon_profile = canonicalize_profile_path(&tmp.path().join("profile")).unwrap();
        let h = profile_health(&canon_profile);
        assert!(h.dir_is_real);
        assert!(h.daemon_is_real_dir);
        assert!(h.no_broken_symlinks);
    }
}
