use std::path::PathBuf;

use thiserror::Error;
use tonic::Code;

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

    #[error("{0}")]
    InvalidUsage(String),

    #[error("{message}")]
    Server { message: String },
}

impl From<tonic::Status> for CliError {
    fn from(status: tonic::Status) -> Self {
        let hint = match status.code() {
            Code::AlreadyExists => {
                " (try a different --name/--namespace, or delete the existing pod first)"
            }
            Code::NotFound => " (check --name and --namespace, no such pod exists)",
            Code::Unavailable => " (the server may be restarting, try again in a moment)",
            _ => "",
        };
        CliError::Server {
            message: format!("{}{hint}", status.message()),
        }
    }
}
