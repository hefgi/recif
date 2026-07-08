//! Integration tests for safe removal — the highest-value destructive-path
//! surface. These exercise the full `remove_profile` flow with a realistic
//! fake master + profile of symlinks in a tempdir (never the real ~/.claude).

use recif::saferemove::remove_profile;

/// End-to-end: a fully populated profile of symlinks + a real daemon/ dir is
/// removed cleanly, and the master survives byte-for-byte.
#[test]
fn full_profile_removal_master_survives() {
    let tmp = tempfile::tempdir().unwrap();
    let master = tmp.path().join("master");
    std::fs::create_dir(&master).unwrap();

    // Populate master with a realistic mix.
    std::fs::write(master.join("settings.json"), b"{\"a\":1}").unwrap();
    std::fs::write(master.join("history.jsonl"), b"{\"line\":1}\n").unwrap();
    std::fs::create_dir(master.join("sessions")).unwrap();
    std::fs::write(master.join("sessions/abc.jsonl"), b"session-data").unwrap();
    std::fs::create_dir(master.join("projects")).unwrap();
    std::fs::write(master.join("projects/p.json"), b"proj").unwrap();

    // Build the profile: symlinks into master + a real daemon dir + a leaked file.
    let profile = tmp.path().join(".claude-test");
    std::fs::create_dir(&profile).unwrap();
    for name in ["settings.json", "history.jsonl", "sessions", "projects"] {
        std::os::unix::fs::symlink(master.join(name), profile.join(name)).unwrap();
    }
    std::fs::create_dir(profile.join("daemon")).unwrap();
    std::fs::write(profile.join("daemon/pid"), b"999").unwrap();
    // a leaked real file the daemon hadn't moved yet
    std::fs::write(profile.join("leaked.tmp"), b"unshared").unwrap();

    let report = remove_profile(&profile, false).unwrap();
    assert_eq!(report.symlinks, 4);
    assert_eq!(report.real_dirs, 1);
    assert_eq!(report.real_files.len(), 1);
    assert!(report.root_removed);
    assert!(!profile.exists());

    // Every master byte intact.
    assert_eq!(std::fs::read(master.join("settings.json")).unwrap(), b"{\"a\":1}");
    assert_eq!(
        std::fs::read(master.join("history.jsonl")).unwrap(),
        b"{\"line\":1}\n"
    );
    assert_eq!(
        std::fs::read(master.join("sessions/abc.jsonl")).unwrap(),
        b"session-data"
    );
    assert_eq!(std::fs::read(master.join("projects/p.json")).unwrap(), b"proj");
}
