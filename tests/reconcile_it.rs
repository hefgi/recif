//! Daemon reconciliation integration tests (PRD §7.4/§7.5).
//!
//! These drive the reconcile functions directly (not real FSEvents) for
//! determinism, plus one optional live-watcher smoke test gated behind an env
//! var so CI stays deterministic.

use std::path::{Path, PathBuf};
use std::time::Duration;

use recif::daemon::reconcile::{full_reconcile, on_master_change, on_profile_change};
use recif::daemon::watcher::FsWatcher;
use recif::profile::reconcile_profile;

fn mk_master(root: &Path) -> PathBuf {
    let m = root.join("master");
    std::fs::create_dir(&m).unwrap();
    std::fs::write(m.join("settings.json"), b"cfg").unwrap();
    std::fs::create_dir(m.join("sessions")).unwrap();
    m
}

fn mk_profile(master: &Path, root: &Path, name: &str) -> PathBuf {
    let p = root.join(format!(".claude-{name}"));
    reconcile_profile(master, &p).unwrap();
    p
}

#[test]
fn new_master_file_propagates_to_all_profiles() {
    let tmp = tempfile::tempdir().unwrap();
    let master = mk_master(tmp.path());
    let a = mk_profile(&master, tmp.path(), "a");
    let b = mk_profile(&master, tmp.path(), "b");

    std::fs::write(master.join("policy.json"), b"pol").unwrap();
    on_master_change(&master, &[a.clone(), b.clone()]).unwrap();

    for p in [&a, &b] {
        assert!(is_symlink(&p.join("policy.json")));
    }
}

#[test]
fn deleted_master_file_removes_links_everywhere() {
    let tmp = tempfile::tempdir().unwrap();
    let master = mk_master(tmp.path());
    let a = mk_profile(&master, tmp.path(), "a");
    let b = mk_profile(&master, tmp.path(), "b");

    std::fs::remove_file(master.join("settings.json")).unwrap();
    on_master_change(&master, &[a.clone(), b.clone()]).unwrap();

    for p in [&a, &b] {
        assert!(!p.join("settings.json").exists());
    }
}

#[test]
fn new_profile_file_moved_symlinked_and_propagated() {
    let tmp = tempfile::tempdir().unwrap();
    let master = mk_master(tmp.path());
    let a = mk_profile(&master, tmp.path(), "a");
    let b = mk_profile(&master, tmp.path(), "b");

    std::fs::write(a.join("scratch.json"), b"x").unwrap();
    on_profile_change(&master, &a, &[a.clone(), b.clone()]).unwrap();

    assert_eq!(std::fs::read(master.join("scratch.json")).unwrap(), b"x");
    assert!(is_symlink(&a.join("scratch.json")));
    assert!(is_symlink(&b.join("scratch.json")));
}

#[test]
fn change_inside_symlinked_dir_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let master = mk_master(tmp.path());
    let a = mk_profile(&master, tmp.path(), "a");
    let config = tmp.path().join(".recif/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();

    let w = FsWatcher::new(&master, &[a.clone()], &config).unwrap();
    // A write "inside" the symlinked sessions dir is a deep path → not classified
    // as a top-level change to master or profile.
    let batch = w.classify([a.join("sessions/new.jsonl")]);
    assert!(batch.is_empty());
}

#[test]
fn denied_entries_ignored_in_full_reconcile() {
    let tmp = tempfile::tempdir().unwrap();
    let master = mk_master(tmp.path());
    // denied master entries
    std::fs::write(master.join(".credentials.json"), b"s").unwrap();
    std::fs::write(master.join("db.lock"), b"l").unwrap();
    let a = mk_profile(&master, tmp.path(), "a");

    full_reconcile(&master, &[a.clone()]).unwrap();
    assert!(!a.join(".credentials.json").exists());
    assert!(!a.join("db.lock").exists());
    assert!(is_symlink(&a.join("settings.json")));
}

/// Optional live-FSEvents smoke test. Enable with RECIF_LIVE_WATCH=1.
#[test]
fn live_watcher_smoke() {
    if std::env::var("RECIF_LIVE_WATCH").unwrap_or_default() != "1" {
        eprintln!("skipping live watcher smoke test (set RECIF_LIVE_WATCH=1 to run)");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let master = mk_master(tmp.path());
    let a = mk_profile(&master, tmp.path(), "a");
    let config = tmp.path().join(".recif/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();

    let w = FsWatcher::new(&master, &[a.clone()], &config).unwrap();
    std::fs::write(master.join("live.json"), b"x").unwrap();

    // Wait for a debounced batch (debounce is 200ms; allow generous timeout).
    let batch = w
        .next_batch_timeout(Duration::from_secs(5))
        .expect("channel open");
    assert!(batch.master_changed, "expected master change, got {batch:?}");
}

fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}
