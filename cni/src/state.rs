use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

const MAX_STATE_SIZE: u64 = 64 * 1024;

#[derive(Clone, Deserialize, Serialize)]
pub struct WorkloadRecord {
    pub workload_name: String,
    pub instance_name: String,
    pub network_name: String,
    pub host_interface: String,
    pub interface_name: String,
    pub ip_address: String,
    pub gateway: String,
}

#[derive(Clone)]
pub struct StateStore {
    directory: PathBuf,
}

impl StateStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    pub fn load(
        &self,
        workload: &str,
        instance: &str,
        network: &str,
    ) -> io::Result<Option<WorkloadRecord>> {
        let path = self.path(workload, instance, network);
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        file.take(MAX_STATE_SIZE + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_STATE_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "workload state is too large",
            ));
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    pub fn save(&self, record: &WorkloadRecord) -> io::Result<()> {
        std::fs::create_dir_all(&self.directory)?;
        std::fs::set_permissions(&self.directory, std::fs::Permissions::from_mode(0o700))?;
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
        serde_json::to_writer(&mut temporary, record).map_err(io::Error::other)?;
        temporary.flush()?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(self.path(
                &record.workload_name,
                &record.instance_name,
                &record.network_name,
            ))
            .map_err(|error| error.error)?;
        File::open(&self.directory)?.sync_all()
    }

    pub fn path(&self, workload: &str, instance: &str, network: &str) -> PathBuf {
        self.directory.join(format!(
            "{}.json",
            stable_id(&[workload, instance, network])
        ))
    }

    pub fn delete(&self, workload: &str, instance: &str, network: &str) -> io::Result<bool> {
        match std::fs::remove_file(self.path(workload, instance, network)) {
            Ok(()) => {
                File::open(&self.directory)?.sync_all()?;
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}

pub fn stable_id(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    let mut id = String::with_capacity(24);
    for byte in &digest[..12] {
        write!(&mut id, "{byte:02x}").expect("writing to a string cannot fail");
    }
    id
}
