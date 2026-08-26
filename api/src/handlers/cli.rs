/// CLI-facing pod & node reads/writes.
use proto::api::v1::{
    CreatePodRequest, CreatePodResponse, DeletePodRequest, DeletePodResponse, GetNodeRequest,
    GetNodeResponse, GetPodRequest, GetPodResponse, ListNodesRequest, ListNodesResponse,
    ListPodsRequest, ListPodsResponse, WatchDesiredStateEvent, watch_desired_state_event,
};
use proto::shared::v1::{PodDetail, PodStatus};
use tonic::{Request, Response, Status};

use crate::service::ApiService;
use crate::validation::validate_dns1123_subdomain;

impl ApiService {
    pub async fn create_pod_impl(
        &self,
        request: Request<CreatePodRequest>,
    ) -> Result<Response<CreatePodResponse>, Status> {
        let mut pod_with_spec = request
            .into_inner()
            .pod
            .ok_or_else(|| Status::invalid_argument("pod is required"))?;

        let name = pod_with_spec
            .pod
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("pod.pod is required"))?
            .name
            .clone();
        let spec = pod_with_spec
            .spec
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("pod.spec is required"))?;
        let namespace = spec.namespace.clone();

        validate_dns1123_subdomain(&name, "pod name")?;
        validate_dns1123_subdomain(&namespace, "namespace")?;

        if spec.containers.is_empty() {
            return Err(Status::invalid_argument(
                "pod spec must include at least one container",
            ));
        }
        let mut seen_container_names = std::collections::HashSet::new();
        for container in &spec.containers {
            if container.name.is_empty() {
                return Err(Status::invalid_argument("container name must not be empty"));
            }
            if !seen_container_names.insert(container.name.as_str()) {
                return Err(Status::invalid_argument(format!(
                    "duplicate container name '{}'",
                    container.name
                )));
            }
            if container.image.is_empty() {
                return Err(Status::invalid_argument(format!(
                    "container '{}' must specify an image",
                    container.name
                )));
            }
        }

        if let Some(pod) = pod_with_spec.pod.as_mut() {
            pod.status = PodStatus::Pending as i32;
        }

        let pod_detail = PodDetail {
            core: Some(pod_with_spec),
            ..Default::default()
        };

        if !self
            .store
            .create_pod(pod_detail.clone())
            .await
            .map_err(|e| e.to_status())?
        {
            return Err(crate::errors::pod_already_exists(&namespace, &name));
        }

        Ok(Response::new(CreatePodResponse {
            pod: Some(pod_detail),
        }))
    }

    pub async fn delete_pod_impl(
        &self,
        request: Request<DeletePodRequest>,
    ) -> Result<Response<DeletePodResponse>, Status> {
        let req = request.into_inner();

        let pod = self
            .store
            .remove_pod(&req.namespace, &req.name)
            .await
            .map_err(|e| e.to_status())?
            .ok_or_else(|| crate::errors::pod_not_found(&req.namespace, &req.name))?;

        if !pod.node_name.is_empty() {
            self.store
                .publish_desired_state_event(
                    &pod.node_name,
                    WatchDesiredStateEvent {
                        action: watch_desired_state_event::Action::Stop as i32,
                        pod: pod.core,
                    },
                )
                .await;
        }

        Ok(Response::new(DeletePodResponse { name: req.name }))
    }

    pub async fn get_pod_impl(
        &self,
        request: Request<GetPodRequest>,
    ) -> Result<Response<GetPodResponse>, Status> {
        let req = request.into_inner();
        let pod = self
            .store
            .get_pod(&req.namespace, &req.name)
            .await
            .map_err(|e| e.to_status())?
            .ok_or_else(|| crate::errors::pod_not_found(&req.namespace, &req.name))?;
        Ok(Response::new(GetPodResponse { pod: Some(pod) }))
    }

    pub async fn list_pods_impl(
        &self,
        _request: Request<ListPodsRequest>,
    ) -> Result<Response<ListPodsResponse>, Status> {
        let pods = self.store.list_pods().await.map_err(|e| e.to_status())?;
        Ok(Response::new(ListPodsResponse { pods }))
    }

    pub async fn get_node_impl(
        &self,
        request: Request<GetNodeRequest>,
    ) -> Result<Response<GetNodeResponse>, Status> {
        let req = request.into_inner();
        let node = self
            .store
            .get_node(&req.name)
            .await
            .map_err(|e| e.to_status())?
            .ok_or_else(|| crate::errors::node_not_found(&req.name))?;
        Ok(Response::new(GetNodeResponse { node: Some(node) }))
    }

    pub async fn list_nodes_impl(
        &self,
        _request: Request<ListNodesRequest>,
    ) -> Result<Response<ListNodesResponse>, Status> {
        let nodes = self.store.list_nodes().await.map_err(|e| e.to_status())?;
        Ok(Response::new(ListNodesResponse { nodes }))
    }
}

#[cfg(test)]
mod tests {
    use proto::shared::v1::{EventType, NodeStatus, Pod, PodDetail, PodSpec, PodWithSpec};
    use tonic::Code;

    use crate::test_support;
    use crate::test_support::service;

    use super::*;

    fn get_pod_request(namespace: &str, name: &str) -> Request<GetPodRequest> {
        Request::new(GetPodRequest {
            name: name.to_string(),
            namespace: namespace.to_string(),
        })
    }

    #[tokio::test]
    async fn test_get_pod_returns_inserted_pod() {
        let service = service();
        let pod = test_support::pod_detail("default", "my-pod");
        service.store.upsert_pod(pod.clone()).await.unwrap();

        let response = service
            .get_pod_impl(get_pod_request("default", "my-pod"))
            .await
            .expect("get_pod on an existing pod should succeed");

        assert_eq!(response.into_inner().pod, Some(pod));
    }

    #[tokio::test]
    async fn test_get_pod_missing_returns_not_found() {
        let service = service();

        let err = service
            .get_pod_impl(get_pod_request("default", "does-not-exist"))
            .await
            .expect_err("get_pod on a missing pod should return NotFound");

        assert_eq!(err.code(), Code::NotFound);
    }

    fn get_node_request(name: &str) -> Request<GetNodeRequest> {
        Request::new(GetNodeRequest {
            name: name.to_string(),
        })
    }

    #[tokio::test]
    async fn test_get_node_returns_inserted_node() {
        let service = service();
        let node = test_support::node("node-1", NodeStatus::Ready);
        service
            .store
            .upsert_and_publish_node(node.clone())
            .await
            .unwrap();

        let response = service
            .get_node_impl(get_node_request("node-1"))
            .await
            .expect("get_node on an existing node should succeed");

        assert_eq!(response.into_inner().node, Some(node));
    }

    #[tokio::test]
    async fn test_get_node_missing_returns_not_found() {
        let service = service();

        let err = service
            .get_node_impl(get_node_request("does-not-exist"))
            .await
            .expect_err("get_node on a missing node should return NotFound");

        assert_eq!(err.code(), Code::NotFound);
    }

    fn pod_sort_key(pod: &PodDetail) -> (String, String) {
        crate::store::pod_key(pod)
    }

    #[tokio::test]
    async fn test_list_pods_empty_store_returns_empty_list() {
        let service = service();

        let response = service
            .list_pods_impl(Request::new(ListPodsRequest {}))
            .await
            .expect("list_pods should always succeed");

        assert!(response.into_inner().pods.is_empty());
    }

    #[tokio::test]
    async fn test_list_pods_returns_single_pod() {
        let service = service();
        let pod = test_support::pod_detail("default", "my-pod");
        service.store.upsert_pod(pod.clone()).await.unwrap();

        let response = service
            .list_pods_impl(Request::new(ListPodsRequest {}))
            .await
            .expect("list_pods should always succeed");

        assert_eq!(response.into_inner().pods, vec![pod]);
    }

    #[tokio::test]
    async fn test_list_pods_returns_all_pods() {
        let service = service();
        let mut expected = vec![
            test_support::pod_detail("default", "pod-a"),
            test_support::pod_detail("default", "pod-b"),
            test_support::pod_detail("kube-system", "pod-c"),
        ];
        for pod in &expected {
            service.store.upsert_pod(pod.clone()).await.unwrap();
        }

        let response = service
            .list_pods_impl(Request::new(ListPodsRequest {}))
            .await
            .expect("list_pods should always succeed");

        // HashMap iteration order isn't deterministic, so compare sorted by (namespace, name)
        let mut got = response.into_inner().pods;
        got.sort_by_key(pod_sort_key);
        expected.sort_by_key(pod_sort_key);
        assert_eq!(got, expected);
    }

    #[tokio::test]
    async fn test_list_nodes_empty_store_returns_empty_list() {
        let service = service();

        let response = service
            .list_nodes_impl(Request::new(ListNodesRequest {}))
            .await
            .expect("list_nodes should always succeed");

        assert!(response.into_inner().nodes.is_empty());
    }

    #[tokio::test]
    async fn test_list_nodes_returns_single_node() {
        let service = service();
        let node = test_support::node("node-1", NodeStatus::Ready);
        service
            .store
            .upsert_and_publish_node(node.clone())
            .await
            .unwrap();

        let response = service
            .list_nodes_impl(Request::new(ListNodesRequest {}))
            .await
            .expect("list_nodes should always succeed");

        assert_eq!(response.into_inner().nodes, vec![node]);
    }

    #[tokio::test]
    async fn test_list_nodes_returns_all_nodes() {
        let service = service();
        let mut expected = vec![
            test_support::node("node-a", NodeStatus::Ready),
            test_support::node("node-b", NodeStatus::Cordon),
            test_support::node("node-c", NodeStatus::NotReady),
        ];
        for node in &expected {
            service
                .store
                .upsert_and_publish_node(node.clone())
                .await
                .unwrap();
        }

        let response = service
            .list_nodes_impl(Request::new(ListNodesRequest {}))
            .await
            .expect("list_nodes should always succeed");

        // HashMap iteration order isn't deterministic, so compare sorted by name
        let mut got = response.into_inner().nodes;
        got.sort_by_key(|n| n.name.clone());
        expected.sort_by_key(|n| n.name.clone());
        assert_eq!(got, expected);
    }

    fn create_pod_request_raw(namespace: &str, name: &str) -> CreatePodRequest {
        CreatePodRequest {
            pod: Some(PodWithSpec {
                pod: Some(Pod {
                    name: name.to_string(),
                    status: PodStatus::Running as i32, // caller-supplied status must be ignored
                    requests: None,
                    limits: None,
                }),
                spec: Some(PodSpec {
                    namespace: namespace.to_string(),
                    containers: vec![test_support::container("app", "busybox")],
                }),
            }),
        }
    }

    fn create_pod_request(namespace: &str, name: &str) -> Request<CreatePodRequest> {
        Request::new(create_pod_request_raw(namespace, name))
    }

    #[tokio::test]
    async fn test_create_pod_returns_pod_detail_with_pending_status() {
        let service = service();

        let response = service
            .create_pod_impl(create_pod_request("default", "my-pod"))
            .await
            .expect("create_pod should succeed")
            .into_inner();

        let core = response.pod.expect("response should contain a pod").core;
        let pod = core.as_ref().and_then(|c| c.pod.as_ref()).unwrap();
        let spec = core.as_ref().and_then(|c| c.spec.as_ref()).unwrap();
        assert_eq!(pod.name, "my-pod");
        assert_eq!(pod.status, PodStatus::Pending as i32);
        assert_eq!(spec.namespace, "default");
    }

    #[tokio::test]
    async fn test_create_pod_persists_to_store() {
        let service = service();

        service
            .create_pod_impl(create_pod_request("default", "my-pod"))
            .await
            .expect("create_pod should succeed");

        let stored = service
            .store
            .get_pod("default", "my-pod")
            .await
            .expect("etcd should be reachable")
            .expect("pod should be in the store");
        assert_eq!(stored.node_name, "");
    }

    #[tokio::test]
    async fn test_create_pod_duplicate_returns_already_exists() {
        let service = service();

        service
            .create_pod_impl(create_pod_request("default", "my-pod"))
            .await
            .expect("first create_pod should succeed");

        let err = service
            .create_pod_impl(create_pod_request("default", "my-pod"))
            .await
            .expect_err("second create_pod should fail");

        assert_eq!(err.code(), Code::AlreadyExists);
        assert_eq!(service.store.list_pods().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_create_pod_publishes_added_event() {
        let service = service();
        let mut events = service.store.subscribe_pod_events();

        service
            .create_pod_impl(create_pod_request("default", "my-pod"))
            .await
            .expect("create_pod should succeed");

        let event = events
            .try_recv()
            .expect("an event should have been published");
        assert_eq!(event.event_type, EventType::Added as i32);
        let pod_name = event
            .pod
            .and_then(|p| p.core)
            .and_then(|c| c.pod)
            .map(|p| p.name);
        assert_eq!(pod_name.as_deref(), Some("my-pod"));
    }

    #[tokio::test]
    async fn test_create_pod_missing_pod_is_invalid_argument() {
        let service = service();

        let err = service
            .create_pod_impl(Request::new(CreatePodRequest { pod: None }))
            .await
            .expect_err("missing pod should fail");

        assert_eq!(err.code(), Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_create_pod_missing_spec_is_invalid_argument() {
        let service = service();
        let mut req = create_pod_request_raw("default", "my-pod");
        req.pod.as_mut().unwrap().spec = None;

        let err = service
            .create_pod_impl(Request::new(req))
            .await
            .expect_err("missing spec should fail");

        assert_eq!(err.code(), Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_create_pod_empty_name_is_invalid_argument() {
        let service = service();

        let err = service
            .create_pod_impl(create_pod_request("default", ""))
            .await
            .expect_err("empty name should fail");

        assert_eq!(err.code(), Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_create_pod_invalid_name_charset_is_invalid_argument() {
        let service = service();

        let err = service
            .create_pod_impl(create_pod_request("default", "My_Pod"))
            .await
            .expect_err("invalid name charset should fail");

        assert_eq!(err.code(), Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_create_pod_empty_namespace_is_invalid_argument() {
        let service = service();

        let err = service
            .create_pod_impl(create_pod_request("", "my-pod"))
            .await
            .expect_err("empty namespace should fail");

        assert_eq!(err.code(), Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_create_pod_invalid_namespace_charset_is_invalid_argument() {
        let service = service();

        let err = service
            .create_pod_impl(create_pod_request("Default_NS", "my-pod"))
            .await
            .expect_err("invalid namespace charset should fail");

        assert_eq!(err.code(), Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_create_pod_no_containers_is_invalid_argument() {
        let service = service();
        let mut req = create_pod_request_raw("default", "my-pod");
        req.pod.as_mut().unwrap().spec.as_mut().unwrap().containers = vec![];

        let err = service
            .create_pod_impl(Request::new(req))
            .await
            .expect_err("no containers should fail");

        assert_eq!(err.code(), Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_create_pod_container_missing_image_is_invalid_argument() {
        let service = service();
        let mut req = create_pod_request_raw("default", "my-pod");
        req.pod.as_mut().unwrap().spec.as_mut().unwrap().containers[0].image = String::new();

        let err = service
            .create_pod_impl(Request::new(req))
            .await
            .expect_err("missing image should fail");

        assert_eq!(err.code(), Code::InvalidArgument);
    }

    fn delete_pod_request(namespace: &str, name: &str) -> Request<DeletePodRequest> {
        Request::new(DeletePodRequest {
            name: name.to_string(),
            namespace: namespace.to_string(),
        })
    }

    #[tokio::test]
    async fn test_create_pod_empty_container_name_is_invalid_argument() {
        let service = service();
        let mut req = create_pod_request_raw("default", "my-pod");
        req.pod.as_mut().unwrap().spec.as_mut().unwrap().containers[0].name = String::new();

        let err = service
            .create_pod_impl(Request::new(req))
            .await
            .expect_err("empty container name should fail");

        assert_eq!(err.code(), Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_delete_pod_removes_existing_pod_and_returns_name() {
        let service = service();
        service
            .create_pod_impl(create_pod_request("default", "my-pod"))
            .await
            .expect("create_pod should succeed");

        let response = service
            .delete_pod_impl(delete_pod_request("default", "my-pod"))
            .await
            .expect("delete_pod should succeed")
            .into_inner();

        assert_eq!(response.name, "my-pod");
        assert_eq!(service.store.get_pod("default", "my-pod").await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_delete_pod_missing_pod_returns_not_found() {
        let service = service();

        let err = service
            .delete_pod_impl(delete_pod_request("default", "does-not-exist"))
            .await
            .expect_err("delete_pod should fail for an unknown pod");

        assert_eq!(err.code(), Code::NotFound);
    }

    #[tokio::test]
    async fn test_create_pod_duplicate_container_names_is_invalid_argument() {
        let service = service();
        let mut req = create_pod_request_raw("default", "my-pod");
        req.pod
            .as_mut()
            .unwrap()
            .spec
            .as_mut()
            .unwrap()
            .containers
            .push(test_support::container("app", "nginx"));

        let err = service
            .create_pod_impl(Request::new(req))
            .await
            .expect_err("duplicate container names should fail");

        assert_eq!(err.code(), Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_delete_pod_publishes_deleted_event() {
        let service = service();
        let mut events = service.store.subscribe_pod_events();
        service
            .create_pod_impl(create_pod_request("default", "my-pod"))
            .await
            .expect("create_pod should succeed");
        events.try_recv().expect("ADDED event from create_pod"); // drain the create event

        service
            .delete_pod_impl(delete_pod_request("default", "my-pod"))
            .await
            .expect("delete_pod should succeed");

        let event = events
            .try_recv()
            .expect("a DELETED event should have been published");
        assert_eq!(event.event_type, EventType::Deleted as i32);
    }

    #[tokio::test]
    async fn test_delete_pod_publishes_stop_to_the_assigned_node() {
        let service = service();
        // create_pod never assigns a node, so seed a scheduled pod directly.
        let mut scheduled = test_support::pod_detail("default", "my-pod");
        scheduled.node_name = "node-a".to_string();
        service.store.upsert_pod(scheduled.clone()).await;

        let mut desired_state_events = service.store.subscribe_desired_state_events("node-a").await;

        service
            .delete_pod_impl(delete_pod_request("default", "my-pod"))
            .await
            .expect("delete_pod should succeed");

        let event = desired_state_events
            .try_recv()
            .expect("node-a should receive a STOP event");
        assert_eq!(event.action, watch_desired_state_event::Action::Stop as i32);
        assert_eq!(event.pod, scheduled.core);
    }

    #[tokio::test]
    async fn test_delete_pod_stop_goes_only_to_the_assigned_node() {
        let service = service();
        let mut scheduled = test_support::pod_detail("default", "my-pod");
        scheduled.node_name = "node-a".to_string();
        service.store.upsert_pod(scheduled).await;

        let mut other_node = service.store.subscribe_desired_state_events("node-b").await;

        service
            .delete_pod_impl(delete_pod_request("default", "my-pod"))
            .await
            .expect("delete_pod should succeed");

        assert!(
            other_node.try_recv().is_err(),
            "node-b must not be told to stop a pod scheduled on node-a"
        );
    }

    #[tokio::test]
    async fn test_delete_pod_without_node_name_skips_desired_state_event() {
        let service = service();
        service
            .create_pod_impl(create_pod_request("default", "my-pod"))
            .await
            .expect("create_pod should succeed");

        let mut desired_state_events = service.store.subscribe_desired_state_events("").await;

        service
            .delete_pod_impl(delete_pod_request("default", "my-pod"))
            .await
            .expect("delete_pod should succeed");

        assert!(
            desired_state_events.try_recv().is_err(),
            "a never-scheduled pod shouldn't trigger a desired-state event"
        );
    }
}
