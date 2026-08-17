use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::Ipv4Addr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const MAX_STATE_SIZE: u64 = 1024 * 1024;

#[derive(Clone)]
pub struct IpPool {
    directory: PathBuf,
    first: u32,
    last: u32,
}

#[derive(Default, Deserialize, Serialize)]
struct PoolState {
    allocated: BTreeSet<Ipv4Addr>,
}

impl IpPool {
    pub fn new(directory: impl Into<PathBuf>, first: Ipv4Addr, last: Ipv4Addr) -> io::Result<Self> {
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

    pub fn allocate(&self) -> io::Result<Ipv4Addr> {
        self.with_state(|state| {
            let address = (self.first..=self.last)
                .map(Ipv4Addr::from)
                .find(|address| !state.allocated.contains(address))
                .ok_or_else(|| io::Error::other("IP pool is exhausted"))?;
            state.allocated.insert(address);
            Ok(address)
        })
    }

    pub fn release(&self, address: Ipv4Addr) -> io::Result<bool> {
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
