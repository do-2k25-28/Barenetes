use proto::cni::v1::{
    AddWorkloadNetworkRequest, AddWorkloadNetworkResponse, DeleteWorkloadNetworkRequest,
    DeleteWorkloadNetworkResponse, GetWorkloadNetworkRequest, GetWorkloadNetworkResponse,
    cni_service_server::CniService,
};
use tonic::{Request, Response, Status};

#[derive(Default)]
pub struct CniRpcService;

#[tonic::async_trait]
impl CniService for CniRpcService {
    async fn add_workload_network(
        &self,
        _request: Request<AddWorkloadNetworkRequest>,
    ) -> Result<Response<AddWorkloadNetworkResponse>, Status> {
        Err(Status::unimplemented(
            "workload network creation is not implemented",
        ))
    }

    async fn delete_workload_network(
        &self,
        _request: Request<DeleteWorkloadNetworkRequest>,
    ) -> Result<Response<DeleteWorkloadNetworkResponse>, Status> {
        Err(Status::unimplemented(
            "workload network deletion is not implemented",
        ))
    }

    async fn get_workload_network(
        &self,
        _request: Request<GetWorkloadNetworkRequest>,
    ) -> Result<Response<GetWorkloadNetworkResponse>, Status> {
        Err(Status::unimplemented(
            "workload network lookup is not implemented",
        ))
    }
}
