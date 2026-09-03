//! mTLS wiring shared by every gRPC binary (`api`, `agent`, `scheduler`, `barectl`):
//! CLI flags, the plaintext/mTLS mode switch, and the `tonic` TLS config
//! builders. Kept here rather than duplicated per-crate since `proto` is
//! already the common dependency of all of them.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;
use tonic::transport::{Certificate, ClientTlsConfig, Identity, ServerTlsConfig};

/// TLS-related CLI flags, flattened into each binary's `Cli`. All three
/// cert/key/CA paths must be given together to enable mTLS; leaving all
/// three unset keeps the binary in plaintext mode.
#[derive(Args, Clone, Debug, Default)]
pub struct TlsArgs {
    /// Path to this binary's TLS certificate (PEM). Combine with --tls-key
    /// and --tls-ca to enable mTLS.
    #[arg(long = "tls-cert", env = "BARENETES_TLS_CERT")]
    pub tls_cert: Option<PathBuf>,

    /// Path to this binary's TLS private key (PEM).
    #[arg(long = "tls-key", env = "BARENETES_TLS_KEY")]
    pub tls_key: Option<PathBuf>,

    /// Path to the cluster CA certificate (PEM), used to verify the peer.
    #[arg(long = "tls-ca", env = "BARENETES_TLS_CA")]
    pub tls_ca: Option<PathBuf>,

    /// Expected server name/CN on the peer certificate. Required on the
    /// client side when connecting over mTLS, since the certs issued by
    /// `barenetes-pki` carry no public DNS name for `tonic` to default to.
    #[arg(long = "tls-server-name", env = "BARENETES_TLS_SERVER_NAME")]
    pub tls_server_name: Option<String>,
}

/// Which of the two supported modes a binary should run in, derived from
/// [`TlsArgs`] by [`tls_mode`].
pub enum TlsMode {
    /// No TLS: the original, unauthenticated behavior. Kept as the default
    /// so a plain `cargo run` still works without a PKI set up.
    Plaintext,
    /// mTLS with the given cert/key/CA paths.
    Mtls {
        cert: PathBuf,
        key: PathBuf,
        ca: PathBuf,
    },
}

/// Decides plaintext vs. mTLS from [`TlsArgs`]: none of the three paths set
/// means plaintext, all three set means mTLS, and any partial combination
/// is rejected rather than silently guessed at.
pub fn tls_mode(args: &TlsArgs) -> Result<TlsMode> {
    match (&args.tls_cert, &args.tls_key, &args.tls_ca) {
        (None, None, None) => Ok(TlsMode::Plaintext),
        (Some(cert), Some(key), Some(ca)) => Ok(TlsMode::Mtls {
            cert: cert.clone(),
            key: key.clone(),
            ca: ca.clone(),
        }),
        _ => bail!(
            "--tls-cert, --tls-key and --tls-ca must all be set together (mTLS) or all omitted (plaintext)"
        ),
    }
}

fn read_pem(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("reading {}", path.display()))
}

/// Builds the server-side TLS config for an mTLS listener: presents
/// `cert`/`key` as its own identity, and requires (not merely accepts) a
/// client certificate signed by `ca`. `client_auth_optional(false)` is set
/// explicitly so mTLS always means "client cert mandatory", never a silent
/// server-only TLS fallback.
pub fn load_server_tls_config(cert: &Path, key: &Path, ca: &Path) -> Result<ServerTlsConfig> {
    let identity = Identity::from_pem(read_pem(cert)?, read_pem(key)?);
    let client_ca_root = Certificate::from_pem(read_pem(ca)?);

    Ok(ServerTlsConfig::new()
        .identity(identity)
        .client_ca_root(client_ca_root)
        .client_auth_optional(false))
}

/// Builds the client-side TLS config for an mTLS connection: presents
/// `cert`/`key` as the client's identity, trusts the server only if its
/// certificate chains to `ca`, and verifies it against `server_name` (the
/// certs `barenetes-pki` issues have no public DNS name, so this can't be
/// inferred from the connection URI the way it would be for a public host).
pub fn load_client_tls_config(
    cert: &Path,
    key: &Path,
    ca: &Path,
    server_name: &str,
) -> Result<ClientTlsConfig> {
    let identity = Identity::from_pem(read_pem(cert)?, read_pem(key)?);
    let ca_certificate = Certificate::from_pem(read_pem(ca)?);

    Ok(ClientTlsConfig::new()
        .identity(identity)
        .ca_certificate(ca_certificate)
        .domain_name(server_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(cert: Option<&str>, key: Option<&str>, ca: Option<&str>) -> TlsArgs {
        TlsArgs {
            tls_cert: cert.map(PathBuf::from),
            tls_key: key.map(PathBuf::from),
            tls_ca: ca.map(PathBuf::from),
            tls_server_name: None,
        }
    }

    #[test]
    fn tls_mode_all_unset_is_plaintext() {
        assert!(matches!(
            tls_mode(&args(None, None, None)).unwrap(),
            TlsMode::Plaintext
        ));
    }

    #[test]
    fn tls_mode_all_set_is_mtls() {
        assert!(matches!(
            tls_mode(&args(Some("c"), Some("k"), Some("a"))).unwrap(),
            TlsMode::Mtls { .. }
        ));
    }

    #[test]
    fn tls_mode_partial_is_rejected() {
        assert!(tls_mode(&args(Some("c"), None, None)).is_err());
        assert!(tls_mode(&args(None, Some("k"), Some("a"))).is_err());
        assert!(tls_mode(&args(Some("c"), Some("k"), None)).is_err());
    }

    #[test]
    fn load_configs_accept_rcgen_generated_pems() {
        use rcgen::{CertificateParams, KeyPair};

        let ca_key = KeyPair::generate().unwrap();
        let ca_cert = CertificateParams::new(vec![])
            .unwrap()
            .self_signed(&ca_key)
            .unwrap();

        let leaf_key = KeyPair::generate().unwrap();
        let leaf_cert = CertificateParams::new(vec!["node-a".to_string()])
            .unwrap()
            .self_signed(&leaf_key)
            .unwrap();

        let dir = tempdir();
        let cert_path = dir.join("leaf.pem");
        let key_path = dir.join("leaf-key.pem");
        let ca_path = dir.join("ca.pem");
        std::fs::write(&cert_path, leaf_cert.pem()).unwrap();
        std::fs::write(&key_path, leaf_key.serialize_pem()).unwrap();
        std::fs::write(&ca_path, ca_cert.pem()).unwrap();

        load_server_tls_config(&cert_path, &key_path, &ca_path)
            .expect("server tls config should build from valid PEMs");
        load_client_tls_config(&cert_path, &key_path, &ca_path, "node-a")
            .expect("client tls config should build from valid PEMs");

        std::fs::remove_dir_all(&dir).ok();
    }

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("barenetes-tls-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
