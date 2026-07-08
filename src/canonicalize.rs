//! Path canonicalization invariant (PRD §6.4).
//!
//! This is the single most load-bearing primitive in Recif: Claude derives the
//! Keychain credential slot from the *literal* `CLAUDE_CONFIG_DIR` string, so a
//! wrong form (tilde vs absolute, trailing slash, un-resolved `..`) silently
//! sends a profile to a different, empty Keychain slot.
//!
//! We canonicalize to an absolute, `~`-expanded, no-trailing-slash form —
//! realpath semantics on the *parent*, but WITHOUT resolving the final
//! component (the profile dir is real; even if the user symlinked it we must
//! keep the literal name because Claude hashes the string).

use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, Context, Result};

/// Canonicalize a path per the §6.4 invariant.
///
/// Steps:
/// 1. Expand a leading `~` / `~/` to `$HOME` (reject `~user` forms).
/// 2. Make absolute (join to CWD if still relative).
/// 3. Resolve the *parent* with realpath semantics; re-append the original
///    final component so a symlinked profile dir keeps its literal name.
/// 4. Strip any trailing separator.
pub fn canonicalize_profile_path(input: &Path) -> Result<PathBuf> {
    let expanded = expand_tilde(input)?;
    let absolute = make_absolute(&expanded)?;

    // Split into parent + final component. Canonicalize the parent (resolves
    // `..`, `.`, and intermediate symlinks) and re-append the literal final
    // component.
    let file_name = absolute
        .file_name()
        .ok_or_else(|| anyhow!("path has no final component: {}", absolute.display()))?
        .to_owned();

    let parent = absolute
        .parent()
        .ok_or_else(|| anyhow!("path has no parent: {}", absolute.display()))?;

    // The parent must exist for realpath semantics. For the profile-dir case
    // the parent is $HOME, which always exists.
    let canonical_parent = std::fs::canonicalize(parent)
        .with_context(|| format!("failed to canonicalize parent {}", parent.display()))?;

    let mut result = canonical_parent;
    result.push(&file_name);
    Ok(strip_trailing_slash(result))
}

/// Expand a leading `~` or `~/` to the user's home directory. Rejects the
/// `~user` form (out of scope for v1).
fn expand_tilde(input: &Path) -> Result<PathBuf> {
    let s = input.to_string_lossy();
    if s == "~" || s.starts_with("~/") {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
        if s == "~" {
            return Ok(home);
        }
        // strip "~/"
        return Ok(home.join(&s[2..]));
    }
    if s.starts_with('~') {
        return Err(anyhow!("~user path expansion is not supported: {}", s));
    }
    Ok(input.to_path_buf())
}

/// Make a path absolute by joining to the current working directory if needed.
/// Also collapses lexical `.` and `..` components that don't need FS lookup for
/// the final segment (the parent is realpath'd separately).
fn make_absolute(input: &Path) -> Result<PathBuf> {
    if input.is_absolute() {
        return Ok(input.to_path_buf());
    }
    let cwd = std::env::current_dir().context("could not determine current directory")?;
    Ok(cwd.join(input))
}

/// Remove a trailing separator from a path, defensively. `PathBuf` normally
/// won't carry one after the operations above, but normalize regardless.
fn strip_trailing_slash(path: PathBuf) -> PathBuf {
    // Re-build from components to drop any RootDir + trailing empties.
    let s = path.to_string_lossy();
    if s.len() > 1 && s.ends_with('/') {
        return PathBuf::from(s.trim_end_matches('/').to_string());
    }
    path
}

/// Lexically collapse `.` and `..` in an already-absolute path without touching
/// the filesystem. Exposed for callers that want a normalized display form.
pub fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out: Vec<Component> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.last(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push(comp);
                }
            }
            other => out.push(other),
        }
    }
    out.iter().map(|c| c.as_os_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_expands_to_home() {
        let home = dirs::home_dir().unwrap();
        let got = canonicalize_profile_path(Path::new("~/.claude-test")).unwrap();
        assert_eq!(got, canon_parent_join(&home, ".claude-test"));
    }

    #[test]
    fn trailing_slash_stripped() {
        let got = canonicalize_profile_path(Path::new("~/.claude-test/")).unwrap();
        let s = got.to_string_lossy();
        assert!(!s.ends_with('/'), "got {s}");
    }

    #[test]
    fn tilde_and_absolute_produce_equal_output() {
        let home = dirs::home_dir().unwrap();
        let via_tilde = canonicalize_profile_path(Path::new("~/.claude-eq")).unwrap();
        let abs = home.join(".claude-eq");
        let via_abs = canonicalize_profile_path(&abs).unwrap();
        assert_eq!(via_tilde, via_abs);
    }

    #[test]
    fn reject_tilde_user() {
        assert!(canonicalize_profile_path(Path::new("~alice/.claude")).is_err());
    }

    #[test]
    fn lexical_collapses_dotdot() {
        let p = Path::new("/a/b/../c/./d");
        assert_eq!(lexical_normalize(p), PathBuf::from("/a/c/d"));
    }

    /// Build the expected result: realpath the parent (home), append literal name.
    fn canon_parent_join(home: &Path, name: &str) -> PathBuf {
        let canon_home = std::fs::canonicalize(home).unwrap();
        canon_home.join(name)
    }
}
