/// Scheduler-facing watches and placement write-back.
use proto::api::v1::{AssignPodRequest, AssignPodResponse, WatchNodesRequest, WatchPodsRequest};
use tonic::{Request, Response, Status};

use crate::service::{ApiService, NodeEventStream, PodEventStream};

impl ApiService {
    pub async fn watch_pods_impl(
        &self,
        _request: Request<WatchPodsRequest>,
    ) -> Result<Response<PodEventStream>, Status> {
        // TODO: stream self.store.subscribe_pod_events() to the client
        Err(Status::unimplemented("watch_pods is not yet implemented"))
    }

    pub async fn watch_nodes_impl(
        &self,
        _request: Request<WatchNodesRequest>,
    ) -> Result<Response<NodeEventStream>, Status> {
        // TODO: stream self.store.subscribe_node_events() to the client
        Err(Status::unimplemented("watch_nodes is not yet implemented"))
    }

    pub async fn assign_pod_impl(
        &self,
        _request: Request<AssignPodRequest>,
    ) -> Result<Response<AssignPodResponse>, Status> {
        // TODO: record the placement outcome on the pod in self.store, then
        // self.store.publish_desired_state_event(node_name, RUN event) so the agent picks it up
        Err(Status::unimplemented("assign_pod is not yet implemented"))
    }
}
