//! Keychain service-name derivation (PRD §6.1, §6.4).
//!
//! Claude Code derives the macOS Keychain credential service name from the
//! config-directory path:
//!
//! ```text
//! Claude Code-credentials-<first-8-hex-of-sha256(absolute-path-string)>
//! ```
//!
//! The default master `~/.claude` uses the *un-suffixed* `Claude Code-credentials`
//! slot. Recif NEVER reads or writes credentials — this module only derives the
//! service name and (for doctor) checks slot existence by shelling out
//! read-only to `security find-generic-password`.

use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};

/// The un-suffixed default Keychain service name for the master `~/.claude`.
pub const DEFAULT_SERVICE: &str = "Claude Code-credentials";

/// Compute the first-8-hex-chars of sha256(path string).
pub fn path_hash(canonical: &Path) -> String {
    let s = canonical.to_string_lossy();
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let digest = hasher.finalize();
    // hex-encode and take the first 8 characters
    let hex = digest.iter().map(|b| format!("{b:02x}")).collect::<String>();
    hex[..8].to_string()
}

/// Derive the Keychain service name for a canonical config-dir path.
///
/// `default_master` is the canonical path of the master `~/.claude`; when
/// `canonical` equals it, the un-suffixed default service is returned (§6.1).
pub fn derive_service_name(canonical: &Path, default_master: &Path) -> String {
    if canonical == default_master {
        return DEFAULT_SERVICE.to_string();
    }
    format!("{DEFAULT_SERVICE}-{}", path_hash(canonical))
}

/// Both service-name forms to probe for a profile: the default un-suffixed slot
/// and the `-<hash>` suffixed slot. Doctor checks whether *either* exists (§6.1).
pub fn candidate_service_names(canonical: &Path, default_master: &Path) -> Vec<String> {
    let mut names = vec![format!("{DEFAULT_SERVICE}-{}", path_hash(canonical))];
    if canonical == default_master {
        names.push(DEFAULT_SERVICE.to_string());
    }
    names
}

/// Check whether a Keychain slot exists for the given service name.
///
/// Read-only: runs `security find-generic-password -s <service>`. Returns
/// `true` if the entry exists. Never reads the credential value. Non-macOS
/// platforms return `false` (no Keychain).
pub fn slot_exists(service: &str) -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    Command::new("security")
        .args(["find-generic-password", "-s", service])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn dead0d42_vector() {
        // §6.1 / §11.1 Part C pinned vector: the absolute path
        // "/Users/<you>/.claude-enzyme" hashes to dead0d42. The empirical value
        // was measured against a specific home; we pin the algorithm against the
        // literal string that produced it.
        let p = PathBuf::from("/Users/fja/.claude-enzyme");
        assert_eq!(path_hash(&p), "dead0d42");
    }

    #[test]
    fn default_master_is_unsuffixed() {
        let master = PathBuf::from("/Users/fja/.claude");
        assert_eq!(derive_service_name(&master, &master), DEFAULT_SERVICE);
    }

    #[test]
    fn profile_is_suffixed() {
        let master = PathBuf::from("/Users/fja/.claude");
        let profile = PathBuf::from("/Users/fja/.claude-enzyme");
        assert_eq!(
            derive_service_name(&profile, &master),
            "Claude Code-credentials-dead0d42"
        );
    }

    #[test]
    fn tilde_vs_absolute_differ() {
        // Guards §11.1 Part C: an unexpanded tilde must hash differently from
        // the absolute form, so canonicalization is mandatory before hashing.
        let abs = PathBuf::from("/Users/fja/.claude-enzyme");
        let tilde = PathBuf::from("~/.claude-enzyme");
        assert_ne!(path_hash(&abs), path_hash(&tilde));
    }
}
