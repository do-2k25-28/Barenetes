use proto::api::v1::CreatePodRequest;
use proto::api::v1::api_server_client::ApiServerClient;
use proto::shared::v1::{Container, Pod, PodSpec, PodStatus, PodWithSpec, Resources};

use crate::cli::CreatePodArgs;
use crate::error::CliError;

pub async fn create_pod(server: &str, args: CreatePodArgs) -> Result<(), CliError> {
    let pod = PodWithSpec {
        pod: Some(Pod {
            name: args.name.clone(),
            status: PodStatus::Pending as i32,
            requests: resources(args.cpu_request, args.memory_request),
            limits: resources(args.cpu_limit, args.memory_limit),
        }),
        spec: Some(PodSpec {
            namespace: args.namespace.clone(),
            containers: vec![Container {
                name: args.name.clone(),
                image: args.image,
                ports: args.ports,
                env: args.env,
            }],
        }),
    };

    let mut client = ApiServerClient::connect(server.to_string())
        .await
        .map_err(|source| CliError::Connect {
            addr: server.to_string(),
            source,
        })?;
    client
        .create_pod(CreatePodRequest { pod: Some(pod) })
        .await?;

    println!(
        "pod/{} created in namespace \"{}\"",
        args.name, args.namespace
    );
    Ok(())
}

fn resources(cpu: Option<i32>, memory: Option<i32>) -> Option<Resources> {
    if cpu.is_none() && memory.is_none() {
        return None;
    }
    Some(Resources {
        cpu: cpu.unwrap_or_default(),
        memory: memory.unwrap_or_default(),
    })
}
