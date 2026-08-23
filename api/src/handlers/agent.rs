/// Agent-facing status reports and desired-state watch.
use proto::api::v1::{
    UpdateNodeStatusRequest, UpdateNodeStatusResponse, UpdatePodStatusRequest,
    UpdatePodStatusResponse, WatchDesiredStateRequest, WatchNodeEvent,
};
use proto::shared::v1::EventType;
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
        request: Request<UpdateNodeStatusRequest>,
    ) -> Result<Response<UpdateNodeStatusResponse>, Status> {
        let req = request.into_inner();
        let node = req
            .node
            .ok_or_else(|| crate::errors::missing_node("<unknown>"))?;

        let newly_registered = self.store.upsert_node(node.clone()).await;
        self.store.publish_node_event(WatchNodeEvent {
            event_type: if newly_registered {
                EventType::Added
            } else {
                EventType::Modified
            } as i32,
            node: Some(node),
        });

        Ok(Response::new(UpdateNodeStatusResponse {}))
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

#[cfg(test)]
mod tests {
    use proto::shared::v1::NodeStatus;
    use tonic::Code;

    use crate::test_support::{self, node};

    use super::*;

    fn update_node_status_request(
        name: &str,
        status: NodeStatus,
    ) -> Request<UpdateNodeStatusRequest> {
        Request::new(UpdateNodeStatusRequest {
            node: Some(node(name, status)),
        })
    }

    #[tokio::test]
    async fn test_update_node_status_first_seen_publishes_added() {
        let service = test_support::service();
        let mut events = service.store.subscribe_node_events();

        service
            .update_node_status_impl(update_node_status_request("node-1", NodeStatus::Ready))
            .await
            .expect("update_node_status with a node should succeed");

        let event = events
            .try_recv()
            .expect("a first-seen node should publish an event");
        assert_eq!(event.event_type, EventType::Added as i32);
        assert_eq!(event.node, Some(node("node-1", NodeStatus::Ready)));
    }

    #[tokio::test]
    async fn test_update_node_status_known_node_publishes_modified() {
        let service = test_support::service();
        let mut events = service.store.subscribe_node_events();

        service
            .update_node_status_impl(update_node_status_request("node-1", NodeStatus::Ready))
            .await
            .unwrap();
        let _ = events.try_recv(); // drain the ADDED event from the first call

        service
            .update_node_status_impl(update_node_status_request("node-1", NodeStatus::NotReady))
            .await
            .expect("update_node_status for a known node should succeed");

        let event = events
            .try_recv()
            .expect("a known-node update should publish an event");
        assert_eq!(event.event_type, EventType::Modified as i32);
        assert_eq!(event.node, Some(node("node-1", NodeStatus::NotReady)));
        assert_eq!(
            service.store.get_node("node-1").await.map(|n| n.status),
            Some(NodeStatus::NotReady as i32)
        );
    }

    #[tokio::test]
    async fn test_update_node_status_missing_node_rejected() {
        let service = test_support::service();

        let err = service
            .update_node_status_impl(Request::new(UpdateNodeStatusRequest { node: None }))
            .await
            .expect_err("a request without a node should be rejected");

        assert_eq!(err.code(), Code::InvalidArgument);
    }
}
