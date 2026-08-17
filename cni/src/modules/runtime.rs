#[path = "runtime/socket.rs"]
mod socket;

use crate::handler::CniRpcService;
use crate::{firewall, network, state};
use proto::cni::v1::cni_service_server::CniServiceServer;
use std::io;
use std::net::Ipv4Addr;
use std::path::Path;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let pool = ip_pool()?;
    network::ensure_bridge()?;
    network::ensure_overlay()?;
    firewall::ensure_egress()?;
    let listener = socket::bind(Path::new("/run/barenetes/cni.sock"))?;

    let result = Server::builder()
        .add_service(CniServiceServer::new(CniRpcService::new(
            pool,
            state::StateStore::new(Path::new("/var/lib/barenetes/cni/workloads")),
        )))
        .serve_with_incoming_shutdown(UnixListenerStream::new(listener), shutdown_signal())
        .await;

    socket::remove(Path::new("/run/barenetes/cni.sock"))?;
    result?;
    Ok(())
}

fn ip_pool() -> io::Result<crate::ip_pool::IpPool> {
    let node_id = network::node_id()?;
    crate::ip_pool::IpPool::new(
        "/var/lib/barenetes/cni",
        Ipv4Addr::new(10, 244, node_id, 2),
        Ipv4Addr::new(10, 244, node_id, 254),
    )
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("cni: failed to install shutdown signal handler: {error}");
    }
}
