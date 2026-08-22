mod handler;
mod socket;

use crate::{network, state};
use handler::CniRpcService;
use proto::cni::v1::cni_service_server::CniServiceServer;
use std::io;
use std::path::Path;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let pools = ip_pool_directory()?;
    network::ensure_bridge()?;
    network::ensure_overlay()?;
    network::ensure_egress()?;
    let listener = socket::bind(Path::new("/run/barenetes/cni.sock"))?;

    let result = Server::builder()
        .add_service(CniServiceServer::new(CniRpcService::new(
            pools,
            state::StateStore::new(Path::new("/var/lib/barenetes/cni/workloads")),
        )))
        .serve_with_incoming_shutdown(UnixListenerStream::new(listener), shutdown_signal())
        .await;

    socket::remove(Path::new("/run/barenetes/cni.sock"))?;
    result?;
    Ok(())
}

fn ip_pool_directory() -> io::Result<crate::ip_pool::IpPoolDirectory> {
    let node_id = network::node_id()?;
    Ok(crate::ip_pool::IpPoolDirectory::new(
        "/var/lib/barenetes/cni",
        node_id,
    ))
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("cni: failed to install shutdown signal handler: {error}");
    }
}
