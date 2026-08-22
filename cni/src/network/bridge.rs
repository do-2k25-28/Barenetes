use std::io;

use super::system::{mtu, run, succeeds};

const IP: &str = "ip";
pub(crate) const BRIDGE_NAME: &str = "barenetes0";

pub(crate) fn ensure() -> io::Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "CNI network setup requires root privileges",
        ));
    }
    if !succeeds(IP, &["link", "show", "dev", BRIDGE_NAME])?
        && !succeeds(IP, &["link", "add", "name", BRIDGE_NAME, "type", "bridge"])?
        && !succeeds(IP, &["link", "show", "dev", BRIDGE_NAME])?
    {
        return Err(io::Error::other("failed to create CNI bridge"));
    }
    let mtu = mtu()?.to_string();
    run(IP, &["link", "set", "dev", BRIDGE_NAME, "mtu", &mtu])?;
    run(IP, &["link", "set", "dev", BRIDGE_NAME, "up"])?;
    run(
        IP,
        &[
            "link",
            "set",
            "dev",
            BRIDGE_NAME,
            "type",
            "bridge",
            "vlan_filtering",
            "1",
        ],
    )
}
