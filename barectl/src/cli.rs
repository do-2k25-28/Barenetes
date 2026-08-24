use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

const DEFAULT_SERVER_ADDR: &str = "http://127.0.0.1:50052";

#[derive(Parser)]
#[command(name = "barectl", version, about = "Command-line client for Barenetes")]
pub struct Cli {
    /// Address of the API server (e.g. http://127.0.0.1:50052)
    #[arg(long, global = true, env = "BARECTL_SERVER", default_value = DEFAULT_SERVER_ADDR)]
    pub server: String,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Create a pod from a manifest file
    #[command(name = "createPod")]
    CreatePod(CreatePodArgs),
}

#[derive(Args)]
pub struct CreatePodArgs {
    /// Path to a YAML manifest describing the pod to create
    #[arg(short, long, value_name = "FILE")]
    pub file: PathBuf,
}
