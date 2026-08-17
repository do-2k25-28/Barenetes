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
        _request: Request<GetPodRequest>,
    ) -> Result<Response<GetPodResponse>, Status> {
        todo!("store.get_pod, return NotFound if missing")
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
