//! Integration tests for profile management against a synthesized fake master
//! in a tempdir (never the real ~/.claude, never launchd).

use std::path::Path;

use recif::config;
use recif::health::{profile_health, sync_status};
use recif::profile::reconcile_profile;
use recif::saferemove;

fn fake_master(root: &Path) -> std::path::PathBuf {
    let master = root.join("master");
    std::fs::create_dir(&master).unwrap();
    std::fs::write(master.join("settings.json"), b"cfg").unwrap();
    std::fs::write(master.join("history.jsonl"), b"{}\n").unwrap();
    std::fs::create_dir(master.join("sessions")).unwrap();
    // denied entries that must NOT be linked
    std::fs::write(master.join(".credentials.json"), b"secret").unwrap();
    std::fs::write(master.join("app.lock"), b"lock").unwrap();
    std::fs::create_dir(master.join("statsig")).unwrap();
    master
}

#[test]
fn add_via_reconcile_then_remove_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let master = fake_master(tmp.path());
    let profile = tmp.path().join(".claude-enzyme");

    // "add": reconcile the profile
    let report = reconcile_profile(&master, &profile).unwrap();
    assert!(report.daemon_recreated);

    // denied entries not present
    assert!(!profile.join(".credentials.json").exists());
    assert!(!profile.join("app.lock").exists());
    assert!(!profile.join("statsig").exists());

    // allowed entries linked
    for n in ["settings.json", "history.jsonl", "sessions"] {
        let m = std::fs::symlink_metadata(profile.join(n)).unwrap();
        assert!(m.file_type().is_symlink(), "{n} should be a symlink");
    }
    // daemon real
    assert!(profile.join("daemon").is_dir());

    // health + sync
    // NB: health checks canonicality; the tempdir path is already canonical here
    let canon = recif::canonicalize::canonicalize_profile_path(&profile).unwrap();
    let h = profile_health(&canon);
    assert!(h.healthy(), "issues: {:?}", h.issues);
    assert!(sync_status(&master, &profile).unwrap().synced());

    // "remove": safe removal leaves master intact
    let rr = saferemove::remove_profile(&profile, false).unwrap();
    assert!(rr.root_removed);
    assert!(!profile.exists());
    assert_eq!(std::fs::read(master.join("settings.json")).unwrap(), b"cfg");
    assert_eq!(std::fs::read(master.join(".credentials.json")).unwrap(), b"secret");
}

#[test]
fn config_roundtrip_with_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let master = fake_master(tmp.path());
    let config_path = tmp.path().join("config.toml");
    let profile = tmp.path().join(".claude-x");

    recif::commands::add::run_with_paths(
        &config_path,
        &master,
        &profile,
        "x",
        Some("test profile".into()),
    )
    .unwrap();

    let cfg = config::load(&config_path).unwrap();
    assert_eq!(cfg.master, master);
    assert_eq!(cfg.profiles.len(), 1);
    assert_eq!(cfg.profiles[0].name, "x");
    assert_eq!(cfg.profiles[0].path, profile);
    assert!(profile.join("daemon").is_dir());

    // idempotent second run: still one profile
    recif::commands::add::run_with_paths(&config_path, &master, &profile, "x", None).unwrap();
    let cfg2 = config::load(&config_path).unwrap();
    assert_eq!(cfg2.profiles.len(), 1);
    // description preserved from first run's created_at handling not clobbered
    assert_eq!(cfg2.profiles[0].created_at, cfg.profiles[0].created_at);
}
