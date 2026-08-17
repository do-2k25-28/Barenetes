use crate::ip_pool::IpPool;
use std::fs::File;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::state::{StateStore, WorkloadRecord, stable_id};
use proto::cni::v1::{
    AddWorkloadNetworkRequest, DeleteWorkloadNetworkRequest, GetWorkloadNetworkRequest, NetworkRef,
    NetworkState, WorkloadNetwork, WorkloadRef,
};

const IP: &str = "ip";
const BRIDGE: &str = "bridge";
pub const BRIDGE_NAME: &str = "barenetes0";
const BRIDGE_ADDRESS: &str = "10.244.0.1/16";
const GATEWAY: &str = "10.244.0.1";
const NSENTER: &str = "nsenter";

// Never resolved through PATH: the daemon runs as root.
const TOOL_DIRECTORIES: &[&str] = &["/usr/sbin", "/sbin", "/usr/bin", "/bin"];

// Leaves room for the 50 bytes of VXLAN encapsulation.
const DEFAULT_MTU: u32 = 1450;

pub fn add_workload_network(
    request: AddWorkloadNetworkRequest,
    pool: &IpPool,
    state: &StateStore,
) -> io::Result<WorkloadNetwork> {
    let (workload, network, pid, interface) = validate_add_request(&request)?;
    let netns = open_netns(&request.netns_path)?;
    if let Some(record) = state.load(
        &workload.workload_name,
        &workload.instance_name,
        &network.network_name,
    )? {
        if record.interface_name != interface || record.vlan_id != network.vlan_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "workload network already exists with different settings",
            ));
        }
        return Ok(record_to_network(&record));
    }

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
                "route", "replace", "default", "via", GATEWAY, "dev", interface,
            ],
        )?;
        run(
            IP,
            &["link", "set", "dev", &host_interface, "master", BRIDGE_NAME],
        )?;
        let vlan = network.vlan_id.to_string();
        run(
            BRIDGE,
            &["vlan", "add", "dev", BRIDGE_NAME, "vid", &vlan, "self"],
        )?;
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
        gateway: GATEWAY.to_owned(),
        vlan_id: network.vlan_id,
    };
    if let Err(error) = state.save(&record) {
        let _ = run(IP, &["link", "delete", &record.host_interface]);
        let _ = pool.release(address);
        return Err(error);
    }
    Ok(record_to_network(&record))
}

pub fn get_workload_network(
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

pub fn delete_workload_network(
    request: DeleteWorkloadNetworkRequest,
    pool: &IpPool,
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
    let address = record
        .ip_address
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "stored IP address is invalid"))?;
    state.delete(
        &workload.workload_name,
        &workload.instance_name,
        &network.network_name,
    )?;
    if let Err(error) = pool.release(address) {
        eprintln!("cni: failed to release {address}: {error}");
    }
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
    if !(1..=4094).contains(&network.vlan_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "network vlan_id must be between 1 and 4094",
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
    let ip = resolve(IP)?;
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

pub fn ensure_bridge() -> io::Result<()> {
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

pub fn mtu() -> io::Result<u32> {
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

fn resolve(program: &str) -> io::Result<PathBuf> {
    let variable = format!("BARENETES_{}_BIN", program.to_uppercase());
    if let Some(value) = std::env::var_os(&variable) {
        let path = PathBuf::from(value);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{variable} does not point to an existing file"),
            ))
        };
    }
    TOOL_DIRECTORIES
        .iter()
        .map(|directory| Path::new(directory).join(program))
        .find(|path| path.is_file())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "{program} not found in {}; set {variable} to its absolute path",
                    TOOL_DIRECTORIES.join(", ")
                ),
            )
        })
}

pub fn succeeds(program: &str, arguments: &[&str]) -> io::Result<bool> {
    Command::new(resolve(program)?)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
}

pub fn run(program: &str, arguments: &[&str]) -> io::Result<()> {
    if succeeds(program, arguments)? {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "network command failed: {program} {}",
            arguments.join(" ")
        )))
    }
}
