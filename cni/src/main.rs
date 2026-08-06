mod handler;

use handler::CniRpcService;
use proto::cni::v1::cni_service_server::CniServiceServer;
use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

const SOCKET_PATH: &str = "/run/barenetes/cni.sock";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let listener = bind_socket(Path::new(SOCKET_PATH))?;

    let result = Server::builder()
        .add_service(CniServiceServer::new(CniRpcService))
        .serve_with_incoming_shutdown(UnixListenerStream::new(listener), shutdown_signal())
        .await;

    remove_socket(Path::new(SOCKET_PATH))?;
    result?;
    Ok(())
}

fn bind_socket(path: &Path) -> io::Result<UnixListener> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket has no parent"))?;
    std::fs::create_dir_all(parent)?;
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o750))?;

    remove_socket(path)?;
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    Ok(listener)
}

fn remove_socket(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(path),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "refusing to replace a non-socket filesystem entry",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("cni: failed to install shutdown signal handler: {error}");
    }
}
