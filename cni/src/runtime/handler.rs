use proto::cni::v1::{
    AddWorkloadNetworkRequest, AddWorkloadNetworkResponse, DeleteWorkloadNetworkRequest,
    DeleteWorkloadNetworkResponse, GetWorkloadNetworkRequest, GetWorkloadNetworkResponse,
    cni_service_server::CniService,
};
use std::io;
use std::sync::{Arc, Mutex};
use tonic::{Request, Response, Status};

use crate::ip_pool::IpPoolDirectory;
use crate::state::StateStore;

pub(crate) struct CniRpcService {
    pools: IpPoolDirectory,
    state: StateStore,
    operation_lock: Arc<Mutex<()>>,
}

impl CniRpcService {
    pub(crate) fn new(pools: IpPoolDirectory, state: StateStore) -> Self {
        Self {
            pools,
            state,
            operation_lock: Arc::new(Mutex::new(())),
        }
    }
}

#[tonic::async_trait]
impl CniService for CniRpcService {
    async fn add_workload_network(
        &self,
        request: Request<AddWorkloadNetworkRequest>,
    ) -> Result<Response<AddWorkloadNetworkResponse>, Status> {
        let pools = self.pools.clone();
        let state = self.state.clone();
        let operation_lock = self.operation_lock.clone();
        let network = tokio::task::spawn_blocking(move || {
            let _guard = operation_lock
                .lock()
                .map_err(|_| io::Error::other("CNI operation lock is poisoned"))?;
            crate::network::add_workload_network(request.into_inner(), &pools, &state)
        })
        .await
        .map_err(|_| Status::internal("network worker failed"))?
        .map_err(status_from_io)?;
        Ok(Response::new(AddWorkloadNetworkResponse {
            network: Some(network),
        }))
    }

    async fn delete_workload_network(
        &self,
        request: Request<DeleteWorkloadNetworkRequest>,
    ) -> Result<Response<DeleteWorkloadNetworkResponse>, Status> {
        let pools = self.pools.clone();
        let state = self.state.clone();
        let operation_lock = self.operation_lock.clone();
        let success = tokio::task::spawn_blocking(move || {
            let _guard = operation_lock
                .lock()
                .map_err(|_| io::Error::other("CNI operation lock is poisoned"))?;
            crate::network::delete_workload_network(request.into_inner(), &pools, &state)
        })
        .await
        .map_err(|_| Status::internal("network worker failed"))?
        .map_err(status_from_io)?;
        Ok(Response::new(DeleteWorkloadNetworkResponse { success }))
    }

    async fn get_workload_network(
        &self,
        request: Request<GetWorkloadNetworkRequest>,
    ) -> Result<Response<GetWorkloadNetworkResponse>, Status> {
        let state = self.state.clone();
        let operation_lock = self.operation_lock.clone();
        let network = tokio::task::spawn_blocking(move || {
            let _guard = operation_lock
                .lock()
                .map_err(|_| io::Error::other("CNI operation lock is poisoned"))?;
            crate::network::get_workload_network(request.into_inner(), &state)
        })
        .await
        .map_err(|_| Status::internal("network worker failed"))?
        .map_err(status_from_io)?;
        Ok(Response::new(GetWorkloadNetworkResponse {
            network: Some(network),
        }))
    }
}

fn status_from_io(error: io::Error) -> Status {
    eprintln!("cni: {error}");
    match error.kind() {
        io::ErrorKind::InvalidInput => Status::invalid_argument(error.to_string()),
        io::ErrorKind::NotFound => Status::not_found(error.to_string()),
        _ => Status::internal(format!("workload network operation failed: {error}")),
    }
}
