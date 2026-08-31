use proto::api::v1::api_server_client::ApiServerClient;
use proto::api::v1::{
    CreatePodRequest, DeletePodRequest, GetNodeRequest, GetPodRequest, ListNodesRequest,
    ListPodsRequest,
};
use proto::shared::v1::{
    Container, Node, NodeStatus, Pod, PodDetail, PodSpec, PodStatus, PodWithSpec, Protocol,
    Resources,
};

use crate::cli::{
    CreatePodArgs, DeletePodArgs, GetNodeArgs, GetPodArgs, ListNodesArgs, ListPodsArgs,
};
use crate::error::CliError;
use crate::manifest::PodManifest;

pub async fn create_pod(server: &str, args: CreatePodArgs) -> Result<(), CliError> {
    let pod = build_pod(args)?;
    let name = pod.pod.as_ref().map(|p| p.name.clone()).unwrap_or_default();
    let namespace = pod
        .spec
        .as_ref()
        .map(|s| s.namespace.clone())
        .unwrap_or_default();

    let mut client = ApiServerClient::connect(server.to_string())
        .await
        .map_err(|source| CliError::Connect {
            addr: server.to_string(),
            source,
        })?;
    client
        .create_pod(CreatePodRequest { pod: Some(pod) })
        .await?;

    println!("pod/{name} created in namespace \"{namespace}\"");
    Ok(())
}

/// Builds the pod either from `--file` (a YAML manifest) or from the flat
/// flags — the two are mutually exclusive, see `CreatePodArgs`.
fn build_pod(args: CreatePodArgs) -> Result<PodWithSpec, CliError> {
    let flags_used = args.name.is_some()
        || args.namespace.is_some()
        || args.image.is_some()
        || !args.ports.is_empty()
        || !args.env.is_empty()
        || args.cpu_request.is_some()
        || args.memory_request.is_some()
        || args.cpu_limit.is_some()
        || args.memory_limit.is_some();

    if let Some(path) = args.file {
        if flags_used {
            return Err(CliError::InvalidUsage(
                "--file cannot be combined with --name/--namespace/--image/--port/--env/resource flags".to_string(),
            ));
        }
        return PodManifest::from_file(&path)?.try_into();
    }

    let name = args.name.ok_or_else(|| {
        CliError::InvalidUsage("--name is required unless --file is used".to_string())
    })?;
    let image = args.image.ok_or_else(|| {
        CliError::InvalidUsage("--image is required unless --file is used".to_string())
    })?;
    let namespace = args.namespace.unwrap_or_else(|| "default".to_string());

    Ok(PodWithSpec {
        pod: Some(Pod {
            name: name.clone(),
            status: PodStatus::Pending as i32,
            requests: resources(args.cpu_request, args.memory_request),
            limits: resources(args.cpu_limit, args.memory_limit),
        }),
        spec: Some(PodSpec {
            namespace,
            containers: vec![Container {
                name,
                image,
                ports: args.ports,
                env: args.env,
            }],
        }),
    })
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

    match response.into_inner().pod {
        Some(pod) => print_pod(&pod),
        None => return Err(CliError::EmptyResponse),
    }

    Ok(())
}

fn pod_name(pod: &PodDetail) -> &str {
    pod.core
        .as_ref()
        .and_then(|c| c.pod.as_ref())
        .map(|p| p.name.as_str())
        .unwrap_or_default()
}

fn pod_namespace(pod: &PodDetail) -> &str {
    pod.core
        .as_ref()
        .and_then(|c| c.spec.as_ref())
        .map(|s| s.namespace.as_str())
        .unwrap_or_default()
}

fn pod_status(pod: &PodDetail) -> PodStatus {
    pod.core
        .as_ref()
        .and_then(|c| c.pod.as_ref())
        .map(|p| PodStatus::try_from(p.status).unwrap_or(PodStatus::Unknown))
        .unwrap_or(PodStatus::Unknown)
}

fn pod_containers(pod: &PodDetail) -> &[Container] {
    pod.core
        .as_ref()
        .and_then(|c| c.spec.as_ref())
        .map(|s| s.containers.as_slice())
        .unwrap_or_default()
}

pub async fn list_pods(server: &str, args: ListPodsArgs) -> Result<(), CliError> {
    let mut client = ApiServerClient::connect(server.to_string())
        .await
        .map_err(|source| CliError::Connect {
            addr: server.to_string(),
            source,
        })?;

    let response = client.list_pods(ListPodsRequest {}).await?;
    let mut pods = response.into_inner().pods;

    if pods.is_empty() {
        println!("No pods found.");
        return Ok(());
    }

    pods.retain(|pod| matches_filters(pod, &args));

    if pods.is_empty() {
        println!("No pods match the given filters.");
        return Ok(());
    }

    pods.sort_by(|a, b| (pod_namespace(a), pod_name(a)).cmp(&(pod_namespace(b), pod_name(b))));

    println!(
        "{:<15} {:<12} {:<10} {:<15} IMAGE",
        "NAME", "NAMESPACE", "STATUS", "NODE"
    );
    for pod in &pods {
        let images = pod_containers(pod)
            .iter()
            .map(|c| c.image.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let status = format!("{:?}", pod_status(pod));
        println!(
            "{:<15} {:<12} {:<10} {:<15} {}",
            pod_name(pod),
            pod_namespace(pod),
            status,
            or_none(&pod.node_name),
            images
        );
    }

    Ok(())
}

pub async fn get_node(server: &str, args: GetNodeArgs) -> Result<(), CliError> {
    let mut client = ApiServerClient::connect(server.to_string())
        .await
        .map_err(|source| CliError::Connect {
            addr: server.to_string(),
            source,
        })?;

    let response = client
        .get_node(GetNodeRequest {
            name: args.name.clone(),
        })
        .await?;

    match response.into_inner().node {
        Some(node) => print_node(&node),
        None => return Err(CliError::EmptyResponse),
    }

    Ok(())
}

fn print_node(node: &Node) {
    let status = NodeStatus::try_from(node.status).unwrap_or(NodeStatus::NotReady);
    println!("Name:        {}", node.name);
    println!("Status:      {:?}", status);
    if let Some(cap) = &node.capacity {
        println!("Capacity:    cpu={}m, memory={}Mi", cap.cpu, cap.memory);
    }
    if let Some(alloc) = &node.allocatable {
        println!("Allocatable: cpu={}m, memory={}Mi", alloc.cpu, alloc.memory);
    }
}

pub async fn list_nodes(server: &str, _args: ListNodesArgs) -> Result<(), CliError> {
    let mut client = ApiServerClient::connect(server.to_string())
        .await
        .map_err(|source| CliError::Connect {
            addr: server.to_string(),
            source,
        })?;

    let nodes = client
        .list_nodes(ListNodesRequest {})
        .await?
        .into_inner()
        .nodes;

    if nodes.is_empty() {
        println!("No nodes found.");
        return Ok(());
    }

    let mut nodes = nodes;
    nodes.sort_by(|a, b| a.name.cmp(&b.name));

    println!(
        "{:<20} {:<12} {:<12} {:<12}",
        "NAME", "STATUS", "CPU", "MEMORY"
    );
    for node in &nodes {
        let status = NodeStatus::try_from(node.status).unwrap_or(NodeStatus::NotReady);
        let cpu = node.capacity.as_ref().map_or(0, |r| r.cpu);
        let mem = node.capacity.as_ref().map_or(0, |r| r.memory);
        println!(
            "{:<20} {:<12} {:<12} {:<12}",
            node.name,
            format!("{:?}", status),
            format!("{}m", cpu),
            format!("{}Mi", mem),
        );
    }

    Ok(())
}

fn matches_filters(pod: &PodDetail, args: &ListPodsArgs) -> bool {
    if let Some(name) = &args.name
        && pod_name(pod) != name
    {
        return false;
    }
    if let Some(namespace) = &args.namespace
        && pod_namespace(pod) != namespace
    {
        return false;
    }
    if let Some(image) = &args.image
        && !pod_containers(pod).iter().any(|c| &c.image == image)
    {
        return false;
    }
    true
}

pub async fn delete_pod(server: &str, args: DeletePodArgs) -> Result<(), CliError> {
    let mut client = ApiServerClient::connect(server.to_string())
        .await
        .map_err(|source| CliError::Connect {
            addr: server.to_string(),
            source,
        })?;

    client
        .delete_pod(DeletePodRequest {
            name: args.name.clone(),
            namespace: args.namespace.clone(),
        })
        .await?;

    println!(
        "pod/{} deleted in namespace \"{}\"",
        args.name, args.namespace
    );
    Ok(())
}

fn print_pod(pod: &PodDetail) {
    let name = pod_name(pod);
    let namespace = pod_namespace(pod);
    let status = pod_status(pod);
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

    let requests = pod
        .core
        .as_ref()
        .and_then(|c| c.pod.as_ref())
        .and_then(|p| p.requests.as_ref());
    let limits = pod
        .core
        .as_ref()
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
    for container in pod_containers(pod) {
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
