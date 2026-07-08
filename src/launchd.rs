//! launchd integration (PRD §7.2, §8.3) behind a thin trait so Linux
//! (`systemd --user`) can slot in during Phase 3 and so tests can mock it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The service label used for the Recif daemon launchd agent.
pub const LABEL: &str = "com.recif.daemon";

/// Health of the daemon service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonState {
    /// Agent loaded and has a live PID.
    Running,
    /// Agent loaded but no live PID (crashed / not started).
    LoadedNoPid,
    /// Agent not loaded at all.
    NotLoaded,
}

/// Abstraction over the service manager so callers (add/doctor/launch) don't
/// depend on launchctl directly.
pub trait ServiceManager {
    /// Install (write plist) and (re)load the daemon agent. Idempotent.
    fn install(&self, exe: &Path) -> Result<()>;
    /// Query current daemon state.
    fn state(&self) -> Result<DaemonState>;
    /// Reload (unload + load) the agent, restarting the daemon.
    fn reload(&self) -> Result<()>;
    /// Whether the plist file exists on disk.
    fn plist_exists(&self) -> bool;
}

/// Real macOS launchd implementation.
pub struct Launchd {
    plist_path: PathBuf,
    log_file: PathBuf,
}

impl Launchd {
    pub fn new(plist_path: PathBuf, log_file: PathBuf) -> Self {
        Launchd {
            plist_path,
            log_file,
        }
    }

    /// Generate the plist XML for the daemon given the recif binary path.
    pub fn plist_xml(&self, exe: &Path) -> String {
        let log = self.log_file.display();
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>daemon</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
            exe = exe.display(),
        )
    }
}

impl ServiceManager for Launchd {
    fn install(&self, exe: &Path) -> Result<()> {
        if let Some(parent) = self.plist_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create LaunchAgents dir {}", parent.display()))?;
        }
        if let Some(parent) = self.log_file.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&self.plist_path, self.plist_xml(exe))
            .with_context(|| format!("failed to write plist {}", self.plist_path.display()))?;
        self.reload()
    }

    fn state(&self) -> Result<DaemonState> {
        if !cfg!(target_os = "macos") {
            return Ok(DaemonState::NotLoaded);
        }
        let output = std::process::Command::new("launchctl")
            .args(["list", LABEL])
            .output();
        let output = match output {
            Ok(o) => o,
            Err(_) => return Ok(DaemonState::NotLoaded),
        };
        if !output.status.success() {
            return Ok(DaemonState::NotLoaded);
        }
        let text = String::from_utf8_lossy(&output.stdout);
        // `launchctl list <label>` prints a plist-ish dict with "PID" = N; if the
        // service isn't running there's no PID key (or it's "-").
        let has_pid = text.lines().any(|l| {
            let t = l.trim();
            t.starts_with("\"PID\"") && !t.contains("= -") && !t.ends_with("= 0;")
        });
        if has_pid {
            Ok(DaemonState::Running)
        } else {
            Ok(DaemonState::LoadedNoPid)
        }
    }

    fn reload(&self) -> Result<()> {
        if !cfg!(target_os = "macos") {
            return Ok(());
        }
        // unload (ignore error if not loaded), then load.
        let _ = std::process::Command::new("launchctl")
            .args(["unload", &self.plist_path.to_string_lossy()])
            .output();
        let out = std::process::Command::new("launchctl")
            .args(["load", &self.plist_path.to_string_lossy()])
            .output()
            .context("failed to run launchctl load")?;
        if !out.status.success() {
            anyhow::bail!(
                "launchctl load failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(())
    }

    fn plist_exists(&self) -> bool {
        self.plist_path.exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_contains_daemon_args_and_paths() {
        let l = Launchd::new(
            PathBuf::from("/tmp/com.recif.daemon.plist"),
            PathBuf::from("/tmp/daemon.log"),
        );
        let xml = l.plist_xml(Path::new("/usr/local/bin/recif"));
        assert!(xml.contains("<string>com.recif.daemon</string>"));
        assert!(xml.contains("<string>/usr/local/bin/recif</string>"));
        assert!(xml.contains("<string>daemon</string>"));
        assert!(xml.contains("<key>KeepAlive</key>"));
        assert!(xml.contains("/tmp/daemon.log"));
    }
}
