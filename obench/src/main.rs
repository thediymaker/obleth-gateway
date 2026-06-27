mod admin;
mod cli;
mod config;
mod engine;
mod persist;
mod profiles;
mod report;
mod seed;
mod seedplan;
mod target;
mod tui;

use clap::Parser;

async fn mod_tui_run(args: &cli::Cli) -> anyhow::Result<()> {
    tui::run(args).await
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = cli::Cli::parse();
    let headless = args.no_tui || (args.target.is_some() && args.profile.is_some());
    if headless {
        let (Some(tgt), Some(prof)) = (args.target, args.profile) else {
            anyhow::bail!("headless needs both --target and --profile");
        };
        let scope = cli::scope_from(args.model.clone(), args.all);
        let code = profiles::run_headless(&args, tgt, prof, scope).await?;
        std::process::exit(code);
    } else {
        mod_tui_run(&args).await
    }
}
