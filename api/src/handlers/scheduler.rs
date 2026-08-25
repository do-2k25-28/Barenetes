/// Scheduler-facing watches and placement write-back.
use proto::api::v1::{AssignPodRequest, AssignPodResponse, WatchNodesRequest, WatchPodsRequest};
use tonic::{Request, Response, Status};

use crate::service::{ApiService, NodeEventStream, PodEventStream};

impl ApiService {
    pub async fn watch_pods_impl(
        &self,
        _request: Request<WatchPodsRequest>,
    ) -> Result<Response<PodEventStream>, Status> {
        let receiver = self.store.subscribe_pod_events();

        Ok(Response::new(crate::service::broadcast_to_stream(
            receiver,
            "watch_pods",
        )))
    }

    pub async fn watch_nodes_impl(
        &self,
        _request: Request<WatchNodesRequest>,
    ) -> Result<Response<NodeEventStream>, Status> {
        let receiver = self.store.subscribe_node_events();

        Ok(Response::new(crate::service::broadcast_to_stream(
            receiver,
            "watch_nodes",
        )))
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

#[cfg(test)]
mod tests {
    use proto::api::v1::{WatchNodeEvent, WatchPodEvent};
    use proto::shared::v1::{EventType, NodeStatus};
    use tokio_stream::StreamExt;

    use crate::test_support;

    use super::*;

    #[tokio::test]
    async fn test_watch_pods_receives_published_event() {
        let service = test_support::service();
        let mut stream = service
            .watch_pods_impl(Request::new(WatchPodsRequest {}))
            .await
            .unwrap()
            .into_inner();

        let pod = test_support::pod_detail("default", "my-pod");
        service.store.publish_pod_event(WatchPodEvent {
            event_type: EventType::Added as i32,
            pod: Some(pod.clone()),
        });

        let event = stream
            .next()
            .await
            .expect("stream ended")
            .expect("event should not be an error");

        assert_eq!(event.pod, Some(pod));
        assert_eq!(event.event_type, EventType::Added as i32);
    }

    #[tokio::test]
    async fn test_watch_nodes_receives_published_event() {
        let service = test_support::service();
        let mut stream = service
            .watch_nodes_impl(Request::new(WatchNodesRequest {}))
            .await
            .unwrap()
            .into_inner();

        let node = test_support::node("node-1", NodeStatus::Ready);
        service.store.publish_node_event(WatchNodeEvent {
            event_type: EventType::Added as i32,
            node: Some(node.clone()),
        });

        let event = stream
            .next()
            .await
            .expect("stream ended")
            .expect("event should not be an error");

        assert_eq!(event.node, Some(node));
        assert_eq!(event.event_type, EventType::Added as i32);
    }

    #[tokio::test]
    async fn test_watch_nodes_receives_added_then_modified() {
        let service = test_support::service();
        let mut stream = service
            .watch_nodes_impl(Request::new(WatchNodesRequest {}))
            .await
            .unwrap()
            .into_inner();

        service
            .store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::Ready))
            .await;
        service
            .store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::NotReady))
            .await;

        let first = stream.next().await.unwrap().unwrap();
        let second = stream.next().await.unwrap().unwrap();

        assert_eq!(first.event_type, EventType::Added as i32);
        assert_eq!(second.event_type, EventType::Modified as i32);
        assert_eq!(second.node.unwrap().status, NodeStatus::NotReady as i32);
    }
}
