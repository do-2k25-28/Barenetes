use cni::state::{StateStore, WorkloadRecord, stable_id};
use proto::cni::v1::{PortMapping, PortProtocol};

fn record() -> WorkloadRecord {
    WorkloadRecord {
        workload_name: "api".into(),
        instance_name: "api-1".into(),
        network_name: "tenant-a".into(),
        host_interface: "v123".into(),
        interface_name: "eth0".into(),
        ip_address: "10.244.0.2".into(),
        gateway: "10.244.0.1".into(),
        vlan_id: 42,
        port_mappings: vec![PortMapping {
            host_port: 8080,
            workload_port: 80,
            protocol: PortProtocol::Tcp as i32,
        }],
    }
}

#[test]
fn saves_loads_and_deletes_a_record() {
    let directory = tempfile::tempdir().unwrap();
    let store = StateStore::new(directory.path());
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
    assert_ne!(stable_id(&["ab", "c"]), stable_id(&["a", "bc"]));
}
