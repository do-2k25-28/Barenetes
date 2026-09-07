//! Resolves `barectl`'s effective server address and TLS material from CLI
//! flags/env (already merged by `clap`), the config file
//! (`barectl_config::FileConfig`, default `$HOME/.config/barectl/config`,
//! overridable via `--barectl-config`/`$BARECTL_CONFIG`), and a plaintext
//! default -- in that priority order, matching kubectl's flag vs.
//! current-context precedence. The file format itself lives in the
//! `barectl-config` crate, shared with `barenetes-pki`'s `barectl-config`
//! subcommand so the two can never write/read incompatible files.
use std::path::PathBuf;

pub use barectl_config::{FileConfig, default_path, load, read_and_encode, save};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use proto::tls::TlsArgs;

use crate::error::CliError;

/// The TLS material `connect()` ends up using, once CLI/env, the config
/// file and the plaintext default have been resolved into one value.
/// `Paths` and `Bytes` are kept distinct (rather than always materializing
/// paths) because a config-file identity has no file on disk to point to --
/// it only ever exists as decoded bytes in memory.
pub enum ResolvedTls {
    Plaintext,
    Paths {
        cert: PathBuf,
        key: PathBuf,
        ca: PathBuf,
        server_name: String,
    },
    Bytes {
        cert: Vec<u8>,
        key: Vec<u8>,
        ca: Vec<u8>,
        server_name: String,
    },
}

/// Resolves the effective server address: CLI flag/env (already merged by
/// clap into `cli_server`) > config file > the plaintext default.
pub fn resolve_server(cli_server: Option<String>, file: Option<&FileConfig>) -> String {
    cli_server
        .or_else(|| file.map(|f| f.server.clone()))
        .unwrap_or_else(|| "http://127.0.0.1:50052".to_string())
}

/// Resolves the effective TLS material: CLI flag/env (already merged by
/// clap into `cli_tls`) > config file > plaintext. `cli_tls`'s "all three or
/// none" rule from `proto::tls::tls_mode` applies at each layer
/// independently -- a config file with a partial TLS identity is rejected
/// just like partial CLI flags are.
pub fn resolve_tls(cli_tls: &TlsArgs, file: Option<&FileConfig>) -> Result<ResolvedTls, CliError> {
    match (&cli_tls.tls_cert, &cli_tls.tls_key, &cli_tls.tls_ca) {
        (Some(cert), Some(key), Some(ca)) => {
            let server_name = cli_tls.tls_server_name.clone().ok_or_else(|| {
                CliError::InvalidUsage(
                    "--tls-server-name is required when connecting over mTLS (--tls-cert/--tls-key/--tls-ca set)"
                        .to_string(),
                )
            })?;
            return Ok(ResolvedTls::Paths {
                cert: cert.clone(),
                key: key.clone(),
                ca: ca.clone(),
                server_name,
            });
        }
        (None, None, None) => {}
        _ => {
            return Err(CliError::InvalidUsage(
                "--tls-cert, --tls-key and --tls-ca must all be set together (mTLS) or all omitted (plaintext)"
                    .to_string(),
            ));
        }
    }

    let Some(file) = file else {
        return Ok(ResolvedTls::Plaintext);
    };

    match (
        &file.certificate_authority_data,
        &file.client_certificate_data,
        &file.client_key_data,
    ) {
        (Some(ca), Some(cert), Some(key)) => {
            let server_name = cli_tls
                .tls_server_name
                .clone()
                .or_else(|| file.tls_server_name.clone())
                .ok_or_else(|| {
                    CliError::InvalidUsage(
                        "config file has TLS data but no tls-server-name (set it in the file or pass --tls-server-name)"
                            .to_string(),
                    )
                })?;
            Ok(ResolvedTls::Bytes {
                cert: decode(cert, "client-certificate-data")?,
                key: decode(key, "client-key-data")?,
                ca: decode(ca, "certificate-authority-data")?,
                server_name,
            })
        }
        (None, None, None) => Ok(ResolvedTls::Plaintext),
        _ => Err(CliError::InvalidUsage(
            "config file TLS data is incomplete (certificate-authority-data, client-certificate-data \
             and client-key-data must all be set or all absent)"
                .to_string(),
        )),
    }
}

fn decode(value: &str, field: &'static str) -> Result<Vec<u8>, CliError> {
    BASE64
        .decode(value)
        .map_err(|source| CliError::DecodeConfig { field, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tls_args(
        cert: Option<&str>,
        key: Option<&str>,
        ca: Option<&str>,
        name: Option<&str>,
    ) -> TlsArgs {
        TlsArgs {
            tls_cert: cert.map(PathBuf::from),
            tls_key: key.map(PathBuf::from),
            tls_ca: ca.map(PathBuf::from),
            tls_server_name: name.map(String::from),
        }
    }

    fn file_config_with_tls() -> FileConfig {
        FileConfig {
            server: "https://cp:50052".to_string(),
            tls_server_name: Some("api".to_string()),
            certificate_authority_data: Some(BASE64.encode("ca-pem")),
            client_certificate_data: Some(BASE64.encode("cert-pem")),
            client_key_data: Some(BASE64.encode("key-pem")),
        }
    }

    #[test]
    fn resolve_server_prefers_cli_over_file() {
        let file = FileConfig {
            server: "https://from-file:50052".to_string(),
            ..Default::default()
        };
        assert_eq!(
            resolve_server(Some("https://from-cli:50052".to_string()), Some(&file)),
            "https://from-cli:50052"
        );
    }

    #[test]
    fn resolve_server_falls_back_to_file_then_default() {
        let file = FileConfig {
            server: "https://from-file:50052".to_string(),
            ..Default::default()
        };
        assert_eq!(resolve_server(None, Some(&file)), "https://from-file:50052");
        assert_eq!(resolve_server(None, None), "http://127.0.0.1:50052");
    }

    #[test]
    fn resolve_tls_is_plaintext_with_nothing_set() {
        assert!(matches!(
            resolve_tls(&tls_args(None, None, None, None), None).unwrap(),
            ResolvedTls::Plaintext
        ));
    }

    #[test]
    fn resolve_tls_prefers_cli_paths_over_file_bytes() {
        let file = file_config_with_tls();
        let resolved = resolve_tls(
            &tls_args(Some("c.pem"), Some("k.pem"), Some("ca.pem"), Some("api")),
            Some(&file),
        )
        .unwrap();
        assert!(matches!(resolved, ResolvedTls::Paths { .. }));
    }

    #[test]
    fn resolve_tls_falls_back_to_file_bytes() {
        let file = file_config_with_tls();
        let resolved = resolve_tls(&tls_args(None, None, None, None), Some(&file)).unwrap();
        match resolved {
            ResolvedTls::Bytes {
                cert,
                key,
                ca,
                server_name,
            } => {
                assert_eq!(cert, b"cert-pem");
                assert_eq!(key, b"key-pem");
                assert_eq!(ca, b"ca-pem");
                assert_eq!(server_name, "api");
            }
            _ => panic!("expected Bytes"),
        }
    }

    #[test]
    fn resolve_tls_rejects_partial_cli_flags() {
        assert!(resolve_tls(&tls_args(Some("c.pem"), None, None, None), None).is_err());
    }

    #[test]
    fn resolve_tls_rejects_partial_file_tls_data() {
        let file = FileConfig {
            server: "https://cp:50052".to_string(),
            client_certificate_data: Some(BASE64.encode("cert-pem")),
            ..Default::default()
        };
        assert!(resolve_tls(&tls_args(None, None, None, None), Some(&file)).is_err());
    }

    #[test]
    fn resolve_tls_requires_server_name_alongside_file_tls_data() {
        let mut file = file_config_with_tls();
        file.tls_server_name = None;
        assert!(resolve_tls(&tls_args(None, None, None, None), Some(&file)).is_err());
    }

    #[test]
    fn resolve_tls_prefers_cli_server_name_over_file_when_using_file_tls_data() {
        let file = file_config_with_tls();
        let resolved = resolve_tls(
            &tls_args(None, None, None, Some("cli-override")),
            Some(&file),
        )
        .unwrap();
        match resolved {
            ResolvedTls::Bytes { server_name, .. } => {
                assert_eq!(server_name, "cli-override");
            }
            _ => panic!("expected Bytes"),
        }
    }
}
