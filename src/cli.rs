//! clap command/argument definitions (PRD §8).
//!
//! Four public commands (`add`, `remove`, `list`, `doctor`) plus two internal
//! entrypoints (`daemon`, `launch`) that the aliases and launchd use but that
//! are not part of the primary public surface.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "recif",
    version,
    about = "Claude Profile Switcher — one master ~/.claude, many isolated profiles sharing it via symlinks",
    long_about = None,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create or update a profile that shares the master config via symlinks.
    Add(AddArgs),

    /// Safely remove a profile (unlinks symlinks; never touches the master).
    Remove(RemoveArgs),

    /// List all profiles and their health/sync status.
    List,

    /// Check the Recif setup and auto-fix common issues.
    Doctor,

    /// [internal] Run the sync daemon (invoked by launchd; not a public command).
    #[command(hide = true)]
    Daemon,

    /// [internal] Launch gate: health-check then exec `claude` for a profile.
    #[command(hide = true)]
    Launch(LaunchArgs),
}

#[derive(Debug, Parser)]
pub struct AddArgs {
    /// Profile name; the profile dir is `~/.claude-<name>`.
    pub name: String,

    /// Master directory to share (default: `~/.claude` or the configured master).
    #[arg(long)]
    pub master: Option<PathBuf>,

    /// Also add a shell alias `claude-<name>` routing through `recif launch`.
    #[arg(long)]
    pub alias: bool,

    /// Optional human-readable description stored in config.
    #[arg(long)]
    pub description: Option<String>,

    /// Override the rc file the alias is written to (default: detected from $SHELL).
    #[arg(long)]
    pub rc: Option<PathBuf>,

    /// Skip launchd install/start (used in tests and CI).
    #[arg(long, hide = true)]
    pub no_daemon_install: bool,
}

#[derive(Debug, Parser)]
pub struct RemoveArgs {
    /// Profile name to remove.
    pub name: String,

    /// Preserve the profile's `daemon/` directory (and root) for forensics.
    #[arg(long)]
    pub keep_daemon: bool,

    /// Skip the confirmation prompt (non-interactive use).
    #[arg(long)]
    pub yes: bool,

    /// Override the rc file the alias is removed from.
    #[arg(long)]
    pub rc: Option<PathBuf>,
}

#[derive(Debug, Parser)]
pub struct LaunchArgs {
    /// Profile name to launch.
    pub name: String,

    /// Arguments passed through to `claude` verbatim.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub passthrough: Vec<String>,
}
