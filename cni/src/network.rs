use crate::ip_pool::IpPool;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::state::{StateStore, WorkloadRecord, stable_id};
use proto::cni::v1::{AddWorkloadNetworkRequest, NetworkRef, WorkloadNetwork, WorkloadRef};

const IP: &str = "ip";
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
    if let Some(record) = state.load(
        &workload.workload_name,
        &workload.instance_name,
        &network.network_name,
    )? {
        if record.interface_name != interface {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "workload network already exists with a different interface",
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

    let setup = (|| {
        run_system(
            IP,
            &[
                "link",
                "add",
                &host_interface,
                "type",
                "veth",
                "peer",
                "name",
                &peer_interface,
            ],
        )?;
        run_system(IP, &["link", "set", &peer_interface, "netns", &pid_string])?;
        run_in_namespace(
            pid,
            &["link", "set", "dev", &peer_interface, "name", interface],
        )?;
        run_in_namespace(
            pid,
            &["address", "replace", &address_with_prefix, "dev", interface],
        )?;
        run_in_namespace(pid, &["link", "set", "dev", interface, "up"])?;
        run_in_namespace(pid, &["link", "set", "dev", "lo", "up"])?;
        run_in_namespace(
            pid,
            &[
                "route", "replace", "default", "via", GATEWAY, "dev", interface,
            ],
        )?;
        run_system(
            IP,
            &["link", "set", "dev", &host_interface, "master", BRIDGE_NAME],
        )?;
        run_system(IP, &["link", "set", "dev", &host_interface, "up"])
    })();

    if let Err(error) = setup {
        let _ = run_system(IP, &["link", "delete", &host_interface]);
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
    };
    if let Err(error) = state.save(&record) {
        let _ = run_system(IP, &["link", "delete", &record.host_interface]);
        let _ = pool.release(address);
        return Err(error);
    }
    Ok(record_to_network(&record))
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

fn run_system(program: &str, arguments: &[&str]) -> io::Result<()> {
    run(program, arguments)
}

fn run_in_namespace(pid: u32, arguments: &[&str]) -> io::Result<()> {
    let target = format!("--net=/proc/{pid}/ns/net");
    let ip = resolve(IP)?;
    let ip = ip.to_string_lossy();
    let mut command_arguments = vec![target.as_str(), "--", ip.as_ref()];
    command_arguments.extend_from_slice(arguments);
    run_system(NSENTER, &command_arguments)
}

fn record_to_network(record: &WorkloadRecord) -> WorkloadNetwork {
    WorkloadNetwork {
        state: proto::cni::v1::NetworkState::Ready as i32,
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
    run(IP, &["link", "set", "dev", BRIDGE_NAME, "up"])
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
