use std::collections::HashSet;
use std::time::Duration;

use proto::agent::v1::kubelet_server::Kubelet;
use proto::agent::v1::{ApplyPodRequest, ApplyPodResponse, DeletePodRequest, DeletePodResponse};
use proto::cni::v1::{NetworkRef, WorkloadRef};
use proto::tls_identity::Role;
use tonic::{Request, Response, Status};

use crate::cgroup;
use crate::cni::Cni;
use crate::containerd::Containerd;
use crate::vlan::VlanAllocations;

const DEFAULT_NAMESPACE: &str = "default";
const DEFAULT_INTERFACE: &str = "eth0";
const DEFAULT_GRACE_PERIOD: Duration = Duration::from_secs(30);
/// Rolling back must not stall the failing call for the whole grace period.
const ROLLBACK_GRACE_PERIOD: Duration = Duration::from_secs(5);

/// Rejects `request` unless its mTLS peer is authorized for `Role::Node` (a
/// no-op in plaintext mode, see [`proto::tls_identity::check_role`]). Every
/// cluster certificate authenticates against the same CA, so without this
/// gate any cert -- a CLI cert, say -- could call `ApplyPod`/`DeletePod`
/// directly on a worker's agent, bypassing the API server and scheduler
/// entirely. The only legitimate caller is this node's own agent process,
/// looping `desired_state::run`'s watch back into its local Kubelet server.
fn require_node_role<T>(request: &Request<T>) -> Result<(), Status> {
    proto::tls_identity::check_role(&proto::tls_identity::peer(request), Role::Node)
}

pub struct KubeletService {
    containerd: Containerd,
    cni: Cni,
    vlans: VlanAllocations,
}

impl KubeletService {
    pub fn new(containerd: Containerd, cni: Cni, vlans: VlanAllocations) -> Self {
        Self {
            containerd,
            cni,
            vlans,
        }
    }

    /// Undo everything `apply_pod` created before failing.
    async fn rollback(
        &self,
        pod_id: &str,
        namespace: &str,
        containers: &[proto::shared::v1::Container],
        vlan: u32,
    ) {
        let workload = WorkloadRef {
            workload_name: pod_id.to_string(),
            instance_name: namespace.to_string(),
        };

        // Detach every network of the pod, also the ones that were never
        // created in this round, so no stale record survives the rollback.
        for container in containers {
            self.cni
                .delete_network(
                    workload.clone(),
                    NetworkRef {
                        network_name: container.name.clone(),
                        vlan_id: vlan,
                    },
                )
                .await
                .map_err(|error| {
                    eprintln!(
                        "agent: failed to roll back network {} for {pod_id}: {error}",
                        container.name
                    )
                })
                .ok();
        }

        // Delete the pod
        self.containerd
            .remove_pod(pod_id, ROLLBACK_GRACE_PERIOD, true)
            .await
            .map_err(|error| {
                eprintln!("agent: failed to roll back containers of {pod_id}: {error}")
            })
            .ok();

        if let Err(error) = cgroup::remove_pod(pod_id) {
            eprintln!("agent: failed to roll back the cgroup of {pod_id}: {error}");
        }

        // The pod is gone entirely, so its VLAN can be reused later.
        if let Err(error) = self.vlans.release(pod_id) {
            eprintln!("agent: failed to release VLAN of {pod_id}: {error}");
        }
    }

    async fn create_containers(
        &self,
        containers: &[proto::shared::v1::Container],
        pod_id: &str,
        namespace: &str,
        vlan: u32,
    ) -> Result<(), Status> {
        let workload = WorkloadRef {
            workload_name: pod_id.to_string(),
            instance_name: namespace.to_string(),
        };

        for container in containers {
            let network = NetworkRef {
                network_name: container.name.clone(),
                vlan_id: vlan,
            };

            let pid = self.containerd.run_container(pod_id, container).await?;

            println!(
                "Workload: {:?} | Network: {:?} | PID: {:?}",
                workload, network, pid
            );

            // Attach the running container to its isolated tenant network,
            // forwarding whatever host<->container port mappings it declared.
            self.cni
                .add_network(
                    workload.clone(),
                    network.clone(),
                    pid,
                    DEFAULT_INTERFACE,
                    container.ports.clone(),
                )
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
        require_node_role(&request)?;
        let pod = request
            .into_inner()
            .pod
            .ok_or_else(|| Status::invalid_argument("missing pod"))?;
        let spec = pod.spec.unwrap_or_default();
        let core = pod.pod.unwrap_or_default();
        if core.name.is_empty() {
            return Err(Status::invalid_argument("missing pod name"));
        }
        let pod_name = core.name;
        // Limits are declared for the whole pod and enforced on the pod cgroup
        // every container of the pod runs under, so the containers share one
        // budget rather than each getting a full copy of it.
        let limits = core.limits;

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
            false => {
                if spec.namespace.contains('.') {
                    return Err(Status::invalid_argument("namespace must not contain '.'"));
                }
                spec.namespace.as_str()
            }
        };
        let pod_id = pod_id(namespace, &pod_name);

        println!("Applying pod {pod_id}");

        // Every container of the pod shares one isolated VLAN. The mapping is
        // sticky: re-applying the same pod reuses its VLAN so existing network
        // settings keep matching.
        let vlan = self
            .vlans
            .allocate_for(&pod_id)
            .map_err(|error| Status::internal(format!("cannot allocate a vlan: {error}")))?;

        // Deleting existing containers of the pod before creating the new ones
        self.containerd
            .remove_pod(&pod_id, DEFAULT_GRACE_PERIOD, true)
            .await?;

        let outcome = async {
            // The pod is recreated from scratch, and so is its cgroup: a
            // re-apply must not keep enforcing the limits of the last one.
            if let Err(error) = cgroup::remove_pod(&pod_id) {
                eprintln!("agent: failed to clear the cgroup of {pod_id}: {error}");
            }
            cgroup::create_pod(&pod_id, limits.as_ref()).map_err(|error| {
                Status::internal(format!("cannot set up the cgroup of {pod_id}: {error}"))
            })?;

            self.create_containers(&spec.containers, &pod_id, namespace, vlan)
                .await
        }
        .await;
        if outcome.is_err() {
            self.rollback(&pod_id, namespace, &spec.containers, vlan)
                .await;
            return outcome.map(|_| Response::new(ApplyPodResponse { pod_id }));
        }

        Ok(Response::new(ApplyPodResponse { pod_id }))
    }

    async fn delete_pod(
        &self,
        request: Request<DeletePodRequest>,
    ) -> Result<Response<DeletePodResponse>, Status> {
        require_node_role(&request)?;
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

        // Detaching every container network before deleting them
        let namespace = namespace_of(&request.pod_id);
        let vlan = self.vlans.vlan_of(&request.pod_id).unwrap_or(0);
        let prefix = format!("{}-", request.pod_id);
        for id in self.containerd.pod_container_ids(&request.pod_id).await? {
            let Some(container) = id.strip_prefix(&prefix) else {
                eprintln!("agent: skipping unrecognised container id {id}");
                continue;
            };
            self.cni
                .delete_network(
                    WorkloadRef {
                        workload_name: request.pod_id.clone(),
                        instance_name: namespace.to_string(),
                    },
                    NetworkRef {
                        network_name: container.to_string(),
                        vlan_id: vlan,
                    },
                )
                .await
                .map_err(|error| {
                    eprintln!(
                        "agent: failed to detach network {container} of {}: {error}",
                        request.pod_id
                    )
                })
                .ok();
        }

        self.containerd
            .remove_pod(&request.pod_id, grace_period, request.force)
            .await?;

        if let Err(error) = cgroup::remove_pod(&request.pod_id) {
            eprintln!(
                "agent: failed to remove the cgroup of {}: {error}",
                request.pod_id
            );
        }

        // The pod is gone, so its VLAN can be reused by a future pod.
        if let Err(error) = self.vlans.release(&request.pod_id) {
            eprintln!(
                "agent: failed to release VLAN of {}: {error}",
                request.pod_id
            );
        }

        Ok(Response::new(DeletePodResponse { success: true }))
    }
}

/// `DeletePodRequest` carries no namespace, only a `pod_id`, so the namespace is
/// encoded into the id returned by `apply_pod` instead of travelling next to it.
fn pod_id(namespace: &str, pod_name: &str) -> String {
    format!("{namespace}.{pod_name}")
}

/// Same id `apply_pod` would compute for a `PodWithSpec`, empty-namespace default
/// included, so a caller building a `DeletePodRequest` from desired state matches
/// whatever id the pod is actually running under.
pub(crate) fn resolve_pod_id(namespace: &str, pod_name: &str) -> String {
    let namespace = if namespace.is_empty() {
        DEFAULT_NAMESPACE
    } else {
        namespace
    };
    pod_id(namespace, pod_name)
}

/// Recovers the namespace encoded at the start of a pod id.
fn namespace_of(pod_id: &str) -> &str {
    pod_id
        .split_once('.')
        .map(|(namespace, _)| namespace)
        .unwrap_or(DEFAULT_NAMESPACE)
}
