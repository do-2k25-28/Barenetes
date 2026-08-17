use super::{bind_socket, firewall, ip_pool::IpPool, remove_socket, state};
use proto::cni::v1::{PortMapping, PortProtocol};
use std::io;
use std::net::Ipv4Addr;
use std::os::unix::fs::PermissionsExt;

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
    assert!(firewall::validate_mappings(&mappings).is_ok());
}

#[test]
fn rejects_invalid_or_duplicate_mappings() {
    assert!(firewall::validate_mappings(&[mapping(0, 80, PortProtocol::Tcp)]).is_err());
    assert!(firewall::validate_mappings(&[mapping(80, 0, PortProtocol::Tcp)]).is_err());
    assert!(firewall::validate_mappings(&[mapping(80, 80, PortProtocol::Unspecified)]).is_err());
    assert!(
        firewall::validate_mappings(&[
            mapping(8080, 80, PortProtocol::Tcp),
            mapping(8080, 81, PortProtocol::Tcp),
        ])
        .is_err()
    );
}

#[test]
fn allocates_persists_and_releases_addresses() {
    let directory = tempfile::tempdir().unwrap();
    let first = Ipv4Addr::new(10, 0, 0, 2);
    let last = Ipv4Addr::new(10, 0, 0, 3);
    let pool = IpPool::new(directory.path(), first, last).unwrap();
    assert_eq!(pool.allocate().unwrap(), first);
    assert_eq!(pool.allocate().unwrap(), last);
    assert!(pool.allocate().is_err());
    assert!(pool.release(first).unwrap());
    assert_eq!(
        IpPool::new(directory.path(), first, last)
            .unwrap()
            .allocate()
            .unwrap(),
        first
    );
}

#[test]
fn fails_closed_on_corrupt_state() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("ip-pool.json"), b"invalid").unwrap();
    let pool = IpPool::new(
        directory.path(),
        Ipv4Addr::new(10, 0, 0, 2),
        Ipv4Addr::new(10, 0, 0, 3),
    )
    .unwrap();
    assert_eq!(
        pool.allocate().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn does_not_remove_a_regular_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cni.sock");
    std::fs::write(&path, b"keep").unwrap();
    assert_eq!(
        remove_socket(&path).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(std::fs::read(path).unwrap(), b"keep");
}

#[tokio::test]
async fn creates_a_restricted_unix_socket() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("run").join("cni.sock");
    let listener = bind_socket(&path).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o660);
    drop(listener);
    remove_socket(&path).unwrap();
}

fn record() -> state::WorkloadRecord {
    state::WorkloadRecord {
        workload_name: "api".into(),
        instance_name: "api-1".into(),
        network_name: "tenant-a".into(),
        host_interface: "v123".into(),
        interface_name: "eth0".into(),
        ip_address: "10.244.0.2".into(),
        gateway: "10.244.0.1".into(),
        vlan_id: 42,
        port_mappings: vec![mapping(8080, 80, PortProtocol::Tcp)],
    }
}

#[test]
fn saves_loads_and_deletes_a_record() {
    let directory = tempfile::tempdir().unwrap();
    let store = state::StateStore::new(directory.path());
    let expected = record();
    store.save(&expected).unwrap();
    let loaded = store.load("api", "api-1", "tenant-a").unwrap().unwrap();
    assert_eq!(loaded.ip_address, expected.ip_address);
    assert_eq!(loaded.vlan_id, expected.vlan_id);
    assert!(store.port_is_used(PortProtocol::Tcp as i32, 8080).unwrap());
    assert!(store.delete("api", "api-1", "tenant-a").unwrap());
    assert!(store.load("api", "api-1", "tenant-a").unwrap().is_none());
}

#[test]
fn stable_ids_include_part_boundaries() {
    assert_ne!(
        state::stable_id(&["ab", "c"]),
        state::stable_id(&["a", "bc"])
    );
}
