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
            .ok_or_else(|| Status::not_found(format!("pods \"{}\" not found", req.name)))?;
        Ok(Response::new(GetPodResponse { pod: Some(pod) }))
    }

    pub async fn list_pods_impl(
        &self,
        _request: Request<ListPodsRequest>,
    ) -> Result<Response<ListPodsResponse>, Status> {
        todo!("store.list_pods")
    }

    pub async fn get_node_impl(
        &self,
        _request: Request<GetNodeRequest>,
    ) -> Result<Response<GetNodeResponse>, Status> {
        todo!("store.get_node, return NotFound if missing")
    }

    pub async fn list_nodes_impl(
        &self,
        _request: Request<ListNodesRequest>,
    ) -> Result<Response<ListNodesResponse>, Status> {
        todo!("store.list_nodes")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use proto::shared::v1::{Pod, PodDetail, PodSpec, PodStatus, PodWithSpec};
    use tonic::Code;

    use crate::store::Store;

    use super::*;

    fn pod_detail(namespace: &str, name: &str) -> PodDetail {
        PodDetail {
            core: Some(PodWithSpec {
                pod: Some(Pod {
                    name: name.to_string(),
                    status: PodStatus::Pending as i32,
                    requests: None,
                    limits: None,
                }),
                spec: Some(PodSpec {
                    namespace: namespace.to_string(),
                    containers: vec![],
                }),
            }),
            container_statuses: vec![],
            pod_ip: String::new(),
            message: String::new(),
            resource_usage: None,
            node_name: String::new(),
        }
    }

    fn get_pod_request(namespace: &str, name: &str) -> Request<GetPodRequest> {
        Request::new(GetPodRequest {
            name: name.to_string(),
            namespace: namespace.to_string(),
        })
    }

    #[tokio::test]
    async fn test_get_pod_returns_inserted_pod() {
        let service = ApiService {
            store: Arc::new(Store::new()),
        };
        let pod = pod_detail("default", "my-pod");
        service.store.upsert_pod(pod.clone()).await;

        let response = service
            .get_pod_impl(get_pod_request("default", "my-pod"))
            .await
            .expect("get_pod on an existing pod should succeed");

        assert_eq!(response.into_inner().pod, Some(pod));
    }

    #[tokio::test]
    async fn test_get_pod_missing_returns_not_found() {
        let service = ApiService {
            store: Arc::new(Store::new()),
        };

        let err = service
            .get_pod_impl(get_pod_request("default", "does-not-exist"))
            .await
            .expect_err("get_pod on a missing pod should return NotFound");

        assert_eq!(err.code(), Code::NotFound);
    }
}
