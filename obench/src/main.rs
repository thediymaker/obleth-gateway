mod admin;
mod cli;
mod config;
mod engine;
mod seedplan;
mod target;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let args = cli::Cli::parse();
    let headless = args.no_tui || args.target.is_some() || args.profile.is_some();
    if headless {
        println!(
            "obench headless stub: target={:?} profile={:?} scope={:?}",
            args.target,
            args.profile,
            cli::scope_from(args.model.clone(), args.all),
        );
    } else {
        println!("obench TUI stub (no args) — TUI added in a later task");
    }
    Ok(())
}
