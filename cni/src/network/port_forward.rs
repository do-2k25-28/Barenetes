use proto::shared::v1::{Port, Protocol};
use std::collections::BTreeSet;
use std::io;

use super::bridge::BRIDGE_NAME;
use super::firewall::{OUTPUT_CHAIN, PREROUTING_CHAIN};
use super::system::{run, succeeds};

const IPTABLES: &str = "iptables";

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
        if !matches!(
            Protocol::try_from(mapping.protocol).ok(),
            Some(Protocol::Tcp | Protocol::Udp)
        ) {
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
    run(IPTABLES, &mapping_rule_refs(&add))
}

fn delete_mapping_rule(rule: &[String]) -> io::Result<()> {
    let check = mapping_rule_refs(rule);
    if !succeeds(IPTABLES, &check)? {
        return Ok(());
    }
    let mut delete = rule.to_vec();
    delete[2] = "-D".into();
    run(IPTABLES, &mapping_rule_refs(&delete))
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
        assert!(
            validate_mappings(&[
                mapping(8080, 80, Protocol::Tcp),
                mapping(5353, 53, Protocol::Udp)
            ])
            .is_ok()
        );
    }

    #[test]
    fn rejects_invalid_or_duplicate_mappings() {
        assert!(validate_mappings(&[mapping(0, 80, Protocol::Tcp)]).is_err());
        assert!(validate_mappings(&[mapping(80, 0, Protocol::Tcp)]).is_err());
        assert!(
            validate_mappings(&[
                mapping(8080, 80, Protocol::Tcp),
                mapping(8080, 81, Protocol::Tcp)
            ])
            .is_err()
        );
    }
}
