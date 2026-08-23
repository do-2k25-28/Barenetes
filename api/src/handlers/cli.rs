/// CLI-facing pod & node reads/writes.
use proto::api::v1::{
    CreatePodRequest, CreatePodResponse, DeletePodRequest, DeletePodResponse, GetNodeRequest,
    GetNodeResponse, GetPodRequest, GetPodResponse, ListNodesRequest, ListNodesResponse,
    ListPodsRequest, ListPodsResponse,
};
use tonic::{Request, Response, Status};

use crate::service::ApiService;

impl ApiService {
    pub async fn create_pod_impl(
        &self,
        _request: Request<CreatePodRequest>,
    ) -> Result<Response<CreatePodResponse>, Status> {
        todo!("Build a PodDetail from the request, store.upsert_pod it, return it")
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
        service.store.upsert_node(node.clone()).await;

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
        service.store.upsert_node(node.clone()).await;

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
            service.store.upsert_node(node.clone()).await;
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
