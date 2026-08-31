use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("could not reach the API server at {addr}: {source}")]
    Connect {
        addr: String,
        source: tonic::transport::Error,
    },

    #[error("api returned no resource for this request")]
    EmptyResponse,

    #[error("{message}")]
    Server { message: String },
}

impl From<tonic::Status> for CliError {
    fn from(status: tonic::Status) -> Self {
        CliError::Server {
            message: status.message().to_string(),
        }
    }
}
