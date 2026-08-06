use std::io;
use std::net::IpAddr;

const IP: &str = "/usr/sbin/ip";
const BRIDGE: &str = "/usr/sbin/bridge";
const VXLAN: &str = "barenetes-vx";

pub fn ensure_overlay() -> io::Result<()> {
    let remote_nodes = remote_nodes()?;
    let Some(local_ip) = parse_optional_ip("BARENETES_NODE_IP")? else {
        return if remote_nodes.is_empty() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "BARENETES_NODE_IP is required with remote nodes",
            ))
        };
    };
    if !crate::network::succeeds(IP, &["link", "show", "dev", VXLAN])? {
        crate::network::run(
            IP,
            &[
                "link",
                "add",
                VXLAN,
                "type",
                "vxlan",
                "id",
                "42",
                "local",
                &local_ip.to_string(),
                "dstport",
                "4789",
                "nolearning",
            ],
        )?;
    }
    crate::network::run(
        IP,
        &[
            "link",
            "set",
            "dev",
            VXLAN,
            "master",
            crate::network::BRIDGE_NAME,
        ],
    )?;
    crate::network::run(IP, &["link", "set", "dev", VXLAN, "up"])?;
    for remote in remote_nodes {
        crate::network::run(
            BRIDGE,
            &[
                "fdb",
                "replace",
                "00:00:00:00:00:00",
                "dev",
                VXLAN,
                "dst",
                &remote.to_string(),
            ],
        )?;
    }
    Ok(())
}

pub fn node_id() -> io::Result<u8> {
    std::env::var("BARENETES_NODE_ID")
        .ok()
        .map(|value| {
            value.parse().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "BARENETES_NODE_ID must be between 0 and 255",
                )
            })
        })
        .transpose()
        .map(|value| value.unwrap_or(0))
}

fn remote_nodes() -> io::Result<Vec<IpAddr>> {
    std::env::var("BARENETES_REMOTE_NODE_IPS")
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .split(',')
                .map(|item| {
                    item.parse().map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidInput, "invalid remote node IP")
                    })
                })
                .collect()
        })
        .transpose()
        .map(|value| value.unwrap_or_default())
}

fn parse_optional_ip(name: &str) -> io::Result<Option<IpAddr>> {
    std::env::var(name)
        .ok()
        .map(|value| {
            value
                .parse()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, format!("invalid {name}")))
        })
        .transpose()
}
