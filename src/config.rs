//! `~/.recif/config.toml` load/save and the profile/config data model (PRD §9).
//!
//! The config stores **canonical absolute paths** everywhere (§6.4, decision
//! #4). It is the source of truth for which profiles exist, where the master
//! is, and where the daemon logs and plist live.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

/// Current config schema version.
pub const VERSION: u32 = 1;

/// Top-level config document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    pub version: u32,
    /// Canonical absolute path of the master directory (`~/.claude` by default).
    pub master: PathBuf,
    pub daemon: DaemonConfig,
    #[serde(default, rename = "profiles")]
    pub profiles: Vec<Profile>,
}

/// Daemon-related paths (§9).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonConfig {
    pub launchd_plist: PathBuf,
    pub log_file: PathBuf,
}

/// A single profile entry (§9).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// RFC3339 timestamp (decision #3).
    pub created_at: String,
    /// Canonical absolute path of the profile directory.
    pub path: PathBuf,
}

impl Config {
    /// Build a fresh default config for the given master, using standard
    /// `~/.recif` and `~/Library/LaunchAgents` locations.
    pub fn new_default(master: PathBuf) -> Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
        let recif_dir = home.join(".recif");
        let launch_agents = home.join("Library/LaunchAgents");
        Ok(Config {
            version: VERSION,
            master,
            daemon: DaemonConfig {
                launchd_plist: launch_agents.join("com.recif.daemon.plist"),
                log_file: recif_dir.join("daemon.log"),
            },
            profiles: Vec::new(),
        })
    }

    /// Look up a profile by name.
    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    /// Insert or update a profile entry by name (idempotent).
    pub fn upsert_profile(&mut self, profile: Profile) {
        if let Some(existing) = self.profiles.iter_mut().find(|p| p.name == profile.name) {
            *existing = profile;
        } else {
            self.profiles.push(profile);
        }
    }

    /// Remove a profile entry by name; returns the removed entry if present.
    pub fn remove_profile(&mut self, name: &str) -> Option<Profile> {
        if let Some(idx) = self.profiles.iter().position(|p| p.name == name) {
            Some(self.profiles.remove(idx))
        } else {
            None
        }
    }
}

/// Default config file path: `~/.recif/config.toml`.
pub fn default_config_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    Ok(home.join(".recif/config.toml"))
}

/// Load config from a path. Errors if the file does not exist or fails to
/// parse; version mismatch is a hard error.
pub fn load(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config {}", path.display()))?;
    let config: Config = toml::from_str(&text)
        .with_context(|| format!("failed to parse config {}", path.display()))?;
    if config.version != VERSION {
        return Err(anyhow!(
            "unsupported config version {} (expected {VERSION})",
            config.version
        ));
    }
    Ok(config)
}

/// Load config if present, else `None`.
pub fn load_optional(path: &Path) -> Result<Option<Config>> {
    if path.exists() {
        Ok(Some(load(path)?))
    } else {
        Ok(None)
    }
}

/// Save config, creating the parent directory if needed. Writes atomically via
/// a temp file + rename so a crash mid-write can't corrupt the config.
pub fn save(path: &Path, config: &Config) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config dir {}", parent.display()))?;
    }
    let text = toml::to_string_pretty(config).context("failed to serialize config")?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text.as_bytes())
        .with_context(|| format!("failed to write temp config {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("failed to move config into place {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut config = Config {
            version: VERSION,
            master: PathBuf::from("/Users/fja/.claude"),
            daemon: DaemonConfig {
                launchd_plist: PathBuf::from("/Users/fja/Library/LaunchAgents/com.recif.daemon.plist"),
                log_file: PathBuf::from("/Users/fja/.recif/daemon.log"),
            },
            profiles: vec![Profile {
                name: "enzyme".into(),
                description: "Enzyme work account".into(),
                created_at: "2026-07-07T00:00:00Z".into(),
                path: PathBuf::from("/Users/fja/.claude-enzyme"),
            }],
        };
        config.upsert_profile(Profile {
            name: "rubbr".into(),
            description: String::new(),
            created_at: "2026-07-07T00:00:00Z".into(),
            path: PathBuf::from("/Users/fja/.claude-rubbr"),
        });

        save(&path, &config).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded, config);
    }

    #[test]
    fn upsert_is_idempotent() {
        let mut config = Config::new_default(PathBuf::from("/Users/fja/.claude")).unwrap();
        let p = Profile {
            name: "x".into(),
            description: "one".into(),
            created_at: "2026-07-07T00:00:00Z".into(),
            path: PathBuf::from("/Users/fja/.claude-x"),
        };
        config.upsert_profile(p.clone());
        config.upsert_profile(Profile {
            description: "two".into(),
            ..p.clone()
        });
        assert_eq!(config.profiles.len(), 1);
        assert_eq!(config.profiles[0].description, "two");
    }

    #[test]
    fn version_mismatch_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "version = 2\nmaster = \"/x\"\n[daemon]\nlaunchd_plist=\"/a\"\nlog_file=\"/b\"\n",
        )
        .unwrap();
        assert!(load(&path).is_err());
    }
}
