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
    rebuild_ip_pools(pools, &live);
    remove_orphan_interfaces(&live)?;
    reinstall_vlan_networking(&live)?;
    reinstall_port_mappings(&live);
    Ok(())
}

fn drop_stale_records(state: &StateStore) -> io::Result<Vec<WorkloadRecord>> {
    let mut live = Vec::new();
    for record in state.records()? {
        match succeeds(IP, &["link", "show", "dev", &record.host_interface]) {
            Ok(true) => live.push(record),
            Ok(false) => {
                eprintln!(
                    "cni: dropping stale record for {}/{} on {}: host interface {} is gone",
                    record.workload_name,
                    record.instance_name,
                    record.network_name,
                    record.host_interface
                );
                if let Err(error) =
                    super::firewall::delete_mappings(&record.ip_address, &record.port_mappings)
                {
                    eprintln!(
                        "cni: failed to remove port mappings for {}/{}: {error}",
                        record.workload_name, record.instance_name
                    );
                }
                if let Err(error) = state.delete(
                    &record.workload_name,
                    &record.instance_name,
                    &record.network_name,
                ) {
                    eprintln!(
                        "cni: failed to delete stale record for {}/{}: {error}",
                        record.workload_name, record.instance_name
                    );
                }
            }
            Err(error) => {
                eprintln!(
                    "cni: keeping {}/{} as live: failed to check host interface {}: {error}",
                    record.workload_name, record.instance_name, record.host_interface
                );
                live.push(record);
            }
        }
    }
    Ok(live)
}

fn rebuild_ip_pools(pools: &IpPoolDirectory, live: &[WorkloadRecord]) {
    let mut allocated_by_vlan: BTreeMap<u32, BTreeSet<Ipv4Addr>> = BTreeMap::new();
    for record in live {
        match record.ip_address.parse::<Ipv4Addr>() {
            Ok(address) => {
                allocated_by_vlan
                    .entry(record.vlan_id)
                    .or_default()
                    .insert(address);
            }
            Err(_) => eprintln!(
                "cni: ignoring invalid stored IP address for {}/{}: {}",
                record.workload_name, record.instance_name, record.ip_address
            ),
        }
    }
    let mut vlans: BTreeSet<u32> = allocated_by_vlan.keys().copied().collect();
    match pools.known_vlans() {
        Ok(known) => vlans.extend(known.into_iter().map(u32::from)),
        Err(error) => eprintln!("cni: failed to list known IP pools: {error}"),
    }
    for vlan in vlans {
        let allocated = allocated_by_vlan.remove(&vlan).unwrap_or_default();
        match pools.pool(vlan).and_then(|pool| pool.reset(allocated)) {
            Ok(()) => {}
            Err(error) => eprintln!("cni: failed to rebuild IP pool for vlan {vlan}: {error}"),
        }
    }
}

fn remove_orphan_interfaces(live: &[WorkloadRecord]) -> io::Result<()> {
    for port in bridge_ports(BRIDGE_NAME)? {
        if super::workload::is_host_interface_name(&port)
            && !live.iter().any(|record| record.host_interface == port)
        {
            eprintln!("cni: removing orphan interface {port}");
            if let Err(error) = run(IP, &["link", "delete", &port]) {
                eprintln!("cni: failed to remove orphan interface {port}: {error}");
            }
        }
    }
    Ok(())
}

fn reinstall_vlan_networking(live: &[WorkloadRecord]) -> io::Result<()> {
    let node = super::node_id()?;
    let vlans: BTreeSet<u32> = live.iter().map(|record| record.vlan_id).collect();
    for vlan in vlans {
        match u8::try_from(vlan) {
            Ok(vlan) => {
                if let Err(error) = super::vlan::ensure(vlan, node) {
                    eprintln!("cni: failed to reinstall networking for vlan {vlan}: {error}");
                }
            }
            Err(_) => eprintln!("cni: skipping reinstall for invalid stored vlan_id {vlan}"),
        }
    }
    Ok(())
}

fn reinstall_port_mappings(live: &[WorkloadRecord]) {
    for record in live {
        if let Err(error) = super::firewall::add_mappings(&record.ip_address, &record.port_mappings)
        {
            eprintln!(
                "cni: failed to reinstall port mappings for {}/{}: {error}",
                record.workload_name, record.instance_name
            );
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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

        rebuild_ip_pools(&pools, &live);

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
    fn rebuild_ip_pools_skips_an_invalid_address_without_aborting() {
        let directory = tempfile::tempdir().unwrap();
        let pools = IpPoolDirectory::new(directory.path(), 1);
        let mut broken = record(100, "not-an-ip");
        broken.instance_name = "broken".into();
        let live = vec![broken, record(100, "10.100.1.5")];

        rebuild_ip_pools(&pools, &live);

        let released = pools
            .pool(100)
            .unwrap()
            .release(Ipv4Addr::new(10, 100, 1, 5))
            .unwrap();
        assert!(
            released,
            "the valid address should still have been marked allocated"
        );
    }

    #[test]
    fn rebuild_ip_pools_clears_a_known_vlan_with_no_live_records() {
        let directory = tempfile::tempdir().unwrap();
        let pools = IpPoolDirectory::new(directory.path(), 1);
        pools.pool(200).unwrap().allocate().unwrap();

        rebuild_ip_pools(&pools, &[]);

        let released = pools
            .pool(200)
            .unwrap()
            .release(Ipv4Addr::new(10, 200, 1, 2))
            .unwrap();
        assert!(!released, "the leaked address should have been cleared");
    }
}
