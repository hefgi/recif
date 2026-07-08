//! Recif — Claude Profile Switcher.
//!
//! Library crate re-exporting modules so integration tests can link against the
//! logic without going through the binary entrypoint. See `PRD.md` and
//! `IMPLEMENTATION_PLAN.md` for the full design.

pub mod canonicalize;
pub mod cli;
pub mod config;
pub mod denylist;
pub mod keychain;
