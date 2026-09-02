use proto::agent::v1::kubelet_server::KubeletServer;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

mod cni;
mod containerd;
mod kubelet;
mod oci;
mod vlan;

const CONTAINERD_SOCKET: &str = "/run/containerd/containerd.sock";
const CNI_SOCKET: &str = "/run/barenetes/cni.sock";
const AGENT_STATE_DIR: &str = "/var/lib/barenetes/agent";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let containerd = containerd::Containerd::connect(CONTAINERD_SOCKET).await?;
    let cni = cni::Cni::new(CNI_SOCKET);
    let vlans = vlan::VlanAllocations::new(AGENT_STATE_DIR)?;
    let kubelet = kubelet::KubeletService::new(containerd, cni, vlans);

    let path = "/run/barenetes/agent.sock";

    // Create the directory if it doesn't exist
    std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap())?;

    // Bind the Unix socket
    let uds = UnixListener::bind(path)?;
    let uds_stream = UnixListenerStream::new(uds);

    // Serve the gRPC service
    Server::builder()
        .add_service(KubeletServer::new(kubelet))
        .serve_with_incoming(uds_stream)
        .await?;

    println!("Kubelet service listening on {}", path);

    Ok(())
}
