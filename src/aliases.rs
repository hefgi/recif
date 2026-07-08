//! Shell alias management (PRD §8.2, decision #6).
//!
//! Recif writes a single-line, marker-tagged alias per profile so it can be
//! found and removed idempotently. The alias routes through `recif launch`
//! (the launch gate) — never `claude` directly.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Marker appended to every Recif-managed alias line so we can find/remove them
/// without touching the user's other aliases.
const MARKER: &str = "# recif-managed";

/// The alias line for a profile (no trailing newline).
pub fn alias_line(name: &str) -> String {
    format!("alias claude-{name}=\"recif launch {name}\" {MARKER}")
}

/// Determine the default rc file: honor `override_rc`, else detect from $SHELL,
/// defaulting to `~/.zshrc` on macOS (decision #6).
pub fn default_rc_file(override_rc: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = override_rc {
        return Ok(p.to_path_buf());
    }
    let home = dirs::home_dir().context("could not determine home directory")?;
    let shell = std::env::var("SHELL").unwrap_or_default();
    let file = if shell.contains("bash") {
        ".bashrc"
    } else {
        // default to zsh on macOS
        ".zshrc"
    };
    Ok(home.join(file))
}

/// Add the alias for `name` to `rc_file`, idempotently. Returns `true` if a line
/// was added, `false` if an identical managed alias already existed.
pub fn add_alias(rc_file: &Path, name: &str) -> Result<bool> {
    let line = alias_line(name);
    let existing = read_or_empty(rc_file)?;
    if existing.lines().any(|l| l.trim() == line) {
        return Ok(false);
    }
    // Also treat a differing managed alias for the same name as present-needs-update.
    let managed_prefix = format!("alias claude-{name}=");
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|l| !(l.contains(MARKER) && l.trim_start().starts_with(&managed_prefix)))
        .map(|l| l.to_string())
        .collect();
    lines.push(line);
    write_lines(rc_file, &lines)?;
    Ok(true)
}

/// Remove the Recif-managed alias for `name` from `rc_file`. Returns `true` if a
/// line was removed.
pub fn remove_alias(rc_file: &Path, name: &str) -> Result<bool> {
    if !rc_file.exists() {
        return Ok(false);
    }
    let existing = read_or_empty(rc_file)?;
    let managed_prefix = format!("alias claude-{name}=");
    let kept: Vec<String> = existing
        .lines()
        .filter(|l| !(l.contains(MARKER) && l.trim_start().starts_with(&managed_prefix)))
        .map(|l| l.to_string())
        .collect();
    let removed = kept.len() != existing.lines().count();
    if removed {
        write_lines(rc_file, &kept)?;
    }
    Ok(removed)
}

fn read_or_empty(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn write_lines(path: &Path, lines: &[String]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut body = lines.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    std::fs::write(path, body).with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let rc = tmp.path().join(".zshrc");
        std::fs::write(&rc, "# my config\nalias ll='ls -la'\n").unwrap();

        assert!(add_alias(&rc, "enzyme").unwrap());
        assert!(!add_alias(&rc, "enzyme").unwrap()); // second time no-op

        let content = std::fs::read_to_string(&rc).unwrap();
        let count = content.matches("alias claude-enzyme=").count();
        assert_eq!(count, 1);
        assert!(content.contains("alias ll='ls -la'")); // user lines preserved
    }

    #[test]
    fn remove_only_managed_line() {
        let tmp = tempfile::tempdir().unwrap();
        let rc = tmp.path().join(".zshrc");
        add_alias(&rc, "enzyme").unwrap();
        add_alias(&rc, "rubbr").unwrap();

        assert!(remove_alias(&rc, "enzyme").unwrap());
        let content = std::fs::read_to_string(&rc).unwrap();
        assert!(!content.contains("claude-enzyme"));
        assert!(content.contains("claude-rubbr"));

        assert!(!remove_alias(&rc, "enzyme").unwrap()); // already gone
    }
}
