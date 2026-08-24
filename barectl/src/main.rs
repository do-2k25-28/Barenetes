use clap::Parser;

mod cli;
mod client;
mod commands;
mod error;
mod manifest;

use cli::{Cli, Commands};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::CreatePod(args) => commands::create_pod::run(&cli.server, args).await,
    };

    if let Err(err) = result {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}
