use proto::agent::v1::kubelet_server::KubeletServer;
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
    let addr = "127.0.0.1:50052".parse()?;

    let containerd = containerd::Containerd::connect(CONTAINERD_SOCKET).await?;
    let cni = cni::Cni::new(CNI_SOCKET);
    let vlans = vlan::VlanAllocations::new(AGENT_STATE_DIR)?;
    let kubelet = kubelet::KubeletService::new(containerd, cni, vlans);

    println!("Kubelet service starting on {}", addr);

    Server::builder()
        .add_service(KubeletServer::new(kubelet))
        .serve(addr)
        .await?;

    Ok(())
}
