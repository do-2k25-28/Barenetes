//! Ties the mTLS peer certificate on an RPC to the identity and role it
//! claims: the concrete fix for node impersonation (any client holding a
//! valid cluster cert could otherwise register or watch as any node, since
//! `node_name` itself was never checked against the caller) and for
//! privilege escalation (every cluster certificate authenticates against
//! the same CA, so without a separate per-RPC role check a worker's
//! certificate -- distributed to every node -- could call CreatePod,
//! DeletePod or AssignPod just as well as a real operator or the
//! scheduler).
use tonic::transport::server::{TcpConnectInfo, TlsConnectInfo};
use tonic::{Request, Status};
use x509_parser::certificate::X509Certificate;
use x509_parser::extensions::GeneralName;

/// The cluster role a peer certificate is authorized for, read from the
/// subject's Organizational Unit set by `barenetes-pki issue --role`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// The API server's own identity. Never itself a caller of any RPC.
    Api,
    Scheduler,
    Cli,
    Node,
}

impl Role {
    fn parse(raw: &str) -> Option<Role> {
        match raw {
            "api" => Some(Role::Api),
            "scheduler" => Some(Role::Scheduler),
            "cli" => Some(Role::Cli),
            "node" => Some(Role::Node),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Role::Api => "api",
            Role::Scheduler => "scheduler",
            Role::Cli => "cli",
            Role::Node => "node",
        }
    }
}

/// What a request's transport tells us about its caller. Kept as two
/// explicit cases rather than a single `Option`, because they must be
/// treated differently: `Plaintext` means no PKI is configured at all, so
/// every check below is a no-op (plain `cargo run` / local dev keeps
/// working); `Mtls` means a client certificate was presented, but its
/// `identity`/`role` are themselves `Option`s since the cert may lack a
/// usable CN/SAN or a recognized OU. A cert that authenticated but carries
/// no usable identity/role must fail closed, not be treated as
/// indistinguishable from a trusted plaintext caller -- that conflation is
/// exactly what let a CA-signed certificate with no CN/SAN bypass the
/// node-name check.
pub(crate) enum Peer {
    Plaintext,
    Mtls {
        identity: Option<String>,
        role: Option<Role>,
    },
}

/// Inspects `request`'s transport for an mTLS peer certificate and, if one
/// is present, extracts its identity (CN, or failing that the first DNS
/// SAN) and role (OU).
pub(crate) fn peer<T>(request: &Request<T>) -> Peer {
    let Some(tls_info) = request.extensions().get::<TlsConnectInfo<TcpConnectInfo>>() else {
        return Peer::Plaintext;
    };
    let Some(leaf) = tls_info
        .peer_certs()
        .and_then(|certs| certs.first().cloned())
    else {
        return Peer::Plaintext;
    };
    let Ok((_, cert)) = x509_parser::parse_x509_certificate(leaf.as_ref()) else {
        // Authenticated over TLS but the leaf cert couldn't be parsed: still
        // an mTLS connection, just one with no usable identity or role.
        return Peer::Mtls {
            identity: None,
            role: None,
        };
    };

    Peer::Mtls {
        identity: identity_of(&cert),
        role: role_of(&cert),
    }
}

fn identity_of(cert: &X509Certificate<'_>) -> Option<String> {
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

fn role_of(cert: &X509Certificate<'_>) -> Option<Role> {
    let ou = cert
        .subject()
        .iter_organizational_unit()
        .next()
        .and_then(|attr| attr.as_str().ok())?;
    Role::parse(ou)
}

/// Rejects a request whose mTLS peer identity doesn't match the `node_name`
/// it claims. Plaintext (no PKI configured) is a no-op: there's no identity
/// to check. An mTLS peer whose certificate has no usable CN/SAN is
/// rejected outright, rather than silently allowed through like plaintext.
pub(crate) fn check_identity(peer: &Peer, node_name: &str) -> Result<(), Status> {
    match peer {
        Peer::Plaintext => Ok(()),
        Peer::Mtls {
            identity: Some(identity),
            ..
        } if identity == node_name => Ok(()),
        Peer::Mtls {
            identity: Some(identity),
            ..
        } => Err(crate::errors::identity_mismatch(identity, node_name)),
        Peer::Mtls { identity: None, .. } => Err(crate::errors::missing_identity(node_name)),
    }
}

/// Rejects a request whose mTLS peer isn't authorized for the `expected`
/// role. Plaintext is a no-op, same as [`check_identity`]. A certificate
/// with no recognized OU is rejected rather than treated as any particular
/// role.
pub(crate) fn check_role(peer: &Peer, expected: Role) -> Result<(), Status> {
    match peer {
        Peer::Plaintext => Ok(()),
        Peer::Mtls {
            role: Some(role), ..
        } if *role == expected => Ok(()),
        Peer::Mtls {
            role: Some(role), ..
        } => Err(crate::errors::role_denied(role.as_str(), expected.as_str())),
        Peer::Mtls { role: None, .. } => {
            Err(crate::errors::role_denied("<none>", expected.as_str()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mtls(identity: Option<&str>, role: Option<Role>) -> Peer {
        Peer::Mtls {
            identity: identity.map(str::to_string),
            role,
        }
    }

    #[test]
    fn check_identity_allows_plaintext() {
        assert!(check_identity(&Peer::Plaintext, "node-a").is_ok());
    }

    #[test]
    fn check_identity_allows_matching_peer() {
        assert!(check_identity(&mtls(Some("node-a"), Some(Role::Node)), "node-a").is_ok());
    }

    #[test]
    fn check_identity_rejects_mismatched_peer() {
        let err = check_identity(&mtls(Some("node-a"), Some(Role::Node)), "node-b").unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn check_identity_rejects_mtls_peer_with_no_usable_identity() {
        let err = check_identity(&mtls(None, Some(Role::Node)), "node-a").unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn check_role_allows_plaintext() {
        assert!(check_role(&Peer::Plaintext, Role::Cli).is_ok());
    }

    #[test]
    fn check_role_allows_matching_role() {
        assert!(check_role(&mtls(Some("node-a"), Some(Role::Node)), Role::Node).is_ok());
    }

    #[test]
    fn check_role_rejects_mismatched_role() {
        let err = check_role(&mtls(Some("node-a"), Some(Role::Node)), Role::Cli).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn check_role_rejects_mtls_peer_with_no_recognized_role() {
        let err = check_role(&mtls(Some("node-a"), None), Role::Node).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }
}
