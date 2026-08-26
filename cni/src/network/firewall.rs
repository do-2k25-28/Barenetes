use proto::shared::v1::{Port, Protocol};
use std::collections::BTreeSet;
use std::io;

use crate::network::{BRIDGE_NAME, run, succeeds};

const IPTABLES: &str = "iptables";
const SYSCTL: &str = "sysctl";
const FORWARD_CHAIN: &str = "BARENETES-FORWARD";
const PREROUTING_CHAIN: &str = "BARENETES-PREROUTING";
const OUTPUT_CHAIN: &str = "BARENETES-OUTPUT";
const TENANT_INTERFACES: &str = "barenetes0.+";

pub(crate) fn ensure_egress() -> io::Result<()> {
    run(SYSCTL, &["-q", "-w", "net.ipv4.ip_forward=1"])?;
    ensure_chain("nat", PREROUTING_CHAIN)?;
    ensure_jump("nat", "PREROUTING", PREROUTING_CHAIN)?;
    ensure_chain("nat", OUTPUT_CHAIN)?;
    ensure_jump("nat", "OUTPUT", OUTPUT_CHAIN)?;
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
        TENANT_INTERFACES,
        "-o",
        TENANT_INTERFACES,
        "-j",
        "DROP",
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
        TENANT_INTERFACES,
        "-j",
        "ACCEPT",
    ])?;
    ensure_rule(&[
        "-t",
        "filter",
        "-C",
        FORWARD_CHAIN,
        "-o",
        TENANT_INTERFACES,
        "-j",
        "ACCEPT",
    ])?;
    // br_netfilter can report same-VLAN bridged traffic as barenetes0 ->
    // barenetes0. Keep it allowed even when the global FORWARD policy is DROP.
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
        let delete = vec!["-t", table, "-D", parent, "-j", chain];
        run(IPTABLES, &delete)?;
    }
    run(IPTABLES, &["-t", table, "-I", parent, "1", "-j", chain])
}

pub(crate) fn ensure_tenant_nat(vlan: u8, node: u8) -> io::Result<()> {
    let source = format!("10.{vlan}.{node}.0/24");
    ensure_rule(&[
        "-t",
        "nat",
        "-C",
        "POSTROUTING",
        "-s",
        &source,
        "!",
        "-o",
        "barenetes0+",
        "-j",
        "MASQUERADE",
    ])
}

pub(crate) fn validate_mappings(mappings: &[Port]) -> io::Result<()> {
    let mut host_ports = BTreeSet::new();
    for mapping in mappings {
        if mapping.external == 0
            || mapping.external > 65535
            || mapping.internal == 0
            || mapping.internal > 65535
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ports must be between 1 and 65535",
            ));
        }
        if Protocol::try_from(mapping.protocol)
            .ok()
            .filter(|protocol| matches!(protocol, Protocol::Tcp | Protocol::Udp))
            .is_none()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "port protocol must be TCP or UDP",
            ));
        }
        if !host_ports.insert((mapping.protocol, mapping.external)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "duplicate host port mapping",
            ));
        }
    }
    Ok(())
}

pub(crate) fn add_mappings(address: &str, mappings: &[Port]) -> io::Result<()> {
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

pub(crate) fn delete_mappings(address: &str, mappings: &[Port]) -> io::Result<()> {
    for mapping in mappings {
        delete_mapping(address, mapping)?;
    }
    Ok(())
}

// PREROUTING handles traffic entering the node; OUTPUT handles traffic
// generated locally, such as curl localhost:<port> on the node itself.
fn mapping_rule(
    chain: &str,
    ingress: bool,
    protocol: &str,
    host: &str,
    destination: &str,
) -> Vec<String> {
    let mut rule = vec!["-t".into(), "nat".into(), "-C".into(), chain.into()];
    if ingress {
        rule.extend(["!", "-i", BRIDGE_NAME].into_iter().map(String::from));
    }
    rule.extend([
        "-p".into(),
        protocol.into(),
        "--dport".into(),
        host.into(),
        "-j".into(),
        "DNAT".into(),
        "--to-destination".into(),
        destination.into(),
    ]);
    rule
}

fn add_mapping(address: &str, mapping: &Port) -> io::Result<()> {
    let protocol = protocol(mapping)?;
    let host = mapping.external.to_string();
    let destination = format!("{address}:{}", mapping.internal);
    ensure_mapping_rule(&mapping_rule(
        PREROUTING_CHAIN,
        true,
        protocol,
        &host,
        &destination,
    ))?;
    ensure_mapping_rule(&mapping_rule(
        OUTPUT_CHAIN,
        false,
        protocol,
        &host,
        &destination,
    ))
}

fn delete_mapping(address: &str, mapping: &Port) -> io::Result<()> {
    let protocol = protocol(mapping)?;
    let host = mapping.external.to_string();
    let destination = format!("{address}:{}", mapping.internal);
    delete_mapping_rule(&mapping_rule(
        PREROUTING_CHAIN,
        true,
        protocol,
        &host,
        &destination,
    ))?;
    delete_mapping_rule(&mapping_rule(
        OUTPUT_CHAIN,
        false,
        protocol,
        &host,
        &destination,
    ))
}

fn protocol(mapping: &Port) -> io::Result<&'static str> {
    match Protocol::try_from(mapping.protocol).ok() {
        Some(Protocol::Tcp) => Ok("tcp"),
        Some(Protocol::Udp) => Ok("udp"),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid port protocol",
        )),
    }
}

fn mapping_rule_refs(rule: &[String]) -> Vec<&str> {
    rule.iter().map(String::as_str).collect()
}

fn ensure_mapping_rule(rule: &[String]) -> io::Result<()> {
    let check = mapping_rule_refs(rule);
    if succeeds(IPTABLES, &check)? {
        return Ok(());
    }
    let mut add = rule.to_vec();
    add[2] = "-A".into();
    let add = mapping_rule_refs(&add);
    run(IPTABLES, &add)
}

fn delete_mapping_rule(rule: &[String]) -> io::Result<()> {
    let check = mapping_rule_refs(rule);
    if !succeeds(IPTABLES, &check)? {
        return Ok(());
    }
    let mut delete = rule.to_vec();
    delete[2] = "-D".into();
    let delete = mapping_rule_refs(&delete);
    run(IPTABLES, &delete)
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

    fn mapping(external: u32, internal: u32, protocol: Protocol) -> Port {
        Port {
            internal,
            external,
            protocol: protocol as i32,
        }
    }

    #[test]
    fn accepts_tcp_and_udp_mappings() {
        let mappings = [
            mapping(8080, 80, Protocol::Tcp),
            mapping(5353, 53, Protocol::Udp),
        ];
        assert!(validate_mappings(&mappings).is_ok());
    }

    #[test]
    fn rejects_invalid_or_duplicate_mappings() {
        assert!(validate_mappings(&[mapping(0, 80, Protocol::Tcp)]).is_err());
        assert!(validate_mappings(&[mapping(80, 0, Protocol::Tcp)]).is_err());
        assert!(
            validate_mappings(&[
                mapping(8080, 80, Protocol::Tcp),
                mapping(8080, 81, Protocol::Tcp),
            ])
            .is_err()
        );
    }
}
