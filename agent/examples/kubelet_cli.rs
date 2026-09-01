//! Minimal client to drive the kubelet service by hand.
//!
//!     cargo run -p agent --example kubelet_cli -- apply <pod> <image> [image...]
//!     cargo run -p agent --example kubelet_cli -- delete <pod-id> [--force]

use proto::agent::v1::kubelet_client::KubeletClient;
use proto::agent::v1::{ApplyPodRequest, DeletePodRequest};
use proto::shared::v1::{Container, Pod, PodSpec, PodWithSpec};

const KUBELET: &str = "http://127.0.0.1:50053";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut client = KubeletClient::connect(KUBELET).await?;

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
