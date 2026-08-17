/// Agent-facing status reports and desired-state watch.
use proto::api::v1::{
    UpdateNodeStatusRequest, UpdateNodeStatusResponse, UpdatePodStatusRequest,
    UpdatePodStatusResponse, WatchDesiredStateRequest,
};
use tonic::{Request, Response, Status};

use crate::service::{ApiService, DesiredStateEventStream};

impl ApiService {
    pub async fn update_pod_status_impl(
        &self,
        _request: Request<UpdatePodStatusRequest>,
    ) -> Result<Response<UpdatePodStatusResponse>, Status> {
        todo!("store.upsert_pod with the reported status")
    }

    pub async fn update_node_status_impl(
        &self,
        _request: Request<UpdateNodeStatusRequest>,
    ) -> Result<Response<UpdateNodeStatusResponse>, Status> {
        todo!("store.upsert_node with the reported status")
    }

    pub async fn watch_desired_state_impl(
        &self,
        _request: Request<WatchDesiredStateRequest>,
    ) -> Result<Response<DesiredStateEventStream>, Status> {
        todo!(
            "stream self.store.subscribe_desired_state_events(&request.get_ref().node_name) \
             directly — the subscription is already scoped to that node, no downstream filtering needed"
        )
    }
}
