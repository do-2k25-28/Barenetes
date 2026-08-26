//! Minimal client to drive the API server by hand.
//!
//!     cargo run -p api --example api_cli -- list-nodes
//!     cargo run -p api --example api_cli -- get-node <name>
//!     cargo run -p api --example api_cli -- update-node-status <name> <status> <cpu_cap> <mem_cap>
//!     cargo run -p api --example api_cli -- create-pod <name> <image> [image...] [--namespace <ns>]
//!     cargo run -p api --example api_cli -- delete-pod <name> [--namespace <ns>]
//!     cargo run -p api --example api_cli -- assign-pod <name> <namespace>
//!         --node <node> | --unschedulable <reason>
//!     cargo run -p api --example api_cli -- update-pod-status <name> <namespace> <status>
//!         [--pod-ip <ip>] [--message <msg>] [--cpu <mcpu>] [--memory <mb>]
//!         [--container <name>:<ACTIVE|CRASHED|WAITING>]
//!     cargo run -p api --example api_cli -- list-pods
//!     cargo run -p api --example api_cli -- get-pod <name> [--namespace <ns>]

use proto::api::v1::api_server_client::ApiServerClient;
use proto::api::v1::{
    AssignPodRequest, CreatePodRequest, DeletePodRequest, GetNodeRequest, GetPodRequest,
    ListNodesRequest, ListPodsRequest, UpdateNodeStatusRequest, UpdatePodStatusRequest,
    assign_pod_request,
};
use proto::shared::v1::{
    Container, ContainerStatus, Node, NodeStatus, Pod, PodSpec, PodStatus, PodWithSpec, Resources,
    State,
};

const API: &str = "http://127.0.0.1:50052";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // validate subcommand before connecting so bad usage shows help
    // instead of a connection error.
    let sub = args.first().map(String::as_str);

    // Bad usage or missing args -> print help and exit.
    let valid = matches!(
        sub,
        Some(
            "list-nodes"
                | "get-node"
                | "get-pod"
                | "update-node-status"
                | "list-pods"
                | "create-pod"
                | "delete-pod"
                | "update-pod-status"
                | "assign-pod"
        )
    );
    let enough_args = match sub {
        Some("get-node" | "get-pod" | "delete-pod") => args.len() >= 2,
        Some("create-pod") => args.len() >= 3,
        Some("update-pod-status") => args.len() >= 4,
        Some("assign-pod") => args.len() >= 4,
        Some("update-node-status") => args.len() >= 5,
        _ => true,
    };

    if !valid || !enough_args {
        eprintln!("usage: api_cli list-nodes");
        eprintln!("       api_cli get-node <name>");
        eprintln!(
            "       api_cli update-node-status <name> <READY|CORDON|DRAIN|NOT_READY> <cpu_cap> <mem_cap>"
        );
        eprintln!("       api_cli create-pod <name> <image> [image...] [--namespace <ns>]");
        eprintln!("       api_cli delete-pod <name> [--namespace <ns>]");
        eprintln!(
            "       api_cli assign-pod <name> <namespace> --node <node> | --unschedulable <reason>"
        );
        eprintln!(
            "       api_cli update-pod-status <name> <namespace> <PENDING|RUNNING|SUCCEEDED|FAILED|UNKNOWN>"
        );
        eprintln!("           [--pod-ip <ip>] [--message <msg>] [--cpu <mcpu>] [--memory <mb>]");
        eprintln!("           [--container <name>:<ACTIVE|CRASHED|WAITING>]");
        eprintln!("       api_cli list-pods");
        eprintln!("       api_cli get-pod <name> [--namespace <ns>]");
        std::process::exit(2);
    }

    // Reject a flag in the pod-name position for create-pod.
    if sub == Some("create-pod") && args[1].starts_with("--") {
        eprintln!("error: pod name must not be a flag");
        std::process::exit(2);
    }

    let mut client = ApiServerClient::connect(API).await?;

    match sub {
        Some("list-nodes") => {
            let nodes = client
                .list_nodes(ListNodesRequest {})
                .await?
                .into_inner()
                .nodes;

            if nodes.is_empty() {
                println!("(no nodes)");
            }
            for node in &nodes {
                println!(
                    "{:<20} status={:<10} cpu={}/{}m  mem={}/{}MB",
                    node.name,
                    format!("{:?}", node.status),
                    node.allocatable.as_ref().map_or(0, |r| r.cpu),
                    node.capacity.as_ref().map_or(0, |r| r.cpu),
                    node.allocatable.as_ref().map_or(0, |r| r.memory),
                    node.capacity.as_ref().map_or(0, |r| r.memory),
                );
            }
        }

        Some("get-node") => {
            let node = client
                .get_node(GetNodeRequest {
                    name: args[1].clone(),
                })
                .await?
                .into_inner()
                .node;

            match node {
                Some(n) => println!(
                    "name={}\nstatus={:?}\ncapacity:    cpu={}m  mem={}MB\nallocatable: cpu={}m  mem={}MB",
                    n.name,
                    n.status,
                    n.capacity.as_ref().map_or(0, |r| r.cpu),
                    n.capacity.as_ref().map_or(0, |r| r.memory),
                    n.allocatable.as_ref().map_or(0, |r| r.cpu),
                    n.allocatable.as_ref().map_or(0, |r| r.memory),
                ),
                None => println!("(no node found)"),
            }
        }

        Some("update-node-status") => {
            let name = args[1].clone();
            let status = match args[2].as_str() {
                "READY" => NodeStatus::Ready,
                "CORDON" => NodeStatus::Cordon,
                "DRAIN" => NodeStatus::Drain,
                "NOT_READY" => NodeStatus::NotReady,
                other => {
                    eprintln!("unknown status: {other} (expected READY|CORDON|DRAIN|NOT_READY)");
                    std::process::exit(2);
                }
            };
            let cpu: i32 = args[3].parse()?;
            let mem: i32 = args[4].parse()?;

            client
                .update_node_status(UpdateNodeStatusRequest {
                    node: Some(Node {
                        name,
                        status: status.into(),
                        capacity: Some(Resources { cpu, memory: mem }),
                        allocatable: Some(Resources { cpu, memory: mem }),
                    }),
                })
                .await?;

            println!("node updated");
        }

        Some("create-pod") => {
            let name = args[1].clone();

            // Images are everything between the name and the first --flag.
            let first_flag = args[2..]
                .iter()
                .position(|a| a.starts_with("--"))
                .map(|i| i + 2)
                .unwrap_or(args.len());
            let containers = args[2..first_flag]
                .iter()
                .enumerate()
                .map(|(i, image)| Container {
                    name: i.to_string(),
                    image: image.clone(),
                    ..Default::default()
                })
                .collect();

            let namespace = args
                .iter()
                .position(|a| a == "--namespace")
                .and_then(|i| args.get(i + 1))
                .cloned()
                .unwrap_or_else(|| "default".to_string());

            let response = client
                .create_pod(CreatePodRequest {
                    pod: Some(PodWithSpec {
                        pod: Some(Pod {
                            name,
                            ..Default::default()
                        }),
                        spec: Some(PodSpec {
                            namespace,
                            containers,
                        }),
                    }),
                })
                .await?
                .into_inner()
                .pod;

            match response {
                Some(p) => println!("{:#?}", p),
                None => println!("pod created"),
            }
        }

        Some("delete-pod") => {
            let name = args[1].clone();
            let namespace = args
                .iter()
                .position(|a| a == "--namespace")
                .and_then(|i| args.get(i + 1))
                .cloned()
                .unwrap_or_else(|| "default".to_string());

            let deleted = client
                .delete_pod(DeletePodRequest { name, namespace })
                .await?
                .into_inner()
                .name;

            println!("deleted: {deleted}");
        }

        Some("assign-pod") => {
            let name = args[1].clone();
            let namespace = args[2].clone();
            let node = args
                .iter()
                .position(|a| a == "--node")
                .and_then(|i| args.get(i + 1))
                .cloned();
            let unschedulable = args
                .iter()
                .position(|a| a == "--unschedulable")
                .and_then(|i| args.get(i + 1))
                .cloned();

            let outcome = match (node, unschedulable) {
                (Some(n), None) => assign_pod_request::Outcome::NodeName(n),
                (None, Some(r)) => assign_pod_request::Outcome::UnschedulableReason(r),
                (Some(_), Some(_)) => {
                    eprintln!("--node and --unschedulable are mutually exclusive");
                    std::process::exit(2);
                }
                (None, None) => {
                    eprintln!("one of --node or --unschedulable is required");
                    std::process::exit(2);
                }
            };

            client
                .assign_pod(AssignPodRequest {
                    name,
                    namespace,
                    outcome: Some(outcome),
                })
                .await?;

            println!("pod assigned");
        }

        Some("update-pod-status") => {
            let name = args[1].clone();
            let namespace = args[2].clone();
            let status = match args[3].as_str() {
                "PENDING" => PodStatus::Pending,
                "RUNNING" => PodStatus::Running,
                "SUCCEEDED" => PodStatus::Succeeded,
                "FAILED" => PodStatus::Failed,
                "UNKNOWN" => PodStatus::Unknown,
                other => {
                    eprintln!(
                        "unknown status: {other} (expected PENDING|RUNNING|SUCCEEDED|FAILED|UNKNOWN)"
                    );
                    std::process::exit(2);
                }
            };

            // Parse optional flags.
            let pod_ip = args
                .iter()
                .position(|a| a == "--pod-ip")
                .and_then(|i| args.get(i + 1))
                .cloned();
            let message = args
                .iter()
                .position(|a| a == "--message")
                .and_then(|i| args.get(i + 1))
                .cloned();
            let cpu = args
                .iter()
                .position(|a| a == "--cpu")
                .and_then(|i| args.get(i + 1))
                .and_then(|s| s.parse::<i32>().ok());
            let memory = args
                .iter()
                .position(|a| a == "--memory")
                .and_then(|i| args.get(i + 1))
                .and_then(|s| s.parse::<i32>().ok());

            // Collect --container name:STATE flags (repeatable).
            let container_statuses: Vec<ContainerStatus> = args
                .windows(2)
                .filter(|w| w[0] == "--container")
                .filter_map(|w| {
                    let (cname, state_str) = w[1].split_once(':')?;
                    let state = match state_str {
                        "ACTIVE" => State::Active,
                        "CRASHED" => State::Crashed,
                        "WAITING" => State::Waiting,
                        _ => return None,
                    };
                    Some(ContainerStatus {
                        name: cname.to_string(),
                        state: state.into(),
                    })
                })
                .collect();

            client
                .update_pod_status(UpdatePodStatusRequest {
                    pod: Some(PodWithSpec {
                        pod: Some(Pod {
                            name,
                            status: status.into(),
                            ..Default::default()
                        }),
                        spec: Some(PodSpec {
                            namespace,
                            ..Default::default()
                        }),
                    }),
                    container_statuses,
                    pod_ip,
                    message,
                    resource_usage: if cpu.is_some() || memory.is_some() {
                        Some(Resources {
                            cpu: cpu.unwrap_or(0),
                            memory: memory.unwrap_or(0),
                        })
                    } else {
                        None
                    },
                })
                .await?;

            println!("pod status updated");
        }

        Some("list-pods") => {
            let pods = client
                .list_pods(ListPodsRequest {})
                .await?
                .into_inner()
                .pods;

            if pods.is_empty() {
                println!("(no pods)");
            }
            for pod in &pods {
                let name = pod
                    .core
                    .as_ref()
                    .and_then(|c| c.pod.as_ref())
                    .map_or("(unknown)", |p| &p.name);
                let status = pod
                    .core
                    .as_ref()
                    .and_then(|c| c.pod.as_ref())
                    .map_or("?".into(), |p| format!("{:?}", p.status));
                println!("{:<20} status={:<10} node={}", name, status, pod.node_name,);
            }
        }

        Some("get-pod") => {
            let name = args[1].clone();
            let namespace = args
                .iter()
                .position(|a| a == "--namespace")
                .and_then(|i| args.get(i + 1))
                .cloned()
                .unwrap_or_else(|| "default".to_string());

            let pod = client
                .get_pod(GetPodRequest { name, namespace })
                .await?
                .into_inner()
                .pod;

            match pod {
                Some(p) => println!("{:#?}", p),
                None => println!("(no pod found)"),
            }
        }

        _ => unreachable!(),
    }

    Ok(())
}
