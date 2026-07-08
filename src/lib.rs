//! Recif — Claude Profile Switcher.
//!
//! Library crate re-exporting modules so integration tests can link against the
//! logic without going through the binary entrypoint. See `PRD.md` and
//! `IMPLEMENTATION_PLAN.md` for the full design.

pub mod aliases;
pub mod canonicalize;
pub mod cli;
pub mod commands;
pub mod config;
pub mod daemon;
pub mod daemon_status;
pub mod denylist;
pub mod health;
pub mod keychain;
pub mod launch;
pub mod launchd;
pub mod profile;
pub mod saferemove;
pub mod symlink;
pub mod tty;
