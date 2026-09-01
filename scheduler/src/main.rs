use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use proto::api::v1::api_server_client::ApiServerClient;
use proto::api::v1::{AssignPodRequest, WatchNodesRequest, WatchPodsRequest, assign_pod_request};
use proto::shared::v1::{EventType, Pod};
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
use tonic::transport::Channel;

mod schedulers;

use schedulers::BasicScheduler;

/// The scheduler runs on worker nodes, never on a control plane, so there's
/// no sensible default: the operator must always say where the API server is.
#[derive(Parser)]
#[command(name = "scheduler", version, about = "Barenetes scheduler")]
struct Cli {
    /// Address of the API server (e.g. http://127.0.0.1:50052)
    #[arg(long, env = "BARENETES_SERVER")]
    server: String,
}

/// (namespace, name)
type PodKey = (String, String);

/// Pods the scheduler has seen and couldn't place yet. Kept around so a
/// capacity-relevant node event can retry them without waiting for the pod
/// itself to change.
#[derive(Default)]
struct SchedulerState {
    scheduler: BasicScheduler,
    pending: HashMap<PodKey, Pod>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let client = ApiServerClient::connect(cli.server.clone()).await?;
    println!("Scheduler connected to API server at {}", cli.server);

    let state = Arc::new(Mutex::new(SchedulerState::default()));

    let nodes_task = tokio::spawn(watch_nodes(client.clone(), state.clone()));
    let pods_task = tokio::spawn(watch_pods(client.clone(), state.clone()));

    let (nodes_result, pods_result) = tokio::try_join!(nodes_task, pods_task)?;
    nodes_result?;
    pods_result?;

    Ok(())
}

/// Keeps the scheduler's node view current and retries any pod that
/// couldn't be placed before, since a node change (added, more allocatable
/// capacity, recovered from NOT_READY) is exactly what makes a pending pod
/// schedulable again.
async fn watch_nodes(
    mut client: ApiServerClient<Channel>,
    state: Arc<Mutex<SchedulerState>>,
) -> Result<()> {
    let mut stream = client.watch_nodes(WatchNodesRequest {}).await?.into_inner();

    while let Some(event) = stream.next().await {
        let event = event?;
        let event_type = event.event_type();
        let Some(node) = event.node else { continue };

        let mut guard = state.lock().await;
        if event_type == EventType::Deleted {
            guard.scheduler.remove_node(&node.name);
        } else {
            guard.scheduler.upsert_node(node);
        }
        let retry: Vec<(PodKey, Pod)> = guard
            .pending
            .iter()
            .map(|(key, pod)| (key.clone(), pod.clone()))
            .collect();
        drop(guard);

        for ((namespace, name), pod) in retry {
            try_schedule(&mut client, &state, &namespace, &name, &pod).await?;
        }
    }

    Ok(())
}

/// Schedules pods as they show up pending, and drops any that get deleted
/// or already-placed (e.g. by a status update) from the retry set.
async fn watch_pods(
    mut client: ApiServerClient<Channel>,
    state: Arc<Mutex<SchedulerState>>,
) -> Result<()> {
    let mut stream = client.watch_pods(WatchPodsRequest {}).await?.into_inner();

    while let Some(event) = stream.next().await {
        let event = event?;
        let event_type = event.event_type();
        let Some(pod_detail) = event.pod else {
            continue;
        };

        let namespace = pod_detail
            .core
            .as_ref()
            .and_then(|core| core.spec.as_ref())
            .map(|spec| spec.namespace.clone())
            .unwrap_or_default();
        let Some(pod) = pod_detail.core.as_ref().and_then(|core| core.pod.clone()) else {
            continue;
        };
        let key = (namespace.clone(), pod.name.clone());

        if event_type == EventType::Deleted || !pod_detail.node_name.is_empty() {
            state.lock().await.pending.remove(&key);
            continue;
        }

        // Only a brand-new pod triggers scheduling here. AssignPod's
        // unschedulable-reason path itself publishes a Modified event with
        // node_name still empty, so reacting to Modified here would make the
        // scheduler retry-loop against its own writes forever; retries for
        // already-pending pods happen from watch_nodes instead.
        if event_type != EventType::Added {
            continue;
        }

        try_schedule(&mut client, &state, &namespace, &pod.name, &pod).await?;
    }

    Ok(())
}

/// Runs placement against the current node view, reports the outcome back
/// to the API server via `AssignPod`, and keeps `pending` in sync so a
/// later node event can retry an unschedulable pod.
async fn try_schedule(
    client: &mut ApiServerClient<Channel>,
    state: &Arc<Mutex<SchedulerState>>,
    namespace: &str,
    name: &str,
    pod: &Pod,
) -> Result<()> {
    let key = (namespace.to_string(), name.to_string());

    let outcome = {
        let mut guard = state.lock().await;
        let outcome = guard.scheduler.place(pod);
        match &outcome {
            Ok(node_name) => {
                println!("Scheduling pod {namespace}/{name} on {node_name}");
                guard.pending.remove(&key);
            }
            Err(reason) => {
                println!("Pod {namespace}/{name} is unschedulable: {reason}");
                guard.pending.insert(key, pod.clone());
            }
        }
        outcome
    };

    let outcome = match outcome {
        Ok(node_name) => assign_pod_request::Outcome::NodeName(node_name),
        Err(reason) => assign_pod_request::Outcome::UnschedulableReason(reason),
    };

    client
        .assign_pod(AssignPodRequest {
            name: name.to_string(),
            namespace: namespace.to_string(),
            outcome: Some(outcome),
        })
        .await?;

    Ok(())
}
