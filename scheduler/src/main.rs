use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use proto::api::v1::api_server_client::ApiServerClient;
use proto::api::v1::{AssignPodRequest, WatchNodesRequest, WatchPodsRequest, assign_pod_request};
use proto::shared::v1::{EventType, NodeStatus, Pod};
use proto::tls::{TlsArgs, TlsMode, load_client_tls_config, tls_mode};
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
    /// Address of the API server (e.g. http://127.0.0.1:50052, or
    /// https://127.0.0.1:50052 when using --tls-*)
    #[arg(long, env = "BARENETES_SERVER")]
    server: String,

    #[command(flatten)]
    tls: TlsArgs,
}

/// Connects to the API server, plaintext or mTLS depending on `tls`. In mTLS mode
/// `--tls-server-name` must be set, since the certs `barenetes-pki` issues carry no
/// public DNS name for `tonic` to default the expected server identity to.
async fn connect(server: String, tls: &TlsArgs) -> Result<ApiServerClient<Channel>> {
    match tls_mode(tls)? {
        TlsMode::Plaintext => Ok(ApiServerClient::connect(server).await?),
        TlsMode::Mtls { cert, key, ca } => {
            let server_name = tls.tls_server_name.as_deref().context(
                "--tls-server-name is required when connecting over mTLS (--tls-cert/--tls-key/--tls-ca set)",
            )?;
            let channel = Channel::from_shared(server)?
                .tls_config(load_client_tls_config(&cert, &key, &ca, server_name)?)?
                .connect()
                .await?;
            Ok(ApiServerClient::new(channel))
        }
    }
}

/// (namespace, name)
type PodKey = (String, String);

/// Pods the scheduler has seen and couldn't place yet. Kept around so a
/// capacity-relevant event — a node event, or another pod being deleted and
/// freeing its claimed resources — can retry them without waiting for the
/// pod itself to change.
#[derive(Default)]
struct SchedulerState {
    scheduler: BasicScheduler,
    pending: HashMap<PodKey, Pod>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    println!("Connecting to API server at {}", cli.server);
    let client = connect(cli.server.clone(), &cli.tls).await?;

    let state = Arc::new(Mutex::new(SchedulerState::default()));

    supervise_watchers(
        watch_nodes(client.clone(), state.clone()),
        watch_pods(client, state),
    )
    .await
}

/// Both watches are required for the scheduler to work. If either one stops,
/// return its result and cancel the other rather than leaving the process
/// running with only half of its state being updated.
async fn supervise_watchers<N, P>(nodes: N, pods: P) -> Result<()>
where
    N: Future<Output = Result<()>>,
    P: Future<Output = Result<()>>,
{
    tokio::select! {
        result = nodes => result.context("node watcher failed"),
        result = pods => result.context("pod watcher failed"),
    }
}

/// Keeps the scheduler's node view current and retries any pod that
/// couldn't be placed before, since a node change (added, more allocatable
/// capacity, recovered from NOT_READY) is exactly what makes a pending pod
/// schedulable again. A pod deletion that frees a placement triggers the
/// same retry from `watch_pods`.
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
            drop(guard);
        } else {
            let node_name = node.name.clone();
            let became_not_ready = node.status() == NodeStatus::NotReady;
            guard.scheduler.upsert_node(node);
            drop(guard);

            if became_not_ready {
                reschedule_orphaned_pods(&mut client, &state, &node_name).await?;
            }
        }

        retry_pending(&mut client, &state).await?;
    }

    Ok(())
}

/// A node just went NOT_READY: every pod placed there is no longer somewhere
/// we can vouch for. Evict each from the scheduler's bookkeeping, tell the API
/// to mark it UNKNOWN (clearing its stale `node_name`), then immediately retry
/// placement exactly like `try_schedule` does for a brand-new pod — landing on
/// PENDING if a fit is found elsewhere, or NO_NODE_AVAILABLE (retried later via
/// `pending`, same as any other unschedulable pod) if not.
async fn reschedule_orphaned_pods(
    client: &mut ApiServerClient<Channel>,
    state: &Arc<Mutex<SchedulerState>>,
    node_name: &str,
) -> Result<()> {
    let orphaned = state.lock().await.scheduler.evict_node(node_name);

    for (namespace, name, limits) in orphaned {
        let reason = format!("node {node_name} became NotReady");
        let result = client
            .assign_pod(AssignPodRequest {
                name: name.clone(),
                namespace: namespace.clone(),
                outcome: Some(assign_pod_request::Outcome::OrphanedReason(reason)),
            })
            .await;

        match result {
            Ok(_) => {}
            Err(status) if status.code() == tonic::Code::NotFound => {
                println!("Pod {namespace}/{name} no longer exists, skipping reschedule");
                continue;
            }
            Err(status) => {
                println!("Failed to mark {namespace}/{name} orphaned: {status}");
                continue;
            }
        }

        let pod = Pod {
            name: name.clone(),
            status: 0,
            requests: None,
            limits: Some(limits),
        };
        try_schedule(client, state, &namespace, &name, &pod, false).await?;
    }

    Ok(())
}

async fn retry_pending(
    client: &mut ApiServerClient<Channel>,
    state: &Arc<Mutex<SchedulerState>>,
) -> Result<()> {
    let retry: Vec<(PodKey, Pod)> = {
        let guard = state.lock().await;
        guard
            .pending
            .iter()
            .map(|(key, pod)| (key.clone(), pod.clone()))
            .collect()
    };

    for ((namespace, name), pod) in retry {
        try_schedule(client, state, &namespace, &name, &pod, true).await?;
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

        if event_type == EventType::Deleted {
            let mut guard = state.lock().await;
            guard.pending.remove(&key);
            let released = guard.scheduler.release_placement(&namespace, &pod.name);
            drop(guard);

            // Only a released placement frees capacity that could unblock a
            // pending pod; a deleted pod that was itself pending changes
            // nothing worth retrying for.
            if released {
                retry_pending(&mut client, &state).await?;
            }
            continue;
        }

        if !pod_detail.node_name.is_empty() {
            let mut guard = state.lock().await;
            guard.pending.remove(&key);
            guard.scheduler.record_placement(
                &namespace,
                &pod.name,
                &pod_detail.node_name,
                pod.limits.unwrap_or_default(),
            );
            continue;
        }

        // Only a brand-new pod triggers scheduling here. AssignPod's
        // unschedulable-reason path itself publishes a Modified event with
        // node_name still empty, so reacting to Modified here would make the
        // scheduler retry-loop against its own writes forever; already-pending
        // pods are retried from watch_nodes on node events, and from this
        // function's own Deleted branch above when a pod deletion frees a
        // placement.
        if event_type != EventType::Added {
            continue;
        }

        try_schedule(&mut client, &state, &namespace, &pod.name, &pod, false).await?;
    }

    Ok(())
}

/// Runs placement against the current node view, reports the outcome back
/// to the API server via `AssignPod`, and keeps `pending` in sync so a
/// later node event can retry an unschedulable pod.
///
/// `only_if_pending` must be `true` for calls driven by a `pending` snapshot
/// (i.e. from `retry_pending`): two retry sweeps can run concurrently (one
/// from `watch_nodes`, one from `watch_pods`'s Deleted branch), and without
/// re-checking that the key is still pending once the lock is held, a sweep
/// working off a stale snapshot can re-place a pod another sweep already
/// resolved — placing it on a second node while the first is never told to
/// stop. It must be `false` for a brand-new `Added` pod, which isn't in
/// `pending` yet.
async fn try_schedule(
    client: &mut ApiServerClient<Channel>,
    state: &Arc<Mutex<SchedulerState>>,
    namespace: &str,
    name: &str,
    pod: &Pod,
    only_if_pending: bool,
) -> Result<()> {
    let key = (namespace.to_string(), name.to_string());

    let outcome = {
        let mut guard = state.lock().await;
        if only_if_pending && !guard.pending.contains_key(&key) {
            // Already resolved by a concurrent retry sweep (placed, or
            // dropped because the pod was deleted) since this call's
            // snapshot was taken.
            return Ok(());
        }
        let outcome = guard.scheduler.place(pod);
        match &outcome {
            Ok(node_name) => {
                println!("Scheduling pod {namespace}/{name} on {node_name}");
                guard.scheduler.record_placement(
                    namespace,
                    name,
                    node_name,
                    pod.limits.unwrap_or_default(),
                );
                guard.pending.remove(&key);
            }
            Err(reason) => {
                println!("Pod {namespace}/{name} is unschedulable: {reason}");
                guard.pending.insert(key.clone(), pod.clone());
            }
        }
        outcome
    };

    let assigned_node = match &outcome {
        Ok(node_name) => Some(node_name.clone()),
        Err(_) => None,
    };

    let outcome = match outcome {
        Ok(node_name) => assign_pod_request::Outcome::NodeName(node_name),
        Err(reason) => assign_pod_request::Outcome::UnschedulableReason(reason),
    };

    let result = client
        .assign_pod(AssignPodRequest {
            name: name.to_string(),
            namespace: namespace.to_string(),
            outcome: Some(outcome),
        })
        .await;

    // A failed AssignPod must never take down the watch loops: a pod
    // deleted mid-retry (NotFound) just drops out of `pending`, and any
    // other transient error is logged so the scheduler keeps serving
    // every other pod.
    match result {
        Ok(_) => {
            if let Some(node_name) = assigned_node {
                println!("Pod {namespace}/{name} successfully assigned to {node_name}");
            }
        }
        Err(status) => {
            if status.code() == tonic::Code::NotFound {
                println!("Pod {namespace}/{name} no longer exists, dropping from pending");
                state.lock().await.pending.remove(&key);
            } else {
                println!("AssignPod failed for {namespace}/{name}: {status}");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::*;

    struct DropSignal(Arc<AtomicBool>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn watcher_error_is_returned_and_pending_sibling_is_cancelled() {
        let sibling_cancelled = Arc::new(AtomicBool::new(false));
        let drop_signal = DropSignal(sibling_cancelled.clone());

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            supervise_watchers(
                async { Err(anyhow::anyhow!("node stream failed")) },
                async move {
                    let _drop_signal = drop_signal;
                    std::future::pending::<Result<()>>().await
                },
            ),
        )
        .await
        .expect("supervisor should not wait for the healthy watcher")
        .expect_err("watcher failure should be returned");

        assert_eq!(
            format!("{result:#}"),
            "node watcher failed: node stream failed"
        );
        assert!(
            sibling_cancelled.load(Ordering::SeqCst),
            "pending watcher should be cancelled"
        );
    }
}
