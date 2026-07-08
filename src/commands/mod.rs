//! Command handlers for the public CLI surface.

pub mod add;
pub mod doctor;
pub mod list;
pub mod remove;

use std::path::PathBuf;

use anyhow::{anyhow, Result};

use crate::config::{self, Config};

/// Load the config from the default path, erroring with a helpful message if it
/// doesn't exist yet.
pub fn load_config() -> Result<(PathBuf, Config)> {
    let path = config::default_config_path()?;
    let cfg = config::load_optional(&path)?
        .ok_or_else(|| anyhow!("no Recif config found at {} — run `recif add <name>` first", path.display()))?;
    Ok((path, cfg))
}

/// Validate a profile name (§8.1): non-empty, no separators/whitespace, matches
/// `[A-Za-z0-9._-]+`, and not a reserved name.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(anyhow!("profile name must not be empty"));
    }
    if name == "daemon" || name == "master" {
        return Err(anyhow!("'{name}' is a reserved name"));
    }
    let ok = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-');
    if !ok {
        return Err(anyhow!(
            "invalid profile name '{name}': only [A-Za-z0-9._-] allowed"
        ));
    }
    Ok(())
}
