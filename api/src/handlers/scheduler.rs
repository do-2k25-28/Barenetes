/// Scheduler-facing watches and placement write-back.
use proto::api::v1::{
    AssignPodRequest, AssignPodResponse, WatchDesiredStateEvent, WatchNodeEvent, WatchNodesRequest,
    WatchPodEvent, WatchPodsRequest, assign_pod_request, watch_desired_state_event,
};
use proto::shared::v1::{EventType, PodStatus};
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};

use crate::service::{ApiService, NodeEventStream, PodEventStream};

impl ApiService {
    pub async fn watch_pods_impl(
        &self,
        _request: Request<WatchPodsRequest>,
    ) -> Result<Response<PodEventStream>, Status> {
        let (snapshot, receiver) = self
            .store
            .subscribe_pod_events_with_snapshot()
            .await
            .map_err(|e| e.to_status())?;

        let opening = tokio_stream::iter(snapshot.into_iter().map(|pod| {
            Ok(WatchPodEvent {
                event_type: EventType::Added as i32,
                pod: Some(pod),
            })
        }));
        let live = crate::service::broadcast_to_stream(receiver, "watch_pods");

        Ok(Response::new(Box::pin(opening.chain(live))))
    }

    pub async fn watch_nodes_impl(
        &self,
        _request: Request<WatchNodesRequest>,
    ) -> Result<Response<NodeEventStream>, Status> {
        let (nodes, receiver) = self
            .store
            .subscribe_node_events_with_snapshot()
            .await
            .map_err(|e| e.to_status())?;

        let opening = tokio_stream::iter(nodes.into_iter().map(|node| {
            Ok(WatchNodeEvent {
                event_type: EventType::Added as i32,
                node: Some(node),
            })
        }));
        let live = crate::service::broadcast_to_stream(receiver, "watch_nodes");

        Ok(Response::new(Box::pin(opening.chain(live))))
    }

    pub async fn assign_pod_impl(
        &self,
        request: Request<AssignPodRequest>,
    ) -> Result<Response<AssignPodResponse>, Status> {
        let AssignPodRequest {
            name,
            namespace,
            outcome,
        } = request.into_inner();
        let outcome =
            outcome.ok_or_else(|| Status::invalid_argument("missing assignment outcome"))?;

        match outcome {
            assign_pod_request::Outcome::NodeName(node_name) => {
                // The agent needs the spec to run the pod, so carry it out of the guard.
                let mut placed = None;
                let found = self
                    .store
                    .update_and_publish_pod(&namespace, &name, EventType::Scheduled, |detail| {
                        detail.node_name = node_name.clone();
                        detail.unschedulable_reason = None;
                        if let Some(core_pod) =
                            detail.core.as_mut().and_then(|core| core.pod.as_mut())
                        {
                            core_pod.status = PodStatus::Pending as i32;
                        }
                        placed = detail.core.clone();
                    })
                    .await
                    .map_err(|e| e.to_status())?;
                if !found {
                    return Err(crate::errors::pod_not_found(&namespace, &name));
                }

                self.store
                    .publish_desired_state_event(
                        &node_name,
                        WatchDesiredStateEvent {
                            action: watch_desired_state_event::Action::Run as i32,
                            pod: placed,
                        },
                    )
                    .await;
            }
            assign_pod_request::Outcome::UnschedulableReason(reason) => {
                // NO_NODE_AVAILABLE is retriable, not terminal: the scheduler retries
                // this pod on every later node event via its `pending` set.
                let found = self
                    .store
                    .update_and_publish_pod(&namespace, &name, EventType::Modified, |detail| {
                        detail.unschedulable_reason = Some(reason);
                        if let Some(core_pod) =
                            detail.core.as_mut().and_then(|core| core.pod.as_mut())
                        {
                            core_pod.status = PodStatus::NoNodeAvailable as i32;
                        }
                    })
                    .await
                    .map_err(|e| e.to_status())?;
                if !found {
                    return Err(crate::errors::pod_not_found(&namespace, &name));
                }
            }
            assign_pod_request::Outcome::OrphanedReason(reason) => {
                // The pod's node went NotReady: nothing vouches for it running there
                // anymore, so drop the stale node assignment instead of leaving the
                // dead node's last-reported status in place.
                let found = self
                    .store
                    .update_and_publish_pod(&namespace, &name, EventType::Modified, |detail| {
                        detail.node_name = String::new();
                        detail.unschedulable_reason = Some(reason);
                        if let Some(core_pod) =
                            detail.core.as_mut().and_then(|core| core.pod.as_mut())
                        {
                            core_pod.status = PodStatus::Unknown as i32;
                        }
                    })
                    .await
                    .map_err(|e| e.to_status())?;
                if !found {
                    return Err(crate::errors::pod_not_found(&namespace, &name));
                }
            }
        }

        Ok(Response::new(AssignPodResponse {}))
    }
}

#[cfg(test)]
mod tests {
    use proto::api::v1::{WatchNodeEvent, WatchPodEvent};
    use proto::shared::v1::{EventType, NodeStatus};
    use tokio_stream::StreamExt;
    use tonic::Code;

    use crate::test_support;

    use super::*;

    fn assign_request(
        namespace: &str,
        name: &str,
        outcome: Option<assign_pod_request::Outcome>,
    ) -> Request<AssignPodRequest> {
        Request::new(AssignPodRequest {
            name: name.to_string(),
            namespace: namespace.to_string(),
            outcome,
        })
    }

    fn placed_on(node_name: &str) -> Option<assign_pod_request::Outcome> {
        Some(assign_pod_request::Outcome::NodeName(node_name.to_string()))
    }

    fn unschedulable(reason: &str) -> Option<assign_pod_request::Outcome> {
        Some(assign_pod_request::Outcome::UnschedulableReason(
            reason.to_string(),
        ))
    }

    fn orphaned(reason: &str) -> Option<assign_pod_request::Outcome> {
        Some(assign_pod_request::Outcome::OrphanedReason(
            reason.to_string(),
        ))
    }

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
    async fn test_watch_pods_replays_pending_pods_created_before_subscribing() {
        let service = test_support::service();
        let pod = test_support::pod_detail("default", "already-pending");
        service.store.upsert_pod(pod.clone()).await.unwrap();

        let mut stream = service
            .watch_pods_impl(Request::new(WatchPodsRequest {}))
            .await
            .unwrap()
            .into_inner();

        let event = stream
            .next()
            .await
            .expect("stream ended")
            .expect("event should not be an error");

        assert_eq!(event.pod, Some(pod));
        assert_eq!(event.event_type, EventType::Added as i32);
    }

    #[tokio::test]
    async fn test_watch_pods_snapshot_includes_already_scheduled_pods_before_pending_ones() {
        let service = test_support::service();
        let mut scheduled = test_support::pod_detail("default", "already-scheduled");
        scheduled.node_name = "node-a".to_string();
        service.store.upsert_pod(scheduled.clone()).await.unwrap();
        let pending = test_support::pod_detail("default", "pending");
        service.store.upsert_pod(pending.clone()).await.unwrap();

        let mut stream = service
            .watch_pods_impl(Request::new(WatchPodsRequest {}))
            .await
            .unwrap()
            .into_inner();

        let first = stream
            .next()
            .await
            .expect("stream ended")
            .expect("event should not be an error");
        assert_eq!(first.pod, Some(scheduled));

        let second = stream
            .next()
            .await
            .expect("stream ended")
            .expect("event should not be an error");
        assert_eq!(second.pod, Some(pending));
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
    async fn test_watch_nodes_replays_existing_nodes_before_subscribing() {
        let service = test_support::service();
        let node = test_support::node("node-1", NodeStatus::Ready);
        service
            .store
            .upsert_and_publish_node(node.clone())
            .await
            .unwrap();

        let mut stream = service
            .watch_nodes_impl(Request::new(WatchNodesRequest {}))
            .await
            .unwrap()
            .into_inner();

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
            .await
            .unwrap();
        service
            .store
            .upsert_and_publish_node(test_support::node("node-1", NodeStatus::NotReady))
            .await
            .unwrap();

        let first = stream.next().await.unwrap().unwrap();
        let second = stream.next().await.unwrap().unwrap();

        assert_eq!(first.event_type, EventType::Added as i32);
        assert_eq!(second.event_type, EventType::Modified as i32);
        assert_eq!(second.node.unwrap().status, NodeStatus::NotReady as i32);
    }

    #[tokio::test]
    async fn test_assign_pod_records_placement_and_publishes_run() {
        let service = test_support::service();
        service
            .store
            .upsert_pod(test_support::pod_detail("default", "my-pod"))
            .await
            .unwrap();

        // Subscribe first: an unwatched node's desired-state events are dropped.
        let mut desired = service.store.subscribe_desired_state_events("node-a").await;
        let mut pod_events = service.store.subscribe_pod_events();

        service
            .assign_pod_impl(assign_request("default", "my-pod", placed_on("node-a")))
            .await
            .expect("assignment should succeed");

        let stored = service
            .store
            .get_pod("default", "my-pod")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.node_name, "node-a");

        let pod_event = pod_events
            .try_recv()
            .expect("a pod event should be published");
        assert_eq!(pod_event.event_type, EventType::Scheduled as i32);

        let run = desired
            .try_recv()
            .expect("node-a should receive a RUN event");
        assert_eq!(run.action, watch_desired_state_event::Action::Run as i32);
        assert_eq!(run.pod, stored.core);
    }

    #[tokio::test]
    async fn test_assign_pod_unschedulable_records_reason() {
        let service = test_support::service();
        service
            .store
            .upsert_pod(test_support::pod_detail("default", "my-pod"))
            .await
            .unwrap();
        let mut pod_events = service.store.subscribe_pod_events();

        service
            .assign_pod_impl(assign_request(
                "default",
                "my-pod",
                unschedulable("no node with enough memory"),
            ))
            .await
            .expect("an unschedulable outcome should still be accepted");

        let stored = service
            .store
            .get_pod("default", "my-pod")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.unschedulable_reason.as_deref(),
            Some("no node with enough memory")
        );
        assert_eq!(stored.node_name, "", "an unplaced pod keeps no node");
        assert_eq!(
            stored.core.unwrap().pod.unwrap().status,
            PodStatus::NoNodeAvailable as i32
        );

        let pod_event = pod_events
            .try_recv()
            .expect("a pod event should be published");
        assert_eq!(pod_event.event_type, EventType::Modified as i32);
    }

    #[tokio::test]
    async fn test_assign_pod_clears_unschedulable_reason_on_placement() {
        let service = test_support::service();
        let mut pod = test_support::pod_detail("default", "my-pod");
        pod.unschedulable_reason = Some("no fit".to_string());
        service.store.upsert_pod(pod).await.unwrap();

        service
            .assign_pod_impl(assign_request("default", "my-pod", placed_on("node-a")))
            .await
            .unwrap();

        let stored = service
            .store
            .get_pod("default", "my-pod")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.unschedulable_reason, None);
        assert_eq!(stored.node_name, "node-a");
    }

    #[tokio::test]
    async fn test_assign_pod_placement_resets_unknown_status_to_pending() {
        let service = test_support::service();
        let mut pod = test_support::pod_detail("default", "my-pod");
        if let Some(core_pod) = pod.core.as_mut().and_then(|core| core.pod.as_mut()) {
            core_pod.status = PodStatus::Unknown as i32;
        }
        service.store.upsert_pod(pod).await.unwrap();

        service
            .assign_pod_impl(assign_request("default", "my-pod", placed_on("node-a")))
            .await
            .unwrap();

        let stored = service
            .store
            .get_pod("default", "my-pod")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.node_name, "node-a");
        assert_eq!(
            stored.core.unwrap().pod.unwrap().status,
            PodStatus::Pending as i32
        );
    }

    #[tokio::test]
    async fn test_assign_pod_orphaned_clears_node_and_sets_unknown() {
        let service = test_support::service();
        let mut pod = test_support::pod_detail("default", "my-pod");
        pod.node_name = "node-a".to_string();
        if let Some(core_pod) = pod.core.as_mut().and_then(|core| core.pod.as_mut()) {
            core_pod.status = PodStatus::Running as i32;
        }
        service.store.upsert_pod(pod).await.unwrap();
        let mut pod_events = service.store.subscribe_pod_events();

        service
            .assign_pod_impl(assign_request(
                "default",
                "my-pod",
                orphaned("node node-a became NotReady"),
            ))
            .await
            .expect("an orphaned outcome should still be accepted");

        let stored = service
            .store
            .get_pod("default", "my-pod")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.node_name, "", "an orphaned pod keeps no node");
        assert_eq!(
            stored.unschedulable_reason.as_deref(),
            Some("node node-a became NotReady")
        );
        assert_eq!(
            stored.core.unwrap().pod.unwrap().status,
            PodStatus::Unknown as i32
        );

        let pod_event = pod_events
            .try_recv()
            .expect("a pod event should be published");
        assert_eq!(pod_event.event_type, EventType::Modified as i32);
    }

    #[tokio::test]
    async fn test_assign_pod_unknown_pod_is_not_found() {
        let service = test_support::service();

        for outcome in [placed_on("node-a"), unschedulable("no fit")] {
            let err = service
                .assign_pod_impl(assign_request("default", "ghost", outcome))
                .await
                .expect_err("assigning a pod that doesn't exist should fail");

            assert_eq!(err.code(), Code::NotFound);
            assert_eq!(err.message(), "pod default/ghost not found");
        }
    }

    #[tokio::test]
    async fn test_assign_pod_missing_outcome_rejected() {
        let service = test_support::service();
        service
            .store
            .upsert_pod(test_support::pod_detail("default", "my-pod"))
            .await
            .unwrap();

        let err = service
            .assign_pod_impl(assign_request("default", "my-pod", None))
            .await
            .expect_err("a request without an outcome should be rejected");

        assert_eq!(err.code(), Code::InvalidArgument);
        assert_eq!(err.message(), "missing assignment outcome");
    }
}
