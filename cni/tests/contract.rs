use proto::cni::v1::{
    AddWorkloadNetworkRequest, DeleteWorkloadNetworkRequest, GetWorkloadNetworkRequest, NetworkRef,
    WorkloadRef,
};

#[test]
fn workload_lifecycle_uses_the_public_cni_contract() {
    let workload = WorkloadRef {
        workload_name: "api".into(),
        instance_name: "api-1".into(),
    };
    let network = NetworkRef {
        network_name: "tenant-a".into(),
        vlan_id: 42,
    };

    let add = AddWorkloadNetworkRequest {
        workload: Some(workload.clone()),
        network: Some(network.clone()),
        netns_path: "/proc/4242/ns/net".into(),
        interface_name: "eth0".into(),
        port_mappings: Vec::new(),
    };
    let get = GetWorkloadNetworkRequest {
        workload: Some(workload.clone()),
        network: Some(network.clone()),
    };
    let delete = DeleteWorkloadNetworkRequest {
        workload: Some(workload),
        network: Some(network),
    };

    assert_eq!(add.netns_path, "/proc/4242/ns/net");
    assert_eq!(get.workload.as_ref().unwrap().instance_name, "api-1");
    assert_eq!(delete.network.as_ref().unwrap().vlan_id, 42);
}
