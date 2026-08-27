use clap::{Args, Parser, Subcommand};
use proto::shared::v1::{EnvVar, Port, Protocol};

const DEFAULT_SERVER_ADDR: &str = "http://127.0.0.1:50052";

#[derive(Parser)]
#[command(name = "barectl", version, about = "Command-line client for Barenetes")]
pub struct Cli {
    /// Address of the API server (e.g. http://127.0.0.1:50052)
    #[arg(long, global = true, env = "BARENETES_SERVER", default_value = DEFAULT_SERVER_ADDR)]
    pub server: String,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Create a pod with a single container
    #[command(name = "createPod")]
    CreatePod(CreatePodArgs),

    /// Fetch a pod by name
    #[command(name = "getPod")]
    GetPod(GetPodArgs),
}

#[derive(Args)]
pub struct CreatePodArgs {
    /// Pod name (also used as the container name)
    #[arg(long, value_parser = non_empty)]
    pub name: String,

    /// Namespace to create the pod in
    #[arg(long, default_value = "default", value_parser = non_empty)]
    pub namespace: String,

    /// Container image (OCI reference)
    #[arg(long, value_parser = non_empty)]
    pub image: String,

    /// Exposed port, format HOST:CONTAINER[/tcp|udp] (repeatable)
    #[arg(long = "port", value_parser = parse_port)]
    pub ports: Vec<Port>,

    /// Environment variable, format KEY=VALUE (repeatable)
    #[arg(long = "env", value_parser = parse_env)]
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
    /// Pod name
    #[arg(long, value_parser = non_empty)]
    pub name: String,

    /// Namespace the pod is in
    #[arg(long, default_value = "default", value_parser = non_empty)]
    pub namespace: String,
}

/// Rejects an empty (or whitespace-only) value, so a mistake like `--name ""`
/// fails immediately instead of round-tripping to the server first.
fn non_empty(raw: &str) -> Result<String, String> {
    if raw.trim().is_empty() {
        return Err("value must not be empty".to_string());
    }
    Ok(raw.to_string())
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
