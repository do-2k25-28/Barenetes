use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::Ipv4Addr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const MAX_STATE_SIZE: u64 = 1024 * 1024;

// Un répertoire par VLAN isole le carnet d'adresses de chaque tenant : deux tenants
// ne peuvent jamais partager ni verrou ni plage d'allocation.
#[derive(Clone)]
pub(crate) struct IpPoolDirectory {
    root: PathBuf,
    node: u8,
}

impl IpPoolDirectory {
    pub(crate) fn new(root: impl Into<PathBuf>, node: u8) -> Self {
        Self {
            root: root.into(),
            node,
        }
    }

    pub(crate) fn pool(&self, vlan: u32) -> io::Result<IpPool> {
        let vlan = u8::try_from(vlan).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "vlan_id must fit in a single byte",
            )
        })?;
        let (first, last) = crate::addressing::pool_range(vlan, self.node);
        IpPool::new(self.root.join(format!("vlan-{vlan}")), first, last)
    }
}

#[derive(Clone)]
pub(crate) struct IpPool {
    directory: PathBuf,
    first: u32,
    last: u32,
}

#[derive(Default, Deserialize, Serialize)]
struct PoolState {
    allocated: BTreeSet<Ipv4Addr>,
}

impl IpPool {
    pub(crate) fn new(
        directory: impl Into<PathBuf>,
        first: Ipv4Addr,
        last: Ipv4Addr,
    ) -> io::Result<Self> {
        if u32::from(first) > u32::from(last) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid IP pool range",
            ));
        }
        Ok(Self {
            directory: directory.into(),
            first: u32::from(first),
            last: u32::from(last),
        })
    }

    pub(crate) fn allocate(&self) -> io::Result<Ipv4Addr> {
        self.with_state(|state| {
            let address = (self.first..=self.last)
                .map(Ipv4Addr::from)
                .find(|address| !state.allocated.contains(address))
                .ok_or_else(|| io::Error::other("IP pool is exhausted"))?;
            state.allocated.insert(address);
            Ok(address)
        })
    }

    pub(crate) fn release(&self, address: Ipv4Addr) -> io::Result<bool> {
        let numeric = u32::from(address);
        if numeric < self.first || numeric > self.last {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "address is outside the IP pool",
            ));
        }
        self.with_state(|state| Ok(state.allocated.remove(&address)))
    }

    fn with_state<T>(
        &self,
        operation: impl FnOnce(&mut PoolState) -> io::Result<T>,
    ) -> io::Result<T> {
        std::fs::create_dir_all(&self.directory)?;
        std::fs::set_permissions(&self.directory, std::fs::Permissions::from_mode(0o700))?;
        let lock = secure_open(&self.directory.join("ip-pool.lock"), true)?;
        lock.lock_exclusive()?;

        let mut state = read_state(&self.directory.join("ip-pool.json"))?;
        let result = operation(&mut state)?;
        write_state(&self.directory, &state)?;
        Ok(result)
    }
}

fn secure_open(path: &Path, create: bool) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

fn read_state(path: &Path) -> io::Result<PoolState> {
    let file = match secure_open(path, false) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(PoolState::default()),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(MAX_STATE_SIZE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STATE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IP pool state is too large",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn write_state(directory: &Path, state: &PoolState) -> io::Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer(&mut temporary, state).map_err(io::Error::other)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(directory.join("ip-pool.json"))
        .map_err(|error| error.error)?;
    File::open(directory)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_gives_each_tenant_its_own_pool() {
        let directory = tempfile::tempdir().unwrap();
        let pools = IpPoolDirectory::new(directory.path(), 1);
        let tenant_a = pools.pool(100).unwrap();
        let tenant_b = pools.pool(200).unwrap();
        let address_a = tenant_a.allocate().unwrap();
        let address_b = tenant_b.allocate().unwrap();
        assert_ne!(address_a, address_b);
        assert_eq!(address_a, Ipv4Addr::new(10, 100, 1, 2));
        assert_eq!(address_b, Ipv4Addr::new(10, 200, 1, 2));
    }

    #[test]
    fn directory_rejects_a_vlan_outside_a_single_byte() {
        let directory = tempfile::tempdir().unwrap();
        let pools = IpPoolDirectory::new(directory.path(), 1);
        assert!(pools.pool(4094).is_err());
    }

    #[test]
    fn allocates_persists_and_releases_addresses() {
        let directory = tempfile::tempdir().unwrap();
        let first = Ipv4Addr::new(10, 0, 0, 2);
        let last = Ipv4Addr::new(10, 0, 0, 3);
        let pool = IpPool::new(directory.path(), first, last).unwrap();
        assert_eq!(pool.allocate().unwrap(), first);
        assert_eq!(pool.allocate().unwrap(), last);
        assert!(pool.allocate().is_err());
        assert!(pool.release(first).unwrap());
        assert_eq!(
            IpPool::new(directory.path(), first, last)
                .unwrap()
                .allocate()
                .unwrap(),
            first
        );
    }

    #[test]
    fn fails_closed_on_corrupt_state() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("ip-pool.json"), b"invalid").unwrap();
        let pool = IpPool::new(
            directory.path(),
            Ipv4Addr::new(10, 0, 0, 2),
            Ipv4Addr::new(10, 0, 0, 3),
        )
        .unwrap();
        assert_eq!(
            pool.allocate().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
