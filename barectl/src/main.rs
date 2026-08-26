use clap::Parser;

mod cli;
mod commands;
mod error;

use cli::{Cli, Commands};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::CreatePod(args) => commands::create_pod(&cli.server, args).await,
        Commands::GetPod(args) => commands::get_pod(&cli.server, args).await
    };

    if let Err(err) = result {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}
