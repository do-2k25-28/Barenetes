use std::io;

use crate::system::{run, succeeds};

const IP: &str = "ip";
pub(crate) const BRIDGE_NAME: &str = "barenetes0";
const BRIDGE_ADDRESS: &str = "10.244.0.1/16";
const DEFAULT_MTU: u32 = 1450;

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

    run(
        IP,
        &["address", "replace", BRIDGE_ADDRESS, "dev", BRIDGE_NAME],
    )?;
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

pub(crate) fn mtu() -> io::Result<u32> {
    let Some(value) = std::env::var("BARENETES_MTU")
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return Ok(DEFAULT_MTU);
    };
    value
        .parse()
        .ok()
        .filter(|mtu| (576..=9000).contains(mtu))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "BARENETES_MTU must be between 576 and 9000",
            )
        })
}
