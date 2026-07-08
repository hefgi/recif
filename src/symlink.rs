//! Low-level symlink primitives (create / replace / verify).
//!
//! All operations are idempotent and use `symlink_metadata` (never `metadata`)
//! so they classify the link itself, never the target. These are the building
//! blocks for `profile.rs` reconcile and the daemon.

use std::path::Path;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SymlinkError {
    #[error("io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("path {0} exists and is a real file/dir, not a symlink (refusing to clobber)")]
    NotASymlink(String),
}

type Result<T> = std::result::Result<T, SymlinkError>;

fn io_err(path: &Path, source: std::io::Error) -> SymlinkError {
    SymlinkError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// What [`ensure_symlink`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkOutcome {
    /// Link already existed and pointed at the correct target.
    AlreadyCorrect,
    /// Link was created fresh.
    Created,
    /// A wrong/broken link was replaced with the correct one.
    Replaced,
}

/// Return the current symlink target if `link` is a symlink, else `None`.
/// Does not follow the link or require the target to exist.
pub fn read_link_target(link: &Path) -> Result<Option<std::path::PathBuf>> {
    match std::fs::symlink_metadata(link) {
        Ok(meta) if meta.file_type().is_symlink() => {
            let target = std::fs::read_link(link).map_err(|e| io_err(link, e))?;
            Ok(Some(target))
        }
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(link, e)),
    }
}

/// Return `true` if `link` is a symlink whose target no longer exists (broken).
pub fn is_broken_symlink(link: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(link) {
        Ok(meta) if meta.file_type().is_symlink() => {
            // metadata() follows the link; NotFound => broken.
            match std::fs::metadata(link) {
                Ok(_) => Ok(false),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
                Err(e) => Err(io_err(link, e)),
            }
        }
        Ok(_) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_err(link, e)),
    }
}

/// Ensure `link` is a symlink pointing at `target`, idempotently.
///
/// - If `link` doesn't exist → create it.
/// - If `link` is a symlink with the wrong/broken target → replace it.
/// - If `link` is a symlink already pointing at `target` → no-op.
/// - If `link` is a **real** file/dir → error (`NotASymlink`); the caller
///   decides policy (the §7.5 "master wins" flow lives in reconcile, not here).
pub fn ensure_symlink(link: &Path, target: &Path) -> Result<LinkOutcome> {
    match std::fs::symlink_metadata(link) {
        Ok(meta) => {
            if !meta.file_type().is_symlink() {
                return Err(SymlinkError::NotASymlink(link.display().to_string()));
            }
            let current = std::fs::read_link(link).map_err(|e| io_err(link, e))?;
            if current == target {
                return Ok(LinkOutcome::AlreadyCorrect);
            }
            // Wrong or broken target: replace.
            std::fs::remove_file(link).map_err(|e| io_err(link, e))?;
            create_symlink(link, target)?;
            Ok(LinkOutcome::Replaced)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            create_symlink(link, target)?;
            Ok(LinkOutcome::Created)
        }
        Err(e) => Err(io_err(link, e)),
    }
}

/// Force-replace whatever is at `link` (real file, symlink, or nothing) with a
/// symlink to `target`. Used by the §7.5 "master wins" path where a real
/// profile-local file must be overwritten by a link to the master copy.
///
/// Refuses to remove a real *directory* here (that would be destructive and is
/// handled by the move-to-master flow); only real files and symlinks are
/// replaced.
pub fn force_symlink(link: &Path, target: &Path) -> Result<LinkOutcome> {
    match std::fs::symlink_metadata(link) {
        Ok(meta) => {
            let ft = meta.file_type();
            if ft.is_dir() {
                return Err(SymlinkError::NotASymlink(link.display().to_string()));
            }
            // symlink or real file → remove the link/file only.
            std::fs::remove_file(link).map_err(|e| io_err(link, e))?;
            create_symlink(link, target)?;
            Ok(LinkOutcome::Replaced)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            create_symlink(link, target)?;
            Ok(LinkOutcome::Created)
        }
        Err(e) => Err(io_err(link, e)),
    }
}

/// Verify that `link` is a symlink pointing exactly at `target` (no following).
pub fn verify_symlink(link: &Path, target: &Path) -> Result<bool> {
    Ok(read_link_target(link)?.as_deref() == Some(target))
}

#[cfg(unix)]
fn create_symlink(link: &Path, target: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link).map_err(|e| io_err(link, e))
}

#[cfg(not(unix))]
fn create_symlink(link: &Path, target: &Path) -> Result<()> {
    compile_error!("Recif currently supports unix symlink semantics only");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_then_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("t.txt");
        std::fs::write(&target, b"x").unwrap();
        let link = dir.path().join("l");

        assert_eq!(ensure_symlink(&link, &target).unwrap(), LinkOutcome::Created);
        assert_eq!(
            ensure_symlink(&link, &target).unwrap(),
            LinkOutcome::AlreadyCorrect
        );
        assert!(verify_symlink(&link, &target).unwrap());
    }

    #[test]
    fn replace_wrong_target() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let link = dir.path().join("l");

        ensure_symlink(&link, &a).unwrap();
        assert_eq!(ensure_symlink(&link, &b).unwrap(), LinkOutcome::Replaced);
        assert!(verify_symlink(&link, &b).unwrap());
    }

    #[test]
    fn repair_broken_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("gone.txt");
        std::fs::write(&target, b"x").unwrap();
        let link = dir.path().join("l");
        ensure_symlink(&link, &target).unwrap();

        std::fs::remove_file(&target).unwrap();
        assert!(is_broken_symlink(&link).unwrap());

        // recreate target and re-ensure -> already points at it, correct.
        std::fs::write(&target, b"x").unwrap();
        assert_eq!(
            ensure_symlink(&link, &target).unwrap(),
            LinkOutcome::AlreadyCorrect
        );
        assert!(!is_broken_symlink(&link).unwrap());
    }

    #[test]
    fn ensure_refuses_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::write(&real, b"x").unwrap();
        let target = dir.path().join("t");
        std::fs::write(&target, b"t").unwrap();
        assert!(matches!(
            ensure_symlink(&real, &target),
            Err(SymlinkError::NotASymlink(_))
        ));
    }

    #[test]
    fn force_replaces_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::write(&real, b"x").unwrap();
        let target = dir.path().join("t");
        std::fs::write(&target, b"t").unwrap();
        assert_eq!(force_symlink(&real, &target).unwrap(), LinkOutcome::Replaced);
        assert!(verify_symlink(&real, &target).unwrap());
    }
}
