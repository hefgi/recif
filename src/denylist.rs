//! Symlink denylist — the single source of truth (PRD §6.2, §6.5).
//!
//! Consumed by `add`, the daemon, and `doctor` alike so they never drift. The
//! rule is a *predicate*, not a hardcoded allowlist: everything in the master
//! top level is symlinked EXCEPT entries matching a denylist rule.

use std::path::Path;

use anyhow::{Context, Result};
use regex_lite::Regex;

/// Exact entry names that are never symlinked (§6.5).
const DENIED_EXACT: &[&str] = &[
    "daemon",
    ".credentials.json",
    // Per-account identity/state (oauthAccount, userID, per-project state).
    // Normally lives at $HOME/.claude.json (outside the master), but a stray
    // copy inside a config dir must never be shared — sharing it would collapse
    // account identity across profiles. Defense-in-depth: never symlink it.
    ".claude.json",
    "statsig",
    ".git",
    ".gitignore",
];

/// Return `true` if a top-level master entry must NOT be symlinked into
/// profiles.
///
/// `entry_is_dir` is accepted for future rules that depend on entry kind; the
/// current §6.5 rules are name-based.
pub fn is_denied(entry_name: &str, _entry_is_dir: bool) -> bool {
    if DENIED_EXACT.contains(&entry_name) {
        return true;
    }
    // Glob/suffix families (§6.5):
    //   *.lock
    //   *.db, *.db-wal, *.db-shm, *.sqlite* (e.g. __store.db)
    if entry_name.ends_with(".lock") {
        return true;
    }
    if entry_name.ends_with(".db")
        || entry_name.ends_with(".db-wal")
        || entry_name.ends_with(".db-shm")
    {
        return true;
    }
    if let Some(pos) = entry_name.find(".sqlite") {
        // matches *.sqlite and *.sqlite3, *.sqlite-wal, etc.
        // guard against a leading-dot filename like ".sqlite" still matching
        let _ = pos;
        return true;
    }
    false
}

/// Compiled matcher equivalent to [`is_denied`], kept for callers that want to
/// reuse a single regex instance. Currently a thin wrapper; the direct
/// predicate is preferred for clarity.
pub fn denied_pattern() -> Regex {
    // *.lock | *.db | *.db-wal | *.db-shm | *.sqlite*
    Regex::new(r"(\.lock|\.db|\.db-wal|\.db-shm|\.sqlite.*)$").expect("static regex is valid")
}

/// The desired set of symlink names for a given master: every top-level entry
/// not denied. This is the shared function `add`, daemon reconcile, and doctor
/// all call so their symlink sets never drift (§6.2).
///
/// Returns names sorted for determinism.
pub fn desired_symlinks(master: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let rd = std::fs::read_dir(master)
        .with_context(|| format!("failed to read master dir {}", master.display()))?;
    for entry in rd {
        let entry = entry.with_context(|| format!("failed to read entry in {}", master.display()))?;
        let name = entry.file_name().to_string_lossy().to_string();
        // Classify without following symlinks; master entries are normally real.
        let is_dir = entry
            .file_type()
            .map(|ft| ft.is_dir())
            .unwrap_or(false);
        if !is_denied(&name, is_dir) {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denylist_table() {
        // Denied (§6.5)
        for (name, is_dir) in [
            ("daemon", true),
            (".credentials.json", false),
            (".claude.json", false),
            ("statsig", true),
            (".git", true),
            (".gitignore", false),
            ("something.lock", false),
            ("__store.db", false),
            ("__store.db-wal", false),
            ("__store.db-shm", false),
            ("data.sqlite", false),
            ("data.sqlite3", false),
        ] {
            assert!(is_denied(name, is_dir), "{name} should be denied");
        }

        // Allowed — representative real Claude entries
        for (name, is_dir) in [
            ("settings.json", false),
            ("settings.local.json", false),
            ("history.jsonl", false),
            ("sessions", true),
            ("tasks", true),
            ("projects", true),
            ("plans", true),
            ("commands", true),
            ("remote-settings.json", false),
            ("policy-limits.json", false),
            ("todos", true),
            ("hooks", true),
            ("plugins", true),
        ] {
            assert!(!is_denied(name, is_dir), "{name} should be allowed");
        }
    }

    #[test]
    fn desired_symlinks_filters_and_sorts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for f in [
            "settings.json",
            "history.jsonl",
            ".credentials.json",
            "foo.lock",
            "__store.db",
        ] {
            std::fs::write(root.join(f), b"x").unwrap();
        }
        std::fs::create_dir(root.join("daemon")).unwrap();
        std::fs::create_dir(root.join("sessions")).unwrap();
        std::fs::create_dir(root.join("statsig")).unwrap();

        let got = desired_symlinks(root).unwrap();
        assert_eq!(got, vec!["history.jsonl", "sessions", "settings.json"]);
    }
}
