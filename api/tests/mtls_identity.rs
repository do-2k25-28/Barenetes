//! Drives a real TLS-terminated `ApiServerServer` over the network, so the
//! mTLS peer-identity check added in api/src/tls_identity.rs is proven at
//! the transport level, not just against a bare `Request` in a unit test.
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use api::service::ApiService;
use api::store::Store;
use proto::api::v1::UpdateNodeStatusRequest;
use proto::api::v1::api_server_client::ApiServerClient;
use proto::api::v1::api_server_server::ApiServerServer;
use proto::shared::v1::{Node, NodeStatus};
use proto::tls::{load_client_tls_config, load_server_tls_config};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use tonic::Code;
use tonic::transport::server::TcpIncoming;
use tonic::transport::{Channel, Server};

struct GeneratedCert {
    pem: String,
    key_pem: String,
}

/// Tests in this binary run concurrently, so each call gets its own
/// directory (pid + a monotonic counter isn't enough on its own: two calls
/// within the same test would otherwise collide on `label`/`name`).
fn temp_file(label: &str, name: &str, contents: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);

    let dir = std::env::temp_dir().join(format!(
        "barenetes-api-mtls-test-{label}-{}-{unique}",
        std::process::id(),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

/// A self-signed CA, mirroring what `barenetes-pki init-ca` produces.
fn make_ca() -> (String, String) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "test cluster CA");
    params.distinguished_name = dn;
    let cert = params.self_signed(&key).unwrap();
    (cert.pem(), key.serialize_pem())
}

/// A leaf cert signed by `ca_pem`/`ca_key_pem`, mirroring `barenetes-pki
/// issue`. Re-parses the CA key from PEM for every call (rather than taking
/// it by value) so the same CA can issue multiple leaves, same as the real
/// tool does by re-reading ca-key.pem each invocation.
fn issue(ca_pem: &str, ca_key_pem: &str, cn: &str) -> GeneratedCert {
    let ca_key = KeyPair::from_pem(ca_key_pem).unwrap();
    let issuer = Issuer::from_ca_cert_pem(ca_pem, ca_key).unwrap();

    let mut params = CertificateParams::new(vec![cn.to_string()]).unwrap();
    params.is_ca = IsCa::ExplicitNoCa;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, cn);
    params.distinguished_name = dn;

    let leaf_key = KeyPair::generate().unwrap();
    let cert = params.signed_by(&leaf_key, &issuer).unwrap();
    GeneratedCert {
        pem: cert.pem(),
        key_pem: leaf_key.serialize_pem(),
    }
}

/// Starts a real mTLS `ApiServerServer` on an OS-assigned loopback port and
/// returns its address plus the background task driving it.
async fn start_mtls_server(
    store: Arc<Store>,
    server_cert: &GeneratedCert,
    ca_pem: &str,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let cert_path = temp_file("server", "cert.pem", &server_cert.pem);
    let key_path = temp_file("server", "key.pem", &server_cert.key_pem);
    let ca_path = temp_file("server", "ca.pem", ca_pem);

    let tls = load_server_tls_config(&cert_path, &key_path, &ca_path).unwrap();
    let incoming = TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = incoming.local_addr().unwrap();

    let server = Server::builder()
        .tls_config(tls)
        .unwrap()
        .add_service(ApiServerServer::new(ApiService { store }));

    let handle = tokio::spawn(async move {
        server.serve_with_incoming(incoming).await.unwrap();
    });

    (addr, handle)
}

async fn mtls_client(
    addr: SocketAddr,
    client_cert: &GeneratedCert,
    ca_pem: &str,
    server_name: &str,
) -> ApiServerClient<Channel> {
    let cert_path = temp_file("client", "cert.pem", &client_cert.pem);
    let key_path = temp_file("client", "key.pem", &client_cert.key_pem);
    let ca_path = temp_file("client", "ca.pem", ca_pem);

    let tls = load_client_tls_config(&cert_path, &key_path, &ca_path, server_name).unwrap();
    let channel = Channel::from_shared(format!("https://{addr}"))
        .unwrap()
        .tls_config(tls)
        .unwrap()
        .connect()
        .await
        .unwrap();
    ApiServerClient::new(channel)
}

fn update_node_status_request(node_name: &str) -> UpdateNodeStatusRequest {
    UpdateNodeStatusRequest {
        node: Some(Node {
            name: node_name.to_string(),
            status: NodeStatus::Ready as i32,
            capacity: None,
            allocatable: None,
        }),
    }
}

#[tokio::test]
async fn matching_client_cert_and_node_name_is_accepted() {
    let (ca_pem, ca_key_pem) = make_ca();
    let server_cert = issue(&ca_pem, &ca_key_pem, "test-server");
    let client_cert = issue(&ca_pem, &ca_key_pem, "node-a");

    let (addr, _server) = start_mtls_server(Arc::new(Store::new()), &server_cert, &ca_pem).await;
    let mut client = mtls_client(addr, &client_cert, &ca_pem, "test-server").await;

    let response = client
        .update_node_status(update_node_status_request("node-a"))
        .await;

    assert!(response.is_ok(), "{:?}", response.err());
}

#[tokio::test]
async fn mismatched_node_name_is_rejected_as_impersonation() {
    let (ca_pem, ca_key_pem) = make_ca();
    let server_cert = issue(&ca_pem, &ca_key_pem, "test-server");
    // The client authenticates as node-a but claims to be node-b.
    let client_cert = issue(&ca_pem, &ca_key_pem, "node-a");

    let (addr, _server) = start_mtls_server(Arc::new(Store::new()), &server_cert, &ca_pem).await;
    let mut client = mtls_client(addr, &client_cert, &ca_pem, "test-server").await;

    let status = client
        .update_node_status(update_node_status_request("node-b"))
        .await
        .expect_err("a node_name that doesn't match the client cert must be rejected");

    assert_eq!(status.code(), Code::PermissionDenied);
}

#[tokio::test]
async fn plaintext_server_is_unaffected_by_the_identity_check() {
    let incoming = TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = incoming.local_addr().unwrap();
    let server = Server::builder().add_service(ApiServerServer::new(ApiService {
        store: Arc::new(Store::new()),
    }));
    tokio::spawn(async move {
        server.serve_with_incoming(incoming).await.unwrap();
    });

    let mut client = ApiServerClient::connect(format!("http://{addr}"))
        .await
        .unwrap();

    // No client cert, no relationship to node_name at all: must still work
    // exactly as it did before mTLS was added.
    let response = client
        .update_node_status(update_node_status_request("node-b"))
        .await;

    assert!(response.is_ok(), "{:?}", response.err());
}
