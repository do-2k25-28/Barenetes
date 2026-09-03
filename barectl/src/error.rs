use std::path::PathBuf;

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

    #[error("could not read manifest {path}: {source}")]
    ReadManifest {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("could not parse manifest {path}: {source}")]
    ParseManifest {
        path: PathBuf,
        source: serde_yaml::Error,
    },

    #[error("could not write command output: {0}")]
    WriteOutput(#[source] std::io::Error),

    #[error("{0}")]
    InvalidUsage(String),

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
