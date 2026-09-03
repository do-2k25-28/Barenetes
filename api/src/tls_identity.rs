//! Ties the mTLS peer certificate on an agent-facing RPC to the `node_name`
//! the request claims: the concrete fix for node impersonation (any client
//! holding a valid cluster cert could otherwise register or watch as any
//! node, since `node_name` itself was never checked against the caller).
use tonic::transport::server::{TcpConnectInfo, TlsConnectInfo};
use tonic::{Request, Status};
use x509_parser::extensions::GeneralName;

/// Extracts the CN, or failing that the first DNS SAN, of the peer's leaf
/// certificate. Returns `None` when the connection isn't mTLS (no
/// `TlsConnectInfo` extension present) so callers can treat that as a
/// no-op: without a PKI configured, the identity check doesn't apply and
/// plaintext dev/local usage keeps working unchanged.
pub(crate) fn peer_identity<T>(request: &Request<T>) -> Option<String> {
    let tls_info = request
        .extensions()
        .get::<TlsConnectInfo<TcpConnectInfo>>()?;
    let leaf = tls_info.peer_certs()?.first()?.to_owned();
    let (_, cert) = x509_parser::parse_x509_certificate(leaf.as_ref()).ok()?;

    let cn = cert
        .subject()
        .iter_common_name()
        .next()
        .and_then(|attr| attr.as_str().ok());
    if let Some(cn) = cn {
        return Some(cn.to_string());
    }

    let sans = cert.subject_alternative_name().ok().flatten()?;
    sans.value.general_names.iter().find_map(|name| match name {
        GeneralName::DNSName(dns) => Some((*dns).to_string()),
        _ => None,
    })
}

/// Rejects a request whose mTLS peer identity doesn't match the `node_name`
/// it claims. A `None` peer (plaintext mode, see [`peer_identity`]) is a
/// no-op: there is no identity to check without a PKI.
pub(crate) fn check_identity(peer: Option<&str>, node_name: &str) -> Result<(), Status> {
    match peer {
        None => Ok(()),
        Some(peer) if peer == node_name => Ok(()),
        Some(peer) => Err(crate::errors::identity_mismatch(peer, node_name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_identity_allows_plaintext_no_peer() {
        assert!(check_identity(None, "node-a").is_ok());
    }

    #[test]
    fn check_identity_allows_matching_peer() {
        assert!(check_identity(Some("node-a"), "node-a").is_ok());
    }

    #[test]
    fn check_identity_rejects_mismatched_peer() {
        let err = check_identity(Some("node-a"), "node-b").unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }
}
