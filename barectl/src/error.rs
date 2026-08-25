use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("could not reach the API server at {addr}: {source}")]
    Connect {
        addr: String,
        source: tonic::transport::Error,
    },

    #[error("{0}")]
    Server(#[from] tonic::Status),
}
