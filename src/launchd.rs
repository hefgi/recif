//! launchd integration (PRD §7.2, §8.3) behind a thin trait so Linux
//! (`systemd --user`) can slot in during Phase 3 and so tests can mock it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The default service label used for the Recif daemon launchd agent.
pub const LABEL: &str = "com.recif.daemon";

/// Derive the launchd label from a plist path: the file stem
/// (`com.recif.daemon.plist` → `com.recif.daemon`). This keeps the `Label` key
/// in the plist and the `launchctl list <label>` probe in agreement, and lets
/// an isolated dry-run use a distinct plist (`com.recif.daemon.test.plist`)
/// without any risk of colliding with the real agent.
pub fn label_from_plist(plist_path: &Path) -> String {
    plist_path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| LABEL.to_string())
}

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
}

/// Real macOS launchd implementation.
pub struct Launchd {
    plist_path: PathBuf,
    log_file: PathBuf,
    config_path: PathBuf,
    label: String,
}

impl Launchd {
    /// `config_path` is baked into the plist as `daemon --config <path>` so the
    /// daemon never depends on launchd's environment to find its config.
    pub fn new(plist_path: PathBuf, log_file: PathBuf, config_path: PathBuf) -> Self {
        let label = label_from_plist(&plist_path);
        Launchd {
            plist_path,
            log_file,
            config_path,
            label,
        }
    }

    /// The launchd label this instance manages (derived from the plist path).
    pub fn label(&self) -> &str {
        &self.label
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
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>daemon</string>
        <string>--config</string>
        <string>{config}</string>
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
            label = self.label,
            exe = exe.display(),
            config = self.config_path.display(),
        )
    }
}

/// Parse the output of `launchctl list <label>` into a daemon state.
///
/// Verified against real macOS output: a running agent's dict contains a
/// `"PID" = <n>;` line; a loaded-but-stopped agent has no `"PID"` key at all
/// (only `"LastExitStatus"`). Extracted as a pure function so it is unit
/// testable against captured fixtures.
pub fn parse_list_output(text: &str) -> DaemonState {
    let has_pid = text.lines().any(|l| l.trim_start().starts_with("\"PID\""));
    if has_pid {
        DaemonState::Running
    } else {
        DaemonState::LoadedNoPid
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
            .args(["list", &self.label])
            .output();
        let output = match output {
            Ok(o) => o,
            Err(_) => return Ok(DaemonState::NotLoaded),
        };
        if !output.status.success() {
            // Non-zero exit means the label isn't loaded.
            return Ok(DaemonState::NotLoaded);
        }
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(parse_list_output(&text))
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_contains_daemon_args_and_paths() {
        let l = Launchd::new(
            PathBuf::from("/tmp/com.recif.daemon.plist"),
            PathBuf::from("/tmp/daemon.log"),
            PathBuf::from("/Users/x/.recif/config.toml"),
        );
        let xml = l.plist_xml(Path::new("/usr/local/bin/recif"));
        assert!(xml.contains("<string>com.recif.daemon</string>"));
        assert!(xml.contains("<string>/usr/local/bin/recif</string>"));
        assert!(xml.contains("<string>daemon</string>"));
        assert!(xml.contains("<key>KeepAlive</key>"));
        assert!(xml.contains("/tmp/daemon.log"));
        // The config path MUST be pinned in the plist args (launchd does not
        // inherit HOME); otherwise the daemon crash-loops.
        assert!(xml.contains("<string>--config</string>"));
        assert!(xml.contains("<string>/Users/x/.recif/config.toml</string>"));
    }

    #[test]
    fn label_derived_from_plist_and_matches_xml() {
        let l = Launchd::new(
            PathBuf::from("/tmp/com.recif.daemon.test.plist"),
            PathBuf::from("/tmp/daemon.log"),
            PathBuf::from("/tmp/config.toml"),
        );
        assert_eq!(l.label(), "com.recif.daemon.test");
        let xml = l.plist_xml(Path::new("/bin/recif"));
        // The Label key and the probe label must agree.
        assert!(xml.contains("<string>com.recif.daemon.test</string>"));
    }

    // Fixtures captured from real macOS `launchctl list <label>` output.
    #[test]
    fn parse_running_agent_fixture() {
        // A running agent's dict contains a "PID" line.
        let running = r#"{
	"LastExitStatus" = 0;
	"PID" = 69684;
	"Label" = "com.example.thing";
}"#;
        assert_eq!(parse_list_output(running), DaemonState::Running);
    }

    #[test]
    fn parse_stopped_agent_fixture() {
        // A loaded-but-stopped agent has NO "PID" key, only LastExitStatus.
        let stopped = r#"{
	"LastExitStatus" = 0;
	"Label" = "com.apple.SafariHistoryServiceAgent";
}"#;
        assert_eq!(parse_list_output(stopped), DaemonState::LoadedNoPid);
    }
}
