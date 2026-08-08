use proto::agent::v1::kubelet_server::KubeletServer;
use tonic::transport::Server;

mod containerd;
mod kubelet;
mod oci;

const CONTAINERD_SOCKET: &str = "/run/containerd/containerd.sock";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "127.0.0.1:50052".parse()?;

    let containerd = containerd::Containerd::connect(CONTAINERD_SOCKET).await?;
    let kubelet = kubelet::KubeletService::new(containerd);

    println!("Kubelet service starting on {}", addr);

    Server::builder()
        .add_service(KubeletServer::new(kubelet))
        .serve(addr)
        .await?;

    Ok(())
}
