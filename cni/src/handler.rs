#![allow(dead_code)]

use proto::cni::v1::{
    AddWorkloadNetworkRequest, CniResponse, DeleteWorkloadNetworkRequest, GetWorkloadNetworkRequest,
};

pub trait NetworkHandler {
    fn add_workload_network(&self, request: AddWorkloadNetworkRequest) -> CniResponse;

    fn delete_workload_network(&self, request: DeleteWorkloadNetworkRequest) -> CniResponse;

    fn get_workload_network(&self, request: GetWorkloadNetworkRequest) -> CniResponse;
}

pub struct EmptyNetworkHandler;

impl NetworkHandler for EmptyNetworkHandler {
    fn add_workload_network(&self, _request: AddWorkloadNetworkRequest) -> CniResponse {
        todo!("Implement workload network creation")
    }

    fn delete_workload_network(&self, _request: DeleteWorkloadNetworkRequest) -> CniResponse {
        todo!("Implement workload network deletion")
    }

    fn get_workload_network(&self, _request: GetWorkloadNetworkRequest) -> CniResponse {
        todo!("Implement workload network lookup")
    }
}
