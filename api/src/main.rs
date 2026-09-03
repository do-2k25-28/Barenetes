use std::sync::Arc;
use std::time::Duration;

use api::service::ApiService;
use api::store::Store;
use clap::Parser;
use proto::api::v1::api_server_server::ApiServerServer;
use proto::tls::{TlsArgs, TlsMode, load_server_tls_config, tls_mode};
use tonic::transport::Server;

#[derive(Parser)]
#[command(name = "api", version, about = "Barenetes API server")]
struct Cli {
    /// Address to bind the API server on
    #[arg(long, env = "BARENETES_API_ADDR", default_value = "127.0.0.1:50052")]
    addr: String,

    #[command(flatten)]
    tls: TlsArgs,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let addr = cli.addr.parse()?;

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

    let mut server = Server::builder()
        .http2_keepalive_interval(Some(Duration::from_secs(10)))
        .http2_keepalive_timeout(Some(Duration::from_secs(20)));

    match tls_mode(&cli.tls)? {
        TlsMode::Mtls { cert, key, ca } => {
            server = server.tls_config(load_server_tls_config(&cert, &key, &ca)?)?;
            tracing::info!("mTLS enabled: client certificates are required");
        }
        TlsMode::Plaintext => {
            tracing::warn!(
                "starting API server WITHOUT TLS: connections are unauthenticated and unencrypted (set --tls-cert/--tls-key/--tls-ca to enable mTLS)"
            );
        }
    }

    server
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
