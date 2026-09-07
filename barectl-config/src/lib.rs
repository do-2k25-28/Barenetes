//! The on-disk format of `barectl`'s kubeconfig-style config file: a
//! single-profile YAML file holding a server address and, optionally, a
//! client TLS identity (base64-embedded, the same way a kubeconfig embeds
//! `client-certificate-data`).
//!
//! Shared between `barectl` (which reads it to connect) and
//! `barenetes-pki`'s `barectl-config` subcommand (which writes one for an
//! operator to hand to a user) so the two can never drift apart on what the
//! file looks like.
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("could not parse {path}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_yaml::Error,
    },

    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("could not serialize config: {0}")]
    Serialize(serde_yaml::Error),
}

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

/// `$HOME/.config/barectl/config`, or `None` if `$HOME` isn't set.
pub fn default_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config/barectl/config"))
}

/// Loads the config file at `path`. A missing file is `Ok(None)`, not an
/// error -- the config file is always optional.
pub fn load(path: &Path) -> Result<Option<FileConfig>, Error> {
    match fs::read_to_string(path) {
        Ok(contents) => serde_yaml::from_str(&contents)
            .map(Some)
            .map_err(|source| Error::Parse {
                path: path.to_path_buf(),
                source,
            }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Serializes `config` to YAML.
pub fn to_yaml(config: &FileConfig) -> Result<String, Error> {
    serde_yaml::to_string(config).map_err(Error::Serialize)
}

/// Writes `config` to `path`, replacing whatever was there. Creates the
/// parent directory (0700) if needed, and writes the file itself at 0600
/// since it may embed a private key.
pub fn save(path: &Path, config: &FileConfig) -> Result<(), Error> {
    let yaml = to_yaml(config)?;

    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        fs::create_dir_all(dir).map_err(|source| Error::Write {
            path: path.to_path_buf(),
            source,
        })?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|source| {
            Error::Write {
                path: path.to_path_buf(),
                source,
            }
        })?;
    }

    write_atomic(path, &yaml, 0o600).map_err(|source| Error::Write {
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

/// Reads `path` and base64-encodes its raw bytes, for embedding into a
/// [`FileConfig`].
pub fn read_and_encode(path: &Path) -> Result<String, Error> {
    let bytes = fs::read(path).map_err(|source| Error::Read {
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
            "barectl-config-lib-test-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
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
        let dir = tempdir("perms");
        let path = dir.join("config");
        save(&path, &file_config_with_tls()).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn to_yaml_omits_absent_tls_fields() {
        let config = FileConfig {
            server: "http://127.0.0.1:50052".to_string(),
            ..Default::default()
        };
        let yaml = to_yaml(&config).unwrap();
        assert!(!yaml.contains("certificate-authority-data"));
        assert!(!yaml.contains("client-certificate-data"));
        assert!(!yaml.contains("client-key-data"));
    }

    #[test]
    fn read_and_encode_base64_encodes_the_raw_file_bytes() {
        let dir = tempdir("read-encode");
        let path = dir.join("leaf.pem");
        fs::write(&path, "cert-contents").unwrap();

        let encoded = read_and_encode(&path).unwrap();
        assert_eq!(BASE64.decode(encoded).unwrap(), b"cert-contents");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_malformed_yaml() {
        let dir = tempdir("malformed");
        let path = dir.join("config");
        fs::write(&path, "not: valid: yaml: at: all:").unwrap();

        assert!(load(&path).is_err());
        fs::remove_dir_all(&dir).ok();
    }
}
