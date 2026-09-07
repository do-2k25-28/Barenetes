use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("could not reach the API server at {addr}: {source}")]
    Connect {
        addr: String,
        source: tonic::transport::Error,
    },

    #[error("TLS configuration error: {0}")]
    Tls(#[from] anyhow::Error),

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

    #[error("{0}")]
    InvalidUsage(String),

    #[error("{message}")]
    Server { message: String },

    #[error("could not read config {path}: {source}")]
    ReadConfig {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("could not parse config {path}: {source}")]
    ParseConfig {
        path: PathBuf,
        source: serde_yaml::Error,
    },

    #[error("could not write config {path}: {source}")]
    WriteConfig {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("could not serialize config {path}: {source}")]
    SerializeConfig {
        path: PathBuf,
        source: serde_yaml::Error,
    },

    #[error("could not decode {field} in config: {source}")]
    DecodeConfig {
        field: &'static str,
        source: base64::DecodeError,
    },

    #[error("could not read {path}: {source}")]
    ReadTlsFile {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl From<tonic::Status> for CliError {
    fn from(status: tonic::Status) -> Self {
        CliError::Server {
            message: status.message().to_string(),
        }
    }
}
