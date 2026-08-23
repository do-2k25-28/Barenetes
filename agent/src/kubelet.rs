use std::collections::HashSet;
use std::time::Duration;

use proto::agent::v1::kubelet_server::Kubelet;
use proto::agent::v1::{ApplyPodRequest, ApplyPodResponse, DeletePodRequest, DeletePodResponse};
use proto::cni::v1::{NetworkRef, WorkloadRef};
use tonic::{Request, Response, Status};

use crate::cni::Cni;
use crate::containerd::Containerd;

const DEFAULT_NAMESPACE: &str = "default";
const DEFAULT_INTERFACE: &str = "eth0";
/// Single tenant network for now, every pod lands on this vlan.
const DEFAULT_VLAN_ID: u32 = 1;
const DEFAULT_GRACE_PERIOD: Duration = Duration::from_secs(30);
/// Rolling back must not stall the failing call for the whole grace period.
const ROLLBACK_GRACE_PERIOD: Duration = Duration::from_secs(5);

pub struct KubeletService {
    containerd: Containerd,
    cni: Cni,
}

impl KubeletService {
    pub fn new(containerd: Containerd, cni: Cni) -> Self {
        Self { containerd, cni }
    }

    /// Undo everything `apply_pod` created before failing.
    async fn rollback(&self, pod_id: &str, attached: Vec<AttachedContainer>) {
        // Delete the pod
        self.containerd
            .remove_pod(pod_id, ROLLBACK_GRACE_PERIOD, true)
            .await
            .map_err(|error| eprintln!("agent: failed to roll back containers of {pod_id}: {error}"))
            .ok();

        // Delete every network attached to the pod
        for entry in attached.iter().rev() {
            self.cni
                .delete_network(entry.workload.clone(), entry.network.clone())
                .await
                .map_err(|error| eprintln!(
                    "agent: failed to roll back network {} for {}/{}: {error}",
                    entry.network.network_name,
                    entry.workload.workload_name,
                    entry.workload.instance_name
                ))
                .ok();
        }
    }

    async fn create_containers(
        &self,
        containers: &[proto::shared::v1::Container],
        pod_id: &str,
        namespace: &str,
        attached: &mut Vec<AttachedContainer>,
    ) -> Result<(), Status> {

        let workload = WorkloadRef {
            workload_name: pod_id.to_string(),
            instance_name: namespace.to_string(),
        };

        for container in containers {
            let network = NetworkRef {
                network_name: container.name.clone(),
                vlan_id: DEFAULT_VLAN_ID,
            };

            let pid = self.containerd.run_container(pod_id, container).await?;
            attached.push(AttachedContainer {
                workload: workload.clone(),
                network: network.clone(),
            });

            println!("Workload: {:?} | Network: {:?} | PID: {:?}", workload, network, pid);

            // Attach the running container to its tenant network.
            self.cni
                .add_network(workload.clone(), network.clone(), pid, DEFAULT_INTERFACE)
                .await?;
        }
        Ok(())
    }
}

#[tonic::async_trait]
impl Kubelet for KubeletService {
    /// Bring the pod to its desired state. Containers of the pod are removed
    /// and recreated, so applying the same pod twice is safe.
    async fn apply_pod(
        &self,
        request: Request<ApplyPodRequest>,
    ) -> Result<Response<ApplyPodResponse>, Status> {
        let pod = request
            .into_inner()
            .pod
            .ok_or_else(|| Status::invalid_argument("missing pod"))?;
        let spec = pod.spec.unwrap_or_default();
        let name = pod
            .pod
            .map(|p| p.name)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| Status::invalid_argument("missing pod name"))?;

        // Container names end up in the container ids, so they must be set and unique
        let mut names = HashSet::new();
        for container in &spec.containers {
            if container.name.is_empty() {
                return Err(Status::invalid_argument("missing container name"));
            }
            if !names.insert(&container.name) {
                return Err(Status::invalid_argument(format!(
                    "duplicate container name {}",
                    container.name
                )));
            }
        }

        let namespace = match spec.namespace.is_empty() {
            true => DEFAULT_NAMESPACE,
            false => spec.namespace.as_str(),
        };
        let pod_id = pod_id(namespace, &name);

        println!("Applying pod {pod_id}");

        // Deleting existing containers of the pod before creating the new ones
        self.containerd
            .remove_pod(&pod_id, DEFAULT_GRACE_PERIOD, true)
            .await?;

        let mut attached = Vec::new();
        let outcome = self
            .create_containers(&spec.containers, &pod_id, namespace, &mut attached)
            .await;
        if outcome.is_err() {
            self.rollback(&pod_id, attached).await;
            return outcome.map(|_| Response::new(ApplyPodResponse { pod_id }));
        }

        Ok(Response::new(ApplyPodResponse { pod_id }))
    }

    async fn delete_pod(
        &self,
        request: Request<DeletePodRequest>,
    ) -> Result<Response<DeletePodResponse>, Status> {
        let request = request.into_inner();
        if request.pod_id.is_empty() {
            return Err(Status::invalid_argument("missing pod id"));
        }

        // Time before the container get killed for good (first then SIGTERM to container, then
        // after grace period send SIGKILL)
        let grace_period = request
            .grace_period_seconds
            .map(|seconds| Duration::from_secs(seconds.into()))
            .unwrap_or(DEFAULT_GRACE_PERIOD);

        println!("Deleting pod {}", request.pod_id);

        self.containerd
            .remove_pod(&request.pod_id, grace_period, request.force)
            .await?;

        Ok(Response::new(DeletePodResponse { success: true }))
    }
}

/// Networks attached by an in-flight `apply_pod`, kept around to be undone on failure.
struct AttachedContainer {
    workload: WorkloadRef,
    network: NetworkRef,
}

/// `DeletePodRequest` carries no namespace, only a `pod_id`, so the namespace is
/// encoded into the id returned by `apply_pod` instead of travelling next to it.
fn pod_id(namespace: &str, name: &str) -> String {
    format!("{namespace}-{name}")
}
