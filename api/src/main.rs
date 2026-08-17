mod handlers;
mod service;
mod store;

use std::sync::Arc;

use proto::api::v1::api_server_server::ApiServerServer;
use tonic::transport::Server;

use service::ApiService;
use store::Store;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "127.0.0.1:50052".parse()?;

    let store = Arc::new(Store::new());
    let api_service = ApiService { store };

    println!("API server starting on {addr}");

    Server::builder()
        .add_service(ApiServerServer::new(api_service))
        .serve(addr)
        .await?;

    Ok(())
}
