use cni::firewall::validate_mappings;
use proto::cni::v1::{PortMapping, PortProtocol};

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
