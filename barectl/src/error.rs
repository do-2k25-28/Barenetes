use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliError {
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

    #[error("unsupported manifest kind \"{0}\", expected \"Pod\"")]
    UnsupportedKind(String),

    #[error(
        "unsupported protocol \"{protocol}\" on container \"{container}\", expected \"TCP\" or \"UDP\""
    )]
    UnsupportedProtocol { container: String, protocol: String },

    #[error("could not reach the API server at {addr}: {source}")]
    Connect {
        addr: String,
        source: tonic::transport::Error,
    },

    #[error("{0}")]
    Server(#[from] tonic::Status),
}
