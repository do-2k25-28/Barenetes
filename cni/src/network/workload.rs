use crate::ip_pool::IpPoolDirectory;
use std::fs::File;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;

use super::bridge;
use super::system::{self, mtu, run, succeeds};

use crate::state::{StateStore, WorkloadRecord, stable_id};
use proto::cni::v1::{
    AddWorkloadNetworkRequest, DeleteWorkloadNetworkRequest, GetWorkloadNetworkRequest, NetworkRef,
    NetworkState, WorkloadNetwork, WorkloadRef,
};

const IP: &str = "ip";
const BRIDGE: &str = "bridge";
const NSENTER: &str = "nsenter";

pub(crate) fn add_workload_network(
    request: AddWorkloadNetworkRequest,
    pools: &IpPoolDirectory,
    state: &StateStore,
) -> io::Result<WorkloadNetwork> {
    let (workload, network, pid, interface) = validate_add_request(&request)?;
    let netns = open_netns(&request.netns_path)?;
    super::firewall::validate_mappings(&request.port_mappings)?;
    if let Some(record) = state.load(
        &workload.workload_name,
        &workload.instance_name,
        &network.network_name,
    )? {
        if record.interface_name != interface
            || record.vlan_id != network.vlan_id
            || record.port_mappings != request.port_mappings
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "workload network already exists with different settings",
            ));
        }
        if !succeeds(IP, &["link", "show", "dev", &record.host_interface])? {
            // Reconcile a durable record whose kernel interface disappeared.
            super::firewall::delete_mappings(&record.ip_address, &record.port_mappings)?;
            let address = record.ip_address.parse().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "stored IP address is invalid")
            })?;
            pools.pool(record.vlan_id)?.release(address)?;
            state.delete(
                &workload.workload_name,
                &workload.instance_name,
                &network.network_name,
            )?;
        } else {
            return Ok(record_to_network(&record));
        }
    }
    for mapping in &request.port_mappings {
        if state.port_is_used(mapping.protocol, mapping.host_port)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "host port is already used",
            ));
        }
    }

    let node = super::node_id()?;
    let gateway = super::vlan::ensure(network.vlan_id as u8, node)?;
    let gateway_string = gateway.to_string();
    let pool = pools.pool(network.vlan_id)?;
    let address = pool.allocate()?;
    let host_interface = format!(
        "v{}",
        &stable_id(&[
            &workload.workload_name,
            &workload.instance_name,
            &network.network_name
        ])[..10]
    );
    let peer_interface = format!("p{}", &host_interface[1..]);
    let address_with_prefix = format!("{address}/16");
    let pid_string = pid.to_string();
    let interface_mtu = mtu()?.to_string();

    let setup = (|| {
        run(
            IP,
            &[
                "link",
                "add",
                &host_interface,
                "mtu",
                &interface_mtu,
                "type",
                "veth",
                "peer",
                "name",
                &peer_interface,
                "mtu",
                &interface_mtu,
            ],
        )?;
        if !netns_matches(&netns, pid)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "netns_path no longer refers to the requested namespace",
            ));
        }
        run(IP, &["link", "set", &peer_interface, "netns", &pid_string])?;
        run_in_namespace(
            &netns,
            &["link", "set", "dev", &peer_interface, "name", interface],
        )?;
        run_in_namespace(
            &netns,
            &["address", "replace", &address_with_prefix, "dev", interface],
        )?;
        run_in_namespace(&netns, &["link", "set", "dev", interface, "up"])?;
        run_in_namespace(&netns, &["link", "set", "dev", "lo", "up"])?;
        run_in_namespace(
            &netns,
            &[
                "route",
                "replace",
                "default",
                "via",
                &gateway_string,
                "dev",
                interface,
            ],
        )?;
        run(
            IP,
            &[
                "link",
                "set",
                "dev",
                &host_interface,
                "master",
                bridge::BRIDGE_NAME,
            ],
        )?;
        let vlan = network.vlan_id.to_string();
        run(BRIDGE, &["vlan", "del", "dev", &host_interface, "vid", "1"])?;
        run(
            BRIDGE,
            &[
                "vlan",
                "add",
                "dev",
                &host_interface,
                "vid",
                &vlan,
                "pvid",
                "untagged",
            ],
        )?;
        run(IP, &["link", "set", "dev", &host_interface, "up"])
    })();

    if let Err(error) = setup {
        let _ = run(IP, &["link", "delete", &host_interface]);
        let _ = pool.release(address);
        return Err(error);
    }

    let record = WorkloadRecord {
        workload_name: workload.workload_name.clone(),
        instance_name: workload.instance_name.clone(),
        network_name: network.network_name.clone(),
        host_interface,
        interface_name: interface.to_owned(),
        ip_address: address.to_string(),
        gateway: gateway_string,
        vlan_id: network.vlan_id,
        port_mappings: request.port_mappings.clone(),
    };
    if let Err(error) = super::firewall::add_mappings(&record.ip_address, &record.port_mappings) {
        let _ = run(IP, &["link", "delete", &record.host_interface]);
        let _ = pool.release(address);
        return Err(error);
    }
    if let Err(error) = state.save(&record) {
        let _ = super::firewall::delete_mappings(&record.ip_address, &record.port_mappings);
        let _ = run(IP, &["link", "delete", &record.host_interface]);
        let _ = pool.release(address);
        return Err(error);
    }
    Ok(record_to_network(&record))
}

pub(crate) fn get_workload_network(
    request: GetWorkloadNetworkRequest,
    state: &StateStore,
) -> io::Result<WorkloadNetwork> {
    let (workload, network) = validate_refs(request.workload.as_ref(), request.network.as_ref())?;
    let record = state
        .load(
            &workload.workload_name,
            &workload.instance_name,
            &network.network_name,
        )?
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "workload network does not exist")
        })?;
    let mut result = record_to_network(&record);
    if !succeeds(IP, &["link", "show", "dev", &record.host_interface])? {
        result.state = NetworkState::Error as i32;
    }
    Ok(result)
}

pub(crate) fn delete_workload_network(
    request: DeleteWorkloadNetworkRequest,
    pools: &IpPoolDirectory,
    state: &StateStore,
) -> io::Result<bool> {
    let (workload, network) = validate_refs(request.workload.as_ref(), request.network.as_ref())?;
    let Some(record) = state.load(
        &workload.workload_name,
        &workload.instance_name,
        &network.network_name,
    )?
    else {
        return Ok(true);
    };
    if succeeds(IP, &["link", "show", "dev", &record.host_interface])? {
        run(IP, &["link", "delete", &record.host_interface])?;
    }
    super::firewall::delete_mappings(&record.ip_address, &record.port_mappings)?;
    let address = record
        .ip_address
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "stored IP address is invalid"))?;
    // Keep the record if IPAM release fails so a retry can recover it.
    pools.pool(record.vlan_id)?.release(address)?;
    state.delete(
        &workload.workload_name,
        &workload.instance_name,
        &network.network_name,
    )?;
    Ok(true)
}

fn validate_add_request(
    request: &AddWorkloadNetworkRequest,
) -> io::Result<(&WorkloadRef, &NetworkRef, u32, &str)> {
    let workload = request
        .workload
        .as_ref()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "workload is required"))?;
    let network = request
        .network
        .as_ref()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "network is required"))?;
    for value in [
        &workload.workload_name,
        &workload.instance_name,
        &network.network_name,
    ] {
        validate_name(value)?;
    }
    if !(1..=255).contains(&network.vlan_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "network vlan_id must be between 1 and 255",
        ));
    }
    let interface = if request.interface_name.is_empty() {
        "eth0"
    } else {
        &request.interface_name
    };
    if interface.len() > 15
        || !interface
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid interface name",
        ));
    }
    let components: Vec<_> = std::path::Path::new(&request.netns_path)
        .components()
        .collect();
    let pid = match components.as_slice() {
        [
            std::path::Component::RootDir,
            std::path::Component::Normal(proc),
            std::path::Component::Normal(pid),
            std::path::Component::Normal(ns),
            std::path::Component::Normal(net),
        ] if *proc == "proc" && *ns == "ns" && *net == "net" => {
            pid.to_string_lossy().parse::<u32>().ok()
        }
        _ => None,
    }
    .filter(|pid| *pid > 0)
    .ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "netns_path must be /proc/<pid>/ns/net",
        )
    })?;
    Ok((workload, network, pid, interface))
}

fn validate_refs<'a>(
    workload: Option<&'a WorkloadRef>,
    network: Option<&'a NetworkRef>,
) -> io::Result<(&'a WorkloadRef, &'a NetworkRef)> {
    let workload = workload
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "workload is required"))?;
    let network = network
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "network is required"))?;
    for value in [
        &workload.workload_name,
        &workload.instance_name,
        &network.network_name,
    ] {
        validate_name(value)?;
    }
    Ok((workload, network))
}

fn validate_name(value: &str) -> io::Result<()> {
    if value.is_empty()
        || value.len() > 63
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid workload or network name",
        ));
    }
    Ok(())
}

fn open_netns(path: &str) -> io::Result<File> {
    let file = File::open(path)?;
    let namespace = file.metadata()?;
    let host = std::fs::metadata("/proc/self/ns/net")?;
    if (namespace.dev(), namespace.ino()) == (host.dev(), host.ino()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "netns_path refers to the host network namespace",
        ));
    }
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, 0) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

fn netns_matches(netns: &File, pid: u32) -> io::Result<bool> {
    let expected = netns.metadata()?;
    let actual = std::fs::metadata(format!("/proc/{pid}/ns/net"))?;
    Ok((expected.dev(), expected.ino()) == (actual.dev(), actual.ino()))
}

fn run_in_namespace(netns: &File, arguments: &[&str]) -> io::Result<()> {
    let target = format!("--net=/proc/self/fd/{}", netns.as_raw_fd());
    let ip = system::resolve(IP)?;
    let ip = ip
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "ip path is not valid UTF-8"))?;
    let mut command_arguments = vec![target.as_str(), "--", ip];
    command_arguments.extend_from_slice(arguments);
    run(NSENTER, &command_arguments)
}

fn record_to_network(record: &WorkloadRecord) -> WorkloadNetwork {
    WorkloadNetwork {
        state: NetworkState::Ready as i32,
        interface_name: record.interface_name.clone(),
        ip_address: record.ip_address.clone(),
        gateway: record.gateway.clone(),
        network_name: record.network_name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(netns_path: &str) -> AddWorkloadNetworkRequest {
        AddWorkloadNetworkRequest {
            workload: Some(WorkloadRef {
                workload_name: "api".into(),
                instance_name: "api-1".into(),
            }),
            network: Some(NetworkRef {
                network_name: "tenant-a".into(),
                vlan_id: 100,
            }),
            netns_path: netns_path.into(),
            interface_name: String::new(),
            port_mappings: Vec::new(),
        }
    }

    #[test]
    fn accepts_a_valid_request() {
        let request = request("/proc/4242/ns/net");

        let (_, _, pid, interface) = validate_add_request(&request).unwrap();

        assert_eq!(pid, 4242);
        assert_eq!(interface, "eth0");
    }

    #[test]
    fn rejects_malformed_netns_paths() {
        for path in [
            "",
            "proc/42/ns/net",
            "/proc/0/ns/net",
            "/proc/self/ns/net",
            "/proc/../proc/1/ns/net",
            "/proc/42/ns/net/extra",
            "/var/run/netns/pod",
        ] {
            assert!(
                validate_add_request(&request(path)).is_err(),
                "{path} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_the_host_network_namespace() {
        assert_eq!(
            open_netns("/proc/self/ns/net").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn rejects_invalid_names() {
        let too_long = "a".repeat(64);
        for name in ["", "a/b", "a b", "a;b", "a$(id)", too_long.as_str()] {
            assert!(validate_name(name).is_err(), "{name} should be rejected");
        }
        assert!(validate_name("api-1.default_x").is_ok());
    }

    #[test]
    fn rejects_vlan_ids_outside_the_bridge_range() {
        for vlan_id in [0, 4095, 65536] {
            let mut request = request("/proc/4242/ns/net");
            request.network.as_mut().unwrap().vlan_id = vlan_id;

            assert!(
                validate_add_request(&request).is_err(),
                "vlan {vlan_id} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_interface_names_the_kernel_cannot_hold() {
        let mut request = request("/proc/4242/ns/net");
        request.interface_name = "e".repeat(16);

        assert!(validate_add_request(&request).is_err());
    }
}
