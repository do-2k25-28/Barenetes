/// CLI-facing pod & node reads/writes.
use proto::api::v1::{
    CreatePodRequest, CreatePodResponse, DeletePodRequest, DeletePodResponse, GetNodeRequest,
    GetNodeResponse, GetPodRequest, GetPodResponse, ListNodesRequest, ListNodesResponse,
    ListPodsRequest, ListPodsResponse, WatchPodEvent,
};
use proto::shared::v1::{EventType, PodDetail, PodStatus};
use tonic::{Request, Response, Status};

use crate::service::ApiService;

impl ApiService {
    pub async fn create_pod_impl(
        &self,
        request: Request<CreatePodRequest>,
    ) -> Result<Response<CreatePodResponse>, Status> {
        let pod_with_spec = request
            .into_inner()
            .pod
            .ok_or_else(|| Status::invalid_argument("pod is required"))?;

        let name = pod_with_spec
            .pod
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("pod.pod is required"))?
            .name
            .clone();
        let namespace = pod_with_spec
            .spec
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("pod.spec is required"))?
            .namespace
            .clone();

        if self.store.get_pod(&namespace, &name).await.is_some() {
            return Err(Status::already_exists(format!(
                "pod {namespace}/{name} already exists"
            )));
        }

        let mut pod_with_spec = pod_with_spec;
        if let Some(pod) = pod_with_spec.pod.as_mut() {
            pod.status = PodStatus::Pending as i32;
        }

        let pod_detail = PodDetail {
            core: Some(pod_with_spec),
            container_statuses: vec![],
            pod_ip: String::new(),
            message: String::new(),
            resource_usage: None,
            node_name: String::new(),
        };

        self.store.upsert_pod(pod_detail.clone()).await;
        self.store.publish_pod_event(WatchPodEvent {
            event_type: EventType::Added as i32,
            pod: Some(pod_detail.clone()),
        });

        Ok(Response::new(CreatePodResponse {
            pod: Some(pod_detail),
        }))
    }

    pub async fn delete_pod_impl(
        &self,
        _request: Request<DeletePodRequest>,
    ) -> Result<Response<DeletePodResponse>, Status> {
        todo!("store.remove_pod, return NotFound if it didn't exist")
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
            .ok_or_else(|| crate::errors::pod_not_found(&req.namespace, &req.name))?;
        Ok(Response::new(GetPodResponse { pod: Some(pod) }))
    }

    pub async fn list_pods_impl(
        &self,
        _request: Request<ListPodsRequest>,
    ) -> Result<Response<ListPodsResponse>, Status> {
        let pods = self.store.list_pods().await;
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
            .ok_or_else(|| crate::errors::node_not_found(&req.name))?;
        Ok(Response::new(GetNodeResponse { node: Some(node) }))
    }

    pub async fn list_nodes_impl(
        &self,
        _request: Request<ListNodesRequest>,
    ) -> Result<Response<ListNodesResponse>, Status> {
        let nodes = self.store.list_nodes().await;
        Ok(Response::new(ListNodesResponse { nodes }))
    }
}

#[cfg(test)]
mod tests {
    use proto::shared::v1::{NodeStatus, PodDetail};
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
        service.store.upsert_pod(pod.clone()).await;

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
        service.store.upsert_and_publish_node(node.clone()).await;

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
        let core = pod.core.as_ref();
        (
            core.and_then(|c| c.spec.as_ref())
                .map(|s| s.namespace.clone())
                .unwrap_or_default(),
            core.and_then(|c| c.pod.as_ref())
                .map(|p| p.name.clone())
                .unwrap_or_default(),
        )
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
        service.store.upsert_pod(pod.clone()).await;

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
            service.store.upsert_pod(pod.clone()).await;
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
        service.store.upsert_and_publish_node(node.clone()).await;

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
            service.store.upsert_and_publish_node(node.clone()).await;
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
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use proto::shared::v1::{Pod, PodSpec, PodWithSpec};
    use tonic::Code;

    use super::*;
    use crate::store::Store;

    fn service() -> ApiService {
        ApiService {
            store: Arc::new(Store::new()),
        }
    }

    fn create_pod_request(namespace: &str, name: &str) -> Request<CreatePodRequest> {
        Request::new(CreatePodRequest {
            pod: Some(PodWithSpec {
                pod: Some(Pod {
                    name: name.to_string(),
                    status: PodStatus::Running as i32, // caller-supplied status must be ignored
                    requests: None,
                    limits: None,
                }),
                spec: Some(PodSpec {
                    namespace: namespace.to_string(),
                    containers: vec![],
                }),
            }),
        })
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
        assert_eq!(service.store.list_pods().await.len(), 1);
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
        let request = Request::new(CreatePodRequest {
            pod: Some(PodWithSpec {
                pod: Some(Pod {
                    name: "my-pod".to_string(),
                    status: PodStatus::Pending as i32,
                    requests: None,
                    limits: None,
                }),
                spec: None,
            }),
        });

        let err = service
            .create_pod_impl(request)
            .await
            .expect_err("missing spec should fail");

        assert_eq!(err.code(), Code::InvalidArgument);
    }
}
