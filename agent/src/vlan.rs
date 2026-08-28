//! Sticky per-pod VLAN allocation.
//!
//! A pod gets a VLAN id on first apply and keeps it across re-applies and
//! restarts, so the CNI keeps seeing identical network settings for the same
//! pod. The mapping is mirrored to `<directory>/vlans.json` after every
//! change; the in-memory map guarded by a mutex is the working state.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// VLAN 1 stays reserved for the default tenant network.
const FIRST_VLAN: u32 = 2;
const LAST_VLAN: u32 = 4094;

#[derive(Default, Deserialize, Serialize)]
struct VlanState {
    allocations: HashMap<String, u32>,
}

pub struct VlanAllocations {
    path: PathBuf,
    state: Mutex<VlanState>,
}

impl VlanAllocations {
    pub fn new(directory: impl Into<PathBuf>) -> io::Result<Self> {
        let path = directory.into().join("vlans.json");
        let state = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => VlanState::default(),
            Err(error) => return Err(error),
        };
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    /// Returns the pod's existing VLAN or assigns it the lowest free one.
    pub fn allocate_for(&self, pod_id: &str) -> io::Result<u32> {
        let mut state = self.lock();
        if let Some(vlan) = state.allocations.get(pod_id) {
            return Ok(*vlan);
        }
        let used: std::collections::HashSet<u32> = state.allocations.values().copied().collect();
        let vlan = (FIRST_VLAN..=LAST_VLAN)
            .find(|vlan| !used.contains(vlan))
            .ok_or_else(|| io::Error::other("no free vlan left"))?;
        state.allocations.insert(pod_id.to_string(), vlan);
        save(&self.path, &state)?;
        Ok(vlan)
    }

    /// Forgets a pod's VLAN once all of its networks are detached.
    pub fn release(&self, pod_id: &str) -> io::Result<()> {
        let mut state = self.lock();
        if state.allocations.remove(pod_id).is_some() {
            save(&self.path, &state)?;
        }
        Ok(())
    }

    /// The VLAN assigned to `pod_id`, if any.
    pub fn vlan_of(&self, pod_id: &str) -> Option<u32> {
        self.lock().allocations.get(pod_id).copied()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VlanState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn save(path: &Path, state: &VlanState) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_vec(state).map_err(io::Error::other)?)
}
