use std::path::Path;

use proto::api::v1::api_server_client::ApiServerClient;
use proto::api::v1::{
    CreatePodRequest, DeletePodRequest, GetNodeRequest, ListNodesRequest, ListPodsRequest,
};
use proto::shared::v1::{
    Container, Node, NodeStatus, Pod, PodDetail, PodSpec, PodStatus, PodWithSpec, Protocol,
    Resources,
};
use proto::tls::{load_client_tls_config, load_client_tls_config_from_bytes};
use tonic::transport::Channel;

use crate::cli::{ConfigSetArgs, CreatePodArgs, DeletePodArgs, GetNodeArgs, GetPodArgs};
use crate::config::{self, FileConfig, ResolvedTls};
use crate::error::CliError;
use crate::manifest::PodManifest;

/// Connects to the API server, plaintext or mTLS depending on `tls`. In mTLS mode
/// the cert/key/CA may come from files on disk (CLI flags/env) or from bytes
/// already decoded from the config file -- see [`ResolvedTls`].
async fn connect(server: &str, tls: &ResolvedTls) -> Result<ApiServerClient<Channel>, CliError> {
    let tls_config = match tls {
        ResolvedTls::Plaintext => None,
        ResolvedTls::Paths {
            cert,
            key,
            ca,
            server_name,
        } => Some(load_client_tls_config(cert, key, ca, server_name)?),
        ResolvedTls::Bytes {
            cert,
            key,
            ca,
            server_name,
        } => Some(load_client_tls_config_from_bytes(
            cert,
            key,
            ca,
            server_name,
        )?),
    };

    match tls_config {
        None => ApiServerClient::connect(server.to_string())
            .await
            .map_err(|source| CliError::Connect {
                addr: server.to_string(),
                source,
            }),
        Some(tls_config) => {
            let channel = Channel::from_shared(server.to_string())
                .map_err(|source| CliError::Tls(source.into()))?
                .tls_config(tls_config)
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

pub async fn create_pod(
    server: &str,
    tls: &ResolvedTls,
    args: CreatePodArgs,
) -> Result<(), CliError> {
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

pub async fn get_pod(server: &str, tls: &ResolvedTls, args: GetPodArgs) -> Result<(), CliError> {
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
        "{:<15} {:<12} {:<16} {:<15} {:<15} {:<8} {:<8} {:<9} {:<9} {:<20} IMAGE",
        "NAME",
        "NAMESPACE",
        "STATUS",
        "NODE",
        "POD-IP",
        "CPU-REQ",
        "CPU-LIM",
        "MEM-REQ",
        "MEM-LIM",
        "PORTS"
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
            "{:<15} {:<12} {:<16} {:<15} {:<15} {:<8} {:<8} {:<9} {:<9} {:<20} {}",
            pod_name(pod),
            pod_namespace(pod),
            status,
            or_none(&pod.node_name),
            pod_ip(pod),
            fmt_resource_quantity(requests.map(|r| r.cpu), "m"),
            fmt_resource_quantity(limits.map(|r| r.cpu), "m"),
            fmt_resource_quantity(requests.map(|r| r.memory), "MB"),
            fmt_resource_quantity(limits.map(|r| r.memory), "MB"),
            pod_ports(pod),
            images
        );
    }

    Ok(())
}

fn pod_ip(pod: &PodDetail) -> &str {
    pod.pod_ip
        .as_deref()
        .filter(|ip| !ip.is_empty())
        .unwrap_or("<none>")
}

fn pod_ports(pod: &PodDetail) -> String {
    let ports = pod_containers(pod)
        .iter()
        .flat_map(|c| c.ports.iter())
        .map(|p| {
            format!(
                "{}->{}/{}",
                p.external,
                p.internal,
                protocol_str(p.protocol)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    if ports.is_empty() {
        "<none>".to_string()
    } else {
        ports
    }
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

pub async fn get_node(server: &str, tls: &ResolvedTls, args: GetNodeArgs) -> Result<(), CliError> {
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
        "{:<20} {:<12} {:<15} {:<10} {:<10} {:<10} {:<10}",
        "NAME", "STATUS", "NODE-IP", "CPU-CAP", "CPU-ALLOC", "MEM-CAP", "MEM-ALLOC"
    );
    for node in &nodes {
        let status = NodeStatus::try_from(node.status).unwrap_or(NodeStatus::NotReady);
        let capacity = node.capacity.as_ref();
        let allocatable = node.allocatable.as_ref();
        println!(
            "{:<20} {:<12} {:<15} {:<10} {:<10} {:<10} {:<10}",
            node.name,
            format!("{:?}", status),
            or_none(&node.ip),
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
    println!("Node IP:     {}", or_none(&node.ip));
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

pub async fn delete_pod(
    server: &str,
    tls: &ResolvedTls,
    args: DeletePodArgs,
) -> Result<(), CliError> {
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

    println!("Name:        {name}");
    println!("Namespace:   {namespace}");
    println!("Status:      {status:?}");
    println!("Node:        {}", or_none(&pod.node_name));
    println!("Pod IP:      {}", pod_ip(pod));

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

/// Writes `args` to the config file at `config_path`, replacing whatever
/// was there. Reads the given PEM files from local disk and embeds them
/// base64-encoded -- `config set` never talks to the network or generates
/// any key material itself, it only packages certs already issued by
/// `barenetes-pki`.
pub fn config_set(config_path: &Path, args: ConfigSetArgs) -> Result<(), CliError> {
    let tls_data = match (&args.tls_cert, &args.tls_key, &args.tls_ca) {
        (Some(cert), Some(key), Some(ca)) => {
            if args.tls_server_name.is_none() {
                return Err(CliError::InvalidUsage(
                    "--tls-server-name is required when --tls-cert/--tls-key/--tls-ca are set"
                        .to_string(),
                ));
            }
            Some((
                config::read_and_encode(ca)?,
                config::read_and_encode(cert)?,
                config::read_and_encode(key)?,
            ))
        }
        (None, None, None) => None,
        _ => {
            return Err(CliError::InvalidUsage(
                "--tls-cert, --tls-key and --tls-ca must all be set together or all omitted"
                    .to_string(),
            ));
        }
    };

    let file_config = FileConfig {
        server: args.server,
        tls_server_name: args.tls_server_name,
        certificate_authority_data: tls_data.as_ref().map(|(ca, _, _)| ca.clone()),
        client_certificate_data: tls_data.as_ref().map(|(_, cert, _)| cert.clone()),
        client_key_data: tls_data.as_ref().map(|(_, _, key)| key.clone()),
    };

    config::save(config_path, &file_config)?;
    println!("wrote {}", config_path.display());
    Ok(())
}

/// Prints the currently configured server and whether a TLS identity is
/// set. Never prints certificate or key material.
pub fn config_view(config_path: &Path) -> Result<(), CliError> {
    let file_config = config::load(config_path)?;
    print!("{}", format_view(config_path, file_config.as_ref()));
    Ok(())
}

fn format_view(config_path: &Path, file_config: Option<&FileConfig>) -> String {
    let Some(config) = file_config else {
        return format!("No config file at {}.\n", config_path.display());
    };

    let tls = if config.client_certificate_data.is_some() {
        "configured"
    } else {
        "not configured (plaintext)"
    };
    format!(
        "Config file:      {}\nServer:           {}\nTLS server name:  {}\nTLS:              {}\n",
        config_path.display(),
        config.server,
        config.tls_server_name.as_deref().unwrap_or("<none>"),
        tls
    )
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

    #[test]
    fn format_view_reports_no_file_when_absent() {
        let path = std::path::PathBuf::from("/tmp/does-not-exist/config");
        assert_eq!(
            format_view(&path, None),
            "No config file at /tmp/does-not-exist/config.\n"
        );
    }

    #[test]
    fn format_view_shows_plaintext_when_no_tls_data() {
        let path = std::path::PathBuf::from("/tmp/config");
        let config = FileConfig {
            server: "http://127.0.0.1:50052".to_string(),
            ..Default::default()
        };
        let view = format_view(&path, Some(&config));
        assert!(view.contains("Server:           http://127.0.0.1:50052"));
        assert!(view.contains("TLS:              not configured (plaintext)"));
        assert!(view.contains("TLS server name:  <none>"));
    }

    #[test]
    fn format_view_shows_configured_when_tls_data_present() {
        let path = std::path::PathBuf::from("/tmp/config");
        let config = FileConfig {
            server: "https://cp:50052".to_string(),
            tls_server_name: Some("api".to_string()),
            certificate_authority_data: Some("ca".to_string()),
            client_certificate_data: Some("cert".to_string()),
            client_key_data: Some("key".to_string()),
        };
        let view = format_view(&path, Some(&config));
        assert!(view.contains("TLS:              configured"));
        assert!(view.contains("TLS server name:  api"));
    }

    fn tempdir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "barectl-commands-test-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn config_set_writes_a_plaintext_config() {
        let dir = tempdir("set-plaintext");
        let config_path = dir.join("config");

        config_set(
            &config_path,
            ConfigSetArgs {
                server: "http://127.0.0.1:50052".to_string(),
                tls_cert: None,
                tls_key: None,
                tls_ca: None,
                tls_server_name: None,
            },
        )
        .unwrap();

        let loaded = config::load(&config_path).unwrap().unwrap();
        assert_eq!(loaded.server, "http://127.0.0.1:50052");
        assert!(loaded.client_certificate_data.is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn config_set_embeds_the_given_pem_files() {
        let dir = tempdir("set-tls");
        let config_path = dir.join("config");
        std::fs::write(dir.join("cert.pem"), "cert-contents").unwrap();
        std::fs::write(dir.join("key.pem"), "key-contents").unwrap();
        std::fs::write(dir.join("ca.pem"), "ca-contents").unwrap();

        config_set(
            &config_path,
            ConfigSetArgs {
                server: "https://cp:50052".to_string(),
                tls_cert: Some(dir.join("cert.pem")),
                tls_key: Some(dir.join("key.pem")),
                tls_ca: Some(dir.join("ca.pem")),
                tls_server_name: Some("api".to_string()),
            },
        )
        .unwrap();

        let loaded = config::load(&config_path).unwrap().unwrap();
        use base64::Engine as _;
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(loaded.client_certificate_data.unwrap())
                .unwrap(),
            b"cert-contents"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn config_set_rejects_partial_tls_flags() {
        let dir = tempdir("set-partial");
        let config_path = dir.join("config");

        let result = config_set(
            &config_path,
            ConfigSetArgs {
                server: "https://cp:50052".to_string(),
                tls_cert: Some(dir.join("cert.pem")),
                tls_key: None,
                tls_ca: None,
                tls_server_name: None,
            },
        );

        assert!(result.is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
