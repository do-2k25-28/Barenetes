mod errors;
mod handlers;
mod service;
mod store;
mod telemetry;
#[cfg(test)]
mod test_support;
mod validation;

use std::sync::Arc;
use std::time::Duration;

use proto::api::v1::api_server_server::ApiServerServer;
use tonic::transport::Server;

use service::ApiService;
use store::Store;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Loopback by default (safe for a single-host/dev setup); a real multi-node
    // deployment needs this reachable from worker nodes, e.g. 0.0.0.0:50052.
    let addr = std::env::var("BARENETES_LISTEN_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:50052".to_string())
        .parse()?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let store = if let Ok(endpoints) = std::env::var("BARENETES_ETCD_ENDPOINTS") {
        let endpoints: Vec<String> = endpoints
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        let client = etcd_client::Client::connect(&endpoints, None).await?;
        tracing::info!(?endpoints, "connected to etcd");
        let store = Arc::new(Store::new_with_etcd(client));
        store.reset_node_liveness().await?;
        store
    } else {
        tracing::info!("running with in-memory store");
        Arc::new(Store::new())
    };

    let api_service = ApiService { store };

    tracing::info!(%addr, "API server starting");

    Server::builder()
        .http2_keepalive_interval(Some(Duration::from_secs(10)))
        .http2_keepalive_timeout(Some(Duration::from_secs(20)))
        .add_service(ApiServerServer::new(api_service))
        .serve_with_shutdown(addr, shutdown_signal())
        .await?;

    tracing::info!("API server shutting down");

    Ok(())
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "failed to install shutdown signal handler");
        std::future::pending::<()>().await;
    }
}
