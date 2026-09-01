use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use proto::shared::v1::{EnvVar, Port, Protocol};

#[derive(Parser)]
#[command(name = "barectl", version, about = "Command-line client for Barenetes")]
pub struct Cli {
    /// Address of the API server
    #[arg(env = "BARENETES_SERVER", default_value = "http://127.0.0.1:50052")]
    pub server: String,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Create a pod with a single container
    #[command(name = "createPod")]
    CreatePod(CreatePodArgs),

    /// List pods validating filters
    #[command(name = "getPod")]
    GetPod(GetPodArgs),

    /// Delete a pod by name and optional namespace
    #[command(name = "deletePod")]
    DeletePod(DeletePodArgs),

    /// Fetch all nodes / one node by name
    #[command(name = "getNode")]
    GetNode(GetNodeArgs),
}

#[derive(Args)]
pub struct CreatePodArgs {
    /// Create the pod from a YAML manifest file, instead of the flags below
    #[arg(short, long)]
    pub file: Option<PathBuf>,

    /// Pod name (also used as the container name); required unless --file is used
    pub name: Option<String>,

    /// Namespace to create the pod in (default "default"); ignored with --file
    #[arg(short, long)]
    pub namespace: Option<String>,

    /// Container image (OCI reference); required unless --file is used
    #[arg(short, long)]
    pub image: Option<String>,

    /// Exposed port, format HOST:CONTAINER[/tcp|udp] (repeatable)
    #[arg(short, long = "port", value_parser = parse_port)]
    pub ports: Vec<Port>,

    /// Environment variable, format KEY=VALUE (repeatable)
    #[arg(short, long = "env", value_parser = parse_env)]
    pub env: Vec<EnvVar>,

    /// CPU request in milli-cpu (e.g. 250 = 0.25 core)
    #[arg(long = "cpu-request")]
    pub cpu_request: Option<i32>,

    /// Memory request in MB
    #[arg(long = "memory-request")]
    pub memory_request: Option<i32>,

    /// CPU limit in milli-cpu
    #[arg(long = "cpu-limit")]
    pub cpu_limit: Option<i32>,

    /// Memory limit in MB
    #[arg(long = "memory-limit")]
    pub memory_limit: Option<i32>,
}

#[derive(Args)]
pub struct GetPodArgs {
    /// Filter over pod name ; if only 1 is returned, display details
    pub name: Option<String>,

    /// Filter over namespace ; combine with pod name to always have 0 or 1 result
    #[arg(long, short)]
    pub namespace: Option<String>,

    /// Filter over container image
    #[arg(long, short)]
    pub image: Option<String>,
}

#[derive(Args)]
pub struct GetNodeArgs {
    /// Name of a specific node for details
    pub name: Option<String>,
}

#[derive(Args)]
pub struct DeletePodArgs {
    /// Pod name
    pub name: String,

    /// Pod namespace, optional
    #[arg(long, short, default_value = "default")]
    pub namespace: String,
}

fn parse_port(raw: &str) -> Result<Port, String> {
    let (ports, protocol) = raw.split_once('/').unwrap_or((raw, "tcp"));
    let (host, container) = ports
        .split_once(':')
        .ok_or_else(|| format!("invalid port \"{raw}\", expected HOST:CONTAINER[/tcp|udp]"))?;
    let external: u32 = host
        .parse()
        .map_err(|_| format!("invalid host port \"{host}\""))?;
    let internal: u32 = container
        .parse()
        .map_err(|_| format!("invalid container port \"{container}\""))?;
    let protocol = match protocol.to_ascii_uppercase().as_str() {
        "TCP" => Protocol::Tcp,
        "UDP" => Protocol::Udp,
        other => return Err(format!("invalid protocol \"{other}\", expected tcp or udp")),
    };

    Ok(Port {
        internal,
        external,
        protocol: protocol as i32,
    })
}

fn parse_env(raw: &str) -> Result<EnvVar, String> {
    let (name, value) = raw
        .split_once('=')
        .ok_or_else(|| format!("invalid env var \"{raw}\", expected KEY=VALUE"))?;
    Ok(EnvVar {
        name: name.to_string(),
        value: value.to_string(),
    })
}
