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

pub(crate) fn validate_configuration(local_node: u8) -> io::Result<()> {
    for (remote_node, _) in remote_nodes()? {
        if remote_node == local_node {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "remote node id must differ from the local node id",
            ));
        }
    }
    Ok(())
}

pub(crate) fn ensure_routes(vlan: u8) -> io::Result<()> {
    for (remote_node, remote_ip) in remote_nodes()? {
        let subnet = format!("10.{vlan}.{remote_node}.0/24");
        let remote_ip = remote_ip.to_string();
        run(IP, &["route", "replace", &subnet, "via", &remote_ip])?;
    }
    Ok(())
}

fn remote_nodes() -> io::Result<Vec<(u8, Ipv4Addr)>> {
    let nodes = std::env::var("BARENETES_REMOTE_NODES")
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .split(',')
                .map(|item| {
                    let (id, ip) = item.split_once('@').ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "remote node must be ID@IPv4")
                    })?;
                    let id = id.parse::<u8>().map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidInput, "invalid remote node id")
                    })?;
                    let ip = ip.parse::<Ipv4Addr>().map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidInput, "invalid remote node IPv4")
                    })?;
                    if id == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "remote node id must be between 1 and 255",
                        ));
                    }
                    Ok((id, ip))
                })
                .collect::<io::Result<Vec<(u8, Ipv4Addr)>>>()
        })
        .transpose()?
        .unwrap_or_default();
    if nodes.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "remote node ids must be unique",
        ));
    }
    Ok(nodes)
}
