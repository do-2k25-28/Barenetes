use proto::api::v1::api_server_client::ApiServerClient;
use tonic::transport::Channel;

use crate::error::CliError;

/// Connects to the API server's `ApiServer` gRPC service.
pub async fn connect(addr: &str) -> Result<ApiServerClient<Channel>, CliError> {
    ApiServerClient::connect(addr.to_string())
        .await
        .map_err(|source| CliError::Connect {
            addr: addr.to_string(),
            source,
        })
}
