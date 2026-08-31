use hyper_util::rt::TokioIo;
use proto::cni::v1::cni_service_client::CniServiceClient;
use proto::cni::v1::{
    AddWorkloadNetworkRequest, DeleteWorkloadNetworkRequest, GetWorkloadNetworkRequest, NetworkRef,
    WorkloadRef,
};
use proto::shared::v1::{Port, Protocol};
use std::env;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Uri};

const DEFAULT_SOCKET: &str = "/run/barenetes/cni.sock";

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
    let socket = env::var("BARENETES_CNI_SOCKET").unwrap_or_else(|_| DEFAULT_SOCKET.to_owned());
    let channel = endpoint
        .connect_with_connector(tower::service_fn(move |_: Uri| {
            let socket = socket.clone();
            async move { UnixStream::connect(socket).await.map(TokioIo::new) }
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
            let port_mappings = args
                .next()
                .map(|spec| parse_port_mapping(&spec))
                .transpose()?
                .into_iter()
                .collect();
            client
                .add_workload_network(AddWorkloadNetworkRequest {
                    workload: Some(workload),
                    network: Some(network_ref),
                    netns_path,
                    interface_name: "eth0".into(),
                    port_mappings,
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

// Optionnel sur ADD : "<port_externe>:<port_interne>", toujours TCP.
fn parse_port_mapping(spec: &str) -> Result<Port, Box<dyn std::error::Error>> {
    let (external, internal) = spec
        .split_once(':')
        .ok_or("format attendu pour le port mapping: <port_externe>:<port_interne>")?;
    Ok(Port {
        external: external.parse()?,
        internal: internal.parse()?,
        protocol: Protocol::Tcp as i32,
    })
}
