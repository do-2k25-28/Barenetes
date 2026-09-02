//! Minimal client to drive the kubelet service by hand.
//!
//!     cargo run -p agent --example kubelet_cli -- apply <pod> <image> [image...]
//!     cargo run -p agent --example kubelet_cli -- delete <pod-id> [--force]
//!
//! Point it at a non-default kubelet with --addr or BARENETES_AGENT_ADDR.

use clap::Parser;
use proto::agent::v1::kubelet_client::KubeletClient;
use proto::agent::v1::{ApplyPodRequest, DeletePodRequest};
use proto::shared::v1::{Container, Pod, PodSpec, PodWithSpec};

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let args = cli.args;
    let mut client = KubeletClient::connect(cli.addr).await?;

    match args.first().map(String::as_str) {
        Some("apply") if args.len() >= 3 => {
            let name = args[1].clone();
            // One container per image, named <pod>-0, <pod>-1, ...
            let containers = args[2..]
                .iter()
                .enumerate()
                .map(|(i, image)| Container {
                    name: i.to_string(),
                    image: image.clone(),
                    ..Default::default()
                })
                .collect();

            let response = client
                .apply_pod(ApplyPodRequest {
                    pod: Some(PodWithSpec {
                        pod: Some(Pod {
                            name,
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
            eprintln!("usage: kubelet_cli apply <pod> <image> [image...]");
            eprintln!("       kubelet_cli delete <pod-id> [--force]");
            std::process::exit(2);
        }
    }

    Ok(())
}
