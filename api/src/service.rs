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
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::{Stream, StreamExt};
use tonic::{Request, Response, Status};

use crate::store::Store;

/// Stream types for the 3 server-streaming RPCs.
pub type PodEventStream =
    Pin<Box<dyn Stream<Item = Result<WatchPodEvent, Status>> + Send + 'static>>;
pub type NodeEventStream =
    Pin<Box<dyn Stream<Item = Result<WatchNodeEvent, Status>> + Send + 'static>>;
pub type DesiredStateEventStream =
    Pin<Box<dyn Stream<Item = Result<WatchDesiredStateEvent, Status>> + Send + 'static>>;

/// A lagged receiver (client fell behind and the channel's ring buffer
/// overwrote unread events) surfaces as one `Status::data_loss` item, which ends the RPC
/// since the client's view is now inconsistent and it must reconnect and re-`List*` to
/// resync, rather than keep consuming a stream with a hole in it.
pub(crate) fn broadcast_to_stream<T>(
    receiver: broadcast::Receiver<T>,
    method: &'static str,
) -> Pin<Box<dyn Stream<Item = Result<T, Status>> + Send + 'static>>
where
    T: Clone + Send + 'static,
{
    Box::pin(BroadcastStream::new(receiver).map(move |item| {
        item.map_err(|BroadcastStreamRecvError::Lagged(n)| {
            tracing::warn!(
                method,
                missed = n,
                "watch fell behind; terminating stream with DATA_LOSS"
            );
            Status::data_loss(format!(
                "watch fell behind and missed {n} event(s); reconnect and re-list to resync"
            ))
        })
    }))
}

// `allow(dead_code)` is temporary: no handler reads `self.store` yet since they're all `todo!()`
// stubs. Remove once a track's handlers actually use it.
#[allow(dead_code)]
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
        crate::telemetry::traced("create_pod", self.create_pod_impl(request)).await
    }
    async fn delete_pod(
        &self,
        request: Request<DeletePodRequest>,
    ) -> Result<Response<DeletePodResponse>, Status> {
        crate::telemetry::traced("delete_pod", self.delete_pod_impl(request)).await
    }
    async fn get_pod(
        &self,
        request: Request<GetPodRequest>,
    ) -> Result<Response<GetPodResponse>, Status> {
        crate::telemetry::traced("get_pod", self.get_pod_impl(request)).await
    }
    async fn list_pods(
        &self,
        request: Request<ListPodsRequest>,
    ) -> Result<Response<ListPodsResponse>, Status> {
        crate::telemetry::traced("list_pods", self.list_pods_impl(request)).await
    }
    async fn get_node(
        &self,
        request: Request<GetNodeRequest>,
    ) -> Result<Response<GetNodeResponse>, Status> {
        crate::telemetry::traced("get_node", self.get_node_impl(request)).await
    }
    async fn list_nodes(
        &self,
        request: Request<ListNodesRequest>,
    ) -> Result<Response<ListNodesResponse>, Status> {
        crate::telemetry::traced("list_nodes", self.list_nodes_impl(request)).await
    }

    // --- scheduler → API
    type WatchPodsStream = PodEventStream;
    async fn watch_pods(
        &self,
        request: Request<WatchPodsRequest>,
    ) -> Result<Response<Self::WatchPodsStream>, Status> {
        crate::telemetry::traced("watch_pods", self.watch_pods_impl(request)).await
    }
    type WatchNodesStream = NodeEventStream;
    async fn watch_nodes(
        &self,
        request: Request<WatchNodesRequest>,
    ) -> Result<Response<Self::WatchNodesStream>, Status> {
        crate::telemetry::traced("watch_nodes", self.watch_nodes_impl(request)).await
    }
    async fn assign_pod(
        &self,
        request: Request<AssignPodRequest>,
    ) -> Result<Response<AssignPodResponse>, Status> {
        crate::telemetry::traced("assign_pod", self.assign_pod_impl(request)).await
    }

    // --- agent → API
    async fn update_pod_status(
        &self,
        request: Request<UpdatePodStatusRequest>,
    ) -> Result<Response<UpdatePodStatusResponse>, Status> {
        crate::telemetry::traced("update_pod_status", self.update_pod_status_impl(request)).await
    }
    async fn update_node_status(
        &self,
        request: Request<UpdateNodeStatusRequest>,
    ) -> Result<Response<UpdateNodeStatusResponse>, Status> {
        crate::telemetry::traced("update_node_status", self.update_node_status_impl(request)).await
    }
    type WatchDesiredStateStream = DesiredStateEventStream;
    async fn watch_desired_state(
        &self,
        request: Request<WatchDesiredStateRequest>,
    ) -> Result<Response<Self::WatchDesiredStateStream>, Status> {
        crate::telemetry::traced(
            "watch_desired_state",
            self.watch_desired_state_impl(request),
        )
        .await
    }
}
