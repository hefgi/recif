//! Launch gate (PRD §7.7, §8.2).
//!
//! `recif launch <name>` looks up the profile's canonical `CLAUDE_CONFIG_DIR`,
//! fast-checks daemon health, and:
//!
//! - healthy → `exec claude` (replace process image) with the env var set and
//!   all passthrough args forwarded, so it behaves exactly like bare `claude`;
//! - unhealthy → warn + prompt `[y/N]`; `y` runs doctor and re-checks, anything
//!   else aborts non-zero. Non-interactive stdin aborts non-zero (scriptable).
//!
//! The gate protects **sync integrity**, not concurrent-write safety on
//! `history.jsonl` (§7.7 scope note) — no history locking here.

use std::io::Write;
#[cfg(any(test, feature = "testutil"))]
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use colored::Colorize;

use crate::cli::LaunchArgs;
use crate::config::Config;
use crate::daemon_status;
use crate::launchd::{DaemonState, Launchd, ServiceManager};

/// Result of a health check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Healthy,
    Unhealthy,
}

/// Abstraction so the gate is testable without launchd or a real `claude`.
pub trait LaunchEnv {
    /// Fast daemon health check.
    fn health(&self) -> Health;
    /// Attempt to repair the daemon (runs doctor's reinstall/reload). Returns
    /// the health after the attempt.
    fn repair(&self) -> Result<Health>;
    /// Whether stdin is an interactive TTY.
    fn is_interactive(&self) -> bool;
    /// exec `claude` with the given config dir and passthrough args. On success
    /// this never returns (process image replaced); returns Err otherwise.
    fn exec_claude(&self, config_dir: &std::path::Path, args: &[String]) -> Result<std::convert::Infallible>;
}

/// Production environment backed by launchd + a real exec.
pub struct RealLaunchEnv {
    svc: Launchd,
    cfg: Config,
}

impl RealLaunchEnv {
    pub fn new(cfg: Config, config_path: std::path::PathBuf) -> Self {
        let svc = Launchd::new(
            cfg.daemon.launchd_plist.clone(),
            cfg.daemon.log_file.clone(),
            config_path,
        );
        RealLaunchEnv { svc, cfg }
    }
}

impl LaunchEnv for RealLaunchEnv {
    fn health(&self) -> Health {
        let state = self.svc.state().unwrap_or(DaemonState::NotLoaded);
        let fresh = daemon_status::heartbeat_fresh(&self.cfg);
        // Healthy = running AND heartbeat not explicitly stale. If no heartbeat
        // file exists yet (None), fall back to "running" per §9.
        if matches!(state, DaemonState::Running) && fresh != Some(false) {
            Health::Healthy
        } else {
            Health::Unhealthy
        }
    }

    fn repair(&self) -> Result<Health> {
        let exe = std::env::current_exe()?;
        if self.svc.plist_exists() {
            self.svc.reload()?;
        } else {
            self.svc.install(&exe)?;
        }
        // Give launchd a moment to bring the daemon up.
        std::thread::sleep(std::time::Duration::from_millis(500));
        Ok(self.health())
    }

    fn is_interactive(&self) -> bool {
        crate::tty::stdin_is_tty()
    }

    fn exec_claude(
        &self,
        config_dir: &std::path::Path,
        args: &[String],
    ) -> Result<std::convert::Infallible> {
        exec_claude_impl(config_dir, args)
    }
}

/// The pure decision logic, parameterized over the environment so it is unit
/// testable. Returns the config dir to exec with once the gate is passed, or an
/// error that the caller turns into a non-zero exit.
pub fn run(args: LaunchArgs) -> Result<()> {
    let (config_path, cfg) = crate::commands::load_config()?;
    let profile = cfg
        .profile(&args.name)
        .ok_or_else(|| anyhow!("no such profile '{}'", args.name))?;
    let config_dir = profile.path.clone();

    let env = RealLaunchEnv::new(cfg.clone(), config_path);
    gate_and_exec(&env, &config_dir, &args.passthrough)
}

/// Shared gate: check health, prompt/repair if needed, then exec.
pub fn gate_and_exec(
    env: &dyn LaunchEnv,
    config_dir: &std::path::Path,
    passthrough: &[String],
) -> Result<()> {
    match env.health() {
        Health::Healthy => {
            // never returns on success
            env.exec_claude(config_dir, passthrough)?;
            unreachable!("exec replaced the process image");
        }
        Health::Unhealthy => {
            eprintln!(
                "{} Recif sync daemon is not running. Your profiles may fall out of sync.",
                "⚠".yellow()
            );
            if !env.is_interactive() {
                return Err(anyhow!(
                    "daemon unhealthy and no TTY to prompt; run `recif doctor` then retry"
                ));
            }
            eprint!("Fix it now (runs `recif doctor`)? [y/N] ");
            std::io::stderr().flush().ok();
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            if matches!(line.trim(), "y" | "Y" | "yes" | "Yes") {
                match env.repair()? {
                    Health::Healthy => {
                        env.exec_claude(config_dir, passthrough)?;
                        unreachable!("exec replaced the process image");
                    }
                    Health::Unhealthy => Err(anyhow!(
                        "daemon still unhealthy after repair; not launching (see `recif doctor`)"
                    )),
                }
            } else {
                Err(anyhow!("aborted: not launching with an unhealthy sync daemon"))
            }
        }
    }
}

/// Actually replace the process image with `claude`.
#[cfg(unix)]
fn exec_claude_impl(
    config_dir: &std::path::Path,
    args: &[String],
) -> Result<std::convert::Infallible> {
    use std::os::unix::process::CommandExt;

    let mut cmd = std::process::Command::new("claude");
    cmd.env("CLAUDE_CONFIG_DIR", config_dir);
    cmd.args(args);
    // exec() only returns on failure.
    let err = cmd.exec();
    Err(anyhow!(
        "failed to exec claude (is it on your PATH?): {err}"
    ))
}

#[cfg(not(unix))]
fn exec_claude_impl(
    _config_dir: &std::path::Path,
    _args: &[String],
) -> Result<std::convert::Infallible> {
    Err(anyhow!("recif launch requires a unix platform for exec"))
}

/// Test double so the gate logic can be exercised without launchd/exec.
#[cfg(any(test, feature = "testutil"))]
pub struct FakeLaunchEnv {
    pub health_states: std::cell::RefCell<Vec<Health>>,
    pub interactive: bool,
    pub repaired_health: Health,
    pub execed: std::cell::RefCell<Option<(PathBuf, Vec<String>)>>,
}

#[cfg(any(test, feature = "testutil"))]
impl FakeLaunchEnv {
    pub fn new(initial: Health, interactive: bool, repaired: Health) -> Self {
        FakeLaunchEnv {
            health_states: std::cell::RefCell::new(vec![initial]),
            interactive,
            repaired_health: repaired,
            execed: std::cell::RefCell::new(None),
        }
    }
}

#[cfg(any(test, feature = "testutil"))]
impl LaunchEnv for FakeLaunchEnv {
    fn health(&self) -> Health {
        let states = self.health_states.borrow();
        *states.last().unwrap()
    }
    fn repair(&self) -> Result<Health> {
        self.health_states.borrow_mut().push(self.repaired_health);
        Ok(self.repaired_health)
    }
    fn is_interactive(&self) -> bool {
        self.interactive
    }
    fn exec_claude(
        &self,
        config_dir: &std::path::Path,
        args: &[String],
    ) -> Result<std::convert::Infallible> {
        *self.execed.borrow_mut() = Some((config_dir.to_path_buf(), args.to_vec()));
        // Simulate a successful exec by returning an error that the caller
        // treats as "process replaced" — but in tests we assert on `execed`
        // BEFORE reaching the unreachable!(), so we short-circuit via a sentinel
        // error the test can detect.
        Err(anyhow!("__EXECED__"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        PathBuf::from("/Users/fja/.claude-test")
    }

    #[test]
    fn healthy_execs_with_passthrough() {
        let env = FakeLaunchEnv::new(Health::Healthy, true, Health::Healthy);
        let args = vec!["--resume".to_string(), "foo".to_string()];
        let res = gate_and_exec(&env, &dir(), &args);
        // exec_claude short-circuits with the __EXECED__ sentinel
        assert!(res.unwrap_err().to_string().contains("__EXECED__"));
        let (cd, passed) = env.execed.borrow().clone().unwrap();
        assert_eq!(cd, dir());
        assert_eq!(passed, args);
    }

    #[test]
    fn unhealthy_non_tty_aborts() {
        let env = FakeLaunchEnv::new(Health::Unhealthy, false, Health::Healthy);
        let res = gate_and_exec(&env, &dir(), &[]);
        let msg = res.unwrap_err().to_string();
        assert!(msg.contains("no TTY"), "{msg}");
        assert!(env.execed.borrow().is_none());
    }
}
