//! Registers this node with the API server and streams its desired state,
//! translating `RUN`/`STOP` events into `ApplyPod`/`DeletePod` calls against
//! the local Kubelet service, so a pod the scheduler assigns here actually
//! starts running instead of just sitting in the API server's store.

use std::time::Duration;

use anyhow::{Context, Result};
use proto::agent::v1::kubelet_client::KubeletClient;
use proto::agent::v1::{ApplyPodRequest, DeletePodRequest};
use proto::api::v1::api_server_client::ApiServerClient;
use proto::api::v1::{
    UpdateNodeStatusRequest, WatchDesiredStateRequest, watch_desired_state_event,
};
use proto::shared::v1::{Node, NodeStatus, Resources};
use tokio_stream::StreamExt;
use tonic::transport::{Channel, Endpoint};

use crate::kubelet::resolve_pod_id;

const RETRY_DELAY: Duration = Duration::from_secs(5);

/// Runs forever: register, watch, react to events; on any error (API server
/// unreachable, stream dropped, ...) log it and retry after `RETRY_DELAY`
/// rather than taking the whole agent process down with it.
pub async fn run(server_addr: String, node_name: String, kubelet_addr: String) -> Result<()> {
    // Lazy channels: the actual TCP connect happens on first RPC, not here, so
    // this doesn't race the Kubelet gRPC server's own bind in main().
    let api_channel = Endpoint::from_shared(server_addr)
        .context("invalid API server address")?
        .connect_lazy();
    let kubelet_channel = Endpoint::from_shared(format!("http://{kubelet_addr}"))
        .context("invalid local kubelet address")?
        .connect_lazy();

    let mut api = ApiServerClient::new(api_channel);
    let mut kubelet = KubeletClient::new(kubelet_channel);

    loop {
        if let Err(error) = register_and_watch(&mut api, &mut kubelet, &node_name).await {
            eprintln!("agent: desired-state watch failed, retrying in {RETRY_DELAY:?}: {error:#}");
        }
        tokio::time::sleep(RETRY_DELAY).await;
    }
}

async fn register_and_watch(
    api: &mut ApiServerClient<Channel>,
    kubelet: &mut KubeletClient<Channel>,
    node_name: &str,
) -> Result<()> {
    // Re-read on every attempt rather than once at startup: it's two cheap
    // reads, it keeps a transient failure (an unreadable /proc/meminfo in a
    // locked-down container) on the retry path where it gets logged, and it
    // picks up capacity that changed while we were disconnected.
    let capacity = detect_capacity().context("failed to detect node capacity")?;

    api.update_node_status(UpdateNodeStatusRequest {
        node: Some(Node {
            name: node_name.to_string(),
            // Discarded and replaced by the store either way: the API is the
            // sole writer of node status, the agent only owns capacity.
            status: NodeStatus::Ready as i32,
            capacity: Some(capacity),
            allocatable: Some(capacity),
        }),
    })
    .await
    .context("failed to register node")?;

    let mut events = api
        .watch_desired_state(WatchDesiredStateRequest {
            node_name: node_name.to_string(),
        })
        .await
        .context("failed to open desired-state watch")?
        .into_inner();

    println!("agent: watching desired state for node {node_name}");

    while let Some(event) = events.next().await {
        let event = event.context("desired-state stream error")?;
        match event.action() {
            watch_desired_state_event::Action::Run => {
                let Some(pod) = event.pod else { continue };
                if let Err(status) = kubelet.apply_pod(ApplyPodRequest { pod: Some(pod) }).await {
                    eprintln!("agent: failed to apply pod from desired state: {status}");
                }
            }
            watch_desired_state_event::Action::Stop => {
                let Some(pod) = event.pod else { continue };
                let namespace = pod
                    .spec
                    .as_ref()
                    .map(|s| s.namespace.as_str())
                    .unwrap_or("");
                let name = pod.pod.as_ref().map(|p| p.name.as_str()).unwrap_or("");
                let pod_id = resolve_pod_id(namespace, name);
                if let Err(status) = kubelet
                    .delete_pod(DeletePodRequest {
                        pod_id,
                        grace_period_seconds: None,
                        force: false,
                    })
                    .await
                {
                    eprintln!("agent: failed to delete pod from desired state: {status}");
                }
            }
            // No reconciliation against whatever's already running for now:
            // just a marker that the opening snapshot is done.
            watch_desired_state_event::Action::Synced => {
                println!("agent: synced with API server's desired state for node {node_name}");
            }
        }
    }

    Ok(())
}

/// Reads the node name from `BARENETES_NODE_NAME` if set, else derives one
/// from the system hostname.
pub(crate) fn detect_node_name() -> Result<String> {
    if let Ok(name) = std::env::var("BARENETES_NODE_NAME") {
        return Ok(name);
    }
    let raw = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .context("failed to read hostname from /proc/sys/kernel/hostname")?;
    let name = sanitize_node_name(raw.trim());
    if name.is_empty() {
        anyhow::bail!(
            "detected hostname {raw:?} sanitizes to an empty node name; \
             set BARENETES_NODE_NAME explicitly"
        );
    }
    Ok(name)
}

/// Reduces a hostname to a valid DNS-1123 node name: first label only
/// (everything before the first '.', so a FQDN's domain suffix is dropped),
/// lowercased, non-alphanumeric characters collapsed to '-', and leading/
/// trailing '-' trimmed (the validation rule requires alnum on both ends).
fn sanitize_node_name(hostname: &str) -> String {
    let first_label = hostname.split('.').next().unwrap_or(hostname);
    let normalized: String = first_label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    normalized.trim_matches('-').to_string()
}

/// Real, current machine resources: CPU as milli-cpu (1000 per core) from the
/// number of available cores, memory in MB parsed from /proc/meminfo's
/// MemTotal line. `allocatable` is reported equal to `capacity` in `run()` --
/// the agent doesn't yet track how much of it running pods have already
/// claimed.
fn detect_capacity() -> Result<Resources> {
    let cores = std::thread::available_parallelism()
        .context("failed to detect available CPU cores")?
        .get();
    let meminfo =
        std::fs::read_to_string("/proc/meminfo").context("failed to read /proc/meminfo")?;
    let mem_total_kb = parse_mem_total_kb(&meminfo)?;
    Ok(Resources {
        cpu: (cores as i32) * 1000,
        memory: (mem_total_kb / 1024) as i32,
    })
}

fn parse_mem_total_kb(meminfo: &str) -> Result<u64> {
    meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kb| kb.parse::<u64>().ok())
        .context("MemTotal not found in /proc/meminfo")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_node_name_lowercases_and_drops_domain_suffix() {
        assert_eq!(sanitize_node_name("Debian13.localdomain"), "debian13");
    }

    #[test]
    fn sanitize_node_name_collapses_invalid_characters() {
        assert_eq!(sanitize_node_name("my_host_01"), "my-host-01");
    }

    #[test]
    fn sanitize_node_name_trims_leading_and_trailing_dashes() {
        assert_eq!(sanitize_node_name("_worker_"), "worker");
    }

    #[test]
    fn sanitize_node_name_of_only_invalid_characters_is_empty() {
        assert_eq!(sanitize_node_name("..."), "");
    }

    #[test]
    fn parse_mem_total_kb_reads_the_first_matching_line() {
        let meminfo = "MemTotal:       16384000 kB\nMemFree:         1000000 kB\n";
        assert_eq!(parse_mem_total_kb(meminfo).unwrap(), 16_384_000);
    }

    #[test]
    fn parse_mem_total_kb_errors_when_missing() {
        assert!(parse_mem_total_kb("garbage\nmore garbage\n").is_err());
    }

    #[test]
    fn detect_capacity_reports_a_plausible_machine() {
        let resources = detect_capacity().unwrap();
        assert!(
            resources.cpu >= 1000,
            "expected at least one core's worth of mCPU"
        );
        assert!(resources.memory > 0, "expected non-zero memory");
    }
}
