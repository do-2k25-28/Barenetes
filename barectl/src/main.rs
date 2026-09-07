use clap::Parser;

mod cli;
mod commands;
mod config;
mod error;
mod manifest;

use cli::{Cli, Commands, ConfigAction, CreateResource, DeleteResource, GetResource};
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
    if let Commands::Config(args) = cli.command {
        let config_path = config_path.ok_or_else(|| {
            CliError::InvalidUsage(
                "no config path: set --barectl-config, $BARECTL_CONFIG or $HOME".to_string(),
            )
        })?;
        return match args.action {
            ConfigAction::Set(args) => commands::config_set(&config_path, args),
            ConfigAction::View => commands::config_view(&config_path),
        };
    }

    let file_config = match &config_path {
        Some(path) => config::load(path)?,
        None => None,
    };
    let server = config::resolve_server(cli.server, file_config.as_ref());
    let tls = config::resolve_tls(&cli.tls, file_config.as_ref())?;

    match cli.command {
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
        Commands::Config(_) => unreachable!("handled above"),
    }
}
