use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use proto::api::v1::api_server_client::ApiServerClient;
use proto::api::v1::{AssignPodRequest, WatchNodesRequest, WatchPodsRequest, assign_pod_request};
use proto::shared::v1::{EventType, NodeStatus, Pod, Resources};
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

/// Keeps the scheduler's node view current and retries pending pods only for
/// node changes that could plausibly unblock one (added, more allocatable
/// capacity, recovered from NOT_READY — see `BasicScheduler::upsert_node`),
/// skipping the retry sweep for events that can't (a `Deleted` node, or a
/// same-or-less-capacity re-report, e.g. from an agent reconnect). A pod
/// deletion that frees a placement triggers the same retry from `watch_pods`.
async fn watch_nodes(
    mut client: ApiServerClient<Channel>,
    state: Arc<Mutex<SchedulerState>>,
) -> Result<()> {
    let mut stream = client.watch_nodes(WatchNodesRequest {}).await?.into_inner();

    while let Some(event) = stream.next().await {
        let event = event?;
        let event_type = event.event_type();
        let Some(node) = event.node else { continue };

        if event_type == EventType::Deleted {
            state.lock().await.scheduler.remove_node(&node.name);
            continue;
        }

        let mut guard = state.lock().await;
        let node_name = node.name.clone();
        let is_not_ready = node.status() == NodeStatus::NotReady;
        let should_retry = guard.scheduler.upsert_node(node);
        drop(guard);

        if is_not_ready {
            reschedule_orphaned_pods(&mut client, &state, &node_name).await?;
        } else if should_retry {
            retry_pending(&mut client, &state).await;
        }
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
        reschedule_one_orphaned_pod(client, state, node_name, &namespace, &name, limits).await?;
    }

    Ok(())
}

/// Marks one pod's assignment to `node_name` lost (clearing `node_name`,
/// status UNKNOWN) and immediately retries placement for it, exactly like
/// `try_schedule` does for a brand-new pod — landing on PENDING if a fit is
/// found elsewhere, or NO_NODE_AVAILABLE (retried later via `pending`) if
/// not. Shared by `reschedule_orphaned_pods` (a node just went NOT_READY)
/// and `watch_pods` (a pod shows up already recorded against a node already
/// confirmed NOT_READY — e.g. a scheduler restart racing that node's own
/// event).
async fn reschedule_one_orphaned_pod(
    client: &mut ApiServerClient<Channel>,
    state: &Arc<Mutex<SchedulerState>>,
    node_name: &str,
    namespace: &str,
    name: &str,
    limits: Resources,
) -> Result<()> {
    let pod = Pod {
        name: name.to_string(),
        status: 0,
        requests: None,
        limits: Some(limits),
    };

    let reason = format!("node {node_name} became NotReady");
    let result = client
        .assign_pod(AssignPodRequest {
            name: name.to_string(),
            namespace: namespace.to_string(),
            outcome: Some(assign_pod_request::Outcome::OrphanedReason(reason)),
        })
        .await;

    match result {
        Ok(_) => {}
        Err(status) if status.code() == tonic::Code::NotFound => {
            println!("Pod {namespace}/{name} no longer exists, skipping reschedule");
            return Ok(());
        }
        Err(status) => {
            println!("Failed to mark {namespace}/{name} orphaned: {status}");
            // Not tracked in `placements` (either just evicted from there, or
            // never recorded there in the first place), so if we don't track
            // it somewhere it's lost from all scheduler bookkeeping forever.
            // Park it in `pending` so a later node event's `retry_pending`
            // sweep can recover it.
            let key = (namespace.to_string(), name.to_string());
            state.lock().await.pending.insert(key, pod);
            return Ok(());
        }
    }

    try_schedule(client, state, namespace, name, &pod, false).await
}

/// Retries every currently-pending pod concurrently rather than one at a
/// time: `try_schedule` already re-checks each pod is still pending under
/// `state`'s lock before placing it (see its doc comment), so running the
/// sweep's RPCs concurrently is safe and just gets pods placed sooner.
/// Best-effort: one pod failing to (re)schedule must not stop the rest of
/// the sweep, so failures are logged rather than propagated.
async fn retry_pending(client: &mut ApiServerClient<Channel>, state: &Arc<Mutex<SchedulerState>>) {
    let retry: Vec<(PodKey, Pod)> = {
        let guard = state.lock().await;
        guard
            .pending
            .iter()
            .map(|(key, pod)| (key.clone(), pod.clone()))
            .collect()
    };

    let tasks = retry.into_iter().map(|((namespace, name), pod)| {
        let mut client = client.clone();
        let state = state.clone();
        Box::pin(
            async move { try_schedule(&mut client, &state, &namespace, &name, &pod, true).await },
        ) as Pin<Box<dyn Future<Output = Result<()>> + Send>>
    });

    run_best_effort(tasks).await;
}

/// Runs every task to completion concurrently. A task's error is logged, not
/// propagated, so one failure can't cancel the others via `JoinSet`'s usual
/// "abort the rest" behavior on drop.
async fn run_best_effort(
    tasks: impl IntoIterator<Item = Pin<Box<dyn Future<Output = Result<()>> + Send>>>,
) {
    let mut set = tokio::task::JoinSet::new();
    for task in tasks {
        set.spawn(task);
    }

    while let Some(result) = set.join_next().await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(err)) => println!("retry_pending: a pod retry failed: {err:#}"),
            Err(join_err) => println!("retry_pending: a pod retry task panicked: {join_err}"),
        }
    }
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
                retry_pending(&mut client, &state).await;
            }
            continue;
        }

        if !pod_detail.node_name.is_empty() {
            let confirmed_not_ready = {
                let guard = state.lock().await;
                guard
                    .scheduler
                    .is_confirmed_not_ready(&pod_detail.node_name)
            };

            if confirmed_not_ready {
                let limits = pod.limits.unwrap_or_default();
                reschedule_one_orphaned_pod(
                    &mut client,
                    &state,
                    &pod_detail.node_name,
                    &namespace,
                    &pod.name,
                    limits,
                )
                .await?;
                continue;
            }

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
    async fn run_best_effort_runs_every_task_even_after_one_fails() {
        let second_ran = Arc::new(AtomicBool::new(false));
        let second_ran_clone = second_ran.clone();

        run_best_effort(vec![
            Box::pin(async { Err(anyhow::anyhow!("boom")) })
                as std::pin::Pin<Box<dyn Future<Output = Result<()>> + Send>>,
            Box::pin(async move {
                second_ran_clone.store(true, Ordering::SeqCst);
                Ok(())
            }),
        ])
        .await;

        assert!(
            second_ran.load(Ordering::SeqCst),
            "a failed task must not stop the other tasks in the same sweep from running"
        );
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
