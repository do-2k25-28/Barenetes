use std::time::Duration;

use proto::agent::v1::kubelet_server::Kubelet;
use proto::agent::v1::{ApplyPodRequest, ApplyPodResponse, DeletePodRequest, DeletePodResponse};
use tonic::{Request, Response, Status};

use crate::containerd::Containerd;

const DEFAULT_NAMESPACE: &str = "default";
const DEFAULT_GRACE_PERIOD: Duration = Duration::from_secs(30);

pub struct KubeletService {
    containerd: Containerd,
}

impl KubeletService {
    pub fn new(containerd: Containerd) -> Self {
        Self { containerd }
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
        let pod_id = pod_id(&spec.namespace, &name);

        println!("Applying pod {pod_id}");

        // Deleting existing containers of the pod before creating the new containers
        self.containerd
            .remove_pod(&pod_id, DEFAULT_GRACE_PERIOD, true)
            .await?;

        for container in &spec.containers {
            self.containerd.run_container(&pod_id, container).await?;
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

/// `DeletePodRequest` carries no namespace, only a `pod_id`, so the namespace is
/// encoded into the id returned by `apply_pod` instead of travelling next to it.
fn pod_id(namespace: &str, name: &str) -> String {
    let namespace = if namespace.is_empty() {
        DEFAULT_NAMESPACE
    } else {
        namespace
    };

    format!("{namespace}-{name}")
}
