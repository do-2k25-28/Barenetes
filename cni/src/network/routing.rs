use std::io;
use std::net::Ipv4Addr;

use super::system::run;

const IP: &str = "ip";

pub(crate) fn node_id() -> io::Result<u8> {
    let value = std::env::var("BARENETES_NODE_ID").unwrap_or_else(|_| "0".to_owned());
    value.parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "BARENETES_NODE_ID must be between 0 and 255",
        )
    })
}

pub(crate) fn ensure_routes(vlan: u8, local_node: u8) -> io::Result<()> {
    for (remote_node, remote_ip) in remote_nodes()? {
        if remote_node == local_node {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "remote node id must differ from the local node id",
            ));
        }
        let subnet = format!("10.{vlan}.{remote_node}.0/24");
        let remote_ip = remote_ip.to_string();
        run(IP, &["route", "replace", &subnet, "via", &remote_ip])?;
    }
    Ok(())
}

fn remote_nodes() -> io::Result<Vec<(u8, Ipv4Addr)>> {
    let ips = std::env::var("BARENETES_REMOTE_NODE_IPS")
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .split(',')
                .map(|item| {
                    item.parse().map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidInput, "invalid remote node IPv4")
                    })
                })
                .collect::<io::Result<Vec<Ipv4Addr>>>()
        })
        .transpose()?
        .unwrap_or_default();
    if ips.is_empty() {
        return Ok(Vec::new());
    }

    let ids = std::env::var("BARENETES_REMOTE_NODE_IDS")
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "BARENETES_REMOTE_NODE_IDS is required with remote nodes",
            )
        })?
        .split(',')
        .map(|item| {
            item.parse::<u8>().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid remote node id")
            })
        })
        .collect::<io::Result<Vec<u8>>>()?;

    if ids.len() != ips.len() || ids.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "remote node ids and IPs must have the same length and ids must be between 1 and 255",
        ));
    }
    if ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "remote node ids must be unique",
        ));
    }
    Ok(ids.into_iter().zip(ips).collect())
}
