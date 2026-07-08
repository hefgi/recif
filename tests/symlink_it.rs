//! Integration tests for symlink primitives against a synthesized fake master.

use recif::symlink::{ensure_symlink, force_symlink, is_broken_symlink, verify_symlink, LinkOutcome};

/// Build a small fake master and mirror it into a profile via ensure_symlink,
/// then re-run to prove idempotency.
#[test]
fn mirror_master_into_profile_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let master = tmp.path().join("master");
    std::fs::create_dir(&master).unwrap();
    for f in ["settings.json", "history.jsonl"] {
        std::fs::write(master.join(f), b"x").unwrap();
    }
    std::fs::create_dir(master.join("sessions")).unwrap();

    let profile = tmp.path().join("profile");
    std::fs::create_dir(&profile).unwrap();

    for name in ["settings.json", "history.jsonl", "sessions"] {
        let out = ensure_symlink(&profile.join(name), &master.join(name)).unwrap();
        assert_eq!(out, LinkOutcome::Created);
    }
    // second pass: all correct
    for name in ["settings.json", "history.jsonl", "sessions"] {
        let out = ensure_symlink(&profile.join(name), &master.join(name)).unwrap();
        assert_eq!(out, LinkOutcome::AlreadyCorrect);
        assert!(verify_symlink(&profile.join(name), &master.join(name)).unwrap());
    }
}

#[test]
fn broken_link_detected_and_repaired() {
    let tmp = tempfile::tempdir().unwrap();
    let master = tmp.path().join("master");
    std::fs::create_dir(&master).unwrap();
    let target = master.join("f");
    std::fs::write(&target, b"x").unwrap();

    let profile = tmp.path().join("profile");
    std::fs::create_dir(&profile).unwrap();
    let link = profile.join("f");
    ensure_symlink(&link, &target).unwrap();

    std::fs::remove_file(&target).unwrap();
    assert!(is_broken_symlink(&link).unwrap());

    // reconcile would remove or recreate; force_symlink to a fresh target repairs
    std::fs::write(&target, b"y").unwrap();
    let out = force_symlink(&link, &target).unwrap();
    assert_eq!(out, LinkOutcome::Replaced);
    assert!(!is_broken_symlink(&link).unwrap());
}
