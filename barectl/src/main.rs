use clap::Parser;

mod cli;
mod commands;
mod error;
mod manifest;

use cli::{Cli, Commands, CreateResource, DeleteResource, GetResource, generate_completions};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Create(args) => match args.resource {
            CreateResource::Pod(args) => commands::create_pod(&cli.server, &cli.tls, args).await,
        },
        Commands::Get(args) => match args.resource {
            GetResource::Pod(args) => commands::get_pod(&cli.server, &cli.tls, args).await,
            GetResource::Node(args) => commands::get_node(&cli.server, &cli.tls, args).await,
        },
        Commands::Delete(args) => match args.resource {
            DeleteResource::Pod(args) => commands::delete_pod(&cli.server, &cli.tls, args).await,
        },
        Commands::Completion(args) => {
            match generate_completions(args.shell, &mut std::io::stdout()) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
                Err(error) => Err(error::CliError::WriteOutput(error)),
            }
        }
    };

    if let Err(err) = result {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}
