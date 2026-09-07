use clap::Parser;

mod cli;
mod commands;
mod config;
mod error;
mod manifest;

use cli::{Cli, Commands, CreateResource, DeleteResource, GetResource, generate_completions};
use error::CliError;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let config_path = cli.config.clone().or_else(config::default_path);

    let result = run(cli, config_path).await;

    if let Err(err) = result {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli, config_path: Option<std::path::PathBuf>) -> Result<(), CliError> {
    match cli.command {
        Commands::Config(args) => {
            let config_path = config_path.ok_or_else(|| {
                CliError::InvalidUsage(
                    "no config path: set --barectl-config, $BARECTL_CONFIG or $HOME".to_string(),
                )
            })?;
            match args.action {
                cli::ConfigAction::Set(args) => commands::config_set(&config_path, args),
                cli::ConfigAction::View => commands::config_view(&config_path),
            }
        }
        command => {
            let file_config = match &config_path {
                Some(path) => config::load(path)?,
                None => None,
            };
            let server = config::resolve_server(cli.server, file_config.as_ref());
            let tls = config::resolve_tls(&cli.tls, file_config.as_ref())?;
            match command {
                Commands::Create(args) => match args.resource {
                    CreateResource::Pod(args) => commands::create_pod(&server, &tls, args).await,
                },
                Commands::Get(args) => match args.resource {
                    GetResource::Pod(args) => commands::get_pod(&server, &tls, args).await,
                    GetResource::Node(args) => commands::get_node(&server, &tls, args).await,
                },
                Commands::Delete(args) => match args.resource {
                    DeleteResource::Pod(args) => commands::delete_pod(&server, &tls, args).await,
                },
                Commands::Completion(args) => {
                    match generate_completions(args.shell, &mut std::io::stdout()) {
                        Ok(()) => Ok(()),
                        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
                        Err(error) => Err(error::CliError::WriteOutput(error)),
                    }
                }
                Commands::Config(_) => unreachable!("handled above"),
            }
        }
    }
}
