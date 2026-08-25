use hyper_util::rt::TokioIo;
use proto::cni::v1::cni_service_client::CniServiceClient;
use proto::cni::v1::{
    AddWorkloadNetworkRequest, DeleteWorkloadNetworkRequest, GetWorkloadNetworkRequest, NetworkRef,
    WorkloadRef,
};
use std::env;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Uri};

const SOCKET: &str = "/run/barenetes/cni.sock";

fn refs(instance: &str, network: &str, vlan: u32) -> (WorkloadRef, NetworkRef) {
    (
        WorkloadRef {
            workload_name: "integration".into(),
            instance_name: instance.into(),
        },
        NetworkRef {
            network_name: network.into(),
            vlan_id: vlan,
        },
    )
}

async fn client() -> Result<CniServiceClient<Channel>, Box<dyn std::error::Error>> {
    let endpoint = Endpoint::try_from("http://localhost")?;
    let channel = endpoint
        .connect_with_connector(tower::service_fn(|_: Uri| async {
            UnixStream::connect(SOCKET).await.map(TokioIo::new)
        }))
        .await?;
    Ok(CniServiceClient::new(channel))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let action = args.next().ok_or("action manquante")?;
    let instance = args.next().ok_or("instance manquante")?;
    let network = args.next().ok_or("network manquante")?;
    let vlan: u32 = args.next().ok_or("VLAN manquant")?.parse()?;
    let netns = args.next();
    let (workload, network_ref) = refs(&instance, &network, vlan);
    let mut client = client().await?;

    match action.as_str() {
        "add" => {
            let netns_path = netns.ok_or("netns manquant pour ADD")?;
            client
                .add_workload_network(AddWorkloadNetworkRequest {
                    workload: Some(workload),
                    network: Some(network_ref),
                    netns_path,
                    interface_name: "eth0".into(),
                    port_mappings: Vec::new(),
                })
                .await?;
        }
        "get" => {
            client
                .get_workload_network(GetWorkloadNetworkRequest {
                    workload: Some(workload),
                    network: Some(network_ref),
                })
                .await?;
        }
        "delete" => {
            client
                .delete_workload_network(DeleteWorkloadNetworkRequest {
                    workload: Some(workload),
                    network: Some(network_ref),
                })
                .await?;
        }
        _ => return Err("action attendue: add, get ou delete".into()),
    }
    Ok(())
}
