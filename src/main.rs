//! Recif binary entrypoint: parse args and dispatch to command handlers.

use anyhow::Result;
use clap::Parser;

use recif::cli::{Cli, Command};

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Add(_) => not_implemented("add"),
        Command::Remove(_) => not_implemented("remove"),
        Command::List => not_implemented("list"),
        Command::Doctor => not_implemented("doctor"),
        Command::Daemon => not_implemented("daemon"),
        Command::Launch(_) => not_implemented("launch"),
    }
}

fn not_implemented(name: &str) -> Result<()> {
    eprintln!("recif {name}: not implemented yet");
    std::process::exit(2);
}
