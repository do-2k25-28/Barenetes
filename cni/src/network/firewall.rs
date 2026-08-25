use proto::cni::v1::{PortMapping, PortProtocol};
use std::collections::BTreeSet;
use std::io;

use crate::network::{BRIDGE_NAME, run, succeeds};

const IPTABLES: &str = "iptables";
const SYSCTL: &str = "sysctl";
const FORWARD_CHAIN: &str = "BARENETES-FORWARD";
const PREROUTING_CHAIN: &str = "BARENETES-PREROUTING";

pub(crate) fn ensure_egress() -> io::Result<()> {
    run(SYSCTL, &["-q", "-w", "net.ipv4.ip_forward=1"])?;
    ensure_chain("nat", PREROUTING_CHAIN)?;
    ensure_jump("nat", "PREROUTING", PREROUTING_CHAIN)?;
    ensure_chain("filter", FORWARD_CHAIN)?;
    ensure_jump("filter", "FORWARD", FORWARD_CHAIN)?;
    // Routed traffic between tenant VLAN interfaces must never be allowed.
    // Same-VLAN traffic stays on the bridge and does not need this rule.
    ensure_rule_first(&[
        "-t",
        "filter",
        "-C",
        FORWARD_CHAIN,
        "-i",
        "barenetes0+",
        "-o",
        "barenetes0+",
        "-j",
        "DROP",
    ])?;
    ensure_rule(&[
        "-t",
        "nat",
        "-C",
        "POSTROUTING",
        "-s",
        "10.0.0.0/8",
        "!",
        "-o",
        BRIDGE_NAME,
        "-j",
        "MASQUERADE",
    ])?;
    ensure_rule(&[
        "-t",
        "filter",
        "-C",
        FORWARD_CHAIN,
        "-m",
        "conntrack",
        "--ctstate",
        "ESTABLISHED,RELATED",
        "-j",
        "ACCEPT",
    ])?;
    ensure_rule(&[
        "-t",
        "filter",
        "-C",
        FORWARD_CHAIN,
        "-i",
        BRIDGE_NAME,
        "-j",
        "ACCEPT",
    ])?;
    ensure_rule(&[
        "-t",
        "filter",
        "-C",
        FORWARD_CHAIN,
        "-o",
        BRIDGE_NAME,
        "-j",
        "ACCEPT",
    ])
}

fn ensure_chain(table: &str, chain: &str) -> io::Result<()> {
    if succeeds(IPTABLES, &["-t", table, "-S", chain])? {
        return Ok(());
    }
    run(IPTABLES, &["-t", table, "-N", chain])
}

// Inserted first so that a DROP policy or another tool's rules cannot shadow it.
fn ensure_jump(table: &str, parent: &str, chain: &str) -> io::Result<()> {
    if succeeds(IPTABLES, &["-t", table, "-C", parent, "-j", chain])? {
        return Ok(());
    }
    run(IPTABLES, &["-t", table, "-I", parent, "1", "-j", chain])
}

pub(crate) fn validate_mappings(mappings: &[PortMapping]) -> io::Result<()> {
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

pub(crate) fn add_mappings(address: &str, mappings: &[PortMapping]) -> io::Result<()> {
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

pub(crate) fn delete_mappings(address: &str, mappings: &[PortMapping]) -> io::Result<()> {
    for mapping in mappings {
        delete_mapping(address, mapping)?;
    }
    Ok(())
}

// Scoped to traffic entering the node, so that workload traffic is not caught too.
fn mapping_rule<'a>(protocol: &'a str, host: &'a str, destination: &'a str) -> [&'a str; 15] {
    [
        "-t",
        "nat",
        "-C",
        PREROUTING_CHAIN,
        "!",
        "-i",
        BRIDGE_NAME,
        "-p",
        protocol,
        "--dport",
        host,
        "-j",
        "DNAT",
        "--to-destination",
        destination,
    ]
}

fn add_mapping(address: &str, mapping: &PortMapping) -> io::Result<()> {
    let protocol = protocol(mapping)?;
    let host = mapping.host_port.to_string();
    let destination = format!("{address}:{}", mapping.workload_port);
    ensure_rule(&mapping_rule(protocol, &host, &destination))
}

fn delete_mapping(address: &str, mapping: &PortMapping) -> io::Result<()> {
    let protocol = protocol(mapping)?;
    let host = mapping.host_port.to_string();
    let destination = format!("{address}:{}", mapping.workload_port);
    let check = mapping_rule(protocol, &host, &destination);
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

fn ensure_rule_first(check: &[&str]) -> io::Result<()> {
    if succeeds(IPTABLES, check)? {
        return Ok(());
    }
    let mut add = check.to_vec();
    if let Some(position) = add.iter().position(|argument| *argument == "-C") {
        add[position] = "-I";
        add.insert(position + 2, "1");
    }
    run(IPTABLES, &add)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping(host_port: u32, workload_port: u32, protocol: PortProtocol) -> PortMapping {
        PortMapping {
            host_port,
            workload_port,
            protocol: protocol as i32,
        }
    }

    #[test]
    fn accepts_tcp_and_udp_mappings() {
        let mappings = [
            mapping(8080, 80, PortProtocol::Tcp),
            mapping(5353, 53, PortProtocol::Udp),
        ];
        assert!(validate_mappings(&mappings).is_ok());
    }

    #[test]
    fn rejects_invalid_or_duplicate_mappings() {
        assert!(validate_mappings(&[mapping(0, 80, PortProtocol::Tcp)]).is_err());
        assert!(validate_mappings(&[mapping(80, 0, PortProtocol::Tcp)]).is_err());
        assert!(validate_mappings(&[mapping(80, 80, PortProtocol::Unspecified)]).is_err());
        assert!(
            validate_mappings(&[
                mapping(8080, 80, PortProtocol::Tcp),
                mapping(8080, 81, PortProtocol::Tcp),
            ])
            .is_err()
        );
    }
}
