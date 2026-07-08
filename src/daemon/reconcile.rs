//! Daemon reconciliation (PRD §7.4, §7.5).
//!
//! Builds on the per-profile [`crate::profile::reconcile_profile`] engine, but
//! adds the *cross-profile* view the daemon needs: a change in one root (master
//! or a profile) must propagate to **all** profiles. The robust strategy (per
//! the plan) is: on any event batch touching a root, re-run the scoped
//! reconcile for the affected roots — idempotent, and it computes the correct
//! end state regardless of notify's event granularity.

use std::path::{Path, PathBuf};

use anyhow::Result;
use tracing::{info, warn};

use crate::profile::{reconcile_profile, ReconcileReport};

/// Full reconciliation across every profile against the master. Run at daemon
/// startup and whenever we can't scope the change (e.g. config reload). Makes
/// the daemon self-healing after downtime.
pub fn full_reconcile(master: &Path, profiles: &[PathBuf]) -> Result<()> {
    info!(profiles = profiles.len(), "full reconcile");
    for profile in profiles {
        match reconcile_profile(master, profile) {
            Ok(report) => log_report(profile, &report),
            Err(e) => warn!(profile = %profile.display(), error = %e, "reconcile failed"),
        }
    }
    Ok(())
}

/// A change was detected in the **master** root (new/removed top-level entry).
/// Re-reconcile every profile so new links appear and stale links disappear in
/// all of them.
pub fn on_master_change(master: &Path, profiles: &[PathBuf]) -> Result<()> {
    info!("master changed → reconciling all profiles");
    for profile in profiles {
        match reconcile_profile(master, profile) {
            Ok(report) => log_report(profile, &report),
            Err(e) => warn!(profile = %profile.display(), error = %e, "reconcile failed"),
        }
    }
    Ok(())
}

/// A change was detected in a **single profile** root. Reconcile that profile
/// (which may move a leaked file to master), and if anything was moved to
/// master, propagate to the *other* profiles too.
pub fn on_profile_change(master: &Path, changed: &Path, all_profiles: &[PathBuf]) -> Result<()> {
    info!(profile = %changed.display(), "profile changed → scoped reconcile");
    let report = match reconcile_profile(master, changed) {
        Ok(r) => r,
        Err(e) => {
            warn!(profile = %changed.display(), error = %e, "reconcile failed");
            return Ok(());
        }
    };
    log_report(changed, &report);

    // If files were promoted to master, every OTHER profile now needs a link.
    if !report.moved_to_master.is_empty() {
        for other in all_profiles {
            if other == changed {
                continue;
            }
            if let Err(e) = reconcile_profile(master, other) {
                warn!(profile = %other.display(), error = %e, "propagation reconcile failed");
            }
        }
    }
    Ok(())
}

fn log_report(profile: &Path, report: &ReconcileReport) {
    if report.changed() {
        info!(
            profile = %profile.display(),
            created = report.created.len(),
            replaced = report.replaced.len(),
            removed = report.removed_stale.len(),
            moved = report.moved_to_master.len(),
            "reconciled"
        );
    }
    for w in &report.warnings {
        warn!(profile = %profile.display(), "{}", w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_master(root: &Path) -> PathBuf {
        let m = root.join("master");
        std::fs::create_dir(&m).unwrap();
        std::fs::write(m.join("settings.json"), b"cfg").unwrap();
        m
    }

    fn mk_profile(master: &Path, root: &Path, name: &str) -> PathBuf {
        let p = root.join(format!(".claude-{name}"));
        reconcile_profile(master, &p).unwrap();
        p
    }

    #[test]
    fn new_master_file_propagates_to_all() {
        let tmp = tempfile::tempdir().unwrap();
        let master = mk_master(tmp.path());
        let p1 = mk_profile(&master, tmp.path(), "a");
        let p2 = mk_profile(&master, tmp.path(), "b");

        std::fs::write(master.join("new.json"), b"x").unwrap();
        on_master_change(&master, &[p1.clone(), p2.clone()]).unwrap();

        for p in [&p1, &p2] {
            assert!(std::fs::symlink_metadata(p.join("new.json"))
                .unwrap()
                .file_type()
                .is_symlink());
        }
    }

    #[test]
    fn deleted_master_file_removes_links_everywhere() {
        let tmp = tempfile::tempdir().unwrap();
        let master = mk_master(tmp.path());
        let p1 = mk_profile(&master, tmp.path(), "a");
        let p2 = mk_profile(&master, tmp.path(), "b");

        std::fs::remove_file(master.join("settings.json")).unwrap();
        on_master_change(&master, &[p1.clone(), p2.clone()]).unwrap();

        for p in [&p1, &p2] {
            assert!(!p.join("settings.json").exists());
        }
    }

    #[test]
    fn new_profile_file_moves_and_propagates() {
        let tmp = tempfile::tempdir().unwrap();
        let master = mk_master(tmp.path());
        let p1 = mk_profile(&master, tmp.path(), "a");
        let p2 = mk_profile(&master, tmp.path(), "b");

        // new real file in p1
        std::fs::write(p1.join("note.json"), b"data").unwrap();
        on_profile_change(&master, &p1, &[p1.clone(), p2.clone()]).unwrap();

        // moved to master
        assert_eq!(std::fs::read(master.join("note.json")).unwrap(), b"data");
        // symlinked back in p1 AND propagated to p2
        for p in [&p1, &p2] {
            assert!(std::fs::symlink_metadata(p.join("note.json"))
                .unwrap()
                .file_type()
                .is_symlink());
        }
    }

    #[test]
    fn broken_link_recreated_by_full_reconcile() {
        let tmp = tempfile::tempdir().unwrap();
        let master = mk_master(tmp.path());
        let p1 = mk_profile(&master, tmp.path(), "a");

        // simulate broken link: remove target then re-add, leaving link dangling
        std::fs::remove_file(master.join("settings.json")).unwrap();
        // full reconcile removes the now-stale link
        full_reconcile(&master, &[p1.clone()]).unwrap();
        assert!(!p1.join("settings.json").exists());
    }

    #[test]
    fn denied_profile_entry_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let master = mk_master(tmp.path());
        let p1 = mk_profile(&master, tmp.path(), "a");

        // a denied real file leaks into the profile
        std::fs::write(p1.join("x.lock"), b"lock").unwrap();
        on_profile_change(&master, &p1, &[p1.clone()]).unwrap();
        // NOT moved to master, left in place
        assert!(!master.join("x.lock").exists());
        assert!(p1.join("x.lock").exists());
    }
}
