use std::io;

use super::bridge::BRIDGE_NAME;
use super::system::{run, succeeds};

const IPTABLES: &str = "iptables";
const SYSCTL: &str = "sysctl";
const FORWARD_CHAIN: &str = "BARENETES-FORWARD";
pub(crate) const PREROUTING_CHAIN: &str = "BARENETES-PREROUTING";
pub(crate) const OUTPUT_CHAIN: &str = "BARENETES-OUTPUT";
const TENANT_INTERFACES: &str = "barenetes0.+";

pub(crate) fn ensure_egress() -> io::Result<()> {
    run(SYSCTL, &["-q", "-w", "net.ipv4.ip_forward=1"])?;
    ensure_chain("nat", PREROUTING_CHAIN)?;
    ensure_jump("nat", "PREROUTING", PREROUTING_CHAIN, &[])?;
    ensure_chain("nat", OUTPUT_CHAIN)?;
    ensure_jump(
        "nat",
        "OUTPUT",
        OUTPUT_CHAIN,
        &["-m", "addrtype", "--dst-type", "LOCAL"],
    )?;
    ensure_chain("filter", FORWARD_CHAIN)?;
    ensure_jump("filter", "FORWARD", FORWARD_CHAIN, &[])?;
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
fn ensure_jump(table: &str, parent: &str, chain: &str, match_args: &[&str]) -> io::Result<()> {
    let mut scoped_check = vec!["-t", table, "-C", parent];
    scoped_check.extend_from_slice(match_args);
    scoped_check.extend(["-j", chain]);
    if succeeds(IPTABLES, &scoped_check)? {
        let mut delete = scoped_check.clone();
        delete[2] = "-D";
        run(IPTABLES, &delete)?;
    }
    // Remove the pre-existing unscoped OUTPUT jump during upgrade.
    if !match_args.is_empty() && succeeds(IPTABLES, &["-t", table, "-C", parent, "-j", chain])? {
        run(IPTABLES, &["-t", table, "-D", parent, "-j", chain])?;
    }
    let mut insert = vec!["-t", table, "-I", parent, "1"];
    insert.extend_from_slice(match_args);
    insert.extend(["-j", chain]);
    run(IPTABLES, &insert)
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

#[cfg(test)]
mod tests {}
