use std::path::PathBuf;

use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use proto::shared::v1::{EnvVar, Port, Protocol};

#[derive(Parser)]
#[command(name = "barectl", version, about = "Command-line client for Barenetes")]
pub struct Cli {
    /// Address of the API server
    #[arg(
        short = 's',
        long,
        env = "BARENETES_SERVER",
        default_value = "http://127.0.0.1:50052",
        global = true
    )]
    pub server: String,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Create a resource
    Create(CreateArgs),

    /// Display one or many resources
    Get(GetArgs),

    /// Delete a resource
    Delete(DeleteArgs),

    /// Generate shell completion code
    Completion(CompletionArgs),
}

#[derive(Args)]
pub struct CreateArgs {
    #[command(subcommand)]
    pub resource: CreateResource,
}

#[derive(Subcommand)]
pub enum CreateResource {
    /// Create a pod
    #[command(visible_aliases = ["pods", "po"])]
    Pod(CreatePodArgs),
}

#[derive(Args)]
pub struct GetArgs {
    #[command(subcommand)]
    pub resource: GetResource,
}

#[derive(Subcommand)]
pub enum GetResource {
    /// Display one or many pods
    #[command(visible_aliases = ["pods", "po"])]
    Pod(GetPodArgs),

    /// Display one or many nodes
    #[command(visible_aliases = ["nodes", "no"])]
    Node(GetNodeArgs),
}

#[derive(Args)]
pub struct DeleteArgs {
    #[command(subcommand)]
    pub resource: DeleteResource,
}

#[derive(Subcommand)]
pub enum DeleteResource {
    /// Delete a pod
    #[command(visible_aliases = ["pods", "po"])]
    Pod(DeletePodArgs),
}

#[derive(Args)]
pub struct CompletionArgs {
    /// Shell for which to generate completion code
    #[arg(value_enum)]
    pub shell: Shell,
}

pub fn generate_completions(
    shell: Shell,
    writer: &mut dyn std::io::Write,
) -> Result<(), std::io::Error> {
    let mut command = Cli::command();
    let mut output = Vec::new();
    generate(shell, &mut command, "barectl", &mut output);
    writer.write_all(&output)
}

#[derive(Args)]
pub struct CreatePodArgs {
    /// Create the pod from a YAML manifest file, instead of the flags below
    #[arg(short, long)]
    pub file: Option<PathBuf>,

    /// Pod name (also used as the container name); required unless --file is used
    pub name: Option<String>,

    /// Namespace to create the pod in; cannot be combined with --file
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
    /// Pod name; if omitted, list pods
    pub name: Option<String>,

    /// Filter by namespace
    #[arg(long, short)]
    pub namespace: Option<String>,

    /// Filter by container image
    #[arg(long, short)]
    pub image: Option<String>,
}

#[derive(Args)]
pub struct GetNodeArgs {
    /// Name of a specific node
    pub name: Option<String>,
}

#[derive(Args)]
pub struct DeletePodArgs {
    /// Pod name
    pub name: String,

    /// Pod namespace
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_resource_names_and_aliases() {
        for resource in ["pod", "pods", "po"] {
            let cli = Cli::try_parse_from(["barectl", "get", resource]).unwrap();
            assert!(matches!(
                cli.command,
                Commands::Get(GetArgs {
                    resource: GetResource::Pod(_)
                })
            ));
        }

        for resource in ["node", "nodes", "no"] {
            let cli = Cli::try_parse_from(["barectl", "get", resource]).unwrap();
            assert!(matches!(
                cli.command,
                Commands::Get(GetArgs {
                    resource: GetResource::Node(_)
                })
            ));
        }
    }

    #[test]
    fn parses_create_and_delete_commands() {
        for resource in ["pod", "pods", "po"] {
            let create =
                Cli::try_parse_from(["barectl", "create", resource, "-f", "pod.yaml"]).unwrap();
            assert!(matches!(
                create.command,
                Commands::Create(CreateArgs {
                    resource: CreateResource::Pod(_)
                })
            ));

            let delete = Cli::try_parse_from(["barectl", "delete", resource, "web"]).unwrap();
            assert!(matches!(
                delete.command,
                Commands::Delete(DeleteArgs {
                    resource: DeleteResource::Pod(_)
                })
            ));
        }
    }

    #[test]
    fn parses_and_generates_shell_completions() {
        let cli = Cli::try_parse_from(["barectl", "completion", "zsh"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Completion(CompletionArgs { shell: Shell::Zsh })
        ));

        for shell in [
            Shell::Bash,
            Shell::Elvish,
            Shell::Fish,
            Shell::PowerShell,
            Shell::Zsh,
        ] {
            let mut output = Vec::new();
            generate_completions(shell, &mut output).unwrap();
            let output = String::from_utf8(output).unwrap();
            assert!(output.contains("barectl"));
            assert!(output.contains("get"));
            assert!(output.contains("pod"));
        }
    }

    #[test]
    fn server_is_a_global_option() {
        let cli = Cli::try_parse_from([
            "barectl",
            "get",
            "nodes",
            "--server",
            "http://api.example:50052",
        ])
        .unwrap();
        assert_eq!(cli.server, "http://api.example:50052");
    }
}
