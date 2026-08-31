use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::net::Ipv4Addr;

use super::bridge::BRIDGE_NAME;
use super::system::{output, run, succeeds};
use crate::ip_pool::IpPoolDirectory;
use crate::state::{StateStore, WorkloadRecord};

const IP: &str = "ip";

pub(crate) fn reconcile(pools: &IpPoolDirectory, state: &StateStore) -> io::Result<()> {
    let live = drop_stale_records(state)?;
    rebuild_ip_pools(pools, &live)?;
    remove_orphan_interfaces(&live)?;
    reinstall_port_mappings(&live)?;
    Ok(())
}

fn drop_stale_records(state: &StateStore) -> io::Result<Vec<WorkloadRecord>> {
    let mut live = Vec::new();
    for record in state.records()? {
        if succeeds(IP, &["link", "show", "dev", &record.host_interface])? {
            live.push(record);
        } else {
            eprintln!(
                "cni: dropping stale record for {}/{} on {}: host interface {} is gone",
                record.workload_name,
                record.instance_name,
                record.network_name,
                record.host_interface
            );
            super::firewall::delete_mappings(&record.ip_address, &record.port_mappings)?;
            state.delete(
                &record.workload_name,
                &record.instance_name,
                &record.network_name,
            )?;
        }
    }
    Ok(live)
}

fn rebuild_ip_pools(pools: &IpPoolDirectory, live: &[WorkloadRecord]) -> io::Result<()> {
    let mut allocated_by_vlan: BTreeMap<u32, BTreeSet<Ipv4Addr>> = BTreeMap::new();
    for record in live {
        let address: Ipv4Addr = record.ip_address.parse().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "stored IP address is invalid")
        })?;
        allocated_by_vlan
            .entry(record.vlan_id)
            .or_default()
            .insert(address);
    }
    for vlan in pools.known_vlans()? {
        let allocated = allocated_by_vlan
            .remove(&u32::from(vlan))
            .unwrap_or_default();
        pools.pool(u32::from(vlan))?.reset(allocated)?;
    }
    Ok(())
}

fn remove_orphan_interfaces(live: &[WorkloadRecord]) -> io::Result<()> {
    for port in bridge_ports(BRIDGE_NAME)? {
        if is_workload_interface(&port) && !live.iter().any(|record| record.host_interface == port)
        {
            eprintln!("cni: removing orphan interface {port}");
            run(IP, &["link", "delete", &port])?;
        }
    }
    Ok(())
}

fn reinstall_port_mappings(live: &[WorkloadRecord]) -> io::Result<()> {
    for record in live {
        super::firewall::add_mappings(&record.ip_address, &record.port_mappings)?;
    }
    Ok(())
}

// `/sys/class/net` is pinned to the netns that mounted it, not to the
// reader's current netns, so a plain fs read lies whenever the daemon
// reached its netns via setns() without also getting a fresh sysfs mount
// (exactly what `nsenter -n` does, and what cni/tests/integration.sh uses to
// simulate nodes). Asking `ip` as a subprocess follows the daemon's actual
// netns correctly.
fn bridge_ports(bridge: &str) -> io::Result<Vec<String>> {
    let listing = output(IP, &["-o", "link", "show", "master", bridge])?;
    Ok(listing
        .lines()
        .filter_map(|line| line.split(':').nth(1))
        .map(|name| name.trim().split('@').next().unwrap_or("").to_string())
        .filter(|name| !name.is_empty())
        .collect())
}

fn is_workload_interface(name: &str) -> bool {
    name.len() == 11
        && name.starts_with('v')
        && name[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_workload_interface_names() {
        assert!(is_workload_interface("v3f9a2b1c4d"));
        assert!(!is_workload_interface("barenetes0"));
        assert!(!is_workload_interface("barenetes0.100"));
        assert!(!is_workload_interface("barenetes-vx"));
        assert!(!is_workload_interface("v3f9a2b1c4"));
        assert!(!is_workload_interface("v3f9a2b1c4dz"));
    }
}
