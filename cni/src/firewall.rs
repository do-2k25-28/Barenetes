use proto::cni::v1::{PortMapping, PortProtocol};
use std::collections::BTreeSet;
use std::io;

use crate::network::{run, succeeds};

const IPTABLES: &str = "/usr/sbin/iptables";
const SYSCTL: &str = "/usr/sbin/sysctl";

pub fn ensure_egress() -> io::Result<()> {
    run(SYSCTL, &["-q", "-w", "net.ipv4.ip_forward=1"])?;
    ensure_rule(&[
        "-t",
        "nat",
        "-C",
        "POSTROUTING",
        "-s",
        "10.244.0.0/16",
        "!",
        "-o",
        crate::network::BRIDGE_NAME,
        "-j",
        "MASQUERADE",
    ])
}

pub fn validate_mappings(mappings: &[PortMapping]) -> io::Result<()> {
    let mut host_ports = BTreeSet::new();
    for mapping in mappings {
        if mapping.host_port == 0
            || mapping.host_port > 65535
            || mapping.workload_port == 0
            || mapping.workload_port > 65535
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ports must be between 1 and 65535",
            ));
        }
        if PortProtocol::try_from(mapping.protocol)
            .ok()
            .filter(|protocol| matches!(protocol, PortProtocol::Tcp | PortProtocol::Udp))
            .is_none()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "port protocol must be TCP or UDP",
            ));
        }
        if !host_ports.insert((mapping.protocol, mapping.host_port)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "duplicate host port mapping",
            ));
        }
    }
    Ok(())
}

pub fn add_mappings(address: &str, mappings: &[PortMapping]) -> io::Result<()> {
    validate_mappings(mappings)?;
    let mut installed = Vec::new();
    for mapping in mappings {
        if let Err(error) = add_mapping(address, mapping) {
            for mapping in installed {
                let _ = delete_mapping(address, mapping);
            }
            return Err(error);
        }
        installed.push(mapping);
    }
    Ok(())
}

pub fn delete_mappings(address: &str, mappings: &[PortMapping]) -> io::Result<()> {
    for mapping in mappings {
        delete_mapping(address, mapping)?;
    }
    Ok(())
}

fn add_mapping(address: &str, mapping: &PortMapping) -> io::Result<()> {
    let protocol = protocol(mapping)?;
    let host = mapping.host_port.to_string();
    let destination = format!("{address}:{}", mapping.workload_port);
    ensure_rule(&[
        "-t",
        "nat",
        "-C",
        "PREROUTING",
        "-p",
        protocol,
        "--dport",
        &host,
        "-j",
        "DNAT",
        "--to-destination",
        &destination,
    ])
}

fn delete_mapping(address: &str, mapping: &PortMapping) -> io::Result<()> {
    let protocol = protocol(mapping)?;
    let host = mapping.host_port.to_string();
    let destination = format!("{address}:{}", mapping.workload_port);
    let check = [
        "-t",
        "nat",
        "-C",
        "PREROUTING",
        "-p",
        protocol,
        "--dport",
        &host,
        "-j",
        "DNAT",
        "--to-destination",
        &destination,
    ];
    if !succeeds(IPTABLES, &check)? {
        return Ok(());
    }
    let mut delete = check.to_vec();
    delete[2] = "-D";
    run(IPTABLES, &delete)
}

fn protocol(mapping: &PortMapping) -> io::Result<&'static str> {
    match PortProtocol::try_from(mapping.protocol).ok() {
        Some(PortProtocol::Tcp) => Ok("tcp"),
        Some(PortProtocol::Udp) => Ok("udp"),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid port protocol",
        )),
    }
}

fn ensure_rule(check: &[&str]) -> io::Result<()> {
    if succeeds(IPTABLES, check)? {
        return Ok(());
    }
    let mut add = check.to_vec();
    if let Some(position) = add.iter().position(|argument| *argument == "-C") {
        add[position] = "-A";
    }
    run(IPTABLES, &add)
}
