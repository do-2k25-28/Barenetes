use std::pin::Pin;
use std::sync::Arc;

use proto::api::v1::api_server_server::ApiServer;
use proto::api::v1::{
    AssignPodRequest, AssignPodResponse, CreatePodRequest, CreatePodResponse, DeletePodRequest,
    DeletePodResponse, GetNodeRequest, GetNodeResponse, GetPodRequest, GetPodResponse,
    ListNodesRequest, ListNodesResponse, ListPodsRequest, ListPodsResponse,
    UpdateNodeStatusRequest, UpdateNodeStatusResponse, UpdatePodStatusRequest,
    UpdatePodStatusResponse, WatchDesiredStateEvent, WatchDesiredStateRequest, WatchNodeEvent,
    WatchNodesRequest, WatchPodEvent, WatchPodsRequest,
};
use tokio_stream::Stream;
use tonic::{Request, Response, Status};

use crate::store::Store;

/// Stream types for the 3 server-streaming RPCs.
pub type PodEventStream =
    Pin<Box<dyn Stream<Item = Result<WatchPodEvent, Status>> + Send + 'static>>;
pub type NodeEventStream =
    Pin<Box<dyn Stream<Item = Result<WatchNodeEvent, Status>> + Send + 'static>>;
pub type DesiredStateEventStream =
    Pin<Box<dyn Stream<Item = Result<WatchDesiredStateEvent, Status>> + Send + 'static>>;

pub struct ApiService {
    pub store: Arc<Store>,
}

#[tonic::async_trait]
impl ApiServer for ApiService {
    // --- CLI → API
    async fn create_pod(
        &self,
        request: Request<CreatePodRequest>,
    ) -> Result<Response<CreatePodResponse>, Status> {
        self.create_pod_impl(request).await
    }
    async fn delete_pod(
        &self,
        request: Request<DeletePodRequest>,
    ) -> Result<Response<DeletePodResponse>, Status> {
        self.delete_pod_impl(request).await
    }
    async fn get_pod(
        &self,
        request: Request<GetPodRequest>,
    ) -> Result<Response<GetPodResponse>, Status> {
        self.get_pod_impl(request).await
    }
    async fn list_pods(
        &self,
        request: Request<ListPodsRequest>,
    ) -> Result<Response<ListPodsResponse>, Status> {
        self.list_pods_impl(request).await
    }
    async fn get_node(
        &self,
        request: Request<GetNodeRequest>,
    ) -> Result<Response<GetNodeResponse>, Status> {
        self.get_node_impl(request).await
    }
    async fn list_nodes(
        &self,
        request: Request<ListNodesRequest>,
    ) -> Result<Response<ListNodesResponse>, Status> {
        self.list_nodes_impl(request).await
    }

    // --- scheduler → API
    type WatchPodsStream = PodEventStream;
    async fn watch_pods(
        &self,
        request: Request<WatchPodsRequest>,
    ) -> Result<Response<Self::WatchPodsStream>, Status> {
        self.watch_pods_impl(request).await
    }
    type WatchNodesStream = NodeEventStream;
    async fn watch_nodes(
        &self,
        request: Request<WatchNodesRequest>,
    ) -> Result<Response<Self::WatchNodesStream>, Status> {
        self.watch_nodes_impl(request).await
    }
    async fn assign_pod(
        &self,
        request: Request<AssignPodRequest>,
    ) -> Result<Response<AssignPodResponse>, Status> {
        self.assign_pod_impl(request).await
    }

    // --- agent → API
    async fn update_pod_status(
        &self,
        request: Request<UpdatePodStatusRequest>,
    ) -> Result<Response<UpdatePodStatusResponse>, Status> {
        self.update_pod_status_impl(request).await
    }
    async fn update_node_status(
        &self,
        request: Request<UpdateNodeStatusRequest>,
    ) -> Result<Response<UpdateNodeStatusResponse>, Status> {
        self.update_node_status_impl(request).await
    }
    type WatchDesiredStateStream = DesiredStateEventStream;
    async fn watch_desired_state(
        &self,
        request: Request<WatchDesiredStateRequest>,
    ) -> Result<Response<Self::WatchDesiredStateStream>, Status> {
        self.watch_desired_state_impl(request).await
    }
}
