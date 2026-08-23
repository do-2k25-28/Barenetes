/// Agent-facing status reports and desired-state watch.
use proto::api::v1::{
    UpdateNodeStatusRequest, UpdateNodeStatusResponse, UpdatePodStatusRequest,
    UpdatePodStatusResponse, WatchDesiredStateRequest, WatchPodEvent,
};
use proto::shared::v1::EventType;
use tonic::{Request, Response, Status};

use crate::service::{ApiService, DesiredStateEventStream};

impl ApiService {
    pub async fn update_pod_status_impl(
        &self,
        request: Request<UpdatePodStatusRequest>,
    ) -> Result<Response<UpdatePodStatusResponse>, Status> {
        let req = request.into_inner();
        // No namespace/name exist yet when the pod field itself is absent, so the
        // not-found message carries "<unknown>" placeholders instead.
        let reported = req
            .pod
            .ok_or_else(|| crate::errors::pod_not_found("<unknown>", "<unknown>"))?;
        let spec = reported.spec.unwrap_or_default();
        let pod = reported
            .pod
            .filter(|pod| !pod.name.is_empty())
            .ok_or_else(|| Status::invalid_argument("missing pod name"))?;
        if spec.namespace.is_empty() {
            return Err(Status::invalid_argument("missing pod namespace"));
        }

        let mut detail = self
            .store
            .get_pod(&spec.namespace, &pod.name)
            .await
            .ok_or_else(|| crate::errors::pod_not_found(&spec.namespace, &pod.name))?;

        // The agent owns the observed lifecycle status; the desired spec stays as
        // stored, and node placement remains the scheduler's authority (AssignPod).
        if let Some(core_pod) = detail.core.as_mut().and_then(|core| core.pod.as_mut()) {
            core_pod.status = pod.status;
        }
        detail.container_statuses = req.container_statuses;
        detail.pod_ip = req.pod_ip;
        detail.message = req.message;
        detail.resource_usage = req.resource_usage;

        self.store.upsert_pod(detail.clone()).await;
        self.store.publish_pod_event(WatchPodEvent {
            event_type: EventType::Modified as i32,
            pod: Some(detail),
        });

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

        self.store.upsert_and_publish_node(node).await;

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
    use proto::api::v1::{UpdatePodStatusRequest, UpdatePodStatusResponse};
    use proto::shared::v1::{
        ContainerStatus, EventType, Pod, PodSpec, PodStatus, PodWithSpec, Resources, State,
    };
    use tonic::{Code, Request};

    use crate::test_support;

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
            pod_ip: "10.0.0.5".to_string(),
            message: String::new(),
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
            .await;
        let mut events = service.store.subscribe_pod_events();

        let response = service
            .update_pod_status_impl(update_request("default", "web"))
            .await
            .unwrap();

        assert_eq!(response.get_ref(), &UpdatePodStatusResponse {});

        let pod = service.store.get_pod("default", "web").await.unwrap();
        let core_pod = pod
            .core
            .as_ref()
            .and_then(|core| core.pod.as_ref())
            .unwrap();
        assert_eq!(core_pod.status, PodStatus::Running as i32);
        assert_eq!(pod.container_statuses.len(), 1);
        assert_eq!(pod.container_statuses[0].name, "main");
        assert_eq!(pod.container_statuses[0].state, State::Active as i32);
        assert_eq!(pod.pod_ip, "10.0.0.5");
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
        service.store.upsert_pod(seed).await;

        service
            .update_pod_status_impl(update_request("default", "web"))
            .await
            .unwrap();

        let pod = service.store.get_pod("default", "web").await.unwrap();
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
    }
}
