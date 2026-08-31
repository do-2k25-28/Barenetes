use proto::shared::v1::Port;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

const MAX_STATE_SIZE: u64 = 64 * 1024;

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct WorkloadRecord {
    pub(crate) workload_name: String,
    pub(crate) instance_name: String,
    pub(crate) network_name: String,
    pub(crate) host_interface: String,
    pub(crate) interface_name: String,
    pub(crate) ip_address: String,
    pub(crate) gateway: String,
    #[serde(default)]
    pub(crate) vlan_id: u32,
    #[serde(default)]
    pub(crate) port_mappings: Vec<Port>,
}

#[derive(Clone)]
pub(crate) struct StateStore {
    directory: PathBuf,
}

impl StateStore {
    pub(crate) fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    pub(crate) fn load(
        &self,
        workload: &str,
        instance: &str,
        network: &str,
    ) -> io::Result<Option<WorkloadRecord>> {
        let path = self.path(workload, instance, network);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        read_record(file).map(Some)
    }

    pub(crate) fn save(&self, record: &WorkloadRecord) -> io::Result<()> {
        std::fs::create_dir_all(&self.directory)?;
        std::fs::set_permissions(&self.directory, std::fs::Permissions::from_mode(0o700))?;
        let bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_STATE_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "workload state is too large",
            ));
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
        temporary.write_all(&bytes)?;
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

    pub(crate) fn path(&self, workload: &str, instance: &str, network: &str) -> PathBuf {
        self.directory.join(format!(
            "{}.json",
            stable_id(&[workload, instance, network])
        ))
    }

    pub(crate) fn delete(&self, workload: &str, instance: &str, network: &str) -> io::Result<bool> {
        match std::fs::remove_file(self.path(workload, instance, network)) {
            Ok(()) => {
                File::open(&self.directory)?.sync_all()?;
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn port_is_used(&self, protocol: i32, host_port: u32) -> io::Result<bool> {
        let entries = match std::fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let record = read_record(File::open(path)?)?;
            if record
                .port_mappings
                .iter()
                .any(|mapping| mapping.protocol == protocol && mapping.external == host_port)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn records(&self) -> io::Result<Vec<WorkloadRecord>> {
        let entries = match std::fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut records = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            records.push(read_record(File::open(path)?)?);
        }
        Ok(records)
    }
}

fn read_record(file: File) -> io::Result<WorkloadRecord> {
    let mut bytes = Vec::new();
    file.take(MAX_STATE_SIZE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STATE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "workload state is too large",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub(crate) fn stable_id(parts: &[&str]) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use proto::shared::v1::Protocol;

    fn record() -> WorkloadRecord {
        WorkloadRecord {
            workload_name: "api".into(),
            instance_name: "api-1".into(),
            network_name: "tenant-a".into(),
            host_interface: "v123".into(),
            interface_name: "eth0".into(),
            ip_address: "10.100.1.2".into(),
            gateway: "10.100.1.1".into(),
            vlan_id: 42,
            port_mappings: vec![Port {
                internal: 80,
                external: 8080,
                protocol: Protocol::Tcp as i32,
            }],
        }
    }

    #[test]
    fn saves_loads_and_deletes_a_record() {
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::new(directory.path());
        let expected = record();
        store.save(&expected).unwrap();
        let loaded = store.load("api", "api-1", "tenant-a").unwrap().unwrap();
        assert_eq!(loaded.ip_address, expected.ip_address);
        assert_eq!(loaded.vlan_id, expected.vlan_id);
        assert!(store.port_is_used(Protocol::Tcp as i32, 8080).unwrap());
        assert!(store.delete("api", "api-1", "tenant-a").unwrap());
        assert!(store.load("api", "api-1", "tenant-a").unwrap().is_none());
    }

    #[test]
    fn stable_ids_include_part_boundaries() {
        assert_ne!(stable_id(&["ab", "c"]), stable_id(&["a", "bc"]));
    }

    #[test]
    fn records_lists_every_saved_workload() {
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::new(directory.path());
        assert!(store.records().unwrap().is_empty());

        let mut second = record();
        second.instance_name = "api-2".into();
        store.save(&record()).unwrap();
        store.save(&second).unwrap();

        let mut instances: Vec<_> = store
            .records()
            .unwrap()
            .into_iter()
            .map(|record| record.instance_name)
            .collect();
        instances.sort();
        assert_eq!(instances, vec!["api-1", "api-2"]);
    }
}
