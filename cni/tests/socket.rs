use std::io;
use std::os::unix::fs::PermissionsExt;

#[test]
fn does_not_remove_a_regular_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cni.sock");
    std::fs::write(&path, b"keep").unwrap();

    assert_eq!(
        cni::remove_socket(&path).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(std::fs::read(path).unwrap(), b"keep");
}

#[tokio::test]
async fn creates_a_restricted_unix_socket() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("run").join("cni.sock");

    let listener = cni::bind_socket(&path).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;

    assert_eq!(mode, 0o660);
    drop(listener);
    cni::remove_socket(&path).unwrap();
}
