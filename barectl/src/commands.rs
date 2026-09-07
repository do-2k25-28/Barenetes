use proto::api::v1::api_server_client::ApiServerClient;
use proto::api::v1::{
    CreatePodRequest, DeletePodRequest, GetNodeRequest, ListNodesRequest, ListPodsRequest,
};
use proto::shared::v1::{
    Container, Node, NodeStatus, Pod, PodDetail, PodSpec, PodStatus, PodWithSpec, Protocol,
    Resources,
};
use proto::tls::{TlsArgs, TlsMode, load_client_tls_config, tls_mode};
use tonic::transport::Channel;

use crate::cli::{CreatePodArgs, DeletePodArgs, GetNodeArgs, GetPodArgs};
use crate::error::CliError;
use crate::manifest::PodManifest;

/// Connects to the API server, plaintext or mTLS depending on `tls`. In mTLS mode
/// `--tls-server-name` must be set, since the certs `barenetes-pki` issues carry no
/// public DNS name for `tonic` to default the expected server identity to.
async fn connect(server: &str, tls: &TlsArgs) -> Result<ApiServerClient<Channel>, CliError> {
    match tls_mode(tls)? {
        TlsMode::Plaintext => {
            ApiServerClient::connect(server.to_string())
                .await
                .map_err(|source| CliError::Connect {
                    addr: server.to_string(),
                    source,
                })
        }
        TlsMode::Mtls { cert, key, ca } => {
            let server_name = tls.tls_server_name.as_deref().ok_or_else(|| {
                CliError::InvalidUsage(
                    "--tls-server-name is required when connecting over mTLS (--tls-cert/--tls-key/--tls-ca set)"
                        .to_string(),
                )
            })?;
            let channel = Channel::from_shared(server.to_string())
                .map_err(|source| CliError::Tls(source.into()))?
                .tls_config(load_client_tls_config(&cert, &key, &ca, server_name)?)
                .map_err(|source| CliError::Connect {
                    addr: server.to_string(),
                    source,
                })?
                .connect()
                .await
                .map_err(|source| CliError::Connect {
                    addr: server.to_string(),
                    source,
                })?;
            Ok(ApiServerClient::new(channel))
        }
    }
}

pub async fn create_pod(server: &str, tls: &TlsArgs, args: CreatePodArgs) -> Result<(), CliError> {
    let pod = build_pod(args)?;
    require_limits(&pod)?;
    let name = pod.pod.as_ref().map(|p| p.name.clone()).unwrap_or_default();
    let namespace = pod
        .spec
        .as_ref()
        .map(|s| s.namespace.clone())
        .unwrap_or_default();

    let mut client = connect(server, tls).await?;
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
            requests: resources(args.cpu_request, args.memory_request)?,
            limits: resources(args.cpu_limit, args.memory_limit)?,
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

/// The scheduler refuses to place any pod without `resources.limits`
/// (`scheduler::schedulers::basic::place`), leaving it stuck `NoNodeAvailable`
/// forever with no further feedback. Catch it here instead, before ever
/// reaching the API server, with a message that says how to fix it from
/// either creation path (flags or manifest).
fn require_limits(pod: &PodWithSpec) -> Result<(), CliError> {
    let has_limits = pod.pod.as_ref().is_some_and(|p| p.limits.is_some());
    if has_limits {
        return Ok(());
    }

    let name = pod.pod.as_ref().map(|p| p.name.as_str()).unwrap_or("");
    Err(CliError::InvalidUsage(format!(
        "pod \"{name}\" is missing resources.limits; the scheduler will never place it without one.\n  \
         Flags:    add --cpu-limit <milli-cpu> --memory-limit <MB>, e.g. --cpu-limit 250 --memory-limit 128\n  \
         Manifest: add a resources.limits block, e.g.\n              \
         resources:\n                \
         limits:\n                  \
         cpu: 250\n                  \
         memory: 128"
    )))
}

fn resources(cpu: Option<i32>, memory: Option<i32>) -> Result<Option<Resources>, CliError> {
    match (cpu, memory) {
        (None, None) => Ok(None),
        (Some(c), Some(m)) => Ok(Some(Resources { cpu: c, memory: m })),
        _ => Err(CliError::InvalidUsage(
            "cpu and memory must both be specified, or both omitted".to_string(),
        )),
    }
}

pub async fn get_pod(server: &str, tls: &TlsArgs, args: GetPodArgs) -> Result<(), CliError> {
    let mut client = connect(server, tls).await?;

    let response = client.list_pods(ListPodsRequest {}).await?;
    let mut pods = response.into_inner().pods;
    pods.retain(|pod| matches_filters(pod, &args));

    if pods.is_empty() {
        println!("0 pods returned.");
        return Ok(());
    }

    // if there's only 1 pod and the request specified a name, display details
    if pods.len() == 1 {
        match &args.name {
            None => {}
            Some(_) => {
                print_pod(&pods[0]);
                return Ok(());
            }
        }
    }

    pods.sort_by(|a, b| (pod_namespace(a), pod_name(a)).cmp(&(pod_namespace(b), pod_name(b))));

    println!(
        "{:<15} {:<12} {:<16} {:<15} {:<8} {:<8} {:<9} {:<9} IMAGE",
        "NAME", "NAMESPACE", "STATUS", "NODE", "CPU-REQ", "CPU-LIM", "MEM-REQ", "MEM-LIM"
    );
    for pod in &pods {
        let images = pod_containers(pod)
            .iter()
            .map(|c| c.image.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let status = format!("{:?}", pod_status(pod));
        let requests = pod_requests(pod);
        let limits = pod_limits(pod);
        println!(
            "{:<15} {:<12} {:<16} {:<15} {:<8} {:<8} {:<9} {:<9} {}",
            pod_name(pod),
            pod_namespace(pod),
            status,
            or_none(&pod.node_name),
            fmt_resource_quantity(requests.map(|r| r.cpu), "m"),
            fmt_resource_quantity(limits.map(|r| r.cpu), "m"),
            fmt_resource_quantity(requests.map(|r| r.memory), "MB"),
            fmt_resource_quantity(limits.map(|r| r.memory), "MB"),
            images
        );
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

fn pod_requests(pod: &PodDetail) -> Option<&Resources> {
    pod.core
        .as_ref()
        .and_then(|c| c.pod.as_ref())
        .and_then(|p| p.requests.as_ref())
}

fn pod_limits(pod: &PodDetail) -> Option<&Resources> {
    pod.core
        .as_ref()
        .and_then(|c| c.pod.as_ref())
        .and_then(|p| p.limits.as_ref())
}

pub async fn get_node(server: &str, tls: &TlsArgs, args: GetNodeArgs) -> Result<(), CliError> {
    let client = connect(server, tls).await?;

    match args.name {
        None => list_nodes(client).await,
        Some(node_name) => get_one_node(client, node_name).await,
    }
}

async fn get_one_node(
    mut client: ApiServerClient<Channel>,
    node_name: String,
) -> Result<(), CliError> {
    let response = client.get_node(GetNodeRequest { name: node_name }).await?;

    match response.into_inner().node {
        Some(node) => print_node(&node),
        None => return Err(CliError::EmptyResponse),
    }

    Ok(())
}

async fn list_nodes(mut client: ApiServerClient<Channel>) -> Result<(), CliError> {
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
        "{:<20} {:<12} {:<10} {:<10} {:<10} {:<10}",
        "NAME", "STATUS", "CPU-CAP", "CPU-ALLOC", "MEM-CAP", "MEM-ALLOC"
    );
    for node in &nodes {
        let status = NodeStatus::try_from(node.status).unwrap_or(NodeStatus::NotReady);
        let capacity = node.capacity.as_ref();
        let allocatable = node.allocatable.as_ref();
        println!(
            "{:<20} {:<12} {:<10} {:<10} {:<10} {:<10}",
            node.name,
            format!("{:?}", status),
            fmt_resource_quantity(capacity.map(|r| r.cpu), "m"),
            fmt_resource_quantity(allocatable.map(|r| r.cpu), "m"),
            fmt_resource_quantity(capacity.map(|r| r.memory), "Mi"),
            fmt_resource_quantity(allocatable.map(|r| r.memory), "Mi"),
        );
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

fn matches_filters(pod: &PodDetail, args: &GetPodArgs) -> bool {
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
        && !pod_containers(pod)
            .iter()
            .any(|c| c.image.contains(image.as_str()))
    {
        return false;
    }
    true
}

pub async fn delete_pod(server: &str, tls: &TlsArgs, args: DeletePodArgs) -> Result<(), CliError> {
    let mut client = connect(server, tls).await?;

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

    let requests = pod_requests(pod);
    let limits = pod_limits(pod);
    if requests.is_some() || limits.is_some() {
        println!();
        if let Some(requests) = requests {
            println!(
                "Requests:    cpu={}m, memory={}MB",
                requests.cpu, requests.memory
            );
        }
        if let Some(limits) = limits {
            println!(
                "Limits:      cpu={}m, memory={}MB",
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

/// Formats a resource quantity for a table column, `-` when the pod/node
/// never reported one (e.g. a pod with only `requests`, or a node
/// `NotReady` since boot with no capacity report yet).
fn fmt_resource_quantity(value: Option<i32>, unit: &str) -> String {
    match value {
        Some(v) => format!("{v}{unit}"),
        None => "-".to_string(),
    }
}

fn protocol_str(protocol: i32) -> &'static str {
    match Protocol::try_from(protocol) {
        Ok(Protocol::Tcp) => "tcp",
        Ok(Protocol::Udp) => "udp",
        Err(_) => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pod_detail(namespace: &str, name: &str, image: &str) -> PodDetail {
        PodDetail {
            core: Some(PodWithSpec {
                pod: Some(Pod {
                    name: name.to_string(),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    namespace: namespace.to_string(),
                    containers: vec![Container {
                        name: name.to_string(),
                        image: image.to_string(),
                        ..Default::default()
                    }],
                }),
            }),
            ..Default::default()
        }
    }

    fn args(name: Option<&str>, namespace: Option<&str>, image: Option<&str>) -> GetPodArgs {
        GetPodArgs {
            name: name.map(String::from),
            namespace: namespace.map(String::from),
            image: image.map(String::from),
        }
    }

    #[test]
    fn image_filter_matches_a_substring_of_the_full_reference() {
        let pod = pod_detail("default", "web", "docker.io/library/nginx:alpine");
        assert!(matches_filters(&pod, &args(None, None, Some("nginx"))));
    }

    #[test]
    fn image_filter_rejects_pods_without_the_substring() {
        let pod = pod_detail("default", "web", "docker.io/library/nginx:alpine");
        assert!(!matches_filters(&pod, &args(None, None, Some("redis"))));
    }

    #[test]
    fn name_filter_is_exact() {
        let pod = pod_detail("default", "web", "nginx:alpine");
        assert!(matches_filters(&pod, &args(Some("web"), None, None)));
        assert!(!matches_filters(&pod, &args(Some("we"), None, None)));
    }

    #[test]
    fn namespace_filter_is_exact() {
        let pod = pod_detail("default", "web", "nginx:alpine");
        assert!(matches_filters(&pod, &args(None, Some("default"), None)));
        assert!(!matches_filters(&pod, &args(None, Some("other"), None)));
    }

    fn pod_with_spec(
        name: &str,
        requests: Option<Resources>,
        limits: Option<Resources>,
    ) -> PodWithSpec {
        PodWithSpec {
            pod: Some(Pod {
                name: name.to_string(),
                status: PodStatus::Pending as i32,
                requests,
                limits,
            }),
            spec: Some(PodSpec {
                namespace: "default".to_string(),
                containers: vec![],
            }),
        }
    }

    #[test]
    fn require_limits_accepts_a_pod_with_limits() {
        let pod = pod_with_spec(
            "web",
            None,
            Some(Resources {
                cpu: 250,
                memory: 128,
            }),
        );
        assert!(require_limits(&pod).is_ok());
    }

    #[test]
    fn require_limits_rejects_a_pod_without_limits() {
        let pod = pod_with_spec(
            "web",
            Some(Resources {
                cpu: 100,
                memory: 64,
            }),
            None,
        );
        let err = require_limits(&pod).unwrap_err().to_string();
        assert!(err.contains("web"), "error should name the pod: {err}");
        assert!(
            err.contains("--cpu-limit") && err.contains("--memory-limit"),
            "error should explain the flags to add: {err}"
        );
        assert!(
            err.contains("resources.limits"),
            "error should explain the manifest field to add: {err}"
        );
    }

    #[test]
    fn fmt_resource_quantity_formats_a_present_value() {
        assert_eq!(fmt_resource_quantity(Some(250), "m"), "250m");
    }

    #[test]
    fn fmt_resource_quantity_shows_dash_when_unset() {
        assert_eq!(fmt_resource_quantity(None, "m"), "-");
    }
}
