use proto::api::v1::api_server_client::ApiServerClient;
use proto::api::v1::{CreatePodRequest, GetPodRequest};
use proto::shared::v1::{
    Container, Pod, PodDetail, PodSpec, PodStatus, PodWithSpec, Protocol, Resources,
};

use crate::cli::{CreatePodArgs, GetPodArgs};
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

pub async fn get_pod(server: &str, args: GetPodArgs) -> Result<(), CliError> {
    let mut client = ApiServerClient::connect(server.to_string())
        .await
        .map_err(|source| CliError::Connect {
            addr: server.to_string(),
            source,
        })?;

    let response = client
        .get_pod(GetPodRequest {
            name: args.name,
            namespace: args.namespace,
        })
        .await?;

    if let Some(pod) = response.into_inner().pod {
        print_pod(&pod);
    }

    Ok(())
}

fn print_pod(pod: &PodDetail) {
    let core = pod.core.as_ref();
    let name = core
        .and_then(|c| c.pod.as_ref())
        .map(|p| p.name.as_str())
        .unwrap_or_default();
    let namespace = core
        .and_then(|c| c.spec.as_ref())
        .map(|s| s.namespace.as_str())
        .unwrap_or_default();
    let status = core
        .and_then(|c| c.pod.as_ref())
        .map(|p| PodStatus::try_from(p.status).unwrap_or(PodStatus::Unknown))
        .unwrap_or(PodStatus::Unknown);
    let pod_ip = pod
        .pod_ip
        .as_deref()
        .filter(|ip| !ip.is_empty())
        .unwrap_or("<none>");

    println!("Name:        {name}");
    println!("Namespace:   {namespace}");
    println!("Status:      {status:?}");
    println!("Node:        {}", or_none(&pod.node_name));
    println!("Pod IP:      {pod_ip}");

    let requests = core
        .and_then(|c| c.pod.as_ref())
        .and_then(|p| p.requests.as_ref());
    let limits = core
        .and_then(|c| c.pod.as_ref())
        .and_then(|p| p.limits.as_ref());
    if requests.is_some() || limits.is_some() {
        println!();
        if let Some(requests) = requests {
            println!(
                "Requests:    cpu={}m, memory={}Mi",
                requests.cpu, requests.memory
            );
        }
        if let Some(limits) = limits {
            println!(
                "Limits:      cpu={}m, memory={}Mi",
                limits.cpu, limits.memory
            );
        }
    }

    println!();
    println!("Containers:");
    if let Some(spec) = core.and_then(|c| c.spec.as_ref()) {
        for container in &spec.containers {
            println!("  - {} ({})", container.name, container.image);
            if !container.ports.is_empty() {
                let ports = container
                    .ports
                    .iter()
                    .map(|p| {
                        format!(
                            "{}->{}/{}",
                            p.external,
                            p.internal,
                            protocol_str(p.protocol)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("      Ports: {ports}");
            }
            if !container.env.is_empty() {
                let env = container
                    .env
                    .iter()
                    .map(|e| format!("{}={}", e.name, e.value))
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("      Env:   {env}");
            }
        }
    }
}

fn or_none(value: &str) -> &str {
    if value.is_empty() { "<none>" } else { value }
}

fn protocol_str(protocol: i32) -> &'static str {
    match Protocol::try_from(protocol) {
        Ok(Protocol::Tcp) => "tcp",
        Ok(Protocol::Udp) => "udp",
        Err(_) => "unknown",
    }
}
