mod handler;
mod socket;

use crate::{network, state};
use handler::CniRpcService;
use proto::cni::v1::cni_service_server::CniServiceServer;
use std::io;
use std::path::PathBuf;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let pools = ip_pool_directory()?;
    network::ensure_bridge()?;
    network::ensure_overlay()?;
    network::ensure_egress()?;
    let socket_path = configured_path("BARENETES_CNI_SOCKET", "/run/barenetes/cni.sock");
    let state_path = configured_path(
        "BARENETES_CNI_STATE_DIR",
        "/var/lib/barenetes/cni/workloads",
    );
    let listener = socket::bind(&socket_path)?;

    let result = Server::builder()
        .add_service(CniServiceServer::new(CniRpcService::new(
            pools,
            state::StateStore::new(state_path),
        )))
        .serve_with_incoming_shutdown(UnixListenerStream::new(listener), shutdown_signal())
        .await;

    socket::remove(&socket_path)?;
    result?;
    Ok(())
}

fn ip_pool_directory() -> io::Result<crate::ip_pool::IpPoolDirectory> {
    let node_id = network::node_id()?;
    Ok(crate::ip_pool::IpPoolDirectory::new(
        configured_path("BARENETES_CNI_IP_POOL_DIR", "/var/lib/barenetes/cni"),
        node_id,
    ))
}

fn configured_path(name: &str, default: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("cni: failed to install shutdown signal handler: {error}");
    }
}
