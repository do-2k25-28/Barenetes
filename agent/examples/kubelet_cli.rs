//! Minimal client to drive the kubelet service by hand.
//!
//!     cargo run -p agent --example kubelet_cli -- apply <pod> <image> [image...]
//!         [--cpu <mCPU>] [--memory <MB>]
//!     cargo run -p agent --example kubelet_cli -- delete <pod-id> [--force]
//!
//! Point it at a non-default kubelet with --addr or BARENETES_AGENT_ADDR.

use clap::Parser;
use proto::agent::v1::kubelet_client::KubeletClient;
use proto::agent::v1::{ApplyPodRequest, DeletePodRequest};
use proto::shared::v1::{Container, Pod, PodSpec, PodWithSpec, Resources};

#[derive(Parser)]
struct Cli {
    /// Address of the kubelet service
    #[arg(
        long,
        env = "BARENETES_AGENT_ADDR",
        default_value = "http://127.0.0.1:50053"
    )]
    addr: String,

    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

fn find_numeric_flag(args: &[String], flag: &str) -> Result<Option<i32>, String> {
    let pos = match args.iter().position(|a| a == flag) {
        Some(p) => p,
        None => return Ok(None),
    };
    let val = args
        .get(pos + 1)
        .ok_or_else(|| format!("{flag} requires a value"))?;
    val.parse::<i32>()
        .map(Some)
        .map_err(|_| format!("{flag}: '{val}' is not a valid integer"))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let args = cli.args;
    let mut client = KubeletClient::connect(cli.addr).await?;

    match args.first().map(String::as_str) {
        Some("apply") if args.len() >= 3 => {
            let name = args[1].clone();

            // Images are everything between the name and the first --flag.
            let first_flag = args[2..]
                .iter()
                .position(|a| a.starts_with("--"))
                .map(|i| i + 2)
                .unwrap_or(args.len());
            let containers = args[2..first_flag]
                .iter()
                .enumerate()
                .map(|(i, image)| Container {
                    name: i.to_string(),
                    image: image.clone(),
                    ..Default::default()
                })
                .collect::<Vec<_>>();
            if containers.is_empty() {
                eprintln!("apply needs at least one image");
                std::process::exit(2);
            }

            let cpu = find_numeric_flag(&args, "--cpu").unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(2);
            });
            let memory = find_numeric_flag(&args, "--memory").unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(2);
            });

            let response = client
                .apply_pod(ApplyPodRequest {
                    pod: Some(PodWithSpec {
                        pod: Some(Pod {
                            name,
                            limits: Some(Resources {
                                cpu: cpu.unwrap_or_default(),
                                memory: memory.unwrap_or_default(),
                            }),
                            ..Default::default()
                        }),
                        spec: Some(PodSpec {
                            namespace: String::new(),
                            containers,
                        }),
                    }),
                })
                .await?;

            println!("applied pod {}", response.into_inner().pod_id);
        }
        Some("delete") if args.len() >= 2 => {
            let response = client
                .delete_pod(DeletePodRequest {
                    pod_id: args[1].clone(),
                    grace_period_seconds: Some(5),
                    force: args.iter().any(|a| a == "--force"),
                })
                .await?;

            println!("deleted: {}", response.into_inner().success);
        }
        _ => {
            eprintln!(
                "usage: kubelet_cli apply <pod> <image> [image...] [--cpu <mCPU>] [--memory <MB>]"
            );
            eprintln!("       kubelet_cli delete <pod-id> [--force]");
            std::process::exit(2);
        }
    }

    Ok(())
}
