#[path = "network/bridge.rs"]
mod bridge;
#[path = "network/overlay.rs"]
mod overlay;
#[path = "network/system.rs"]
mod system;
#[path = "network/workload.rs"]
mod workload;

pub(crate) use bridge::{BRIDGE_NAME, ensure as ensure_bridge};
pub(crate) use overlay::{ensure_overlay, node_id};
pub(crate) use system::{run, succeeds};
pub(crate) use workload::{add_workload_network, delete_workload_network, get_workload_network};
