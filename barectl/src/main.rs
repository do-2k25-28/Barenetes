use clap::Parser;

mod cli;
mod commands;
mod error;
mod manifest;

use cli::{Cli, Commands, CreateResource, DeleteResource, GetResource};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Create(args) => match args.resource {
            CreateResource::Pod(args) => commands::create_pod(&cli.server, args).await,
        },
        Commands::Get(args) => match args.resource {
            GetResource::Pod(args) => commands::get_pod(&cli.server, args).await,
            GetResource::Node(args) => commands::get_node(&cli.server, args).await,
        },
        Commands::Delete(args) => match args.resource {
            DeleteResource::Pod(args) => commands::delete_pod(&cli.server, args).await,
        },
    };

    if let Err(err) = result {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}
