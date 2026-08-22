use std::io;
use std::net::Ipv4Addr;

use super::bridge;
use super::system::{mtu, run, succeeds};
use crate::addressing;

const IP: &str = "ip";
const BRIDGE: &str = "bridge";

pub(crate) fn interface_name(vlan: u8) -> String {
    format!("{}.{vlan}", bridge::BRIDGE_NAME)
}

// Idempotent : la sous-interface porte la passerelle du tenant, le bridge lui-même
// n'a plus d'adresse. Le VLAN est taggé sur le port du bridge, ce qui est correct
// puisque c'est la sous-interface qui le reçoit, pas le bridge en accès direct.
pub(crate) fn ensure(vlan: u8, node: u8) -> io::Result<Ipv4Addr> {
    let name = interface_name(vlan);
    let vlan_string = vlan.to_string();
    if !succeeds(IP, &["link", "show", "dev", &name])? {
        run(
            BRIDGE,
            &[
                "vlan",
                "add",
                "dev",
                bridge::BRIDGE_NAME,
                "vid",
                &vlan_string,
                "self",
            ],
        )?;
        run(
            IP,
            &[
                "link",
                "add",
                "link",
                bridge::BRIDGE_NAME,
                "name",
                &name,
                "type",
                "vlan",
                "id",
                &vlan_string,
            ],
        )?;
    }
    let gateway = addressing::gateway(vlan, node);
    run(
        IP,
        &["address", "replace", &format!("{gateway}/16"), "dev", &name],
    )?;
    let interface_mtu = mtu()?.to_string();
    run(IP, &["link", "set", "dev", &name, "mtu", &interface_mtu])?;
    run(IP, &["link", "set", "dev", &name, "up"])?;
    Ok(gateway)
}
