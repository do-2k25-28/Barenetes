/// Agent-facing status reports and desired-state watch.
use proto::api::v1::{
    UpdateNodeStatusRequest, UpdateNodeStatusResponse, UpdatePodStatusRequest,
    UpdatePodStatusResponse, WatchDesiredStateEvent, WatchDesiredStateRequest,
    watch_desired_state_event,
};
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};

use crate::service::{ApiService, DesiredStateEventStream};
use crate::validation::validate_dns1123_subdomain;

impl ApiService {
    pub async fn update_pod_status_impl(
        &self,
        request: Request<UpdatePodStatusRequest>,
    ) -> Result<Response<UpdatePodStatusResponse>, Status> {
        let UpdatePodStatusRequest {
            pod,
            container_statuses,
            pod_ip,
            message,
            resource_usage,
        } = request.into_inner();
        // No namespace/name exist yet when the pod field itself is absent, so the
        // not-found message carries "<unknown>" placeholders instead.
        let reported = pod.ok_or_else(|| crate::errors::pod_not_found("<unknown>", "<unknown>"))?;
        let spec = reported.spec.unwrap_or_default();

        // Aggregate missing-field errors instead of first-error-wins, so a request missing
        // both pod name and namespace doesn't have the namespace problem silently swallowed
        // by an early return on the name check.
        let mut missing = Vec::new();
        if reported.pod.as_ref().is_none_or(|pod| pod.name.is_empty()) {
            missing.push("pod name");
        }
        if spec.namespace.is_empty() {
            missing.push("pod namespace");
        }
        if !missing.is_empty() {
            return Err(Status::invalid_argument(format!(
                "missing {}",
                missing.join(", ")
            )));
        }
        let reported_pod = reported.pod.unwrap();

        let found = self
            .store
            .update_pod_status(&spec.namespace, &reported_pod.name, |detail| {
                // The agent owns the observed lifecycle status; the desired spec stays as
                // stored, and node placement remains the scheduler's authority (AssignPod).
                if let Some(core_pod) = detail.core.as_mut().and_then(|core| core.pod.as_mut()) {
                    core_pod.status = reported_pod.status;
                }
                // Only overwrite a runtime field when the agent actually
                // reported a new value.
                if !container_statuses.is_empty() {
                    detail.container_statuses = container_statuses;
                }
                if pod_ip.is_some() {
                    detail.pod_ip = pod_ip;
                }
                if message.is_some() {
                    detail.message = message;
                }
                if resource_usage.is_some() {
                    detail.resource_usage = resource_usage;
                }
            })
            .await
            .map_err(|e| Status::unavailable(e.to_string()))?;
        if !found {
            return Err(crate::errors::pod_not_found(
                &spec.namespace,
                &reported_pod.name,
            ));
        }

        Ok(Response::new(UpdatePodStatusResponse {}))
    }

    pub async fn update_node_status_impl(
        &self,
        request: Request<UpdateNodeStatusRequest>,
    ) -> Result<Response<UpdateNodeStatusResponse>, Status> {
        let req = request.into_inner();
        let node = req
            .node
            .ok_or_else(|| crate::errors::missing_node("<unknown>"))?;
        if node.name.is_empty() {
            return Err(Status::invalid_argument("missing node name"));
        }

        self.store
            .upsert_and_publish_node(node)
            .await
            .map_err(|e| Status::unavailable(e.to_string()))?;

        Ok(Response::new(UpdateNodeStatusResponse {}))
    }

    pub async fn watch_desired_state_impl(
        &self,
        request: Request<WatchDesiredStateRequest>,
    ) -> Result<Response<DesiredStateEventStream>, Status> {
        let node_name = request.into_inner().node_name;
        validate_dns1123_subdomain(&node_name, "node name")?;

        // Opening with the node's current desired set, closed by SYNCED, is what lets an
        // agent reconcile on connect instead of missing whatever was published while it
        // was away. The snapshot and the subscription share one guard, so nothing is lost
        // between them.
        let (assigned, receiver) = self
            .store
            .subscribe_desired_state_with_snapshot(&node_name)
            .await;

        let snapshot = assigned.into_iter().map(|pod| {
            Ok(WatchDesiredStateEvent {
                action: watch_desired_state_event::Action::Run as i32,
                pod: Some(pod),
            })
        });
        let synced = std::iter::once(Ok(WatchDesiredStateEvent {
            action: watch_desired_state_event::Action::Synced as i32,
            pod: None,
        }));

        let opening = tokio_stream::iter(snapshot.chain(synced));
        let live = crate::service::broadcast_to_stream(receiver, "watch_desired_state");

        Ok(Response::new(Box::pin(opening.chain(live))))
    }
}

#[cfg(test)]
mod tests {
    use proto::api::v1::{
        UpdateNodeStatusRequest, UpdatePodStatusRequest, UpdatePodStatusResponse,
        WatchDesiredStateEvent, watch_desired_state_event,
    };
    use proto::shared::v1::{
        ContainerStatus, EventType, NodeStatus, Pod, PodDetail, PodSpec, PodStatus, PodWithSpec,
        Resources, State,
    };
    use tokio_stream::StreamExt;
    use tonic::{Code, Request};

    use crate::test_support::{self, node};

    use super::*;

    fn update_request(namespace: &str, name: &str) -> Request<UpdatePodStatusRequest> {
        Request::new(UpdatePodStatusRequest {
            pod: Some(PodWithSpec {
                pod: Some(Pod {
                    name: name.to_string(),
                    status: PodStatus::Running as i32,
                    requests: None,
                    limits: None,
                }),
                spec: Some(PodSpec {
                    namespace: namespace.to_string(),
                    containers: vec![],
                }),
            }),
            container_statuses: vec![ContainerStatus {
                name: "main".to_string(),
                state: State::Active as i32,
            }],
            pod_ip: Some("10.0.0.5".to_string()),
            message: None,
            resource_usage: Some(Resources {
                cpu: 250,
                memory: 128,
            }),
        })
    }

    #[tokio::test]
    async fn update_pod_status_updates_runtime_fields_and_publishes_modified() {
        let service = test_support::service();
        service
            .store
            .upsert_pod(test_support::pod_detail("default", "web"))
            .await
            .unwrap();
        let mut events = service.store.subscribe_pod_events();

        let response = service
            .update_pod_status_impl(update_request("default", "web"))
            .await
            .unwrap();

        assert_eq!(response.get_ref(), &UpdatePodStatusResponse {});

        let pod = service
            .store
            .get_pod("default", "web")
            .await
            .unwrap()
            .unwrap();
        let core_pod = pod
            .core
            .as_ref()
            .and_then(|core| core.pod.as_ref())
            .unwrap();
        assert_eq!(core_pod.status, PodStatus::Running as i32);
        assert_eq!(pod.container_statuses.len(), 1);
        assert_eq!(pod.container_statuses[0].name, "main");
        assert_eq!(pod.container_statuses[0].state, State::Active as i32);
        assert_eq!(pod.pod_ip, Some("10.0.0.5".to_string()));
        assert_eq!(pod.resource_usage.as_ref().unwrap().cpu, 250);

        let event = events.try_recv().unwrap();
        assert_eq!(event.event_type, EventType::Modified as i32);
        let event_pod = event.pod.unwrap();
        assert_eq!(
            event_pod
                .core
                .as_ref()
                .and_then(|core| core.pod.as_ref())
                .unwrap()
                .name,
            "web"
        );
    }

    #[tokio::test]
    async fn update_pod_status_preserves_scheduler_owned_fields() {
        let service = test_support::service();
        let mut seed = test_support::pod_detail("default", "web");
        seed.node_name = "node-a".to_string();
        seed.unschedulable_reason = Some("no node fits".to_string());
        if let Some(core_pod) = seed.core.as_mut().and_then(|core| core.pod.as_mut()) {
            core_pod.requests = Some(Resources {
                cpu: 100,
                memory: 64,
            });
        }
        service.store.upsert_pod(seed).await.unwrap();

        service
            .update_pod_status_impl(update_request("default", "web"))
            .await
            .unwrap();

        let pod = service
            .store
            .get_pod("default", "web")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pod.node_name, "node-a");
        assert_eq!(pod.unschedulable_reason.as_deref(), Some("no node fits"));
        let core_pod = pod
            .core
            .as_ref()
            .and_then(|core| core.pod.as_ref())
            .unwrap();
        assert_eq!(core_pod.requests.as_ref().unwrap().cpu, 100);
    }

    #[tokio::test]
    async fn update_pod_status_unknown_pod_is_not_found() {
        let service = test_support::service();

        let err = service
            .update_pod_status_impl(update_request("default", "ghost"))
            .await
            .unwrap_err();

        assert_eq!(err.code(), Code::NotFound);
        assert_eq!(err.message(), "pod default/ghost not found");
    }

    #[tokio::test]
    async fn update_pod_status_rejects_missing_fields() {
        let service = test_support::service();

        let mut request = update_request("default", "web");
        request.get_mut().pod = None;
        let err = service.update_pod_status_impl(request).await.unwrap_err();
        assert_eq!(err.code(), Code::NotFound);
        assert_eq!(err.message(), "pod <unknown>/<unknown> not found");

        let request = update_request("default", "");
        let err = service.update_pod_status_impl(request).await.unwrap_err();
        assert_eq!(err.code(), Code::InvalidArgument);
        assert_eq!(err.message(), "missing pod name");

        let mut request = update_request("", "web");
        request
            .get_mut()
            .pod
            .as_mut()
            .unwrap()
            .spec
            .as_mut()
            .unwrap()
            .namespace = String::new();
        let err = service.update_pod_status_impl(request).await.unwrap_err();
        assert_eq!(err.code(), Code::InvalidArgument);
        assert_eq!(err.message(), "missing pod namespace");

        let request = update_request("", "");
        let err = service.update_pod_status_impl(request).await.unwrap_err();
        assert_eq!(err.code(), Code::InvalidArgument);
        assert_eq!(err.message(), "missing pod name, pod namespace");
    }

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
            service
                .store
                .get_node("node-1")
                .await
                .unwrap()
                .map(|n| n.status),
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

    #[tokio::test]
    async fn test_update_node_status_blank_name_rejected() {
        let service = test_support::service();

        let err = service
            .update_node_status_impl(update_node_status_request("", NodeStatus::Ready))
            .await
            .expect_err("a blank node name should be rejected");

        assert_eq!(err.code(), Code::InvalidArgument);
        assert_eq!(err.message(), "missing node name");
    }

    fn watch_desired_state_request(node_name: &str) -> Request<WatchDesiredStateRequest> {
        Request::new(WatchDesiredStateRequest {
            node_name: node_name.to_string(),
        })
    }

    fn run_event() -> WatchDesiredStateEvent {
        WatchDesiredStateEvent {
            action: watch_desired_state_event::Action::Run as i32,
            pod: None,
        }
    }

    /// Consumes the opening snapshot up to and including SYNCED, returning the pods it named.
    async fn drain_snapshot(
        stream: &mut DesiredStateEventStream,
    ) -> Vec<Option<proto::shared::v1::PodWithSpec>> {
        let mut snapshot = Vec::new();
        loop {
            let event = stream
                .next()
                .await
                .expect("stream ended before SYNCED")
                .expect("snapshot event should not be an error");
            if event.action == watch_desired_state_event::Action::Synced as i32 {
                assert_eq!(event.pod, None, "SYNCED carries no pod");
                return snapshot;
            }
            assert_eq!(event.action, watch_desired_state_event::Action::Run as i32);
            snapshot.push(event.pod);
        }
    }

    #[tokio::test]
    async fn test_watch_desired_state_receives_event_for_its_own_node() {
        let service = test_support::service();
        let mut stream = service
            .watch_desired_state_impl(watch_desired_state_request("node-a"))
            .await
            .unwrap()
            .into_inner();
        assert!(
            drain_snapshot(&mut stream).await.is_empty(),
            "no pods are assigned to node-a yet"
        );

        service
            .store
            .publish_desired_state_event("node-a", run_event())
            .await;

        let event = stream
            .next()
            .await
            .expect("stream ended")
            .expect("event should not be an error");

        assert_eq!(event.action, watch_desired_state_event::Action::Run as i32);
    }

    fn assigned_pod(namespace: &str, name: &str, node_name: &str) -> PodDetail {
        let mut pod = test_support::pod_detail(namespace, name);
        pod.node_name = node_name.to_string();
        pod
    }

    #[tokio::test]
    async fn test_watch_desired_state_opens_with_synced_when_nothing_is_assigned() {
        let service = test_support::service();

        let mut stream = service
            .watch_desired_state_impl(watch_desired_state_request("node-a"))
            .await
            .unwrap()
            .into_inner();

        let first = stream
            .next()
            .await
            .expect("stream ended")
            .expect("event should not be an error");

        assert_eq!(
            first.action,
            watch_desired_state_event::Action::Synced as i32
        );
        assert_eq!(first.pod, None);
    }

    #[tokio::test]
    async fn test_watch_desired_state_opens_with_the_nodes_assigned_pods() {
        let service = test_support::service();
        let first_pod = assigned_pod("default", "pod-a", "node-a");
        let second_pod = assigned_pod("default", "pod-b", "node-a");
        service.store.upsert_pod(first_pod.clone()).await;
        service.store.upsert_pod(second_pod.clone()).await;

        let mut stream = service
            .watch_desired_state_impl(watch_desired_state_request("node-a"))
            .await
            .unwrap()
            .into_inner();

        let mut snapshot = drain_snapshot(&mut stream).await;
        snapshot.sort_by_key(|pod| {
            pod.as_ref()
                .and_then(|core| core.pod.as_ref())
                .map(|pod| pod.name.clone())
        });

        assert_eq!(snapshot, vec![first_pod.core, second_pod.core]);
    }

    #[tokio::test]
    async fn test_watch_desired_state_snapshot_excludes_other_nodes_pods() {
        let service = test_support::service();
        service
            .store
            .upsert_pod(assigned_pod("default", "mine", "node-a"))
            .await;
        service
            .store
            .upsert_pod(assigned_pod("default", "theirs", "node-b"))
            .await;
        // An unscheduled pod belongs to no node's desired set.
        service
            .store
            .upsert_pod(test_support::pod_detail("default", "pending"))
            .await;

        let mut stream = service
            .watch_desired_state_impl(watch_desired_state_request("node-a"))
            .await
            .unwrap()
            .into_inner();

        let snapshot = drain_snapshot(&mut stream).await;

        assert_eq!(
            snapshot.len(),
            1,
            "only node-a's pod belongs in its snapshot"
        );
        let name = snapshot[0]
            .as_ref()
            .and_then(|core| core.pod.as_ref())
            .map(|pod| pod.name.as_str());
        assert_eq!(name, Some("mine"));
    }

    #[tokio::test]
    async fn test_watch_desired_state_streams_live_events_after_synced() {
        let service = test_support::service();
        service
            .store
            .upsert_pod(assigned_pod("default", "pod-a", "node-a"))
            .await;

        let mut stream = service
            .watch_desired_state_impl(watch_desired_state_request("node-a"))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(drain_snapshot(&mut stream).await.len(), 1);

        service
            .store
            .publish_desired_state_event("node-a", run_event())
            .await;

        let event = stream
            .next()
            .await
            .expect("stream ended")
            .expect("event should not be an error");

        assert_eq!(event.action, watch_desired_state_event::Action::Run as i32);
    }

    #[tokio::test]
    async fn test_watch_desired_state_empty_node_name_rejected() {
        let service = test_support::service();

        let err = service
            .watch_desired_state_impl(watch_desired_state_request(""))
            .await
            .map(|_| ())
            .expect_err("a blank node name should be rejected");

        assert_eq!(err.code(), Code::InvalidArgument);
        assert_eq!(err.message(), "node name must not be empty");
    }

    #[tokio::test]
    async fn test_watch_desired_state_malformed_node_name_rejected() {
        let service = test_support::service();

        let err = service
            .watch_desired_state_impl(watch_desired_state_request("Node_A!"))
            .await
            .map(|_| ())
            .expect_err("a malformed node name should be rejected");

        assert_eq!(err.code(), Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_watch_desired_state_does_not_receive_other_nodes_events() {
        let service = test_support::service();
        let mut stream = service
            .watch_desired_state_impl(watch_desired_state_request("node-b"))
            .await
            .unwrap()
            .into_inner();
        drain_snapshot(&mut stream).await;

        service
            .store
            .publish_desired_state_event("node-a", run_event())
            .await;

        let result =
            tokio::time::timeout(std::time::Duration::from_millis(50), stream.next()).await;

        assert!(
            result.is_err(),
            "node-b's stream must not observe an event published for node-a"
        );
    }
}
