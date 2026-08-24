mod errors;
mod handlers;
mod service;
mod store;
mod telemetry;
#[cfg(test)]
mod test_support;
mod validation;

use std::sync::Arc;

use proto::api::v1::api_server_server::ApiServerServer;
use tonic::transport::Server;

use service::ApiService;
use store::Store;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "127.0.0.1:50052".parse()?;

    let store = Arc::new(Store::new());

    let liveness_store = store.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(store::HEARTBEAT_INTERVAL);
        loop {
            interval.tick().await;
            liveness_store
                .sweep_stale_nodes(store::NODE_STALE_TIMEOUT)
                .await;
        }
    });

    let api_service = ApiService { store };

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!(%addr, "API server starting");

    Server::builder()
        .add_service(ApiServerServer::new(api_service))
        .serve_with_shutdown(addr, shutdown_signal())
        .await?;

    tracing::info!("API server shutting down");

    Ok(())
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "failed to install shutdown signal handler");
    }
}
