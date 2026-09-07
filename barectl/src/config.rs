//! `barectl`'s kubeconfig-style config file: a single-profile YAML file
//! (default `$HOME/.config/barectl/config`, overridable via
//! `--barectl-config`/`$BARECTL_CONFIG`) that holds the server address and,
//! optionally, a client TLS identity already issued by `barenetes-pki`
//! (base64-embedded, the same way a kubeconfig embeds
//! `client-certificate-data`). Resolution priority everywhere is CLI flag >
//! env var > this file > a plaintext default, matching kubectl's flag vs.
//! current-context precedence.
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use proto::tls::TlsArgs;
use serde::{Deserialize, Serialize};

use crate::error::CliError;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct FileConfig {
    pub server: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_server_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate_authority_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_certificate_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_key_data: Option<String>,
}

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

/// `$HOME/.config/barectl/config`, or `None` if `$HOME` isn't set (in which
/// case the caller falls back to no config file rather than erroring, except
/// for `config set`/`config view` which need a concrete path to act on).
pub fn default_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config/barectl/config"))
}

/// Loads the config file at `path`. A missing file is `Ok(None)`, not an
/// error -- the config file is always optional.
pub fn load(path: &Path) -> Result<Option<FileConfig>, CliError> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            serde_yaml::from_str(&contents)
                .map(Some)
                .map_err(|source| CliError::ParseConfig {
                    path: path.to_path_buf(),
                    source,
                })
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(CliError::ReadConfig {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Writes `config` to `path`, replacing whatever was there. Creates the
/// parent directory (0700) if needed, and writes the file itself at 0600
/// since it may embed a private key.
pub fn save(path: &Path, config: &FileConfig) -> Result<(), CliError> {
    let yaml = serde_yaml::to_string(config).map_err(|source| CliError::SerializeConfig {
        path: path.to_path_buf(),
        source,
    })?;

    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        fs::create_dir_all(dir).map_err(|source| CliError::WriteConfig {
            path: path.to_path_buf(),
            source,
        })?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|source| {
            CliError::WriteConfig {
                path: path.to_path_buf(),
                source,
            }
        })?;
    }

    write_atomic(path, &yaml, 0o600).map_err(|source| CliError::WriteConfig {
        path: path.to_path_buf(),
        source,
    })
}

/// Writes `contents` to `path` at `mode` without ever exposing a
/// world/group-readable window, by creating a sibling temp file with the
/// target mode already set and renaming it into place (same idiom as
/// `barenetes-pki`'s `write_pem`).
fn write_atomic(path: &Path, contents: &str, mode: u32) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp_path = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config"),
        std::process::id()
    ));

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(&tmp_path)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    fs::rename(&tmp_path, path)
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
            let server_name = file.tls_server_name.clone().ok_or_else(|| {
                CliError::InvalidUsage(
                    "config file has TLS data but no tls-server-name".to_string(),
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

/// Reads `path` and base64-encodes its raw bytes, for embedding into a
/// [`FileConfig`] by `config set`.
pub fn read_and_encode(path: &Path) -> Result<String, CliError> {
    let bytes = fs::read(path).map_err(|source| CliError::ReadTlsFile {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(BASE64.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "barectl-config-test-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

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
    fn load_returns_none_for_a_missing_file() {
        let dir = tempdir("missing");
        assert!(load(&dir.join("config")).unwrap().is_none());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir("roundtrip");
        let path = dir.join("nested").join("config");
        let config = file_config_with_tls();

        save(&path, &config).unwrap();
        let loaded = load(&path).unwrap().unwrap();

        assert_eq!(loaded, config);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_writes_the_file_at_mode_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir("perms");
        let path = dir.join("config");
        save(&path, &file_config_with_tls()).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        fs::remove_dir_all(&dir).ok();
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
}
