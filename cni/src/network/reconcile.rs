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
    reinstall_vlan_networking(&live)?;
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
    let mut vlans: BTreeSet<u32> = allocated_by_vlan.keys().copied().collect();
    vlans.extend(pools.known_vlans()?.into_iter().map(u32::from));
    for vlan in vlans {
        let allocated = allocated_by_vlan.remove(&vlan).unwrap_or_default();
        pools.pool(vlan)?.reset(allocated)?;
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

fn reinstall_vlan_networking(live: &[WorkloadRecord]) -> io::Result<()> {
    let node = super::node_id()?;
    let vlans: BTreeSet<u32> = live.iter().map(|record| record.vlan_id).collect();
    for vlan in vlans {
        let vlan = u8::try_from(vlan)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "stored vlan_id is invalid"))?;
        super::vlan::ensure(vlan, node)?;
    }
    Ok(())
}

fn reinstall_port_mappings(live: &[WorkloadRecord]) -> io::Result<()> {
    for record in live {
        super::firewall::add_mappings(&record.ip_address, &record.port_mappings)?;
    }
    Ok(())
}

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

    fn record(vlan_id: u32, ip_address: &str) -> WorkloadRecord {
        WorkloadRecord {
            workload_name: "api".into(),
            instance_name: "api-1".into(),
            network_name: "tenant-a".into(),
            host_interface: "v123".into(),
            interface_name: "eth0".into(),
            ip_address: ip_address.into(),
            gateway: "10.100.1.1".into(),
            vlan_id,
            port_mappings: Vec::new(),
        }
    }

    #[test]
    fn rebuild_ip_pools_covers_a_live_vlan_missing_its_pool_directory() {
        let directory = tempfile::tempdir().unwrap();
        let pools = IpPoolDirectory::new(directory.path(), 1);
        let live = vec![record(100, "10.100.1.5")];

        rebuild_ip_pools(&pools, &live).unwrap();

        assert_eq!(pools.known_vlans().unwrap(), vec![100]);
        let released = pools
            .pool(100)
            .unwrap()
            .release(Ipv4Addr::new(10, 100, 1, 5))
            .unwrap();
        assert!(
            released,
            "the live address should have been marked allocated"
        );
    }

    #[test]
    fn rebuild_ip_pools_clears_a_known_vlan_with_no_live_records() {
        let directory = tempfile::tempdir().unwrap();
        let pools = IpPoolDirectory::new(directory.path(), 1);
        pools.pool(200).unwrap().allocate().unwrap();

        rebuild_ip_pools(&pools, &[]).unwrap();

        let released = pools
            .pool(200)
            .unwrap()
            .release(Ipv4Addr::new(10, 200, 1, 2))
            .unwrap();
        assert!(!released, "the leaked address should have been cleared");
    }
}
