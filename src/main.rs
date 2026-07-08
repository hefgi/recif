//! Recif binary entrypoint: parse args and dispatch to command handlers.

use std::process::ExitCode;

use clap::Parser;
use colored::Colorize;

use recif::cli::{Cli, Command};
use recif::commands;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Add(args) => commands::add::run(args),
        Command::Remove(args) => commands::remove::run(args),
        Command::List => commands::list::run(),
        Command::Doctor => commands::doctor::run(),
        Command::Daemon => recif::daemon::run(),
        Command::Launch(args) => recif::launch::run(args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{} {:#}", "error:".red().bold(), e);
            ExitCode::from(1)
        }
    }
}
